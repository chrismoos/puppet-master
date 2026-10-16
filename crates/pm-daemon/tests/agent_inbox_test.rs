// The scripted agent's inbox directory is named by a process-wide
// environment variable, so this test lives in its own binary. Cargo gives
// each integration target its own process: setting that variable beside
// the other supervision tests changed what every session spawned
// alongside it resolved as an inbound channel, and three of them failed
// only when the suite ran multi-threaded.

mod support;

use pm_daemon::daemon::UserMessage;
use pm_protocol::domain::{AgentInboxMode, Event, HookKind, MessageOrigin};
use support::{
    agent_inbox, daemon_env, end_supervisor_turn, hook, spawn_child, spawn_supervisor,
    spawn_test_session, unix_ms,
};

/// The inbox directory is process-wide, so one test at a time.
static ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A session's inbox is named after its own agent's process. The mux is
/// keyed by terminal, and terminal ids and session ids are independent,
/// so a lookup by session id finds whichever terminal carries that
/// number: another session's shell, or another session's agent. Writing
/// there reports a delivery the intended agent never hears.
#[tokio::test]
async fn an_inbox_is_resolved_from_the_sessions_own_agent_terminal() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let other = spawn_test_session(&env, "other");
    // An extra terminal pushes the terminal ids past the session ids,
    // which is the only reason the two are distinguishable here.
    env.daemon.create_shell(other, "shell").unwrap();
    let session = spawn_test_session(&env, "subject");
    let terminal = env.daemon.agent_terminal(session).unwrap();
    assert_ne!(
        terminal.id, session,
        "the ids have to diverge or this asserts nothing"
    );
    let pid = env
        .daemon
        .mux
        .child_pid(terminal.id)
        .expect("a live agent child");

    let _inbox = agent_inbox(&env, session);
    let channel = env
        .daemon
        .inbound_channel(session)
        .expect("an inbound channel");
    let pm_adapters::InboundChannel::ClaudeSocket { path, .. } = channel else {
        panic!("the scripted agent resolves a socket channel");
    };
    assert_eq!(
        path.file_name().unwrap().to_string_lossy(),
        format!("{pid}.sock"),
        "the inbox must be the one this session's own agent binds"
    );
}

#[tokio::test]
/// The quiet window and the half-written line exist to protect a
/// terminal from having text appended to it. A notice that goes to the
/// agent's own inbox appends to nothing, so it must not be held back by
/// either.
async fn a_notice_to_the_agents_inbox_does_not_wait_for_the_terminal() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    // Bound where the supervisor's own agent resolves it, so a notice
    // that went to another session's inbox would find nothing here and
    // fall back to the terminal, which is what this asserts against.
    let _inbox = agent_inbox(&env, supervisor);
    let (child, _) = spawn_child(&env, supervisor, "wake:inbox").await;
    end_supervisor_turn(&env, supervisor);

    // The same half-written line that blocks a terminal notice.
    let (_replay, _rx, _guard) = env.daemon.attach(supervisor).await.unwrap();
    env.daemon
        .pty_input(supervisor, bytes::Bytes::from_static(b"half written"));
    hook(&env, child, HookKind::TurnEnded, "");

    let wakes = env.daemon.process_supervisor_wakes_at(unix_ms()).await;
    assert_eq!(
        wakes.len(),
        1,
        "a notice that is not typed must not wait on the terminal"
    );
    assert!(wakes[0].sessions.contains(&child));

    // And it is announced on delivery, because the channel acknowledged
    // it: there is no turn to wait for and nothing to retry.
    assert!(
        env.daemon
            .process_supervisor_wakes_at(unix_ms() + 60_000)
            .await
            .is_empty(),
        "an acknowledged notice must not be delivered twice"
    );
}

/// A message from the user has no session behind it. Reading the sender
/// field without consulting the origin would name session 0, so the
/// record has to say plainly that nobody sent it.
#[tokio::test]
async fn a_user_message_records_no_sending_session() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let mut inbox = agent_inbox(&env, session);

    let outcome = env
        .daemon
        .send_user_message(
            session,
            UserMessage {
                text: "how is the migration going",
                reply: true,
                mode: AgentInboxMode::Queue,
                idempotency_key: "voice-1",
                voice: true,
            },
        )
        .await
        .unwrap();
    let message_id = outcome.message_id.expect("a reply capability was minted");

    let message = env.daemon.agent_message(message_id).unwrap();
    assert_eq!(message.origin, MessageOrigin::Voice);
    assert_eq!(
        message.from_session(),
        None,
        "a voice message has no sending session"
    );
    assert_eq!(message.to_session_id, session);

    let delivered = inbox.lines.recv().await.expect("the agent was handed it");
    // Matched without case, and on substance rather than phrasing: the
    // contract is what the agent is required to know, not the sentence
    // it is told in.
    let said = delivered.to_lowercase().replace('\n', " ");
    for required in [
        // Its answer is spoken, as written, so it writes speech.
        "spoken",
        // Short enough to listen to.
        "two sentences",
        // None of what cannot be read aloud.
        "no file paths",
        // Where it is up to beats a finished answer the user waits for.
        "answer now rather than when the work is done",
    ] {
        assert!(
            said.contains(required),
            "a voice message has to carry {required:?}: {delivered}"
        );
    }
}

/// Without the voice flag the text is handed over as written, so the
/// spoken-answer instruction never reaches an agent that is not being
/// asked for one.
#[tokio::test]
async fn a_typed_user_message_carries_no_spoken_answer_instruction() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let mut inbox = agent_inbox(&env, session);

    env.daemon
        .send_user_message(
            session,
            UserMessage {
                text: "rerun the failing lane",
                reply: false,
                mode: AgentInboxMode::Queue,
                idempotency_key: "",
                voice: false,
            },
        )
        .await
        .unwrap();

    let delivered = inbox.lines.recv().await.expect("the agent was handed it");
    assert!(
        delivered.contains("rerun the failing lane"),
        "the text reaches the agent: {delivered}"
    );
    assert!(
        !delivered.contains("read aloud"),
        "a typed message is not spoken: {delivered}"
    );
}

/// A message the user never asked an answer to is still recorded,
/// because it is the only record that they said it.
#[tokio::test]
async fn a_user_message_without_a_reply_is_still_recorded() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let _inbox = agent_inbox(&env, session);

    let outcome = env
        .daemon
        .send_user_message(
            session,
            UserMessage {
                text: "stop after this step",
                reply: false,
                mode: AgentInboxMode::Queue,
                idempotency_key: "voice-no-reply",
                voice: false,
            },
        )
        .await
        .unwrap();
    let message_id = outcome.message_id.expect("recorded even without a reply");
    let message = env.daemon.agent_message(message_id).unwrap();
    assert_eq!(message.origin, MessageOrigin::User);
    assert_eq!(message.body, "stop after this step");
    assert!(
        message.reply_token.is_none(),
        "no answer was asked for, so no capability was minted"
    );
}

/// A client whose acknowledgment was lost retries the same send. The key
/// it carries has to return the message it already created rather than
/// say the same words to the agent twice.
#[tokio::test]
async fn a_retried_user_message_is_not_delivered_twice() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let mut inbox = agent_inbox(&env, session);

    let first = env
        .daemon
        .send_user_message(
            session,
            UserMessage {
                text: "what is blocking you",
                reply: true,
                mode: AgentInboxMode::Queue,
                idempotency_key: "voice-retry",
                voice: true,
            },
        )
        .await
        .unwrap();
    inbox.lines.recv().await.expect("delivered once");

    let again = env
        .daemon
        .send_user_message(
            session,
            UserMessage {
                text: "what is blocking you",
                reply: true,
                mode: AgentInboxMode::Queue,
                idempotency_key: "voice-retry",
                voice: true,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        again.message_id, first.message_id,
        "the retry names the message the first send created"
    );
    assert_eq!(again.delivery, "duplicate");
    assert_eq!(again.input_state, "already_sent");
    assert!(
        inbox.lines.try_recv().is_err(),
        "the agent was not told the same thing twice"
    );
}

/// An answer to the user has no session to be written into, so it is
/// published to whoever is connected instead.
#[tokio::test]
async fn an_answer_to_the_user_is_published_rather_than_written_to_a_session() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let _inbox = agent_inbox(&env, session);

    let outcome = env
        .daemon
        .send_user_message(
            session,
            UserMessage {
                text: "did the backfill finish",
                reply: true,
                mode: AgentInboxMode::Queue,
                idempotency_key: "voice-answer",
                voice: true,
            },
        )
        .await
        .unwrap();
    let message_id = outcome.message_id.unwrap();
    let token = env
        .daemon
        .agent_message(message_id)
        .unwrap()
        .reply_token
        .expect("a capability to spend");

    let (_snapshot, mut events) = env.daemon.subscribe();
    env.daemon
        .reply_to_agent_message(
            session,
            message_id,
            &token,
            "it finished, nothing left to do",
        )
        .await
        .unwrap();

    let arrived = loop {
        match events.recv().await.expect("the event bus stays open") {
            Event::AgentReplyArrived(arrived) => break arrived,
            _ => continue,
        }
    };
    assert_eq!(arrived.message_id, message_id);
    assert_eq!(arrived.session_id, session);
    assert_eq!(arrived.body, "it finished, nothing left to do");

    let answered = env.daemon.agent_message(message_id).unwrap();
    assert_eq!(
        answered.reply_body.as_deref(),
        Some("it finished, nothing left to do"),
        "the answer is kept for whoever reads it later"
    );
}

/// Queue mode must not reach a working agent mid-turn, and the outcome
/// has to report the mode that actually carried the message rather than
/// the one that was asked for.
#[tokio::test]
async fn queue_mode_does_not_steer_a_working_session() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let mut inbox = agent_inbox(&env, session);
    hook(&env, session, HookKind::PromptSubmitted, "");

    let outcome = env
        .daemon
        .send_user_message(
            session,
            UserMessage {
                text: "when you get a moment",
                reply: false,
                mode: AgentInboxMode::Queue,
                idempotency_key: "",
                voice: true,
            },
        )
        .await
        .unwrap();

    inbox.lines.recv().await.expect("delivered");
    assert_ne!(
        outcome.input_state, "steered",
        "queue mode waits for the turn to end: {outcome:?}"
    );
}

/// Asking to steer a channel that cannot interrupt a turn still
/// delivers, and must report that it did not steer. Claiming an
/// interrupt that only queued would tell the user their correction
/// landed while the agent carries on with what it was already doing.
#[tokio::test]
async fn steer_mode_reports_honestly_when_the_channel_cannot_steer() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let mut inbox = agent_inbox(&env, session);
    hook(&env, session, HookKind::PromptSubmitted, "");

    let channel = env
        .daemon
        .inbound_channel(session)
        .expect("an inbound channel");
    assert!(
        !channel.supports_steering(),
        "this test only means anything on a channel that cannot steer"
    );

    let outcome = env
        .daemon
        .send_user_message(
            session,
            UserMessage {
                text: "stop and look at the lock file",
                reply: false,
                mode: AgentInboxMode::Steer,
                idempotency_key: "",
                voice: true,
            },
        )
        .await
        .unwrap();

    inbox.lines.recv().await.expect("delivered anyway");
    assert_ne!(
        outcome.input_state, "steered",
        "the request was queued, so the outcome must not say it steered: {outcome:?}"
    );
}
