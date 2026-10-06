mod support;

use pm_protocol::domain::{AgentKind, ControllerMsg, HarnessStatus, WorkerMsg};
use support::*;

fn enroll(env: &TestEnv, protocol_version: u32) -> pm_daemon::daemon::WorkerRegistration {
    let (token, _) = env
        .daemon
        .create_worker_enrollment("installer-host")
        .unwrap();
    let reg = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "installer-host-key",
            hostname: "installer-host",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version,
        })
        .unwrap();
    let project = env.daemon.get_project(env.project_id).unwrap();
    env.daemon
        .set_bucket_workers(project.bucket_id, &[0, reg.worker_id], 0, None)
        .unwrap();
    env.daemon
        .set_project_workers(project.id, &[0, reg.worker_id], Some(reg.worker_id))
        .unwrap();
    reg
}

#[tokio::test]
async fn installation_requests_and_failure_output_reach_the_selected_worker() {
    let env = daemon_env();
    let mut reg = enroll(&env, pm_protocol::WORKER_PROTOCOL_VERSION);
    let daemon = env.daemon.clone();
    let project_id = env.project_id;
    let worker_id = reg.worker_id;
    let request = tokio::spawn(async move {
        daemon
            .harness_status(project_id, worker_id, Some(AgentKind::Test), true)
            .await
    });
    let message = tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
        .await
        .unwrap()
        .unwrap();
    let req_id = match message {
        ControllerMsg::HarnessRequest {
            req_id,
            agent: AgentKind::Test,
            install: true,
        } => req_id,
        other => panic!("unexpected request: {other:?}"),
    };
    let expected = HarnessStatus {
        state: "failed".into(),
        command: "installer".into(),
        output: "permission denied".into(),
        error: "exit status 7".into(),
    };
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::HarnessStatus {
            req_id,
            status: expected.clone(),
        },
    );
    assert_eq!(request.await.unwrap().unwrap(), (AgentKind::Test, expected));
}

#[tokio::test]
async fn old_workers_receive_no_installation_messages() {
    let env = daemon_env();
    let mut reg = enroll(&env, pm_protocol::WORKER_PROTOCOL_HARNESS_INSTALL - 1);
    let (_, status) = env
        .daemon
        .harness_status(env.project_id, reg.worker_id, Some(AgentKind::Test), false)
        .await
        .unwrap();
    assert_eq!(status.state, "unsupported");
    let error = env
        .daemon
        .harness_status(env.project_id, reg.worker_id, Some(AgentKind::Test), true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("update this worker"));
    assert!(reg.rx.try_recv().is_err());
}

#[tokio::test]
async fn disconnected_workers_report_an_error() {
    let env = daemon_env();
    let reg = enroll(&env, pm_protocol::WORKER_PROTOCOL_VERSION);
    env.daemon.disconnect_worker(&reg.link);
    let error = env
        .daemon
        .harness_status(env.project_id, reg.worker_id, Some(AgentKind::Test), false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("offline"));
}

#[tokio::test]
async fn local_check_uses_the_projects_resolved_agent_without_creating_a_session() {
    let env = codex_tui_daemon_env();
    env.daemon
        .set_project_default_agent(env.project_id, Some(AgentKind::Codex))
        .unwrap();
    let (agent, status) = env
        .daemon
        .harness_status(env.project_id, 0, None, false)
        .await
        .unwrap();
    assert_eq!(agent, AgentKind::Codex);
    assert_ne!(status.state, "installing");
    assert!(env.daemon.subscribe().0.sessions.is_empty());
}
