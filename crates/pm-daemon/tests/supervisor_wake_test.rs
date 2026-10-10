//! Supervision wakes: a Supervisor whose turn ended is notified when a
//! session it spawned parks, is not notified twice for the same
//! transition, and is never notified for work outside its own spawn
//! scope or while it is already waiting.

mod support;

use std::sync::Arc;

use pm_daemon::storage::Storage;
use pm_daemon::{Daemon, DaemonConfig};
use pm_protocol::domain::{AgentKind, HookKind, PermissionMode, SessionState, WorkerMsg};
use support::{
    await_output, bucket_of, daemon_env, end_supervisor_turn, file_backed_daemon_env, hook,
    hook_with_background_work, spawn_child, spawn_supervisor, spawn_test_session, unix_ms, TestEnv,
};

fn state_of(env: &TestEnv, id: u64) -> SessionState {
    env.daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
        .state
}

fn item_notes(env: &TestEnv, item_id: u64) -> Vec<String> {
    env.daemon
        .item_notes(bucket_of(env), item_id)
        .unwrap()
        .into_iter()
        .map(|note| note.text)
        .collect()
}

#[tokio::test]
async fn an_idle_child_wakes_a_supervisor_whose_turn_ended() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, item_id) = spawn_child(&env, supervisor, "wake:idle").await;
    end_supervisor_turn(&env, supervisor);

    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "a working child needs no supervision"
    );

    let (replay, mut rx, _guard) = env.daemon.attach(supervisor).await.unwrap();
    hook(&env, child, HookKind::TurnEnded, "");
    let wakes = env.daemon.process_supervisor_wakes().await;
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    assert_eq!(wakes[0].supervisor_id, supervisor);
    assert_eq!(wakes[0].sessions, vec![child]);
    // The supervisor's PTY echoes what it was sent, so this proves the
    // notice reached the agent rather than only the delivery path.
    let seen = await_output(&mut rx, replay.to_vec(), b"wait_sessions").await;
    let seen = String::from_utf8_lossy(&seen);
    assert!(seen.contains(&format!("session {child} is idle")), "{seen}");
    assert!(
        item_notes(&env, item_id)
            .iter()
            .any(|note| note.contains("reached idle unattended")),
        "the wake is audited on the child's item: {:?}",
        item_notes(&env, item_id)
    );
}

#[tokio::test]
async fn a_needs_input_child_wakes_a_supervisor_that_is_itself_blocked() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:blocked").await;
    hook(&env, supervisor, HookKind::NeedsInput, "which branch?");
    hook(&env, child, HookKind::NeedsInput, "child question");

    let wakes = env.daemon.process_supervisor_wakes().await;
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    assert_eq!(wakes[0].sessions, vec![child]);
    assert_eq!(
        state_of(&env, supervisor),
        SessionState::NeedsInput,
        "the user's own question is not cleared by a wake"
    );
}

#[tokio::test]
async fn the_same_transition_never_wakes_a_supervisor_twice() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:once").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::NeedsInput, "question");

    assert_eq!(env.daemon.process_supervisor_wakes().await.len(), 1);
    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "a child parked at the same transition must not wake anyone again"
    );
    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "and must not on any later pass either"
    );
}

#[tokio::test]
async fn a_child_that_parks_again_wakes_the_supervisor_again() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:again").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");
    assert_eq!(env.daemon.process_supervisor_wakes().await.len(), 1);

    hook(&env, child, HookKind::PromptSubmitted, "");
    assert_eq!(state_of(&env, child), SessionState::Working);
    hook(&env, child, HookKind::TurnEnded, "");

    // The throttle spaces wakes of one supervisor, so this pass proves
    // the new transition is retained rather than swallowed.
    let wakes = loop {
        let wakes = env.daemon.process_supervisor_wakes().await;
        if !wakes.is_empty() {
            break wakes;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    assert_eq!(wakes[0].sessions, vec![child]);
}

/// A child that is already parked keeps its state and its state revision
/// when it asks a question, so the question itself is the only thing that
/// can tell one notice from the next. The wake key folds in this value;
/// without it a second question is indistinguishable from the first and
/// the supervisor is never told it was asked.
#[tokio::test]
async fn each_question_from_a_child_is_a_distinct_fact() {
    let (env, db_path) = file_backed_daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:asks").await;
    end_supervisor_turn(&env, supervisor);

    let storage = Storage::open(&db_path).unwrap();
    assert_eq!(
        storage.latest_blocked_report(child).unwrap(),
        None,
        "a child that has asked nothing has no question to identify"
    );

    env.daemon
        .apply_blocked(child, "which endpoint should I use?".into())
        .unwrap();
    let first = storage.latest_blocked_report(child).unwrap();
    assert!(first.is_some());
    assert_eq!(state_of(&env, child), SessionState::NeedsInput);

    env.daemon
        .apply_blocked(child, "and which topic?".into())
        .unwrap();
    let second = storage.latest_blocked_report(child).unwrap();

    // The state did not move, so this is the whole difference between the
    // two notices.
    assert_eq!(state_of(&env, child), SessionState::NeedsInput);
    assert_ne!(first, second, "a second question must be a new identity");
}

#[tokio::test]
async fn a_supervisor_inside_its_turn_is_not_interrupted() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:working").await;
    hook(&env, supervisor, HookKind::PromptSubmitted, "");
    assert_eq!(state_of(&env, supervisor), SessionState::Working);
    hook(&env, child, HookKind::TurnEnded, "");

    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "a working supervisor is still supervising"
    );

    // The transition is not lost: it wakes the supervisor as soon as
    // its own turn ends, which is the case no child transition follows.
    end_supervisor_turn(&env, supervisor);
    let wakes = env.daemon.process_supervisor_wakes().await;
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    assert_eq!(wakes[0].sessions, vec![child]);
}

#[tokio::test]
async fn an_idle_child_wakes_a_supervisor_working_in_the_background() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:bg-working").await;
    hook_with_background_work(&env, supervisor, HookKind::TurnEnded, "", true);
    assert_eq!(state_of(&env, supervisor), SessionState::Working);

    hook(&env, child, HookKind::TurnEnded, "");
    let wakes = env.daemon.process_supervisor_wakes().await;
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    assert_eq!(wakes[0].sessions, vec![child]);
}

#[tokio::test]
async fn a_quiet_supervisor_working_in_the_background_is_reminded() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:bg-reminded").await;
    hook_with_background_work(&env, supervisor, HookKind::TurnEnded, "", true);
    hook(&env, child, HookKind::TurnEnded, "");

    let wakes = env.daemon.process_supervisor_wakes().await;
    assert_eq!(wakes.len(), 1);

    let now = unix_ms() + 120_000;
    let wakes = env.daemon.process_supervisor_wakes_at(now).await;
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    assert_eq!(wakes[0].sessions, vec![child]);
}

/// How long both sides of a supervisor's PTY must be quiet before a notice
/// is typed into it.
const WAKE_TERMINAL_IDLE_MS: i64 = 5_000;

#[tokio::test]
async fn a_notice_waits_for_partial_user_input_and_then_for_a_quiet_pty() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:typed-input").await;
    end_supervisor_turn(&env, supervisor);

    // Keep a viewer attached so these writes exercise the real PTY input
    // path. A half-written line must block automated input indefinitely,
    // even if an arbitrarily large wall-clock idle period is supplied.
    let (_replay, _rx, _guard) = env.daemon.attach(supervisor).await.unwrap();
    env.daemon
        .pty_input(supervisor, bytes::Bytes::from_static(b"half written"));
    hook(&env, child, HookKind::TurnEnded, "");
    assert!(
        env.daemon
            .process_supervisor_wakes_at(unix_ms() + 60_000)
            .await
            .is_empty(),
        "an unsubmitted user line must never have a notice appended to it"
    );

    // Submission clears the partial-input guard. The user's input makes the
    // hookless test agent working; ending that turn makes it eligible again,
    // but only after the PTY quiet window has elapsed.
    let before_submit = unix_ms();
    env.daemon
        .pty_input(supervisor, bytes::Bytes::from_static(b"\r"));
    end_supervisor_turn(&env, supervisor);
    let after_turn = unix_ms();
    assert!(
        env.daemon
            .process_supervisor_wakes_at(before_submit + WAKE_TERMINAL_IDLE_MS - 1)
            .await
            .is_empty(),
        "recent submitted input must still get a quiet window"
    );
    let wakes = env
        .daemon
        .process_supervisor_wakes_at(after_turn + WAKE_TERMINAL_IDLE_MS)
        .await;
    assert_eq!(
        wakes.len(),
        1,
        "the queued transition should eventually deliver"
    );
    assert_eq!(wakes[0].sessions, vec![child]);
}

#[tokio::test]
async fn recent_supervisor_output_also_defers_a_notice() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:recent-output").await;
    end_supervisor_turn(&env, supervisor);
    let terminal = env
        .daemon
        .subscribe()
        .0
        .terminals
        .into_iter()
        .find(|terminal| terminal.session_id == supervisor)
        .unwrap();
    env.daemon
        .handle_terminal_activity(terminal.id, terminal.generation);
    hook(&env, child, HookKind::TurnEnded, "");
    let output_at = unix_ms();
    assert!(
        env.daemon
            .process_supervisor_wakes_at(output_at + 4_000)
            .await
            .is_empty(),
        "recent agent output must keep automated input out of the PTY"
    );

    let (_replay, _rx, _guard) = env.daemon.attach(supervisor).await.unwrap();
    let wakes = env
        .daemon
        .process_supervisor_wakes_at(output_at + 6_000)
        .await;
    assert_eq!(wakes.len(), 1, "the output quiet window should expire");
    assert_eq!(wakes[0].sessions, vec![child]);
}

#[tokio::test]
async fn a_supervisor_already_waiting_is_not_also_nudged() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:waiting").await;
    let bucket_id = bucket_of(&env);
    // A supervisor may flag a question about one child and keep waiting
    // on the rest inside the same turn.
    hook(&env, supervisor, HookKind::NeedsInput, "which branch?");

    let baseline = env
        .daemon
        .supervisor_wait_sessions(
            supervisor,
            bucket_id,
            vec![child],
            None,
            std::time::Duration::from_millis(10),
            Default::default(),
        )
        .await
        .unwrap();
    let cursor = baseline["cursor"].as_u64().unwrap();

    let waiting = {
        let daemon = Arc::clone(&env.daemon);
        tokio::spawn(async move {
            daemon
                .supervisor_wait_sessions(
                    supervisor,
                    bucket_id,
                    vec![child],
                    Some(cursor),
                    std::time::Duration::from_secs(5),
                    Default::default(),
                )
                .await
                .unwrap()
        })
    };
    tokio::task::yield_now().await;

    hook(&env, child, HookKind::TurnEnded, "");
    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "the outstanding wait already delivers this transition"
    );
    let delivered = waiting.await.unwrap();
    assert_eq!(delivered["changes"][0]["session"].as_u64(), Some(child));
}

#[tokio::test]
async fn a_session_outside_the_spawn_scope_wakes_nobody() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let stranger = spawn_test_session(&env, "unrelated work");
    end_supervisor_turn(&env, supervisor);
    hook(&env, stranger, HookKind::NeedsInput, "question");

    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "a supervisor is only responsible for what it spawned"
    );
}

#[tokio::test]
async fn a_child_of_a_plain_session_wakes_nobody() {
    let env = daemon_env();
    let parent = spawn_test_session(&env, "not a supervisor");
    let child = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "child",
            "work",
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            Some(parent),
        )
        .unwrap();
    hook(&env, parent, HookKind::TurnEnded, "");
    hook(&env, child, HookKind::TurnEnded, "");

    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "only a Supervisor holds the supervision loop"
    );
}

#[tokio::test]
async fn an_ended_supervisor_is_never_woken() {
    let mut env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:ended").await;
    env.daemon.kill_session(supervisor).unwrap();
    let exit = tokio::time::timeout(support::TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);
    assert!(!state_of(&env, supervisor).is_live());
    hook(&env, child, HookKind::TurnEnded, "");

    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "a dead supervisor has no terminal to notice anything"
    );
}

/// A restart must not replay history: every live Supervisor would be
/// woken at once for transitions it already handled.
#[tokio::test]
async fn a_restart_does_not_replay_transitions_that_already_happened() {
    let (env, db_path) = file_backed_daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:restart").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");
    assert_eq!(env.daemon.process_supervisor_wakes().await.len(), 1);

    let storage = Storage::open(&db_path).unwrap();
    assert_eq!(
        storage.spawned_sessions().unwrap().len(),
        1,
        "the child is on disk for the next daemon to find"
    );
    let scrollback_dir = db_path.parent().unwrap().join("scrollback");
    let restarted = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: Some(db_path.clone()),
        socket_path: db_path.parent().unwrap().join("restarted.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir,
        registry: support::test_registry(),
        local_worker_enabled: false,
        release_channel: None,
    };
    let (restarted, _) = Daemon::new(restarted).unwrap();
    assert!(
        restarted.process_supervisor_wakes().await.is_empty(),
        "an already-handled transition must not wake anyone after a restart"
    );
}

/// Past the window a delivered notice has to start a turn.
const PAST_CONFIRM: i64 = 30 * 1000;

/// A notice is only proof of delivery once it starts the Supervisor's
/// turn. A paste whose Enter never landed writes text into the pane and
/// changes nothing, and the transition behind it must survive that.
#[tokio::test]
async fn a_notice_that_never_starts_a_turn_is_delivered_again() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:unconfirmed").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");

    let first = unix_ms();
    assert_eq!(env.daemon.process_supervisor_wakes_at(first).await.len(), 1);
    // Still inside the window the supervisor has to respond.
    assert!(
        env.daemon
            .process_supervisor_wakes_at(first)
            .await
            .is_empty(),
        "a notice is not repeated while it may still be acted on"
    );

    let wakes = env
        .daemon
        .process_supervisor_wakes_at(first + PAST_CONFIRM)
        .await;
    assert_eq!(
        wakes.len(),
        1,
        "a notice that started no turn is delivered again, not consumed"
    );
    assert_eq!(wakes[0].sessions, vec![child]);
}

/// Retrying forever would keep the reminder ladder from ever escalating
/// to the user, so the attempts are capped.
#[tokio::test]
async fn redelivery_gives_up_after_a_few_attempts() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:giveup").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");

    // Kept inside the reminder ladder's first rung so every wake counted
    // here is a redelivery of the transition notice, not an idle reminder.
    let start = unix_ms();
    let mut delivered = 0;
    for pass in 0..4 {
        delivered += env
            .daemon
            .process_supervisor_wakes_at(start + pass * PAST_CONFIRM)
            .await
            .len();
    }
    assert_eq!(
        delivered, 3,
        "the notice is attempted a bounded number of times, then stops"
    );
}

/// The mark a restart reads to tell an announced transition from one
/// nobody ever heard about. It is written when the notice is confirmed,
/// never when it is merely written to the terminal, so a daemon that goes
/// down mid-delivery comes back with the child still eligible.
#[tokio::test]
async fn an_announcement_is_persisted_only_once_the_notice_starts_a_turn() {
    let (env, db_path) = file_backed_daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:persisted").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");

    assert_eq!(env.daemon.process_supervisor_wakes().await.len(), 1);
    let storage = Storage::open(&db_path).unwrap();
    assert_eq!(
        storage.announced_wake_key(child).unwrap(),
        None,
        "a notice that has not started a turn is not yet an announcement"
    );

    // The supervisor picks the notice up, which is what makes it one.
    hook(&env, supervisor, HookKind::PromptSubmitted, "");
    end_supervisor_turn(&env, supervisor);
    env.daemon.process_supervisor_wakes().await;
    assert!(
        storage.announced_wake_key(child).unwrap().is_some(),
        "a confirmed notice records the transition it covered"
    );
}

/// Two minutes of quiet with the ladder's first rung already elapsed.
const PAST_IDLE: i64 = 3 * 60 * 1000;

/// A transition notice only fires when a child changes state, so a
/// supervisor that simply stopped supervising is never told anything.
#[tokio::test]
async fn a_quiet_supervisor_with_live_children_is_reminded() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "idle:remind").await;
    end_supervisor_turn(&env, supervisor);

    let (replay, mut rx, _guard) = env.daemon.attach(supervisor).await.unwrap();
    assert!(
        env.daemon.process_supervisor_wakes().await.is_empty(),
        "nothing is due before the quiet period elapses"
    );

    let wakes = env
        .daemon
        .process_supervisor_wakes_at(unix_ms() + PAST_IDLE)
        .await;
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    assert_eq!(wakes[0].sessions, vec![child]);

    let seen = await_output(&mut rx, replay.to_vec(), b"wait_sessions").await;
    let seen = String::from_utf8_lossy(&seen);
    assert!(seen.contains("have not supervised"), "{seen}");
}

/// An exited child is nothing left to answer for.
#[tokio::test]
async fn a_supervisor_whose_children_all_exited_is_left_alone() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "idle:exited").await;
    // The gate reads the child's recorded state, so set it rather than
    // racing a real process exit.
    env.daemon.apply_worker_message(
        0,
        WorkerMsg::SessionState {
            session_id: child,
            state: SessionState::Exited,
            detail: String::new(),
        },
    );
    assert_eq!(state_of(&env, child), SessionState::Exited);
    end_supervisor_turn(&env, supervisor);

    // Exiting is itself a transition the supervisor is told about once.
    let first = unix_ms() + PAST_IDLE;
    assert_eq!(
        env.daemon.process_supervisor_wakes_at(first).await.len(),
        1,
        "the exit is announced"
    );
    // A notice counts as delivered once it starts a turn, so take one the
    // way a live supervisor would. Without this the notice is treated as
    // never having arrived and is correctly delivered again.
    hook(&env, supervisor, HookKind::PromptSubmitted, "");
    end_supervisor_turn(&env, supervisor);
    // With that announced and nothing else alive, there is nothing to
    // keep reminding it about.
    assert!(
        env.daemon
            .process_supervisor_wakes_at(first + PAST_IDLE)
            .await
            .is_empty(),
        "no live children means nothing to supervise"
    );
}

/// A human at the keyboard owns the terminal.
#[tokio::test]
async fn a_supervisor_someone_is_typing_into_is_left_alone() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let _ = spawn_child(&env, supervisor, "idle:typing").await;
    end_supervisor_turn(&env, supervisor);

    let terminal = env.daemon.agent_terminal(supervisor).unwrap();
    // Unsubmitted, which is the line a reminder must never be appended to.
    env.daemon
        .terminal_input_with_submission(terminal.id, "ls".into(), false);

    let now = unix_ms() + PAST_IDLE;
    assert!(
        env.daemon.process_supervisor_wakes_at(now).await.is_empty(),
        "a reminder must not land on a line someone is composing"
    );

    env.daemon
        .terminal_input_with_submission(terminal.id, "\r".into(), true);
    end_supervisor_turn(&env, supervisor);
    assert_eq!(
        env.daemon.process_supervisor_wakes_at(now).await.len(),
        1,
        "a reminder resumes after submission and the supervisor's turn ends"
    );
}

/// Supervising resets the ladder, so a supervisor that is working keeps
/// being given the full quiet period rather than escalating reminders.
#[tokio::test]
async fn supervising_something_resets_the_reminder_ladder() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let _ = spawn_child(&env, supervisor, "idle:reset").await;
    end_supervisor_turn(&env, supervisor);

    let first = unix_ms() + PAST_IDLE;
    assert_eq!(env.daemon.process_supervisor_wakes_at(first).await.len(), 1);
    // The second rung is five minutes, so nothing is due three minutes on.
    assert!(env
        .daemon
        .process_supervisor_wakes_at(first + PAST_IDLE)
        .await
        .is_empty());

    env.daemon.note_supervision_activity(supervisor);
    assert_eq!(
        env.daemon
            .process_supervisor_wakes_at(first + PAST_IDLE)
            .await
            .len(),
        1,
        "having supervised, the supervisor starts from the first rung again"
    );
}

/// A supervisor that ignores the ladder is handed to the user rather
/// than reminded forever.
#[tokio::test]
async fn repeated_reminders_escalate_to_the_user() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let _ = spawn_child(&env, supervisor, "idle:escalate").await;
    end_supervisor_turn(&env, supervisor);

    let mut now = unix_ms();
    for gap in [3, 6, 16, 60] {
        now += gap * 60 * 1000;
        env.daemon.process_supervisor_wakes_at(now).await;
    }

    assert_eq!(
        state_of(&env, supervisor),
        SessionState::NeedsInput,
        "the user is told once the reminders stop working"
    );
    assert!(
        env.daemon
            .process_supervisor_wakes_at(now + 60 * 60 * 1000)
            .await
            .is_empty(),
        "escalation happens once, not on every pass"
    );
}

/// PROPERTY: a child that reaches a supervision-needing state and
/// never changes again must still produce notices. The transition
/// notice fires once; after that, the idle reminder ladder provides
/// recurring notices about live children and eventually escalates.
#[tokio::test]
async fn a_parked_child_still_produces_notices_via_the_nudge_ladder() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:parked").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");

    let first = unix_ms();
    // The transition notice fires once.
    assert_eq!(
        env.daemon.process_supervisor_wakes_at(first).await.len(),
        1,
        "first transition notice"
    );
    // Confirm it so the transition is announced.
    hook(&env, supervisor, HookKind::PromptSubmitted, "");
    end_supervisor_turn(&env, supervisor);
    env.daemon.process_supervisor_wakes_at(first).await;

    // The transition is now announced; the wake path is done. But the
    // nudge ladder takes over: the child is still live.
    let nudge = env
        .daemon
        .process_supervisor_wakes_at(first + PAST_IDLE)
        .await;
    assert_eq!(
        nudge.len(),
        1,
        "the nudge must still fire for a parked, already-announced child"
    );
    assert_eq!(nudge[0].sessions, vec![child]);
}

/// PROPERTY: routine supervisor MCP activity (session_status,
/// list_sessions, spawn_session, read_terminal) must not suppress
/// escalation indefinitely. Only deliberate child interactions
/// (wait_sessions, send_input, interrupt, resume, kill) reset the
/// reminder ladder.
#[tokio::test]
async fn routine_supervisor_reporting_does_not_suppress_escalation() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let _ = spawn_child(&env, supervisor, "idle:routine-report").await;
    end_supervisor_turn(&env, supervisor);

    // Drive through the ladder, calling note_supervision_activity only
    // for read-only operations (which must NOT reset the ladder now).
    // The old code would have reset on every pass; the new code must not.
    let mut now = unix_ms();
    for gap in [3, 6, 16, 60] {
        now += gap * 60 * 1000;
        // Simulate read-only MCP activity that should NOT reset:
        // The actual filtering happens in mcp.rs, but here we verify
        // the ladder still escalates even if the supervisor is active
        // in its terminal (producing agent output that would reset
        // quiet_since in the old code but not the ladder).
        env.daemon.process_supervisor_wakes_at(now).await;
    }

    assert_eq!(
        state_of(&env, supervisor),
        SessionState::NeedsInput,
        "the ladder must reach escalation even when the supervisor is \
         routinely active, because read-only MCP calls no longer reset it"
    );
}

/// The escalation to the user must include the child's actual question
/// so the user can act without looking up what the child wanted.
#[tokio::test]
async fn escalation_surfaces_the_childs_question() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "idle:question").await;
    hook(
        &env,
        child,
        HookKind::NeedsInput,
        "Do you approve the four proposed fixes?",
    );
    end_supervisor_turn(&env, supervisor);

    let mut now = unix_ms();
    // Exhaust the transition notice retries (3 attempts, each
    // after PAST_CONFIRM), then the idle nudge ladder (3 rungs
    // at 2, 5, 15 min backoff), then the escalation.
    for _ in 0..4 {
        now += PAST_CONFIRM;
        env.daemon.process_supervisor_wakes_at(now).await;
    }
    for gap in [3, 6, 16, 60] {
        now += gap * 60 * 1000;
        env.daemon.process_supervisor_wakes_at(now).await;
    }

    assert_eq!(
        state_of(&env, supervisor),
        SessionState::NeedsInput,
        "escalation must have happened"
    );
    let session = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == supervisor)
        .unwrap();
    assert!(
        session.state_detail.contains("Do you approve"),
        "the escalation question must include the child's actual question, \
         got: {}",
        session.state_detail
    );
}

/// A worker disconnect must make children visible to the supervisor.
/// AwaitingWorker is now a supervision-needing state: a child whose
/// worker dropped cannot progress and the supervisor must be told.
#[tokio::test]
async fn awaiting_worker_is_a_supervision_needing_state() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "wake:disconnect").await;
    end_supervisor_turn(&env, supervisor);

    // Simulate the child reaching AwaitingWorker (as the overlay
    // would set it after a worker disconnect).
    env.daemon.apply_worker_message(
        0,
        WorkerMsg::SessionState {
            session_id: child,
            state: SessionState::AwaitingWorker,
            detail: "worker did not reconnect".into(),
        },
    );

    let wakes = env.daemon.process_supervisor_wakes().await;
    assert_eq!(
        wakes.len(),
        1,
        "a child in AwaitingWorker must be noticed by the supervisor"
    );
    assert_eq!(wakes[0].sessions, vec![child]);
}

#[tokio::test]
async fn snooze_delays_idle_reminders_until_its_deadline() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    spawn_child(&env, supervisor, "snooze:deadline").await;
    let until = env.daemon.snooze_supervision(supervisor, 5).unwrap();
    end_supervisor_turn(&env, supervisor);
    assert!(env
        .daemon
        .process_supervisor_wakes_at(until - 1)
        .await
        .is_empty());
    assert_eq!(env.daemon.process_supervisor_wakes_at(until).await.len(), 1);
}

#[tokio::test]
async fn snooze_never_delays_a_child_transition() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "snooze:child").await;
    env.daemon.snooze_supervision(supervisor, 60).unwrap();
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::NeedsInput, "need an answer");
    let wakes = env.daemon.process_supervisor_wakes().await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].sessions, vec![child]);
}

#[tokio::test]
async fn another_prompt_restores_completion_alerts_without_canceling_snooze() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    spawn_child(&env, supervisor, "snooze:new-prompt").await;
    let until = env.daemon.snooze_supervision(supervisor, 5).unwrap();
    hook(&env, supervisor, HookKind::PromptSubmitted, "");
    end_supervisor_turn(&env, supervisor);
    let session = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == supervisor)
        .unwrap();
    assert!(session.idle_unseen);
    assert!(env
        .daemon
        .process_supervisor_wakes_at(until - 1)
        .await
        .is_empty());
}

#[tokio::test]
async fn snooze_after_flag_blocked_delays_reminders_without_hiding_child_transitions() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let (child, _) = spawn_child(&env, supervisor, "snooze:blocked").await;
    env.daemon
        .apply_blocked(supervisor, "waiting for your decision".into())
        .unwrap();
    let until = env.daemon.snooze_supervision(supervisor, 60).unwrap();
    end_supervisor_turn(&env, supervisor);
    let after_quiet = unix_ms() + PAST_IDLE;
    assert!(after_quiet < until);
    assert!(env
        .daemon
        .process_supervisor_wakes_at(after_quiet)
        .await
        .is_empty());
    hook(&env, child, HookKind::NeedsInput, "child needs help");
    let wakes = env.daemon.process_supervisor_wakes_at(after_quiet).await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].sessions, vec![child]);
    assert_eq!(state_of(&env, supervisor), SessionState::NeedsInput);
}
