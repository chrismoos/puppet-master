//! Turns whose end was never reported.
//!
//! A hook-integrated agent is the only thing that can end its own turn, so
//! a lost Stop hook pins a session in Working, where no supervision path
//! ever looks at it. These cover the watchdog that infers the end, the
//! guards that keep it from firing on an agent that is merely slow, and
//! the promotion back to Working when such an agent resumes.

mod support;

use std::time::{SystemTime, UNIX_EPOCH};

use pm_daemon::stale_turn::{StaleTurnKind, HOOKS_UNOBSERVED_DETAIL, INFERRED_IDLE_DETAIL};
use pm_protocol::domain::{
    AgentKind, HookKind, PermissionMode, SessionState, WorkerMsg, LOCAL_WORKER_ID,
};
use support::{
    codex_tui_daemon_env, hooked_agent_daemon_env, restartable_hooked_agent_daemon_env,
    spawn_test_session, TestEnv,
};

/// Comfortably past both the quiet threshold and the hook-silence grace.
const WELL_PAST_QUIET_MS: i64 = 5 * 60 * 1000;

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn session(env: &TestEnv, id: u64) -> pm_protocol::domain::Session {
    env.daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
}

/// Puts a session into Working without a hook, which is what a launch
/// whose hooks never fired looks like from the daemon's side.
fn force_working(env: &TestEnv, id: u64) {
    env.daemon.apply_worker_message(
        LOCAL_WORKER_ID,
        WorkerMsg::SessionState {
            session_id: id,
            state: SessionState::Working,
            detail: String::new(),
        },
    );
}

fn hook(env: &TestEnv, session: u64, kind: HookKind) {
    let token = env.daemon.session_token(session).unwrap().unwrap();
    env.daemon
        .handle_hook_event(&token, kind, "", "", "", false)
        .unwrap();
}

/// A session on the hook-integrated test adapter, mid-turn with its hooks
/// known to work: the state a lost turn-end hook leaves behind.
fn spawn_hooked_session(env: &TestEnv) -> u64 {
    spawn_hooked_session_for(env, AgentKind::Codex)
}

fn spawn_hooked_session_for(env: &TestEnv, agent: AgentKind) -> u64 {
    let id = env
        .daemon
        .spawn_session(
            env.project_id,
            agent,
            "hooked task",
            "do the work",
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    hook(env, id, HookKind::Started);
    hook(env, id, HookKind::PromptSubmitted);
    assert_eq!(session(env, id).state, SessionState::Working);
    id
}

/// Two passes are required, so this is what "the watchdog ran long
/// enough to act" looks like.
fn run_passes(env: &TestEnv, at: i64) -> Vec<pm_daemon::stale_turn::StaleTurnOutcome> {
    let first = env.daemon.process_stale_turns_at(at);
    assert!(first.is_empty(), "one observation must not be enough");
    env.daemon.process_stale_turns_at(at)
}

#[tokio::test]
async fn a_quiet_turn_with_no_reported_end_is_inferred_to_have_ended() {
    let env = codex_tui_daemon_env();
    let id = spawn_hooked_session(&env);

    assert!(
        env.daemon.process_stale_turns_at(unix_ms()).is_empty(),
        "a turn that just started is not stale"
    );

    let outcomes = run_passes(&env, unix_ms() + WELL_PAST_QUIET_MS);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0].session_id, id);
    assert_eq!(outcomes[0].kind, StaleTurnKind::InferredIdle);

    let after = session(&env, id);
    assert_eq!(after.state, SessionState::Idle);
    assert_eq!(
        after.state_detail, INFERRED_IDLE_DETAIL,
        "an inferred end must be distinguishable from a reported one"
    );
}

#[tokio::test]
async fn a_reported_turn_end_leaves_nothing_for_the_watchdog() {
    let env = codex_tui_daemon_env();
    let id = spawn_hooked_session(&env);
    hook(&env, id, HookKind::TurnEnded);

    assert!(
        env.daemon
            .process_stale_turns_at(unix_ms() + WELL_PAST_QUIET_MS)
            .is_empty(),
        "a session that is not working cannot have a stale turn"
    );
    assert_eq!(
        session(&env, id).state_detail,
        "",
        "a clean stop carries no inferred-end detail"
    );
}

#[tokio::test]
async fn terminal_output_keeps_a_slow_turn_alive() {
    let env = codex_tui_daemon_env();
    let id = spawn_hooked_session(&env);
    let terminal = env.daemon.agent_terminal(id).unwrap();

    let stale_at = unix_ms() + WELL_PAST_QUIET_MS;
    assert!(
        env.daemon.process_stale_turns_at(stale_at).is_empty(),
        "one observation is never enough to act on"
    );

    // The agent was thinking, not finished. Its next byte of output lands
    // now, so a pass taken just after it sees a live turn and must drop
    // the observation it had already made.
    env.daemon
        .handle_terminal_activity(terminal.id, terminal.generation);
    assert!(env
        .daemon
        .process_stale_turns_at(unix_ms() + 1_000)
        .is_empty());

    // With the count cleared, going quiet again starts over: this pass is
    // the first observation of the new silence, not the second of the old.
    assert!(
        env.daemon.process_stale_turns_at(stale_at).is_empty(),
        "output must reset the consecutive-pass count, not merely pause it"
    );
    assert_eq!(session(&env, id).state, SessionState::Working);

    // And the pass after that acts, which proves the reset delayed the
    // demotion rather than disabling it.
    let outcomes = env.daemon.process_stale_turns_at(stale_at);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0].kind, StaleTurnKind::InferredIdle);
}

#[tokio::test]
async fn an_agent_that_resumes_after_an_inferred_end_goes_back_to_work() {
    let env = codex_tui_daemon_env();
    let id = spawn_hooked_session(&env);
    let terminal = env.daemon.agent_terminal(id).unwrap();
    run_passes(&env, unix_ms() + WELL_PAST_QUIET_MS);
    assert_eq!(session(&env, id).state, SessionState::Idle);

    // The inference was wrong: the agent was quiet, not done. One write
    // must put it back to Working rather than leaving it parked forever.
    env.daemon
        .handle_terminal_activity(terminal.id, terminal.generation);
    assert_eq!(
        session(&env, id).state,
        SessionState::Working,
        "an inferred end must be reversible by the agent itself"
    );
}

#[tokio::test]
async fn a_real_turn_end_after_an_inferred_one_is_accepted() {
    let env = codex_tui_daemon_env();
    let id = spawn_hooked_session(&env);
    run_passes(&env, unix_ms() + WELL_PAST_QUIET_MS);

    hook(&env, id, HookKind::TurnEnded);
    let after = session(&env, id);
    assert_eq!(after.state, SessionState::Idle);
    assert_eq!(
        after.state_detail, "",
        "a hook that finally arrives replaces the inference with the real thing"
    );
}

#[tokio::test]
async fn a_hookless_agent_is_never_second_guessed() {
    let env = codex_tui_daemon_env();
    // The plain test adapter installs no lifecycle hooks, so its Working
    // state is not evidence that anything was lost.
    let id = spawn_test_session(&env, "hookless work");
    force_working(&env, id);

    assert!(
        env.daemon
            .process_stale_turns_at(unix_ms() + WELL_PAST_QUIET_MS)
            .is_empty(),
        "a hookless agent has no turn-end signal to lose"
    );
    assert!(env
        .daemon
        .process_stale_turns_at(unix_ms() + WELL_PAST_QUIET_MS)
        .is_empty());
    assert_eq!(session(&env, id).state, SessionState::Working);
}

#[tokio::test]
async fn a_generation_whose_hooks_never_fired_is_reported_and_downgraded() {
    let env = codex_tui_daemon_env();
    let id = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Codex,
            "hooked task",
            "do the work",
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    // No hook of any kind arrives: the agent CLI ignored the hook
    // configuration entirely.
    force_working(&env, id);

    assert!(
        env.daemon.process_stale_turns_at(unix_ms()).is_empty(),
        "the grace period covers process startup"
    );

    let outcomes = env
        .daemon
        .process_stale_turns_at(unix_ms() + WELL_PAST_QUIET_MS);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0].kind, StaleTurnKind::HooksUnobserved);
    assert_eq!(session(&env, id).state_detail, HOOKS_UNOBSERVED_DETAIL);

    assert!(
        env.daemon
            .process_stale_turns_at(unix_ms() + 2 * WELL_PAST_QUIET_MS)
            .is_empty(),
        "the downgrade is reported once per generation, not every pass"
    );
}

#[tokio::test]
async fn the_last_hook_seen_is_available_for_diagnosis() {
    let env = codex_tui_daemon_env();
    let id = spawn_hooked_session(&env);

    let (kind, at) = env.daemon.last_hook_seen(id).expect("a hook has arrived");
    assert_eq!(kind, HookKind::PromptSubmitted);
    assert!(at > 0);
}

/// Gemini has no StopFailure counterpart, so a turn that dies before its
/// AfterAgent hook reports nothing at all. Nothing else moves a session
/// out of Working, so the watchdog is the whole recovery path, and it
/// must reach the session rather than leaving it working forever.
#[tokio::test]
async fn an_agent_with_no_turn_failed_hook_still_recovers_from_a_dead_turn() {
    let env = hooked_agent_daemon_env(AgentKind::Gemini);
    let id = spawn_hooked_session_for(&env, AgentKind::Gemini);

    let outcomes = run_passes(&env, unix_ms() + WELL_PAST_QUIET_MS);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0].kind, StaleTurnKind::InferredIdle);

    let after = session(&env, id);
    assert_eq!(after.state, SessionState::Idle);
    assert_eq!(
        after.state_detail, INFERRED_IDLE_DETAIL,
        "a turn nobody reported the end of must not read as a clean stop"
    );

    // And the recovery is a demotion, not a verdict: the agent was only
    // quiet, so its next byte of output puts the turn back.
    let terminal = env.daemon.agent_terminal(id).unwrap();
    env.daemon
        .handle_terminal_activity(terminal.id, terminal.generation);
    assert_eq!(session(&env, id).state, SessionState::Working);
}

/// A daemon restart does not stop the agents already running on a host,
/// so a session that was mid-turn comes back still Working with its turn
/// end still owed. The watchdog is the only thing that can end it, and it
/// used to hold the one fact it needs — that this generation's hooks work
/// — in memory only, which left exactly those sessions unwatched forever.
#[tokio::test]
async fn a_turn_that_outlived_a_daemon_restart_is_still_watched() {
    let (env, restart) = restartable_hooked_agent_daemon_env(AgentKind::Codex);
    let registration = enroll_remote_worker(&env);
    allow_remote_worker(&env, registration.worker_id);
    let id = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Codex,
            "hooked task",
            "do the work",
            None,
            PermissionMode::Inherit,
            Some(registration.worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    hook(&env, id, HookKind::Started);
    hook(&env, id, HookKind::PromptSubmitted);
    assert_eq!(session(&env, id).state, SessionState::Working);
    let terminal = env.daemon.agent_terminal(id).unwrap();

    let credential = registration.credential.clone();
    let reopened = restart.reopen(&env);
    reconnect_remote_worker(&reopened, &credential, &[id]);
    assert_eq!(
        session(&reopened, id).state,
        SessionState::Working,
        "the agent kept running on its host, so the turn is still owed"
    );
    assert_eq!(
        reopened.daemon.agent_terminal(id).unwrap().generation,
        terminal.generation,
        "a remote agent is not respawned by a controller restart"
    );

    let outcomes = run_passes(&reopened, unix_ms() + WELL_PAST_QUIET_MS);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0].session_id, id);
    assert_eq!(
        outcomes[0].kind,
        StaleTurnKind::InferredIdle,
        "hooks that were observed before the restart are still evidence \
         that this generation reports its turns"
    );
    let after = session(&reopened, id);
    assert_eq!(after.state, SessionState::Idle);
    assert_eq!(after.state_detail, INFERRED_IDLE_DETAIL);

    // The inference is a demotion, not a verdict, and it has to stay
    // reversible across the restart too: the promotion it arms lives in the
    // same per-generation state the restart erased.
    let terminal = reopened.daemon.agent_terminal(id).unwrap();
    reopened
        .daemon
        .handle_terminal_activity(terminal.id, terminal.generation);
    assert_eq!(
        session(&reopened, id).state,
        SessionState::Working,
        "the agent was quiet, not finished"
    );
}

/// The other half of the same fact: a generation whose hooks never fired
/// must still be told apart from one whose hooks worked, across a restart.
#[tokio::test]
async fn a_restart_does_not_turn_a_broken_launch_into_a_quiet_agent() {
    let (env, restart) = restartable_hooked_agent_daemon_env(AgentKind::Codex);
    let registration = enroll_remote_worker(&env);
    allow_remote_worker(&env, registration.worker_id);
    let id = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Codex,
            "hooked task",
            "do the work",
            None,
            PermissionMode::Inherit,
            Some(registration.worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    // No hook of any kind arrives before the restart.
    let credential = registration.credential.clone();
    let reopened = restart.reopen(&env);
    reconnect_remote_worker(&reopened, &credential, &[id]);

    let outcomes = reopened
        .daemon
        .process_stale_turns_at(unix_ms() + WELL_PAST_QUIET_MS);
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0].kind, StaleTurnKind::HooksUnobserved);
    assert_eq!(session(&reopened, id).state_detail, HOOKS_UNOBSERVED_DETAIL);
    assert!(
        reopened
            .daemon
            .process_stale_turns_at(unix_ms() + 2 * WELL_PAST_QUIET_MS)
            .is_empty(),
        "the downgrade is still reported once per generation, not every pass"
    );
}

fn enroll_remote_worker(env: &TestEnv) -> pm_daemon::daemon::WorkerRegistration {
    let (token, _expires) = env.daemon.create_worker_enrollment("host").unwrap();
    env.daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap()
}

fn reconnect_remote_worker(env: &TestEnv, credential: &str, live_sessions: &[u64]) {
    env.daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential,
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions,
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
}

fn allow_remote_worker(env: &TestEnv, worker_id: u64) {
    let snapshot = env.daemon.subscribe().0;
    let project = snapshot
        .projects
        .iter()
        .find(|p| p.id == env.project_id)
        .unwrap()
        .clone();
    let bucket = snapshot
        .buckets
        .into_iter()
        .find(|b| b.id == project.bucket_id)
        .unwrap();
    let mut bucket_allowed = bucket.allowed_worker_ids;
    if !bucket_allowed.contains(&worker_id) {
        bucket_allowed.push(worker_id);
    }
    env.daemon
        .set_bucket_workers(
            project.bucket_id,
            &bucket_allowed,
            bucket.default_worker_id,
            None,
        )
        .unwrap();
    let mut project_allowed = project.allowed_worker_ids;
    if !project_allowed.contains(&worker_id) {
        project_allowed.push(worker_id);
    }
    env.daemon
        .set_project_workers(env.project_id, &project_allowed, Some(worker_id))
        .unwrap();
}
