//! Push delivery integration tests: registration over HTTP, event
//! derivation from committed transitions, dedupe across a daemon
//! restart, retry/backoff, receipt-driven endpoint disable, and the
//! APNs/FCM adapter contracts against a local mock provider. No real
//! provider credentials are used; signing keys are throwaway values
//! generated for these tests only.

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use pm_daemon::daemon::AgentReport;
use pm_daemon::push::SETTING_PUSH_GATEWAY_URL;
use pm_daemon::storage::Storage;
use pm_daemon::{Daemon, DaemonConfig};
use pm_protocol::domain::{AgentKind, HookKind, PermissionMode};
use tower::util::ServiceExt;

struct PushEnv {
    daemon: Arc<Daemon>,
    /// A second handle on the same database for direct row setup and
    /// assertions from outside the daemon.
    storage: Storage,
    db_path: std::path::PathBuf,
    project_id: u64,
    _tmp: tempfile::TempDir,
}

fn push_env() -> PushEnv {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let daemon = open_daemon(&db_path, tmp.path());
    let bucket_id = daemon.create_bucket("push-bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "push-project", tmp.path().to_str().unwrap())
        .unwrap();
    let storage = Storage::open(&db_path).unwrap();
    PushEnv {
        daemon,
        storage,
        db_path,
        project_id,
        _tmp: tmp,
    }
}

fn open_daemon(db_path: &std::path::Path, tmp: &std::path::Path) -> Arc<Daemon> {
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: Some(db_path.to_path_buf()),
        socket_path: tmp.join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.join("scrollback"),
        registry: pm_adapters::AdapterRegistry::empty(),
        local_worker_enabled: false,
        release_channel: None,
    };
    Arc::new(Daemon::new(config).unwrap().0)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Creates a session directly in storage and returns (id, hook token).
fn seeded_session(env: &PushEnv, supervisor: bool, spawned_by: Option<u64>) -> (u64, String) {
    let session = env
        .storage
        .create_session(
            env.project_id,
            AgentKind::Test,
            "task",
            "prompt",
            PermissionMode::Default,
            0,
            true,
            supervisor,
            spawned_by,
            now(),
        )
        .unwrap();
    let token = format!("hook-token-{}", session.id);
    env.storage.set_session_token(session.id, &token).unwrap();
    (session.id, token)
}

fn enroll_device(daemon: &Daemon, installation: &str) -> (u64, u64, String) {
    daemon.auth_setup("testuser", "hunter2hunter2").ok();
    let enrollment = daemon
        .mobile_enroll(pm_daemon::mobile::MobileEnrollRequest {
            proof: pm_daemon::mobile::MobileEnrollProof::Password {
                username: "testuser".into(),
                password: "hunter2hunter2".into(),
            },
            app_installation_id: installation.into(),
            name: "phone".into(),
            platform: "ios".into(),
        })
        .unwrap();
    (
        enrollment.device.user_id,
        enrollment.device.id,
        enrollment.tokens.access_token,
    )
}

async fn register_via_http(
    daemon: &Arc<Daemon>,
    device_id: u64,
    bearer: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    let app = pm_daemon::http::router(daemon.clone());
    app.oneshot(
        Request::builder()
            .method("PUT")
            .uri(format!("/api/mobile/devices/{device_id}/push"))
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// The device keypair these tests register. Notifications are sealed to
/// the public half, so opening a relayed payload with the secret half is
/// what proves the content actually reached the device intact.
fn device_keypair() -> ([u8; 32], [u8; 32]) {
    use std::sync::OnceLock;
    static KEYS: OnceLock<([u8; 32], [u8; 32])> = OnceLock::new();
    *KEYS.get_or_init(pm_push::hpke::generate_keypair)
}

fn device_public_key_b64() -> String {
    use base64::engine::{general_purpose::STANDARD as B64, Engine};
    B64.encode(device_keypair().1)
}

/// Opens the sealed payload the daemon handed the relay.
fn open_sealed(request: &RecordedRequest) -> serde_json::Value {
    use base64::engine::{general_purpose::STANDARD as B64, Engine};
    let sealed_b64 = request.body["sealed_payload"]
        .as_str()
        .expect("sealed_payload");
    let sealed = B64.decode(sealed_b64).expect("sealed payload is base64");
    let plaintext = pm_push::hpke::open(&device_keypair().0, &sealed).expect("payload opens");
    serde_json::from_slice(&plaintext).expect("sealed payload is json")
}

fn device_registration() -> serde_json::Value {
    serde_json::json!({
        "token": "apns-device-token",
        "environment": "sandbox",
        "locale": "en-US",
        "publicKey": device_public_key_b64(),
    })
}

/// One recorded provider request and the programmable response queue.
#[derive(Clone, Default)]
struct MockProvider {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    responses: Arc<Mutex<VecDeque<MockResponse>>>,
}

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    path: String,
    body: serde_json::Value,
    raw_body: String,
}

#[derive(Debug, Clone)]
struct MockResponse {
    status: u16,
    body: serde_json::Value,
    headers: Vec<(String, String)>,
}

impl MockProvider {
    fn push_response(&self, status: u16, body: serde_json::Value) {
        self.push_response_with_headers(status, body, vec![]);
    }

    fn push_response_with_headers(
        &self,
        status: u16,
        body: serde_json::Value,
        headers: Vec<(String, String)>,
    ) {
        self.responses.lock().unwrap().push_back(MockResponse {
            status,
            body,
            headers,
        });
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn provider_requests(&self) -> Vec<RecordedRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.path != "/token")
            .collect()
    }
}

/// Serves the mock provider on a loopback port. The `/token` path
/// always answers the OAuth exchange; everything else consumes the
/// programmed response queue (default 200).
async fn start_mock_provider(mock: MockProvider) -> String {
    use axum::extract::State;
    use axum::response::IntoResponse;

    async fn handler(
        State(mock): State<MockProvider>,
        request: Request<Body>,
    ) -> axum::response::Response {
        let (parts, body) = request.into_parts();
        let bytes = axum::body::to_bytes(body, 1 << 20)
            .await
            .unwrap_or_default();
        let raw_body = String::from_utf8_lossy(&bytes).to_string();
        let recorded = RecordedRequest {
            method: parts.method.to_string(),
            path: parts.uri.path().to_string(),
            body: serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
            raw_body,
        };
        let is_token = recorded.path == "/token";
        mock.requests.lock().unwrap().push(recorded);
        if is_token {
            return axum::Json(serde_json::json!({
                "access_token": "mock-oauth-token",
                "expires_in": 3600,
            }))
            .into_response();
        }
        let response = mock.responses.lock().unwrap().pop_front();
        match response {
            Some(response) => {
                let mut builder = axum::http::Response::builder().status(response.status);
                for (name, value) in &response.headers {
                    builder = builder.header(name, value);
                }
                builder
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(response.body.to_string()))
                    .unwrap()
            }
            None => axum::Json(serde_json::json!({})).into_response(),
        }
    }

    let app = axum::Router::new()
        .fallback(axum::routing::any(handler))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

fn configure_gateway(daemon: &Daemon, endpoint: &str) {
    daemon
        .set_setting(SETTING_PUSH_GATEWAY_URL, Some(endpoint))
        .unwrap();
}

fn report(headline: &str) -> AgentReport {
    AgentReport::Report {
        goal: String::new(),
        headline: headline.into(),
        summary: None,
        note: String::new(),
        glance: None,
        context: None,
        clear: Vec::new(),
        git: Default::default(),
    }
}

fn pending_deliveries(storage: &Storage) -> Vec<pm_daemon::storage::NotificationDelivery> {
    storage
        .due_notification_deliveries(now() + 100 * 60 * 60 * 1000, 100)
        .unwrap()
}

#[tokio::test]
async fn registration_is_device_bound_and_rotates() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");

    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let endpoint = body_json(response).await["endpoint"].clone();
    assert_eq!(endpoint["environment"], "sandbox");
    assert_eq!(endpoint["events"]["needsInput"], true);
    assert_eq!(endpoint["previewsEnabled"], false);
    assert!(
        endpoint.to_string().find("apns-device-token").is_none(),
        "registration responses never echo the platform token"
    );
    let stored = env.storage.get_push_endpoint(device_id).unwrap().unwrap();
    assert!(!stored.token_ciphertext.contains("apns-device-token"));

    // Another device's bearer cannot touch this device's endpoint.
    let (_, other_device, other_bearer) = enroll_device(&env.daemon, "app-2");
    assert_ne!(other_device, device_id);
    let forbidden =
        register_via_http(&env.daemon, device_id, &other_bearer, device_registration()).await;
    assert_eq!(forbidden.status(), StatusCode::NOT_FOUND);

    // Token rotation replaces the ciphertext in place.
    let mut rotated = device_registration();
    rotated["token"] = "apns-device-token-2".into();
    rotated["previewsEnabled"] = true.into();
    rotated["events"] = serde_json::json!({ "needsInput": true, "failed": false });
    let response = register_via_http(&env.daemon, device_id, &bearer, rotated).await;
    assert_eq!(response.status(), StatusCode::OK);
    let endpoint = body_json(response).await["endpoint"].clone();
    assert_eq!(endpoint["previewsEnabled"], true);
    assert_eq!(endpoint["events"]["failed"], false);
    assert_eq!(endpoint["events"]["completed"], true);
    let rotated_row = env.storage.get_push_endpoint(device_id).unwrap().unwrap();
    assert_ne!(rotated_row.token_ciphertext, stored.token_ciphertext);

    // Unregister removes the endpoint.
    let app = pm_daemon::http::router(env.daemon.clone());
    let deleted = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/mobile/devices/{device_id}/push"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(env.storage.get_push_endpoint(device_id).unwrap().is_none());
}

#[tokio::test]
async fn needs_input_transition_delivers_via_the_gateway_contract() {
    let env = push_env();
    let mock = MockProvider::default();
    let base = start_mock_provider(mock.clone()).await;
    configure_gateway(&env.daemon, &base);
    mock.push_response(200, serde_json::json!({"status": "accepted"}));

    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);

    let (session_id, token) = seeded_session(&env, false, None);
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "agent-1", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::NeedsInput,
            "pick one",
            "agent-1",
            "",
            false,
        )
        .unwrap();

    let pending = pending_deliveries(&env.storage);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "needs-input");
    assert_eq!(pending[0].session_id, session_id);
    let collapse = pending[0].collapse_id.clone();
    assert_ne!(
        collapse,
        format!("pm-session-{session_id}"),
        "the collapse id travels as a cleartext header, so it must not name the session"
    );
    assert!(
        collapse.len() == 32 && collapse.chars().all(|c| c.is_ascii_hexdigit()),
        "expected an opaque hex id, got {collapse}"
    );

    let stats = env.daemon.process_push_deliveries(now()).await;
    assert_eq!((stats.attempted, stats.sent), (1, 1));

    let requests = mock.provider_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/push");
    assert_eq!(request.body["device_token"], "apns-device-token");
    assert_eq!(request.body["environment"], "sandbox");
    assert_eq!(request.body["collapse_id"], collapse);
    assert_eq!(
        request.body.as_object().unwrap().len(),
        4,
        "the relay is told nothing beyond token, payload, environment and collapse id"
    );

    // The relay never sees any of this; only the device key opens it.
    let sealed = open_sealed(request);
    assert_eq!(sealed["title"], "Session needs input");
    assert_eq!(
        sealed["body"], "",
        "previews are off by default, so no headline or detail leaves"
    );
    assert_eq!(sealed["session_id"], session_id);
    assert_eq!(sealed["state"], "needs-input");
    assert!(
        sealed["counter"].as_u64().unwrap() > 0,
        "a zero counter is rejected by the device as a replay"
    );
    assert!(
        !request.raw_body.contains("pick one"),
        "the needs-input question never enters a payload"
    );
    assert!(
        !request.raw_body.contains("Session needs input"),
        "not even the generic title reaches the relay in the clear"
    );

    let settled = env
        .storage
        .lookup_notification_delivery(&pending[0].event_id, device_id)
        .unwrap()
        .unwrap();
    assert_eq!(settled.status, "sent");
    assert_eq!(
        settled.provider_message_id, None,
        "the relay does not hand back an APNs id"
    );
}

#[tokio::test]
async fn clean_turn_end_completes_and_previews_stay_generic() {
    let env = push_env();
    let mock = MockProvider::default();
    let base = start_mock_provider(mock.clone()).await;
    configure_gateway(&env.daemon, &base);

    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);

    // A failed turn is not completed work.
    let (_, token) = seeded_session(&env, false, None);
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "agent-1", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::TurnFailed,
            "rate limited",
            "agent-1",
            "",
            false,
        )
        .unwrap();
    assert!(pending_deliveries(&env.storage).is_empty());

    // A turn that answered only in text, without any agent report,
    // notifies on the idle transition.
    let (text_session, token) = seeded_session(&env, false, None);
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "agent-2", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "agent-2", "", false)
        .unwrap();
    let pending = pending_deliveries(&env.storage);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "completed");
    assert_eq!(pending[0].session_id, text_session);

    let (session_id, token) = seeded_session(&env, false, None);
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "agent-3", "", false)
        .unwrap();
    env.daemon
        .handle_agent_report(&token, report("built the push pipeline"))
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "agent-3", "", false)
        .unwrap();
    let pending: Vec<_> = pending_deliveries(&env.storage)
        .into_iter()
        .filter(|delivery| delivery.session_id == session_id)
        .collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "completed");

    mock.push_response(200, serde_json::json!({"status": "accepted"}));
    mock.push_response(200, serde_json::json!({"status": "accepted"}));
    env.daemon.process_push_deliveries(now()).await;
    let requests = mock.provider_requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(open_sealed(request)["title"], "Session completed work");
        assert!(
            !request.raw_body.contains("built the push pipeline"),
            "headlines only travel when previews are enabled"
        );
    }
}

#[tokio::test]
async fn supervised_workers_stay_silent_by_default() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);

    let (supervisor_id, supervisor_token) = seeded_session(&env, true, None);
    let (_, worker_token) = seeded_session(&env, false, Some(supervisor_id));

    env.daemon
        .handle_hook_event(&worker_token, HookKind::PromptSubmitted, "", "w", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&worker_token, HookKind::NeedsInput, "q", "w", "", false)
        .unwrap();
    assert!(
        pending_deliveries(&env.storage).is_empty(),
        "a supervised worker's needs-input is its supervisor's problem"
    );

    env.daemon
        .handle_hook_event(
            &supervisor_token,
            HookKind::PromptSubmitted,
            "",
            "s",
            "",
            false,
        )
        .unwrap();
    env.daemon
        .handle_hook_event(&supervisor_token, HookKind::NeedsInput, "q", "s", "", false)
        .unwrap();
    assert_eq!(pending_deliveries(&env.storage).len(), 1);
}

#[tokio::test]
async fn dedupe_survives_daemon_restart() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);

    let (_, token) = seeded_session(&env, false, None);
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "a", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "q", "a", "", false)
        .unwrap();
    let pending = pending_deliveries(&env.storage);
    assert_eq!(pending.len(), 1);
    let event_id = pending[0].event_id.clone();

    // Restart the daemon on the same database.
    let tmp_root = env.db_path.parent().unwrap().to_path_buf();
    drop(env.daemon);
    let daemon = open_daemon(&env.db_path, &tmp_root);

    // A replayed hook for the state the session is already in does not
    // produce a second event.
    daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "q", "a", "", false)
        .unwrap();
    assert_eq!(pending_deliveries(&env.storage).len(), 1);

    // Even a raw enqueue of the same durable event id stays one row.
    let inserted = env
        .storage
        .enqueue_notification_delivery(
            &event_id,
            device_id,
            pending[0].session_id,
            "needs-input",
            &pending[0].collapse_id,
            "pending",
            now(),
            now(),
        )
        .unwrap();
    assert!(!inserted, "the dedupe key survives the restart");
    assert_eq!(pending_deliveries(&env.storage).len(), 1);

    // A genuinely new transition on the restarted daemon gets a new
    // event id and a second delivery row.
    daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "a", "", false)
        .unwrap();
    daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "q2", "a", "", false)
        .unwrap();
    let pending = pending_deliveries(&env.storage);
    assert_eq!(pending.len(), 2);
    assert_ne!(pending[0].event_id, pending[1].event_id);
}

#[tokio::test]
async fn retries_back_off_and_eventually_send() {
    let env = push_env();
    let mock = MockProvider::default();
    let base = start_mock_provider(mock.clone()).await;
    configure_gateway(&env.daemon, &base);
    mock.push_response(500, serde_json::json!({"reason": "relay down"}));
    mock.push_response(200, serde_json::json!({"status": "rate_limited"}));
    mock.push_response(200, serde_json::json!({"status": "accepted"}));

    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let (_, token) = seeded_session(&env, false, None);
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "a", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "q", "a", "", false)
        .unwrap();
    let event_id = pending_deliveries(&env.storage)[0].event_id.clone();
    let delivery = |storage: &Storage| {
        storage
            .lookup_notification_delivery(&event_id, device_id)
            .unwrap()
            .unwrap()
    };

    let t0 = now();
    let stats = env.daemon.process_push_deliveries(t0).await;
    assert_eq!((stats.attempted, stats.sent), (1, 0));
    let after_first = delivery(&env.storage);
    assert_eq!(after_first.status, "pending");
    assert_eq!(after_first.attempt_count, 1);
    assert!(after_first.next_attempt_at_unix_ms >= t0 + 5_000);

    // Not due yet: nothing is attempted before the backoff elapses.
    let stats = env.daemon.process_push_deliveries(t0 + 1_000).await;
    assert_eq!(stats.attempted, 0);

    let t1 = after_first.next_attempt_at_unix_ms;
    let stats = env.daemon.process_push_deliveries(t1).await;
    assert_eq!((stats.attempted, stats.sent), (1, 0));
    let after_second = delivery(&env.storage);
    assert_eq!(after_second.attempt_count, 2);
    assert!(
        after_second.next_attempt_at_unix_ms >= t1 + 30_000,
        "the second retry waits longer than the first"
    );

    let stats = env
        .daemon
        .process_push_deliveries(after_second.next_attempt_at_unix_ms)
        .await;
    assert_eq!((stats.attempted, stats.sent), (1, 1));
    assert_eq!(delivery(&env.storage).status, "sent");
    assert_eq!(mock.provider_requests().len(), 3);
}

#[tokio::test]
async fn device_not_registered_receipt_disables_the_endpoint() {
    let env = push_env();
    let mock = MockProvider::default();
    let base = start_mock_provider(mock.clone()).await;
    configure_gateway(&env.daemon, &base);
    mock.push_response(200, serde_json::json!({"status": "device_gone"}));

    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let (_, token) = seeded_session(&env, false, None);
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "a", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "q", "a", "", false)
        .unwrap();
    let event_id = pending_deliveries(&env.storage)[0].event_id.clone();

    env.daemon.process_push_deliveries(now()).await;
    let endpoint = env.storage.get_push_endpoint(device_id).unwrap().unwrap();
    assert!(endpoint.disabled_at_unix_ms.is_some());
    assert_eq!(endpoint.disabled_reason, "device-not-registered");
    let settled = env
        .storage
        .lookup_notification_delivery(&event_id, device_id)
        .unwrap()
        .unwrap();
    assert_eq!(settled.status, "unregistered");

    // Later transitions produce no deliveries for the dead endpoint.
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "a", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "q2", "a", "", false)
        .unwrap();
    assert!(pending_deliveries(&env.storage).is_empty());

    // Re-registration turns the endpoint back on.
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let endpoint = env.storage.get_push_endpoint(device_id).unwrap().unwrap();
    assert!(endpoint.disabled_at_unix_ms.is_none());
}

#[tokio::test]
async fn foreground_hint_suppresses_delivery_to_that_device() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let (session_id, token) = seeded_session(&env, false, None);

    let app = pm_daemon::http::router(env.daemon.clone());
    let hint = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/mobile/devices/{device_id}/foreground"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "sessionId": session_id.to_string() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hint.status(), StatusCode::NO_CONTENT);

    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "a", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "q", "a", "", false)
        .unwrap();
    assert!(
        pending_deliveries(&env.storage).is_empty(),
        "the foregrounded device sees the state in-app instead"
    );
    let stats = env.daemon.process_push_deliveries(now()).await;
    assert_eq!(stats.attempted, 0);
}

#[tokio::test]
async fn web_activity_suppresses_delivery_to_every_device_of_that_user() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "app-1");
    let response = register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let (_, token) = seeded_session(&env, false, None);

    let app = pm_daemon::http::router(env.daemon.clone());
    let report_activity = || {
        app.clone().oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/user/activity")
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
    };
    assert_eq!(
        report_activity().await.unwrap().status(),
        StatusCode::NO_CONTENT
    );

    let needs_input = |turn: &str| {
        env.daemon
            .handle_hook_event(&token, HookKind::PromptSubmitted, "", "a", "", false)
            .unwrap();
        env.daemon
            .handle_hook_event(&token, HookKind::NeedsInput, turn, "a", "", false)
            .unwrap();
    };

    needs_input("q1");
    assert!(
        pending_deliveries(&env.storage).is_empty(),
        "the user is at the web UI, so nothing is queued for their phone"
    );
    assert_eq!(env.daemon.process_push_deliveries(now()).await.attempted, 0);

    // The threshold is the user's to set, and zero turns the gate off
    // even while the same interaction is still fresh.
    env.daemon
        .set_push_web_idle_minutes(
            env.storage.get_mobile_device(device_id).unwrap().user_id,
            Some(b"0"),
        )
        .unwrap();
    assert_eq!(
        report_activity().await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    needs_input("q2");
    assert_eq!(
        pending_deliveries(&env.storage).len(),
        1,
        "a zero threshold pushes regardless of web use"
    );
}

#[tokio::test]
async fn reporting_web_activity_needs_an_identity() {
    let env = push_env();
    let app = pm_daemon::http::router(env.daemon.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/user/activity")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn web_activity_is_recorded_against_the_reporting_user_only() {
    use argon2::password_hash::{PasswordHasher, SaltString};
    use argon2::Argon2;
    use rand::rngs::OsRng;

    let env = push_env();
    let (alice, alice_device, alice_bearer) = enroll_device(&env.daemon, "app-alice");
    let bob_hash = Argon2::default()
        .hash_password(b"bob-password-long", &SaltString::generate(&mut OsRng))
        .unwrap()
        .to_string();
    env.storage.create_user("bob", &bob_hash, 2).unwrap();
    let bob_enrollment = env
        .daemon
        .mobile_enroll(pm_daemon::mobile::MobileEnrollRequest {
            proof: pm_daemon::mobile::MobileEnrollProof::Password {
                username: "bob".into(),
                password: "bob-password-long".into(),
            },
            app_installation_id: "app-bob".into(),
            name: "bob phone".into(),
            platform: "ios".into(),
        })
        .unwrap();
    let bob_device = bob_enrollment.device.id;
    assert_ne!(alice, bob_enrollment.device.user_id);
    for (device_id, bearer) in [
        (alice_device, alice_bearer.clone()),
        (bob_device, bob_enrollment.tokens.access_token.clone()),
    ] {
        let response =
            register_via_http(&env.daemon, device_id, &bearer, device_registration()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let app = pm_daemon::http::router(env.daemon.clone());
    let activity = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/user/activity")
                .header(header::AUTHORIZATION, format!("Bearer {alice_bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(activity.status(), StatusCode::NO_CONTENT);

    let (_, token) = seeded_session(&env, false, None);
    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "a", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "q", "a", "", false)
        .unwrap();

    let queued = pending_deliveries(&env.storage);
    assert_eq!(
        queued.iter().map(|d| d.device_id).collect::<Vec<_>>(),
        vec![bob_device],
        "only the user who reported the interaction is held back"
    );
}

fn apply_hook(env: &PushEnv, token: &str, kind: HookKind) {
    env.daemon
        .handle_hook_event(token, kind, "", "snooze-agent", "", false)
        .unwrap();
}

#[tokio::test]
async fn snooze_silences_only_its_current_completion_across_all_notification_paths() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "snooze-app");
    assert_eq!(
        register_via_http(&env.daemon, device_id, &bearer, device_registration())
            .await
            .status(),
        StatusCode::OK
    );
    let (id, token) = seeded_session(&env, true, None);
    apply_hook(&env, &token, HookKind::PromptSubmitted);
    env.daemon.snooze_supervision(id, 5).unwrap();
    let (_, mut events) = env.daemon.subscribe();
    apply_hook(&env, &token, HookKind::TurnEnded);
    assert!(pending_deliveries(&env.storage).is_empty());
    assert!(!env.storage.get_session(id).unwrap().idle_unseen);
    assert!(
        !env.daemon
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
            .idle_unseen
    );
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(
            event,
            pm_protocol::domain::Event::SessionAlert(_)
        ));
    }
    apply_hook(&env, &token, HookKind::PromptSubmitted);
    apply_hook(&env, &token, HookKind::TurnEnded);
    assert!(env.storage.get_session(id).unwrap().idle_unseen);
    let pending = pending_deliveries(&env.storage);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "completed");
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(event,
        pm_protocol::domain::Event::SessionAlert(alert) if alert.kind == pm_protocol::domain::SessionAlertKind::Completed)));
}

#[tokio::test]
async fn prompt_submitted_after_snooze_restores_push_completion() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "snooze-new-prompt");
    assert_eq!(
        register_via_http(&env.daemon, device_id, &bearer, device_registration())
            .await
            .status(),
        StatusCode::OK
    );
    let (id, token) = seeded_session(&env, true, None);
    apply_hook(&env, &token, HookKind::PromptSubmitted);
    env.daemon.snooze_supervision(id, 5).unwrap();
    apply_hook(&env, &token, HookKind::PromptSubmitted);
    apply_hook(&env, &token, HookKind::TurnEnded);
    assert!(env.storage.get_session(id).unwrap().idle_unseen);
    assert_eq!(pending_deliveries(&env.storage).len(), 1);
}

#[tokio::test]
async fn snooze_preserves_needs_input_alerts_and_survives_controller_restart() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "snooze-restart");
    assert_eq!(
        register_via_http(&env.daemon, device_id, &bearer, device_registration())
            .await
            .status(),
        StatusCode::OK
    );
    let (id, token) = seeded_session(&env, true, None);
    apply_hook(&env, &token, HookKind::PromptSubmitted);
    let until = env.daemon.snooze_supervision(id, 5).unwrap();
    let restarted = open_daemon(&env.db_path, env._tmp.path());
    restarted
        .handle_hook_event(&token, HookKind::TurnEnded, "", "snooze-agent", "", false)
        .unwrap();
    assert!(!env.storage.get_session(id).unwrap().idle_unseen);
    assert!(pending_deliveries(&env.storage).is_empty());
    let generation = env.storage.agent_terminal(id).unwrap().generation;
    assert_eq!(
        env.storage
            .supervision_snoozed_until(id, generation)
            .unwrap(),
        until
    );
    assert_eq!(
        env.storage
            .supervision_snoozed_until(id, generation + 1)
            .unwrap(),
        0
    );
    assert!(!env
        .storage
        .finish_supervision_turn(id, generation + 1, Some(1))
        .unwrap());
    restarted
        .handle_hook_event(
            &token,
            HookKind::PromptSubmitted,
            "",
            "snooze-agent",
            "",
            false,
        )
        .unwrap();
    restarted.snooze_supervision(id, 5).unwrap();
    restarted
        .handle_hook_event(
            &token,
            HookKind::NeedsInput,
            "need permission",
            "snooze-agent",
            "",
            false,
        )
        .unwrap();
    assert!(env.storage.get_session(id).unwrap().needs_input_unseen);
    let pending = pending_deliveries(&env.storage);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "needs-input");
}

#[tokio::test]
async fn snooze_after_flag_blocked_keeps_push_and_restores_completion_on_new_prompt() {
    let env = push_env();
    let (_, device_id, bearer) = enroll_device(&env.daemon, "blocked-snooze");
    assert_eq!(
        register_via_http(&env.daemon, device_id, &bearer, device_registration())
            .await
            .status(),
        StatusCode::OK
    );
    let (id, token) = seeded_session(&env, true, None);
    apply_hook(&env, &token, HookKind::PromptSubmitted);
    env.daemon
        .apply_blocked(id, "waiting for your decision".into())
        .unwrap();
    env.daemon.snooze_supervision(id, 60).unwrap();
    apply_hook(&env, &token, HookKind::TurnEnded);
    let session = env.storage.get_session(id).unwrap();
    assert!(session.needs_input_unseen);
    assert!(!session.idle_unseen);
    let pending = pending_deliveries(&env.storage);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "needs-input");
    apply_hook(&env, &token, HookKind::PromptSubmitted);
    apply_hook(&env, &token, HookKind::TurnEnded);
    let pending = pending_deliveries(&env.storage);
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().any(|delivery| delivery.state == "completed"));
    assert!(env.storage.get_session(id).unwrap().idle_unseen);
}
