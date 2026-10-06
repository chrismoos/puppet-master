//! HTTP route and raw TCP forwarding contracts, including remote worker transport.

mod support;

use support::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn echo_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    (port, task)
}

/// A controller reached by a name that is not this machine's hostname,
/// so a URL built from it can only have come from the configured value.
const PUBLIC_URL: &str = "https://controller.example:8443";

/// A port with nothing listening on it.
async fn dead_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    listener.local_addr().unwrap().port()
}

fn forward_of(env: &TestEnv, forward_id: u64) -> Option<pm_protocol::domain::SessionForward> {
    env.daemon
        .subscribe()
        .0
        .forwards
        .into_iter()
        .find(|f| f.id == forward_id)
}

/// Creates a user and returns a session cookie token.
/// The dashboard credential these tests reach a forward with. A bearer token
/// rather than a cookie, because the cookie stopped authenticating anything but
/// the mint that produces this.
fn setup_user(env: &TestEnv) -> String {
    support::signed_in_bearer(&env.daemon)
}

/// Builds an HTTP request line + headers carrying the dashboard's access
/// token, terminated by the blank line. The caller can append a body.
fn http_request_with_auth(auth: &str) -> Vec<u8> {
    format!(
        "GET / HTTP/1.1\r\n\
         Host: localhost\r\n\
         Authorization: {auth}\r\n\
         \r\n"
    )
    .into_bytes()
}

#[tokio::test]
async fn a_remote_forward_streams_through_the_dial_back_endpoint() {
    use futures::{SinkExt, StreamExt};
    use pm_protocol::domain::{AgentKind, ControllerMsg, PermissionMode, WorkerMsg};
    use tokio_tungstenite::tungstenite;

    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: Some("127.0.0.1:0".parse().unwrap()),
        public_url: Some(PUBLIC_URL.to_string()),
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let worker_addr = handle.worker_addr.unwrap();
    let host_identity = pm_tls::Identity::generate().unwrap();

    let session = daemon.auth_setup("admin", "longenoughpassword").unwrap();
    let auth = support::dashboard_bearer(&daemon, &session);

    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();
    let (enroll_token, _) = daemon.create_worker_enrollment("laptop").unwrap();
    let mut reg = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enroll_token,
            credential: "",
            peer_key_hash: &host_identity.key_hash().to_hex(),
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
    let worker_id = reg.worker_id;
    daemon
        .set_bucket_workers(bucket, &[0, worker_id], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project, &[worker_id], Some(worker_id))
        .unwrap();

    let sid = daemon
        .spawn_session(
            project,
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
        .unwrap();
    match tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
        .await
        .unwrap()
    {
        Some(ControllerMsg::Spawn { .. }) => {}
        other => panic!("expected the remote spawn command, got {other:?}"),
    }
    let token = daemon.session_token(sid).unwrap().unwrap();
    let (echo_port, _echo) = echo_server().await;

    let forward = daemon
        .publish_port(&token, echo_port, "echo-tcp", "echo", "tcp")
        .await
        .unwrap();

    // Connect with a valid credential so the proxy lets us through.
    let http_req = http_request_with_auth(&auth);
    let payload = b"hello remote";
    let mut client = TcpStream::connect(("127.0.0.1", forward.listener_port))
        .await
        .unwrap();
    client.write_all(&http_req).await.unwrap();
    client.write_all(payload).await.unwrap();

    let (req_id, port, stream_token) = match tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
        .await
        .unwrap()
    {
        Some(ControllerMsg::ForwardOpen {
            req_id,
            port,
            token,
        }) => (req_id, port, token),
        other => panic!("expected ForwardOpen, got {other:?}"),
    };
    assert_eq!(port, echo_port);

    // The simulated worker dials its local target, reports the dial
    // outcome, and dials back the stream endpoint with the token.
    let mut target = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    daemon.apply_worker_message(
        worker_id,
        WorkerMsg::ForwardOpened {
            req_id,
            ok: true,
            error: String::new(),
        },
    );
    let mut ws = dial_worker_plane(worker_addr, &host_identity, &stream_token)
        .await
        .expect("the enrolled host dials back the stream endpoint");

    let expected_len = http_req.len() + payload.len();
    // TCP does not preserve the two client writes as one read: the replayed
    // headers and payload may arrive in separate WebSocket frames. Echo every
    // frame until the complete request has crossed the worker path.
    let mut forwarded = 0;
    while forwarded < expected_len {
        let msg = tokio::time::timeout(TEST_TIMEOUT, ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let data = match msg {
            tungstenite::Message::Binary(b) => b,
            other => panic!("expected binary frame, got {other:?}"),
        };
        forwarded += data.len();
        target.write_all(&data).await.unwrap();
        let mut echoed = vec![0u8; data.len()];
        target.read_exact(&mut echoed).await.unwrap();
        ws.send(tungstenite::Message::Binary(echoed.into()))
            .await
            .unwrap();
    }
    assert_eq!(forwarded, expected_len);

    let mut reply = vec![0u8; expected_len];
    tokio::time::timeout(TEST_TIMEOUT, client.read_exact(&mut reply))
        .await
        .expect("reply through the forward")
        .unwrap();
    assert!(reply.ends_with(payload));

    let snapshot = daemon.subscribe().0;
    let live = snapshot
        .forwards
        .iter()
        .find(|f| f.id == forward.id)
        .unwrap();
    assert_eq!(live.target_reachable, Some(true));

    // The stream token was consumed by the dial-back; a replay fails.
    let replay = dial_worker_plane(worker_addr, &host_identity, &stream_token).await;
    assert!(replay.is_err(), "a used stream token must be rejected");

    // A host the controller has never enrolled holds no key it recognizes,
    // so the token alone gets it nothing.
    let stranger = pm_tls::Identity::generate().unwrap();
    let forged = dial_worker_plane(worker_addr, &stranger, &stream_token).await;
    assert!(
        forged.is_err(),
        "an unenrolled host must not claim a stream"
    );
}

async fn http_target() -> (u16, tokio::task::JoinHandle<()>) {
    use axum::{
        body::{to_bytes, Body},
        extract::Request,
        response::IntoResponse,
        routing::any,
        Router,
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = Router::new().fallback(any(|request: Request| async {
        let path = request.uri().to_string();
        let headers = request.headers().clone();
        let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
        let mut response = axum::Json(serde_json::json!({
            "path": path, "cookie": headers.get("cookie").and_then(|v| v.to_str().ok()),
            "authorization": headers.get("authorization").and_then(|v| v.to_str().ok()),
            "prefix": headers.get("x-forwarded-prefix").and_then(|v| v.to_str().ok()),
            "forwardedHost": headers.get("x-forwarded-host").and_then(|v| v.to_str().ok()),
            "forwardedProto": headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()),
            "host": headers.get("host").and_then(|v| v.to_str().ok()),
            "body": String::from_utf8_lossy(&body)
        }))
        .into_response();
        if path == "/redirect" {
            *response.status_mut() = axum::http::StatusCode::FOUND;
            response
                .headers_mut()
                .insert("location", "/next?q=1".parse().unwrap());
            response.headers_mut().append(
                "set-cookie",
                "app=ok; Path=/; Domain=localhost; HttpOnly"
                    .parse()
                    .unwrap(),
            );
            response
                .headers_mut()
                .append("set-cookie", "pm_session=bad; Path=/".parse().unwrap());
        }
        response.map(Body::new)
    }));
    (
        port,
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }),
    )
}

async fn route_request(
    env: &TestEnv,
    path: &str,
    credential: Credential<'_>,
) -> axum::response::Response {
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder().uri(path);
    match credential {
        Credential::None => {}
        Credential::Dashboard(auth) => {
            request = request.header(axum::http::header::AUTHORIZATION, auth);
        }
        Credential::ForwardCookie(cookie) => {
            request = request.header(axum::http::header::COOKIE, cookie);
        }
    }
    pm_daemon::http::router(env.daemon.clone())
        .oneshot(request.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap()
}

/// Each call publishes from a session of its own, so it needs a slug of
/// its own: slugs are unique across the whole controller.
async fn publish_http(env: &TestEnv, port: u16) -> pm_protocol::domain::SessionForward {
    static NEXT_SLUG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let slug = format!(
        "preview-{}",
        NEXT_SLUG.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let id = spawn_test_session(env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    env.daemon
        .publish_port(&token, port, &slug, "preview", "http")
        .await
        .unwrap()
}

/// A session the user has shut down gives up the hostname it reserved,
/// while its forward row stays so a resume still knows which port it
/// published. Holding the name on a session that is gone stranded
/// published URLs that nobody could reclaim.
#[tokio::test]
async fn killing_a_session_frees_the_slug_it_published() {
    let env = daemon_env_with_public_url("https://controller.example:8443");
    let (port, _target) = http_target().await;
    let forward = publish_http(&env, port).await;
    let slug = forward.slug.clone();

    env.daemon.kill_session(forward.session_id).unwrap();

    let kept = forward_of_daemon(&env.daemon, forward.id)
        .expect("the forward row outlives the kill, for a resume to find");
    assert_eq!(kept.slug, "", "the killed session no longer holds the name");

    let next = spawn_test_session(&env, "next");
    let token = env.daemon.session_token(next).unwrap().unwrap();
    let taken = env
        .daemon
        .publish_port(&token, port, &slug, "preview", "http")
        .await
        .unwrap();
    assert_eq!(taken.slug, slug);
    assert_eq!(
        taken.session_id, next,
        "the name belongs to the session that took it"
    );
}

#[tokio::test]
async fn http_routes_preserve_public_origin_and_prefix_without_a_listener() {
    let env = daemon_env_with_public_url("https://controller.example:8443/pm");
    let (port, target) = http_target().await;
    let forward = publish_http(&env, port).await;
    assert_eq!(forward.listener_port, 0);
    assert_eq!(
        forward.url,
        format!(
            "https://controller.example:8443/pm/forwards/{}/",
            forward.id
        )
    );
    let token = env
        .daemon
        .session_token(forward.session_id)
        .unwrap()
        .unwrap();
    let again = env
        .daemon
        .publish_port(&token, port, &forward.slug, "preview", "http")
        .await
        .unwrap();
    assert_eq!(again.url, forward.url);
    env.daemon.unbind_session_forwards(forward.session_id);
    assert!(forward_of(&env, forward.id).unwrap().url.is_empty());
    env.daemon.recover_forwards().await;
    assert_eq!(forward_of(&env, forward.id).unwrap().url, forward.url);
    target.abort();
}

#[tokio::test]
async fn route_strips_prefix_preserves_query_and_body_and_removes_pm_credentials() {
    use tower::ServiceExt;
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let (port, target) = http_target().await;
    let forward = publish_http(&env, port).await;
    // The credential that authenticates this is the one asserted below to be
    // absent upstream: a forwarded application must not be handed the
    // dashboard's own token, and the pm_ cookies go the same way.
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(format!(
            "/forwards/{}/nested/index.html?x=1&fwd_token=discard&y=2",
            forward.id
        ))
        .header(
            "cookie",
            "pm_session=stale; pm_fwd=old; pm_fwd_123=other; app=keep",
        )
        .header(axum::http::header::AUTHORIZATION, &auth)
        .body(axum::body::Body::from("hello upstream"))
        .unwrap();
    let response = pm_daemon::http::router(env.daemon.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["path"], "/nested/index.html?x=1&y=2");
    assert_eq!(body["body"], "hello upstream");
    assert_eq!(body["cookie"], "app=keep");
    assert!(body["authorization"].is_null());
    assert_eq!(body["prefix"], format!("/forwards/{}", forward.id));
    assert_eq!(
        forward_of(&env, forward.id).unwrap().target_reachable,
        Some(true)
    );
    target.abort();
}

#[tokio::test]
async fn safari_handoff_sets_independent_secure_cookies_and_clean_urls() {
    use tower::ServiceExt;
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let (port, target) = http_target().await;
    let first = publish_http(&env, port).await;
    let second = publish_http(&env, port).await;
    let mut cookies = Vec::new();
    for forward in [&first, &second] {
        let token = env.daemon.mint_forward_token(1, "user".into(), forward.id);
        let request = axum::http::Request::builder()
            .uri(format!("/forwards/{}/?fwd_token={token}&x=1", forward.id))
            .header("accept", "text/html")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = pm_daemon::http::router(env.daemon.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), 307);
        assert_eq!(
            response.headers()["location"],
            format!("/forwards/{}/?x=1", forward.id)
        );
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        assert!(cookie.contains("; Secure"));
        assert!(cookie.contains(&format!("Path=/forwards/{}/", forward.id)));
        cookies.push(cookie.split(';').next().unwrap().to_string());
    }
    for (forward, cookie) in [(&first, &cookies[0]), (&second, &cookies[1])] {
        for _ in 0..2 {
            assert_eq!(
                route_request(
                    &env,
                    &format!("/forwards/{}/asset.js", forward.id),
                    Credential::ForwardCookie(cookie)
                )
                .await
                .status(),
                200
            );
        }
    }
    assert_eq!(
        route_request(
            &env,
            &format!("/forwards/{}/", second.id),
            Credential::ForwardCookie(&cookies[0])
        )
        .await
        .status(),
        401
    );
    target.abort();
}

#[tokio::test]
async fn route_auth_errors_unavailable_targets_and_close_have_http_statuses() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let session = setup_user(&env);
    let forward = publish_http(&env, dead_port().await).await;
    let path = format!("/forwards/{}/", forward.id);
    assert_eq!(
        route_request(&env, &path, Credential::None).await.status(),
        401
    );
    let expired = route_request(&env, &format!("{path}?fwd_token=expired"), Credential::None).await;
    assert_eq!(expired.status(), 401);
    assert!(String::from_utf8(
        axum::body::to_bytes(expired.into_body(), 4096)
            .await
            .unwrap()
            .to_vec()
    )
    .unwrap()
    .contains("expired"));
    let auth = session.clone();
    assert_eq!(
        route_request(&env, &path, Credential::Dashboard(&auth))
            .await
            .status(),
        502
    );
    assert_eq!(
        forward_of(&env, forward.id).unwrap().target_reachable,
        Some(false)
    );
    env.daemon.unbind_session_forwards(forward.session_id);
    assert_eq!(
        route_request(&env, &path, Credential::Dashboard(&auth))
            .await
            .status(),
        503
    );
    env.daemon.close_forward(forward.id).unwrap();
    assert_eq!(
        route_request(&env, &path, Credential::Dashboard(&auth))
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn route_rewrites_redirect_and_app_cookie_paths_and_rejects_pm_cookie_writes() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let (port, target) = http_target().await;
    let forward = publish_http(&env, port).await;
    let response = route_request(
        &env,
        &format!("/forwards/{}/redirect", forward.id),
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        response.headers()["location"],
        format!("/forwards/{}/next?q=1", forward.id)
    );
    let cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .collect::<Vec<_>>();
    assert_eq!(cookies.len(), 1);
    assert_eq!(
        cookies[0].to_str().unwrap(),
        format!("app=ok; Path=/forwards/{}/; HttpOnly", forward.id)
    );
    target.abort();
}

#[tokio::test]
async fn routes_require_a_live_session_and_a_known_forward_for_token_minting() {
    use tower::ServiceExt;
    let env = daemon_env_with_public_url(PUBLIC_URL);
    assert!(env
        .daemon
        .publish_port("invalid", 8080, "preview", "preview", "http")
        .await
        .is_err());
    let auth = setup_user(&env);
    let response = pm_daemon::http::router(env.daemon.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/forwards/999999/token")
                .header(axum::http::header::AUTHORIZATION, &auth)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    assert!(env
        .daemon
        .publish_port(&token, 8080, "preview", "preview", "http")
        .await
        .is_err());
}

#[tokio::test]
async fn route_websocket_upgrade_proxies_messages() {
    use axum::{extract::ws::WebSocketUpgrade, routing::get, Router};
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let target_port = listener.local_addr().unwrap().port();
    let target = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/events",
                get(|ws: WebSocketUpgrade| async {
                    ws.on_upgrade(|mut socket| async move {
                        while let Some(Ok(message)) = socket.recv().await {
                            if socket.send(message).await.is_err() {
                                break;
                            }
                        }
                    })
                }),
            ),
        )
        .await
        .unwrap();
    });
    let forward = publish_http(&env, target_port).await;
    let token = env.daemon.mint_forward_token(1, "user".into(), forward.id);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = pm_daemon::http::router(env.daemon.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let request = format!(
        "ws://{address}/forwards/{}/events?fwd_token={token}",
        forward.id
    )
    .into_client_request()
    .unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(Message::Text("hello websocket".into()))
        .await
        .unwrap();
    let reply = tokio::time::timeout(TEST_TIMEOUT, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply, Message::Text("hello websocket".into()));
    socket.close(None).await.unwrap();
    target.abort();
    server.abort();
}

#[tokio::test]
async fn raw_tcp_keeps_its_listener_and_persisted_port() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let id = spawn_test_session(&env, "raw");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let (port, echo) = echo_server().await;
    let forward = env
        .daemon
        .publish_port(&token, port, "raw", "raw", "tcp")
        .await
        .unwrap();
    assert_ne!(forward.listener_port, 0);
    let mut socket = TcpStream::connect(("127.0.0.1", forward.listener_port))
        .await
        .unwrap();
    socket.write_all(b"raw payload").await.unwrap();
    let mut reply = [0; 11];
    socket.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"raw payload");
    drop(socket);
    env.daemon.unbind_session_forwards(id);
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if TcpListener::bind(("127.0.0.1", forward.listener_port))
                .await
                .is_ok()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    env.daemon.bind_session_forwards(id).await;
    assert_eq!(
        forward_of(&env, forward.id).unwrap().listener_port,
        forward.listener_port
    );
    env.daemon.close_forward(forward.id).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if TcpStream::connect(("127.0.0.1", forward.listener_port))
                .await
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    echo.abort();
}

#[tokio::test]
async fn a_remote_http_route_proxies_through_the_worker_stream() {
    use futures::{SinkExt, StreamExt};
    use pm_protocol::domain::{AgentKind, ControllerMsg, PermissionMode, WorkerMsg};
    use tokio_tungstenite::tungstenite;

    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: Some("127.0.0.1:0".parse().unwrap()),
        public_url: Some(PUBLIC_URL.to_string()),
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let worker_addr = handle.worker_addr.unwrap();
    let host_identity = pm_tls::Identity::generate().unwrap();

    // Set up user for cookie auth.
    let session = daemon.auth_setup("admin", "longenoughpassword").unwrap();
    let auth = support::dashboard_bearer(&daemon, &session);

    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();
    let (enroll_token, _) = daemon.create_worker_enrollment("laptop").unwrap();
    let mut reg = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enroll_token,
            credential: "",
            peer_key_hash: &host_identity.key_hash().to_hex(),
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
    let worker_id = reg.worker_id;
    daemon
        .set_bucket_workers(bucket, &[0, worker_id], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project, &[worker_id], Some(worker_id))
        .unwrap();

    let sid = daemon
        .spawn_session(
            project,
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
        .unwrap();
    match tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
        .await
        .unwrap()
    {
        Some(ControllerMsg::Spawn { .. }) => {}
        other => panic!("expected the remote spawn command, got {other:?}"),
    }
    let token = daemon.session_token(sid).unwrap().unwrap();
    let (echo_port, _echo) = echo_server().await;

    let forward = daemon
        .publish_port(&token, echo_port, "echo", "echo", "http")
        .await
        .unwrap();

    let url = format!(
        "http://{}/forwards/{}/nested?x=1",
        handle.http_addr.unwrap(),
        forward.id
    );
    let client = tokio::spawn(async move {
        reqwest::Client::new()
            .get(url)
            .header(reqwest::header::AUTHORIZATION, auth)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    });

    let (req_id, port, stream_token) = match tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
        .await
        .unwrap()
    {
        Some(ControllerMsg::ForwardOpen {
            req_id,
            port,
            token,
        }) => (req_id, port, token),
        other => panic!("expected ForwardOpen, got {other:?}"),
    };
    assert_eq!(port, echo_port);

    // The simulated worker dials its local target, reports the dial
    // outcome, and dials back the stream endpoint with the token.
    let _target = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    daemon.apply_worker_message(
        worker_id,
        WorkerMsg::ForwardOpened {
            req_id,
            ok: true,
            error: String::new(),
        },
    );
    let mut ws = dial_worker_plane(worker_addr, &host_identity, &stream_token)
        .await
        .expect("the enrolled host dials back the stream endpoint");

    let mut request = Vec::new();
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        let msg = tokio::time::timeout(TEST_TIMEOUT, ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tungstenite::Message::Binary(data) = msg {
            request.extend_from_slice(&data);
        }
    }
    let request = String::from_utf8(request).unwrap();
    assert!(request.starts_with("GET /nested?x=1 HTTP/1.1"));
    assert!(!request.contains("pm_session"));
    ws.send(tungstenite::Message::Binary(
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"
            .to_vec()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, client)
            .await
            .unwrap()
            .unwrap(),
        "hello"
    );

    let snapshot = daemon.subscribe().0;
    let live = snapshot
        .forwards
        .iter()
        .find(|f| f.id == forward.id)
        .unwrap();
    assert_eq!(live.target_reachable, Some(true));

    // The stream token was consumed by the dial-back; a replay fails.
    let replay = dial_worker_plane(worker_addr, &host_identity, &stream_token).await;
    assert!(replay.is_err(), "a used stream token must be rejected");

    // A host the controller has never enrolled holds no key it recognizes,
    // so the token alone gets it nothing.
    let stranger = pm_tls::Identity::generate().unwrap();
    let forged = dial_worker_plane(worker_addr, &stranger, &stream_token).await;
    assert!(
        forged.is_err(),
        "an unenrolled host must not claim a stream"
    );
}

#[tokio::test]
async fn streamed_response_is_delivered_before_the_upstream_finishes() {
    use axum::{body::Body, routing::get, Router};
    use futures::StreamExt;
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/",
                get(|| async {
                    Body::from_stream(
                        futures::stream::once(async {
                            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"first event"))
                        })
                        .chain(futures::stream::pending()),
                    )
                }),
            ),
        )
        .await
        .unwrap();
    });
    let forward = publish_http(&env, port).await;
    let token = env.daemon.mint_forward_token(1, "user".into(), forward.id);
    let response = route_request(
        &env,
        &format!("/forwards/{}/?fwd_token={token}", forward.id),
        Credential::None,
    )
    .await;
    assert_eq!(response.status(), 200);
    let mut stream = response.into_body().into_data_stream();
    let first = tokio::time::timeout(TEST_TIMEOUT, stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.as_ref(), b"first event");
    target.abort();
}

/// The dashboard host in the share-domain tests, and the wildcard domain its
/// forwards live under. A registrable domain of its own, which is the
/// arrangement a share domain is for: a preview on a site of its own.
const SHARE_PUBLIC_URL: &str = "https://pm.example";
const SHARE_DOMAIN: &str = "pm-preview.example";

fn share_env() -> TestEnv {
    daemon_env_with_forward_mount(
        SHARE_PUBLIC_URL,
        pm_daemon::forward::ForwardConfig {
            share_domain: Some(SHARE_DOMAIN.to_string()),
            ..Default::default()
        },
    )
}

/// A request to the router carrying an explicit Host, which is what
/// selects a forward under the share domain.
async fn host_request(
    env: &TestEnv,
    host: &str,
    method: &str,
    path: &str,
    credential: Credential<'_>,
) -> axum::response::Response {
    send_with_host(env, host, method, path, credential, false).await
}

/// The same, announcing an HTML navigation, which is what the token
/// handoff and the sign-in redirect key off.
async fn host_navigation(
    env: &TestEnv,
    host: &str,
    path: &str,
    credential: Credential<'_>,
) -> axum::response::Response {
    send_with_host(env, host, "GET", path, credential, true).await
}

/// What a request to a forward authenticates with.
///
/// Not interchangeable. The dashboard holds an access token and sends a header.
/// A top-level navigation can send neither, so the handshake leaves it a cookie
/// scoped to the one forward, and that is what a reopened preview carries.
#[derive(Clone, Copy)]
enum Credential<'a> {
    None,
    Dashboard(&'a str),
    ForwardCookie(&'a str),
}

async fn send_with_host(
    env: &TestEnv,
    host: &str,
    method: &str,
    path: &str,
    credential: Credential<'_>,
    html: bool,
) -> axum::response::Response {
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host);
    if html {
        request = request.header("accept", "text/html");
    }
    match credential {
        Credential::None => {}
        Credential::Dashboard(auth) => {
            request = request.header(axum::http::header::AUTHORIZATION, auth);
        }
        Credential::ForwardCookie(cookie) => {
            request = request.header(axum::http::header::COOKIE, cookie);
        }
    }
    pm_daemon::http::router(env.daemon.clone())
        .oneshot(request.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_share_domain_mounts_each_forward_at_the_root_of_its_own_subdomain() {
    let env = share_env();
    let auth = setup_user(&env);
    let (port, target) = http_target().await;
    let forward = publish_http(&env, port).await;
    assert_eq!(
        forward.url,
        format!("https://{}.{SHARE_DOMAIN}/", forward.slug),
        "the published URL is the forward's own origin"
    );
    assert_eq!(forward.listener_port, 0, "the web server still serves it");

    let host = format!("{}.{SHARE_DOMAIN}", forward.slug);
    let response = host_request(
        &env,
        &host,
        "GET",
        "/nested/index.html?x=1",
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(
        body["path"], "/nested/index.html?x=1",
        "a rooted mount strips nothing but passes the path through"
    );
    assert!(
        body["prefix"].is_null(),
        "a forward that owns its origin has no prefix to compose against"
    );
    assert_eq!(body["forwardedHost"], host);
    assert_eq!(body["forwardedProto"], "https");
    assert_eq!(
        body["cookie"],
        serde_json::Value::Null,
        "the dashboard session cookie is still stripped upstream"
    );
    target.abort();
}

/// A forward's host belongs to the forward, so no dashboard route may
/// answer on it: the preview would otherwise shadow, or be shadowed by,
/// the controller's own API.
#[tokio::test]
async fn a_share_host_serves_no_dashboard_route() {
    let env = share_env();
    let auth = setup_user(&env);
    let (port, target) = http_target().await;
    let forward = publish_http(&env, port).await;
    let host = format!("{}.{SHARE_DOMAIN}", forward.slug);
    for path in ["/api/version", "/api/me", "/login", "/"] {
        let response = host_request(&env, &host, "GET", path, Credential::Dashboard(&auth)).await;
        assert_eq!(response.status(), 200, "{path}");
        assert_eq!(
            json_body(response).await["path"],
            path,
            "{path} must reach the forwarded application, not the dashboard"
        );
    }
    target.abort();
}

/// The dashboard keeps its own host while a share domain is configured.
#[tokio::test]
async fn the_dashboard_host_is_untouched_by_a_share_domain() {
    let env = share_env();
    let auth = setup_user(&env);
    let response = host_request(
        &env,
        "pm.example",
        "GET",
        "/api/version",
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["forwardMount"]["mode"], "share-domain");
    assert_eq!(body["forwardMount"]["shareDomain"], SHARE_DOMAIN);

    // The share domain's own apex is not a forward either.
    let apex = host_request(
        &env,
        SHARE_DOMAIN,
        "GET",
        "/api/version",
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(apex.status(), 200);
    assert!(json_body(apex).await["version"].is_string());
}

/// The point of the mode: a forward's cookie is scoped to its own origin
/// rather than to a path under the dashboard's.
#[tokio::test]
async fn a_share_host_handoff_scopes_the_cookie_to_the_whole_origin() {
    let env = share_env();
    let (port, target) = http_target().await;
    let forward = publish_http(&env, port).await;
    let token = env.daemon.mint_forward_token(1, "user".into(), forward.id);
    let host = format!("{}.{SHARE_DOMAIN}", forward.slug);
    let response = host_navigation(
        &env,
        &host,
        &format!("/app?fwd_token={token}&x=1"),
        Credential::None,
    )
    .await;
    assert_eq!(response.status(), 307);
    assert_eq!(
        response.headers()["location"],
        "/app?x=1",
        "the clean URL stays on the forward's own origin"
    );
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(!cookie.contains("/forwards/"), "{cookie}");
    assert!(cookie.contains("; Secure"), "{cookie}");

    let jar = cookie.split(';').next().unwrap().to_string();
    let replay = host_request(
        &env,
        &host,
        "GET",
        "/asset.js",
        Credential::ForwardCookie(&jar),
    )
    .await;
    assert_eq!(replay.status(), 200);

    // The cookie is still scoped to one forward, so it does not open
    // another one on its own subdomain.
    let second = publish_http(&env, port).await;
    let second_host = format!("{}.{SHARE_DOMAIN}", second.slug);
    let other = host_request(
        &env,
        &second_host,
        "GET",
        "/",
        Credential::ForwardCookie(&jar),
    )
    .await;
    assert_eq!(other.status(), 401);
    let navigated = host_navigation(
        &env,
        &second_host,
        "/deep/page?x=one%20two&y=3",
        Credential::ForwardCookie(&jar),
    )
    .await;
    assert_eq!(navigated.status(), 303);
    let location = reqwest::Url::parse(navigated.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(
        location.origin(),
        reqwest::Url::parse(SHARE_PUBLIC_URL).unwrap().origin()
    );
    let handoff = location
        .fragment()
        .unwrap()
        .strip_prefix("/forward-open?")
        .unwrap();
    let parsed = reqwest::Url::parse(&format!("{SHARE_PUBLIC_URL}/?{handoff}")).unwrap();
    let query: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(query["id"], second.id.to_string());
    assert_eq!(
        query["destination"],
        format!("{}deep/page?x=one%20two&y=3", second.url)
    );
    target.abort();
}

/// Safari sends no app bearer to a preview origin, so the one dashboard
/// endpoint a forward host answers is minting its own opening token. It
/// mints no other forward's.
#[tokio::test]
async fn a_share_host_mints_only_its_own_opening_token() {
    let env = share_env();
    let auth = setup_user(&env);
    let (port, target) = http_target().await;
    let forward = publish_http(&env, port).await;
    let other = publish_http(&env, port).await;
    let host = format!("{}.{SHARE_DOMAIN}", forward.slug);

    let minted = host_request(
        &env,
        &host,
        "POST",
        &format!("/api/forwards/{}/token", forward.id),
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(minted.status(), 200);
    let token = json_body(minted).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        env.daemon
            .verify_forward_token(&token, forward.id)
            .as_deref(),
        Some("testuser")
    );

    let foreign = host_request(
        &env,
        &host,
        "POST",
        &format!("/api/forwards/{}/token", other.id),
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(foreign.status(), 200);
    assert_eq!(
        json_body(foreign).await["path"],
        format!("/api/forwards/{}/token", other.id),
        "another forward's token endpoint is just a path on this application"
    );
    target.abort();
}

/// Nothing is mounted on a rooted forward, so the application's own
/// redirect and cookie paths reach the browser as written. `Domain` is
/// still dropped: on a share domain it would set a cookie every sibling
/// forward could read.
#[tokio::test]
async fn a_share_host_leaves_application_paths_alone_but_still_drops_domain() {
    let env = share_env();
    let auth = setup_user(&env);
    let (port, target) = http_target().await;
    let forward = publish_http(&env, port).await;
    let response = host_request(
        &env,
        &format!("{}.{SHARE_DOMAIN}", forward.slug),
        "GET",
        "/redirect",
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(response.status(), 302);
    assert_eq!(response.headers()["location"], "/next?q=1");
    let cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(cookies, vec!["app=ok; Path=/; HttpOnly".to_string()]);
    target.abort();
}

/// A slug is the forward's hostname, so it is checked before anything
/// is recorded and every rejection says which rule it broke.
#[tokio::test]
async fn publishing_refuses_a_slug_that_is_not_a_dns_label() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let (port, target) = http_target().await;
    for (slug, reason) in [
        ("", "a slug is required"),
        ("   ", "a slug is required"),
        ("Docs", "uppercase letters are rejected, not folded"),
        ("docs_preview", "a-z, 0-9 and hyphen"),
        ("ab", "at least 3 characters"),
        ("-docs", "starts and ends with a letter or digit"),
        ("do--cs", "no consecutive hyphens"),
        ("admin", "that slug is reserved"),
        ("f7", "followed by digits"),
    ] {
        let error = env
            .daemon
            .publish_port(&token, port, slug, "preview", "http")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(reason),
            "slug {slug:?} was refused with {error:?}, which does not say {reason:?}"
        );
    }
    assert!(
        env.daemon.subscribe().0.forwards.is_empty(),
        "a refused publish recorded a forward"
    );
    env.daemon
        .publish_port(&token, port, " docs-preview ", "preview", "http")
        .await
        .expect("surrounding whitespace is a transport artifact, not part of the name");
    target.abort();
}

/// A slug is a hostname, so it belongs to one forward on the whole
/// controller rather than one per session.
#[tokio::test]
async fn a_slug_in_use_by_another_forward_is_refused_by_name() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let (port, target) = http_target().await;
    let first = spawn_test_session(&env, "first");
    let first_token = env.daemon.session_token(first).unwrap().unwrap();
    let taken = env
        .daemon
        .publish_port(&first_token, port, "docs-preview", "", "http")
        .await
        .unwrap();

    let second = spawn_test_session(&env, "second");
    let second_token = env.daemon.session_token(second).unwrap().unwrap();
    let error = env
        .daemon
        .publish_port(&second_token, port + 1, "docs-preview", "", "http")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("docs-preview"),
        "the rejection does not name the slug: {error:?}"
    );
    assert!(
        error.contains(&taken.id.to_string()),
        "the rejection does not name the forward holding it: {error:?}"
    );

    // Closing the forward frees the name for whoever wants it next.
    env.daemon.close_forward(taken.id).unwrap();
    env.daemon
        .publish_port(&second_token, port + 1, "docs-preview", "", "http")
        .await
        .expect("a freed slug is available again");
    target.abort();
}

/// The URL is already in the user's hands by the time a rename is asked
/// for, so the forward keeps the name it was published under.
#[tokio::test]
async fn republishing_a_port_under_another_slug_is_refused_by_name() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let (port, target) = http_target().await;
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let first = env
        .daemon
        .publish_port(&token, port, "docs-preview", "", "http")
        .await
        .unwrap();

    let error = env
        .daemon
        .publish_port(&token, port, "api-preview", "", "http")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("docs-preview"),
        "the rejection does not name the existing slug: {error:?}"
    );
    assert_eq!(
        forward_of(&env, first.id).unwrap().slug,
        "docs-preview",
        "a refused rename changed the forward"
    );

    // Republishing under the same name is how a resumed agent recovers.
    let again = env
        .daemon
        .publish_port(&token, port, "docs-preview", "", "http")
        .await
        .unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(again.url, first.url);
    target.abort();
}

/// The label is free text for the dashboard, and a publisher that sends
/// none gets the slug, so a forward is never nameless in the UI.
#[tokio::test]
async fn the_label_defaults_to_the_slug() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let (port, target) = http_target().await;
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let unlabelled = env
        .daemon
        .publish_port(&token, port, "docs-preview", "  ", "http")
        .await
        .unwrap();
    assert_eq!(unlabelled.label, "docs-preview");

    let labelled = env
        .daemon
        .publish_port(&token, port + 1, "api-preview", "vite dev server", "http")
        .await
        .unwrap();
    assert_eq!(labelled.label, "vite dev server");
    assert_eq!(labelled.slug, "api-preview");
    target.abort();
}

/// The forward is hosted at its slug, and the id host it would have had
/// before slugs existed is not a second way in.
#[tokio::test]
async fn a_share_host_dispatches_by_slug_alone() {
    let env = share_env();
    let auth = setup_user(&env);
    let (port, target) = http_target().await;
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let forward = env
        .daemon
        .publish_port(&token, port, "docs-preview", "", "http")
        .await
        .unwrap();
    assert_eq!(forward.url, format!("https://docs-preview.{SHARE_DOMAIN}/"));

    let by_slug = host_request(
        &env,
        &format!("docs-preview.{SHARE_DOMAIN}"),
        "GET",
        "/",
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(by_slug.status(), 200);

    let by_id = host_request(
        &env,
        &format!("f{}.{SHARE_DOMAIN}", forward.id),
        "GET",
        "/",
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(
        by_id.status(),
        404,
        "a named forward answered at its id host too"
    );
    target.abort();
}

/// Rows written before publishing required a slug keep the host they
/// were already handed out under.
#[tokio::test]
async fn a_forward_published_before_slugs_keeps_its_id_host() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("pm.db");
    let forward_id = {
        let storage = pm_daemon::storage::Storage::open(&db).unwrap();
        let bucket = storage.create_bucket("legacy").unwrap();
        let project = storage
            .create_project(bucket.id, "legacy", tmp.path().to_str().unwrap())
            .unwrap();
        let session = storage
            .create_session(
                project.id,
                pm_protocol::domain::AgentKind::Test,
                "legacy",
                "",
                pm_protocol::domain::PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        storage
            .create_session_forward(session.id, 5173, 0, "", "preview", "http", 1)
            .unwrap()
            .id
    };

    let env = daemon_env_with_forward_mount_on_db(
        SHARE_PUBLIC_URL,
        pm_daemon::forward::ForwardConfig {
            share_domain: Some(SHARE_DOMAIN.to_string()),
            ..Default::default()
        },
        db,
    );
    env.daemon.recover_forwards().await;
    assert_eq!(
        forward_of(&env, forward_id).unwrap().url,
        format!("https://f{forward_id}.{SHARE_DOMAIN}/"),
        "an unnamed forward moved off its id host"
    );

    // Unauthenticated, so the assertion is about which handler answered
    // rather than about reaching a target that is not running.
    let response = host_request(
        &env,
        &format!("f{forward_id}.{SHARE_DOMAIN}"),
        "GET",
        "/",
        Credential::None,
    )
    .await;
    assert_eq!(
        response.status(),
        401,
        "the id host did not reach the forward"
    );
}

/// A host under the share domain that names no live forward is not a
/// forward at all, and must not be mistaken for the dashboard either.
#[tokio::test]
async fn an_unknown_share_host_is_not_found() {
    let env = share_env();
    let auth = setup_user(&env);
    let response = host_request(
        &env,
        &format!("f4242.{SHARE_DOMAIN}"),
        "GET",
        "/",
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(response.status(), 404);
}

fn port_env(lo: u16, hi: u16) -> TestEnv {
    daemon_env_with_forward_mount(
        "http://pm.example:7676",
        pm_daemon::forward::ForwardConfig {
            share_port_range: Some((lo, hi)),
            ..Default::default()
        },
    )
}

/// Reads one response from a per-forward listener over a real socket,
/// which is the only way to exercise the listener rather than the router.
async fn port_request(port: u16, request: &str) -> String {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(TEST_TIMEOUT, socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8_lossy(&response).to_string()
}

#[tokio::test]
async fn a_per_forward_port_serves_that_forward_at_the_root_and_no_dashboard_route() {
    let env = port_env(41500, 41599);
    let auth = setup_user(&env);
    let (target_port, target) = http_target().await;
    let forward = publish_http(&env, target_port).await;
    let bound = forward.listener_port;
    assert!(
        (41500..=41599).contains(&bound),
        "the listener binds from the configured range, got {bound}"
    );
    assert_eq!(forward.url, format!("http://pm.example:{bound}/"));

    let response = port_request(
        bound,
        &format!(
            "GET /nested?x=1 HTTP/1.1\r\nHost: pm.example:{bound}\r\nAuthorization: {auth}\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    assert!(response.contains("\"path\":\"/nested?x=1\""), "{response}");
    assert!(
        response.contains("\"prefix\":null"),
        "a per-forward listener sends no prefix: {response}"
    );

    // The dashboard is not on this listener: its own API path is just a
    // path on the forwarded application.
    let dashboard = port_request(
        bound,
        &format!(
            "GET /api/version HTTP/1.1\r\nHost: pm.example:{bound}\r\nAuthorization: {auth}\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;
    assert!(
        dashboard.contains("\"path\":\"/api/version\""),
        "{dashboard}"
    );
    assert!(!dashboard.contains("\"gitRev\""), "{dashboard}");

    // Its own opening token still mints, which a first visit needs.
    let minted = port_request(
        bound,
        &format!(
            "POST /api/forwards/{}/token HTTP/1.1\r\nHost: pm.example:{bound}\r\nAuthorization: {auth}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            forward.id
        ),
    )
    .await;
    assert!(minted.contains("\"expiresInMs\""), "{minted}");
    target.abort();
}

/// The port is a resource: closing a forward has to give it back, and a
/// controller restart has to take the same one again so a handed-out URL
/// keeps working.
#[tokio::test]
async fn a_per_forward_port_is_released_on_close_and_retaken_on_recovery() {
    let env = port_env(41600, 41699);
    let (target_port, target) = http_target().await;
    let forward = publish_http(&env, target_port).await;
    let bound = forward.listener_port;

    env.daemon.unbind_session_forwards(forward.session_id);
    tokio::time::timeout(TEST_TIMEOUT, async {
        while TcpListener::bind(("127.0.0.1", bound)).await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("unbinding releases the port");
    assert!(
        forward_of(&env, forward.id).unwrap().url.is_empty(),
        "an unbound forward names no URL"
    );

    env.daemon.recover_forwards().await;
    let recovered = forward_of(&env, forward.id).unwrap();
    assert_eq!(
        recovered.listener_port, bound,
        "recovery retakes the persisted port"
    );
    assert_eq!(recovered.url, forward.url);

    env.daemon.close_forward(forward.id).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, async {
        while TcpStream::connect(("127.0.0.1", bound)).await.is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("closing a forward releases its port");
    target.abort();
}

/// Raw TCP keeps its own range: the share range is for HTTP mounts and
/// the two must not be confused for one another.
#[tokio::test]
async fn the_share_port_range_does_not_move_raw_tcp_listeners() {
    let env = daemon_env_with_forward_mount(
        "http://pm.example:7676",
        pm_daemon::forward::ForwardConfig {
            port_range: Some((41700, 41749)),
            share_port_range: Some((41750, 41799)),
            ..Default::default()
        },
    );
    let session = spawn_test_session(&env, "raw");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let (echo_port, echo) = echo_server().await;
    let raw = env
        .daemon
        .publish_port(&token, echo_port, "raw", "raw", "tcp")
        .await
        .unwrap();
    assert!(
        (41700..=41749).contains(&raw.listener_port),
        "raw TCP uses --forward-ports, got {}",
        raw.listener_port
    );
    let (target_port, target) = http_target().await;
    let http = env
        .daemon
        .publish_port(&token, target_port, "preview", "preview", "http")
        .await
        .unwrap();
    assert!(
        (41750..=41799).contains(&http.listener_port),
        "an HTTP mount uses the share range, got {}",
        http.listener_port
    );
    echo.abort();
    target.abort();
}

/// Two forwards in one snapshot must never be named the same thing under
/// any mount: a client that renders both links has no other way to tell
/// them apart, and one cookie would then open the wrong preview.
#[tokio::test]
async fn every_mount_names_two_forwards_distinctly_in_one_snapshot() {
    let mounts = [
        (
            "path prefix",
            pm_daemon::forward::ForwardConfig::default(),
            "https://controller.example:8443",
        ),
        (
            "share domain",
            pm_daemon::forward::ForwardConfig {
                share_domain: Some(SHARE_DOMAIN.to_string()),
                ..Default::default()
            },
            SHARE_PUBLIC_URL,
        ),
        (
            "per-forward ports",
            pm_daemon::forward::ForwardConfig {
                share_port_range: Some((41800, 41899)),
                ..Default::default()
            },
            "http://pm.example:7676",
        ),
    ];
    for (label, forward, public_url) in mounts {
        let env = daemon_env_with_forward_mount(public_url, forward);
        let (port, target) = http_target().await;
        let first = publish_http(&env, port).await;
        let second = publish_http(&env, port).await;
        assert_ne!(first.id, second.id, "{label}");
        for forward in [&first, &second] {
            assert!(
                !forward.url.is_empty(),
                "{label} named no URL for forward {}",
                forward.id
            );
        }
        assert_ne!(first.url, second.url, "{label} named both forwards alike");

        // And in the snapshot a client actually reads, which is where a
        // dashboard gets the two links it renders side by side.
        let mut urls = env
            .daemon
            .subscribe()
            .0
            .forwards
            .iter()
            .map(|f| f.url.clone())
            .collect::<Vec<_>>();
        urls.sort();
        urls.dedup();
        assert_eq!(urls.len(), 2, "{label} snapshot URLs collided: {urls:?}");
        target.abort();
    }
}

/// A persisted forward must answer for itself on the very first request
/// after a restart. It used to be able to answer 503 "this forward is
/// stopped" instead: the browser plane began accepting while
/// `recover_forwards` had not run, so a live forward looked stopped. That
/// is a lasting wrong answer for a preview page that only loads once, and
/// no amount of retrying by the client fixes a 503 it already rendered.
///
/// Each mount is checked on the address that mount actually publishes.
#[tokio::test]
async fn a_persisted_forward_never_reports_itself_stopped_after_a_restart() {
    for (label, forward_config, public_url) in [
        (
            "path prefix",
            pm_daemon::forward::ForwardConfig::default(),
            "http://pm.example:7676",
        ),
        (
            "share domain",
            pm_daemon::forward::ForwardConfig {
                share_domain: Some(SHARE_DOMAIN.to_string()),
                ..Default::default()
            },
            "http://pm.example:7676",
        ),
        (
            "per-forward ports",
            pm_daemon::forward::ForwardConfig {
                share_port_range: Some((41900, 41999)),
                ..Default::default()
            },
            // The published host has to be the one the listener is
            // actually reachable on for this test to speak to it.
            "http://127.0.0.1:7676",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("pm.db");
        let config = |forward: pm_daemon::forward::ForwardConfig| pm_daemon::DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward,
            db_path: Some(db.clone()),
            socket_path: tmp.path().join("pm.sock"),
            worker_addr: None,
            public_url: Some(public_url.to_string()),
            http_addr: Some("127.0.0.1:0".parse().unwrap()),
            http_tls: None,
            scrollback_dir: tmp.path().join("sb"),
            registry: test_registry(),
            local_worker_enabled: true,
            release_channel: None,
        };

        let (first, handle) = pm_daemon::start(config(forward_config.clone()))
            .await
            .unwrap();
        let bucket = first.create_bucket("b").unwrap();
        let project = first
            .create_project(bucket, "p", tmp.path().to_str().unwrap())
            .unwrap();
        let session = first
            .spawn_session(
                project,
                pm_protocol::domain::AgentKind::Test,
                "task",
                "prompt",
                None,
                pm_protocol::domain::PermissionMode::Inherit,
                None,
                true,
                false,
                None,
            )
            .unwrap();
        let token = first.session_token(session).unwrap().unwrap();
        let (target_port, target) = http_target().await;
        let forward = first
            .publish_port(&token, target_port, "preview", "preview", "http")
            .await
            .unwrap();
        handle.shutdown().await;
        drop(first);

        // The restarted controller: the first request on its browser
        // plane is the one that used to race recovery.
        let (second, handle) = pm_daemon::start(config(forward_config.clone()))
            .await
            .unwrap();
        let addr = handle.http_addr.unwrap();
        let live = forward_of_daemon(&second, forward.id).expect("the forward recovered");
        let request = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let response = match label {
            "share domain" => request
                .get(format!("http://{addr}/"))
                .header("host", format!("{}.{SHARE_DOMAIN}", forward.slug)),
            "per-forward ports" => {
                assert!(
                    (41900..=41999).contains(&live.listener_port),
                    "{label} did not rebind from its range, got {}",
                    live.listener_port
                );
                request.get(format!("http://127.0.0.1:{}/", live.listener_port))
            }
            _ => request.get(format!("http://{addr}/forwards/{}/", forward.id)),
        }
        .send()
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            401,
            "{label} answered {} for a recovered forward with no credential",
            response.status()
        );
        assert!(
            !live.url.is_empty(),
            "{label} named no URL for the recovered forward"
        );
        handle.shutdown().await;
        target.abort();
    }
}

fn forward_of_daemon(
    daemon: &std::sync::Arc<pm_daemon::Daemon>,
    forward_id: u64,
) -> Option<pm_protocol::domain::SessionForward> {
    daemon
        .subscribe()
        .0
        .forwards
        .into_iter()
        .find(|f| f.id == forward_id)
}

/// Ends a live session so it can be resumed in place.
async fn end_session(env: &mut TestEnv, id: u64) {
    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);
}

/// Publishes one forward from a live session. The caller holds the echo
/// server's task so the forwarded port stays occupied for the test.
async fn publish_one(env: &TestEnv, id: u64) -> (u16, tokio::task::JoinHandle<()>) {
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let (port, echo) = echo_server().await;
    env.daemon
        .publish_port(&token, port, "docs-preview", "preview", "http")
        .await
        .unwrap();
    (port, echo)
}

#[tokio::test]
async fn a_resumed_session_is_given_its_forwards_in_its_instructions() {
    let (mut env, recorded) = daemon_env_recording_instructions(PUBLIC_URL);
    let id = spawn_test_session(&env, "p");
    let (port, _echo) = publish_one(&env, id).await;

    end_session(&mut env, id).await;
    env.daemon.resume_session(id).unwrap();

    let instructions = recorded.latest(id).unwrap();
    // The kill released the name, so the resume is told the port it
    // published rather than a hostname it no longer holds. The port is what
    // a resume needs, to put its server back where the forward expects it.
    assert!(
        instructions.contains(&format!("unnamed, local port {port}")),
        "the resumed agent was not given its forward: {instructions}"
    );
    assert!(
        !instructions.contains("docs-preview"),
        "the resumed agent was offered a name its session gave up: {instructions}"
    );
    assert!(
        instructions.contains("confirm each server is still listening"),
        "the inventory reached the agent without its instruction: {instructions}"
    );
}

#[tokio::test]
async fn the_inventory_sits_under_the_compiled_instruction_layers() {
    let (mut env, recorded) = daemon_env_recording_instructions(PUBLIC_URL);
    let bucket_id = bucket_of(&env);
    env.daemon
        .set_instructions(
            bucket_id,
            None,
            pm_protocol::domain::InstructionTarget::All,
            "Deploy only on green.",
            0,
            "",
            None,
        )
        .unwrap();
    let id = spawn_test_session(&env, "p");
    let (_port, _echo) = publish_one(&env, id).await;

    end_session(&mut env, id).await;
    env.daemon.resume_session(id).unwrap();

    let instructions = recorded.latest(id).unwrap();
    let policy = instructions.find("Deploy only on green.").unwrap();
    let inventory = instructions.find("unnamed, local port").unwrap();
    assert!(
        policy < inventory,
        "the inventory displaced the instruction layers: {instructions}"
    );
}

/// The snapshot is what the session's instruction layers compiled to, and
/// a resume that only republished the same forwards must not read as an
/// instruction change.
#[tokio::test]
async fn the_inventory_stays_out_of_the_instruction_snapshot() {
    let (mut env, _recorded) = daemon_env_recording_instructions(PUBLIC_URL);
    let id = spawn_test_session(&env, "p");
    let (_port, _echo) = publish_one(&env, id).await;

    end_session(&mut env, id).await;
    env.daemon.resume_session(id).unwrap();

    let generation = env.daemon.agent_terminal(id).unwrap().generation;
    let (compiled, _sources, _hash) = env.daemon.instruction_snapshot(id, generation).unwrap();
    assert!(
        !compiled.contains("docs-preview, local port"),
        "the snapshot recorded this resume's forwards as instruction policy: {compiled}"
    );
}

/// Claude reads the same inventory from its session-start hook, so the
/// daemon leaves its instructions alone rather than saying it twice.
#[tokio::test]
async fn an_agent_that_carries_session_start_context_is_not_given_the_inventory() {
    let (mut env, recorded) = daemon_env_recording_instructions(PUBLIC_URL);
    let id = spawn_agent_session(&env, pm_protocol::domain::AgentKind::ClaudeCode, "p");
    let (_port, _echo) = publish_one(&env, id).await;

    end_session(&mut env, id).await;
    env.daemon.resume_session(id).unwrap();

    let instructions = recorded.latest(id).unwrap();
    assert!(
        !instructions.contains("docs-preview, local port"),
        "an agent that reads its own session-start context was told twice: {instructions}"
    );
    assert!(
        !env.daemon
            .forward_inventory_for_token(&env.daemon.session_token(id).unwrap().unwrap())
            .unwrap()
            .is_empty(),
        "the hook path lost the inventory the instructions path skipped"
    );
}

#[tokio::test]
async fn a_fresh_spawn_is_given_no_inventory() {
    let (env, recorded) = daemon_env_recording_instructions(PUBLIC_URL);
    let id = spawn_test_session(&env, "p");
    let (_port, _echo) = publish_one(&env, id).await;

    let instructions = recorded.latest(id).unwrap();
    assert!(
        !instructions.contains("docs-preview, local port"),
        "a session that never restarted was given an inventory: {instructions}"
    );
}

#[tokio::test]
async fn a_resumed_session_with_no_forwards_is_given_no_inventory() {
    let (mut env, recorded) = daemon_env_recording_instructions(PUBLIC_URL);
    let id = spawn_test_session(&env, "p");

    end_session(&mut env, id).await;
    env.daemon.resume_session(id).unwrap();

    let instructions = recorded.latest(id).unwrap();
    assert!(
        !instructions.contains("Port forwards this session published"),
        "a session with no forwards was given an inventory: {instructions}"
    );
}

/// A share domain under the dashboard's own registrable domain gives a preview
/// a different origin and the same site, so reads are blocked and the session
/// cookie still rides along on requests the preview makes to the dashboard.
/// That is the operator's trade to make, the same one the per-forward-port
/// mount already offers, so the daemon starts and names it. What it still
/// refuses is a share domain that cannot carry a forward's label at all.
#[tokio::test]
async fn a_same_site_share_domain_starts_and_only_a_non_name_is_refused() {
    let start = |share_domain: &str, public_url: &str| {
        let tmp = tempfile::tempdir().unwrap();
        let config = pm_daemon::DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward: pm_daemon::forward::ForwardConfig {
                share_domain: Some(share_domain.to_string()),
                ..Default::default()
            },
            db_path: None,
            socket_path: tmp.path().join("pm.sock"),
            worker_addr: None,
            public_url: (!public_url.is_empty()).then(|| public_url.to_string()),
            http_addr: None,
            http_tls: None,
            scrollback_dir: tmp.path().join("sb"),
            registry: test_registry(),
            local_worker_enabled: false,
            release_channel: None,
        };
        pm_daemon::Daemon::new(config)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };

    // The same site as the dashboard, by either route to being one.
    start("share.example.com", "https://pm.example.com").unwrap();
    start("pm.example.com", "https://pm.example.com").unwrap();
    // A registrable domain of its own, which is what a share domain is for.
    start("previews.example.net", "https://pm.example.com").unwrap();
    // A dashboard reached by address is never the same site as a name.
    start("previews.example.net", "http://127.0.0.1:7676").unwrap();
    // Without a public URL there is nothing to compare, so it starts and warns.
    start("share.example.com", "").unwrap();

    // An address carries no label in front of it, so it names no forward.
    let refused = start("127.0.0.1", "https://pm.example.com").unwrap_err();
    assert!(
        refused.contains("is not a name"),
        "the refusal must say why, got {refused:?}"
    );
}

/// How a target treats the connection a request arrived on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reuse {
    /// Keeps it open, as any HTTP/1.1 server does.
    KeepAlive,
    /// Answers with `Connection: close`, so the proxy must not reuse it.
    AsksToClose,
    /// Keeps the header silent but closes the socket once the response
    /// is out, which is what a server with a short idle keep-alive does
    /// to a connection sitting in the pool.
    ClosesSilently,
    /// Answers an upgrade with 101 and then holds the connection.
    Upgrades,
    /// Declares a longer body than it sends and then closes, which is a
    /// misframed response: the connection is no longer at a message
    /// boundary and nothing may be read off it again.
    ShortBody,
    /// Answers correctly and then appends a second complete, valid
    /// response nobody asked for. Whoever read off this connection next
    /// would be handed that response instead of the answer to their own
    /// request.
    AppendsAnExtraResponse,
}

/// The body the appending target tacks on, which no caller must ever
/// receive.
const APPENDED_BODY: &str = "LEAKED-SECRET";

/// A loopback HTTP/1.1 target that counts the connections it accepts, so
/// a test can see whether the proxy opened a new one or reused the one
/// it already had.
struct CountingTarget {
    port: u16,
    accepted: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for CountingTarget {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl CountingTarget {
    fn connections(&self) -> usize {
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }
}

async fn counting_target(reuse: Reuse) -> CountingTarget {
    use std::sync::atomic::Ordering;

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = accepted.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let ordinal = counter.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::spawn(async move {
                let mut pending = Vec::new();
                loop {
                    let mut buf = [0u8; 2048];
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => pending.extend_from_slice(&buf[..n]),
                    }
                    let Some(end) = pending
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|at| at + 4)
                    else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&pending[..end]).to_lowercase();
                    pending.drain(..end);
                    let upgrading = head.contains("upgrade: websocket");
                    let owned = match (reuse, upgrading) {
                        (Reuse::ShortBody, _) => {
                            // Ten promised, two delivered, then the close.
                            Some("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhi".to_string())
                        }
                        (Reuse::AppendsAnExtraResponse, _) => {
                            let own = format!("conn-{ordinal}");
                            Some(format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{own}\
                                 HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{APPENDED_BODY}",
                                own.len(),
                                APPENDED_BODY.len()
                            ))
                        }
                        _ => None,
                    };
                    let response = match (reuse, upgrading) {
                        (Reuse::Upgrades, true) => {
                            "HTTP/1.1 101 Switching Protocols\r\n\
                             Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
                        }
                        (Reuse::AsksToClose, _) => {
                            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                        }
                        _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
                    };
                    let bytes = owned.as_deref().unwrap_or(response);
                    if sock.write_all(bytes.as_bytes()).await.is_err() {
                        break;
                    }
                    if reuse == Reuse::ShortBody {
                        break;
                    }
                    if reuse == Reuse::ClosesSilently {
                        break;
                    }
                    if reuse == Reuse::Upgrades && upgrading {
                        // The tunnel is the proxy's to drive from here.
                        std::future::pending::<()>().await;
                    }
                }
            });
        }
    });
    CountingTarget {
        port,
        accepted,
        task,
    }
}

async fn proxied_get(env: &TestEnv, forward_id: u64, path: &str, auth: &str) -> (u16, String) {
    let response = route_request(
        env,
        &format!("/forwards/{forward_id}/{path}"),
        Credential::Dashboard(auth),
    )
    .await;
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// The in-process worker's own branch of the proxy: a direct loopback
/// connect, which is pooled exactly as a worker stream is.
#[tokio::test]
async fn the_local_target_path_reuses_one_upstream_connection() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::KeepAlive).await;
    let forward = publish_http(&env, target.port).await;

    for _ in 0..5 {
        assert_eq!(
            proxied_get(&env, forward.id, "page", &auth).await,
            (200, "ok".to_string())
        );
    }
    assert_eq!(
        target.connections(),
        1,
        "five requests through one forward opened one upstream"
    );
    assert_eq!(env.daemon.idle_upstreams(forward.id), 1);
}

#[tokio::test]
async fn concurrent_requests_leave_at_most_the_idle_cap_pooled() {
    use pm_daemon::forward_upstream::MAX_IDLE_PER_TARGET;

    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::KeepAlive).await;
    let forward = publish_http(&env, target.port).await;

    let concurrent = MAX_IDLE_PER_TARGET * 2;
    let mut requests = Vec::new();
    for _ in 0..concurrent {
        requests.push(proxied_get(&env, forward.id, "page", &auth));
    }
    for (status, body) in futures::future::join_all(requests).await {
        assert_eq!((status, body.as_str()), (200, "ok"));
    }
    assert!(
        target.connections() > 1,
        "concurrent requests cannot share one HTTP/1 connection"
    );
    assert!(
        env.daemon.idle_upstreams(forward.id) <= MAX_IDLE_PER_TARGET,
        "pooled {} upstreams, cap is {MAX_IDLE_PER_TARGET}",
        env.daemon.idle_upstreams(forward.id)
    );
}

/// A 101 hands the connection to the tunnel, so it is no longer an HTTP
/// connection anyone else can send a request down.
#[tokio::test]
async fn an_upgraded_connection_is_never_pooled() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::Upgrades).await;
    let forward = publish_http(&env, target.port).await;

    let upgrade = axum::http::Request::builder()
        .uri(format!("/forwards/{}/socket", forward.id))
        .header(axum::http::header::AUTHORIZATION, &auth)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = {
        use tower::ServiceExt;
        pm_daemon::http::router(env.daemon.clone())
            .oneshot(upgrade)
            .await
            .unwrap()
    };
    assert_eq!(response.status(), 101);
    assert_eq!(env.daemon.idle_upstreams(forward.id), 0);
    assert_eq!(target.connections(), 1);

    assert_eq!(
        proxied_get(&env, forward.id, "page", &auth).await,
        (200, "ok".to_string())
    );
    assert_eq!(
        target.connections(),
        2,
        "the next request cannot ride the tunnel"
    );
}

#[tokio::test]
async fn an_upstream_that_asks_to_close_is_not_reused() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::AsksToClose).await;
    let forward = publish_http(&env, target.port).await;

    for _ in 0..3 {
        assert_eq!(
            proxied_get(&env, forward.id, "page", &auth).await,
            (200, "ok".to_string())
        );
        assert_eq!(env.daemon.idle_upstreams(forward.id), 0);
    }
    assert_eq!(
        target.connections(),
        3,
        "a connection the target asked to close is not reused"
    );
}

/// A client asking to close is answered on a connection that goes no
/// further either, whatever the target said.
#[tokio::test]
async fn a_client_asking_to_close_leaves_nothing_pooled() {
    use tower::ServiceExt;
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::KeepAlive).await;
    let forward = publish_http(&env, target.port).await;

    let request = axum::http::Request::builder()
        .uri(format!("/forwards/{}/page", forward.id))
        .header(axum::http::header::AUTHORIZATION, &auth)
        .header("connection", "close")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = pm_daemon::http::router(env.daemon.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = axum::body::to_bytes(response.into_body(), 1024 * 1024).await;
    assert_eq!(env.daemon.idle_upstreams(forward.id), 0);
}

/// A target that closes each connection once the response is out,
/// which is what a server with a short idle keep-alive does to one
/// sitting in the pool. Every request must still be answered: a close
/// landing between the pool's liveness check and the send is what the
/// retry exists for, and without it the user sees a 502. Run on
/// threads, and often enough, for that race to show if it is there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_target_that_closes_between_requests_still_answers_every_one() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::ClosesSilently).await;
    let forward = publish_http(&env, target.port).await;

    for attempt in 0..300 {
        assert_eq!(
            proxied_get(&env, forward.id, "page", &auth).await,
            (200, "ok".to_string()),
            "request {attempt} was not answered"
        );
    }
    assert_eq!(
        target.connections(),
        300,
        "each closed connection was replaced rather than reported as an error"
    );
}

#[tokio::test]
async fn stopping_a_forward_leaves_no_pooled_upstream() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::KeepAlive).await;
    let forward = publish_http(&env, target.port).await;

    assert_eq!(
        proxied_get(&env, forward.id, "page", &auth).await,
        (200, "ok".to_string())
    );
    assert_eq!(env.daemon.idle_upstreams(forward.id), 1);

    env.daemon.unbind_session_forwards(forward.session_id);
    assert_eq!(env.daemon.idle_upstreams(forward.id), 0);
    let (status, _) = proxied_get(&env, forward.id, "page", &auth).await;
    assert_eq!(status, 503, "a stopped forward is not reachable");
}

/// The reuse the fix is about: on the controller-to-worker hop. Each
/// upstream costs a `ForwardOpen` round trip, a mutual-TLS handshake and
/// a WebSocket upgrade, so a page's worth of requests paying for one is
/// the whole point.
#[tokio::test]
async fn sequential_requests_through_a_remote_forward_open_one_upstream() {
    use pm_protocol::domain::{AgentKind, ControllerMsg, PermissionMode};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: Some("127.0.0.1:0".parse().unwrap()),
        public_url: Some(PUBLIC_URL.to_string()),
        http_addr: None,
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let worker_addr = handle.worker_addr.unwrap();
    let identity = pm_tls::Identity::generate().unwrap();
    let session = daemon.auth_setup("admin", "longenoughpassword").unwrap();
    let auth = support::dashboard_bearer(&daemon, &session);

    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();
    let (enroll, _) = daemon.create_worker_enrollment("laptop").unwrap();
    let mut reg = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enroll,
            credential: "",
            peer_key_hash: &identity.key_hash().to_hex(),
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
    let worker_id = reg.worker_id;
    daemon
        .set_bucket_workers(bucket, &[0, worker_id], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project, &[worker_id], Some(worker_id))
        .unwrap();
    let session = daemon
        .spawn_session(
            project,
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
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
            .await
            .unwrap(),
        Some(ControllerMsg::Spawn { .. })
    ));
    let token = daemon.session_token(session).unwrap().unwrap();
    let target = counting_target(Reuse::KeepAlive).await;
    let forward = daemon
        .publish_port(&token, target.port, "remote-pool", "preview", "http")
        .await
        .unwrap();

    let opens = std::sync::Arc::new(AtomicUsize::new(0));
    let relay = tokio::spawn(relay_forward_opens(
        daemon.clone(),
        worker_id,
        reg.rx,
        worker_addr,
        identity.clone(),
        opens.clone(),
        None,
    ));

    for _ in 0..4 {
        let request = axum::http::Request::builder()
            .uri(format!("/forwards/{}/page", forward.id))
            .header(axum::http::header::AUTHORIZATION, &auth)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tokio::time::timeout(
            TEST_TIMEOUT,
            pm_daemon::http::router(daemon.clone()).oneshot(request),
        )
        .await
        .expect("the remote forward answers")
        .unwrap();
        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
    }

    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "four requests through one remote forward asked the worker to dial once"
    );
    assert_eq!(target.connections(), 1, "and the worker dialed it once");
    assert_eq!(daemon.idle_upstreams(forward.id), 1);
    relay.abort();
}

/// A streamed body on a connection that is not going back into the
/// pool: the lease is dropped as soon as the handler returns, and the
/// response still has to stream. The pooled path has its own cover in
/// `streamed_response_is_delivered_before_the_upstream_finishes`.
#[tokio::test]
async fn a_streamed_body_on_a_closing_connection_still_reaches_the_client() {
    use futures::StreamExt;

    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                // Chunked, so the body can start before it is complete,
                // and `Connection: close` so the proxy must not keep it.
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\n\
                          Transfer-Encoding: chunked\r\n\
                          Connection: close\r\n\r\n\
                          b\r\nfirst event\r\n",
                    )
                    .await;
                std::future::pending::<()>().await;
            });
        }
    });
    let forward = publish_http(&env, port).await;

    let response = route_request(
        &env,
        &format!("/forwards/{}/", forward.id),
        Credential::Dashboard(&auth),
    )
    .await;
    assert_eq!(response.status(), 200);
    let mut stream = response.into_body().into_data_stream();
    let first = tokio::time::timeout(TEST_TIMEOUT, stream.next())
        .await
        .expect("the first chunk arrives before the body is complete")
        .unwrap()
        .unwrap();
    assert_eq!(first.as_ref(), b"first event");
    assert_eq!(env.daemon.idle_upstreams(forward.id), 0);
    target.abort();
}

/// Killing a session releases the names it published. A port forward
/// keeps its row for a resume to find, so nothing else takes the
/// proxy's connections to it: a reusable tunnel to a target the user
/// has stopped publishing must not survive the kill.
///
/// Whether the route answers at all in the moment after a kill is the
/// reconciler's business, not this test's, so only the connection is
/// asserted here.
#[tokio::test]
async fn killing_a_session_drops_the_upstream_connections_to_its_port_forward() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::KeepAlive).await;

    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let forward = env
        .daemon
        .publish_port(&token, target.port, "killed-port", "preview", "http")
        .await
        .unwrap();

    assert_eq!(
        proxied_get(&env, forward.id, "page", &auth).await,
        (200, "ok".to_string())
    );
    assert_eq!(env.daemon.idle_upstreams(forward.id), 1);
    assert_eq!(target.connections(), 1);

    env.daemon.kill_session(session).unwrap();

    assert_eq!(
        env.daemon.idle_upstreams(forward.id),
        0,
        "a killed session's forward must leave no reusable connection"
    );
    let _ = proxied_get(&env, forward.id, "page", &auth).await;
    assert_eq!(
        target.connections(),
        2,
        "whatever the route answers now, it cannot answer down the killed session's connection"
    );
}

/// Reads a proxied response without insisting the body arrives whole,
/// for the cases where the upstream breaks its own framing.
async fn proxied_get_allowing_a_broken_body(
    env: &TestEnv,
    forward_id: u64,
    path: &str,
    auth: &str,
) -> (u16, Option<String>) {
    let response = route_request(
        env,
        &format!("/forwards/{forward_id}/{path}"),
        Credential::Dashboard(auth),
    )
    .await;
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    (status, body)
}

/// A response whose framing the upstream broke leaves the connection
/// somewhere other than a message boundary, so it can never be read off
/// again. Reuse is what would turn that into one caller's bytes being
/// read as another's, so the connection has to be retired.
#[tokio::test]
async fn a_misframed_response_retires_the_connection() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let target = counting_target(Reuse::ShortBody).await;
    let forward = publish_http(&env, target.port).await;

    let (status, body) = proxied_get_allowing_a_broken_body(&env, forward.id, "page", &auth).await;
    assert_eq!(status, 200, "the head was well formed, so it was relayed");
    assert_eq!(
        body, None,
        "the body fails rather than arriving short, so no caller reads a \
         truncated response as a complete one"
    );
    assert_eq!(target.connections(), 1);
    assert_eq!(
        env.daemon.idle_upstreams(forward.id),
        0,
        "a connection left mid-body must not be pooled"
    );

    let _ = proxied_get_allowing_a_broken_body(&env, forward.id, "page", &auth).await;
    assert_eq!(
        target.connections(),
        2,
        "the next request opened a new connection at the target"
    );
}

/// The one that matters. An upstream that appends a second complete,
/// valid response after a correctly framed one leaves that response
/// queued on the connection. If the proxy reused it, the next caller —
/// a different identity on the same forward — would be handed the
/// appended response instead of the answer to their own request.
#[tokio::test]
async fn an_upstream_that_queues_an_extra_response_cannot_serve_the_next_identity() {
    use tower::ServiceExt;

    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);

    // First, that the premise holds: this target really does leave a
    // second complete response on the socket. Checked on a target of
    // its own so it does not disturb the connection count below.
    {
        let probe = counting_target(Reuse::AppendsAnExtraResponse).await;
        let mut raw = TcpStream::connect(("127.0.0.1", probe.port)).await.unwrap();
        raw.write_all(b"GET /page HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains(APPENDED_BODY) {
            let mut buf = [0u8; 1024];
            let n = tokio::time::timeout(TEST_TIMEOUT, raw.read(&mut buf))
                .await
                .expect("the target appends a second response")
                .unwrap();
            assert_ne!(n, 0, "the target closed before appending anything");
            seen.extend_from_slice(&buf[..n]);
        }
        let wire = String::from_utf8_lossy(&seen);
        assert_eq!(
            wire.matches("HTTP/1.1 200 OK").count(),
            2,
            "one request drew two complete responses onto the wire: {wire:?}"
        );
    }

    let target = counting_target(Reuse::AppendsAnExtraResponse).await;
    let forward = publish_http(&env, target.port).await;

    // The dashboard's identity asks first.
    let (status, body) = proxied_get(&env, forward.id, "page", &auth).await;
    assert_eq!((status, body.as_str()), (200, "conn-1"));
    assert_eq!(target.connections(), 1);
    assert_eq!(
        env.daemon.idle_upstreams(forward.id),
        0,
        "a connection with a response still queued on it must not be pooled"
    );

    // A different authenticated identity on the same forward asks next:
    // a scoped forward token minted for another user entirely.
    let other = env
        .daemon
        .mint_forward_token(2, "someone-else".into(), forward.id);
    let request = axum::http::Request::builder()
        .uri(format!("/forwards/{}/page?fwd_token={other}", forward.id))
        .body(axum::body::Body::empty())
        .unwrap();
    let response = pm_daemon::http::router(env.daemon.clone())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let second = String::from_utf8_lossy(&bytes).into_owned();

    assert_ne!(
        second, APPENDED_BODY,
        "the second identity was served the response queued on the first one's connection"
    );
    assert_eq!(
        second, "conn-2",
        "the second identity got the answer to its own request, on a connection of its own"
    );
    assert_eq!(
        target.connections(),
        2,
        "the queued-up connection was retired rather than reused"
    );
}

/// A request as a browser makes it: `Accept: text/html` is what marks a
/// page open, and only a page open can be sent through the handoff.
async fn routed(
    env: &TestEnv,
    method: &str,
    path: &str,
    accept: &str,
    cookie: Option<&str>,
) -> axum::response::Response {
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("accept", accept);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    pm_daemon::http::router(env.daemon.clone())
        .oneshot(request.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap()
}

/// The state every handed-out link is in after the controller restarts:
/// the browser still holds the cookie, the daemon's token table does not.
/// A page open goes through the handoff for a fresh token rather than
/// dead-ending on the one it cannot use.
#[tokio::test]
async fn a_page_open_with_a_dead_forward_cookie_goes_through_the_handoff() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    setup_user(&env);
    let forward = publish_http(&env, dead_port().await).await;
    let path = format!("/forwards/{}/", forward.id);
    let stale = format!("pm_fwd_{}=wiped", forward.id);

    let response = routed(&env, "GET", &path, "text/html,*/*", Some(&stale)).await;
    assert_eq!(response.status(), 303);
    let location = response.headers()["location"].to_str().unwrap().to_string();
    assert!(location.contains("forward-open"), "{location}");

    // The credential that failed is dropped, so declining the handoff
    // does not leave it presented on every later request.
    let cleared = response.headers()["set-cookie"].to_str().unwrap();
    assert!(
        cleared.starts_with(&format!("pm_fwd_{}=;", forward.id)),
        "{cleared}"
    );
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
}

/// The two cookies a browser actually sends together, which neither the
/// handoff change nor this wave covered on its own.
///
/// A reader of a forward holds the dashboard's session cookie as well as the
/// forward's own, because the forward is on the dashboard's site. The session
/// cookie no longer authenticates a forward, so it must not short-circuit the
/// handoff: the reader is sent through it on the strength of the dead forward
/// cookie, exactly as it would be with no session at all. The alternative would
/// be a forward served on ambient session authority, which is the thing this
/// wave removed.
#[tokio::test]
async fn a_signed_session_cookie_does_not_spare_a_page_open_the_handoff() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let session = env
        .daemon
        .auth_setup("testuser", "longenoughpassword")
        .unwrap();
    let forward = publish_http(&env, dead_port().await).await;
    let path = format!("/forwards/{}/", forward.id);

    // The signed value a login hands the browser, beside a forward cookie the
    // daemon no longer knows.
    let both = format!("pm_session={session}; pm_fwd_{}=wiped", forward.id);
    let response = routed(&env, "GET", &path, "text/html,*/*", Some(&both)).await;

    assert_eq!(
        response.status(),
        303,
        "a live session must not serve a forward by itself"
    );
    assert!(response.headers()["location"]
        .to_str()
        .unwrap()
        .contains("forward-open"));

    // And the session cookie survives it: the handoff clears the credential
    // that failed, which is the forward's, not the one that signs in.
    let cleared = response.headers()["set-cookie"].to_str().unwrap();
    assert!(
        cleared.starts_with(&format!("pm_fwd_{}=;", forward.id)),
        "{cleared}"
    );
    assert!(
        !cleared.contains("pm_session="),
        "signing in is not what expired: {cleared}"
    );
}

/// The loop breaker. The handoff is what minted the query token, so
/// sending the reader back for another would never terminate.
#[tokio::test]
async fn a_page_open_with_a_dead_query_token_is_refused_rather_than_redirected() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    setup_user(&env);
    let forward = publish_http(&env, dead_port().await).await;
    let path = format!("/forwards/{}/?fwd_token=wiped", forward.id);

    let response = routed(&env, "GET", &path, "text/html,*/*", None).await;
    assert_eq!(response.status(), 401);
}

/// A subresource cannot use a redirect: it would decode the login page as
/// the script or image it asked for, which reads as a parse error rather
/// than as an authentication problem.
#[tokio::test]
async fn a_subresource_with_a_dead_forward_cookie_is_refused() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    setup_user(&env);
    let forward = publish_http(&env, dead_port().await).await;
    let path = format!("/forwards/{}/app.js", forward.id);
    let stale = format!("pm_fwd_{}=wiped", forward.id);

    let response = routed(&env, "GET", &path, "*/*", Some(&stale)).await;
    assert_eq!(response.status(), 401);
    assert!(response.headers().get("location").is_none());
}

/// A 303 would come back as a GET without its body, so a submission is
/// refused rather than silently discarded on the way to a login page.
#[tokio::test]
async fn a_post_with_a_dead_forward_cookie_is_refused() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    setup_user(&env);
    let forward = publish_http(&env, dead_port().await).await;
    let path = format!("/forwards/{}/submit", forward.id);
    let stale = format!("pm_fwd_{}=wiped", forward.id);

    let response = routed(&env, "POST", &path, "text/html,*/*", Some(&stale)).await;
    assert_eq!(response.status(), 401);
    assert!(response.headers().get("location").is_none());
}
