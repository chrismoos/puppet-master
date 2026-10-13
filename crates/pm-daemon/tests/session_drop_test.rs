//! Files dropped onto a session from the dashboard: stored on the host
//! that runs the session, with the path typed into its composer.

mod support;

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use bytes::Bytes;
use pm_protocol::domain::{
    AgentKind, ControllerMsg, PathCheck, PermissionMode, TerminalKind, WorkerMsg,
};
use pm_protocol::terminal_frame::{self, TerminalFrame};
use pm_protocol::{WORKER_PROTOCOL_SESSION_DROP, WORKER_PROTOCOL_VERSION};
use support::{await_output, daemon_env, signed_in_bearer, spawn_test_session, TestEnv};
use tokio::sync::mpsc;
use tower::util::ServiceExt;

/// What the controller asked a host to store.
#[derive(Debug, Clone)]
struct StoredDrop {
    session_id: u64,
    filename: String,
    content: Vec<u8>,
}

/// A simulated host that answers the path check a spawn needs and
/// stores whatever drops arrive, reporting a path of its own choosing.
struct FakeHost {
    worker_id: u64,
    link: Arc<pm_daemon::workers::WorkerLink>,
    drops: Arc<Mutex<Vec<StoredDrop>>>,
}

fn enroll_host(env: &TestEnv, protocol_version: u32) -> FakeHost {
    let (token, _expires) = env.daemon.create_worker_enrollment("remote").unwrap();
    let registration = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-remote",
            hostname: "remote",
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
    let host = FakeHost {
        worker_id: registration.worker_id,
        link: registration.link,
        drops: Arc::new(Mutex::new(Vec::new())),
    };
    let daemon = env.daemon.clone();
    let worker_id = host.worker_id;
    let recorded = host.drops.clone();
    let mut rx = registration.rx;
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                ControllerMsg::PathCheck { req_id, .. } => {
                    daemon.apply_worker_message(
                        worker_id,
                        WorkerMsg::PathChecked {
                            req_id,
                            status: PathCheck::Ok,
                            detail: String::new(),
                        },
                    );
                }
                ControllerMsg::SessionDrop {
                    req_id,
                    session_id,
                    filename,
                    content,
                } => {
                    recorded.lock().unwrap().push(StoredDrop {
                        session_id,
                        filename: filename.clone(),
                        content,
                    });
                    daemon.apply_worker_message(
                        worker_id,
                        WorkerMsg::SessionDropped {
                            req_id,
                            ok: true,
                            error: String::new(),
                            path: format!("/host/drops/session-{session_id}/{filename}"),
                        },
                    );
                }
                _ => {}
            }
        }
    });
    host
}

fn route_to(env: &TestEnv, worker_id: u64) {
    let snapshot = env.daemon.subscribe().0;
    let project = snapshot
        .projects
        .iter()
        .find(|p| p.id == env.project_id)
        .unwrap()
        .clone();
    env.daemon
        .set_bucket_workers(project.bucket_id, &[0, worker_id], worker_id, None)
        .unwrap();
    env.daemon
        .set_project_workers(env.project_id, &[worker_id], Some(worker_id))
        .unwrap();
}

fn spawn_remote_session(env: &TestEnv, worker_id: u64) -> u64 {
    env.daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "drop target",
            "hold still",
            None,
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            false,
            None,
        )
        .unwrap()
}

fn agent_terminal(env: &TestEnv, session_id: u64) -> pm_protocol::domain::Terminal {
    env.daemon
        .subscribe()
        .0
        .terminals
        .into_iter()
        .find(|t| t.session_id == session_id && t.kind == TerminalKind::Agent)
        .expect("the session has an agent terminal")
}

/// A remote terminal nothing is watching has no upstream, so a write to
/// it lands nowhere. This is what a viewer with the pane open provides.
fn attach_remote_terminal(
    env: &TestEnv,
    host: &FakeHost,
    session_id: u64,
) -> mpsc::Receiver<Bytes> {
    let terminal = agent_terminal(env, session_id);
    let (tx, frames) = mpsc::channel(64);
    host.link
        .connect_terminal_stream(terminal.id, terminal.generation, tx);
    frames
}

fn typed_bytes(frames: &mut mpsc::Receiver<Bytes>) -> Vec<u8> {
    let mut written = Vec::new();
    while let Ok(frame) = frames.try_recv() {
        if let Some(TerminalFrame::Input { data, .. }) = terminal_frame::decode(&frame) {
            written.extend_from_slice(data);
        }
    }
    written
}

fn drop_uri(session: u64, filename: &str) -> String {
    format!("/api/sessions/{session}/drops?filename={filename}")
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_local_drop_is_stored_and_its_path_typed_into_the_composer() {
    let env = daemon_env();
    let auth = signed_in_bearer(&env.daemon);
    let session = spawn_test_session(&env, "drop-target");
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let (replay, mut rx) = env.daemon.mux.attach(session).unwrap();
    let acc = await_output(&mut rx, replay.to_vec(), b"READY drop-target").await;

    let response = app
        .oneshot(
            Request::post(drop_uri(session, "Screen%20Shot.png"))
                .header(header::AUTHORIZATION, &auth)
                .body(Body::from("png-bytes"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    let path = body["path"].as_str().unwrap().to_owned();
    assert_eq!(body["typed"], serde_json::Value::Bool(true));
    assert!(path.ends_with("/Screen-Shot.png"), "{path}");
    assert_eq!(std::fs::read(&path).unwrap(), b"png-bytes");

    // A PTY echoes what it is given, so this is the composer receiving
    // the path rather than the write merely being accepted.
    await_output(&mut rx, acc, path.as_bytes()).await;
}

#[tokio::test]
async fn a_drop_is_refused_without_the_dashboard_credential() {
    let env = daemon_env();
    let _signed_in = signed_in_bearer(&env.daemon);
    let session = spawn_test_session(&env, "drop-auth");
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let response = app
        .oneshot(
            Request::post(drop_uri(session, "shot.png"))
                .body(Body::from("bytes"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_drop_outside_the_size_limits_is_refused() {
    let env = daemon_env();
    let auth = signed_in_bearer(&env.daemon);
    let session = spawn_test_session(&env, "drop-limit");
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let oversized = app
        .clone()
        .oneshot(
            Request::post(drop_uri(session, "big.bin"))
                .header(header::AUTHORIZATION, &auth)
                .body(Body::from(vec![
                    b'x';
                    pm_daemon::drops::SESSION_DROP_FILE_MAX + 1
                ]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let empty = app
        .oneshot(
            Request::post(drop_uri(session, "empty.bin"))
                .header(header::AUTHORIZATION, &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_drop_cannot_name_another_sessions_terminal() {
    let env = daemon_env();
    let auth = signed_in_bearer(&env.daemon);
    let mine = spawn_test_session(&env, "drop-mine");
    let theirs = spawn_test_session(&env, "drop-theirs");
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let terminal = agent_terminal(&env, theirs);

    let response = app
        .oneshot(
            Request::post(format!(
                "{}&terminal={}",
                drop_uri(mine, "shot.png"),
                terminal.id
            ))
            .header(header::AUTHORIZATION, &auth)
            .body(Body::from("bytes"))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("belongs to another session"),
        "{body}"
    );
}

#[tokio::test]
async fn a_remote_drop_is_stored_by_the_host_that_runs_the_session() {
    let env = daemon_env();
    let auth = signed_in_bearer(&env.daemon);
    let host = enroll_host(&env, WORKER_PROTOCOL_VERSION);
    route_to(&env, host.worker_id);
    let session = spawn_remote_session(&env, host.worker_id);
    let mut frames = attach_remote_terminal(&env, &host, session);
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let response = app
        .oneshot(
            Request::post(drop_uri(session, "shot.png"))
                .header(header::AUTHORIZATION, &auth)
                .body(Body::from("remote-bytes"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    let path = format!("/host/drops/session-{session}/shot.png");
    assert_eq!(body["path"], path);
    assert_eq!(body["typed"], serde_json::Value::Bool(true));

    let stored = host.drops.lock().unwrap().clone();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].session_id, session);
    assert_eq!(stored[0].filename, "shot.png");
    assert_eq!(stored[0].content, b"remote-bytes");

    // Plain bytes and a trailing space, with no paste markers and no
    // Enter: the viewer can keep typing around it.
    assert_eq!(typed_bytes(&mut frames), format!("{path} ").into_bytes());
}

#[tokio::test]
async fn a_host_too_old_to_store_a_drop_says_so() {
    let env = daemon_env();
    let auth = signed_in_bearer(&env.daemon);
    let host = enroll_host(&env, WORKER_PROTOCOL_SESSION_DROP - 1);
    route_to(&env, host.worker_id);
    let session = spawn_remote_session(&env, host.worker_id);
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let response = app
        .oneshot(
            Request::post(drop_uri(session, "shot.png"))
                .header(header::AUTHORIZATION, &auth)
                .body(Body::from("bytes"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["error"].as_str().unwrap().contains("too old"),
        "{body}"
    );
    assert!(
        host.drops.lock().unwrap().is_empty(),
        "an old host was sent a drop it cannot decode"
    );
}
