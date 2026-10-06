//! Session manager + mux integration: spawn a scripted agent in a real
//! PTY and exercise attach replay, fan-out, input, exit, and failure.

mod support;

use bytes::Bytes;
use pm_daemon::daemon::WebTerminalReplay;
use pm_protocol::domain::{AgentKind, SessionState};
use support::*;

#[tokio::test]
async fn spawn_marks_session_working_and_agent_starts() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "hello-world");
    assert_eq!(
        env.daemon.subscribe().0.sessions[0].state,
        SessionState::Working
    );

    let (replay, mut rx) = env.daemon.mux.attach(id).unwrap();
    await_output(&mut rx, replay.to_vec(), b"READY hello-world").await;
}

#[tokio::test]
async fn input_reaches_agent_and_output_fans_out_to_both_viewers() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");

    let (r1, mut rx1) = env.daemon.mux.attach(id).unwrap();
    await_output(&mut rx1, r1.to_vec(), b"READY p").await;
    let (r2, mut rx2) = env.daemon.mux.attach(id).unwrap();

    env.daemon
        .mux
        .input(id, Bytes::from_static(b"echo fanout-check\n"))
        .unwrap();

    await_output(&mut rx1, Vec::new(), b"OUT fanout-check").await;
    await_output(&mut rx2, r2.to_vec(), b"OUT fanout-check").await;
}

#[tokio::test]
async fn local_terminal_uses_the_shared_attach_input_and_snapshot_pipeline() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "shared-pipeline");

    let (replay, mut rx, guard) = env.daemon.attach(id).await.unwrap();
    assert!(guard.is_some(), "local viewers participate in relay gating");
    await_output(&mut rx, replay.to_vec(), b"READY shared-pipeline").await;

    env.daemon
        .terminal_input(id, Bytes::from_static(b"echo shared-input\n"));
    await_output(&mut rx, Vec::new(), b"OUT shared-input").await;
    drop(guard);

    let attach = env.daemon.attach_web_terminal(id, 1024, None).unwrap();
    assert!(
        attach.guard.is_some(),
        "web viewers use the same relay guard"
    );
    let WebTerminalReplay::Complete { bytes } = attach.replay else {
        panic!("warm local relay should serve its mirrored snapshot immediately");
    };
    let rendered = String::from_utf8_lossy(&bytes);
    assert!(rendered.contains("shared-input"), "snapshot: {rendered:?}");
}

/// gridtui's footer carries a tick counter that advances between any two
/// snapshots, so comparisons blank it out.
fn without_ticks(snapshot: &[u8]) -> String {
    const TICK: &str = "tick ";
    let text = String::from_utf8_lossy(snapshot);
    let mut out = String::new();
    let mut rest = text.as_ref();
    while let Some(at) = rest.find(TICK) {
        out.push_str(&rest[..at + TICK.len()]);
        rest = rest[at + TICK.len()..].trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

#[tokio::test]
async fn a_joining_web_viewer_gets_the_screen_laid_out_for_the_pty_size() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "geometry");
    let pty_size = (150, 30);
    env.daemon.terminal_resize(id, pty_size.0, pty_size.1);

    let mut first = env.daemon.attach_web_terminal(id, 1024, None).unwrap();
    assert!(matches!(first.replay, WebTerminalReplay::Streaming(_)));
    env.daemon
        .terminal_input(id, Bytes::from_static(b"gridtui\n"));
    await_output(&mut first.output, Vec::new(), b"STATUS 150x30 tick").await;

    let joined = env.daemon.attach_web_terminal(id, 1024, None).unwrap();
    assert_eq!(joined.pty_size, pty_size);
    let WebTerminalReplay::Complete { bytes } = joined.replay else {
        panic!("a second viewer joins the warm relay");
    };
    let truth = env.daemon.mux.attach_snapshot(id).unwrap();
    assert_eq!(truth.size, pty_size);
    assert_eq!(
        without_ticks(&bytes),
        without_ticks(&truth.bytes),
        "the relay mirror must hold the screen the PTY's own model holds"
    );
}

#[tokio::test]
async fn late_attach_replays_history_already_written() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");

    let (r1, mut rx1) = env.daemon.mux.attach(id).unwrap();
    env.daemon
        .mux
        .input(id, Bytes::from_static(b"echo history-marker\n"))
        .unwrap();
    await_output(&mut rx1, r1.to_vec(), b"OUT history-marker").await;

    let (replay, _rx) = env.daemon.mux.attach(id).unwrap();
    let replay = String::from_utf8_lossy(&replay);
    assert!(
        replay.contains("READY p"),
        "replay missing banner: {replay}"
    );
    assert!(
        replay.contains("OUT history-marker"),
        "replay missing history: {replay}"
    );
}

#[tokio::test]
async fn exit_persists_state_code_and_transcript() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");

    let (r, mut rx) = env.daemon.mux.attach(id).unwrap();
    await_output(&mut rx, r.to_vec(), b"READY p").await;
    env.daemon
        .mux
        .input(id, Bytes::from_static(b"echo last-words\nexit 3\n"))
        .unwrap();

    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.semantic_session_id, id);
    assert_eq!(exit.exit_code, Some(3));

    env.daemon.handle_session_exit(exit);
    let session = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert_eq!(session.state, SessionState::Exited);
    assert_eq!(session.exit_code, Some(3));
    assert!(session.ended_at_unix_ms.is_some());

    let transcript = std::fs::read(env.daemon.scrollback_path(id)).unwrap();
    let transcript = String::from_utf8_lossy(&transcript);
    assert!(
        transcript.contains("OUT last-words"),
        "transcript missing output: {transcript}"
    );

    assert!(env.daemon.mux.attach(id).is_err());
}

#[tokio::test]
async fn kill_terminates_the_agent() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let (r, mut rx) = env.daemon.mux.attach(id).unwrap();
    await_output(&mut rx, r.to_vec(), b"READY p").await;

    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.semantic_session_id, id);
}

#[tokio::test]
async fn spawn_failure_marks_session_failed_with_reason() {
    let mut registry = pm_adapters::AdapterRegistry::empty();
    registry.register(Box::new(pm_adapters::TestAgentAdapter {
        program: "/nonexistent/agent-binary".into(),
    }));
    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry,
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _exit_rx) = pm_daemon::Daemon::new(config).unwrap();
    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();

    let err = daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap_err();
    assert!(err.to_string().contains("spawn failed"), "{err}");

    let session = &daemon.subscribe().0.sessions[0];
    assert_eq!(session.state, SessionState::Failed);
    assert!(!session.state_detail.is_empty());
}

#[tokio::test]
async fn interrupt_delivers_ctrl_c_through_the_pty() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let (r, mut rx) = env.daemon.mux.attach(id).unwrap();
    await_output(&mut rx, r.to_vec(), b"READY p").await;

    env.daemon.interrupt_session(id).unwrap();
    assert_eq!(
        env.daemon
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|session| session.id == id)
            .unwrap()
            .state,
        SessionState::Idle,
        "a successful interrupt deterministically ends the active turn"
    );
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.semantic_session_id, id);
}

#[tokio::test]
async fn scrollback_is_capped_but_keeps_newest_output() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let (r, mut rx) = env.daemon.mux.attach(id).unwrap();
    await_output(&mut rx, r.to_vec(), b"READY p").await;

    let big = pm_daemon::mux::SCROLLBACK_CAP_BYTES + 4096;
    env.daemon
        .mux
        .input(id, Bytes::from(format!("bigout {big}\necho tail-marker\n")))
        .unwrap();
    await_output(&mut rx, Vec::new(), b"OUT tail-marker").await;

    let (replay, _) = env.daemon.mux.attach(id).unwrap();
    assert!(replay.len() <= pm_daemon::mux::SCROLLBACK_CAP_BYTES);
    assert!(String::from_utf8_lossy(&replay).contains("OUT tail-marker"));
}

#[tokio::test]
async fn terminate_all_signals_live_sessions() {
    let mut env = daemon_env();
    let a = spawn_test_session(&env, "p");
    let _b = spawn_test_session(&env, "p");
    let (r, mut rx) = env.daemon.mux.attach(a).unwrap();
    await_output(&mut rx, r.to_vec(), b"READY p").await;

    let signalled = env.daemon.mux.terminate_all();
    assert_eq!(signalled, 2, "both live sessions signalled");

    for _ in 0..2 {
        tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
            .await
            .expect("a session did not exit after terminate_all")
            .unwrap();
    }
}

/// A viewer that falls behind a burst loses chunks rather than the
/// stream ending. Production detaches or continues on lag in every
/// place it reads one of these, so the helper the tests wait on has to
/// behave the same or a flood makes them fail at random.
#[tokio::test]
async fn awaiting_output_survives_a_viewer_falling_behind() {
    let (tx, mut rx) = tokio::sync::broadcast::channel::<Bytes>(4);
    for i in 0..64u8 {
        let _ = tx.send(Bytes::from(vec![b'a' + (i % 26)]));
    }
    let _ = tx.send(Bytes::from_static(b"OUT tail-marker"));

    let seen = support::await_output(&mut rx, Vec::new(), b"OUT tail-marker").await;
    assert!(String::from_utf8_lossy(&seen).contains("OUT tail-marker"));
}

/// A session whose agent could not start is exactly when an operator
/// needs a shell on that machine: to see whether the binary is there at
/// all. Opening one must not depend on the agent having run.
#[tokio::test]
async fn a_shell_opens_on_a_session_whose_agent_failed_to_spawn() {
    let mut registry = pm_adapters::AdapterRegistry::empty();
    registry.register(Box::new(pm_adapters::TestAgentAdapter {
        program: "/nonexistent/agent-binary".into(),
    }));
    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry,
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _exit_rx) = pm_daemon::Daemon::new(config).unwrap();
    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();
    daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap_err();

    let session = daemon.subscribe().0.sessions[0].clone();
    assert_eq!(session.state, SessionState::Failed);

    let terminal = daemon
        .create_shell(session.id, "Shell")
        .expect("a shell should open on a session whose agent never started");
    let opened = daemon.terminal(terminal).unwrap();
    assert_eq!(opened.session_id, session.id);
    assert_eq!(opened.kind, pm_protocol::domain::TerminalKind::Shell);
}
