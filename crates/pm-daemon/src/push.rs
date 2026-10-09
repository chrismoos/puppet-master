//! Daemon-hosted push notifications. Events derive from committed
//! session state transitions inside `Daemon::publish`, dedupe on a
//! durable event id, queue in `notification_deliveries`, and drain
//! through a provider adapter with retry/backoff. Payloads never carry
//! terminal text, prompts, item bodies, filesystem paths, or secrets;
//! with previews off they carry only opaque ids and a generic title.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pm_protocol::domain::{
    ProgramStatusState, Session, SessionAlert, SessionAlertKind, SessionRole, SessionState,
};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::daemon::{now_unix_ms, Daemon, DaemonError};
use crate::mobile::MobileAuthError;
use crate::secrets::{open_secret, seal_secret};
use crate::storage::{
    NotificationDelivery, PushEndpoint, PushPolicy, DELIVERY_FAILED, DELIVERY_PENDING,
    DELIVERY_SUPPRESSED, DELIVERY_UNREGISTERED,
};
use pm_protocol::gateway::{
    SealedNotification, PROVIDER_GATEWAY, PUSH_PLACEHOLDER_BODY, PUSH_PLACEHOLDER_TITLE,
    SEALED_PAYLOAD_KEY,
};
use pm_push::{
    gateway::{GatewayConfig, GatewayProvider},
    PushMessage, PushProvider, SendOutcome, ENVIRONMENT_PRODUCTION, ENVIRONMENT_SANDBOX,
};

/// Settings key: which event classes notify, as a comma-separated
/// subset of needs-input, failed, completed. Empty disables all.
pub const SETTING_PUSH_EVENTS: &str = "push.events";
pub const PUSH_EVENTS_DEFAULT: &str = "needs-input,failed,completed";

/// Settings key: which sessions notify. `default` is supervisors plus
/// workers with no supervising session; also `all`, `supervisors`,
/// `none`. Per-bucket/role rows in push_policies override it.
pub const SETTING_PUSH_SCOPE: &str = "push.scope";
pub const PUSH_SCOPE_DEFAULT: &str = "default";

/// Settings key: hours settled delivery rows are kept as the dedupe
/// window before receipt cleanup deletes them.
pub const SETTING_PUSH_DEDUPE_WINDOW_HOURS: &str = "push.dedupe_window_hours";
pub const PUSH_DEDUPE_WINDOW_HOURS_DEFAULT: i64 = 48;

pub const SETTING_PUSH_GATEWAY_URL: &str = "push.gateway.url";
/// The relay a controller reaches when the operator has not named one,
/// so a device that enrols and registers is delivered to without any
/// further setup. Clearing the setting to an empty value is what turns
/// push off; unsetting it restores this.
pub const PUSH_GATEWAY_URL_DEFAULT: &str = "https://pushgw.puppet-master.xyz";

/// User setting: how long after the last interaction with the web UI
/// that user's devices stay silent. Stored per user in `user_settings`
/// as a JSON number of minutes; 0 pushes regardless of web use.
pub const SETTING_PUSH_WEB_IDLE_MINUTES: &str = "push.web_idle_minutes";
pub const PUSH_WEB_IDLE_MINUTES_DEFAULT: i64 = 3;
/// A day of silence is already far past any plausible "I am at my
/// desk" window, and the value is a threshold, not a schedule.
pub const PUSH_WEB_IDLE_MINUTES_MAX: i64 = 1440;
/// A stored value is one short JSON number.
pub const PUSH_WEB_IDLE_MINUTES_MAX_BYTES: usize = 16;

/// Settings that hold operator credentials. Their values never appear
/// in listings or logs, only whether they are set, and they are sealed
/// with the installation secret before they reach the database, the
/// same way device push tokens are.
pub const SECRET_SETTINGS: &[&str] = &[];

pub const PUSH_TOKEN_MAX: usize = 512;
pub const PUSH_LOCALE_MAX: usize = 40;

/// A device hint is trusted this long; a foregrounded app refreshes it
/// while the session stays open.
const FOREGROUND_HINT_TTL_MS: i64 = 60_000;

const MINUTE_MS: i64 = 60_000;

/// Retry delays indexed by completed attempts; a delivery settles as
/// failed once attempts reach `MAX_PUSH_ATTEMPTS`.
const PUSH_BACKOFF_MS: &[i64] = &[5_000, 30_000, 120_000, 600_000, 1_800_000];
const MAX_PUSH_ATTEMPTS: u32 = 6;

const DELIVERY_BATCH: usize = 32;
/// Half a SHA-256, hex encoded: comfortably inside the 64-byte
/// apns-collapse-id limit and far past guessing.
const OPAQUE_ID_BYTES: usize = 16;

/// Names for the gates that can decline a queued notification. Each is
/// correct behaviour, and each is otherwise indistinguishable from a
/// push that was lost.
const GATE_NO_ENDPOINTS: &str = "no-registered-endpoints";
const GATE_SUPERVISION_SNOOZE: &str = "supervision_snooze";
const GATE_CLASSIFICATION: &str = "classification";
const GATE_EVENT_MASK: &str = "policy-event-mask";
const GATE_SCOPE: &str = "policy-scope";
const GATE_DEVICE_MASK: &str = "device-event-mask";
const GATE_FOREGROUND: &str = "foreground-hint";
const GATE_WEB_ACTIVE: &str = "web-activity";
const GATE_DEDUPE: &str = "dedupe";

pub const DISABLE_REASON_UNREGISTERED: &str = "device-not-registered";
/// The relay only forwards sealed payloads, so an endpoint that never
/// registered a usable X25519 key can never be delivered to.
pub const DISABLE_REASON_NO_PUBLIC_KEY: &str = "no-public-key";

/// Length of the raw X25519 public key an endpoint seals to.
const PUSH_PUBLIC_KEY_LEN: usize = 32;

/// Longest alert body in chars, ellipsis included. The state detail has
/// no storage cap, and lock screens clip long bodies anyway.
const PUSH_BODY_MAX_CHARS: usize = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushEventClass {
    NeedsInput,
    Failed,
    Completed,
    ConnectionApproval,
}

impl PushEventClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            PushEventClass::NeedsInput => "needs-input",
            PushEventClass::Failed => "failed",
            PushEventClass::Completed => "completed",
            PushEventClass::ConnectionApproval => "connection-approval",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "needs-input" => Some(PushEventClass::NeedsInput),
            "failed" => Some(PushEventClass::Failed),
            "completed" => Some(PushEventClass::Completed),
            "connection-approval" => Some(PushEventClass::ConnectionApproval),
            _ => None,
        }
    }

    pub fn mask(&self) -> u32 {
        match self {
            PushEventClass::NeedsInput => 1,
            PushEventClass::Failed => 2,
            PushEventClass::Completed => 4,
            PushEventClass::ConnectionApproval => PushEventClass::NeedsInput.mask(),
        }
    }
}

pub const PUSH_EVENTS_ALL_MASK: u32 = 1 | 2 | 4;

/// Renders an events mask back to CSV, the inverse of parse_events_csv.
pub fn events_csv(mask: u32) -> String {
    [
        PushEventClass::NeedsInput,
        PushEventClass::Failed,
        PushEventClass::Completed,
    ]
    .iter()
    .filter(|class| mask & class.mask() != 0)
    .map(|class| class.as_str())
    .collect::<Vec<_>>()
    .join(",")
}

/// Parses an events CSV into a mask; None when a token is unknown.
pub fn parse_events_csv(csv: &str) -> Option<u32> {
    let mut mask = 0;
    for token in csv.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        mask |= PushEventClass::parse(token)?.mask();
    }
    Some(mask)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushScope {
    /// Supervisors and workers with no supervising session; supervised
    /// workers stay silent because their supervisor is the audience.
    Default,
    All,
    Supervisors,
    None,
}

impl PushScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            PushScope::Default => "default",
            PushScope::All => "all",
            PushScope::Supervisors => "supervisors",
            PushScope::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "default" => Some(PushScope::Default),
            "all" => Some(PushScope::All),
            "supervisors" => Some(PushScope::Supervisors),
            "none" => Some(PushScope::None),
            _ => None,
        }
    }
}

/// Whether an endpoint carries a public key a notification can be
/// sealed to. The relay forwards nothing else, so an endpoint that
/// fails this can never be delivered to and is disabled rather than
/// retried per notification forever.
pub(crate) fn seals_to_a_usable_key(endpoint: &PushEndpoint) -> bool {
    use base64::engine::{general_purpose::STANDARD as B64, Engine};
    B64.decode(&endpoint.public_key)
        .is_ok_and(|bytes| bytes.len() == PUSH_PUBLIC_KEY_LEN)
}

/// The web-activity rule itself: an interaction inside the idle window
/// holds back the push. An `idle_window_ms` of zero disables the gate,
/// which is how a user asks to be pushed while they are at the desk.
pub(crate) fn web_activity_holds_push(
    last_activity: Option<i64>,
    idle_window_ms: i64,
    now: i64,
) -> bool {
    idle_window_ms > 0 && last_activity.is_some_and(|ts| now - ts <= idle_window_ms)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WebIdleMinutesError {
    #[error("web idle minutes must be at most {PUSH_WEB_IDLE_MINUTES_MAX_BYTES} bytes")]
    TooLarge,
    #[error("web idle minutes must be a whole number: {0}")]
    Malformed(String),
    #[error("web idle minutes must be between 0 and {PUSH_WEB_IDLE_MINUTES_MAX}")]
    OutOfRange,
}

/// Parses the user's idle threshold and returns one stable
/// representation suitable for storage and synchronization.
pub fn normalize_web_idle_minutes(input: &[u8]) -> Result<(i64, String), WebIdleMinutesError> {
    if input.len() > PUSH_WEB_IDLE_MINUTES_MAX_BYTES {
        return Err(WebIdleMinutesError::TooLarge);
    }
    let minutes: i64 = serde_json::from_slice(input)
        .map_err(|error| WebIdleMinutesError::Malformed(error.to_string()))?;
    if !(0..=PUSH_WEB_IDLE_MINUTES_MAX).contains(&minutes) {
        return Err(WebIdleMinutesError::OutOfRange);
    }
    Ok((minutes, minutes.to_string()))
}

pub(crate) fn scope_allows(scope: PushScope, role: SessionRole, supervised: bool) -> bool {
    match scope {
        PushScope::All => true,
        PushScope::None => false,
        PushScope::Supervisors => role == SessionRole::Supervisor,
        PushScope::Default => role == SessionRole::Supervisor || !supervised,
    }
}

/// Classifies one committed transition into a notification class.
/// Completed is any working-to-idle transition without a state detail,
/// the same turn end that marks the session unseen; a failed, interrupted
/// or inferred turn end carries a detail and never counts.
pub(crate) fn classify_transition(
    from: SessionState,
    to: SessionState,
    state_detail: &str,
) -> Option<PushEventClass> {
    match (from, to) {
        (from, SessionState::NeedsInput) if from != SessionState::NeedsInput => {
            Some(PushEventClass::NeedsInput)
        }
        (from, SessionState::Failed) if from != SessionState::Failed => {
            Some(PushEventClass::Failed)
        }
        (SessionState::Working, SessionState::Idle) if state_detail.is_empty() => {
            Some(PushEventClass::Completed)
        }
        _ => None,
    }
}

/// The durable event identity: same committed transition, same id,
/// across restarts, reconciliation, and provider retries.
pub(crate) fn push_event_id(
    installation_id: &str,
    session_id: u64,
    generation: u64,
    class: PushEventClass,
    state_revision: u64,
) -> String {
    format!(
        "{installation_id}:{session_id}:{generation}:{}:{state_revision}",
        class.as_str()
    )
}

/// An approval delivery's event id is this prefix and the call id; it is only stored locally or sealed.
const CONNECTION_APPROVAL_EVENT_PREFIX: &str = "connection-approval:";

/// The call an approval delivery is about, sealed so a tap can open it.
fn approval_id(class: PushEventClass, event_id: &str) -> Option<String> {
    (class == PushEventClass::ConnectionApproval)
        .then(|| event_id.strip_prefix(CONNECTION_APPROVAL_EVENT_PREFIX))
        .flatten()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// Bytes an observer sees, derived from something they must not learn.
/// The label separates uses so two of them cannot be compared, and the
/// device id keeps one session looking different to each device.
pub(crate) fn opaque_id(secret: &str, label: &str, device_id: u64, value: u64) -> String {
    use hmac::{Mac, SimpleHmac};
    let mut mac = SimpleHmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(label.as_bytes());
    mac.update(&device_id.to_be_bytes());
    mac.update(&value.to_be_bytes());
    hex::encode(&mac.finalize().into_bytes()[..OPAQUE_ID_BYTES])
}

/// Collapse ids are per session so a later state replaces stale
/// needs-input content where the platform supports it. The session id
/// itself cannot be the value: the collapse id travels as a cleartext
/// APNs header, so a readable one tells the push service which session
/// each alert belongs to and, because session ids count up, how many
/// there have been.
pub(crate) fn push_collapse_id(secret: &str, device_id: u64, session_id: u64) -> String {
    opaque_id(secret, "collapse", device_id, session_id)
}

/// What the app is told the notification came from. The installation id
/// is permanent, so sending it would let anyone on the path link every
/// notification from one controller for as long as it exists. A device
/// enrols with several controllers, so this still has to tell them
/// apart; it is returned at registration for the device to map back.
pub(crate) fn push_controller_ref(secret: &str, device_id: u64) -> String {
    opaque_id(secret, "controller", device_id, 0)
}

/// Alert text. With previews off the title is generic and the body
/// empty; with previews on the controller may add project name and the
/// agent-authored question, failure detail, or headline, never terminal
/// output or report context.
pub(crate) fn message_content(
    class: PushEventClass,
    previews_enabled: bool,
    project_name: &str,
    headline: &str,
    state_detail: &str,
) -> (String, String) {
    let action = match class {
        PushEventClass::NeedsInput => "needs input",
        PushEventClass::Failed => "failed",
        PushEventClass::Completed => "completed work",
        PushEventClass::ConnectionApproval => "requires connection approval",
    };
    if !previews_enabled || project_name.is_empty() {
        return (format!("Session {action}"), String::new());
    }
    let detail = state_detail.trim();
    let body = match class {
        PushEventClass::NeedsInput
        | PushEventClass::Failed
        | PushEventClass::ConnectionApproval
            if !detail.is_empty() =>
        {
            detail
        }
        _ => headline,
    };
    let project_name = crate::text::unescape_html_entities(project_name);
    (
        format!("{project_name}: session {action}"),
        truncate_with_ellipsis(
            &crate::text::unescape_html_entities(body),
            PUSH_BODY_MAX_CHARS,
        ),
    )
}

/// Truncates on a char boundary to at most `max` chars, the last of
/// which is an ellipsis when anything was cut.
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// A built provider adapter cached under the fingerprint of the
/// credential settings that produced it.
type CachedProvider = (String, Arc<dyn PushProvider>);

/// In-memory push state on the daemon: foreground hints, the delivery
/// worker wakeup, and the per-provider adapter cache.
pub(crate) struct PushRuntime {
    /// device id -> (session id, hint time); entries expire by TTL.
    foreground: Mutex<HashMap<u64, (u64, i64)>>,
    /// user id -> last web interaction time. Never persisted: after a
    /// restart nobody has interacted with this daemon's web UI yet.
    web_activity: Mutex<HashMap<u64, i64>>,
    wakeup: tokio::sync::Notify,
    /// Rebuilt when the fingerprint of the credential settings changes.
    providers: Mutex<HashMap<&'static str, CachedProvider>>,
}

impl Default for PushRuntime {
    fn default() -> Self {
        Self {
            foreground: Mutex::new(HashMap::new()),
            web_activity: Mutex::new(HashMap::new()),
            wakeup: tokio::sync::Notify::new(),
            providers: Mutex::new(HashMap::new()),
        }
    }
}

/// Validated push registration input.
pub struct RegisterPushEndpoint {
    pub token: String,
    pub environment: String,
    pub locale: String,
    pub previews_enabled: bool,
    pub event_mask: u32,
    /// X25519 public key (base64), required for gateway mode. The
    /// daemon seals the notification payload with this key so only
    /// the device can read it.
    pub public_key: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PushDeliveryStats {
    pub attempted: usize,
    pub sent: usize,
}

impl Daemon {
    /// Registers or rotates the push endpoint of one of the user's
    /// devices. Re-registration replaces the token and re-enables an
    /// endpoint disabled by a DeviceNotRegistered receipt.
    pub fn register_push_endpoint(
        &self,
        user_id: u64,
        device_id: u64,
        registration: RegisterPushEndpoint,
    ) -> Result<PushEndpoint, MobileAuthError> {
        let device = self
            .storage()
            .get_mobile_device(device_id)
            .map_err(|_| MobileAuthError::DeviceNotFound)?;
        if device.user_id != user_id || device.revoked_at_unix_ms.is_some() {
            return Err(MobileAuthError::DeviceNotFound);
        }
        if registration.environment != ENVIRONMENT_PRODUCTION
            && registration.environment != ENVIRONMENT_SANDBOX
        {
            return Err(MobileAuthError::Rejected(format!(
                "environment must be {ENVIRONMENT_PRODUCTION} or {ENVIRONMENT_SANDBOX}"
            )));
        }
        let token = registration.token.trim();
        if token.is_empty() || token.chars().count() > PUSH_TOKEN_MAX {
            return Err(MobileAuthError::Rejected(format!(
                "token must be 1 to {PUSH_TOKEN_MAX} characters"
            )));
        }
        // The relay only carries sealed payloads, so an endpoint with no
        // key could never be delivered to.
        if registration.public_key.trim().is_empty() {
            return Err(MobileAuthError::Rejected(
                "public_key is required: notifications are sealed to the device".into(),
            ));
        }
        if registration.locale.chars().count() > PUSH_LOCALE_MAX {
            return Err(MobileAuthError::Rejected(format!(
                "locale must be at most {PUSH_LOCALE_MAX} characters"
            )));
        }
        let sealed = seal_secret(&self.installation_secret(), token);
        let endpoint = self.storage().upsert_push_endpoint(
            device_id,
            &sealed,
            &registration.environment,
            registration.locale.trim(),
            registration.previews_enabled,
            registration.event_mask & PUSH_EVENTS_ALL_MASK,
            &registration.public_key,
            now_unix_ms(),
        )?;
        info!(
            device = device_id,
            environment = registration.environment,
            "push endpoint registered"
        );
        Ok(endpoint)
    }

    pub fn unregister_push_endpoint(
        &self,
        user_id: u64,
        device_id: u64,
    ) -> Result<bool, MobileAuthError> {
        let device = self
            .storage()
            .get_mobile_device(device_id)
            .map_err(|_| MobileAuthError::DeviceNotFound)?;
        if device.user_id != user_id {
            return Err(MobileAuthError::DeviceNotFound);
        }
        let removed = self
            .storage()
            .delete_push_endpoint(device_id)
            .map_err(|_| MobileAuthError::Internal)?;
        if removed {
            info!(device = device_id, "push endpoint unregistered");
        }
        Ok(removed)
    }

    pub fn push_endpoint(&self, device_id: u64) -> Result<Option<PushEndpoint>, MobileAuthError> {
        self.storage()
            .get_push_endpoint(device_id)
            .map_err(|_| MobileAuthError::Internal)
    }

    /// Records which session terminal a device currently has in the
    /// foreground; delivery to that device for that session is
    /// suppressed while the hint is fresh.
    pub fn set_push_foreground_hint(&self, device_id: u64, session_id: Option<u64>) {
        let mut hints = self.push_runtime().foreground.lock().unwrap();
        match session_id {
            Some(session_id) => {
                hints.insert(device_id, (session_id, now_unix_ms()));
            }
            None => {
                hints.remove(&device_id);
            }
        }
    }

    fn foreground_suppressed(&self, device_id: u64, session_id: u64, now: i64) -> bool {
        let hints = self.push_runtime().foreground.lock().unwrap();
        hints
            .get(&device_id)
            .is_some_and(|(s, ts)| *s == session_id && now - ts <= FOREGROUND_HINT_TTL_MS)
    }

    /// Records that the user just interacted with the web UI. The
    /// browser reports interaction, not presence: a tab left open on a
    /// second monitor sends nothing.
    pub fn note_web_activity(&self, user_id: u64) {
        self.push_runtime()
            .web_activity
            .lock()
            .unwrap()
            .insert(user_id, now_unix_ms());
    }

    /// Whether the user's own web use holds back every push to them.
    /// This is the single place the rule is decided, so narrowing it to
    /// the session on screen stays a change to one predicate.
    fn web_activity_suppressed(&self, user_id: u64, now: i64) -> bool {
        let last = self
            .push_runtime()
            .web_activity
            .lock()
            .unwrap()
            .get(&user_id)
            .copied();
        web_activity_holds_push(last, self.push_web_idle_ms(user_id), now)
    }

    /// The user's idle threshold in milliseconds, falling back to the
    /// default when unset or unreadable.
    fn push_web_idle_ms(&self, user_id: u64) -> i64 {
        let stored = self
            .storage()
            .get_user_setting(user_id, SETTING_PUSH_WEB_IDLE_MINUTES)
            .ok()
            .flatten();
        let minutes = stored
            .and_then(|value| normalize_web_idle_minutes(value.as_bytes()).ok())
            .map_or(PUSH_WEB_IDLE_MINUTES_DEFAULT, |(minutes, _)| minutes);
        minutes * MINUTE_MS
    }

    pub fn list_push_policies(&self) -> Result<Vec<PushPolicy>, DaemonError> {
        Ok(self.storage().list_push_policies()?)
    }

    pub fn set_push_policy(
        &self,
        bucket_id: u64,
        role: &str,
        events: &str,
        scope: &str,
    ) -> Result<(), DaemonError> {
        if !matches!(role, "" | "worker" | "supervisor") {
            return Err(DaemonError::Rejected(
                "role must be empty, worker, or supervisor".into(),
            ));
        }
        if parse_events_csv(events).is_none() {
            return Err(DaemonError::Rejected(format!(
                "events must be a comma-separated subset of {PUSH_EVENTS_DEFAULT}"
            )));
        }
        if PushScope::parse(scope).is_none() {
            return Err(DaemonError::Rejected(
                "scope must be default, all, supervisors, or none".into(),
            ));
        }
        self.storage().get_bucket(bucket_id)?;
        self.storage()
            .set_push_policy(bucket_id, role, events, scope, now_unix_ms())?;
        info!(bucket = bucket_id, role, "push policy set");
        Ok(())
    }

    pub fn delete_push_policy(&self, bucket_id: u64, role: &str) -> Result<bool, DaemonError> {
        Ok(self.storage().delete_push_policy(bucket_id, role)?)
    }

    fn push_events_setting(&self) -> u32 {
        self.storage()
            .get_setting(SETTING_PUSH_EVENTS)
            .ok()
            .flatten()
            .as_deref()
            .and_then(parse_events_csv)
            .unwrap_or(PUSH_EVENTS_ALL_MASK)
    }

    /// Reports push configuration once at startup. Delivery needs both
    /// credentials and at least one registered device, and a pipeline
    /// that is silent for want of a device looks exactly like one that
    /// is silent for want of a key.
    pub fn log_push_configuration(&self) {
        let endpoints = self.storage().active_push_endpoints().unwrap_or_default();
        let sandbox = endpoints
            .iter()
            .filter(|(e, _)| e.environment == ENVIRONMENT_SANDBOX)
            .count();

        match self.push_gateway_url() {
            None => info!(
                devices = endpoints.len(),
                "push gateway url is set to empty, no notification will be sent"
            ),
            Some(url) => info!(
                url,
                events = events_csv(self.push_events_setting()),
                scope = self.push_scope_setting().as_str(),
                devices = endpoints.len(),
                sandbox,
                production = endpoints.len() - sandbox,
                "push configured"
            ),
        }

        if endpoints.is_empty() {
            info!("no device is registered for push, so there is nothing to deliver to");
        }
    }

    fn push_scope_setting(&self) -> PushScope {
        self.storage()
            .get_setting(SETTING_PUSH_SCOPE)
            .ok()
            .flatten()
            .as_deref()
            .and_then(PushScope::parse)
            .unwrap_or(PushScope::Default)
    }

    /// Resolves the effective (events mask, scope) for a session:
    /// bucket+role row, then bucket row, then controller settings.
    pub(crate) fn resolve_push_policy(
        &self,
        bucket_id: u64,
        role: SessionRole,
    ) -> (u32, PushScope) {
        let policies = self.storage().list_push_policies().unwrap_or_default();
        let pick = |want_role: &str| {
            policies
                .iter()
                .find(|p| p.bucket_id == bucket_id && p.role == want_role)
        };
        let policy = pick(role.as_str()).or_else(|| pick(""));
        match policy {
            Some(policy) => (
                parse_events_csv(&policy.events).unwrap_or(0),
                PushScope::parse(&policy.scope).unwrap_or(PushScope::Default),
            ),
            None => (self.push_events_setting(), self.push_scope_setting()),
        }
    }

    fn push_dedupe_window_ms(&self) -> i64 {
        let hours = self
            .storage()
            .get_setting(SETTING_PUSH_DEDUPE_WINDOW_HOURS)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(PUSH_DEDUPE_WINDOW_HOURS_DEFAULT);
        hours.saturating_mul(60 * 60 * 1000)
    }

    /// Derives a notification event from one committed state
    /// transition and queues one delivery per eligible endpoint.
    /// Called under the state lock right after the transition commits.
    pub(crate) fn observe_push_transition(
        &self,
        session: &Session,
        bucket_id: u64,
        generation: u64,
        from: SessionState,
        to: SessionState,
    ) -> Option<SessionAlert> {
        let declined = |gate: &'static str| {
            debug!(
                session = session.id,
                from = from.as_str(),
                to = to.as_str(),
                gate,
                "push event declined"
            );
        };
        let (marks_revision, _) = match self.storage().session_transition_marks(session.id) {
            Ok(marks) => marks,
            Err(e) => {
                warn!(session = session.id, error = %e, "push transition marks read failed");
                return None;
            }
        };
        // A done root record is a clean finish even though its detail
        // names it, which an empty detail stands for everywhere else.
        let root_done = session
            .program_status
            .first()
            .is_some_and(|root| root.id.is_empty() && root.state == ProgramStatusState::Done);
        let detail = if root_done { "" } else { &session.state_detail };
        let Some(class) = classify_transition(from, to, detail) else {
            declined(GATE_CLASSIFICATION);
            return None;
        };
        if class == PushEventClass::Completed
            && self
                .storage()
                .supervision_completion_silent(session.id, generation, marks_revision)
                .unwrap_or(false)
        {
            declined(GATE_SUPERVISION_SNOOZE);
            return None;
        }
        let (events_mask, scope) = self.resolve_push_policy(bucket_id, session.role);
        if events_mask & class.mask() == 0 {
            declined(GATE_EVENT_MASK);
            return None;
        }
        if !scope_allows(scope, session.role, session.spawned_by_session_id.is_some()) {
            declined(GATE_SCOPE);
            return None;
        }
        // Everything above decides whether this transition is worth telling
        // anyone about. A connected client is told from here, so it renders
        // the same judgement rather than repeating it, and so a fleet with
        // no phone registered still raises alerts.
        let alert = SessionAlert {
            session_id: session.id,
            kind: match class {
                PushEventClass::NeedsInput => SessionAlertKind::NeedsInput,
                PushEventClass::Failed => SessionAlertKind::Failed,
                PushEventClass::Completed => SessionAlertKind::Completed,
                PushEventClass::ConnectionApproval => SessionAlertKind::NeedsInput,
            },
        };
        let endpoints = match self.storage().active_push_endpoints() {
            Ok(endpoints) if !endpoints.is_empty() => endpoints,
            Ok(_) => {
                declined(GATE_NO_ENDPOINTS);
                return Some(alert);
            }
            Err(e) => {
                warn!(session = session.id, error = %e, "push endpoint read failed");
                return Some(alert);
            }
        };
        let event_id = push_event_id(
            &self.installation_id(),
            session.id,
            generation,
            class,
            marks_revision,
        );
        let secret = self.installation_secret();
        let now = now_unix_ms();
        let mut queued = false;
        for (endpoint, user_id) in endpoints {
            if endpoint.event_mask & class.mask() == 0 {
                debug!(
                    session = session.id,
                    device = endpoint.device_id,
                    event = class.as_str(),
                    gate = GATE_DEVICE_MASK,
                    "push event declined"
                );
                continue;
            }
            let foreground = self.foreground_suppressed(endpoint.device_id, session.id, now);
            if foreground {
                debug!(
                    session = session.id,
                    device = endpoint.device_id,
                    event = class.as_str(),
                    gate = GATE_FOREGROUND,
                    "push event declined"
                );
            }
            let web_active = !foreground && self.web_activity_suppressed(user_id, now);
            if web_active {
                info!(
                    session = session.id,
                    device = endpoint.device_id,
                    event = class.as_str(),
                    gate = GATE_WEB_ACTIVE,
                    "push event declined"
                );
            }
            let suppressed = foreground || web_active;
            let status = if suppressed {
                DELIVERY_SUPPRESSED
            } else {
                DELIVERY_PENDING
            };
            let collapse_id = push_collapse_id(&secret, endpoint.device_id, session.id);
            match self.storage().enqueue_notification_delivery(
                &event_id,
                endpoint.device_id,
                session.id,
                class.as_str(),
                &collapse_id,
                status,
                now,
                now,
            ) {
                Ok(true) if !suppressed => queued = true,
                Ok(false) => debug!(
                    session = session.id,
                    device = endpoint.device_id,
                    event = class.as_str(),
                    gate = GATE_DEDUPE,
                    "push event declined"
                ),
                Ok(_) => {}
                Err(e) => warn!(error = %e, "push delivery enqueue failed"),
            }
        }
        if queued {
            debug!(
                session = session.id,
                event = class.as_str(),
                "push event queued"
            );
            self.push_runtime().wakeup.notify_one();
        }
        Some(alert)
    }

    pub(crate) fn queue_connection_approval_push(&self, call: &crate::connections::Call) {
        let class = PushEventClass::ConnectionApproval;
        let event_id = format!("{CONNECTION_APPROVAL_EVENT_PREFIX}{}", call.id);
        if self.push_events_setting() & class.mask() == 0 {
            return;
        }
        let Ok(endpoints) = self.storage().active_push_endpoints() else {
            return;
        };
        let now = now_unix_ms();
        for (endpoint, user_id) in endpoints {
            if endpoint.event_mask & class.mask() == 0 || !seals_to_a_usable_key(&endpoint) {
                continue;
            }
            let suppressed = self.web_activity_suppressed(user_id, now);
            let status = if suppressed {
                DELIVERY_SUPPRESSED
            } else {
                DELIVERY_PENDING
            };
            let collapse_id = opaque_id(
                &self.installation_secret(),
                &event_id,
                endpoint.device_id,
                call.session_id,
            );
            if let Err(error) = self.storage().enqueue_notification_delivery(
                &event_id,
                endpoint.device_id,
                call.session_id,
                class.as_str(),
                &collapse_id,
                status,
                now,
                now,
            ) {
                warn!(%error,"connection approval notification could not be queued");
            }
        }
        self.push_runtime().wakeup.notify_one();
    }

    pub(crate) fn push_wakeup(&self) -> &tokio::sync::Notify {
        &self.push_runtime().wakeup
    }

    /// The relay to deliver through: the operator's setting when one is
    /// stored, otherwise the built-in default. An explicitly empty
    /// value is a decision to send nothing, so it does not fall back.
    pub(crate) fn push_gateway_url(&self) -> Option<String> {
        match self.setting_value(SETTING_PUSH_GATEWAY_URL) {
            Some(stored) => Some(stored.trim().to_string()).filter(|url| !url.is_empty()),
            None => Some(PUSH_GATEWAY_URL_DEFAULT.to_string()),
        }
    }

    /// The relay is the only way out. A controller that wants to reach
    /// Apple directly runs its own relay and points at that.
    fn push_provider(&self) -> Result<Arc<dyn PushProvider>, String> {
        let url = self
            .push_gateway_url()
            .ok_or("push gateway url is set to empty")?;
        let fingerprint = fingerprint(&[&url]);
        if let Some(cached) = self.cached_provider(PROVIDER_GATEWAY, &fingerprint) {
            return Ok(cached);
        }
        let adapter = Arc::new(GatewayProvider::new(GatewayConfig { endpoint: url })?);
        self.cache_provider(PROVIDER_GATEWAY, fingerprint, adapter.clone());
        Ok(adapter)
    }

    fn cached_provider(
        &self,
        name: &'static str,
        fingerprint: &str,
    ) -> Option<Arc<dyn PushProvider>> {
        let providers = self.push_runtime().providers.lock().unwrap();
        providers
            .get(name)
            .filter(|(cached, _)| cached == fingerprint)
            .map(|(_, adapter)| adapter.clone())
    }

    fn cache_provider(
        &self,
        name: &'static str,
        fingerprint: String,
        adapter: Arc<dyn PushProvider>,
    ) {
        self.push_runtime()
            .providers
            .lock()
            .unwrap()
            .insert(name, (fingerprint, adapter));
    }

    fn build_push_message(
        &self,
        delivery: &NotificationDelivery,
        endpoint: &PushEndpoint,
        token: String,
        class: PushEventClass,
    ) -> PushMessage {
        let (project_name, headline, state_detail) = if endpoint.previews_enabled {
            let session = self.storage().get_session(delivery.session_id).ok();
            let project = session
                .as_ref()
                .and_then(|s| self.storage().get_project(s.project_id).ok());
            let (headline, state_detail) = session
                .map(|s| (s.headline, s.state_detail))
                .unwrap_or_default();
            (
                project.map(|p| p.name).unwrap_or_default(),
                headline,
                state_detail,
            )
        } else {
            (String::new(), String::new(), String::new())
        };
        let approval_id = approval_id(class, &delivery.event_id);
        let state_detail = match &approval_id {
            Some(id) => {
                let call = self.storage().connection_call(id).ok();
                let connection = call
                    .as_ref()
                    .and_then(|call| self.storage().connection(call.connection_id).ok());
                match (call, connection) {
                    (Some(call), Some(connection)) => format!(
                        "Review {} on {} in Approvals.",
                        call.tool, connection.config.name
                    ),
                    _ => "Review the privileged tool call in Approvals.".to_string(),
                }
            }
            None => state_detail,
        };
        let (title, body) = message_content(
            class,
            endpoint.previews_enabled,
            &project_name,
            &headline,
            &state_detail,
        );
        let controller_ref = push_controller_ref(&self.installation_secret(), endpoint.device_id);
        // An endpoint with no usable key is disabled before it reaches
        // here, so only a sealing failure leaves the payload unsealed.
        let sealed = seals_to_a_usable_key(endpoint)
            .then(|| {
                self.seal_notification(endpoint, &controller_ref, delivery, class, &title, &body)
            })
            .flatten();
        let mutable_content = sealed.is_some();
        let data = match sealed {
            Some(sealed_b64) => serde_json::json!({ SEALED_PAYLOAD_KEY: sealed_b64 }),
            None => serde_json::json!({}),
        };
        PushMessage {
            token,
            title: PUSH_PLACEHOLDER_TITLE.to_string(),
            body: PUSH_PLACEHOLDER_BODY.to_string(),
            collapse_id: delivery.collapse_id.clone(),
            event_id: delivery.event_id.clone(),
            data,
            environment: endpoint.environment.clone(),
            mutable_content,
        }
    }

    /// Seals a notification for the device's public key using standard
    /// HPKE. Returns the base64-encoded sealed payload, or None if the
    /// key is invalid. The monotonic counter is incremented atomically
    /// so the device can reject replays.
    fn seal_notification(
        &self,
        endpoint: &PushEndpoint,
        controller_ref: &str,
        delivery: &NotificationDelivery,
        class: PushEventClass,
        title: &str,
        body: &str,
    ) -> Option<String> {
        use base64::engine::{general_purpose::STANDARD as B64, Engine};

        let pk_bytes = B64.decode(&endpoint.public_key).ok()?;
        let pk: [u8; 32] = pk_bytes.try_into().ok()?;

        let counter = self
            .storage()
            .increment_push_counter(endpoint.device_id)
            .ok()?;

        let now_ms = now_unix_ms();
        let notification = SealedNotification {
            title: title.to_string(),
            body: body.to_string(),
            controller_id: controller_ref.to_string(),
            session_id: delivery.session_id,
            state: class.as_str().to_string(),
            event_id: delivery.event_id.clone(),
            deep_link: format!(
                "puppetmaster://controller/{}/session/{}",
                controller_ref, delivery.session_id
            ),
            timestamp_unix_ms: now_ms,
            counter,
            approval_id: approval_id(class, &delivery.event_id),
        };

        let plaintext = notification.to_padded_json();
        let sealed = pm_push::hpke::seal(&pk, &plaintext);
        Some(B64.encode(&sealed))
    }

    /// One drain pass of the delivery queue plus receipt cleanup.
    /// Runs from the server's delivery task; tests call it directly.
    pub async fn process_push_deliveries(&self, now: i64) -> PushDeliveryStats {
        let mut stats = PushDeliveryStats::default();
        let due = match self
            .storage()
            .due_notification_deliveries(now, DELIVERY_BATCH)
        {
            Ok(due) => due,
            Err(e) => {
                warn!(error = %e, "push delivery queue read failed");
                return stats;
            }
        };
        for delivery in due {
            stats.attempted += 1;
            if self.attempt_push_delivery(&delivery, now).await {
                stats.sent += 1;
            }
        }
        let cutoff = now - self.push_dedupe_window_ms();
        if let Ok(pruned) = self.storage().prune_notification_deliveries(cutoff) {
            if pruned > 0 {
                debug!(pruned, "push delivery receipts pruned");
            }
        }
        stats
    }

    async fn attempt_push_delivery(&self, delivery: &NotificationDelivery, now: i64) -> bool {
        let storage = self.storage();
        let settle = |status: &str, error: &str| {
            let _ = storage.mark_delivery_settled(delivery.id, status, error, now);
        };
        let endpoint = match storage.get_push_endpoint(delivery.device_id) {
            Ok(Some(endpoint)) if endpoint.disabled_at_unix_ms.is_none() => endpoint,
            Ok(Some(endpoint)) => {
                info!(
                    device = delivery.device_id,
                    session = delivery.session_id,
                    event = delivery.state,
                    reason = %endpoint.disabled_reason,
                    "push delivery dropped: endpoint is disabled"
                );
                settle(DELIVERY_FAILED, "endpoint removed or disabled");
                return false;
            }
            Ok(None) => {
                info!(
                    device = delivery.device_id,
                    session = delivery.session_id,
                    event = delivery.state,
                    "push delivery dropped: endpoint is gone"
                );
                settle(DELIVERY_FAILED, "endpoint removed or disabled");
                return false;
            }
            Err(e) => {
                warn!(
                    device = delivery.device_id,
                    session = delivery.session_id,
                    error = %e,
                    "push delivery dropped: endpoint lookup failed"
                );
                settle(DELIVERY_FAILED, "endpoint removed or disabled");
                return false;
            }
        };
        if !seals_to_a_usable_key(&endpoint) {
            let _ = storage.disable_push_endpoint(
                delivery.device_id,
                DISABLE_REASON_NO_PUBLIC_KEY,
                now,
            );
            settle(DELIVERY_FAILED, DISABLE_REASON_NO_PUBLIC_KEY);
            warn!(
                device = delivery.device_id,
                session = delivery.session_id,
                reason = DISABLE_REASON_NO_PUBLIC_KEY,
                "push endpoint disabled: it registered no usable public key, so nothing can be \
                 sealed to it. The device must register again."
            );
            return false;
        }
        let Some(class) = PushEventClass::parse(&delivery.state) else {
            settle(DELIVERY_FAILED, "unknown event class");
            return false;
        };
        let Some(token) = open_secret(&self.installation_secret(), &endpoint.token_ciphertext)
        else {
            settle(DELIVERY_FAILED, "stored token unreadable");
            return false;
        };
        let provider = match self.push_provider() {
            Ok(provider) => provider,
            Err(detail) => {
                self.retry_or_fail(delivery, &detail, now);
                return false;
            }
        };
        let message = self.build_push_message(delivery, &endpoint, token, class);
        tracing::debug!(
            device = delivery.device_id,
            session = delivery.session_id,
            previews = endpoint.previews_enabled,
            title = %message.title,
            body = %message.body,
            mutable_content = message.mutable_content,
            "built push message"
        );
        match provider.send(&message).await {
            SendOutcome::Sent {
                provider_message_id,
            } => {
                let _ =
                    storage.mark_delivery_sent(delivery.id, provider_message_id.as_deref(), now);
                info!(
                    device = delivery.device_id,
                    session = delivery.session_id,
                    event = delivery.state,
                    "push notification sent"
                );
                true
            }
            SendOutcome::Retryable { detail } => {
                self.retry_or_fail(delivery, &detail, now);
                false
            }
            SendOutcome::DeviceNotRegistered => {
                let _ = storage.disable_push_endpoint(
                    delivery.device_id,
                    DISABLE_REASON_UNREGISTERED,
                    now,
                );
                settle(DELIVERY_UNREGISTERED, DISABLE_REASON_UNREGISTERED);
                provider.invalidate_device(&message.token).await;
                info!(
                    device = delivery.device_id,
                    "push endpoint disabled by provider receipt"
                );
                false
            }
            SendOutcome::Rejected { detail } => {
                warn!(
                    device = delivery.device_id,
                    detail, "push delivery rejected"
                );
                settle(DELIVERY_FAILED, &detail);
                false
            }
        }
    }

    fn retry_or_fail(&self, delivery: &NotificationDelivery, detail: &str, now: i64) {
        let attempts_after = delivery.attempt_count + 1;
        if attempts_after >= MAX_PUSH_ATTEMPTS {
            warn!(
                device = delivery.device_id,
                detail, "push delivery gave up after retries"
            );
            let _ = self
                .storage()
                .mark_delivery_settled(delivery.id, DELIVERY_FAILED, detail, now);
            return;
        }
        let index = (delivery.attempt_count as usize).min(PUSH_BACKOFF_MS.len() - 1);
        let _ = self.storage().mark_delivery_retry(
            delivery.id,
            now + PUSH_BACKOFF_MS[index],
            detail,
            now,
        );
    }
}

fn fingerprint(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::DaemonConfig;
    use crate::mobile::{MobileEnrollProof, MobileEnrollRequest};
    use pm_protocol::gateway::PUSH_DATA_KEY;

    #[test]
    fn an_events_mask_round_trips_through_csv() {
        for csv in [
            "needs-input",
            "failed",
            "needs-input,completed",
            "needs-input,failed,completed",
        ] {
            let mask = parse_events_csv(csv).unwrap();
            assert_eq!(events_csv(mask), csv, "round trip failed for {csv}");
        }
        assert_eq!(events_csv(0), "");
        assert_eq!(
            events_csv(PUSH_EVENTS_ALL_MASK),
            "needs-input,failed,completed"
        );
    }

    #[test]
    fn web_activity_holds_push_only_inside_the_threshold() {
        let window = 3 * MINUTE_MS;
        let now = 1_000_000;
        assert!(
            web_activity_holds_push(Some(now - window), window, now),
            "an interaction exactly at the threshold still counts as active"
        );
        assert!(web_activity_holds_push(Some(now - window + 1), window, now));
        assert!(
            !web_activity_holds_push(Some(now - window - 1), window, now),
            "one millisecond past the threshold the user is idle"
        );
        assert!(
            !web_activity_holds_push(None, window, now),
            "a user who has never touched the web UI is never active"
        );
    }

    #[test]
    fn a_zero_threshold_disables_the_gate() {
        let now = 1_000_000;
        assert!(
            !web_activity_holds_push(Some(now), 0, now),
            "zero minutes means push regardless of web use"
        );
    }

    #[test]
    fn web_idle_minutes_accepts_whole_minutes_and_rejects_the_rest() {
        assert_eq!(normalize_web_idle_minutes(b"3").unwrap(), (3, "3".into()));
        assert_eq!(normalize_web_idle_minutes(b" 0 ").unwrap(), (0, "0".into()));
        assert_eq!(
            normalize_web_idle_minutes(PUSH_WEB_IDLE_MINUTES_MAX.to_string().as_bytes()).unwrap(),
            (
                PUSH_WEB_IDLE_MINUTES_MAX,
                PUSH_WEB_IDLE_MINUTES_MAX.to_string()
            ),
        );
        assert_eq!(
            normalize_web_idle_minutes(b"-1"),
            Err(WebIdleMinutesError::OutOfRange)
        );
        assert_eq!(
            normalize_web_idle_minutes((PUSH_WEB_IDLE_MINUTES_MAX + 1).to_string().as_bytes()),
            Err(WebIdleMinutesError::OutOfRange)
        );
        for input in [&b"1.5"[..], &b"\"3\""[..], &b"null"[..], &b""[..]] {
            assert!(
                matches!(
                    normalize_web_idle_minutes(input),
                    Err(WebIdleMinutesError::Malformed(_))
                ),
                "accepted {:?}",
                String::from_utf8_lossy(input),
            );
        }
        assert_eq!(
            normalize_web_idle_minutes(b"00000000000000003"),
            Err(WebIdleMinutesError::TooLarge)
        );
    }

    #[test]
    fn the_stored_threshold_decides_how_long_web_use_suppresses() {
        let d = daemon();
        let (user_id, _) = enrolled_device(&d);
        let now = now_unix_ms();
        assert!(
            !d.web_activity_suppressed(user_id, now),
            "no interaction yet"
        );

        d.note_web_activity(user_id);
        assert!(d.web_activity_suppressed(user_id, now));
        assert!(
            !d.web_activity_suppressed(
                user_id,
                now + PUSH_WEB_IDLE_MINUTES_DEFAULT * MINUTE_MS + 1
            ),
            "the default threshold is {PUSH_WEB_IDLE_MINUTES_DEFAULT} minutes"
        );

        d.set_push_web_idle_minutes(user_id, Some(b"10")).unwrap();
        assert!(d.web_activity_suppressed(user_id, now + 9 * MINUTE_MS));

        d.set_push_web_idle_minutes(user_id, Some(b"0")).unwrap();
        assert!(
            !d.web_activity_suppressed(user_id, now),
            "zero disables the gate even with a fresh interaction"
        );
    }

    #[test]
    fn an_unset_gateway_url_falls_back_to_the_hosted_relay() {
        let d = daemon();
        assert_eq!(
            d.push_gateway_url().as_deref(),
            Some(PUSH_GATEWAY_URL_DEFAULT),
            "a controller that never configured a relay still delivers"
        );
    }

    #[test]
    fn an_empty_gateway_url_disables_push_rather_than_falling_back() {
        let d = daemon();
        d.set_setting(SETTING_PUSH_GATEWAY_URL, Some("")).unwrap();
        assert_eq!(
            d.push_gateway_url(),
            None,
            "clearing the value is how an operator asks to send nothing"
        );

        d.set_setting(SETTING_PUSH_GATEWAY_URL, None).unwrap();
        assert_eq!(
            d.push_gateway_url().as_deref(),
            Some(PUSH_GATEWAY_URL_DEFAULT),
            "unsetting it restores the default"
        );
    }

    #[test]
    fn a_configured_gateway_url_wins_over_the_default() {
        let d = daemon();
        d.set_setting(SETTING_PUSH_GATEWAY_URL, Some("https://relay.example.com"))
            .unwrap();
        assert_eq!(
            d.push_gateway_url().as_deref(),
            Some("https://relay.example.com")
        );
    }

    /// Everything a reader could learn about the session must be
    /// inside the seal. The cleartext payload crosses Apple's
    /// infrastructure, so it carries the sealed blob and nothing else.
    #[test]
    fn a_sealed_notification_leaves_nothing_readable_in_the_cleartext() {
        use base64::engine::{general_purpose::STANDARD as B64, Engine};

        const PROJECT_NAME: &str = "acme-secret-migration";
        const HEADLINE: &str = "rotating the production signing key";
        const STATE_DETAIL: &str = "approve the rollout to eu-west?";

        let d = daemon();
        let bucket = d.storage().create_bucket("b").unwrap();
        let project = d
            .storage()
            .create_project(bucket.id, PROJECT_NAME, "/tmp/p")
            .unwrap();
        let session = d
            .storage()
            .create_session(
                project.id,
                pm_protocol::domain::AgentKind::ClaudeCode,
                "task",
                "prompt",
                pm_protocol::domain::PermissionMode::Bypass,
                0,
                true,
                false,
                None,
                1000,
            )
            .unwrap();
        d.storage()
            .set_headline_summary(session.id, HEADLINE, "")
            .unwrap();
        d.storage()
            .update_session_state(session.id, SessionState::NeedsInput, STATE_DETAIL)
            .unwrap();

        let (secret_key, public_key) = pm_push::hpke::generate_keypair();
        let (user_id, device_id) = enrolled_device(&d);
        let mut previews = registration();
        previews.previews_enabled = true;
        previews.public_key = B64.encode(public_key);
        d.register_push_endpoint(user_id, device_id, previews)
            .unwrap();
        let endpoint = d.storage().get_push_endpoint(device_id).unwrap().unwrap();

        let delivery = NotificationDelivery {
            id: 1,
            event_id: "inst:1:1:needs-input:1".into(),
            device_id,
            session_id: session.id,
            state: PushEventClass::NeedsInput.as_str().into(),
            collapse_id: push_collapse_id(&d.installation_secret(), device_id, session.id),
            status: DELIVERY_PENDING.into(),
            provider_message_id: None,
            attempt_count: 0,
            next_attempt_at_unix_ms: 0,
            last_error: String::new(),
            created_at_unix_ms: 1000,
        };

        let message = d.build_push_message(
            &delivery,
            &endpoint,
            "device-token".into(),
            PushEventClass::NeedsInput,
        );
        assert!(
            message.mutable_content,
            "a usable key must produce a sealed payload"
        );

        let payload = pm_push::apns_payload(&message);
        let cleartext = serde_json::to_string(&payload).unwrap();
        for secret in [PROJECT_NAME, HEADLINE, STATE_DETAIL] {
            assert!(
                !cleartext.contains(secret),
                "{secret:?} reached the cleartext payload: {cleartext}"
            );
        }
        assert_eq!(payload["aps"]["alert"]["title"], PUSH_PLACEHOLDER_TITLE);
        assert_eq!(payload["aps"]["alert"]["body"], PUSH_PLACEHOLDER_BODY);

        let data = payload[PUSH_DATA_KEY].as_object().unwrap();
        assert_eq!(
            data.keys().collect::<Vec<_>>(),
            vec![SEALED_PAYLOAD_KEY],
            "the cleartext dict must carry the blob and nothing else"
        );
        for routing in ["controllerId", "sessionId", "state", "eventId", "url"] {
            assert!(
                !cleartext.contains(routing),
                "{routing} still names a cleartext field: {cleartext}"
            );
        }

        let sealed = B64
            .decode(data[SEALED_PAYLOAD_KEY].as_str().unwrap())
            .unwrap();
        let opened = pm_push::hpke::open(&secret_key, &sealed).expect("device can open the seal");
        let notification: SealedNotification = serde_json::from_slice(&opened).unwrap();
        assert!(
            notification.title.contains(PROJECT_NAME),
            "the seal must still carry the real title: {}",
            notification.title
        );
        assert_eq!(notification.body, STATE_DETAIL);
        assert_eq!(notification.session_id, session.id);
        assert_eq!(notification.event_id, delivery.event_id);
        assert_eq!(
            notification.state,
            PushEventClass::NeedsInput.as_str(),
            "the event class is a fact about the session and belongs inside the seal"
        );
        assert_eq!(
            notification.controller_id,
            push_controller_ref(&d.installation_secret(), device_id)
        );
        assert!(notification.deep_link.contains(&session.id.to_string()));
    }

    /// A send that could not be sealed has no extension to open it, so
    /// falling back to the real content would put on the lock screen
    /// exactly what sealing exists to hide.
    #[test]
    fn an_unsealed_notification_shows_the_placeholder_and_carries_no_routing() {
        const PROJECT_NAME: &str = "acme-secret-migration";
        const HEADLINE: &str = "rotating the production signing key";

        let d = daemon();
        let bucket = d.storage().create_bucket("b").unwrap();
        let project = d
            .storage()
            .create_project(bucket.id, PROJECT_NAME, "/tmp/p")
            .unwrap();
        let session = d
            .storage()
            .create_session(
                project.id,
                pm_protocol::domain::AgentKind::ClaudeCode,
                "task",
                "prompt",
                pm_protocol::domain::PermissionMode::Bypass,
                0,
                true,
                false,
                None,
                1000,
            )
            .unwrap();
        d.storage()
            .set_headline_summary(session.id, HEADLINE, "")
            .unwrap();

        let (user_id, device_id) = enrolled_device(&d);
        let mut unusable = registration();
        unusable.previews_enabled = true;
        unusable.public_key = TEST_UNUSABLE_PUBLIC_KEY.into();
        d.register_push_endpoint(user_id, device_id, unusable)
            .unwrap();
        let endpoint = d.storage().get_push_endpoint(device_id).unwrap().unwrap();

        let delivery = NotificationDelivery {
            id: 1,
            event_id: "inst:1:1:needs-input:1".into(),
            device_id,
            session_id: session.id,
            state: PushEventClass::NeedsInput.as_str().into(),
            collapse_id: push_collapse_id(&d.installation_secret(), device_id, session.id),
            status: DELIVERY_PENDING.into(),
            provider_message_id: None,
            attempt_count: 0,
            next_attempt_at_unix_ms: 0,
            last_error: String::new(),
            created_at_unix_ms: 1000,
        };

        let message = d.build_push_message(
            &delivery,
            &endpoint,
            "device-token".into(),
            PushEventClass::NeedsInput,
        );
        assert!(
            !message.mutable_content,
            "nothing was sealed, so no extension should be woken"
        );
        assert_eq!(message.title, PUSH_PLACEHOLDER_TITLE);
        assert_eq!(message.body, PUSH_PLACEHOLDER_BODY);

        let cleartext = serde_json::to_string(&pm_push::apns_payload(&message)).unwrap();
        for secret in [PROJECT_NAME, HEADLINE, "sessionId", "controllerId"] {
            assert!(
                !cleartext.contains(secret),
                "{secret:?} reached an unsealed payload: {cleartext}"
            );
        }
    }

    fn daemon() -> Daemon {
        let tmp = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward: Default::default(),
            db_path: None,
            socket_path: tmp.path().join("unused.sock"),
            http_addr: None,
            http_tls: None,
            worker_addr: None,
            public_url: None,
            scrollback_dir: tmp.path().join("sb"),
            registry: pm_adapters::AdapterRegistry::empty(),
            local_worker_enabled: true,
            release_channel: None,
        };
        Daemon::new(config).unwrap().0
    }

    fn enrolled_device(d: &Daemon) -> (u64, u64) {
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d
            .mobile_enroll(MobileEnrollRequest {
                proof: MobileEnrollProof::Password {
                    username: "testuser".into(),
                    password: "hunter2hunter2".into(),
                },
                app_installation_id: "app-1".into(),
                name: "phone".into(),
                platform: "ios".into(),
            })
            .unwrap();
        (enrollment.device.user_id, enrollment.device.id)
    }

    fn registration() -> RegisterPushEndpoint {
        RegisterPushEndpoint {
            token: "device-token-1".into(),
            environment: ENVIRONMENT_SANDBOX.into(),
            locale: "en-US".into(),
            previews_enabled: false,
            event_mask: PUSH_EVENTS_ALL_MASK,
            public_key: TEST_DEVICE_PUBLIC_KEY.into(),
        }
    }

    /// A raw 32-byte X25519 public key, base64. Registration requires
    /// one because every notification is sealed to the device.
    const TEST_DEVICE_PUBLIC_KEY: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=";

    /// Base64 of sixteen bytes: non-empty, so registration accepts it,
    /// but not an X25519 key, so nothing can be sealed to it.
    const TEST_UNUSABLE_PUBLIC_KEY: &str = "AQIDBAUGBwgJCgsMDQ4PEA==";

    #[derive(Clone, Default)]
    struct LogCapture(Arc<Mutex<Vec<u8>>>);

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

    /// Runs `body` with this module's logs captured, so a test can
    /// assert on what an operator would actually see. The subscriber is
    /// scoped to the calling thread, which is why the delivery pass
    /// runs on a current-thread runtime.
    fn with_captured_logs<T>(body: impl FnOnce() -> T) -> (T, String) {
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::new("pm_daemon::push=debug"))
            .finish();
        let out = tracing::subscriber::with_default(subscriber, body);
        (out, capture.text())
    }

    fn drain_deliveries(d: &Daemon, now: i64) -> (PushDeliveryStats, String) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        with_captured_logs(|| rt.block_on(d.process_push_deliveries(now)))
    }

    fn queue_one_delivery(d: &Daemon, device_id: u64, at: i64) {
        d.storage()
            .enqueue_notification_delivery(
                "evt-1",
                device_id,
                7,
                PushEventClass::NeedsInput.as_str(),
                "collapse-1",
                DELIVERY_PENDING,
                at,
                at,
            )
            .unwrap();
    }

    /// The drop was silent, which is why notifications disappeared with
    /// nothing for an operator to look at.
    #[test]
    fn a_delivery_to_a_disabled_endpoint_says_the_device_and_the_reason() {
        let d = daemon();
        let (user_id, device_id) = enrolled_device(&d);
        d.register_push_endpoint(user_id, device_id, registration())
            .unwrap();
        queue_one_delivery(&d, device_id, 10);
        d.storage()
            .disable_push_endpoint(device_id, DISABLE_REASON_UNREGISTERED, 15)
            .unwrap();

        let (stats, logs) = drain_deliveries(&d, 20);

        assert_eq!(stats.attempted, 1);
        assert_eq!(stats.sent, 0);
        assert!(
            logs.contains("push delivery dropped: endpoint is disabled"),
            "the drop must name itself:\n{logs}"
        );
        assert!(
            logs.contains(&format!("device={device_id}")),
            "the drop must name the device:\n{logs}"
        );
        assert!(
            logs.contains(DISABLE_REASON_UNREGISTERED),
            "the drop must name the disable reason:\n{logs}"
        );
    }

    #[test]
    fn a_delivery_to_a_removed_endpoint_says_so() {
        let d = daemon();
        let (_, device_id) = enrolled_device(&d);
        queue_one_delivery(&d, device_id, 10);

        let (stats, logs) = drain_deliveries(&d, 20);

        assert_eq!(stats.attempted, 1);
        assert!(
            logs.contains("push delivery dropped: endpoint is gone"),
            "{logs}"
        );
    }

    /// Without a key nothing can be sealed, so the endpoint used to fail
    /// every notification forever while `disabled` stayed false.
    #[test]
    fn an_endpoint_with_no_usable_public_key_is_disabled() {
        let d = daemon();
        let (user_id, device_id) = enrolled_device(&d);
        let mut unusable = registration();
        unusable.public_key = TEST_UNUSABLE_PUBLIC_KEY.into();
        d.register_push_endpoint(user_id, device_id, unusable)
            .unwrap();
        queue_one_delivery(&d, device_id, 10);

        let (stats, logs) = drain_deliveries(&d, 20);
        assert_eq!(stats.attempted, 1);
        assert_eq!(stats.sent, 0);

        let endpoint = d
            .storage()
            .get_push_endpoint(device_id)
            .unwrap()
            .expect("the endpoint is still on record");
        assert!(
            endpoint.disabled_at_unix_ms.is_some(),
            "an endpoint that cannot be sealed to must stop being retried"
        );
        assert_eq!(endpoint.disabled_reason, DISABLE_REASON_NO_PUBLIC_KEY);
        assert!(
            logs.contains(DISABLE_REASON_NO_PUBLIC_KEY),
            "the disable must name its reason:\n{logs}"
        );

        queue_one_delivery(&d, device_id, 30);
        let (stats, _) = drain_deliveries(&d, 40);
        assert_eq!(
            stats.sent, 0,
            "a disabled endpoint keeps quiet rather than retrying forever"
        );
    }

    #[test]
    fn an_endpoint_with_a_usable_key_is_left_alone() {
        let d = daemon();
        let (user_id, device_id) = enrolled_device(&d);
        d.register_push_endpoint(user_id, device_id, registration())
            .unwrap();
        queue_one_delivery(&d, device_id, 10);

        let (_, logs) = drain_deliveries(&d, 20);

        let endpoint = d.storage().get_push_endpoint(device_id).unwrap().unwrap();
        assert!(
            endpoint.disabled_at_unix_ms.is_none(),
            "a sealable endpoint must not be disabled"
        );
        assert!(!logs.contains(DISABLE_REASON_NO_PUBLIC_KEY), "{logs}");
    }

    /// The browser is told by the same judgement that gates push, and a
    /// fleet with no phone registered still raises alerts: the endpoint
    /// check used to run first and swallowed the whole decision.
    #[test]
    fn an_alert_is_raised_for_a_connected_client_without_any_push_endpoint() {
        use pm_protocol::domain::{AgentKind, PermissionMode};

        let d = daemon();
        let bucket = d.storage().create_bucket("b").unwrap();
        let project = d
            .storage()
            .create_project(bucket.id, "p", "/tmp/p")
            .unwrap();
        let session = d
            .storage()
            .create_session(
                project.id,
                AgentKind::ClaudeCode,
                "task",
                "prompt",
                PermissionMode::Bypass,
                0,
                true,
                false,
                None,
                1000,
            )
            .unwrap();
        assert!(
            d.storage().active_push_endpoints().unwrap().is_empty(),
            "this test is about the no-endpoint path"
        );

        let alert = d.observe_push_transition(
            &session,
            bucket.id,
            1,
            SessionState::Working,
            SessionState::NeedsInput,
        );

        assert_eq!(
            alert,
            Some(SessionAlert {
                session_id: session.id,
                kind: SessionAlertKind::NeedsInput,
            })
        );
    }

    /// A transition the classifier does not consider alert-worthy raises
    /// nothing, so a client never has to second-guess what it is sent.
    #[test]
    fn an_unclassified_transition_raises_no_alert() {
        use pm_protocol::domain::{AgentKind, PermissionMode};

        let d = daemon();
        let bucket = d.storage().create_bucket("b").unwrap();
        let project = d
            .storage()
            .create_project(bucket.id, "p", "/tmp/p")
            .unwrap();
        let session = d
            .storage()
            .create_session(
                project.id,
                AgentKind::ClaudeCode,
                "task",
                "prompt",
                PermissionMode::Bypass,
                0,
                true,
                false,
                None,
                1000,
            )
            .unwrap();

        let alert = d.observe_push_transition(
            &session,
            bucket.id,
            1,
            SessionState::Starting,
            SessionState::Working,
        );

        assert_eq!(alert, None);
    }

    /// Every gate below is correct behaviour, and each was previously
    /// indistinguishable from a push that was simply lost.
    #[test]
    fn a_declined_notification_names_the_gate_that_declined_it() {
        use pm_protocol::domain::{AgentKind, PermissionMode};

        let d = daemon();
        let bucket = d.storage().create_bucket("b").unwrap();
        let project = d
            .storage()
            .create_project(bucket.id, "p", "/tmp/p")
            .unwrap();
        let session = |supervised: Option<u64>| {
            d.storage()
                .create_session(
                    project.id,
                    AgentKind::ClaudeCode,
                    "task",
                    "prompt",
                    PermissionMode::Bypass,
                    0,
                    true,
                    false,
                    supervised,
                    1000,
                )
                .unwrap()
        };

        let plain = session(None);
        let (_, logs) = with_captured_logs(|| {
            d.observe_push_transition(
                &plain,
                bucket.id,
                1,
                SessionState::Working,
                SessionState::NeedsInput,
            )
        });
        assert!(
            logs.contains(GATE_NO_ENDPOINTS),
            "a controller with no registered device must say so:\n{logs}"
        );

        let (user_id, device_id) = enrolled_device(&d);
        d.register_push_endpoint(user_id, device_id, registration())
            .unwrap();

        let (_, logs) = with_captured_logs(|| {
            d.observe_push_transition(
                &plain,
                bucket.id,
                1,
                SessionState::Idle,
                SessionState::Working,
            )
        });
        assert!(
            logs.contains(GATE_CLASSIFICATION),
            "a transition that is not a notifiable event must say so:\n{logs}"
        );

        let supervised = session(Some(plain.id));
        let (_, logs) = with_captured_logs(|| {
            d.observe_push_transition(
                &supervised,
                bucket.id,
                1,
                SessionState::Working,
                SessionState::NeedsInput,
            )
        });
        assert!(
            logs.contains(GATE_SCOPE),
            "the default scope silences supervised workers, and must say so:\n{logs}"
        );

        d.set_push_policy(bucket.id, "", "completed", "all")
            .unwrap();
        let (_, logs) = with_captured_logs(|| {
            d.observe_push_transition(
                &plain,
                bucket.id,
                1,
                SessionState::Working,
                SessionState::NeedsInput,
            )
        });
        assert!(
            logs.contains(GATE_EVENT_MASK),
            "an event the policy does not carry must say so:\n{logs}"
        );

        d.set_push_policy(bucket.id, "", "needs-input,failed,completed", "all")
            .unwrap();
        let queue_twice = || {
            d.observe_push_transition(
                &plain,
                bucket.id,
                2,
                SessionState::Working,
                SessionState::NeedsInput,
            )
        };
        let (_, first) = with_captured_logs(queue_twice);
        assert!(
            !first.contains("push event declined"),
            "the first queueing passes every gate:\n{first}"
        );
        let (_, second) = with_captured_logs(queue_twice);
        assert!(
            second.contains(GATE_DEDUPE),
            "the same event twice must name dedupe:\n{second}"
        );

        d.note_web_activity(user_id);
        let (_, logs) = with_captured_logs(|| {
            d.observe_push_transition(
                &plain,
                bucket.id,
                3,
                SessionState::Working,
                SessionState::NeedsInput,
            )
        });
        assert!(
            logs.contains(GATE_WEB_ACTIVE),
            "a user at the web UI must say so:\n{logs}"
        );
    }

    #[test]
    fn a_usable_key_is_the_right_length_and_base64() {
        let usable = |key: &str| {
            let mut endpoint = PushEndpoint {
                device_id: 1,
                token_ciphertext: String::new(),
                environment: ENVIRONMENT_SANDBOX.into(),
                locale: String::new(),
                previews_enabled: false,
                event_mask: PUSH_EVENTS_ALL_MASK,
                public_key: String::new(),
                push_counter: 0,
                created_at_unix_ms: 0,
                updated_at_unix_ms: 0,
                disabled_at_unix_ms: None,
                disabled_reason: String::new(),
            };
            endpoint.public_key = key.to_string();
            seals_to_a_usable_key(&endpoint)
        };
        assert!(usable(TEST_DEVICE_PUBLIC_KEY));
        assert!(!usable(""), "an endpoint that never registered a key");
        assert!(
            !usable(TEST_UNUSABLE_PUBLIC_KEY),
            "sixteen bytes is not a key"
        );
        assert!(!usable("not base64 at all"));
    }

    #[test]
    fn classify_needs_input_and_failed() {
        assert_eq!(
            classify_transition(SessionState::Working, SessionState::NeedsInput, "q"),
            Some(PushEventClass::NeedsInput)
        );
        assert_eq!(
            classify_transition(SessionState::Working, SessionState::Failed, "boom"),
            Some(PushEventClass::Failed)
        );
        assert_eq!(
            classify_transition(SessionState::Starting, SessionState::Working, ""),
            None
        );
        assert_eq!(
            classify_transition(SessionState::Working, SessionState::Exited, ""),
            None,
            "clean exits never notify"
        );
    }

    /// The watchdog's inferred stop must never reach a user's phone as a
    /// finished turn: nothing was completed, the end was guessed.
    #[test]
    fn an_inferred_turn_end_is_not_a_completion() {
        for detail in [
            crate::stale_turn::INFERRED_IDLE_DETAIL,
            crate::stale_turn::HOOKS_UNOBSERVED_DETAIL,
        ] {
            assert_eq!(
                classify_transition(SessionState::Working, SessionState::Idle, detail),
                None,
                "{detail}"
            );
        }
    }

    #[test]
    fn a_clean_turn_end_completes_without_a_checkpoint() {
        assert_eq!(
            classify_transition(SessionState::Working, SessionState::Idle, ""),
            Some(PushEventClass::Completed),
            "a turn that answered only in text is finished work"
        );
        assert_eq!(
            classify_transition(SessionState::Working, SessionState::Idle, "turn failed"),
            None,
            "a failed turn carries a detail and is not completed work"
        );
        assert_eq!(
            classify_transition(
                SessionState::Working,
                SessionState::Idle,
                crate::daemon::INTERRUPTED_DETAIL
            ),
            None,
            "an interrupted turn is not completed work"
        );
        assert_eq!(
            classify_transition(SessionState::NeedsInput, SessionState::Idle, ""),
            None,
            "only working-to-idle can complete"
        );
    }

    #[test]
    fn scope_defaults_silence_supervised_workers() {
        for (scope, role, supervised, expected) in [
            (PushScope::Default, SessionRole::Supervisor, false, true),
            (PushScope::Default, SessionRole::Supervisor, true, true),
            (PushScope::Default, SessionRole::Worker, false, true),
            (PushScope::Default, SessionRole::Worker, true, false),
            (PushScope::All, SessionRole::Worker, true, true),
            (PushScope::Supervisors, SessionRole::Worker, false, false),
            (PushScope::Supervisors, SessionRole::Supervisor, false, true),
            (PushScope::None, SessionRole::Supervisor, false, false),
        ] {
            assert_eq!(
                scope_allows(scope, role, supervised),
                expected,
                "{scope:?} {role:?} supervised={supervised}"
            );
        }
    }

    #[test]
    fn events_csv_parses_and_rejects() {
        assert_eq!(parse_events_csv(""), Some(0));
        assert_eq!(
            parse_events_csv("needs-input,failed,completed"),
            Some(PUSH_EVENTS_ALL_MASK)
        );
        assert_eq!(
            parse_events_csv("failed"),
            Some(PushEventClass::Failed.mask())
        );
        assert_eq!(parse_events_csv("needs-input, completed"), Some(1 | 4));
        assert_eq!(parse_events_csv("bogus"), None);
    }

    #[test]
    fn event_ids_are_durable_and_distinct() {
        let id = push_event_id("inst", 7, 3, PushEventClass::NeedsInput, 12);
        assert_eq!(id, "inst:7:3:needs-input:12");
        assert_ne!(
            id,
            push_event_id("inst", 7, 3, PushEventClass::NeedsInput, 13)
        );
        let secret = "installation-secret";
        let id = push_collapse_id(secret, 3, 7);
        assert_ne!(id, "pm-session-7", "the session id must not be readable");
        assert_eq!(id.len(), OPAQUE_ID_BYTES * 2);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id, push_collapse_id(secret, 3, 7), "stable for one session");
        assert_ne!(
            id,
            push_collapse_id(secret, 3, 8),
            "distinct between sessions, or a later state would replace the wrong alert"
        );
        assert_ne!(
            id,
            push_collapse_id(secret, 4, 7),
            "two devices must not be able to correlate one session"
        );
        assert_ne!(
            push_controller_ref(secret, 3),
            push_controller_ref(secret, 4),
            "the controller reference is per device too"
        );
        assert_ne!(
            id,
            opaque_id(secret, "controller", 3, 7),
            "labels keep uses from being compared"
        );
    }

    #[test]
    fn message_content_hides_details_unless_previews_enabled() {
        for class in [
            PushEventClass::NeedsInput,
            PushEventClass::Failed,
            PushEventClass::Completed,
        ] {
            let (title, body) =
                message_content(class, false, "proj", "fixing the bug", "Merge now?");
            assert!(title.starts_with("Session "));
            assert!(body.is_empty());
        }
        let (title, body) = message_content(
            PushEventClass::NeedsInput,
            false,
            "proj",
            "fixing the bug",
            "",
        );
        assert_eq!(title, "Session needs input");
        assert!(body.is_empty());
        let (title, body) =
            message_content(PushEventClass::Completed, true, "", "fixed the bug", "");
        assert_eq!(title, "Session completed work");
        assert!(body.is_empty());
    }

    #[test]
    fn message_content_needs_input_body_is_the_question() {
        let (title, body) = message_content(
            PushEventClass::NeedsInput,
            true,
            "proj",
            "fixing the bug",
            "Merge the fix into master?",
        );
        assert_eq!(title, "proj: session needs input");
        assert_eq!(body, "Merge the fix into master?");
    }

    #[test]
    fn message_content_failed_body_is_the_failure_detail() {
        let (title, body) = message_content(
            PushEventClass::Failed,
            true,
            "proj",
            "fixing the bug",
            "worker did not reconnect",
        );
        assert_eq!(title, "proj: session failed");
        assert_eq!(body, "worker did not reconnect");
    }

    #[test]
    fn message_content_completed_body_is_the_headline() {
        let (title, body) = message_content(
            PushEventClass::Completed,
            true,
            "proj",
            "fixed the bug",
            "stale detail",
        );
        assert_eq!(title, "proj: session completed work");
        assert_eq!(body, "fixed the bug");
    }

    #[test]
    fn message_content_falls_back_to_headline_without_detail() {
        for class in [PushEventClass::NeedsInput, PushEventClass::Failed] {
            for detail in ["", "  \n "] {
                let (_, body) = message_content(class, true, "proj", "fixing the bug", detail);
                assert_eq!(body, "fixing the bug");
            }
        }
    }

    #[test]
    fn message_content_truncates_long_bodies_on_a_char_boundary() {
        let fits = "q".repeat(PUSH_BODY_MAX_CHARS);
        let (_, body) = message_content(PushEventClass::NeedsInput, true, "proj", "", &fits);
        assert_eq!(body, fits);

        let long = "é".repeat(PUSH_BODY_MAX_CHARS + 20);
        let (_, body) = message_content(PushEventClass::Failed, true, "proj", "", &long);
        assert_eq!(body.chars().count(), PUSH_BODY_MAX_CHARS);
        assert!(body.ends_with('…'));
        assert!(body.starts_with(&"é".repeat(PUSH_BODY_MAX_CHARS - 1)));
    }

    #[test]
    fn message_content_unescapes_html_entities_in_title_and_body() {
        let (title, body) = message_content(
            PushEventClass::NeedsInput,
            true,
            "Auth &amp; Core",
            "unused headline",
            "Should we deploy &lt;v1&gt; &amp; &apos;v2&apos;?",
        );
        assert_eq!(title, "Auth & Core: session needs input");
        assert_eq!(body, "Should we deploy <v1> & 'v2'?");

        let (title, body) = message_content(
            PushEventClass::Completed,
            true,
            "App",
            "Building &amp; testing &#8212; 5/5",
            "",
        );
        assert_eq!(title, "App: session completed work");
        assert_eq!(body, "Building & testing — 5/5");
    }

    #[test]
    fn connection_approval_push_uses_needs_input_preferences_and_deduplicates() {
        use pm_protocol::domain::{AgentKind, PermissionMode};
        let d = daemon();
        let (user_id, device_id) = enrolled_device(&d);
        d.register_push_endpoint(user_id, device_id, registration())
            .unwrap();
        let bucket = d.storage().create_bucket("connections").unwrap();
        let project = d
            .storage()
            .create_project(bucket.id, "project", "/tmp/connection-push")
            .unwrap();
        let session = d
            .storage()
            .create_session(
                project.id,
                AgentKind::ClaudeCode,
                "task",
                "prompt",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                now_unix_ms(),
            )
            .unwrap();
        let call: crate::connections::Call = serde_json::from_value(serde_json::json!({
            "id":"pending-call", "request_id":"request", "session_id":session.id, "project_id":project.id,
            "connection_id":1, "connection_revision":1, "tool":"write", "arguments":{}, "justification":"Update record",
            "status":"pending", "created_at":now_unix_ms(), "expires_at":now_unix_ms()+60_000,
            "decided_by":null, "result":null, "error":null
        })).unwrap();
        d.queue_connection_approval_push(&call);
        d.queue_connection_approval_push(&call);
        let deliveries = d
            .storage()
            .due_notification_deliveries(now_unix_ms() + 1, DELIVERY_BATCH)
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].state, "connection-approval");
        assert_eq!(deliveries[0].session_id, session.id);
        assert_eq!(
            PushEventClass::ConnectionApproval.mask(),
            PushEventClass::NeedsInput.mask()
        );
        assert_eq!(
            PushEventClass::parse("connection-approval"),
            Some(PushEventClass::ConnectionApproval)
        );
        let (_, body) = message_content(
            PushEventClass::ConnectionApproval,
            true,
            "Project",
            "",
            "Review in PM",
        );
        assert_eq!(body, "Review in PM");
    }

    /// Nothing about the call, its id included, may reach the cleartext or the collapse id.
    #[test]
    fn an_approval_notification_seals_its_id_and_leaves_no_trace_in_the_cleartext() {
        use base64::engine::{general_purpose::STANDARD as B64, Engine};
        use pm_protocol::domain::{AgentKind, PermissionMode};

        const CONNECTION_NAME: &str = "billing-ledger";
        const TOOL: &str = "refund_customer_payment";
        const JUSTIFICATION: &str = "Refund duplicate charge";
        const ARGUMENT: &str = "cus_sensitive_4242";

        let d = daemon();
        let bucket = d.storage().create_bucket("b").unwrap();
        let project = d
            .storage()
            .create_project(bucket.id, "payments", "/tmp/approval-seal")
            .unwrap();
        let session = d
            .storage()
            .create_session(
                project.id,
                AgentKind::ClaudeCode,
                "task",
                "prompt",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                now_unix_ms(),
            )
            .unwrap();
        let config: crate::connections::Config = serde_json::from_value(serde_json::json!({
            "name": CONNECTION_NAME, "project_id": project.id,
            "endpoint": format!("http://{}", std::net::Ipv4Addr::LOCALHOST),
        }))
        .unwrap();
        let connection = d
            .storage()
            .create_connection(config, Some(session.id))
            .unwrap();
        let call: crate::connections::Call = serde_json::from_value(serde_json::json!({
            "id": "c".repeat(64), "request_id": "refund", "session_id": session.id,
            "project_id": project.id, "connection_id": connection.id,
            "connection_revision": connection.revision, "tool": TOOL,
            "arguments": {"customer": ARGUMENT}, "justification": JUSTIFICATION,
            "status": "pending", "created_at": now_unix_ms(),
            "expires_at": now_unix_ms() + 60_000, "decided_by": null, "result": null,
            "error": null, "requires_approval": true
        }))
        .unwrap();
        let (call, _) = d.storage().create_connection_call(call).unwrap();

        let (secret_key, public_key) = pm_push::hpke::generate_keypair();
        let (user_id, device_id) = enrolled_device(&d);
        let mut previews = registration();
        previews.previews_enabled = true;
        previews.public_key = B64.encode(public_key);
        d.register_push_endpoint(user_id, device_id, previews)
            .unwrap();
        let endpoint = d.storage().get_push_endpoint(device_id).unwrap().unwrap();

        d.queue_connection_approval_push(&call);
        let delivery = d
            .storage()
            .due_notification_deliveries(now_unix_ms() + 1, DELIVERY_BATCH)
            .unwrap()
            .pop()
            .expect("the approval was queued");
        let message = d.build_push_message(
            &delivery,
            &endpoint,
            "device-token".into(),
            PushEventClass::ConnectionApproval,
        );
        assert!(message.mutable_content);

        let payload = pm_push::apns_payload(&message);
        let cleartext = serde_json::to_string(&payload).unwrap();
        for secret in [
            call.id.as_str(),
            TOOL,
            CONNECTION_NAME,
            JUSTIFICATION,
            ARGUMENT,
            "approval",
            "payments",
        ] {
            assert!(
                !cleartext.contains(secret),
                "{secret:?} reached the cleartext payload: {cleartext}"
            );
            assert!(
                !message.collapse_id.contains(secret),
                "{secret:?} reached the collapse id"
            );
        }
        let data = payload[PUSH_DATA_KEY].as_object().unwrap();
        assert_eq!(data.keys().collect::<Vec<_>>(), vec![SEALED_PAYLOAD_KEY]);

        let sealed = B64
            .decode(data[SEALED_PAYLOAD_KEY].as_str().unwrap())
            .unwrap();
        let opened = pm_push::hpke::open(&secret_key, &sealed).expect("device can open the seal");
        let notification: SealedNotification = serde_json::from_slice(&opened).unwrap();
        assert_eq!(notification.approval_id.as_deref(), Some(call.id.as_str()));
        assert_eq!(
            notification.state,
            PushEventClass::ConnectionApproval.as_str()
        );
        assert_eq!(notification.session_id, session.id);
        assert!(
            notification
                .deep_link
                .ends_with(&format!("/session/{}", session.id)),
            "an app that predates approvals still opens the session: {}",
            notification.deep_link
        );
        assert!(notification.body.contains(TOOL), "{}", notification.body);
        assert!(
            !String::from_utf8_lossy(&opened).contains(ARGUMENT),
            "arguments are reviewed in the app, never carried in a notification"
        );
    }

    /// Without previews the alert is generic, yet the sealed id still routes the tap.
    #[test]
    fn an_approval_without_previews_is_generic_but_still_routes() {
        assert_eq!(
            approval_id(
                PushEventClass::ConnectionApproval,
                "connection-approval:abc"
            )
            .as_deref(),
            Some("abc")
        );
        assert_eq!(
            approval_id(PushEventClass::NeedsInput, "connection-approval:abc"),
            None
        );
        assert_eq!(
            approval_id(PushEventClass::ConnectionApproval, "connection-approval:"),
            None
        );
        let (title, body) = message_content(
            PushEventClass::ConnectionApproval,
            false,
            "payments",
            "",
            "Review refund_customer_payment on billing-ledger in Approvals.",
        );
        assert_eq!(title, "Session requires connection approval");
        assert!(body.is_empty());
    }

    #[test]
    fn registration_validates_rotates_and_reenables() {
        let d = daemon();
        let (user_id, device_id) = enrolled_device(&d);

        let mut bad = registration();
        bad.public_key = String::new();
        assert!(matches!(
            d.register_push_endpoint(user_id, device_id, bad),
            Err(MobileAuthError::Rejected(_))
        ));
        let mut bad = registration();
        bad.environment = "staging".into();
        assert!(matches!(
            d.register_push_endpoint(user_id, device_id, bad),
            Err(MobileAuthError::Rejected(_))
        ));
        let mut bad = registration();
        bad.token = " ".into();
        assert!(matches!(
            d.register_push_endpoint(user_id, device_id, bad),
            Err(MobileAuthError::Rejected(_))
        ));
        assert!(matches!(
            d.register_push_endpoint(user_id + 1, device_id, registration()),
            Err(MobileAuthError::DeviceNotFound)
        ));

        let endpoint = d
            .register_push_endpoint(user_id, device_id, registration())
            .unwrap();
        assert!(!endpoint.token_ciphertext.contains("device-token-1"));
        let secret = d.installation_secret();
        assert_eq!(
            open_secret(&secret, &endpoint.token_ciphertext).as_deref(),
            Some("device-token-1")
        );

        d.storage()
            .disable_push_endpoint(device_id, DISABLE_REASON_UNREGISTERED, 5)
            .unwrap();
        let mut rotated = registration();
        rotated.token = "device-token-2".into();
        rotated.event_mask = PushEventClass::Failed.mask();
        let endpoint = d
            .register_push_endpoint(user_id, device_id, rotated)
            .unwrap();
        assert!(
            endpoint.disabled_at_unix_ms.is_none(),
            "rotation re-enables"
        );
        assert_eq!(endpoint.event_mask, PushEventClass::Failed.mask());
        assert_eq!(
            open_secret(&secret, &endpoint.token_ciphertext).as_deref(),
            Some("device-token-2")
        );
    }

    #[test]
    fn policy_resolution_prefers_the_most_specific_row() {
        let d = daemon();
        let bucket = d.storage().create_bucket("push-bucket").unwrap();

        assert_eq!(
            d.resolve_push_policy(bucket.id, SessionRole::Worker),
            (PUSH_EVENTS_ALL_MASK, PushScope::Default),
            "controller defaults apply with no rows"
        );

        d.set_setting(SETTING_PUSH_EVENTS, Some("failed")).unwrap();
        d.set_setting(SETTING_PUSH_SCOPE, Some("supervisors"))
            .unwrap();
        assert_eq!(
            d.resolve_push_policy(bucket.id, SessionRole::Worker),
            (PushEventClass::Failed.mask(), PushScope::Supervisors)
        );

        d.set_push_policy(bucket.id, "", "needs-input", "all")
            .unwrap();
        assert_eq!(
            d.resolve_push_policy(bucket.id, SessionRole::Worker),
            (PushEventClass::NeedsInput.mask(), PushScope::All)
        );

        d.set_push_policy(bucket.id, "worker", "completed", "none")
            .unwrap();
        assert_eq!(
            d.resolve_push_policy(bucket.id, SessionRole::Worker),
            (PushEventClass::Completed.mask(), PushScope::None),
            "the role-specific row wins for that role"
        );
        assert_eq!(
            d.resolve_push_policy(bucket.id, SessionRole::Supervisor),
            (PushEventClass::NeedsInput.mask(), PushScope::All),
            "other roles keep the bucket-wide row"
        );

        assert!(d.delete_push_policy(bucket.id, "worker").unwrap());
        assert_eq!(
            d.resolve_push_policy(bucket.id, SessionRole::Worker),
            (PushEventClass::NeedsInput.mask(), PushScope::All)
        );

        assert!(matches!(
            d.set_push_policy(bucket.id, "boss", "failed", "all"),
            Err(DaemonError::Rejected(_))
        ));
        assert!(matches!(
            d.set_push_policy(bucket.id, "", "bogus", "all"),
            Err(DaemonError::Rejected(_))
        ));
        assert!(matches!(
            d.set_push_policy(bucket.id, "", "failed", "sometimes"),
            Err(DaemonError::Rejected(_))
        ));
        assert!(matches!(
            d.set_push_policy(bucket.id + 99, "", "failed", "all"),
            Err(DaemonError::Storage(_))
        ));
    }
}
