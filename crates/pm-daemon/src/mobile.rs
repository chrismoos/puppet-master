//! Mobile device auth: revocable per-device credentials for the
//! mobile app. A device enrolls once with a password or a one-use
//! enrollment token, then holds a short-lived bearer access token and
//! a rotating refresh token. Tokens are stored only as sha256 hashes;
//! refresh-token reuse revokes the whole device token family.

use rand::rngs::OsRng;
use rand::RngCore;
use tracing::{info, warn};

use crate::auth::{generate_token, hash_token, AuthError};
use crate::daemon::{now_unix_ms, Daemon};
use crate::storage::{MobileDevice, SocketTicket, StorageError, TerminalTicketBinding};

/// Settings key: minutes a bearer access token stays valid. Governs the
/// dashboard's token as well as a device's, under the name it was first given.
pub const SETTING_MOBILE_ACCESS_TTL_MINUTES: &str = "mobile.access_token_ttl_minutes";
pub const MOBILE_ACCESS_TTL_MINUTES_DEFAULT: i64 = 15;

/// Settings key: days a mobile refresh token stays valid. Each
/// rotation issues a fresh token, so the family lives while the device
/// refreshes at least once per window.
pub const SETTING_MOBILE_REFRESH_TTL_DAYS: &str = "mobile.refresh_token_ttl_days";
pub const MOBILE_REFRESH_TTL_DAYS_DEFAULT: i64 = 90;

/// Settings key: minutes a one-use enrollment token stays valid.
pub const SETTING_MOBILE_ENROLL_TTL_MINUTES: &str = "mobile.enrollment_token_ttl_minutes";
pub const MOBILE_ENROLL_TTL_MINUTES_DEFAULT: i64 = 10;

/// Settings key: seconds a one-use socket ticket stays valid between
/// its HTTPS mint and the WebSocket upgrade that consumes it.
pub const SETTING_MOBILE_SOCKET_TICKET_TTL_SECONDS: &str = "mobile.socket_ticket_ttl_seconds";
pub const MOBILE_SOCKET_TICKET_TTL_SECONDS_DEFAULT: i64 = 30;

/// Replay bound applied when an attach-ticket request does not ask for
/// one, matching the browser terminal replay cap.
pub const TERMINAL_TICKET_REPLAY_DEFAULT_BYTES: u64 = 256 * 1024;

/// Largest replay a terminal attach ticket may request.
pub const TERMINAL_TICKET_REPLAY_MAX_BYTES: u64 = 1024 * 1024;

/// Settings key for the stable installation id. Deliberately not in
/// `KNOWN_SETTINGS`: it identifies this installation to mobile clients
/// and must not be edited or reset through the settings API.
const INSTALLATION_ID_KEY: &str = "installation.id";

const INSTALLATION_ID_BYTES: usize = 16;

pub const MOBILE_DEVICE_NAME_MAX: usize = 80;
pub const MOBILE_DEVICE_PLATFORM_MAX: usize = 40;
pub const MOBILE_APP_INSTALLATION_ID_MAX: usize = 128;

const SECOND_MS: i64 = 1000;
const MINUTE_MS: i64 = 60 * 1000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MobileAuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("{0}")]
    InvalidEnrollment(String),
    #[error("invalid or expired token")]
    InvalidToken,
    #[error("{0}")]
    Rejected(String),
    #[error("mobile device not found")]
    DeviceNotFound,
    #[error("terminal not found")]
    TerminalNotFound,
    #[error("terminal generation changed")]
    StaleGeneration,
    #[error("internal auth error")]
    Internal,
}

impl From<AuthError> for MobileAuthError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::InvalidCredentials => MobileAuthError::InvalidCredentials,
            _ => MobileAuthError::Internal,
        }
    }
}

impl From<StorageError> for MobileAuthError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::NotFound(..) => MobileAuthError::DeviceNotFound,
            StorageError::Conflict(msg) => MobileAuthError::InvalidEnrollment(msg),
            _ => MobileAuthError::Internal,
        }
    }
}

/// How a device proves the user's identity when enrolling.
pub enum MobileEnrollProof {
    Password { username: String, password: String },
    EnrollToken(String),
}

pub struct MobileEnrollRequest {
    pub proof: MobileEnrollProof,
    /// App-generated stable id for this installation.
    pub app_installation_id: String,
    pub name: String,
    pub platform: String,
}

/// Raw tokens returned to the device exactly once; only hashes are stored.
pub struct MobileTokens {
    pub access_token: String,
    pub access_expires_at_unix_ms: i64,
    pub refresh_token: String,
    pub refresh_expires_at_unix_ms: i64,
}

pub struct MobileEnrollment {
    pub device: MobileDevice,
    pub tokens: MobileTokens,
}

impl Daemon {
    /// The stable id mobile clients use to recognize this installation
    /// across URL changes. Generated once and then permanent.
    pub fn installation_id(&self) -> String {
        let mut bytes = [0u8; INSTALLATION_ID_BYTES];
        OsRng.fill_bytes(&mut bytes);
        self.storage()
            .ensure_setting(INSTALLATION_ID_KEY, &hex::encode(bytes))
            .unwrap_or_default()
    }

    fn mobile_ttl_ms(&self, key: &str, default: i64, unit_ms: i64) -> i64 {
        let value = self
            .storage()
            .get_setting(key)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(default);
        value.saturating_mul(unit_ms)
    }

    /// Mints a one-use, short-lived enrollment token. Returns the raw
    /// token (shown once) and its expiry.
    pub fn create_mobile_enrollment_token(
        &self,
        user_id: u64,
    ) -> Result<(String, i64), MobileAuthError> {
        let token = generate_token();
        let now = now_unix_ms();
        let expires = now
            + self.mobile_ttl_ms(
                SETTING_MOBILE_ENROLL_TTL_MINUTES,
                MOBILE_ENROLL_TTL_MINUTES_DEFAULT,
                MINUTE_MS,
            );
        self.storage()
            .create_mobile_enrollment_token(&hash_token(&token), user_id, now, expires)
            .map_err(|_| MobileAuthError::Internal)?;
        info!(user = user_id, "mobile enrollment token minted");
        Ok((token, expires))
    }

    /// Registers a device after verifying either a password login or a
    /// one-use enrollment token, and issues its first token pair. An
    /// installation that is already enrolled for this user keeps its
    /// device record: logging out and back in correlates onto the same
    /// row instead of adding another.
    pub fn mobile_enroll(
        &self,
        request: MobileEnrollRequest,
    ) -> Result<MobileEnrollment, MobileAuthError> {
        let app_installation_id = request.app_installation_id.trim();
        if app_installation_id.is_empty() {
            return Err(MobileAuthError::Rejected(
                "deviceId must not be empty".into(),
            ));
        }
        if app_installation_id.chars().count() > MOBILE_APP_INSTALLATION_ID_MAX {
            return Err(MobileAuthError::Rejected(format!(
                "deviceId must be at most {MOBILE_APP_INSTALLATION_ID_MAX} characters"
            )));
        }
        let name = request.name.trim();
        if name.chars().count() > MOBILE_DEVICE_NAME_MAX {
            return Err(MobileAuthError::Rejected(format!(
                "name must be at most {MOBILE_DEVICE_NAME_MAX} characters"
            )));
        }
        let platform = request.platform.trim();
        if platform.chars().count() > MOBILE_DEVICE_PLATFORM_MAX {
            return Err(MobileAuthError::Rejected(format!(
                "platform must be at most {MOBILE_DEVICE_PLATFORM_MAX} characters"
            )));
        }
        let user_id = match request.proof {
            MobileEnrollProof::Password { username, password } => {
                self.verify_password(&username, &password)?
            }
            MobileEnrollProof::EnrollToken(token) => self
                .storage()
                .consume_mobile_enrollment_token(&hash_token(&token), now_unix_ms())?,
        };
        let (device, reused) = self.storage().enroll_mobile_device(
            user_id,
            name,
            platform,
            app_installation_id,
            &generate_token(),
            now_unix_ms(),
        )?;
        let tokens = self.issue_mobile_tokens(device.id, device.user_id)?;
        if reused {
            info!(
                device = device.id,
                user = user_id,
                "mobile device re-enrolled, previous credentials dropped"
            );
        } else {
            info!(device = device.id, user = user_id, "mobile device enrolled");
        }
        // Said rather than only logged. The refresh token this hands back lives
        // for ninety days, rotates itself forward, and works from anything
        // holding it, so a device enrolled behind the user's back is a durable
        // credential they would otherwise find only by opening a list.
        self.publish_security_notice(
            pm_protocol::domain::SecurityNoticeKind::DeviceEnrolled,
            &device.name,
            "this device can now read your sessions and type into their terminals. \
             Revoke it in user settings if it is not yours.",
        );
        Ok(MobileEnrollment { device, tokens })
    }

    /// Rotates a refresh token into a fresh token pair. Each rotated
    /// token carries one retry: a second presentation revokes the
    /// unspent successor and mints a replacement pair; a third
    /// presentation or any reuse after the successor has been spent
    /// revokes the whole device token family.
    pub fn mobile_refresh(
        &self,
        refresh_token: &str,
    ) -> Result<(MobileTokens, u64), MobileAuthError> {
        let now = now_unix_ms();
        let stored = self
            .storage()
            .lookup_mobile_refresh_token(&hash_token(refresh_token))
            .map_err(|_| MobileAuthError::Internal)?
            .ok_or(MobileAuthError::InvalidToken)?;
        let device = self
            .storage()
            .get_mobile_device(stored.device_id)
            .map_err(|_| MobileAuthError::InvalidToken)?;
        if device.revoked_at_unix_ms.is_some() {
            return Err(MobileAuthError::InvalidToken);
        }
        if now > stored.expires_at_unix_ms {
            return Err(MobileAuthError::InvalidToken);
        }
        if stored.used_at_unix_ms.is_some() {
            // Already rotated. Check retry eligibility.
            if let (Some(ref sr_hash), Some(ref sa_hash)) = (
                &stored.successor_refresh_hash,
                &stored.successor_access_hash,
            ) {
                if let Some(condition) = self
                    .storage()
                    .check_mobile_successor_spent(sr_hash, sa_hash)
                    .map_err(|_| MobileAuthError::Internal)?
                {
                    self.storage()
                        .revoke_mobile_device(device.id, now)
                        .map_err(|_| MobileAuthError::Internal)?;
                    warn!(
                        device = device.id,
                        user = device.user_id,
                        condition,
                        "mobile refresh token reuse after successor spent, device revoked"
                    );
                    return Err(MobileAuthError::InvalidToken);
                }
                if stored.retry_at_unix_ms.is_some() {
                    self.storage()
                        .revoke_mobile_device(device.id, now)
                        .map_err(|_| MobileAuthError::Internal)?;
                    warn!(
                        device = device.id,
                        user = device.user_id,
                        "mobile refresh token third use, device revoked"
                    );
                    return Err(MobileAuthError::InvalidToken);
                }
                // Valid retry: revoke the unspent successor and mint
                // a replacement pair.
                self.storage()
                    .revoke_mobile_successor_tokens(sr_hash, sa_hash)
                    .map_err(|_| MobileAuthError::Internal)?;
                self.storage()
                    .mark_mobile_refresh_token_retry(stored.id, now)
                    .map_err(|_| MobileAuthError::Internal)?;
                let tokens = self.issue_mobile_tokens(device.id, device.user_id)?;
                let new_sr_hash = hash_token(&tokens.refresh_token);
                let new_sa_hash = hash_token(&tokens.access_token);
                self.storage()
                    .set_mobile_refresh_token_successor(stored.id, &new_sr_hash, &new_sa_hash)
                    .map_err(|_| MobileAuthError::Internal)?;
                self.storage()
                    .touch_mobile_device(device.id, now)
                    .map_err(|_| MobileAuthError::Internal)?;
                return Ok((tokens, device.id));
            }
            self.storage()
                .revoke_mobile_device(device.id, now)
                .map_err(|_| MobileAuthError::Internal)?;
            warn!(
                device = device.id,
                user = device.user_id,
                "mobile refresh token reuse detected, device revoked"
            );
            return Err(MobileAuthError::InvalidToken);
        }
        // First use: rotate normally.
        self.storage()
            .mark_mobile_refresh_token_used(stored.id, now)
            .map_err(|_| MobileAuthError::Internal)?;
        let tokens = self.issue_mobile_tokens(device.id, device.user_id)?;
        // Record successor hashes for the one-retry rule.
        let successor_refresh_hash = hash_token(&tokens.refresh_token);
        let successor_access_hash = hash_token(&tokens.access_token);
        self.storage()
            .set_mobile_refresh_token_successor(
                stored.id,
                &successor_refresh_hash,
                &successor_access_hash,
            )
            .map_err(|_| MobileAuthError::Internal)?;
        self.storage()
            .touch_mobile_device(device.id, now)
            .map_err(|_| MobileAuthError::Internal)?;
        Ok((tokens, device.id))
    }

    fn issue_mobile_tokens(
        &self,
        device_id: u64,
        user_id: u64,
    ) -> Result<MobileTokens, MobileAuthError> {
        let now = now_unix_ms();
        let access_token = generate_token();
        let access_expires = now
            + self.mobile_ttl_ms(
                SETTING_MOBILE_ACCESS_TTL_MINUTES,
                MOBILE_ACCESS_TTL_MINUTES_DEFAULT,
                MINUTE_MS,
            );
        let refresh_token = generate_token();
        let refresh_expires = now
            + self.mobile_ttl_ms(
                SETTING_MOBILE_REFRESH_TTL_DAYS,
                MOBILE_REFRESH_TTL_DAYS_DEFAULT,
                DAY_MS,
            );
        self.storage()
            .create_access_token(
                Some(device_id),
                user_id,
                None,
                &hash_token(&access_token),
                now,
                access_expires,
            )
            .map_err(|_| MobileAuthError::Internal)?;
        self.storage()
            .create_mobile_refresh_token(
                device_id,
                &hash_token(&refresh_token),
                now,
                refresh_expires,
            )
            .map_err(|_| MobileAuthError::Internal)?;
        Ok(MobileTokens {
            access_token,
            access_expires_at_unix_ms: access_expires,
            refresh_token,
            refresh_expires_at_unix_ms: refresh_expires,
        })
    }

    /// Resolves a bearer access token to the user it speaks for and the
    /// device holding it, which is `None` for the dashboard's own token.
    /// Expired tokens and revoked devices resolve to None. Marks the token's
    /// first authenticated use for the one-retry rule.
    pub fn access_token_verify(&self, access_token: &str) -> Option<(u64, String, Option<u64>)> {
        let now = now_unix_ms();
        let token_hash = hash_token(access_token);
        let holder = self
            .storage()
            .lookup_access_token(&token_hash, now)
            .ok()??;
        let username = self.storage().get_username(holder.user_id).ok()??;
        if let Some(device_id) = holder.device_id {
            let _ = self.storage().touch_mobile_device(device_id, now);
        }
        // Only the first use is a write. Marking it unconditionally opened a
        // write transaction on every authenticated request, which is every
        // request the dashboard makes.
        if !holder.first_used {
            let _ = self
                .storage()
                .mark_mobile_access_token_first_use(&token_hash, now);
        }
        Some((holder.user_id, username, holder.device_id))
    }

    /// Mints the dashboard's access token from a signed-in cookie session.
    ///
    /// The cookie's whole remaining job. The token is returned once, in a JSON
    /// body, and is never written to a cookie: the point of the exchange is to
    /// hand the page a credential the browser will not attach to anybody
    /// else's request.
    ///
    /// The session that minted it is recorded, so signing out ends the
    /// authority rather than leaving a token alive for the rest of its term.
    pub fn issue_web_access_token(
        &self,
        user_id: u64,
        session_value: &str,
    ) -> Result<(String, i64), MobileAuthError> {
        let now = now_unix_ms();
        let token = generate_token();
        let expires = now
            + self.mobile_ttl_ms(
                SETTING_MOBILE_ACCESS_TTL_MINUTES,
                MOBILE_ACCESS_TTL_MINUTES_DEFAULT,
                MINUTE_MS,
            );
        let session_hash = self.session_token_hash(session_value);
        self.storage()
            .create_access_token(
                None,
                user_id,
                session_hash.as_deref(),
                &hash_token(&token),
                now,
                expires,
            )
            .map_err(|_| MobileAuthError::Internal)?;
        Ok((token, expires))
    }

    pub fn list_mobile_devices(&self, user_id: u64) -> Result<Vec<MobileDevice>, MobileAuthError> {
        self.storage()
            .list_mobile_devices(user_id)
            .map_err(|_| MobileAuthError::Internal)
    }

    /// Revokes one of the user's devices: bearer access stops
    /// immediately and every stored refresh token becomes unusable.
    pub fn revoke_mobile_device(
        &self,
        user_id: u64,
        device_id: u64,
    ) -> Result<(), MobileAuthError> {
        let device = self
            .storage()
            .get_mobile_device(device_id)
            .map_err(|_| MobileAuthError::DeviceNotFound)?;
        if device.user_id != user_id {
            return Err(MobileAuthError::DeviceNotFound);
        }
        self.storage()
            .revoke_mobile_device(device_id, now_unix_ms())
            .map_err(|_| MobileAuthError::Internal)?;
        info!(device = device_id, user = user_id, "mobile device revoked");
        Ok(())
    }

    /// Mints a one-use control socket ticket consumed by the `/ws`
    /// upgrade. The raw ticket is returned once and stored as a hash.
    pub fn create_control_socket_ticket(
        &self,
        user_id: u64,
        device_id: Option<u64>,
    ) -> Result<(String, i64), MobileAuthError> {
        self.create_socket_ticket(user_id, device_id, None)
    }

    /// Mints a one-use terminal attach ticket bound to the terminal's
    /// current generation and a bounded replay size.
    pub fn create_terminal_attach_ticket(
        &self,
        user_id: u64,
        device_id: Option<u64>,
        terminal_id: u64,
        generation: u64,
        replay_bytes: Option<u64>,
    ) -> Result<(String, i64), MobileAuthError> {
        let terminal = self
            .terminal(terminal_id)
            .map_err(|_| MobileAuthError::TerminalNotFound)?;
        if terminal.generation != generation {
            return Err(MobileAuthError::StaleGeneration);
        }
        let replay_bytes = replay_bytes
            .unwrap_or(TERMINAL_TICKET_REPLAY_DEFAULT_BYTES)
            .clamp(1, TERMINAL_TICKET_REPLAY_MAX_BYTES);
        self.create_socket_ticket(
            user_id,
            device_id,
            Some(TerminalTicketBinding {
                terminal_id,
                generation,
                replay_bytes,
            }),
        )
    }

    fn create_socket_ticket(
        &self,
        user_id: u64,
        device_id: Option<u64>,
        terminal: Option<TerminalTicketBinding>,
    ) -> Result<(String, i64), MobileAuthError> {
        let ticket = generate_token();
        let now = now_unix_ms();
        let expires = now
            + self.mobile_ttl_ms(
                SETTING_MOBILE_SOCKET_TICKET_TTL_SECONDS,
                MOBILE_SOCKET_TICKET_TTL_SECONDS_DEFAULT,
                SECOND_MS,
            );
        self.storage()
            .create_socket_ticket(
                &hash_token(&ticket),
                user_id,
                device_id,
                terminal,
                now,
                expires,
            )
            .map_err(|_| MobileAuthError::Internal)?;
        let device = device_id.unwrap_or_default();
        match terminal {
            Some(binding) => info!(
                user = user_id,
                device,
                terminal = binding.terminal_id,
                generation = binding.generation,
                replay_bytes = binding.replay_bytes,
                "terminal attach ticket minted"
            ),
            None => info!(user = user_id, device, "control socket ticket minted"),
        }
        Ok((ticket, expires))
    }

    /// Consumes a control socket ticket during the `/ws` upgrade,
    /// resolving the user and device it authenticates. Every rejection
    /// is logged without the ticket value.
    pub fn consume_control_socket_ticket(&self, ticket: &str) -> Option<(u64, Option<u64>)> {
        let ticket = self.consume_socket_ticket(ticket, "control", None)?;
        let device = ticket.device_id.unwrap_or_default();
        if let Some(binding) = ticket.terminal {
            warn!(
                user = ticket.user_id,
                device,
                terminal = binding.terminal_id,
                "terminal attach ticket rejected on the control socket"
            );
            return None;
        }
        info!(
            user = ticket.user_id,
            device, "control socket ticket consumed"
        );
        Some((ticket.user_id, ticket.device_id))
    }

    /// Consumes a terminal attach ticket during the terminal WebSocket
    /// upgrade. The URL's terminal and generation must match the mint
    /// bindings exactly; a mismatched ticket is already burned.
    pub fn consume_terminal_attach_ticket(
        &self,
        ticket: &str,
        terminal_id: u64,
        generation: u64,
    ) -> Option<TerminalTicketAttach> {
        let ticket = self.consume_socket_ticket(ticket, "terminal", Some(terminal_id))?;
        let Some(binding) = ticket.terminal else {
            warn!(
                user = ticket.user_id,
                device = ticket.device_id,
                "control socket ticket rejected on a terminal socket"
            );
            return None;
        };
        if binding.terminal_id != terminal_id || binding.generation != generation {
            warn!(
                user = ticket.user_id,
                device = ticket.device_id,
                ticket_terminal = binding.terminal_id,
                terminal = terminal_id,
                ticket_generation = binding.generation,
                generation,
                "terminal attach ticket binding mismatch"
            );
            return None;
        }
        info!(
            user = ticket.user_id,
            device = ticket.device_id,
            terminal = terminal_id,
            "terminal attach ticket consumed"
        );
        Some(TerminalTicketAttach {
            user_id: ticket.user_id,
            device_id: ticket.device_id,
            replay_bytes: binding.replay_bytes as usize,
        })
    }

    fn consume_socket_ticket(
        &self,
        ticket: &str,
        socket: &'static str,
        terminal_hint: Option<u64>,
    ) -> Option<SocketTicket> {
        match self
            .storage()
            .consume_socket_ticket(&hash_token(ticket), now_unix_ms())
        {
            Ok(consumed) => Some(consumed),
            Err(StorageError::Conflict(reason)) => {
                // A client that keeps failing here retries indefinitely, and
                // without the terminal it is reaching for there is nothing in
                // the log to say which client or what it wants.
                warn!(
                    socket,
                    %reason,
                    terminal = terminal_hint.unwrap_or_default(),
                    "socket ticket rejected"
                );
                None
            }
            Err(e) => {
                warn!(socket, error = %e, "socket ticket lookup failed");
                None
            }
        }
    }
}

/// The identity a consumed terminal attach ticket authenticates, plus
/// the replay bound it was minted with.
pub struct TerminalTicketAttach {
    pub user_id: u64,
    /// The device that minted the ticket, or `None` when the dashboard did.
    pub device_id: Option<u64>,
    pub replay_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::DaemonConfig;

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

    fn enroll_request(proof: MobileEnrollProof) -> MobileEnrollRequest {
        MobileEnrollRequest {
            proof,
            app_installation_id: "app-1234".into(),
            name: "test phone".into(),
            platform: "ios".into(),
        }
    }

    fn password_proof() -> MobileEnrollProof {
        MobileEnrollProof::Password {
            username: "testuser".into(),
            password: "hunter2hunter2".into(),
        }
    }

    /// Token-bearing results deliberately do not implement Debug, so
    /// `unwrap_err` is unavailable on them.
    fn expect_err<T>(result: Result<T, MobileAuthError>) -> MobileAuthError {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    #[test]
    fn installation_id_is_stable() {
        let d = daemon();
        let id = d.installation_id();
        assert!(!id.is_empty());
        assert_eq!(d.installation_id(), id);
    }

    #[test]
    fn enroll_with_password_and_bearer_verify() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        assert_eq!(enrollment.device.name, "test phone");
        assert_eq!(enrollment.device.platform, "ios");
        assert_eq!(enrollment.device.app_installation_id, "app-1234");
        assert!(enrollment.device.revoked_at_unix_ms.is_none());
        let (user_id, username, device_id) = d
            .access_token_verify(&enrollment.tokens.access_token)
            .unwrap();
        assert_eq!(username, "testuser");
        assert_eq!(user_id, enrollment.device.user_id);
        assert_eq!(device_id, Some(enrollment.device.id));
    }

    #[test]
    fn enroll_rejects_bad_password_and_bad_input() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let wrong = MobileEnrollProof::Password {
            username: "testuser".into(),
            password: "wrong-password".into(),
        };
        assert_eq!(
            expect_err(d.mobile_enroll(enroll_request(wrong))),
            MobileAuthError::InvalidCredentials
        );
        let mut request = enroll_request(password_proof());
        request.app_installation_id = "  ".into();
        assert!(matches!(
            expect_err(d.mobile_enroll(request)),
            MobileAuthError::Rejected(_)
        ));
        assert!(d.list_mobile_devices(1).unwrap().is_empty());
    }

    #[test]
    fn enroll_token_is_single_use() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let (user_id, _) = d
            .auth_verify_user(&d.auth_login("testuser", "hunter2hunter2").unwrap())
            .unwrap();
        let (token, expires) = d.create_mobile_enrollment_token(user_id).unwrap();
        assert!(expires > now_unix_ms());
        let enrollment = d
            .mobile_enroll(enroll_request(MobileEnrollProof::EnrollToken(
                token.clone(),
            )))
            .unwrap();
        assert_eq!(enrollment.device.user_id, user_id);
        assert_eq!(
            expect_err(d.mobile_enroll(enroll_request(MobileEnrollProof::EnrollToken(token)))),
            MobileAuthError::InvalidEnrollment("enrollment token already used".into())
        );
        assert_eq!(
            expect_err(
                d.mobile_enroll(enroll_request(MobileEnrollProof::EnrollToken(
                    "bogus".into()
                )))
            ),
            MobileAuthError::InvalidEnrollment("unknown enrollment token".into())
        );
    }

    #[test]
    fn refresh_returns_the_device_so_a_client_can_recover_its_id() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();

        let (_, device_id) = d.mobile_refresh(&enrollment.tokens.refresh_token).unwrap();

        // A device that lost its stored id recovers it here instead of
        // re-enrolling, so it must be the same device it enrolled as.
        assert_eq!(device_id, enrollment.device.id);
    }

    #[test]
    fn logging_back_in_reuses_the_installation_device() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let first = d.mobile_enroll(enroll_request(password_proof())).unwrap();

        // The app keeps its installation id across a sign-out, so the
        // second login is the same phone arriving again.
        let mut again = enroll_request(password_proof());
        again.name = "renamed phone".into();
        let second = d.mobile_enroll(again).unwrap();

        assert_eq!(second.device.id, first.device.id);
        assert_eq!(second.device.name, "renamed phone");
        assert_eq!(
            second.device.created_at_unix_ms, first.device.created_at_unix_ms,
            "the device keeps the date it was first enrolled"
        );
        let devices = d.list_mobile_devices(first.device.user_id).unwrap();
        assert_eq!(devices.len(), 1);

        assert!(d.access_token_verify(&second.tokens.access_token).is_some());
        assert!(
            d.access_token_verify(&first.tokens.access_token).is_none(),
            "the previous login's access token must stop working"
        );
        assert_eq!(
            expect_err(d.mobile_refresh(&first.tokens.refresh_token)),
            MobileAuthError::InvalidToken
        );
    }

    #[test]
    fn a_second_installation_enrolls_as_its_own_device() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let first = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let mut other = enroll_request(password_proof());
        other.app_installation_id = "app-5678".into();
        let second = d.mobile_enroll(other).unwrap();

        assert_ne!(second.device.id, first.device.id);
        assert_eq!(
            d.list_mobile_devices(first.device.user_id).unwrap().len(),
            2
        );
    }

    #[test]
    fn one_installation_enrolled_by_two_users_stays_two_devices() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let sam = d
            .storage()
            .create_user("sam", "unused-hash", now_unix_ms())
            .unwrap();
        let testuser = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let (token, _) = d.create_mobile_enrollment_token(sam).unwrap();
        let other = d
            .mobile_enroll(enroll_request(MobileEnrollProof::EnrollToken(token)))
            .unwrap();

        assert_ne!(other.device.id, testuser.device.id);
        assert_eq!(other.device.user_id, sam);
        assert_eq!(
            d.list_mobile_devices(testuser.device.user_id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(d.list_mobile_devices(sam).unwrap().len(), 1);
    }

    #[test]
    fn a_revoked_installation_comes_back_as_a_new_device() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let first = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let user_id = first.device.user_id;
        d.revoke_mobile_device(user_id, first.device.id).unwrap();

        let second = d.mobile_enroll(enroll_request(password_proof())).unwrap();

        assert_ne!(
            second.device.id, first.device.id,
            "a revoked device is not revived by re-enrolling"
        );
        let devices = d.list_mobile_devices(user_id).unwrap();
        assert_eq!(
            devices.iter().map(|device| device.id).collect::<Vec<_>>(),
            vec![second.device.id],
            "the revoked row stays out of the list"
        );
    }

    #[test]
    fn refresh_lost_response_retry_returns_a_replacement_pair() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let first_refresh = enrollment.tokens.refresh_token;

        let (rotated, _) = d.mobile_refresh(&first_refresh).unwrap();
        assert_ne!(rotated.refresh_token, first_refresh);
        assert_ne!(rotated.access_token, enrollment.tokens.access_token);

        // The client never received the response (lost in transit),
        // so neither successor token has been used yet. The retry
        // revokes the unspent successor and mints a replacement pair.
        let (retry, _) = d.mobile_refresh(&first_refresh).unwrap();
        assert_ne!(retry.access_token, rotated.access_token);
        assert_ne!(retry.refresh_token, rotated.refresh_token);

        // The first successor no longer verifies or refreshes.
        assert!(d.access_token_verify(&rotated.access_token).is_none());
        assert_eq!(
            expect_err(d.mobile_refresh(&rotated.refresh_token)),
            MobileAuthError::InvalidToken
        );

        // The replacement pair works.
        assert!(d.access_token_verify(&retry.access_token).is_some());
        assert!(d
            .storage()
            .get_mobile_device(enrollment.device.id)
            .unwrap()
            .revoked_at_unix_ms
            .is_none());
    }

    #[test]
    fn refresh_third_use_revokes_the_family() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let first_refresh = enrollment.tokens.refresh_token.clone();

        let (rotated, _) = d.mobile_refresh(&first_refresh).unwrap();
        // Use the retry without spending the successor.
        let _ = d.mobile_refresh(&first_refresh).unwrap();
        // Third use: revokes.
        assert_eq!(
            expect_err(d.mobile_refresh(&first_refresh)),
            MobileAuthError::InvalidToken
        );
        assert!(d.access_token_verify(&rotated.access_token).is_none());
        assert!(d
            .storage()
            .get_mobile_device(enrollment.device.id)
            .unwrap()
            .revoked_at_unix_ms
            .is_some());
        assert!(d
            .list_mobile_devices(enrollment.device.user_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn old_token_after_successor_rotation_revokes() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let first_refresh = enrollment.tokens.refresh_token.clone();

        let (rotated, _) = d.mobile_refresh(&first_refresh).unwrap();
        // The client received the successor and rotated it again.
        let (_second, _) = d.mobile_refresh(&rotated.refresh_token).unwrap();
        // Presenting the old token now that the successor was rotated revokes.
        assert_eq!(
            expect_err(d.mobile_refresh(&first_refresh)),
            MobileAuthError::InvalidToken
        );
        assert!(d
            .storage()
            .get_mobile_device(enrollment.device.id)
            .unwrap()
            .revoked_at_unix_ms
            .is_some());
    }

    #[test]
    fn old_token_after_successor_access_use_revokes() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let first_refresh = enrollment.tokens.refresh_token.clone();

        let (rotated, _) = d.mobile_refresh(&first_refresh).unwrap();
        // The client received the successor and used the access token.
        assert!(d.access_token_verify(&rotated.access_token).is_some());
        // Presenting the old token now that the successor access was used revokes.
        assert_eq!(
            expect_err(d.mobile_refresh(&first_refresh)),
            MobileAuthError::InvalidToken
        );
        assert!(d
            .storage()
            .get_mobile_device(enrollment.device.id)
            .unwrap()
            .revoked_at_unix_ms
            .is_some());
    }

    #[test]
    fn expired_rotated_token_still_rejected() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let device_id = enrollment.device.id;
        let now = now_unix_ms();

        // Create a refresh token that has already expired.
        let expired_refresh = generate_token();
        d.storage()
            .create_mobile_refresh_token(device_id, &hash_token(&expired_refresh), now - 2, now - 1)
            .unwrap();
        // Even without the one-retry path, expiry still applies.
        assert_eq!(
            expect_err(d.mobile_refresh(&expired_refresh)),
            MobileAuthError::InvalidToken
        );
    }

    #[test]
    fn revoke_invalidates_access_and_refresh_together() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let user_id = enrollment.device.user_id;
        d.revoke_mobile_device(user_id, enrollment.device.id)
            .unwrap();
        assert!(d
            .access_token_verify(&enrollment.tokens.access_token)
            .is_none());
        assert_eq!(
            expect_err(d.mobile_refresh(&enrollment.tokens.refresh_token)),
            MobileAuthError::InvalidToken
        );
        assert_eq!(
            d.revoke_mobile_device(user_id, 9999).unwrap_err(),
            MobileAuthError::DeviceNotFound
        );
    }

    /// The lookup reports whether first use is already recorded, so verifying
    /// does not have to write to find out. Marking it on every request opened a
    /// write transaction per authenticated call, which is every call the
    /// dashboard makes, and SQLite allows one writer.
    #[test]
    fn a_tokens_first_use_is_recorded_once_and_reported_by_the_lookup() {
        let d = daemon();
        let session = d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let (user_id, _) = d.auth_verify_user(&session).unwrap();
        let (token, _) = d.issue_web_access_token(user_id, &session).unwrap();
        let hash = hash_token(&token);

        let before = d
            .storage()
            .lookup_access_token(&hash, now_unix_ms())
            .unwrap()
            .unwrap();
        assert!(!before.first_used, "an unused token reports no first use");
        assert_eq!(before.device_id, None, "the dashboard holds no device");
        assert_eq!(before.user_id, user_id);

        assert!(d.access_token_verify(&token).is_some());
        let after = d
            .storage()
            .lookup_access_token(&hash, now_unix_ms())
            .unwrap()
            .unwrap();
        assert!(after.first_used, "the first use is recorded");

        // And it stays recorded, so a later request has nothing to write.
        assert!(d.access_token_verify(&token).is_some());
        assert!(
            d.storage()
                .lookup_access_token(&hash, now_unix_ms())
                .unwrap()
                .unwrap()
                .first_used
        );
    }

    #[test]
    fn expired_tokens_do_not_authenticate() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let device_id = enrollment.device.id;
        let user_id = enrollment.device.user_id;
        let now = now_unix_ms();

        let expired_access = generate_token();
        d.storage()
            .create_access_token(
                Some(device_id),
                user_id,
                None,
                &hash_token(&expired_access),
                now - 2,
                now - 1,
            )
            .unwrap();
        assert!(d.access_token_verify(&expired_access).is_none());

        let expired_refresh = generate_token();
        d.storage()
            .create_mobile_refresh_token(device_id, &hash_token(&expired_refresh), now - 2, now - 1)
            .unwrap();
        assert_eq!(
            expect_err(d.mobile_refresh(&expired_refresh)),
            MobileAuthError::InvalidToken
        );

        let expired_enroll = generate_token();
        d.storage()
            .create_mobile_enrollment_token(
                &hash_token(&expired_enroll),
                enrollment.device.user_id,
                now - 2,
                now - 1,
            )
            .unwrap();
        assert_eq!(
            expect_err(
                d.mobile_enroll(enroll_request(MobileEnrollProof::EnrollToken(
                    expired_enroll
                )))
            ),
            MobileAuthError::InvalidEnrollment("enrollment token expired".into())
        );
    }

    #[test]
    fn ttl_settings_shift_expiries() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        d.set_setting(SETTING_MOBILE_ACCESS_TTL_MINUTES, Some("1"))
            .unwrap();
        d.set_setting(SETTING_MOBILE_REFRESH_TTL_DAYS, Some("1"))
            .unwrap();
        let before = now_unix_ms();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let access_ttl = enrollment.tokens.access_expires_at_unix_ms - before;
        let refresh_ttl = enrollment.tokens.refresh_expires_at_unix_ms - before;
        assert!((MINUTE_MS..=MINUTE_MS + 10_000).contains(&access_ttl));
        assert!((DAY_MS..=DAY_MS + 10_000).contains(&refresh_ttl));
    }

    fn enrolled_daemon() -> (Daemon, u64, u64) {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let enrollment = d.mobile_enroll(enroll_request(password_proof())).unwrap();
        let user_id = enrollment.device.user_id;
        let device_id = enrollment.device.id;
        (d, user_id, device_id)
    }

    #[test]
    fn control_socket_ticket_is_single_use() {
        let (d, user_id, device_id) = enrolled_daemon();
        let (ticket, expires) = d
            .create_control_socket_ticket(user_id, Some(device_id))
            .unwrap();
        assert!(expires > now_unix_ms());
        assert_eq!(
            d.consume_control_socket_ticket(&ticket),
            Some((user_id, Some(device_id)))
        );
        assert_eq!(d.consume_control_socket_ticket(&ticket), None);
        assert_eq!(d.consume_control_socket_ticket("bogus"), None);
    }

    #[test]
    fn control_ticket_burns_when_presented_to_a_terminal_socket() {
        let (d, user_id, device_id) = enrolled_daemon();
        let (ticket, _) = d
            .create_control_socket_ticket(user_id, Some(device_id))
            .unwrap();
        assert!(d.consume_terminal_attach_ticket(&ticket, 7, 1).is_none());
        // The kind mismatch already consumed it.
        assert_eq!(d.consume_control_socket_ticket(&ticket), None);
    }

    fn plant_terminal_ticket(
        d: &Daemon,
        user_id: u64,
        device_id: u64,
        binding: TerminalTicketBinding,
    ) -> String {
        let ticket = generate_token();
        let now = now_unix_ms();
        d.storage()
            .create_socket_ticket(
                &hash_token(&ticket),
                user_id,
                Some(device_id),
                Some(binding),
                now,
                now + 30_000,
            )
            .unwrap();
        ticket
    }

    #[test]
    fn terminal_ticket_binding_is_strict_and_burns_on_mismatch() {
        let (d, user_id, device_id) = enrolled_daemon();
        let binding = TerminalTicketBinding {
            terminal_id: 7,
            generation: 3,
            replay_bytes: 1024,
        };

        let ticket = plant_terminal_ticket(&d, user_id, device_id, binding);
        assert!(d.consume_terminal_attach_ticket(&ticket, 8, 3).is_none());
        // A mismatched presentation still burned the ticket.
        assert!(d.consume_terminal_attach_ticket(&ticket, 7, 3).is_none());

        let ticket = plant_terminal_ticket(&d, user_id, device_id, binding);
        assert!(d.consume_terminal_attach_ticket(&ticket, 7, 4).is_none());

        let ticket = plant_terminal_ticket(&d, user_id, device_id, binding);
        let attach = d.consume_terminal_attach_ticket(&ticket, 7, 3).unwrap();
        assert_eq!(attach.user_id, user_id);
        assert_eq!(attach.device_id, Some(device_id));
        assert_eq!(attach.replay_bytes, 1024);
        assert!(d.consume_terminal_attach_ticket(&ticket, 7, 3).is_none());
        assert_eq!(d.consume_control_socket_ticket(&ticket), None);
    }

    #[test]
    fn expired_and_revoked_socket_tickets_are_rejected() {
        let (d, user_id, device_id) = enrolled_daemon();
        let now = now_unix_ms();

        let expired = generate_token();
        d.storage()
            .create_socket_ticket(
                &hash_token(&expired),
                user_id,
                Some(device_id),
                None,
                now - 2,
                now - 1,
            )
            .unwrap();
        assert_eq!(d.consume_control_socket_ticket(&expired), None);

        let (ticket, _) = d
            .create_control_socket_ticket(user_id, Some(device_id))
            .unwrap();
        d.revoke_mobile_device(user_id, device_id).unwrap();
        assert_eq!(d.consume_control_socket_ticket(&ticket), None);
    }

    #[test]
    fn terminal_ticket_mint_requires_a_live_terminal() {
        let (d, user_id, device_id) = enrolled_daemon();
        assert_eq!(
            expect_err(d.create_terminal_attach_ticket(user_id, Some(device_id), 999, 1, None)),
            MobileAuthError::TerminalNotFound
        );
    }

    #[test]
    fn socket_ticket_ttl_setting_shifts_expiry() {
        let (d, user_id, device_id) = enrolled_daemon();
        d.set_setting(SETTING_MOBILE_SOCKET_TICKET_TTL_SECONDS, Some("1"))
            .unwrap();
        let before = now_unix_ms();
        let (_, expires) = d
            .create_control_socket_ticket(user_id, Some(device_id))
            .unwrap();
        let ttl = expires - before;
        assert!((SECOND_MS..=SECOND_MS + 10_000).contains(&ttl));
    }
}
