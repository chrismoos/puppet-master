//! Drives the real `pm _hook` binary against a live daemon: the same
//! path an injected Claude Code hook takes in production.

use std::process::Stdio;
use std::time::Duration;

use pm_adapters::{AdapterRegistry, TestAgentAdapter};
use pm_protocol::domain::{AgentKind, SessionState};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn pm_hook_binary_flips_session_state() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("pm.sock");

    // Any quiet PTY-friendly binary works as the fake agent here; the
    // test only exercises the hook path, not agent I/O.
    let mut registry = AdapterRegistry::empty();
    registry.register(Box::new(TestAgentAdapter {
        program: "/bin/sleep".into(),
    }));
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: socket_path.clone(),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("sb"),
        registry,
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _handle) = pm_daemon::start(config).await.unwrap();

    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();
    let session_id = {
        let d = daemon.clone();
        tokio::task::spawn_blocking(move || {
            d.spawn_session(
                project,
                AgentKind::Test,
                "t",
                "300",
                None,
                pm_protocol::domain::PermissionMode::Inherit,
                None,
                true,
                false,
                None,
            )
        })
        .await
        .unwrap()
        .unwrap()
    };
    let token = daemon.session_token(session_id).unwrap().unwrap();

    let run_hook = |kind: &'static str, stdin_payload: &'static str| {
        let socket = socket_path.clone();
        let token = token.clone();
        async move {
            let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_pm"))
                .args(["_hook", kind])
                .env("PM_SOCKET", &socket)
                .env("PM_SESSION_TOKEN", &token)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            use tokio::io::AsyncWriteExt;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin_payload.as_bytes())
                .await
                .unwrap();
            let out = tokio::time::timeout(TEST_TIMEOUT, child.wait_with_output())
                .await
                .expect("hook binary timed out")
                .unwrap();
            assert!(
                out.status.success(),
                "pm _hook failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        }
    };

    run_hook(
        "needs-input",
        r#"{"message": "Claude needs your permission to run a command"}"#,
    )
    .await;
    let session = daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == session_id)
        .unwrap();
    assert_eq!(session.state, SessionState::NeedsInput);
    assert_eq!(
        session.state_detail,
        "Claude needs your permission to run a command"
    );

    run_hook("turn-ended", "").await;
    let session = daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == session_id)
        .unwrap();
    assert_eq!(session.state, SessionState::NeedsInput);

    run_hook("prompt-submitted", "").await;
    run_hook(
        "turn-failed",
        r#"{"error":"rate_limit","error_details":"429 Too Many Requests"}"#,
    )
    .await;
    let session = daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == session_id)
        .unwrap();
    assert_eq!(session.state, SessionState::Idle);
    assert_eq!(session.state_detail, "rate_limit");

    run_hook("prompt-submitted", "").await;
    assert_eq!(
        daemon
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|s| s.id == session_id)
            .unwrap()
            .state,
        SessionState::Working
    );
    let output = run_hook(
        "turn-failed",
        r#"{"hook_event_name":"Interrupt","session_id":"codex-interrupted","turn_id":"turn-canceled"}"#,
    )
    .await;
    assert!(output.is_empty());
    let session = daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == session_id)
        .unwrap();
    assert_eq!(session.state, SessionState::Idle);
    assert_eq!(session.state_detail, "interrupted by user");
    assert_eq!(
        session.agent_session_id.as_deref(),
        Some("codex-interrupted")
    );
}

/// Emulates how agent CLIs execute lifecycle hooks: the command string
/// from the production Claude settings file runs through `/bin/sh -c`.
/// The scripted agent waits for one line of input, then runs the Stop
/// hook exactly as a finished turn would, then stays alive.
struct ShellHookAgent {
    pm_exe: std::path::PathBuf,
}

impl pm_adapters::AgentAdapter for ShellHookAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Test
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    fn spawn_command(
        &self,
        ctx: &pm_adapters::SpawnCtx,
    ) -> Result<pm_adapters::SpawnPlan, pm_adapters::AdapterError> {
        // The daemon fills pm_exe with the test harness binary; point it
        // at the relocated real pm binary before composing settings.
        let ctx = pm_adapters::SpawnCtx {
            cwd: ctx.cwd.clone(),
            task_prompt: ctx.task_prompt.clone(),
            permission_mode: ctx.permission_mode,
            compiled_instructions: ctx.compiled_instructions.clone(),
            model_endpoint: ctx.model_endpoint.clone(),
            fullscreen: ctx.fullscreen,
            integration: pm_adapters::Integration {
                session_id: ctx.integration.session_id,
                session_token: ctx.integration.session_token.clone(),
                socket_path: ctx.integration.socket_path.clone(),
                pm_exe: self.pm_exe.clone(),
                files_dir: ctx.integration.files_dir.clone(),
                mcp_url: ctx.integration.mcp_url.clone(),
                agent_port: None,
            },
        };
        let claude = pm_adapters::ClaudeCodeAdapter.spawn_command(&ctx)?;
        let settings_at = claude
            .spec
            .args
            .iter()
            .position(|a| a == "--settings")
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&claude.spec.args[settings_at + 1]).unwrap(),
        )
        .unwrap();
        let stop_command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_string();
        Ok(pm_adapters::SpawnPlan {
            spec: pm_adapters::CommandSpec {
                program: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    format!("read _line && {stop_command} < /dev/null && exec sleep 300"),
                ],
                env: claude.spec.env,
                cwd: ctx.cwd.clone(),
            },
            agent_session_id: None,
            detect_osc9_needs_input: false,
        })
    }
}

/// The observed field failure: pm installed under a macOS shared folder
/// ("/Volumes/My Shared Files/..."), project cwd under the same tree.
/// The unquoted hook command died with `/bin/sh: /Volumes/My: No such
/// file or directory`, so the daemon never saw the turn end and the
/// session displayed working forever.
#[tokio::test]
async fn lifecycle_hooks_survive_paths_with_spaces() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("My Shared Files");
    let bin_dir = shared.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let pm_exe = bin_dir.join("pm");
    std::fs::copy(env!("CARGO_BIN_EXE_pm"), &pm_exe).unwrap();
    let project_dir = shared.join("proj dir");
    std::fs::create_dir_all(&project_dir).unwrap();

    let mut registry = AdapterRegistry::empty();
    registry.register(Box::new(ShellHookAgent { pm_exe }));
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("sb"),
        registry,
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _handle) = pm_daemon::start(config).await.unwrap();

    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", project_dir.to_str().unwrap())
        .unwrap();
    let session_id = {
        let d = daemon.clone();
        tokio::task::spawn_blocking(move || {
            d.spawn_session(
                project,
                AgentKind::Test,
                "t",
                "finish one turn",
                None,
                pm_protocol::domain::PermissionMode::Inherit,
                None,
                true,
                false,
                None,
            )
        })
        .await
        .unwrap()
        .unwrap()
    };

    let state_of = |d: &std::sync::Arc<pm_daemon::Daemon>| {
        d.subscribe()
            .0
            .sessions
            .into_iter()
            .find(|s| s.id == session_id)
            .unwrap()
            .state
    };
    assert_eq!(state_of(&daemon), SessionState::Working);

    // Release the agent; it runs the composed Stop hook through the shell.
    daemon.pty_input(session_id, bytes::Bytes::from_static(b"\n"));

    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let state = state_of(&daemon);
        if state == SessionState::Idle {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session never went idle, still {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn pm_hook_binary_fails_cleanly_without_a_token() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_pm"))
        .args(["_hook", "turn-ended"])
        .env_remove("PM_SESSION_TOKEN")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("PM_SESSION_TOKEN"));
}
