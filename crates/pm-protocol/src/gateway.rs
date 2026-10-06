//! Wire types for the push gateway relay. Shared by the controller
//! (pm-daemon) and the relay (pm-pushgw) so the two ends cannot drift.
//!
//! The gateway is stateless about devices: it holds no registrations
//! and no device tokens. The controller sends the raw APNs device
//! token with each push, matching the design of Matrix Sygnal and
//! Home Assistant's push relay.

use serde::{Deserialize, Serialize};

// ── Byte budget ──────────────────────────────────────────────────────

/// APNs hard ceiling per payload.
pub const APNS_PAYLOAD_CEILING: usize = 4096;

/// Conservative overhead for the outer APNs JSON envelope (aps dict,
/// mutable-content, thread-id, base64 key overhead). Real overhead
/// depends on title/collapse-id lengths, so this leaves margin.
pub const APNS_OUTER_OVERHEAD: usize = 200;

/// X25519 encapsulated key (32) + AES-GCM-128 tag (16).
pub const HPKE_OVERHEAD: usize = 48;

/// Maximum plaintext the sealed JSON may occupy so the final APNs
/// payload stays within `APNS_PAYLOAD_CEILING`.
///
///   4096 − 200 (outer) = 3896 base64 chars
///   3896 × 3/4 ≈ 2922 raw bytes
///   2922 − 48 (HPKE overhead) = 2874 plaintext
///
/// Rounded down for safety. `SEALED_JSON_PADDED_LEN` is what the
/// controller actually emits and must stay at or under this.
pub const SEALED_JSON_BUDGET: usize = 2850;

/// The one length every sealed plaintext is serialized to, so the
/// ciphertext's size says nothing about the title or body it carries.
/// Without it the sealed length is exactly plaintext + `HPKE_OVERHEAD`
/// and an observer can estimate headline length from the payload alone.
///
/// A quarter of the APNs ceiling sits far inside `SEALED_JSON_BUDGET`
/// while still leaving room for a long headline, so a fixed size costs
/// nothing a bucketed one would save.
pub const SEALED_JSON_PADDED_LEN: usize = APNS_PAYLOAD_CEILING / 4;

const _: () = assert!(SEALED_JSON_PADDED_LEN <= SEALED_JSON_BUDGET);

/// Key of the filler field that pads the sealed JSON. The notification
/// service extension parses the plaintext and reads named keys, so it
/// ignores this one and needs no knowledge of the padding.
pub const SEALED_PAD_KEY: &str = "pad";

/// Filler character, chosen because JSON encodes it as itself.
const SEALED_PAD_CHAR: char = 'A';

/// Appended to content this shortened, so a truncated alert reads as one.
const ELLIPSIS: char = '\u{2026}';

/// Domain label passed as the HPKE `info` parameter for domain
/// separation, ensuring keys derived for push cannot be confused
/// with keys from any other subsystem.
pub const HPKE_DOMAIN: &[u8] = b"pm-push-hpke-v1";

/// Provider name registered at the push endpoint and used for
/// provider selection in the delivery queue.
pub const PROVIDER_GATEWAY: &str = "gateway";

/// What the lock screen shows for every push that leaves the
/// controller. The real title and body live inside the seal, so this
/// is all that reaches Apple and all a device that cannot decrypt
/// will ever display.
pub const PUSH_PLACEHOLDER_TITLE: &str = "Puppet Master";
pub const PUSH_PLACEHOLDER_BODY: &str = "New notification";

/// Key of the cleartext data dict in the APNs payload, agreed
/// between the payload builder, the notification service extension
/// and the app's tap router.
pub const PUSH_DATA_KEY: &str = "pm";

/// The only key inside that dict. The notification service extension
/// needs it to find the blob, and nothing else in the dict would
/// survive the trip to Apple unread.
pub const SEALED_PAYLOAD_KEY: &str = "pm_sealed";

// ── Sealed notification (inner JSON, encrypted for the device) ──────

/// The cleartext JSON sealed inside the HPKE ciphertext. The device's
/// Notification Service Extension decrypts this and rewrites the
/// alert. Fields are deliberately flat strings/numbers so the
/// extension does not need a JSON object parser beyond key lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedNotification {
    pub title: String,
    pub body: String,
    pub controller_id: String,
    pub session_id: u64,
    pub state: String,
    pub event_id: String,
    pub deep_link: String,
    pub timestamp_unix_ms: i64,
    /// Monotonic per device, so the extension can reject replays.
    pub counter: u64,
    /// The call a connection-approval notification asks about; older apps ignore it and route by session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
}

impl SealedNotification {
    /// Serializes to exactly `SEALED_JSON_PADDED_LEN` bytes: content
    /// that would not fit is truncated, then a filler field pads the
    /// JSON out so every notification seals to the same length. Call
    /// this *before* sealing.
    pub fn to_padded_json(&self) -> Vec<u8> {
        let padded = pad_to_fixed_len(self.to_truncated_json());
        debug_assert_eq!(
            padded.len(),
            SEALED_JSON_PADDED_LEN,
            "a notification that is not a fixed length tells an observer how long its \
             content is, which is the whole point of padding it"
        );
        padded
    }

    /// Shrinks content until the record, with an empty filler field, fits
    /// inside the fixed length.
    ///
    /// Everything here is measured against the *serialized* length rather
    /// than the raw one, and that is the point. `serde_json` writes a control
    /// character as `\u00XX`, six bytes for one, so no fixed margin bounds how
    /// far a field can expand on the way out. Sizing against raw bytes let a
    /// record overshoot, the padding gave up silently, and the length went back
    /// to tracking the content.
    fn to_truncated_json(&self) -> Vec<u8> {
        let mut trimmed = self.clone();
        if !fits_with_filler(&trimmed) {
            trimmed.body = longest_fitting(&trimmed, &self.body, |n, v| n.body = v);
        }
        if !fits_with_filler(&trimmed) {
            trimmed.body = String::new();
            trimmed.title = longest_fitting(&trimmed, &self.title, |n, v| n.title = v);
        }
        if !fits_with_filler(&trimmed) {
            // Nothing the content can do. An empty alert is still a fixed
            // length, where an oversized one is a measurement of its own body.
            trimmed.title = String::new();
            trimmed.body = String::new();
        }
        serde_json::to_vec(&trimmed).unwrap_or_default()
    }
}

/// The length this record serializes to once the filler field is present,
/// which is what the padding actually has to fit inside. Measuring the bare
/// record instead is what let the two disagree.
fn len_with_filler(notif: &SealedNotification) -> usize {
    let Ok(serde_json::Value::Object(mut fields)) = serde_json::to_value(notif) else {
        return usize::MAX;
    };
    fields.insert(SEALED_PAD_KEY.to_string(), String::new().into());
    serde_json::to_vec(&fields)
        .map(|json| json.len())
        .unwrap_or(usize::MAX)
}

fn fits_with_filler(notif: &SealedNotification) -> bool {
    len_with_filler(notif) <= SEALED_JSON_PADDED_LEN
}

/// The longest prefix of `value`, counted in characters, that leaves the whole
/// record inside the fixed length.
///
/// Found by halving rather than by arithmetic, because a character serializes
/// to anywhere between one and six bytes and the only reliable measure of a
/// candidate is to serialize it.
fn longest_fitting(
    notif: &SealedNotification,
    value: &str,
    set: fn(&mut SealedNotification, String),
) -> String {
    let chars: Vec<char> = value.chars().collect();
    let candidate = |count: usize| -> String {
        let mut out: String = chars[..count].iter().collect();
        if count < chars.len() {
            out.push(ELLIPSIS);
        }
        out
    };
    let mut best = String::new();
    let (mut low, mut high) = (0usize, chars.len());
    while low <= high {
        let mid = low + (high - low) / 2;
        let probe_value = candidate(mid);
        let mut probe = notif.clone();
        set(&mut probe, probe_value.clone());
        if fits_with_filler(&probe) {
            best = probe_value;
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }
    best
}

/// Adds a filler field to `json` so the result is exactly
/// `SEALED_JSON_PADDED_LEN` bytes.
///
/// The truncation above guarantees the input fits, so the branches that return
/// the input untouched are unreachable from [`SealedNotification::to_padded_json`]
/// and a debug assertion there holds them to it. They remain because this takes
/// arbitrary JSON.
fn pad_to_fixed_len(json: Vec<u8>) -> Vec<u8> {
    let Ok(serde_json::Value::Object(mut fields)) =
        serde_json::from_slice::<serde_json::Value>(&json)
    else {
        return json;
    };
    fields.insert(SEALED_PAD_KEY.to_string(), String::new().into());
    let Ok(empty_filler) = serde_json::to_vec(&fields) else {
        return json;
    };
    let Some(filler_len) = SEALED_JSON_PADDED_LEN.checked_sub(empty_filler.len()) else {
        return json;
    };
    let filler = SEALED_PAD_CHAR.to_string().repeat(filler_len);
    fields.insert(SEALED_PAD_KEY.to_string(), filler.into());
    serde_json::to_vec(&fields).unwrap_or(json)
}

// ── Gateway push request (controller → gateway, HTTP POST) ──────────

/// A push request from a controller to the gateway relay. The relay
/// forwards the sealed payload to APNs at the given device token; it
/// holds no per-controller state and authenticates nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayPushRequest {
    /// Raw APNs device token (hex string).
    pub device_token: String,
    /// Base64-encoded HPKE ciphertext (enc ‖ ciphertext ‖ tag).
    pub sealed_payload: String,
    /// `production` or `sandbox`; selects the APNs endpoint.
    pub environment: String,
    /// APNs collapse id for notification stacking.
    pub collapse_id: String,
}

// ── Gateway push response ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayPushResponse {
    pub status: GatewayPushStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayPushStatus {
    /// Accepted and relayed to APNs.
    Accepted,
    /// The device token is gone (APNs 410 or 400 BadDeviceToken).
    /// The controller should disable the endpoint.
    DeviceGone,
    /// Too many requests for this token or source.
    RateLimited,
    /// Permanent rejection (bad token format, bad payload, etc.).
    Rejected,
}

// ── Device token validation ─────────────────────────────────────────

/// APNs device tokens are hex-encoded and typically 64 characters
/// (32 bytes). Allow some headroom for future changes.
const TOKEN_MIN_HEX_LEN: usize = 32;
const TOKEN_MAX_HEX_LEN: usize = 200;

/// Returns true if the token looks like a valid APNs hex token.
/// This is a format gate, not a validity check — only APNs can say
/// whether a token is real.
pub fn is_valid_device_token(token: &str) -> bool {
    let len = token.len();
    (TOKEN_MIN_HEX_LEN..=TOKEN_MAX_HEX_LEN).contains(&len)
        && token.bytes().all(|b| b.is_ascii_hexdigit())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(title: &str, body: &str) -> SealedNotification {
        SealedNotification {
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
        }
    }

    #[test]
    fn sealed_notification_truncates_body_to_budget() {
        let notif = notification("Session needs input", &"x".repeat(5000));
        let json = notif.to_padded_json();
        assert!(
            json.len() <= SEALED_JSON_BUDGET,
            "padded JSON is {} bytes, budget is {}",
            json.len(),
            SEALED_JSON_BUDGET
        );
        let parsed: SealedNotification = serde_json::from_slice(&json).unwrap();
        assert!(parsed.body.ends_with('…'), "body: {}", parsed.body);
        assert_eq!(parsed.title, "Session needs input");
    }

    /// Truncation spends body and title before the routing id, and approvals pad to the same length.
    #[test]
    fn an_approval_keeps_its_id_through_truncation_at_the_fixed_length() {
        let id = "a".repeat(64);
        let mut notif = notification(&"t".repeat(5000), &"x".repeat(5000));
        notif.state = "connection-approval".into();
        notif.event_id = format!("connection-approval:{id}");
        notif.approval_id = Some(id.clone());
        let json = notif.to_padded_json();
        assert_eq!(json.len(), SEALED_JSON_PADDED_LEN);
        let parsed: SealedNotification = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed.approval_id.as_deref(), Some(id.as_str()));
        assert_eq!(parsed.session_id, 42);
    }

    /// Records sealed before approvals still open, and others gain no empty key.
    #[test]
    fn approval_id_is_optional_in_both_directions() {
        let legacy = br#"{"title":"t","body":"b","controller_id":"c","session_id":7,
            "state":"needs-input","event_id":"e","deep_link":"puppetmaster://c/session/7",
            "timestamp_unix_ms":1,"counter":2}"#;
        let parsed: SealedNotification = serde_json::from_slice(legacy).unwrap();
        assert_eq!(parsed.approval_id, None);
        let written = String::from_utf8(notification("t", "b").to_padded_json()).unwrap();
        assert!(!written.contains("approval_id"), "{written}");
    }

    #[test]
    fn short_notification_is_not_truncated() {
        let notif = notification("t", "short");
        let json = notif.to_padded_json();
        let parsed: SealedNotification = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed.body, "short");
    }

    /// The leak this padding closes: the sealed blob is exactly the
    /// plaintext plus `HPKE_OVERHEAD`, so a plaintext whose length
    /// tracks project name plus headline length hands an observer an
    /// estimate of both from the payload size alone.
    #[test]
    fn content_length_does_not_change_the_padded_length() {
        let terse = notification("Puppet Master", "");
        let verbose = notification(
            "acme-secret-migration: session needs input",
            "rotating the production signing key before the audit window closes",
        );
        let truncated = notification("acme-secret-migration", &"x".repeat(5000));

        let lengths: Vec<usize> = [&terse, &verbose, &truncated]
            .iter()
            .map(|n| n.to_padded_json().len())
            .collect();

        assert_eq!(
            lengths,
            vec![SEALED_JSON_PADDED_LEN; lengths.len()],
            "padded lengths differ, so size still tracks content"
        );
    }

    /// Truncation only ever trimmed the body, so a title long enough to
    /// fill the budget on its own would leave the JSON unpaddable.
    #[test]
    fn an_oversized_title_is_also_trimmed_to_the_padded_length() {
        let notif = notification(&"t".repeat(5000), "body");
        let json = notif.to_padded_json();
        assert_eq!(json.len(), SEALED_JSON_PADDED_LEN);
        let parsed: SealedNotification = serde_json::from_slice(&json).unwrap();
        assert!(parsed.title.ends_with('…'), "title: {}", parsed.title);
    }

    /// The existing cases all used characters that serialize to themselves, so
    /// they could not see this: `serde_json` writes a control character as
    /// `\u00XX`, six bytes for one, and the truncation used to size fields in
    /// raw bytes with a flat twenty-byte margin. A record could overshoot, the
    /// padding gave up without saying so, and the length went back to tracking
    /// the content at six bytes a character — legibly, linearly, which is
    /// exactly the leak the padding exists to close.
    ///
    /// The content author is the agent, and the body cap is in characters and
    /// applied after entity decoding, so 180 of these is reachable.
    #[test]
    fn escaping_does_not_change_the_padded_length() {
        let cases = [
            (
                "150 control characters",
                notification("t", &"\u{1}".repeat(150)),
            ),
            (
                "180 control characters",
                notification("t", &"\u{1}".repeat(180)),
            ),
            (
                "a control-character title",
                notification(&"\u{1}".repeat(5000), "b"),
            ),
            ("a quoted title", notification(&"\"".repeat(1000), "b")),
            (
                "quotes and backslashes",
                notification("t", &"\"\\".repeat(180)),
            ),
            ("astral plane", notification("t", &"\u{1f600}".repeat(180))),
            (
                "a lone surrogate's worth of escapes",
                notification("t", &"\u{7}\u{8}\u{b}\u{c}".repeat(90)),
            ),
        ];
        for (label, notif) in cases {
            assert_eq!(
                notif.to_padded_json().len(),
                SEALED_JSON_PADDED_LEN,
                "{label} escaped to a different length, so its size is a measurement of it"
            );
        }
    }

    /// What a fixed length is worth: an observer watching sizes learns nothing,
    /// however the content is chosen. Three records that serialize to wildly
    /// different raw lengths have to come out identical.
    #[test]
    fn an_observer_cannot_tell_these_apart_by_size() {
        let lengths: Vec<usize> = [
            notification("needs input", ""),
            notification("t", &"\u{1}".repeat(180)),
            notification(&"\u{1}".repeat(5000), &"\u{1}".repeat(5000)),
            notification("acme: session needs input", "rotating the signing key"),
        ]
        .iter()
        .map(|n| n.to_padded_json().len())
        .collect();
        assert_eq!(lengths, vec![SEALED_JSON_PADDED_LEN; 4]);
    }

    /// Content that has to be shortened still arrives as something a person can
    /// read, rather than being dropped on the way to a fixed length.
    #[test]
    fn a_shortened_body_still_carries_what_fits() {
        let notif = notification("Session needs input", &"\u{1}".repeat(180));
        let parsed: SealedNotification = serde_json::from_slice(&notif.to_padded_json()).unwrap();
        assert_eq!(parsed.title, "Session needs input");
        assert!(
            parsed.body.ends_with(ELLIPSIS),
            "a shortened body says it was shortened: {:?}",
            parsed.body
        );
        assert!(!parsed.body.is_empty());
    }

    /// The filler is an extra JSON key, and every reader of the
    /// plaintext -- the notification service extension included --
    /// looks fields up by name.
    #[test]
    fn the_filler_is_a_named_field_the_parse_ignores() {
        let json = notification("Session needs input", "headline").to_padded_json();
        let fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&json).unwrap();
        assert!(!fields[SEALED_PAD_KEY].as_str().unwrap().is_empty());

        let parsed: SealedNotification = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed.title, "Session needs input");
        assert_eq!(parsed.body, "headline");
    }

    #[test]
    fn token_validation() {
        // Valid hex tokens
        assert!(is_valid_device_token(&"a".repeat(64)));
        assert!(is_valid_device_token(&"0123456789abcdefABCDEF".repeat(3)));
        // Too short
        assert!(!is_valid_device_token("abcdef"));
        // Non-hex
        assert!(!is_valid_device_token(&"g".repeat(64)));
        // Path traversal attempt
        assert!(!is_valid_device_token("../../../etc/passwd"));
        // Empty
        assert!(!is_valid_device_token(""));
    }

    /// Shortening counts characters rather than bytes, so it cannot land in the
    /// middle of one and hand the extension something that is not UTF-8.
    #[test]
    fn shortening_never_splits_a_character() {
        for filler in ["héllo wörld ", "🙂", "\u{1}", "日本語テキスト"] {
            let notif = notification("t", &filler.repeat(4000));
            let json = notif.to_padded_json();
            let parsed: SealedNotification =
                serde_json::from_slice(&json).expect("the shortened body is still valid JSON");
            assert!(parsed.body.ends_with(ELLIPSIS), "{:?}", parsed.body);
            // A split character would have made the body invalid UTF-8 before
            // it ever reached here, so reaching here with the same characters
            // back is the assertion.
            assert!(parsed.body.chars().count() > 0);
        }
    }

    #[test]
    fn gateway_response_round_trips() {
        let resp = GatewayPushResponse {
            status: GatewayPushStatus::DeviceGone,
            detail: Some("410 from APNs".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: GatewayPushResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, GatewayPushStatus::DeviceGone);
    }
}
