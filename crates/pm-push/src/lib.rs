//! Push provider adapters. The daemon hosts delivery and talks to
//! APNs and FCM directly with operator-supplied credentials, so
//! notification metadata never routes through a third-party relay.
//! The trait stays narrow (`send`, `receipt`, `invalidate_device`) so
//! a Puppet Master-operated proxy provider can slot in later without
//! changing session events or device records.
//!
//! The `gateway` module adds a third provider that seals, signs and
//! relays through a centralized push gateway, letting controllers
//! without their own APNs key still deliver notifications.

pub mod gateway;
pub mod hpke;

use std::time::Duration;

use pm_protocol::gateway::{APNS_PAYLOAD_CEILING, PUSH_DATA_KEY};
use serde::Deserialize;
use tracing::{debug, warn};

pub const PROVIDER_APNS: &str = "apns";

pub const ENVIRONMENT_PRODUCTION: &str = "production";
pub const ENVIRONMENT_SANDBOX: &str = "sandbox";

const APNS_PRODUCTION_ENDPOINT: &str = "https://api.push.apple.com";
const APNS_SANDBOX_ENDPOINT: &str = "https://api.sandbox.push.apple.com";

/// APNs rejects provider tokens older than an hour; refresh under that.
const APNS_JWT_LIFETIME: Duration = Duration::from_secs(45 * 60);
const PROVIDER_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

const APNS_ID_HEADER: &str = "apns-id";
const APNS_STATUS_GONE: u16 = 410;
const APNS_STATUS_TOO_MANY_REQUESTS: u16 = 429;
const APNS_STATUS_SERVER_ERROR_FIRST: u16 = 500;
const APNS_STATUS_SERVER_ERROR_LAST: u16 = 599;

/// One notification handed to a provider. Everything in it is already
/// policy-filtered: no terminal text, prompts, paths, or secrets, and
/// title/body carry project/headline only when previews are enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushMessage {
    /// Decrypted platform token. Only the debug-level send audit line
    /// carries it.
    pub token: String,
    pub title: String,
    pub body: String,
    /// Per-session collapse key so a later state replaces stale content.
    pub collapse_id: String,
    /// Durable event id, also delivered so the app can dedupe.
    pub event_id: String,
    /// Opaque routing payload (controller/session ids, state).
    pub data: serde_json::Value,
    /// `production` or `sandbox`; selects the APNs host.
    pub environment: String,
    /// Sets the APNs `mutable-content` flag, without which iOS never
    /// invokes the notification service extension. Ignored by FCM,
    /// which has no equivalent in its own message shape.
    pub mutable_content: bool,
}

/// The settled result of one send attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Sent {
        provider_message_id: Option<String>,
    },
    /// Transient provider or network trouble; retry with backoff.
    Retryable {
        detail: String,
    },
    /// The platform reports the token is gone; disable the endpoint.
    DeviceNotRegistered,
    /// Permanent rejection (bad credentials or payload); do not retry.
    Rejected {
        detail: String,
    },
}

/// A later receipt check for providers that settle asynchronously.
/// Direct APNs/FCM sends settle in the send response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptOutcome {
    Final,
    DeviceNotRegistered,
}

/// What one APNs send addressed and what APNs answered, so a caller can
/// log a rejection without re-deriving any of it. Deliberately carries
/// no device token: callers log a hash of their own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApnsAttempt {
    /// Base URL the request went to, including any endpoint override.
    pub host: String,
    pub topic: String,
    pub environment: String,
    /// Absent when the request never reached APNs.
    pub status: Option<u16>,
    pub apns_id: Option<String>,
    /// The APNs `reason` string, such as `BadDeviceToken` or `BadTopic`.
    pub reason: Option<String>,
}

impl ApnsAttempt {
    /// Renders an absent field as `-` so every log line has the same
    /// shape whether or not APNs answered.
    pub fn status_field(&self) -> String {
        self.status
            .map_or_else(|| MISSING_FIELD.to_string(), |s| s.to_string())
    }

    pub fn apns_id_field(&self) -> &str {
        self.apns_id.as_deref().unwrap_or(MISSING_FIELD)
    }

    pub fn reason_field(&self) -> &str {
        self.reason.as_deref().unwrap_or(MISSING_FIELD)
    }
}

/// Stands in for a log field APNs did not give us.
pub const MISSING_FIELD: &str = "-";

#[async_trait::async_trait]
pub trait PushProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn send(&self, message: &PushMessage) -> SendOutcome;
    /// Checks a previously returned provider message id. Providers that
    /// settle synchronously return `Final`.
    async fn receipt(&self, provider_message_id: &str) -> ReceiptOutcome;
    /// Best-effort provider-side cleanup when an endpoint is disabled.
    /// APNs/FCM have no unregister API, so the direct adapters only
    /// drop cached state.
    async fn invalidate_device(&self, token: &str);
}

fn provider_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(PROVIDER_HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// The value APNs expects for a set `aps` flag.
const APS_FLAG_SET: u8 = 1;

/// The one APNs payload builder. Both the daemon's direct sends and the
/// gateway relay go through it, so the two cannot drift.
///
/// `"pm"` carries the sealed blob and nothing else. It is not also
/// copied to `"body"` (the key expo-notifications maps to
/// `content.data`): the copy would double the ciphertext and push the
/// payload past `APNS_PAYLOAD_CEILING`, which `SEALED_JSON_BUDGET` is
/// derived from on the assumption of a single copy. The notification
/// service extension writes the decrypted routing fields to both keys
/// on the device, where the duplication costs nothing.
/// The payload's serialized size when it is over what APNs accepts, or `None`
/// when it fits.
///
/// The compile-time assertion beside the padding constants compares two
/// constants, which says the design fits and not that a given payload does.
/// This measures the bytes that would actually go on the wire.
fn oversized_payload(body: &serde_json::Value) -> Option<usize> {
    let len = serde_json::to_vec(body).map(|v| v.len()).unwrap_or(0);
    (len > APNS_PAYLOAD_CEILING).then_some(len)
}

pub fn apns_payload(message: &PushMessage) -> serde_json::Value {
    let mut aps = serde_json::json!({
        "alert": { "title": message.title, "body": message.body },
        "thread-id": message.collapse_id,
        "sound": "default",
    });
    if message.mutable_content {
        aps["mutable-content"] = serde_json::json!(APS_FLAG_SET);
    }
    serde_json::json!({
        "aps": aps,
        PUSH_DATA_KEY: message.data,
    })
}

/// Token-based APNs auth: an ES256 JWT signed with the operator's .p8
/// key, refreshed before Apple's one-hour limit.
pub struct ApnsProvider {
    /// Signs production sends, and sandbox sends too when no sandbox
    /// key is configured.
    key: SigningKey,
    sandbox_key: Option<SigningKey>,
    team_id: String,
    topic: String,
    /// Overrides the per-environment APNs host; used for local
    /// contract tests and self-hosted relays.
    endpoint_override: Option<String>,
    client: reqwest::Client,
}

/// A signing key and the id APNs identifies it by. No `Debug`: a
/// derived one would print the PEM.
pub struct ApnsKey {
    /// PEM contents of the PKCS#8 .p8 signing key.
    pub key_p8: String,
    pub key_id: String,
}

pub struct ApnsConfig {
    /// PEM contents of the PKCS#8 .p8 signing key.
    pub key_p8: String,
    pub key_id: String,
    /// Signs sandbox sends when present. Without it the key above
    /// serves both environments, which is all a key scoped to both
    /// requires.
    pub sandbox_key: Option<ApnsKey>,
    pub team_id: String,
    /// The app bundle id, sent as apns-topic.
    pub topic: String,
    pub endpoint_override: Option<String>,
}

/// One signing key and the provider token minted from it. A key scoped
/// to one APNs environment is answered with `BadEnvironmentKeyInToken`
/// in the other, so each key caches its own JWT instead of sharing one.
struct SigningKey {
    key: jsonwebtoken::EncodingKey,
    key_id: String,
    jwt: std::sync::Mutex<Option<(String, std::time::Instant)>>,
}

impl SigningKey {
    fn new(key_p8: &str, key_id: String, what: &str) -> Result<Self, String> {
        let key = jsonwebtoken::EncodingKey::from_ec_pem(key_p8.as_bytes())
            .map_err(|_| format!("APNs {what} is not a valid EC PEM key"))?;
        Ok(Self {
            key,
            key_id,
            jwt: std::sync::Mutex::new(None),
        })
    }

    fn bearer_jwt(&self, team_id: &str) -> Result<String, String> {
        let mut cached = self.jwt.lock().unwrap();
        if let Some((token, minted)) = cached.as_ref() {
            if minted.elapsed() < APNS_JWT_LIFETIME {
                return Ok(token.clone());
            }
        }
        let issued_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let claims = serde_json::json!({ "iss": team_id, "iat": issued_at });
        let token = jsonwebtoken::encode(&header, &claims, &self.key)
            .map_err(|e| format!("APNs JWT signing failed: {e}"))?;
        *cached = Some((token.clone(), std::time::Instant::now()));
        Ok(token)
    }
}

impl ApnsProvider {
    pub fn new(config: ApnsConfig) -> Result<Self, String> {
        if config.key_id.is_empty() || config.team_id.is_empty() || config.topic.is_empty() {
            return Err("APNs needs key id, team id, and topic".into());
        }
        let key = SigningKey::new(&config.key_p8, config.key_id, "signing key")?;
        let sandbox_key = match config.sandbox_key {
            Some(sandbox) if sandbox.key_id.is_empty() => {
                return Err("APNs sandbox signing key needs a key id".into())
            }
            Some(sandbox) => Some(SigningKey::new(
                &sandbox.key_p8,
                sandbox.key_id,
                "sandbox signing key",
            )?),
            None => None,
        };
        Ok(Self {
            key,
            sandbox_key,
            team_id: config.team_id,
            topic: config.topic,
            endpoint_override: config.endpoint_override,
            client: provider_client()?,
        })
    }

    fn endpoint(&self, environment: &str) -> String {
        if let Some(endpoint) = &self.endpoint_override {
            return endpoint.trim_end_matches('/').to_string();
        }
        if environment == ENVIRONMENT_SANDBOX {
            APNS_SANDBOX_ENDPOINT.to_string()
        } else {
            APNS_PRODUCTION_ENDPOINT.to_string()
        }
    }

    /// The key the message's environment calls for. An endpoint
    /// override pins the host but deliberately not the key, so a
    /// sandbox message relayed through a local host is still signed
    /// for sandbox.
    fn signing_key(&self, environment: &str) -> &SigningKey {
        match &self.sandbox_key {
            Some(sandbox) if environment == ENVIRONMENT_SANDBOX => sandbox,
            _ => &self.key,
        }
    }

    fn bearer_jwt(&self, environment: &str) -> Result<String, String> {
        self.signing_key(environment).bearer_jwt(&self.team_id)
    }
}

#[derive(Deserialize)]
struct ApnsErrorBody {
    reason: Option<String>,
}

impl ApnsProvider {
    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// Sends and reports what the exchange addressed and what came back.
    /// `PushProvider::send` drops the report; a caller that logs the
    /// outcome uses this instead, since the host, topic, status,
    /// `apns-id` and `reason` are known only here.
    pub async fn send_reporting(&self, message: &PushMessage) -> (SendOutcome, ApnsAttempt) {
        let host = self.endpoint(&message.environment);
        let mut attempt = ApnsAttempt {
            host: host.clone(),
            topic: self.topic.clone(),
            environment: message.environment.clone(),
            ..ApnsAttempt::default()
        };
        let jwt = match self.bearer_jwt(&message.environment) {
            Ok(jwt) => jwt,
            Err(detail) => return (SendOutcome::Rejected { detail }, attempt),
        };
        let url = format!("{host}/3/device/{}", message.token);
        let body = apns_payload(message);
        // Refused here rather than discovered in APNs rejections. An oversized
        // payload fails identically on every retry, because nothing about it
        // changes, so the queue spends its whole backoff on a send that cannot
        // work and the user is simply never told. Rejected rather than
        // Retryable for the same reason.
        if let Some(oversized) = oversized_payload(&body) {
            warn!(
                bytes = oversized,
                ceiling = APNS_PAYLOAD_CEILING,
                collapse_id = %message.collapse_id,
                "refusing a push larger than APNs accepts: it would be rejected on every retry"
            );
            return (
                SendOutcome::Rejected {
                    detail: format!(
                        "payload is {oversized} bytes, over the {APNS_PAYLOAD_CEILING}-byte \
                         APNs limit"
                    ),
                },
                attempt,
            );
        }
        // An audit of what a send really carries, safe to emit only
        // because the payload is redacted: content and routing stay
        // inside the sealed blob. The device token is deliberate, and
        // debug keeps it out of default output.
        debug!(
            host = %host,
            topic = %self.topic,
            environment = %message.environment,
            collapse_id = %message.collapse_id,
            device_token = %message.token,
            payload = %body,
            "APNs send payload"
        );
        let response = self
            .client
            .post(&url)
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-collapse-id", &message.collapse_id)
            .json(&body)
            .send()
            .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                return (
                    SendOutcome::Retryable {
                        detail: format!("APNs request failed: {e}"),
                    },
                    attempt,
                )
            }
        };
        let status = response.status();
        attempt.status = Some(status.as_u16());
        attempt.apns_id = response
            .headers()
            .get(APNS_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if status.is_success() {
            let provider_message_id = attempt.apns_id.clone();
            return (
                SendOutcome::Sent {
                    provider_message_id,
                },
                attempt,
            );
        }
        let reason = response
            .json::<ApnsErrorBody>()
            .await
            .ok()
            .and_then(|b| b.reason)
            .unwrap_or_default();
        if !reason.is_empty() {
            attempt.reason = Some(reason.clone());
        }
        let outcome = match (status.as_u16(), reason.as_str()) {
            (APNS_STATUS_GONE, _)
            | (_, "Unregistered")
            | (_, "BadDeviceToken")
            | (_, "ExpiredToken") => SendOutcome::DeviceNotRegistered,
            (APNS_STATUS_TOO_MANY_REQUESTS, _)
            | (APNS_STATUS_SERVER_ERROR_FIRST..=APNS_STATUS_SERVER_ERROR_LAST, _) => {
                SendOutcome::Retryable {
                    detail: format!("APNs {status}: {reason}"),
                }
            }
            _ => SendOutcome::Rejected {
                detail: format!("APNs {status}: {reason}"),
            },
        };
        (outcome, attempt)
    }
}

#[async_trait::async_trait]
impl PushProvider for ApnsProvider {
    fn name(&self) -> &'static str {
        PROVIDER_APNS
    }

    async fn send(&self, message: &PushMessage) -> SendOutcome {
        self.send_reporting(message).await.0
    }

    async fn receipt(&self, _provider_message_id: &str) -> ReceiptOutcome {
        ReceiptOutcome::Final
    }

    async fn invalidate_device(&self, _token: &str) {
        debug!("APNs endpoint invalidated");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::{general_purpose::STANDARD as B64, Engine};
    use pm_protocol::gateway::{
        SealedNotification, PUSH_PLACEHOLDER_BODY, PUSH_PLACEHOLDER_TITLE, SEALED_PAYLOAD_KEY,
    };

    /// A P-256 key generated for these tests. It signs nothing that
    /// leaves the process and is not an APNs credential.
    const TEST_SIGNING_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
        MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgHh0kMwDgKewsZ+RB\n\
        Qyqbbbrv15tZlzawIipPVulFoPqhRANCAARGJC5z51GNxouCHlHMDp6Pb5yGEzZ5\n\
        RspQCuEcKlVwuhoPF7oRE9YaZSR7NPrdQJ66YI1Rh5KHhMRpI5PnLs2k\n\
        -----END PRIVATE KEY-----";

    /// A second P-256 key, standing in for an operator whose sandbox
    /// .p8 differs from the production one. Also signs nothing real.
    const TEST_SANDBOX_SIGNING_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
        MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg00JOo9Qkvn49eG57\n\
        QsFJfoYTk2UGdnZE1+CKX7nKo1WhRANCAAQd36d37ksToHKrE7F00tASCBR1NxVd\n\
        oaf0EIWuxrRIDrf5BqjK4bkh8LfzaWgcCaE6gnvLBM1qwpeS7prb7Y4u\n\
        -----END PRIVATE KEY-----";

    const TEST_TOPIC: &str = "com.test.app";
    const TEST_KEY_ID: &str = "TESTKEY123";
    const TEST_SANDBOX_KEY_ID: &str = "SANDKEY456";

    fn provider(endpoint_override: Option<String>) -> ApnsProvider {
        ApnsProvider::new(ApnsConfig {
            key_p8: TEST_SIGNING_KEY.into(),
            key_id: TEST_KEY_ID.into(),
            sandbox_key: None,
            team_id: "TESTTEAM".into(),
            topic: TEST_TOPIC.into(),
            endpoint_override,
        })
        .unwrap()
    }

    fn provider_with_sandbox_key(endpoint_override: Option<String>) -> ApnsProvider {
        ApnsProvider::new(ApnsConfig {
            key_p8: TEST_SIGNING_KEY.into(),
            key_id: TEST_KEY_ID.into(),
            sandbox_key: Some(ApnsKey {
                key_p8: TEST_SANDBOX_SIGNING_KEY.into(),
                key_id: TEST_SANDBOX_KEY_ID.into(),
            }),
            team_id: "TESTTEAM".into(),
            topic: TEST_TOPIC.into(),
            endpoint_override,
        })
        .unwrap()
    }

    /// Reads back the key APNs would be told signed the token, which is
    /// what distinguishes the two keys on the wire.
    fn kid_of(jwt: &str) -> String {
        jsonwebtoken::decode_header(jwt)
            .expect("provider token is a well-formed JWT")
            .kid
            .expect("APNs requires a kid header")
    }

    /// Answers one request with a canned APNs response and returns the
    /// base URL to point a provider at.
    async fn one_shot_apns(status: u16, apns_id: &str, body: &'static str) -> String {
        one_shot_apns_capturing(status, apns_id, body).await.0
    }

    /// As `one_shot_apns`, and also hands back the request headers so a
    /// test can assert on what the provider actually put on the wire.
    async fn one_shot_apns_capturing(
        status: u16,
        apns_id: &str,
        body: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let apns_id = apns_id.to_string();
        let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.split();
            let headers = drain_request(reader).await;
            headers_tx.send(headers).ok();
            let response = format!(
                "HTTP/1.1 {status} X\r\napns-id: {apns_id}\r\n\
                 content-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            use tokio::io::AsyncWriteExt;
            writer.write_all(response.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
        });
        (format!("http://{addr}"), headers_rx)
    }

    /// Reads past the request headers so the client is not answered
    /// before it has finished sending, returning what it read.
    async fn drain_request(reader: tokio::net::tcp::ReadHalf<'_>) -> Vec<String> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(reader).lines();
        let mut headers = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.is_empty() {
                break;
            }
            headers.push(line);
        }
        headers
    }

    fn message() -> PushMessage {
        PushMessage {
            token: "device-token".into(),
            title: PUSH_PLACEHOLDER_TITLE.into(),
            body: PUSH_PLACEHOLDER_BODY.into(),
            collapse_id: "session-42".into(),
            event_id: "evt-1".into(),
            data: serde_json::json!({ SEALED_PAYLOAD_KEY: "c2VhbGVk" }),
            environment: "production".into(),
            mutable_content: false,
        }
    }

    #[test]
    fn the_apns_payload_carries_alert_thread_and_the_sealed_blob() {
        let payload = apns_payload(&message());
        assert_eq!(payload["aps"]["alert"]["title"], PUSH_PLACEHOLDER_TITLE);
        assert_eq!(payload["aps"]["alert"]["body"], PUSH_PLACEHOLDER_BODY);
        assert_eq!(payload["aps"]["thread-id"], "session-42");
        assert_eq!(payload["aps"]["sound"], "default");
        assert_eq!(payload[PUSH_DATA_KEY][SEALED_PAYLOAD_KEY], "c2VhbGVk");
    }

    /// A second copy of the blob under `"body"` would roughly double
    /// the largest payload and breach `APNS_PAYLOAD_CEILING`, which
    /// `SEALED_JSON_BUDGET` is sized against assuming one copy. The
    /// blob every send now carries is a padded plaintext, so the worst
    /// case is measured through the real seal path as well.
    /// The ceiling was asserted only in tests, and the compile-time assertion
    /// beside the padding constants compares two constants rather than a
    /// payload. So an oversized send reached APNs, was rejected, and was retried
    /// across roughly forty minutes failing identically every time, because
    /// nothing about the payload changes between attempts. The user is simply
    /// never told the session needs them.
    #[test]
    fn a_payload_over_the_apns_ceiling_is_named_before_it_is_sent() {
        let fits = apns_payload(&message());
        assert_eq!(oversized_payload(&fits), None);

        let mut huge = message();
        huge.data = serde_json::json!({
            SEALED_PAYLOAD_KEY: "A".repeat(pm_protocol::gateway::APNS_PAYLOAD_CEILING + 1)
        });
        let payload = apns_payload(&huge);
        let reported = oversized_payload(&payload).expect("an oversized payload is reported");
        assert_eq!(
            reported,
            serde_json::to_vec(&payload).unwrap().len(),
            "the size reported is the size that would go on the wire"
        );
        assert!(reported > pm_protocol::gateway::APNS_PAYLOAD_CEILING);
    }

    #[test]
    fn the_sealed_blob_is_not_duplicated_in_the_payload() {
        let mut big = message();
        let blob = "A".repeat(pm_protocol::gateway::SEALED_JSON_BUDGET);
        big.data = serde_json::json!({ SEALED_PAYLOAD_KEY: blob });

        let payload = apns_payload(&big);
        assert!(
            payload.get("body").is_none(),
            "unexpected body copy in {}",
            serde_json::to_string(&payload).unwrap().len()
        );
        let encoded = serde_json::to_vec(&payload).unwrap();
        assert!(
            encoded.len() <= pm_protocol::gateway::APNS_PAYLOAD_CEILING,
            "payload of {} bytes exceeds the APNs ceiling",
            encoded.len()
        );

        let (_, public_key) = crate::hpke::generate_keypair();
        let mut worst = message();
        worst.mutable_content = true;
        worst.data = serde_json::json!({
            SEALED_PAYLOAD_KEY: sealed_blob(&public_key, &"t".repeat(5000), &"b".repeat(5000)),
        });
        let encoded = serde_json::to_vec(&apns_payload(&worst)).unwrap();
        assert!(
            encoded.len() <= pm_protocol::gateway::APNS_PAYLOAD_CEILING,
            "padded worst case of {} bytes exceeds the APNs ceiling",
            encoded.len()
        );
    }

    /// The sealed length is exactly the plaintext plus `HPKE_OVERHEAD`,
    /// so before padding the payload size tracked project name plus
    /// headline length and told an observer whether previews were on.
    #[test]
    fn the_sealed_blob_length_does_not_track_the_notification_content() {
        let (_, public_key) = crate::hpke::generate_keypair();

        let terse = sealed_blob(&public_key, "Session needs input", "");
        let verbose = sealed_blob(
            &public_key,
            "acme-secret-migration: session needs input",
            "rotating the production signing key before the audit window closes",
        );

        let lengths: Vec<usize> = [&terse, &verbose]
            .iter()
            .map(|blob| {
                let mut sealed_message = message();
                sealed_message.data = serde_json::json!({ SEALED_PAYLOAD_KEY: blob });
                apns_payload(&sealed_message)[PUSH_DATA_KEY][SEALED_PAYLOAD_KEY]
                    .as_str()
                    .unwrap()
                    .len()
            })
            .collect();

        assert_eq!(
            lengths[0], lengths[1],
            "pm_sealed lengths differ, so size still tracks content"
        );
    }

    /// The device reads named keys out of the plaintext, so the filler
    /// must survive the trip and then be ignored.
    #[test]
    fn a_padded_notification_round_trips_through_open_and_parse() {
        let (secret_key, public_key) = crate::hpke::generate_keypair();
        let blob = sealed_blob(&public_key, "acme: session needs input", "the headline");

        let opened =
            crate::hpke::open(&secret_key, &B64.decode(&blob).unwrap()).expect("the seal opens");
        assert_eq!(opened.len(), pm_protocol::gateway::SEALED_JSON_PADDED_LEN);

        let fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&opened).unwrap();
        assert!(
            !fields[pm_protocol::gateway::SEALED_PAD_KEY]
                .as_str()
                .unwrap()
                .is_empty(),
            "the filler did not reach the device"
        );

        let carried: SealedNotification = serde_json::from_slice(&opened).unwrap();
        assert_eq!(carried.title, "acme: session needs input");
        assert_eq!(carried.body, "the headline");
        assert_eq!(carried.session_id, 42);
        assert_eq!(carried.counter, 1);
    }

    /// Seals a notification the way the controller does: pad the
    /// plaintext, seal it, base64 it for the payload.
    fn sealed_blob(public_key: &[u8; 32], title: &str, body: &str) -> String {
        let notification = SealedNotification {
            title: title.into(),
            body: body.into(),
            controller_id: "ctrl-abc123".into(),
            session_id: 42,
            state: "needs-input".into(),
            event_id: "evt-1".into(),
            deep_link: "puppetmaster://controller/abc/session/42".into(),
            timestamp_unix_ms: 1700000000000,
            counter: 1,
            approval_id: None,
        };
        B64.encode(crate::hpke::seal(
            public_key,
            &notification.to_padded_json(),
        ))
    }

    /// Without `mutable-content` iOS never runs the notification
    /// service extension, and the flag must stay absent otherwise so a
    /// plain alert is not held waiting for an extension that will not
    /// change it.
    #[test]
    fn mutable_content_appears_only_when_the_message_asks_for_it() {
        let plain = apns_payload(&message());
        assert!(
            plain["aps"].get("mutable-content").is_none(),
            "unexpected mutable-content in {plain}"
        );

        let mut wants = message();
        wants.mutable_content = true;
        let sealed = apns_payload(&wants);
        assert_eq!(sealed["aps"]["mutable-content"], 1);
    }

    /// The device-gone classification used to discard the reason, which
    /// left an operator unable to tell a bad token from a bad topic
    /// from the wrong environment.
    #[tokio::test]
    async fn a_device_gone_rejection_still_reports_its_reason() {
        let endpoint = one_shot_apns(400, "APNS-ID-1", r#"{"reason":"BadDeviceToken"}"#).await;
        let (outcome, attempt) = provider(Some(endpoint.clone()))
            .send_reporting(&message())
            .await;

        assert_eq!(outcome, SendOutcome::DeviceNotRegistered);
        assert_eq!(attempt.reason.as_deref(), Some("BadDeviceToken"));
        assert_eq!(attempt.status, Some(400));
        assert_eq!(attempt.apns_id.as_deref(), Some("APNS-ID-1"));
        assert_eq!(attempt.host, endpoint);
        assert_eq!(attempt.topic, TEST_TOPIC);
        assert_eq!(attempt.environment, ENVIRONMENT_PRODUCTION);
    }

    #[tokio::test]
    async fn a_bad_topic_is_reported_apart_from_a_bad_token() {
        let endpoint = one_shot_apns(400, "APNS-ID-2", r#"{"reason":"BadTopic"}"#).await;
        let (outcome, attempt) = provider(Some(endpoint)).send_reporting(&message()).await;

        assert!(
            matches!(outcome, SendOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert_eq!(attempt.reason.as_deref(), Some("BadTopic"));
        assert_eq!(attempt.status, Some(400));
    }

    #[tokio::test]
    async fn an_unregistered_token_reports_the_gone_status() {
        let endpoint = one_shot_apns(410, "APNS-ID-3", r#"{"reason":"Unregistered"}"#).await;
        let (outcome, attempt) = provider(Some(endpoint)).send_reporting(&message()).await;

        assert_eq!(outcome, SendOutcome::DeviceNotRegistered);
        assert_eq!(attempt.status, Some(410));
        assert_eq!(attempt.reason.as_deref(), Some("Unregistered"));
    }

    #[tokio::test]
    async fn a_successful_send_reports_the_apns_id() {
        let endpoint = one_shot_apns(200, "APNS-ID-4", "").await;
        let (outcome, attempt) = provider(Some(endpoint)).send_reporting(&message()).await;

        assert_eq!(
            outcome,
            SendOutcome::Sent {
                provider_message_id: Some("APNS-ID-4".into()),
            }
        );
        assert_eq!(attempt.status, Some(200));
        assert_eq!(attempt.reason, None);
    }

    /// A send that never reaches APNs still says what it addressed.
    #[tokio::test]
    async fn an_unreachable_host_is_still_reported() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let (outcome, attempt) = provider(Some(format!("http://{addr}")))
            .send_reporting(&message())
            .await;

        assert!(
            matches!(outcome, SendOutcome::Retryable { .. }),
            "{outcome:?}"
        );
        assert_eq!(attempt.status, None);
        assert_eq!(attempt.topic, TEST_TOPIC);
        assert_eq!(attempt.environment, ENVIRONMENT_PRODUCTION);
    }

    /// Host and environment are logged together so a token minted in one
    /// environment and sent to the other is diagnosable.
    #[test]
    fn the_host_follows_the_message_environment() {
        let provider = provider(None);
        assert_eq!(
            provider.endpoint(ENVIRONMENT_PRODUCTION),
            APNS_PRODUCTION_ENDPOINT
        );
        assert_eq!(
            provider.endpoint(ENVIRONMENT_SANDBOX),
            APNS_SANDBOX_ENDPOINT
        );
    }

    fn sandbox_message() -> PushMessage {
        PushMessage {
            environment: ENVIRONMENT_SANDBOX.into(),
            ..message()
        }
    }

    /// The symptom that prompted the second key slot: a .p8 scoped to
    /// one environment is answered with `BadEnvironmentKeyInToken` in
    /// the other, so the key has to follow the message.
    #[test]
    fn a_configured_sandbox_key_signs_only_sandbox_sends() {
        let provider = provider_with_sandbox_key(None);
        let production = provider.bearer_jwt(ENVIRONMENT_PRODUCTION).unwrap();
        let sandbox = provider.bearer_jwt(ENVIRONMENT_SANDBOX).unwrap();

        assert_eq!(kid_of(&production), TEST_KEY_ID);
        assert_eq!(kid_of(&sandbox), TEST_SANDBOX_KEY_ID);
    }

    /// An operator holding one key scoped to both environments must not
    /// be made to configure a second one, so without a sandbox key the
    /// single key signs both exactly as it did before.
    #[test]
    fn without_a_sandbox_key_one_key_signs_both_environments() {
        let provider = provider(None);
        let production = provider.bearer_jwt(ENVIRONMENT_PRODUCTION).unwrap();
        let sandbox = provider.bearer_jwt(ENVIRONMENT_SANDBOX).unwrap();

        assert_eq!(kid_of(&production), TEST_KEY_ID);
        assert_eq!(kid_of(&sandbox), TEST_KEY_ID);
        assert_eq!(
            production, sandbox,
            "both environments should share the one cached token"
        );
    }

    /// A single shared cache would serve whichever environment sent
    /// first to both afterwards, reproducing the original bug only
    /// after the first send. Each key caches its own token instead.
    #[test]
    fn the_jwt_cache_cannot_leak_a_token_across_environments() {
        let production_first = provider_with_sandbox_key(None);
        assert_eq!(
            kid_of(&production_first.bearer_jwt(ENVIRONMENT_PRODUCTION).unwrap()),
            TEST_KEY_ID
        );
        assert_eq!(
            kid_of(&production_first.bearer_jwt(ENVIRONMENT_SANDBOX).unwrap()),
            TEST_SANDBOX_KEY_ID
        );

        let sandbox_first = provider_with_sandbox_key(None);
        assert_eq!(
            kid_of(&sandbox_first.bearer_jwt(ENVIRONMENT_SANDBOX).unwrap()),
            TEST_SANDBOX_KEY_ID
        );
        assert_eq!(
            kid_of(&sandbox_first.bearer_jwt(ENVIRONMENT_PRODUCTION).unwrap()),
            TEST_KEY_ID
        );
    }

    /// Splitting the cache must not stop it caching: a second send in
    /// the same environment reuses the minted token.
    #[test]
    fn each_environment_reuses_its_own_cached_token() {
        let provider = provider_with_sandbox_key(None);
        let first = provider.bearer_jwt(ENVIRONMENT_SANDBOX).unwrap();
        let second = provider.bearer_jwt(ENVIRONMENT_SANDBOX).unwrap();
        assert_eq!(first, second);
        assert_ne!(first, provider.bearer_jwt(ENVIRONMENT_PRODUCTION).unwrap());
    }

    /// An override exists to point sends at a local host, and must not
    /// drag the key choice along with it.
    #[test]
    fn an_endpoint_override_pins_the_host_but_not_the_key() {
        let provider = provider_with_sandbox_key(Some("http://127.0.0.1:9/".into()));
        assert_eq!(
            provider.endpoint(ENVIRONMENT_PRODUCTION),
            "http://127.0.0.1:9"
        );
        assert_eq!(provider.endpoint(ENVIRONMENT_SANDBOX), "http://127.0.0.1:9");

        assert_eq!(
            kid_of(&provider.bearer_jwt(ENVIRONMENT_SANDBOX).unwrap()),
            TEST_SANDBOX_KEY_ID
        );
        assert_eq!(
            kid_of(&provider.bearer_jwt(ENVIRONMENT_PRODUCTION).unwrap()),
            TEST_KEY_ID
        );
    }

    /// What APNs is told, rather than what the provider intended.
    #[tokio::test]
    async fn the_sandbox_key_signs_the_request_apns_receives() {
        let (endpoint, headers) = one_shot_apns_capturing(200, "APNS-ID-5", "").await;
        let (outcome, _) = provider_with_sandbox_key(Some(endpoint))
            .send_reporting(&sandbox_message())
            .await;
        assert!(matches!(outcome, SendOutcome::Sent { .. }), "{outcome:?}");

        let headers = headers.await.unwrap();
        let jwt = headers
            .iter()
            .filter_map(|h| h.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.trim())
            .expect("APNs send carries an authorization header")
            .strip_prefix("bearer ")
            .expect("APNs auth is a bearer token");
        assert_eq!(kid_of(jwt), TEST_SANDBOX_KEY_ID);
    }

    #[test]
    fn a_sandbox_key_needs_its_key_id() {
        let err = ApnsProvider::new(ApnsConfig {
            key_p8: TEST_SIGNING_KEY.into(),
            key_id: TEST_KEY_ID.into(),
            sandbox_key: Some(ApnsKey {
                key_p8: TEST_SANDBOX_SIGNING_KEY.into(),
                key_id: String::new(),
            }),
            team_id: "TESTTEAM".into(),
            topic: TEST_TOPIC.into(),
            endpoint_override: None,
        })
        .err()
        .expect("a sandbox key without an id is rejected");
        assert!(err.contains("sandbox signing key needs a key id"), "{err}");
    }

    /// A rejected sandbox key must not read as a problem with the
    /// primary one.
    #[test]
    fn an_unusable_sandbox_key_says_which_key_it_means() {
        let err = ApnsProvider::new(ApnsConfig {
            key_p8: TEST_SIGNING_KEY.into(),
            key_id: TEST_KEY_ID.into(),
            sandbox_key: Some(ApnsKey {
                key_p8: "not a pem".into(),
                key_id: TEST_SANDBOX_KEY_ID.into(),
            }),
            team_id: "TESTTEAM".into(),
            topic: TEST_TOPIC.into(),
            endpoint_override: None,
        })
        .err()
        .expect("an unparsable sandbox key is rejected");
        assert_eq!(err, "APNs sandbox signing key is not a valid EC PEM key");
    }

    #[test]
    fn absent_response_fields_render_as_a_placeholder() {
        let attempt = ApnsAttempt::default();
        assert_eq!(attempt.status_field(), MISSING_FIELD);
        assert_eq!(attempt.apns_id_field(), MISSING_FIELD);
        assert_eq!(attempt.reason_field(), MISSING_FIELD);

        let answered = ApnsAttempt {
            status: Some(400),
            apns_id: Some("id".into()),
            reason: Some("BadTopic".into()),
            ..ApnsAttempt::default()
        };
        assert_eq!(answered.status_field(), "400");
        assert_eq!(answered.apns_id_field(), "id");
        assert_eq!(answered.reason_field(), "BadTopic");
    }

    #[derive(Clone, Default)]
    struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl LogCapture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for LogCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for LogCapture {
        type Writer = Self;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    /// Sends one message at a local fake APNs endpoint with this
    /// crate's debug logs captured, returning what an operator running
    /// with `-v` would see. The subscriber is scoped to the calling
    /// thread, which is why the send runs on a current-thread runtime.
    /// It filters by level rather than with `EnvFilter`, which captures
    /// nothing when two tests hold scoped subscribers at once.
    fn send_capturing_logs(message: &PushMessage) -> (ApnsAttempt, String) {
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let attempt = tracing::subscriber::with_default(subscriber, || {
            rt.block_on(async {
                let endpoint = one_shot_apns(200, "APNS-ID-AUDIT", "").await;
                provider(Some(endpoint)).send_reporting(message).await.1
            })
        });
        (attempt, capture.text())
    }

    /// The audit line stands in for reading the wire, so it has to
    /// carry the exact payload sent and enough about the exchange to
    /// place it without a second line.
    #[test]
    fn the_send_audit_line_carries_the_payload_and_what_addressed_it() {
        let message = message();
        let (attempt, logs) = send_capturing_logs(&message);

        let payload = serde_json::to_string(&apns_payload(&message)).unwrap();
        assert!(logs.contains(&payload), "payload missing from {logs}");
        assert!(
            logs.contains(&message.token),
            "device token missing from {logs}"
        );
        assert!(logs.contains(&attempt.host), "host missing from {logs}");
        assert!(logs.contains(TEST_TOPIC), "topic missing from {logs}");
        assert!(
            logs.contains(ENVIRONMENT_PRODUCTION),
            "environment missing from {logs}"
        );
        assert!(
            logs.contains(&message.collapse_id),
            "collapse id missing from {logs}"
        );
    }

    /// The line is only safe to emit because the payload is redacted.
    /// A sealed send's content lives in the blob, so the audit shows
    /// the placeholder alert and never the session's own words.
    #[test]
    fn a_sealed_sends_audit_line_reveals_neither_project_nor_headline() {
        const PROJECT_NAME: &str = "acme-secret-migration";
        const HEADLINE: &str = "rotating the production signing key";

        let (secret_key, public_key) = crate::hpke::generate_keypair();
        let notification = SealedNotification {
            title: format!("{PROJECT_NAME}: session needs input"),
            body: HEADLINE.into(),
            controller_id: "ctrl-opaque".into(),
            session_id: 42,
            state: "needs-input".into(),
            event_id: "evt-1".into(),
            deep_link: "puppetmaster://controller/abc/session/42".into(),
            timestamp_unix_ms: 1000,
            counter: 1,
            approval_id: None,
        };
        let sealed = crate::hpke::seal(&public_key, &notification.to_padded_json());

        let mut message = message();
        message.mutable_content = true;
        message.data = serde_json::json!({ SEALED_PAYLOAD_KEY: B64.encode(&sealed) });

        let (_, logs) = send_capturing_logs(&message);

        for secret in [PROJECT_NAME, HEADLINE] {
            assert!(
                !logs.contains(secret),
                "{secret:?} reached the audit line: {logs}"
            );
        }
        assert!(
            logs.contains(PUSH_PLACEHOLDER_TITLE),
            "the placeholder alert is what the line should show: {logs}"
        );

        // Absence above is redaction only if the seal really held them.
        let opened = crate::hpke::open(&secret_key, &sealed).expect("the seal opens");
        let carried: SealedNotification = serde_json::from_slice(&opened).unwrap();
        assert!(carried.title.contains(PROJECT_NAME), "{}", carried.title);
        assert_eq!(carried.body, HEADLINE);
    }
}
