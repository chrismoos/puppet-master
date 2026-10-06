//! Push provider that relays through a push gateway.
//!
//! Slots into the existing `PushProvider` trait beside APNS and FCM,
//! inheriting the delivery queue, retry backoff and 410 handling
//! unchanged.
//!
//! The daemon is the single seal site: it calls seal_notification for
//! all providers including gateway, and the sealed blob arrives in
//! message.data["pm_sealed"]. This provider never seals — it reads
//! the pre-sealed payload and relays it.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use pm_protocol::gateway::{
    GatewayPushRequest, GatewayPushResponse, GatewayPushStatus, PROVIDER_GATEWAY,
    SEALED_PAYLOAD_KEY,
};
use tracing::debug;

use crate::{PushMessage, PushProvider, SendOutcome};

const GATEWAY_HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Configuration for the gateway provider.
pub struct GatewayConfig {
    /// The gateway's base URL (e.g. `https://pushgw.puppet-master.xyz`).
    /// The provider appends `/v1/push`.
    pub endpoint: String,
}

pub struct GatewayProvider {
    endpoint: String,
    client: reqwest::Client,
}

impl GatewayProvider {
    pub fn new(config: GatewayConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(GATEWAY_HTTP_TIMEOUT)
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self {
            endpoint: config.endpoint.trim_end_matches('/').to_string(),
            client,
        })
    }

    /// Builds a push request for relay through the gateway. The sealed
    /// payload is read from the message data — the daemon is the
    /// single seal site.
    fn prepare_request(&self, message: &PushMessage) -> Result<GatewayPushRequest, String> {
        let sealed_b64 = message
            .data
            .get(SEALED_PAYLOAD_KEY)
            .and_then(|v| v.as_str())
            .ok_or_else(|| "no pm_sealed in push data".to_string())?;
        B64.decode(sealed_b64)
            .map_err(|e| format!("pm_sealed is not valid base64: {e}"))?;

        Ok(GatewayPushRequest {
            device_token: message.token.clone(),
            sealed_payload: sealed_b64.to_string(),
            environment: message.environment.clone(),
            collapse_id: message.collapse_id.clone(),
        })
    }
}

#[async_trait::async_trait]
impl PushProvider for GatewayProvider {
    fn name(&self) -> &'static str {
        PROVIDER_GATEWAY
    }

    async fn send(&self, message: &PushMessage) -> SendOutcome {
        let request = match self.prepare_request(message) {
            Ok(r) => r,
            Err(detail) => {
                return SendOutcome::Rejected { detail };
            }
        };

        let url = format!("{}/v1/push", self.endpoint);
        let response = match self.client.post(&url).json(&request).send().await {
            Ok(resp) => resp,
            Err(e) => {
                return SendOutcome::Retryable {
                    detail: format!("gateway request failed: {e}"),
                }
            }
        };

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        debug!(
            gateway_status = %status,
            body_len = body.len(),
            "gateway push response"
        );

        let parsed: GatewayPushResponse = match serde_json::from_str(&body) {
            Ok(p) => p,
            Err(_) => {
                if status.is_server_error() || status == 429 {
                    return SendOutcome::Retryable {
                        detail: format!("gateway {status}: {body}"),
                    };
                }
                return SendOutcome::Rejected {
                    detail: format!("gateway {status}: unparseable response"),
                };
            }
        };

        match parsed.status {
            GatewayPushStatus::Accepted => SendOutcome::Sent {
                provider_message_id: None,
            },
            GatewayPushStatus::DeviceGone => SendOutcome::DeviceNotRegistered,
            GatewayPushStatus::RateLimited => SendOutcome::Retryable {
                detail: parsed.detail.unwrap_or_else(|| "rate limited".into()),
            },
            GatewayPushStatus::Rejected => SendOutcome::Rejected {
                detail: parsed.detail.unwrap_or_else(|| "rejected".into()),
            },
        }
    }

    async fn receipt(&self, _provider_message_id: &str) -> crate::ReceiptOutcome {
        // The gateway settles synchronously in the send response.
        crate::ReceiptOutcome::Final
    }

    async fn invalidate_device(&self, _token: &str) {
        // No cleanup needed at the gateway; the DeviceGone response
        // handles device removal synchronously.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_request_reads_pre_sealed_data() {
        let config = GatewayConfig {
            endpoint: "https://example.com".into(),
        };
        let provider = GatewayProvider::new(config).unwrap();

        let sealed = B64.encode(b"pre-sealed-by-daemon");
        let message = PushMessage {
            token: "a".repeat(64),
            title: "Session needs input".into(),
            body: "Worker session 42 is waiting".into(),
            collapse_id: "session-42".into(),
            event_id: "evt-1".into(),
            data: serde_json::json!({
                "pm_sealed": sealed,
            }),
            environment: "production".into(),
            mutable_content: false,
        };

        let request = provider.prepare_request(&message).unwrap();
        assert_eq!(request.device_token, "a".repeat(64));
        assert_eq!(request.sealed_payload, sealed);
        assert_eq!(request.environment, "production");
        assert_eq!(request.collapse_id, "session-42");
    }

    #[test]
    fn prepare_request_fails_without_pm_sealed() {
        let config = GatewayConfig {
            endpoint: "https://example.com".into(),
        };
        let provider = GatewayProvider::new(config).unwrap();

        let message = PushMessage {
            token: "a".repeat(64),
            title: "t".into(),
            body: "b".into(),
            collapse_id: "c".into(),
            event_id: "e".into(),
            data: serde_json::json!({}),
            environment: "production".into(),
            mutable_content: false,
        };

        assert!(provider.prepare_request(&message).is_err());
    }
}
