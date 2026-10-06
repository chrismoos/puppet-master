//! Controller-side worker transport: a worker is simulated by driving
//! the daemon's registration, relay, and lifecycle entry points directly,
//! so the routing that dispatches a session to its worker is exercised
//! without a real worker process or WebSocket.

mod support;

use pm_protocol::domain::{
    AgentKind, ClientEnvelope, ClientMsg, ControllerMsg, Event, FsEntry, ItemWrite, PermissionMode,
    ServerMsg, SessionState, TerminalKind, TerminalRunState, Worker, WorkerMsg, WorkerTerminal,
    LOCAL_WORKER_ID,
};
use support::*;

fn session_state(env: &TestEnv, id: u64) -> SessionState {
    env.daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
        .state
}

fn session(env: &TestEnv, id: u64) -> pm_protocol::domain::Session {
    env.daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|session| session.id == id)
        .unwrap()
}

fn session_worker(env: &TestEnv, id: u64) -> u64 {
    env.daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
        .worker_id
}

fn worker(env: &TestEnv, id: u64) -> Worker {
    env.daemon
        .subscribe()
        .0
        .workers
        .into_iter()
        .find(|w| w.id == id)
        .unwrap()
}

fn enroll(env: &TestEnv) -> pm_daemon::daemon::WorkerRegistration {
    let (token, _expires) = env.daemon.create_worker_enrollment("laptop").unwrap();
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

/// A second machine, which brings its own key: a pinned key identifies one
/// host, so two of them never share one.
fn enroll_second_machine(env: &TestEnv) -> pm_daemon::daemon::WorkerRegistration {
    let (token, _expires) = env.daemon.create_worker_enrollment("desk-vm").unwrap();
    env.daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-desk.local",
            hostname: "desk.local",
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

/// Reconnects an already-enrolled worker with its credential, announcing
/// the sessions it still runs.
fn reconnect(
    env: &TestEnv,
    credential: &str,
    live_sessions: &[u64],
) -> pm_daemon::daemon::WorkerRegistration {
    reconnect_with_terminals(env, credential, live_sessions, &[])
}

fn reconnect_with_terminals(
    env: &TestEnv,
    credential: &str,
    live_sessions: &[u64],
    live_terminals: &[pm_protocol::domain::WorkerTerminal],
) -> pm_daemon::daemon::WorkerRegistration {
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
            live_terminals,
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap()
}

fn allow_remote(env: &TestEnv, worker_id: u64) {
    let snapshot = env.daemon.subscribe().0;
    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == env.project_id)
        .unwrap();
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

fn spawn_remote(env: &TestEnv, worker_id: u64) -> u64 {
    allow_remote(env, worker_id);
    env.daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "t",
            "p",
            None,
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            false,
            None,
        )
        .unwrap()
}

/// A worker speaks for its own sessions and no others. Four arms of
/// `apply_worker_message` already check that and three did not, so one
/// compromised host could walk the small sequential session ids and rewrite
/// the state of every session on the controller, local ones included.
#[tokio::test]
async fn a_host_cannot_speak_about_another_hosts_session() {
    let env = daemon_env();
    let first = enroll(&env);
    let second = enroll_second_machine(&env);
    let victim = spawn_remote(&env, first.worker_id);
    let local = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "local",
            "p",
            None,
            PermissionMode::Inherit,
            Some(LOCAL_WORKER_ID),
            true,
            false,
            None,
        )
        .unwrap();
    let terminal = env.daemon.subscribe().0.terminals;
    let victim_terminal = terminal
        .iter()
        .find(|t| t.session_id == victim)
        .expect("the remote session has a terminal");

    let before = session_state(&env, victim);
    for message in [
        WorkerMsg::SessionState {
            session_id: victim,
            state: SessionState::Failed,
            detail: "not from your host".into(),
        },
        WorkerMsg::NeedsInput { session_id: victim },
        WorkerMsg::TerminalActivity {
            terminal_id: victim_terminal.id,
            generation: victim_terminal.generation,
        },
    ] {
        env.daemon.apply_worker_message(second.worker_id, message);
    }
    assert_eq!(
        session_state(&env, victim),
        before,
        "the second host rewrote a session on the first"
    );

    // Local sessions are not the sending host's either.
    env.daemon.apply_worker_message(
        second.worker_id,
        WorkerMsg::SessionState {
            session_id: local,
            state: SessionState::Failed,
            detail: "not from your host".into(),
        },
    );
    assert_ne!(session_state(&env, local), SessionState::Failed);

    // The host that owns the session is still heard, or the check would have
    // silenced the feature instead of the attack.
    env.daemon.apply_worker_message(
        first.worker_id,
        WorkerMsg::SessionState {
            session_id: victim,
            state: SessionState::Failed,
            detail: "the agent really did fail".into(),
        },
    );
    assert_eq!(session_state(&env, victim), SessionState::Failed);
}

#[tokio::test]
async fn remote_project_cwd_override_applies_to_one_session_and_survives_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let project_path = "/configured/project";
    let config = || pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: Some(db_path.clone()),
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _channels) = pm_daemon::Daemon::new(config()).unwrap();
    let daemon = std::sync::Arc::new(daemon);
    let bucket_id = daemon.create_bucket("bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "project", project_path)
        .unwrap();
    let (token, _) = daemon.create_worker_enrollment("remote").unwrap();
    let mut remote = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-remote",
            hostname: "remote",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "~/",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    let worker_id = remote.worker_id;
    daemon
        .set_bucket_workers(bucket_id, &[0, worker_id], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project_id, &[0, worker_id], None)
        .unwrap();
    daemon
        .set_bucket_workers(bucket_id, &[0, worker_id], worker_id, None)
        .unwrap();
    daemon
        .set_project_workers(project_id, &[worker_id], None)
        .unwrap();

    let default_session = daemon
        .spawn_session(
            project_id,
            AgentKind::Test,
            "default cwd",
            "",
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    match remote.rx.recv().await.unwrap() {
        ControllerMsg::Spawn { cwd, .. } => assert_eq!(cwd, project_path),
        other => panic!("expected spawn, got {other:?}"),
    }
    assert_eq!(
        daemon
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|session| session.id == default_session)
            .unwrap()
            .cwd,
        project_path
    );

    let override_path = "/worker/checkouts/project";
    let supervisor_session = daemon
        .spawn_session(
            project_id,
            AgentKind::Test,
            "supervisor override",
            "",
            Some(override_path),
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            true,
            None,
        )
        .unwrap();
    match remote.rx.recv().await.unwrap() {
        ControllerMsg::Spawn { cwd, .. } => assert_eq!(cwd, override_path),
        other => panic!("expected supervisor spawn, got {other:?}"),
    }
    let supervisor = daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|session| session.id == supervisor_session)
        .unwrap();
    assert_eq!(supervisor.cwd, override_path);
    assert!(supervisor.supervisor_api);

    let hydrated_project = daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|project| project.id == project_id)
        .unwrap();
    assert_eq!(
        hydrated_project
            .worker_paths
            .iter()
            .find(|path| path.worker_id == worker_id),
        None,
        "an override applies to its own session, not to the project"
    );

    let after_override = daemon
        .spawn_session(
            project_id,
            AgentKind::Test,
            "after override",
            "",
            None,
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    match remote.rx.recv().await.unwrap() {
        ControllerMsg::Spawn { cwd, .. } => assert_eq!(cwd, project_path),
        other => panic!("expected spawn after the override, got {other:?}"),
    }
    drop(remote);
    drop(daemon);

    let (restarted, _channels) = pm_daemon::Daemon::new(config()).unwrap();
    let hydrated_project = restarted
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|project| project.id == project_id)
        .unwrap();
    assert_eq!(
        hydrated_project
            .worker_paths
            .iter()
            .find(|path| path.worker_id == worker_id),
        None
    );
    let sessions = restarted.subscribe().0.sessions;
    let cwd_of = |id| {
        sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap()
            .cwd
            .as_str()
    };
    assert_eq!(cwd_of(supervisor_session), override_path);
    assert_eq!(cwd_of(after_override), project_path);
}

#[tokio::test]
async fn controller_restart_shows_eligible_remote_session_offline_then_reconciles_once() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = || pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: Some(db_path.clone()),
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _channels) = pm_daemon::Daemon::new(config()).unwrap();
    let daemon = std::sync::Arc::new(daemon);
    let bucket_id = daemon.create_bucket("bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "project", tmp.path().to_str().unwrap())
        .unwrap();
    let (enrollment, _) = daemon.create_worker_enrollment("remote-host").unwrap();
    let mut registration = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enrollment,
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
        .unwrap();
    let worker_id = registration.worker_id;
    daemon
        .set_bucket_workers(bucket_id, &[0, worker_id], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project_id, &[worker_id], Some(worker_id))
        .unwrap();
    let credential = registration.credential.clone();
    let session_id = daemon
        .spawn_session(
            project_id,
            AgentKind::Test,
            "recover me",
            "keep working",
            None,
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    let terminal_id = match registration.rx.recv().await.unwrap() {
        ControllerMsg::Spawn {
            terminal_id,
            generation: 1,
            ..
        } => terminal_id,
        other => panic!("expected initial spawn, got {other:?}"),
    };
    let token = daemon.session_token(session_id).unwrap().unwrap();
    daemon.apply_worker_message(
        worker_id,
        WorkerMsg::HookReport {
            session_token: token,
            kind: pm_protocol::domain::HookKind::Started,
            detail: String::new(),
            agent_session_id: "native-session".into(),
            transcript_path: "/remote/transcript.jsonl".into(),
            req_id: 0,
            background_work: false,
        },
    );
    let before = daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    drop(registration);
    drop(daemon);

    let (reopened, _channels) = pm_daemon::Daemon::new(config()).unwrap();
    let reopened = std::sync::Arc::new(reopened);
    let snapshot = reopened.subscribe().0;
    let offline = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(offline.state, SessionState::AwaitingWorker);
    assert_eq!(
        offline.state_detail,
        "worker offline, resumes when it reconnects"
    );
    assert_eq!(
        offline.last_activity_at_unix_ms,
        before.last_activity_at_unix_ms
    );
    assert_eq!(
        snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.id == terminal_id)
            .unwrap()
            .generation,
        1
    );
    let owner = snapshot
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .unwrap();
    assert_eq!(owner.hostname, "host.local");
    assert!(!owner.online);
    assert_eq!(
        reopened
            .supervisor_list_sessions(bucket_id)
            .unwrap()
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .state,
        SessionState::AwaitingWorker
    );

    assert!(reopened.interrupt_session(session_id).is_err());
    assert!(reopened.kill_session(session_id).is_err());
    assert_eq!(
        reopened
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .state,
        SessionState::AwaitingWorker
    );

    let stale_inventory = [WorkerTerminal {
        terminal_id,
        generation: 99,
        kind: TerminalKind::Agent,
        state: TerminalRunState::Running,
        agent_resumable: true,
        transcript_available: false,
    }];
    let mut reconnected = reopened
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential: &credential,
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &stale_inventory,
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    assert!(matches!(
        reconnected.rx.recv().await,
        Some(ControllerMsg::TerminalKill {
            terminal_id: id,
            generation: 99,
        }) if id == terminal_id
    ));
    match reconnected.rx.recv().await.unwrap() {
        ControllerMsg::Spawn {
            session_id: resumed_session,
            terminal_id: resumed_terminal,
            generation: 2,
            resume_agent_session_id,
            ..
        } => {
            assert_eq!(resumed_session, session_id);
            assert_eq!(resumed_terminal, terminal_id);
            assert_eq!(resume_agent_session_id, "native-session");
        }
        other => panic!("expected one reconciled spawn, got {other:?}"),
    }
    assert!(reconnected.rx.try_recv().is_err());
    let sessions = reopened.subscribe().0.sessions;
    assert_eq!(
        sessions
            .iter()
            .filter(|session| session.id == session_id)
            .count(),
        1
    );
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .state,
        SessionState::Idle
    );

    reopened.disconnect_worker(&reconnected.link);
    assert_eq!(
        reopened
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .state,
        SessionState::AwaitingWorker
    );
    let current_inventory = [WorkerTerminal {
        terminal_id,
        generation: 2,
        kind: TerminalKind::Agent,
        state: TerminalRunState::Running,
        agent_resumable: true,
        transcript_available: false,
    }];
    let survivor = reopened
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential: &credential,
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &current_inventory,
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    assert!(survivor.rx.is_empty());
    assert_eq!(
        reopened.agent_terminal(session_id).unwrap().generation,
        2,
        "a surviving generation is adopted without another restart"
    );
    assert_eq!(
        reopened
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .state,
        SessionState::Idle,
        "a surviving resumed agent is ready for input"
    );
}

#[tokio::test]
async fn offline_derivation_is_scoped_to_each_remote_worker() {
    let env = daemon_env();
    let first = enroll(&env);
    let first_session = spawn_remote(&env, first.worker_id);
    let second = enroll_second_machine(&env);
    let second_session = spawn_remote(&env, second.worker_id);

    env.daemon.disconnect_worker(&first.link);
    let snapshot = env.daemon.subscribe().0;
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == first_session)
            .unwrap()
            .state,
        SessionState::AwaitingWorker
    );
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == second_session)
            .unwrap()
            .state,
        pm_protocol::domain::SessionState::Working
    );

    env.daemon.disconnect_worker(&second.link);
    assert!(env
        .daemon
        .subscribe()
        .0
        .sessions
        .iter()
        .filter(|session| { session.id == first_session || session.id == second_session })
        .all(|session| session.state == SessionState::AwaitingWorker));
}

#[tokio::test]
async fn removed_worker_sessions_do_not_follow_reenrollment() {
    let env = daemon_env();
    let registration = enroll(&env);
    let old_worker = registration.worker_id;
    let session_id = spawn_remote(&env, old_worker);
    env.daemon.disconnect_worker(&registration.link);
    env.daemon.remove_worker(old_worker).unwrap();

    let snapshot = env.daemon.subscribe().0;
    let removed = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(removed.state, SessionState::Failed);
    assert_eq!(removed.state_detail, "worker removed");
    assert!(!snapshot
        .workers
        .iter()
        .any(|worker| worker.id == old_worker));

    let replacement = enroll(&env);
    assert_eq!(
        env.daemon
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .state,
        SessionState::Failed
    );
    assert!(replacement.rx.is_empty());
}

#[tokio::test]
async fn dropping_a_bucket_host_repairs_and_republishes_its_projects() {
    let env = daemon_env();
    let registration = enroll(&env);
    let worker_id = registration.worker_id;
    allow_remote(&env, worker_id);
    let bucket_id = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|project| project.id == env.project_id)
        .unwrap()
        .bucket_id;
    let (_, mut events) = env.daemon.subscribe();

    env.daemon
        .set_bucket_workers(bucket_id, &[LOCAL_WORKER_ID], LOCAL_WORKER_ID, None)
        .unwrap();

    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|project| project.id == env.project_id)
        .unwrap();
    assert_eq!(
        project.worker_id,
        Some(LOCAL_WORKER_ID),
        "the pinned project moves to the bucket's remaining Host"
    );
    assert_eq!(project.allowed_worker_ids, vec![LOCAL_WORKER_ID]);

    let mut changed = Vec::new();
    while let Ok(Ok(event)) = tokio::time::timeout(TEST_TIMEOUT, events.recv()).await {
        if let Event::ProjectChanged(project) = event {
            changed.push(project.id);
        }
        if changed.contains(&env.project_id) {
            break;
        }
    }
    assert!(
        changed.contains(&env.project_id),
        "a repaired project must be republished so open views stop showing the dropped Host"
    );
}

#[tokio::test]
async fn remote_hook_and_terminal_activity_ordering_has_the_same_semantics_as_local() {
    let env = daemon_env();
    let mut registration = enroll(&env);

    for activity_first in [false, true] {
        let session_id = spawn_remote(&env, registration.worker_id);
        let (terminal_id, generation) = match registration.rx.recv().await.unwrap() {
            ControllerMsg::Spawn {
                terminal_id,
                generation,
                ..
            } => (terminal_id, generation),
            other => panic!("expected spawn, got {other:?}"),
        };
        let token = env.daemon.session_token(session_id).unwrap().unwrap();
        if activity_first {
            env.daemon.apply_worker_message(
                registration.worker_id,
                WorkerMsg::TerminalActivity {
                    terminal_id,
                    generation,
                },
            );
        }
        env.daemon.apply_worker_message(
            registration.worker_id,
            WorkerMsg::HookReport {
                session_token: token,
                kind: pm_protocol::domain::HookKind::TurnEnded,
                detail: String::new(),
                agent_session_id: String::new(),
                transcript_path: String::new(),
                req_id: 0,
                background_work: false,
            },
        );
        if !activity_first {
            env.daemon.apply_worker_message(
                registration.worker_id,
                WorkerMsg::TerminalActivity {
                    terminal_id,
                    generation,
                },
            );
        }

        let current = session(&env, session_id);
        assert_eq!(current.state, SessionState::Idle);
        assert!(current.last_agent_activity_at_unix_ms > 0);
    }
}

/// A remote agent's Stop hook must be able to receive the same report
/// nudge the controller-local path returns, or an agent on a worker can
/// finish a turn without ever naming its session on the dashboard.
#[tokio::test]
async fn a_relayed_turn_end_is_answered_with_the_report_nudge() {
    let env = daemon_env();
    let mut registration = enroll(&env);
    let session_id = spawn_remote(&env, registration.worker_id);
    assert!(matches!(
        registration.rx.recv().await.unwrap(),
        ControllerMsg::Spawn { .. }
    ));
    let token = env.daemon.session_token(session_id).unwrap().unwrap();

    env.daemon.apply_worker_message(
        registration.worker_id,
        WorkerMsg::HookReport {
            session_token: token.clone(),
            kind: pm_protocol::domain::HookKind::TurnEnded,
            detail: String::new(),
            agent_session_id: String::new(),
            transcript_path: String::new(),
            req_id: 7,
            background_work: false,
        },
    );

    match registration.rx.recv().await.unwrap() {
        ControllerMsg::HookResult { req_id, nudge } => {
            assert_eq!(req_id, 7);
            assert!(
                nudge.contains("headline"),
                "a turn that ended with no headline is nudged: {nudge}"
            );
        }
        other => panic!("expected a hook result, got {other:?}"),
    }
}

/// A worker that predates the reply sends req_id 0 and is not waiting for
/// one, so answering it would be a message nothing consumes.
#[tokio::test]
async fn a_relayed_hook_without_a_request_id_is_not_answered() {
    let env = daemon_env();
    let mut registration = enroll(&env);
    let session_id = spawn_remote(&env, registration.worker_id);
    assert!(matches!(
        registration.rx.recv().await.unwrap(),
        ControllerMsg::Spawn { .. }
    ));
    let token = env.daemon.session_token(session_id).unwrap().unwrap();

    env.daemon.apply_worker_message(
        registration.worker_id,
        WorkerMsg::HookReport {
            session_token: token,
            kind: pm_protocol::domain::HookKind::TurnEnded,
            detail: String::new(),
            agent_session_id: String::new(),
            transcript_path: String::new(),
            req_id: 0,
            background_work: false,
        },
    );

    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            registration.rx.recv()
        )
        .await
        .is_err(),
        "an unrequested reply must not be sent"
    );
    assert_eq!(session(&env, session_id).state, SessionState::Idle);
}

#[tokio::test]
async fn a_pending_remote_attach_does_not_block_other_commands() {
    let env = daemon_env();
    let mut registration = enroll(&env);
    let session_id = spawn_remote(&env, registration.worker_id);
    assert!(matches!(
        registration.rx.recv().await,
        Some(ControllerMsg::Spawn { .. })
    ));

    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let mut connection = pm_daemon::connection::ConnState::default();
    pm_daemon::connection::handle_message(
        &env.daemon,
        ClientEnvelope {
            seq: 1,
            msg: ClientMsg::AttachPty { session_id },
        },
        &out_tx,
        &mut connection,
    )
    .await;

    assert!(matches!(
        registration.rx.recv().await,
        Some(ControllerMsg::TerminalAttach { .. })
    ));

    pm_daemon::connection::handle_message(
        &env.daemon,
        ClientEnvelope {
            seq: 2,
            msg: ClientMsg::CreateBucket {
                name: "while-attaching".into(),
                allowed_worker_ids: vec![0],
                default_worker_id: 0,
                is_default: false,
            },
        },
        &out_tx,
        &mut connection,
    )
    .await;

    let response = tokio::time::timeout(std::time::Duration::from_millis(100), out_rx.recv())
        .await
        .expect("command was blocked behind remote replay")
        .expect("connection output ended");
    assert!(matches!(
        response,
        ServerMsg::CommandResult {
            seq: 2,
            result: Ok(Some(_)),
            ..
        }
    ));

    pm_daemon::connection::handle_message(
        &env.daemon,
        ClientEnvelope {
            seq: 3,
            msg: ClientMsg::DetachPty { session_id },
        },
        &out_tx,
        &mut connection,
    )
    .await;
    assert!(matches!(
        out_rx.recv().await,
        Some(ServerMsg::CommandResult {
            seq: 1,
            result: Err(_),
            ..
        })
    ));
    assert!(matches!(
        out_rx.recv().await,
        Some(ServerMsg::CommandResult {
            seq: 3,
            result: Ok(None),
            ..
        })
    ));
    connection.abort_all();
}

/// The renderer choice is the controller's, so it has to travel with a
/// remote spawn: a worker has no access to the setting.
#[tokio::test]
async fn a_remote_spawn_carries_the_renderer_setting() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;

    spawn_remote(&env, worker_id);
    match reg.rx.try_recv().unwrap() {
        ControllerMsg::Spawn { fullscreen, .. } => {
            assert!(!fullscreen, "the alternate screen is off by default")
        }
        other => panic!("expected spawn, got {other:?}"),
    }

    env.daemon
        .set_setting(pm_daemon::daemon::SETTING_SPAWN_FULLSCREEN, Some("true"))
        .unwrap();
    spawn_remote(&env, worker_id);
    match reg.rx.try_recv().unwrap() {
        ControllerMsg::Spawn { fullscreen, .. } => {
            assert!(fullscreen, "the setting reaches the next spawn")
        }
        other => panic!("expected spawn, got {other:?}"),
    }
}

#[tokio::test]
async fn a_remote_session_spawns_relays_pty_and_ends() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    assert!(env.daemon.workers.is_online(worker_id));
    assert!(worker(&env, worker_id).online, "worker shows online");

    let sid = spawn_remote(&env, worker_id);

    // The controller dispatched the spawn to the worker rather than the
    // local mux, carrying the spawn-truecolor setting.
    env.daemon
        .set_setting(pm_daemon::daemon::SETTING_SPAWN_TRUECOLOR, Some("false"))
        .unwrap();
    match reg.rx.try_recv().unwrap() {
        ControllerMsg::Spawn {
            session_id,
            truecolor,
            ..
        } => {
            assert_eq!(session_id, sid);
            assert!(truecolor, "spawned before the setting changed");
        }
        other => panic!("expected spawn, got {other:?}"),
    }
    assert!(
        !env.daemon.mux.is_running(sid),
        "no local pty for a remote session"
    );
    assert_eq!(
        session_state(&env, sid),
        pm_protocol::domain::SessionState::Working
    );

    // A viewer attaching asks the worker to start relaying and waits for
    // its scrollback replay. Drive the attach concurrently and simulate
    // the worker's reply.
    let daemon = env.daemon.clone();
    let attach = tokio::spawn(async move { daemon.attach(sid).await });
    let terminal_id = match tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerMsg::TerminalAttach {
            terminal_id,
            generation: 1,
            ..
        } => terminal_id,
        other => panic!("expected attach, got {other:?}"),
    };
    reg.link.feed_terminal_output(
        terminal_id,
        1,
        pm_protocol::terminal_frame::FLAG_REPLAY
            | pm_protocol::terminal_frame::FLAG_REPLAY_START
            | pm_protocol::terminal_frame::FLAG_REPLAY_END,
        bytes::Bytes::from_static(b"READY remote"),
    );
    let (replay, mut rx, _guard) = attach.await.unwrap().unwrap();
    assert_eq!(&replay[..], b"READY remote", "scrollback replay on attach");

    // Live output the worker relays reaches the attached viewer.
    reg.link
        .feed_terminal_output(terminal_id, 1, 0, bytes::Bytes::from_static(b" working"));
    await_output(&mut rx, replay.to_vec(), b"READY remote working").await;

    // A second viewer replays from the controller's warm mirror with no
    // further worker round-trip. The mirror serves screen state rather
    // than retained bytes, so the replay is read by rendering it.
    let (replay2, _rx2, _guard2) = env.daemon.attach(sid).await.unwrap();
    let rendered = String::from_utf8_lossy(&replay2);
    assert!(
        rendered.contains("READY remote working"),
        "replay was: {rendered:?}"
    );
    assert!(
        reg.rx.try_recv().is_err(),
        "a second viewer must not trigger another attach"
    );

    // A state the worker reports is applied.
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::SessionState {
            session_id: sid,
            state: SessionState::NeedsInput,
            detail: "which db?".into(),
        },
    );
    assert_eq!(session_state(&env, sid), SessionState::NeedsInput);

    // Viewer keystrokes route down to the worker.
    let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::channel(8);
    reg.link
        .connect_terminal_stream(terminal_id, 1, terminal_tx);
    env.daemon
        .pty_input(sid, bytes::Bytes::from_static(b"yes\r"));
    let input = tokio::time::timeout(TEST_TIMEOUT, terminal_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        pm_protocol::terminal_frame::decode(&input),
        Some(pm_protocol::terminal_frame::TerminalFrame::Input {
            generation: 1,
            submitted: false,
            data: b"yes\r",
        })
    ));

    // The worker reporting exit ends the session.
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::TerminalExit {
            terminal_id,
            generation: 1,
            exit_code: Some(0),
            state: TerminalRunState::Exited,
            transcript_available: false,
            transcript_size: 0,
            detail: String::new(),
        },
    );
    assert_eq!(session_state(&env, sid), SessionState::Exited);
}

/// A worker that stops an agent itself says why, and the session shows
/// that reason rather than a plain exit. A failed run it does not explain
/// is still read as a spawn failure.
#[tokio::test]
async fn a_failed_remote_exit_carries_the_workers_reason() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;

    let sid = spawn_remote(&env, worker_id);
    let terminal_id = match reg.rx.try_recv().unwrap() {
        ControllerMsg::Spawn { terminal_id, .. } => terminal_id,
        other => panic!("expected spawn, got {other:?}"),
    };
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::TerminalExit {
            terminal_id,
            generation: 1,
            exit_code: None,
            state: TerminalRunState::Failed,
            transcript_available: false,
            transcript_size: 0,
            detail: "agent cannot reach its puppet-master tools at http://127.0.0.1:1/mcp".into(),
        },
    );
    let failed = session(&env, sid);
    assert_eq!(failed.state, SessionState::Failed);
    assert_eq!(
        failed.state_detail,
        "agent cannot reach its puppet-master tools at http://127.0.0.1:1/mcp"
    );

    let sid = spawn_remote(&env, worker_id);
    let terminal_id = match reg.rx.try_recv().unwrap() {
        ControllerMsg::Spawn { terminal_id, .. } => terminal_id,
        other => panic!("expected spawn, got {other:?}"),
    };
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::TerminalExit {
            terminal_id,
            generation: 1,
            exit_code: None,
            state: TerminalRunState::Failed,
            transcript_available: false,
            transcript_size: 0,
            detail: String::new(),
        },
    );
    let unexplained = session(&env, sid);
    assert_eq!(unexplained.state, SessionState::Failed);
    assert_eq!(unexplained.state_detail, "worker failed to spawn terminal");
}

#[tokio::test]
async fn a_remote_shell_exit_persists_its_generation_and_can_restart() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    let sid = spawn_remote(&env, worker_id);
    let _ = reg.rx.recv().await;
    let terminal_id = env.daemon.create_shell(sid, "debug").unwrap();
    assert!(
        matches!(reg.rx.recv().await, Some(ControllerMsg::SpawnShell { terminal_id: id, generation: 1, .. }) if id == terminal_id)
    );

    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::TerminalExit {
            terminal_id,
            generation: 1,
            exit_code: Some(7),
            state: TerminalRunState::Exited,
            transcript_available: false,
            transcript_size: 0,
            detail: String::new(),
        },
    );
    let terminal = env.daemon.terminal(terminal_id).unwrap();
    assert_eq!(terminal.state, TerminalRunState::Exited);
    assert_eq!(terminal.exit_code, Some(7));
    assert_eq!(
        session_state(&env, sid),
        pm_protocol::domain::SessionState::Working,
        "shell exit must not end the agent session"
    );
    assert!(!terminal.scrollback_available);

    env.daemon.restart_terminal(terminal_id).unwrap();
    assert!(
        matches!(reg.rx.recv().await, Some(ControllerMsg::SpawnShell { terminal_id: id, generation: 2, .. }) if id == terminal_id)
    );
    assert_eq!(env.daemon.terminal(terminal_id).unwrap().generation, 2);
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::TerminalExit {
            terminal_id,
            generation: 2,
            exit_code: None,
            state: TerminalRunState::Failed,
            transcript_available: false,
            transcript_size: 0,
            detail: String::new(),
        },
    );
    assert_eq!(
        env.daemon.terminal(terminal_id).unwrap().state,
        TerminalRunState::Failed
    );
    env.daemon.restart_terminal(terminal_id).unwrap();
    assert!(
        matches!(reg.rx.recv().await, Some(ControllerMsg::SpawnShell { terminal_id: id, generation: 3, .. }) if id == terminal_id)
    );
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::TerminalExit {
            terminal_id,
            generation: 3,
            exit_code: Some(0),
            state: TerminalRunState::Exited,
            transcript_available: false,
            transcript_size: 0,
            detail: String::new(),
        },
    );
    assert!(env.daemon.terminal(terminal_id).is_err());
    assert_eq!(
        session_state(&env, sid),
        pm_protocol::domain::SessionState::Working,
        "clean shell exit must remove only the shell"
    );
    env.daemon.close_terminal(terminal_id).unwrap();
}

#[tokio::test]
async fn registration_records_the_reported_pm_build_and_a_missing_one_reads_as_unknown() {
    let env = daemon_env();

    let (token, _expires) = env.daemon.create_worker_enrollment("laptop").unwrap();
    let versioned = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "0.1.0+aaa1111",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    assert_eq!(
        worker(&env, versioned.worker_id).pm_version,
        "0.1.0+aaa1111"
    );

    env.daemon.disconnect_worker(&versioned.link);
    let reconnected = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential: &versioned.credential,
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "0.1.0+bbb2222",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    assert_eq!(reconnected.worker_id, versioned.worker_id);
    assert_eq!(
        worker(&env, versioned.worker_id).pm_version,
        "0.1.0+bbb2222",
        "a reconnect after an upgrade refreshes the stored build"
    );

    let (token, _expires) = env.daemon.create_worker_enrollment("old-box").unwrap();
    let versionless = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-old.local",
            hostname: "old.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    assert_ne!(versionless.worker_id, versioned.worker_id);
    assert_eq!(
        worker(&env, versionless.worker_id).pm_version,
        "",
        "a build that predates version reporting registers with no version"
    );

    assert_eq!(
        worker(&env, pm_protocol::domain::LOCAL_WORKER_ID).pm_version,
        pm_daemon::pm_build_version(),
        "the embedded local worker carries the controller's own build"
    );
}

#[tokio::test]
async fn spawning_on_an_offline_worker_is_refused() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    allow_remote(&env, worker_id);
    env.daemon.disconnect_worker(&reg.link);
    assert!(!env.daemon.workers.is_online(worker_id));
    assert!(!worker(&env, worker_id).online);

    let err = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "t",
            "p",
            None,
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            false,
            None,
        )
        .unwrap_err();
    assert!(err.to_string().contains("offline"), "{err}");
}

#[tokio::test]
async fn creating_a_shell_on_an_offline_worker_leaves_no_terminal() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let sid = spawn_remote(&env, reg.worker_id);
    assert!(matches!(
        reg.rx.recv().await,
        Some(ControllerMsg::Spawn { .. })
    ));
    let before = env.daemon.subscribe().0.terminals.len();

    env.daemon.disconnect_worker(&reg.link);
    let error = env.daemon.create_shell(sid, "offline").unwrap_err();

    assert!(error.to_string().contains("offline"), "{error}");
    assert_eq!(env.daemon.subscribe().0.terminals.len(), before);
}

#[tokio::test]
async fn a_spawn_without_a_worker_resolves_the_cascade() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    let bucket_id = env.daemon.subscribe().0.projects[0].bucket_id;

    // Bucket default routes an unspecified spawn to the remote worker.
    env.daemon
        .set_bucket_default_worker(bucket_id, worker_id)
        .unwrap();
    let sid = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "t",
            "p",
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    assert_eq!(session_worker(&env, sid), worker_id);
    assert!(matches!(
        reg.rx.try_recv().unwrap(),
        ControllerMsg::Spawn { .. }
    ));

    // A project override wins over the bucket default (here, back to local).
    env.daemon
        .set_project_worker(env.project_id, Some(0))
        .unwrap();
    let local = spawn_test_session(&env, "p");
    assert_eq!(session_worker(&env, local), 0);
}

#[tokio::test]
async fn a_directory_listing_proxies_to_the_worker() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    let mut rx = reg.rx;

    let daemon = env.daemon.clone();
    let task = tokio::spawn(async move { daemon.worker_fs_list(worker_id, "/srv".into()).await });

    let req_id = match tokio::time::timeout(TEST_TIMEOUT, rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerMsg::FsList { req_id, path } => {
            assert_eq!(path, "/srv");
            req_id
        }
        other => panic!("expected fs list, got {other:?}"),
    };

    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::FsListing {
            req_id,
            ok: true,
            error: String::new(),
            dir: "/srv".into(),
            parent: Some("/".into()),
            entries: vec![FsEntry {
                name: "api".into(),
                path: "/srv/api".into(),
            }],
        },
    );

    let listing = task.await.unwrap().unwrap();
    assert!(listing.ok);
    assert_eq!(listing.dir, "/srv");
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].path, "/srv/api");
}

#[tokio::test]
async fn a_remote_session_attachment_is_read_by_its_worker_and_stored() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    let session_id = spawn_remote(&env, worker_id);
    assert!(matches!(
        reg.rx.recv().await,
        Some(ControllerMsg::Spawn { .. })
    ));
    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|project| project.id == env.project_id)
        .unwrap();
    let item = env
        .daemon
        .upsert_item(
            project.bucket_id,
            &ItemWrite {
                bucket_id: project.bucket_id,
                title: Some("remote file".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .0;
    let bucket_id = project.bucket_id;
    let item_id = item.id;

    let daemon = env.daemon.clone();
    let task = tokio::spawn(async move {
        daemon
            .attach_item_file(
                session_id,
                bucket_id,
                item_id,
                "artifact.txt",
                None,
                Some("text/plain"),
            )
            .await
    });
    let req_id = match tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerMsg::FileRead {
            req_id,
            root,
            path,
            max_bytes,
        } => {
            assert_eq!(root, session(&env, session_id).cwd);
            assert_eq!(path, "artifact.txt");
            assert_eq!(
                max_bytes,
                pm_daemon::storage::ITEM_ATTACHMENT_FILE_MAX as u64
            );
            req_id
        }
        other => panic!("expected file read, got {other:?}"),
    };
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::FileRead {
            req_id,
            ok: true,
            error: String::new(),
            content: b"remote bytes".to_vec(),
            filename: "artifact.txt".into(),
        },
    );
    let attachment = task.await.unwrap().unwrap();
    assert_eq!(attachment.filename, "artifact.txt");
    assert_eq!(
        env.daemon
            .get_item_attachment(bucket_id, item_id, attachment.id)
            .unwrap()
            .content,
        b"remote bytes"
    );
}

#[tokio::test]
async fn a_disconnect_keeps_sessions_until_the_grace_elapses() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    let epoch = reg.epoch;
    let sid = spawn_remote(&env, worker_id);
    assert_eq!(
        session_state(&env, sid),
        pm_protocol::domain::SessionState::Working
    );

    // A disconnect alone does not fail the session; it awaits a reconnect.
    env.daemon.disconnect_worker(&reg.link);
    assert!(!env.daemon.workers.is_online(worker_id));
    assert_eq!(session_state(&env, sid), SessionState::AwaitingWorker);

    // The grace leaves a session the daemon still intends to resume alone.
    // Stamping it ended would drop it out of the snapshot's recent-session
    // window a minute later, taking work that is coming back with it.
    env.daemon.fail_worker_if_still_gone(worker_id, epoch);
    assert_eq!(session_state(&env, sid), SessionState::AwaitingWorker);
    assert_eq!(
        session(&env, sid).ended_at_unix_ms,
        None,
        "an offline worker is not evidence the session ended"
    );
}

/// The sidebar hides a session the daemon reports as ended, so publishing a
/// failure for one that is only unreachable makes live work vanish from the
/// list with nothing to say it is coming back.
#[tokio::test]
async fn the_grace_publishes_awaiting_worker_rather_than_a_failure() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    let epoch = reg.epoch;
    let sid = spawn_remote(&env, worker_id);

    env.daemon.disconnect_worker(&reg.link);
    let (_snapshot, mut events) = env.daemon.subscribe();
    env.daemon.fail_worker_if_still_gone(worker_id, epoch);

    while let Ok(event) = events.try_recv() {
        if let Event::SessionChanged(changed) = event {
            assert_ne!(
                changed.state,
                SessionState::Failed,
                "an offline worker must not be published as a session failure"
            );
        }
    }
    let view = session(&env, sid);
    assert_eq!(view.state, SessionState::AwaitingWorker);
    assert_eq!(
        view.state_detail,
        "worker offline, resumes when it reconnects"
    );
}

/// The exemption follows the auto-resume authority, not the disconnect: a
/// session killed just before the link dropped is not coming back, so the
/// grace still ends it.
#[tokio::test]
async fn the_grace_still_fails_a_session_the_daemon_will_not_resume() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    let epoch = reg.epoch;
    let resumed = spawn_remote(&env, worker_id);
    let abandoned = spawn_remote(&env, worker_id);

    // The kill reaches the worker and clears the intent to keep it running,
    // then the link drops before the exit is reported.
    env.daemon.kill_session(abandoned).unwrap();
    env.daemon.disconnect_worker(&reg.link);
    env.daemon.fail_worker_if_still_gone(worker_id, epoch);

    assert_eq!(session_state(&env, resumed), SessionState::AwaitingWorker);
    let ended = session(&env, abandoned);
    assert_eq!(ended.state, SessionState::Failed);
    assert_eq!(ended.state_detail, "worker did not reconnect");
    assert!(ended.ended_at_unix_ms.is_some());
}

#[tokio::test]
async fn a_reconnecting_worker_readopts_its_surviving_sessions() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    let credential = reg.credential.clone();
    let stale_epoch = reg.epoch;
    let sid = spawn_remote(&env, worker_id);

    env.daemon.disconnect_worker(&reg.link);

    // The worker returns announcing the session still runs.
    let reg2 = reconnect(&env, &credential, &[sid]);
    assert_eq!(
        reg2.worker_id, worker_id,
        "reconnect resolves the same worker"
    );
    assert!(env.daemon.workers.is_online(worker_id));
    assert_eq!(
        session_state(&env, sid),
        pm_protocol::domain::SessionState::Working,
        "survivor kept"
    );

    // The stale grace from the first connection no longer fires.
    env.daemon.fail_worker_if_still_gone(worker_id, stale_epoch);
    assert_eq!(
        session_state(&env, sid),
        pm_protocol::domain::SessionState::Working
    );

    // Even a survivor the controller had already failed is re-adopted.
    env.daemon.disconnect_worker(&reg2.link);
    env.daemon.fail_worker_if_still_gone(worker_id, reg2.epoch);
    assert_eq!(session_state(&env, sid), SessionState::AwaitingWorker);
    let reg3 = reconnect(&env, &credential, &[sid]);
    assert_eq!(
        session_state(&env, sid),
        pm_protocol::domain::SessionState::Working,
        "re-adopted"
    );
    let _ = reg3;
}

#[tokio::test]
async fn a_restarted_worker_resumes_a_missing_desired_session() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    let credential = reg.credential.clone();
    let sid = spawn_remote(&env, worker_id);
    let terminal_id = match reg.rx.recv().await.unwrap() {
        ControllerMsg::Spawn { terminal_id, .. } => terminal_id,
        other => panic!("expected spawn, got {other:?}"),
    };
    let token = env.daemon.session_token(sid).unwrap().unwrap();
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::HookReport {
            session_token: token,
            kind: pm_protocol::domain::HookKind::Started,
            detail: String::new(),
            agent_session_id: "native-session".into(),
            transcript_path: "/worker/session.jsonl".into(),
            req_id: 0,
            background_work: false,
        },
    );
    env.daemon.disconnect_worker(&reg.link);

    let (_, mut events) = env.daemon.subscribe();
    let mut reconnected = reconnect(&env, &credential, &[]);
    let mut recovered_terminal_seen = false;
    let mut recovered_session_seen = false;
    loop {
        match events.try_recv().unwrap() {
            Event::SessionChanged(session) if session.id == sid => {
                assert_eq!(session.state, SessionState::Idle);
                recovered_session_seen = true;
            }
            Event::TerminalChanged(terminal)
                if terminal.id == terminal_id && terminal.generation == 2 =>
            {
                recovered_terminal_seen = true;
            }
            Event::WorkerChanged(worker) if worker.id == worker_id && worker.online => {
                assert!(recovered_terminal_seen);
                assert!(recovered_session_seen);
                break;
            }
            _ => {}
        }
    }
    match reconnected.rx.recv().await.unwrap() {
        ControllerMsg::Spawn {
            session_id,
            terminal_id: resumed_terminal,
            generation,
            resume_agent_session_id,
            ..
        } => {
            assert_eq!(session_id, sid);
            assert_eq!(resumed_terminal, terminal_id);
            assert_eq!(generation, 2);
            assert_eq!(resume_agent_session_id, "native-session");
        }
        other => panic!("expected resumed spawn, got {other:?}"),
    }
    assert_eq!(session_state(&env, sid), SessionState::Idle);
}

#[tokio::test]
async fn reconnect_inventory_readopts_agent_and_shell_terminals() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    let credential = reg.credential.clone();
    let sid = spawn_remote(&env, worker_id);
    let agent_id = match reg.rx.recv().await.unwrap() {
        ControllerMsg::Spawn { terminal_id, .. } => terminal_id,
        other => panic!("expected spawn, got {other:?}"),
    };
    let shell_id = env.daemon.create_shell(sid, "debug").unwrap();
    assert!(matches!(
        reg.rx.recv().await,
        Some(ControllerMsg::SpawnShell { terminal_id, .. }) if terminal_id == shell_id
    ));
    env.daemon.disconnect_worker(&reg.link);

    let inventory = [
        WorkerTerminal {
            terminal_id: agent_id,
            generation: 1,
            kind: TerminalKind::Agent,
            state: TerminalRunState::Running,
            agent_resumable: true,
            transcript_available: false,
        },
        WorkerTerminal {
            terminal_id: shell_id,
            generation: 1,
            kind: TerminalKind::Shell,
            state: TerminalRunState::Running,
            agent_resumable: false,
            transcript_available: false,
        },
    ];
    let mut reconnected = reconnect_with_terminals(&env, &credential, &[sid], &inventory);
    assert!(reconnected.rx.try_recv().is_err());
    let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::channel(2);
    reconnected
        .link
        .connect_terminal_stream(shell_id, 1, terminal_tx);
    env.daemon
        .terminal_input(shell_id, bytes::Bytes::from_static(b"pwd\r"));
    assert!(matches!(
        pm_protocol::terminal_frame::decode(&terminal_rx.recv().await.unwrap()),
        Some(pm_protocol::terminal_frame::TerminalFrame::Input {
            generation: 1,
            submitted: false,
            data: b"pwd\r",
        })
    ));
}

#[tokio::test]
async fn a_restarted_worker_fails_sessions_it_no_longer_runs() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    let credential = reg.credential.clone();
    let sid = spawn_remote(&env, worker_id);

    env.daemon.disconnect_worker(&reg.link);
    // The worker process restarted: it comes back running nothing.
    reconnect(&env, &credential, &[]);
    assert_eq!(
        session_state(&env, sid),
        SessionState::Failed,
        "a session the worker no longer runs is failed"
    );
}

fn spawned_terminal(reg: &mut pm_daemon::daemon::WorkerRegistration) -> u64 {
    match reg.rx.try_recv().unwrap() {
        ControllerMsg::Spawn { terminal_id, .. } => terminal_id,
        other => panic!("expected spawn, got {other:?}"),
    }
}

fn running_agent(terminal_id: u64) -> WorkerTerminal {
    WorkerTerminal {
        terminal_id,
        generation: 1,
        kind: TerminalKind::Agent,
        state: TerminalRunState::Running,
        agent_resumable: false,
        transcript_available: false,
    }
}

fn terminal_state(env: &TestEnv, terminal_id: u64) -> TerminalRunState {
    env.daemon
        .subscribe()
        .0
        .terminals
        .into_iter()
        .find(|terminal| terminal.id == terminal_id)
        .unwrap()
        .state
}

/// A kill clears the intent to keep a session running before the worker
/// acts on it. If the worker restarts first, it comes back without the
/// session and nothing would ever report it gone, so the reconnect ends it.
#[tokio::test]
async fn a_reconnect_ends_a_killed_session_the_worker_no_longer_has() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    let credential = reg.credential.clone();
    let orphaned = spawn_remote(&env, worker_id);
    let orphaned_terminal = spawned_terminal(&mut reg);
    let dying = spawn_remote(&env, worker_id);
    let dying_terminal = spawned_terminal(&mut reg);
    let kept = spawn_remote(&env, worker_id);
    let kept_terminal = spawned_terminal(&mut reg);
    env.daemon.kill_session(orphaned).unwrap();
    env.daemon.kill_session(dying).unwrap();
    env.daemon.disconnect_worker(&reg.link);

    let (_, mut events) = env.daemon.subscribe();
    let mut reconnected = reconnect_with_terminals(
        &env,
        &credential,
        &[dying, kept],
        &[running_agent(dying_terminal), running_agent(kept_terminal)],
    );

    let ended = session(&env, orphaned);
    assert_eq!(ended.state, SessionState::Failed);
    assert_eq!(
        ended.state_detail,
        "worker reconnected without this session"
    );
    assert!(ended.ended_at_unix_ms.is_some());
    assert_eq!(
        terminal_state(&env, orphaned_terminal),
        TerminalRunState::Failed
    );
    let mut published_session = false;
    let mut published_terminal = false;
    while let Ok(event) = events.try_recv() {
        match event {
            Event::SessionChanged(changed) if changed.id == orphaned => {
                published_session |= changed.state == SessionState::Failed;
            }
            Event::TerminalChanged(changed) if changed.id == orphaned_terminal => {
                published_terminal |= changed.state == TerminalRunState::Failed;
            }
            _ => {}
        }
    }
    assert!(published_session, "the ended session was never published");
    assert!(published_terminal, "the ended terminal was never published");

    // An announced session is still the worker's to end: it is told to
    // kill it and reports the exit itself.
    assert!(session_state(&env, dying).is_live());
    match reconnected.rx.try_recv().unwrap() {
        ControllerMsg::TerminalKill {
            terminal_id,
            generation,
        } => {
            assert_eq!(terminal_id, dying_terminal);
            assert_eq!(generation, 1);
        }
        other => panic!("expected a kill for the announced session, got {other:?}"),
    }
    assert!(reconnected.rx.try_recv().is_err());
    assert_eq!(session_state(&env, kept), SessionState::Working);
    assert_eq!(
        terminal_state(&env, kept_terminal),
        TerminalRunState::Running
    );
}

/// A worker answers a kill for a terminal it no longer has with an exit,
/// and that answer can arrive after the terminal's real exit was recorded.
#[tokio::test]
async fn a_trailing_exit_does_not_overwrite_the_recorded_one() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    let sid = spawn_remote(&env, worker_id);
    let terminal_id = spawned_terminal(&mut reg);
    let exit = |exit_code| WorkerMsg::TerminalExit {
        terminal_id,
        generation: 1,
        exit_code,
        state: TerminalRunState::Exited,
        transcript_available: false,
        transcript_size: 0,
        detail: String::new(),
    };

    env.daemon.apply_worker_message(worker_id, exit(Some(3)));
    let recorded = session(&env, sid);
    assert_eq!(recorded.state, SessionState::Exited);
    assert_eq!(recorded.exit_code, Some(3));

    env.daemon.apply_worker_message(worker_id, exit(None));
    let after = session(&env, sid);
    assert_eq!(after.exit_code, Some(3));
    assert_eq!(after.ended_at_unix_ms, recorded.ended_at_unix_ms);
}

/// Allows a worker for the bucket and project without making it the
/// default anywhere, so explicit host selection is the only route to it.
fn allow_without_default(env: &TestEnv, worker_id: u64) {
    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == env.project_id)
        .unwrap();
    env.daemon
        .set_bucket_workers(project.bucket_id, &[0, worker_id], 0, None)
        .unwrap();
    env.daemon
        .set_project_workers(env.project_id, &[0, worker_id], None)
        .unwrap();
}

async fn spawn_with_host(env: &TestEnv, host: &str, worker_id: Option<u64>) -> Result<u64, String> {
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let mut connection = pm_daemon::connection::ConnState::default();
    pm_daemon::connection::handle_message(
        &env.daemon,
        ClientEnvelope {
            seq: 1,
            msg: ClientMsg::SpawnSession {
                project_id: env.project_id,
                agent: Some(AgentKind::Test),
                task_title: "t".into(),
                task_prompt: "p".into(),
                cwd: String::new(),
                permission_mode: PermissionMode::Inherit,
                worker_id,
                items_api: true,
                supervisor_api: false,
                model_profile_id: None,
                host: host.into(),
                initial_cols: None,
                initial_rows: None,
            },
        },
        &out_tx,
        &mut connection,
    )
    .await;
    match out_rx.recv().await.unwrap() {
        ServerMsg::CommandResult { result, .. } => result.map(|id| id.unwrap()),
        other => panic!("unexpected reply: {other:?}"),
    }
}

#[tokio::test]
async fn explicit_spawn_host_selects_a_configured_non_default_worker() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    let mut rx = answering_host_requests(env.daemon.clone(), worker_id, reg.rx);
    allow_without_default(&env, worker_id);
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/laptop/repo"))
        .unwrap();

    let by_name = spawn_with_host(&env, "laptop", None).await.unwrap();
    assert_eq!(session_worker(&env, by_name), worker_id);
    assert_eq!(
        session(&env, by_name).cwd,
        "/laptop/repo",
        "the per-worker path is the launch directory"
    );
    assert!(matches!(rx.recv().await, Some(ControllerMsg::Spawn { .. })));

    assert_eq!(
        env.daemon
            .resolve_spawn_host(env.project_id, &worker_id.to_string())
            .unwrap(),
        worker_id,
        "hosts resolve by id as well as name"
    );
}

#[tokio::test]
async fn explicit_spawn_host_rejects_workers_the_project_does_not_allow() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    allow_without_default(&env, worker_id);

    assert_eq!(
        env.daemon
            .resolve_spawn_host(env.project_id, "laptop")
            .unwrap(),
        worker_id,
        "an allowed worker is selectable without a per-worker path of its own"
    );

    let err = env
        .daemon
        .resolve_spawn_host(env.project_id, "no-such-host")
        .unwrap_err();
    match &err {
        pm_daemon::daemon::DaemonError::HostNotConfigured { requested, valid } => {
            assert_eq!(requested, "no-such-host");
            assert_eq!(
                valid,
                &[(0, "local".to_string()), (worker_id, "laptop".to_string())]
            );
        }
        other => panic!("expected HostNotConfigured, got {other:?}"),
    }

    let err = spawn_with_host(&env, "no-such-host", None)
        .await
        .unwrap_err();
    assert!(
        err.contains(&format!(
            "valid workers: local (id 0), laptop (id {worker_id})"
        )),
        "rejection lists the valid choices: {err}"
    );

    let err = spawn_with_host(&env, "local", Some(0)).await.unwrap_err();
    assert!(
        err.contains("cannot both be set"),
        "host and worker_id are mutually exclusive: {err}"
    );
}

/// A worker the project allows but holds no path for launches in the
/// project's own directory, which is what the dashboard promises when it
/// labels a blank per-worker path as inherited.
#[tokio::test]
async fn explicit_spawn_host_without_its_own_path_inherits_the_project_path() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    let mut rx = answering_host_requests(env.daemon.clone(), worker_id, reg.rx);
    allow_without_default(&env, worker_id);

    let id = spawn_with_host(&env, "laptop", None).await.unwrap();
    assert_eq!(session_worker(&env, id), worker_id);
    assert_eq!(
        session(&env, id).cwd,
        env.project_root().to_str().unwrap(),
        "the project's path is the launch directory"
    );
    assert!(matches!(rx.recv().await, Some(ControllerMsg::Spawn { .. })));
}

#[tokio::test]
async fn omitted_spawn_host_keeps_the_default_worker_resolution() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    allow_without_default(&env, worker_id);

    let inherited = spawn_with_host(&env, "", None).await.unwrap();
    assert_eq!(
        session_worker(&env, inherited),
        0,
        "no selection resolves the bucket default"
    );

    env.daemon
        .set_project_workers(env.project_id, &[0, worker_id], Some(worker_id))
        .unwrap();
    let overridden = spawn_with_host(&env, "", None).await.unwrap();
    assert_eq!(
        session_worker(&env, overridden),
        worker_id,
        "no selection resolves the project worker override"
    );
    // The effective default is always an explicit choice too.
    assert_eq!(
        env.daemon
            .resolve_spawn_host(env.project_id, "laptop")
            .unwrap(),
        worker_id
    );
}

#[tokio::test]
async fn project_worker_paths_set_clear_and_validate() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;

    let err = env
        .daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/laptop/repo"))
        .unwrap_err();
    assert!(
        err.to_string().contains("not allowed"),
        "paths only attach to allowed workers: {err}"
    );

    allow_without_default(&env, worker_id);
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/laptop/repo"))
        .unwrap();
    let paths = |env: &TestEnv| {
        env.daemon
            .subscribe()
            .0
            .projects
            .into_iter()
            .find(|p| p.id == env.project_id)
            .unwrap()
            .worker_paths
    };
    assert_eq!(paths(&env).len(), 1);
    assert_eq!(paths(&env)[0].worker_id, worker_id);
    assert_eq!(paths(&env)[0].path, "/laptop/repo");

    // Clearing falls back to the project path instead of persisting an
    // empty string, whether cleared with None or an empty value.
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some(""))
        .unwrap();
    assert!(paths(&env).is_empty());
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/laptop/repo"))
        .unwrap();
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, None)
        .unwrap();
    assert!(paths(&env).is_empty());
}

/// Drives the client protocol message the pm CLI sends, so worker
/// references resolve by id or name through the daemon connection.
async fn set_worker_path_msg(
    env: &TestEnv,
    worker: &str,
    path: Option<&str>,
) -> Result<(), String> {
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
    let mut connection = pm_daemon::connection::ConnState::default();
    pm_daemon::connection::handle_message(
        &env.daemon,
        ClientEnvelope {
            seq: 1,
            msg: ClientMsg::SetProjectWorkerPath {
                project_id: env.project_id,
                worker: worker.into(),
                path: path.map(str::to_string),
            },
        },
        &out_tx,
        &mut connection,
    )
    .await;
    match out_rx.recv().await.unwrap() {
        ServerMsg::CommandResult { result, .. } => result.map(|_| ()),
        other => panic!("unexpected reply: {other:?}"),
    }
}

#[tokio::test]
async fn client_protocol_sets_and_clears_worker_paths_by_id_or_name() {
    let env = daemon_env();
    let reg = enroll(&env);
    let worker_id = reg.worker_id;
    allow_without_default(&env, worker_id);
    let paths = |env: &TestEnv| {
        env.daemon
            .subscribe()
            .0
            .projects
            .into_iter()
            .find(|p| p.id == env.project_id)
            .unwrap()
            .worker_paths
    };

    set_worker_path_msg(&env, "laptop", Some("/laptop/repo"))
        .await
        .unwrap();
    assert_eq!(paths(&env)[0].worker_id, worker_id);
    assert_eq!(paths(&env)[0].path, "/laptop/repo");

    set_worker_path_msg(&env, &worker_id.to_string(), Some("/laptop/repo-v2"))
        .await
        .unwrap();
    assert_eq!(paths(&env)[0].path, "/laptop/repo-v2");

    set_worker_path_msg(&env, "laptop", None).await.unwrap();
    assert!(paths(&env).is_empty());

    let err = set_worker_path_msg(&env, "desk-vm", Some("/x"))
        .await
        .unwrap_err();
    assert!(
        err.contains("allowed workers") && err.contains("laptop"),
        "an unknown reference is rejected with the allowed workers: {err}"
    );
}

/// Re-enrolling is how an existing host moves onto mutual TLS, so it must
/// not read as a new machine. The row keeps its id, and everything attached
/// to that id — the bucket default, the project override, the per-host path,
/// and the session history — survives the rotation.
#[tokio::test]
async fn re_enrolling_rotates_a_host_in_place_and_keeps_what_points_at_it() {
    let env = daemon_env();
    let first = enroll(&env);
    let worker_id = first.worker_id;
    let bucket_id = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == env.project_id)
        .unwrap()
        .bucket_id;
    env.daemon
        .set_bucket_workers(bucket_id, &[0, worker_id], 0, None)
        .unwrap();
    env.daemon
        .set_project_workers(env.project_id, &[worker_id], Some(worker_id))
        .unwrap();
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/laptop/repo"))
        .unwrap();
    env.daemon.disconnect_worker(&first.link);

    let (token, _expires) = env.daemon.create_worker_reenrollment(worker_id).unwrap();
    let second = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "rotated-key",
            hostname: "host.local",
            platform: "linux",
            pm_version: "0.2.0+ccc3333",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();

    assert_eq!(
        second.worker_id, worker_id,
        "re-enrollment must rotate the existing host, not add another"
    );
    assert_ne!(
        second.credential, first.credential,
        "re-enrollment issues a fresh credential"
    );
    assert_eq!(worker(&env, worker_id).name, "laptop", "the name is kept");
    assert_eq!(worker(&env, worker_id).pm_version, "0.2.0+ccc3333");

    let snapshot = env.daemon.subscribe().0;
    let project = snapshot
        .projects
        .iter()
        .find(|p| p.id == env.project_id)
        .unwrap();
    assert_eq!(project.worker_id, Some(worker_id));
    assert_eq!(project.worker_paths[0].path, "/laptop/repo");
    assert!(snapshot
        .buckets
        .iter()
        .find(|b| b.id == bucket_id)
        .unwrap()
        .allowed_worker_ids
        .contains(&worker_id));
}

/// Adding a Host for a machine that is already enrolled would give one
/// machine two ids, and the per-worker project path stays on the old one —
/// the host comes back looking configured but launches from nowhere. The
/// key proves it is the same machine, so the second enrollment is refused
/// and says which Host to re-enroll instead.
#[tokio::test]
async fn enrolling_a_machine_that_is_already_a_host_is_refused_and_keeps_its_path() {
    let env = daemon_env();
    let first = enroll(&env);
    let worker_id = first.worker_id;
    allow_without_default(&env, worker_id);
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/Users/admin/api"))
        .unwrap();
    env.daemon.disconnect_worker(&first.link);

    let (token, _expires) = env.daemon.create_worker_enrollment("laptop-again").unwrap();
    let err = env
        .daemon
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
        });
    let err = match err {
        Ok(_) => panic!("a machine already enrolled must not become a second Host"),
        Err(err) => err.to_string(),
    };
    assert!(
        err.contains("already enrolled as host") && err.contains("laptop"),
        "the refusal names the Host to re-enroll: {err}"
    );

    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == env.project_id)
        .unwrap();
    assert_eq!(
        project.worker_paths,
        vec![pm_protocol::domain::ProjectPath {
            worker_id,
            path: "/Users/admin/api".into()
        }],
        "the configured path stays attached to the Host that owns it"
    );

    // The machine still reconnects as the Host it enrolled as.
    let again = reconnect(&env, &first.credential, &[]);
    assert_eq!(again.worker_id, worker_id);
}

/// The old credential is what an attacker would replay after an operator
/// rotates a host, and the old key is what they would present.
#[tokio::test]
async fn a_rotated_host_stops_accepting_its_previous_key_and_credential() {
    let env = daemon_env();
    let first = enroll(&env);
    env.daemon.disconnect_worker(&first.link);
    let (token, _) = env
        .daemon
        .create_worker_reenrollment(first.worker_id)
        .unwrap();
    let second = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "rotated-key",
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
        .unwrap();
    env.daemon.disconnect_worker(&second.link);

    let stale_key = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential: &second.credential,
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
        });
    assert!(stale_key.is_err(), "the replaced key no longer resolves");

    let stale_credential = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential: &first.credential,
            peer_key_hash: "rotated-key",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        });
    assert!(
        stale_credential.is_err(),
        "the replaced credential is refused even from the current key"
    );
}

/// The handshake proves the key, so registration must not fall back to
/// trusting a credential on its own.
#[tokio::test]
async fn registration_without_a_proven_key_is_refused() {
    let env = daemon_env();
    let registered = enroll(&env);
    env.daemon.disconnect_worker(&registered.link);

    let unauthenticated = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential: &registered.credential,
            peer_key_hash: "",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        });
    assert!(
        unauthenticated.is_err(),
        "a credential alone must not register a host"
    );

    let unknown_key = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential: &registered.credential,
            peer_key_hash: "some-other-key",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        });
    assert!(
        unknown_key.is_err(),
        "a valid credential from an unpinned key must not register a host"
    );
}

/// A host the controller dials has no route to the browser plane, so its
/// agents' reports come up the control link instead. They must be answered
/// exactly as a direct post would be.
#[tokio::test]
async fn a_relayed_agent_report_is_answered_over_the_control_link() {
    let env = daemon_env();
    let mut registration = enroll(&env);
    let session_id = spawn_remote(&env, registration.worker_id);
    match registration.rx.recv().await.unwrap() {
        ControllerMsg::Spawn { .. } => {}
        other => panic!("expected the remote spawn, got {other:?}"),
    }
    let token = env.daemon.session_token(session_id).unwrap().unwrap();

    env.daemon.apply_worker_message(
        registration.worker_id,
        WorkerMsg::McpRequest {
            req_id: 7,
            bearer: token.clone(),
            body: serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "report",
                    "arguments": { "headline": "relayed through the control link" }
                }
            })
            .to_string(),
        },
    );

    let answer = tokio::time::timeout(std::time::Duration::from_secs(5), registration.rx.recv())
        .await
        .expect("the controller answers a relayed report")
        .unwrap();
    match answer {
        ControllerMsg::McpResponse {
            req_id,
            status,
            body,
        } => {
            assert_eq!(req_id, 7, "the answer is correlated to its request");
            assert_eq!(status, 200);
            assert!(
                body.contains("\"result\""),
                "expected a JSON-RPC result, got {body}"
            );
        }
        other => panic!("expected the relayed answer, got {other:?}"),
    }
    assert_eq!(
        session(&env, session_id).headline,
        "relayed through the control link",
        "a relayed report must take effect exactly as a direct one does"
    );
}

/// A body that is not a request at all must be refused rather than reaching
/// the tool layer.
#[tokio::test]
async fn a_relayed_report_that_is_not_a_request_is_refused() {
    let env = daemon_env();
    let mut registration = enroll(&env);
    env.daemon.apply_worker_message(
        registration.worker_id,
        WorkerMsg::McpRequest {
            req_id: 3,
            bearer: "no-such-token".into(),
            body: "not json at all".into(),
        },
    );
    match tokio::time::timeout(std::time::Duration::from_secs(5), registration.rx.recv())
        .await
        .expect("the controller answers")
        .unwrap()
    {
        ControllerMsg::McpResponse {
            req_id: 3,
            status,
            body,
        } => {
            assert_eq!(status, 400);
            assert!(body.is_empty());
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A remote agent that is not installed on the host fails to spawn, and
/// that is precisely when an operator opens a shell there: to find out
/// whether the binary exists at all. The shell must still reach the
/// worker.
#[tokio::test]
async fn a_shell_reaches_a_remote_host_whose_agent_failed_to_spawn() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let worker_id = reg.worker_id;
    let sid = spawn_remote(&env, worker_id);
    assert!(matches!(
        reg.rx.try_recv().unwrap(),
        ControllerMsg::Spawn { .. }
    ));

    // The host reports what a missing agent binary looks like.
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::SessionState {
            session_id: sid,
            state: SessionState::Failed,
            detail: "spawn failed: Unable to spawn claude because it doesn't exist on the \
                     filesystem and was not found in PATH"
                .into(),
        },
    );
    assert_eq!(session_state(&env, sid), SessionState::Failed);

    let terminal = env
        .daemon
        .create_shell(sid, "Shell")
        .expect("a shell should open on a host whose agent never started");
    match reg.rx.try_recv().unwrap() {
        ControllerMsg::SpawnShell { terminal_id, .. } => assert_eq!(terminal_id, terminal),
        other => panic!("expected the shell to be dispatched to the host, got {other:?}"),
    }
}

/// A host reporting that a session failed must not make the session
/// disappear. A snapshot keeps a failed session only while its end is
/// recent, so a state change that never stamps one hides the session
/// the instant it fails, and with it every action an operator would
/// take next.
#[tokio::test]
/// A worker-hosted session is where nearly every agent runs, so the
/// turn-end flag has to survive the trip across the control link, not
/// just the local hook path.
async fn a_workers_hook_report_carries_background_work_to_the_controller() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "worker background work");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    // The shape a worker sends when its agent ended a turn with a
    // subagent still running.
    let reported = pm_protocol::domain::WorkerMsg::HookReport {
        session_token: token.clone(),
        kind: pm_protocol::domain::HookKind::TurnEnded,
        detail: String::new(),
        agent_session_id: String::new(),
        transcript_path: String::new(),
        req_id: 0,
        background_work: true,
    };
    let back = pm_protocol::domain::WorkerMsg::decode(&reported.clone().encode_to_vec()).unwrap();
    assert_eq!(back, reported, "the flag must survive the wire");

    // And the controller acts on it: the session it names is working,
    // not idle, once the report lands.
    env.daemon
        .handle_hook_event(
            &token,
            pm_protocol::domain::HookKind::TurnEnded,
            "",
            "",
            "",
            true,
        )
        .unwrap();
    assert_eq!(
        env.daemon.get_session_exact(id).unwrap().state,
        pm_protocol::domain::SessionState::Working
    );
}

#[tokio::test]
async fn a_remote_session_that_fails_stays_visible() {
    let env = daemon_env();
    let mut reg = enroll(&env);
    let sid = spawn_remote(&env, reg.worker_id);
    let _ = reg.rx.try_recv();

    env.daemon.apply_worker_message(
        reg.worker_id,
        WorkerMsg::SessionState {
            session_id: sid,
            state: SessionState::Failed,
            detail: "spawn failed".into(),
        },
    );

    let listed = env.daemon.subscribe().0.sessions;
    let found = listed.iter().find(|s| s.id == sid);
    assert!(
        found.is_some(),
        "a session that just failed should still be listed, got {:?}",
        listed.iter().map(|s| (s.id, s.state)).collect::<Vec<_>>()
    );
    assert_eq!(found.unwrap().state, SessionState::Failed);
}

#[tokio::test]
async fn spawn_initial_size_is_gated_on_worker_protocol_version() {
    let env = daemon_env();

    let (token, _) = env.daemon.create_worker_enrollment("old-worker").unwrap();
    let mut old_reg = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-old.local",
            hostname: "old.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_SPAWN_INITIAL_SIZE - 1,
        })
        .unwrap();
    allow_remote(&env, old_reg.worker_id);

    let _ = env
        .daemon
        .spawn_session_with_agent_override(
            env.project_id,
            Some(AgentKind::Test),
            "t",
            "p",
            None,
            PermissionMode::Inherit,
            Some(old_reg.worker_id),
            true,
            false,
            None,
            None,
            Some((140, 40)),
        )
        .unwrap();

    match old_reg.rx.recv().await.unwrap() {
        ControllerMsg::Spawn {
            initial_cols,
            initial_rows,
            ..
        } => {
            assert_eq!(initial_cols, None);
            assert_eq!(initial_rows, None);
        }
        other => panic!("expected Spawn, got {other:?}"),
    }

    let (token2, _) = env.daemon.create_worker_enrollment("new-worker").unwrap();
    let mut new_reg = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token2,
            credential: "",
            peer_key_hash: "key-new.local",
            hostname: "new.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_SPAWN_INITIAL_SIZE,
        })
        .unwrap();
    allow_remote(&env, new_reg.worker_id);

    let _ = env
        .daemon
        .spawn_session_with_agent_override(
            env.project_id,
            Some(AgentKind::Test),
            "t",
            "p",
            None,
            PermissionMode::Inherit,
            Some(new_reg.worker_id),
            true,
            false,
            None,
            None,
            Some((140, 40)),
        )
        .unwrap();

    match new_reg.rx.recv().await.unwrap() {
        ControllerMsg::Spawn {
            initial_cols,
            initial_rows,
            ..
        } => {
            assert_eq!(initial_cols, Some(140));
            assert_eq!(initial_rows, Some(40));
        }
        other => panic!("expected Spawn, got {other:?}"),
    }
}

/// The mint-only API is what bounds an attack: nothing reads an existing secret
/// back, so anything holding a session makes a new credential rather than
/// stealing one. The catch is that the new one is keyed to itself, so it
/// outlives the password and the cookie it was minted with, and revocation is
/// the only undo. Revocation needs the user to know, and until this a host
/// joining or being replaced produced a log line and a row in a list they had
/// to think to open.
#[tokio::test]
async fn enrolling_or_replacing_a_host_tells_the_user() {
    use pm_protocol::domain::SecurityNoticeKind;
    let env = daemon_env();
    let (_snapshot, mut events) = env.daemon.subscribe();

    let first = enroll(&env);
    let notice = next_security_notice(&mut events).await;
    assert_eq!(notice.kind, SecurityNoticeKind::HostEnrolled);
    assert!(
        notice.detail.contains("dispatched sessions"),
        "{}",
        notice.detail
    );

    // Redeeming a re-enrollment with a different key is the silent one: same
    // id, same name, same project paths, a different machine.
    let (token, _) = env
        .daemon
        .create_worker_reenrollment(first.worker_id)
        .unwrap();
    env.daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-somebody-else",
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
        .unwrap();
    let notice = next_security_notice(&mut events).await;
    assert_eq!(notice.kind, SecurityNoticeKind::HostKeyReplaced);
    assert!(
        notice.detail.contains("different machine"),
        "{}",
        notice.detail
    );
}

/// A session rewriting the standing instructions is the product's durable
/// prompt-injection surface: one injected page becomes what every session in the
/// bucket is launched with. A human doing it from the UI is not news.
#[tokio::test]
async fn only_a_session_rewriting_instructions_tells_the_user() {
    use pm_protocol::domain::{InstructionTarget, SecurityNoticeKind};
    let env = daemon_env();
    let bucket = bucket_of(&env);
    // The writer is recorded against the session that wrote it, so it has to be
    // a real one.
    let writer = spawn_test_session(&env, "rewrite the instructions");
    let (_snapshot, mut events) = env.daemon.subscribe();

    // The person at the keyboard, which raises nothing.
    env.daemon
        .set_instructions(bucket, None, InstructionTarget::All, "by hand", 0, "", None)
        .unwrap();

    // A session, which does. Each target is its own layer with its own
    // revision, so the supervisor layer's first write expects 0.
    let layer = env
        .daemon
        .set_instructions(
            bucket,
            None,
            InstructionTarget::Supervisor,
            "by an agent",
            0,
            "",
            Some(writer),
        )
        .unwrap();
    assert_eq!(layer.revision, 1);
    let notice = next_security_notice(&mut events).await;
    assert_eq!(notice.kind, SecurityNoticeKind::InstructionsRewritten);
    assert!(
        notice.detail.contains(&format!("session {writer}")),
        "{}",
        notice.detail
    );
    assert!(
        notice.detail.contains("every project in this bucket"),
        "bucket scope is the part worth naming: {}",
        notice.detail
    );
}

/// Skips the other events a registration or a write publishes, and fails rather
/// than hanging if no notice arrives.
async fn next_security_notice(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
) -> pm_protocol::domain::SecurityNotice {
    let deadline = std::time::Duration::from_secs(5);
    tokio::time::timeout(deadline, async {
        loop {
            match events.recv().await.expect("the event bus stayed open") {
                Event::SecurityNotice(notice) => return notice,
                _ => continue,
            }
        }
    })
    .await
    .expect("a security notice was published")
}

#[tokio::test]
async fn remote_supervisor_snooze_and_prompt_hooks_control_completion_notifications() {
    use pm_protocol::domain::{HookKind, SessionRole};
    let env = daemon_env();
    let mut registration = enroll(&env);
    let id = spawn_remote(&env, registration.worker_id);
    let (terminal_id, generation) = match registration.rx.recv().await.unwrap() {
        ControllerMsg::Spawn {
            terminal_id,
            generation,
            ..
        } => (terminal_id, generation),
        other => panic!("expected spawn, got {other:?}"),
    };
    env.daemon
        .update_session_role_apis(id, None, Some(true), Some(SessionRole::Supervisor))
        .unwrap();
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let hook = |kind| {
        env.daemon.apply_worker_message(
            registration.worker_id,
            WorkerMsg::HookReport {
                session_token: token.clone(),
                kind,
                detail: String::new(),
                agent_session_id: String::new(),
                transcript_path: String::new(),
                req_id: 0,
                background_work: false,
            },
        )
    };
    hook(HookKind::PromptSubmitted);
    env.daemon.snooze_supervision(id, 5).unwrap();
    hook(HookKind::TurnEnded);
    assert_eq!(session(&env, id).state, SessionState::Idle);
    assert!(!session(&env, id).idle_unseen);
    hook(HookKind::PromptSubmitted);
    env.daemon.snooze_supervision(id, 5).unwrap();
    hook(HookKind::PromptSubmitted);
    hook(HookKind::TurnEnded);
    assert!(session(&env, id).idle_unseen);
    hook(HookKind::PromptSubmitted);
    env.daemon.snooze_supervision(id, 5).unwrap();
    let (_, mut events) = env.daemon.subscribe();
    env.daemon.apply_worker_message(
        registration.worker_id,
        WorkerMsg::TerminalExit {
            terminal_id,
            generation,
            exit_code: Some(1),
            state: TerminalRunState::Failed,
            transcript_available: false,
            transcript_size: 0,
            detail: "agent crashed".into(),
        },
    );
    assert_eq!(session(&env, id).state, SessionState::Failed);
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(event,
        Event::SessionAlert(alert) if alert.kind == pm_protocol::domain::SessionAlertKind::Failed))
    );
}
