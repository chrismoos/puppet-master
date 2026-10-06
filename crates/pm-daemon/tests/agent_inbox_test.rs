// The scripted agent's inbox directory is named by a process-wide
// environment variable, so this test lives in its own binary. Cargo gives
// each integration target its own process: setting that variable beside
// the other supervision tests changed what every session spawned
// alongside it resolved as an inbound channel, and three of them failed
// only when the suite ran multi-threaded.

mod support;

use pm_protocol::domain::HookKind;
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
