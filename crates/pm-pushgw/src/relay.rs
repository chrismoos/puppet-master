//! HTTP relay endpoint and request validation.
//!
//! The gateway is stateless about devices: it holds no registrations,
//! no device tokens, and no disk state. Each push request carries the
//! raw APNs device token, which is the bearer credential for which
//! device receives the push.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use pm_protocol::gateway::{
    is_valid_device_token, GatewayPushRequest, GatewayPushResponse, GatewayPushStatus,
    PUSH_PLACEHOLDER_BODY, PUSH_PLACEHOLDER_TITLE, SEALED_PAYLOAD_KEY,
};
use pm_push::{ApnsProvider, SendOutcome};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::client_ip::{client_ip, source_key, TrustedProxies};
use crate::limits::{Admission, LimitPolicy, Limiter, MemoryStore};

/// The APNs reason for a token that was never valid for the topic and
/// environment it was sent to. `Unregistered` is deliberately not
/// counted against a source: it answers a token that was valid once,
/// which an honest controller sends after an app is uninstalled.
const APNS_REASON_BAD_DEVICE_TOKEN: &str = "BadDeviceToken";

pub struct RelayState {
    apns: ApnsProvider,
    limiter: Limiter,
    trusted_proxies: TrustedProxies,
}

impl RelayState {
    pub fn new(apns: ApnsProvider, trusted_proxies: TrustedProxies, limits: LimitPolicy) -> Self {
        let limiter = Limiter::new(Arc::new(MemoryStore::default()), limits);
        Self::with_limiter(apns, trusted_proxies, limiter)
    }

    pub fn with_limiter(
        apns: ApnsProvider,
        trusted_proxies: TrustedProxies,
        limiter: Limiter,
    ) -> Self {
        Self {
            apns,
            limiter,
            trusted_proxies,
        }
    }

    /// Full SHA-256 digest of a device token, for rate-limit keying.
    /// Uses the full 32 bytes to avoid collision between distinct tokens.
    fn token_hash(token: &str) -> String {
        let mut h = Sha256::new();
        h.update(token.as_bytes());
        hex::encode(h.finalize())
    }

    /// Short hash for safe logging. NOT for keying — too narrow.
    fn log_hash(token: &str) -> String {
        let mut h = Sha256::new();
        h.update(token.as_bytes());
        let digest = h.finalize();
        hex::encode(&digest[..8])
    }
}

fn rate_limited(detail: &str) -> Json<GatewayPushResponse> {
    Json(GatewayPushResponse {
        status: GatewayPushStatus::RateLimited,
        detail: Some(detail.into()),
    })
}

pub fn router(state: Arc<RelayState>) -> Router {
    Router::new()
        .route("/v1/push", post(handle_push))
        .route("/health", axum::routing::get(|| async { "ok" }))
        .with_state(state)
}

// ── Helpers ─────────────────────────────────────────────────────────

// ── Push relay ───────────────────────────────────────────────────────

async fn handle_push(
    State(state): State<Arc<RelayState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<GatewayPushRequest>,
) -> impl IntoResponse {
    let token_log = RelayState::log_hash(&request.device_token);

    // 1. Device token format validation — BEFORE the token is
    //    interpolated into the APNs URL.
    if !is_valid_device_token(&request.device_token) {
        return Json(GatewayPushResponse {
            status: GatewayPushStatus::Rejected,
            detail: Some("device_token must be ASCII hex, 32–200 chars".into()),
        });
    }

    // 2. Environment validation.
    if request.environment != "production" && request.environment != "sandbox" {
        return Json(GatewayPushResponse {
            status: GatewayPushStatus::Rejected,
            detail: Some("environment must be production or sandbox".into()),
        });
    }

    // 3. Source blocks and rate limits.
    let source = source_key(client_ip(addr.ip(), &headers, &state.trusted_proxies));
    let token_key = RelayState::token_hash(&request.device_token);
    match state.limiter.admit(&source, &token_key).await {
        Admission::Allowed => {}
        Admission::Blocked(remaining) => {
            debug!(
                source = %source,
                remaining_secs = remaining.as_secs(),
                "push refused, source is blocked"
            );
            return rate_limited("source is temporarily blocked for sending invalid device tokens");
        }
        Admission::SourceLimited => {
            debug!(source = %source, "push refused, source over its rate limit");
            return rate_limited("too many requests from this source");
        }
        Admission::TokenLimited => {
            debug!(
                source = %source, token_hash = %token_log,
                "push refused, device token over its rate limit"
            );
            return rate_limited("too many requests for this device token");
        }
        Admission::StoreFailed => return rate_limited("rate limiting is unavailable"),
    }

    // 4. Relay to APNs.
    let message = relay_message(
        request.device_token.clone(),
        request.environment.clone(),
        request.collapse_id.clone(),
        &request.sealed_payload,
    );

    let (result, attempt) = state.apns.send_reporting(&message).await;

    let host = attempt.host.as_str();
    let topic = attempt.topic.as_str();
    let environment = attempt.environment.as_str();
    let status = attempt.status_field();
    let apns_id = attempt.apns_id_field();
    let reason = attempt.reason_field();

    match result {
        SendOutcome::Sent { .. } => {
            debug!(
                host, topic, environment,
                status = %status, apns_id, reason,
                token_hash = %token_log,
                "push relayed to APNs"
            );
            Json(GatewayPushResponse {
                status: GatewayPushStatus::Accepted,
                detail: None,
            })
        }
        SendOutcome::DeviceNotRegistered => {
            // APNs says the token is gone (410 Unregistered or
            // 400 BadDeviceToken). Return synchronously — no stored
            // feedback needed since the gateway is stateless.
            //
            // `BadDeviceToken` also answers a token minted in the other
            // environment, so `host` and `environment` are logged beside
            // the reason to show which pair was actually addressed.
            info!(
                host, topic, environment,
                status = %status, apns_id, reason,
                token_hash = %token_log, source = %source,
                "APNs reports device gone"
            );
            if reason == APNS_REASON_BAD_DEVICE_TOKEN {
                if let Some(blocked_for) = state.limiter.record_bad_token(&source).await {
                    warn!(
                        source = %source,
                        block_secs = blocked_for.as_secs(),
                        "source blocked for sending invalid device tokens"
                    );
                }
            }
            Json(GatewayPushResponse {
                status: GatewayPushStatus::DeviceGone,
                detail: Some("device-not-registered".into()),
            })
        }
        SendOutcome::Retryable { detail } => {
            warn!(
                host, topic, environment,
                status = %status, apns_id, reason,
                token_hash = %token_log, detail = %detail,
                "APNs transient error"
            );
            Json(GatewayPushResponse {
                status: GatewayPushStatus::Rejected,
                detail: Some(detail),
            })
        }
        SendOutcome::Rejected { detail } => {
            warn!(
                host, topic, environment,
                status = %status, apns_id, reason,
                token_hash = %token_log, detail = %detail,
                "APNs rejected the push"
            );
            Json(GatewayPushResponse {
                status: GatewayPushStatus::Rejected,
                detail: Some(detail),
            })
        }
    }
}

/// The notification the relay hands to APNs. Title and body are fixed
/// placeholders: the gateway cannot read the sealed blob, so the real
/// content is only assembled on the device by the notification service
/// extension, which `mutable_content` is what invokes.
fn relay_message(
    device_token: String,
    environment: String,
    collapse_id: String,
    sealed_payload: &str,
) -> pm_push::PushMessage {
    pm_push::PushMessage {
        token: device_token,
        title: PUSH_PLACEHOLDER_TITLE.into(),
        body: PUSH_PLACEHOLDER_BODY.into(),
        collapse_id,
        event_id: String::new(),
        data: serde_json::json!({ SEALED_PAYLOAD_KEY: sealed_payload }),
        environment,
        mutable_content: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use pm_protocol::gateway::PUSH_DATA_KEY;
    use tower::util::ServiceExt;

    const PROXY: [u8; 4] = [127, 0, 0, 1];

    fn test_state() -> Arc<RelayState> {
        Arc::new(RelayState::new(
            create_test_apns("https://localhost:0".into()),
            TrustedProxies::default(),
            LimitPolicy::default(),
        ))
    }

    /// A relay behind a trusted proxy on loopback, sending to `apns`.
    fn proxied_state(apns: String) -> Arc<RelayState> {
        Arc::new(RelayState::new(
            create_test_apns(apns),
            TrustedProxies::new([PROXY.into()]),
            LimitPolicy::default(),
        ))
    }

    /// Stands in for APNs and answers every push the same way.
    async fn fake_apns(status: StatusCode, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().fallback(move || async move { (status, body) });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn device_token(n: u32) -> String {
        format!("{n:064x}")
    }

    fn create_test_apns(endpoint: String) -> ApnsProvider {
        let test_key = "-----BEGIN PRIVATE KEY-----\n\
            MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgHh0kMwDgKewsZ+RB\n\
            Qyqbbbrv15tZlzawIipPVulFoPqhRANCAARGJC5z51GNxouCHlHMDp6Pb5yGEzZ5\n\
            RspQCuEcKlVwuhoPF7oRE9YaZSR7NPrdQJ66YI1Rh5KHhMRpI5PnLs2k\n\
            -----END PRIVATE KEY-----";
        ApnsProvider::new(pm_push::ApnsConfig {
            key_p8: test_key.into(),
            key_id: "TESTKEY123".into(),
            sandbox_key: None,
            team_id: "TESTTEAM".into(),
            topic: "com.test.app".into(),
            endpoint_override: Some(endpoint),
        })
        .unwrap()
    }

    fn make_request(token: &str, env: &str) -> GatewayPushRequest {
        GatewayPushRequest {
            device_token: token.into(),
            sealed_payload: B64.encode(b"sealed-payload-bytes"),
            environment: env.into(),
            collapse_id: "collapse".into(),
        }
    }

    async fn do_push(
        app: &Router,
        request: &GatewayPushRequest,
    ) -> (StatusCode, GatewayPushResponse) {
        do_push_from(app, request, None).await
    }

    /// Sends a push as the proxy would, naming `client` as the address
    /// it forwarded for.
    async fn do_push_from(
        app: &Router,
        request: &GatewayPushRequest,
        client: Option<&str>,
    ) -> (StatusCode, GatewayPushResponse) {
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/push")
            .header("content-type", "application/json")
            .extension(ConnectInfo(SocketAddr::from((PROXY, 9999))));
        if let Some(client) = client {
            builder = builder.header("x-forwarded-for", client);
        }
        let response = app
            .clone()
            .oneshot(
                builder
                    .body(axum::body::Body::from(
                        serde_json::to_string(request).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body: GatewayPushResponse = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        (status, body)
    }

    // ── Payload tests ───────────────────────────────────────────────

    #[test]
    fn the_relayed_payload_sets_mutable_content_and_carries_the_sealed_blob() {
        let message = relay_message(
            "abcd1234".into(),
            "sandbox".into(),
            "session-7".into(),
            "c2VhbGVk",
        );
        assert_eq!(message.environment, "sandbox");
        assert!(message.mutable_content);

        let payload = pm_push::apns_payload(&message);
        assert_eq!(payload["aps"]["mutable-content"], 1);
        assert_eq!(payload["aps"]["thread-id"], "session-7");
        assert_eq!(payload[PUSH_DATA_KEY][SEALED_PAYLOAD_KEY], "c2VhbGVk");
        assert_eq!(payload["aps"]["alert"]["title"], PUSH_PLACEHOLDER_TITLE);
        assert_eq!(payload["aps"]["alert"]["body"], PUSH_PLACEHOLDER_BODY);
    }

    // ── Limits ──────────────────────────────────────────────────────

    const CLIENT_A: &str = "203.0.113.9";
    const CLIENT_B: &str = "198.51.100.7";
    const BAD_TOKEN_BODY: &str = r#"{"reason":"BadDeviceToken"}"#;
    const UNREGISTERED_BODY: &str = r#"{"reason":"Unregistered"}"#;
    const BLOCKED_DETAIL: &str = "temporarily blocked";

    /// Sends `count` pushes from `client`, each to a different token,
    /// and returns the last answer.
    async fn push_distinct_tokens(app: &Router, client: &str, count: u32) -> GatewayPushResponse {
        let mut last = None;
        for n in 0..count {
            let request = make_request(&device_token(n), "production");
            last = Some(do_push_from(app, &request, Some(client)).await.1);
        }
        last.expect("at least one push")
    }

    #[tokio::test]
    async fn clients_behind_the_proxy_are_limited_separately() {
        let apns = fake_apns(StatusCode::OK, "").await;
        let app = router(proxied_state(apns));
        let policy = LimitPolicy::default();

        let last = push_distinct_tokens(&app, CLIENT_A, policy.pushes_per_source).await;
        assert_eq!(last.status, GatewayPushStatus::Accepted);
        let over = push_distinct_tokens(&app, CLIENT_A, 1).await;
        assert_eq!(over.status, GatewayPushStatus::RateLimited);

        let other = push_distinct_tokens(&app, CLIENT_B, 1).await;
        assert_eq!(other.status, GatewayPushStatus::Accepted);
    }

    #[tokio::test]
    async fn a_source_sending_invalid_tokens_is_blocked() {
        let apns = fake_apns(StatusCode::BAD_REQUEST, BAD_TOKEN_BODY).await;
        let app = router(proxied_state(apns));
        let policy = LimitPolicy::default();

        let last = push_distinct_tokens(&app, CLIENT_A, policy.bad_tokens_per_source).await;
        assert_eq!(last.status, GatewayPushStatus::DeviceGone);

        let blocked = push_distinct_tokens(&app, CLIENT_A, 1).await;
        assert_eq!(blocked.status, GatewayPushStatus::RateLimited);
        assert!(blocked.detail.unwrap().contains(BLOCKED_DETAIL));

        let other = push_distinct_tokens(&app, CLIENT_B, 1).await;
        assert_eq!(other.status, GatewayPushStatus::DeviceGone);
    }

    /// A controller clearing out devices whose app was uninstalled is
    /// answered `Unregistered` for each, and must not be blocked for it.
    #[tokio::test]
    async fn unregistered_tokens_do_not_block_the_source() {
        let apns = fake_apns(StatusCode::GONE, UNREGISTERED_BODY).await;
        let app = router(proxied_state(apns));
        let policy = LimitPolicy::default();

        let last = push_distinct_tokens(&app, CLIENT_A, policy.bad_tokens_per_source * 2).await;
        assert_eq!(last.status, GatewayPushStatus::DeviceGone);
    }

    /// Without a trusted proxy the forwarded address is the sender's
    /// own claim, so a block must land on the peer and not be escaped
    /// by changing the header.
    #[tokio::test]
    async fn an_untrusted_forwarded_address_does_not_escape_a_block() {
        let apns = fake_apns(StatusCode::BAD_REQUEST, BAD_TOKEN_BODY).await;
        let app = router(Arc::new(RelayState::new(
            create_test_apns(apns),
            TrustedProxies::default(),
            LimitPolicy::default(),
        )));
        let policy = LimitPolicy::default();

        push_distinct_tokens(&app, CLIENT_A, policy.bad_tokens_per_source).await;
        let blocked = push_distinct_tokens(&app, CLIENT_B, 1).await;
        assert_eq!(blocked.status, GatewayPushStatus::RateLimited);
        assert!(blocked.detail.unwrap().contains(BLOCKED_DETAIL));
    }

    #[test]
    fn token_hash_is_full_length() {
        let h = RelayState::token_hash("tok1");
        assert_eq!(
            h.len(),
            64,
            "token hash must be full SHA-256 (32 bytes hex)"
        );
    }

    #[test]
    fn log_hash_is_short() {
        let h = RelayState::log_hash("tok1");
        assert_eq!(h.len(), 16, "log hash is 8 bytes hex");
    }

    // ── Token validation tests ──────────────────────────────────────

    #[tokio::test]
    async fn push_rejects_a_token_that_is_not_hex() {
        let state = test_state();
        let app = router(state);
        let req = make_request("../../../etc/passwd", "production");
        let (_, body) = do_push(&app, &req).await;
        assert_eq!(body.status, GatewayPushStatus::Rejected);
        assert!(body.detail.as_deref().unwrap().contains("hex"));
    }

    #[tokio::test]
    async fn push_rejects_too_short_token() {
        let state = test_state();
        let app = router(state);
        let req = make_request("abcdef", "production");
        let (_, body) = do_push(&app, &req).await;
        assert_eq!(body.status, GatewayPushStatus::Rejected);
    }

    // ── Signature tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn push_rejects_bad_environment_value() {
        let state = test_state();
        let app = router(state);
        let token = "a".repeat(64);
        let req = make_request(&token, "staging");
        let (_, body) = do_push(&app, &req).await;
        assert_eq!(body.status, GatewayPushStatus::Rejected);
        assert!(body.detail.as_deref().unwrap().contains("environment"));
    }

    // ── Health ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_endpoint() {
        let state = test_state();
        let app = router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
