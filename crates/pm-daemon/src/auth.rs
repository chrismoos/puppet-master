//! Cookie-session auth: one users table (a single row in practice),
//! argon2 password hashes, random bearer tokens stored only as
//! sha256 hashes. The unix socket bypasses all of this by design;
//! only the HTTP surface authenticates.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::daemon::{now_unix_ms, Daemon};

pub const SESSION_COOKIE: &str = "pm_session";

const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

const TOKEN_BYTES: usize = 32;

const MIN_PASSWORD_CHARS: usize = 8;

/// Domain separation, so the signature over a session token can never be
/// confused with a MAC this installation computes for anything else.
const SESSION_COOKIE_MAC_LABEL: &[u8] = b"pm session cookie v1\0";

/// Bytes of the tag the cookie carries. A forger has to produce all of them at
/// once and learns nothing from a near miss, so this is about keeping the
/// cookie short rather than about the margin.
const SESSION_COOKIE_MAC_BYTES: usize = 16;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AuthError {
    #[error("setup already completed")]
    AlreadySetUp,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("username must not be empty")]
    EmptyUsername,
    #[error("password must be at least {MIN_PASSWORD_CHARS} characters")]
    PasswordTooShort,
    #[error("new password must differ from the current one")]
    PasswordUnchanged,
    #[error("internal auth error")]
    Internal,
}

/// Public so tests can compute the at-rest hash of a known token.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Random bearer token, used for both auth sessions and per-session
/// hook/report tokens.
pub(crate) fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

impl Daemon {
    pub fn needs_setup(&self) -> bool {
        self.storage().user_count().map(|n| n == 0).unwrap_or(false)
    }

    /// Creates the initial user and returns a logged-in session token.
    pub fn auth_setup(&self, username: &str, password: &str) -> Result<String, AuthError> {
        if username.trim().is_empty() {
            return Err(AuthError::EmptyUsername);
        }
        if password.chars().count() < MIN_PASSWORD_CHARS {
            return Err(AuthError::PasswordTooShort);
        }
        if self
            .storage()
            .user_count()
            .map_err(|_| AuthError::Internal)?
            > 0
        {
            return Err(AuthError::AlreadySetUp);
        }
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|_| AuthError::Internal)?
            .to_string();
        let user_id = self
            .storage()
            .create_user(username.trim(), &hash, now_unix_ms())
            .map_err(|_| AuthError::AlreadySetUp)?;
        self.issue_token(user_id)
    }

    pub fn auth_login(&self, username: &str, password: &str) -> Result<String, AuthError> {
        let user_id = self.verify_password(username, password)?;
        self.issue_token(user_id)
    }

    /// Checks a username/password pair without creating a session.
    ///
    /// A username that does not exist costs the same as one that does. Returning
    /// before the argon2 work made the two measurably different — a point lookup
    /// against a full hash verification, tens of microseconds against ten
    /// milliseconds — so one request per guess enumerated valid usernames. One
    /// user per install and an operator-chosen name make that worth little, but
    /// it is a few lines and a reader will measure it.
    pub(crate) fn verify_password(&self, username: &str, password: &str) -> Result<u64, AuthError> {
        let found = self
            .storage()
            .get_user_by_name(username.trim())
            .map_err(|_| AuthError::Internal)?;
        let (user_id, stored_hash) = match found {
            Some(found) => found,
            // Verified against a hash of this installation's own making, so the
            // work is real rather than a sleep that a loaded host would skew.
            None => (0, self.absent_user_hash()),
        };
        let parsed = PasswordHash::new(&stored_hash).map_err(|_| AuthError::Internal)?;
        let verified = Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
        if !verified || user_id == 0 {
            return Err(AuthError::InvalidCredentials);
        }
        Ok(user_id)
    }

    /// A PHC string no password matches, computed once per run, so a login for a
    /// username that does not exist does the same argon2 work as one that does.
    fn absent_user_hash(&self) -> String {
        self.absent_user_hash
            .get_or_init(|| {
                let salt = SaltString::generate(&mut OsRng);
                Argon2::default()
                    // A random secret rather than a fixed one, so the stored
                    // hash this verifies against is not a known quantity.
                    .hash_password(generate_token().as_bytes(), &salt)
                    .map(|hash| hash.to_string())
                    .unwrap_or_default()
            })
            .clone()
    }

    /// Proves the person at the keyboard for an action a session must not
    /// authorize on its own. A cookie says a browser signed in once and
    /// keeps saying it for thirty days, so anything that transfers trust
    /// asks for the password again.
    pub fn reauthenticate(&self, cookie_value: &str, password: &str) -> Result<u64, AuthError> {
        let (user_id, _) = self
            .auth_verify_user(cookie_value)
            .ok_or(AuthError::InvalidCredentials)?;
        let stored = self
            .storage()
            .get_user_hash(user_id)
            .map_err(|_| AuthError::Internal)?
            .ok_or(AuthError::InvalidCredentials)?;
        let parsed = PasswordHash::new(&stored).map_err(|_| AuthError::Internal)?;
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .map_err(|_| AuthError::InvalidCredentials)?;
        Ok(user_id)
    }

    /// Replaces the password of the user holding `token`, after proving
    /// they know the current one. Every other session that user holds is
    /// dropped, so a stolen cookie does not outlive the change, while the
    /// caller stays signed in where they made it.
    pub fn change_password(
        &self,
        cookie_value: &str,
        current: &str,
        next: &str,
    ) -> Result<(), AuthError> {
        let user_id = self.reauthenticate(cookie_value, current)?;
        let keep = self
            .session_token_of(cookie_value)
            .ok_or(AuthError::InvalidCredentials)?;
        if next.chars().count() < MIN_PASSWORD_CHARS {
            return Err(AuthError::PasswordTooShort);
        }
        if next == current {
            return Err(AuthError::PasswordUnchanged);
        }
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(next.as_bytes(), &salt)
            .map_err(|_| AuthError::Internal)?
            .to_string();
        self.storage()
            .set_user_password(user_id, &hash)
            .map_err(|_| AuthError::Internal)?;
        self.storage()
            .delete_other_auth_sessions(user_id, &hash_token(&keep))
            .map_err(|_| AuthError::Internal)?;
        Ok(())
    }

    /// Ends a session and every access token minted from it. Dropping only the
    /// session row would leave the dashboard's token answering for the rest of
    /// its term, which is not what signing out means.
    pub fn auth_logout(&self, cookie_value: &str) {
        if let Some(token) = self.session_token_of(cookie_value) {
            let session_hash = hash_token(&token);
            let _ = self
                .storage()
                .delete_access_tokens_of_session(&session_hash);
            let _ = self.storage().delete_auth_session(&session_hash);
        }
    }

    /// The hash an access token records to name the session that minted it.
    pub(crate) fn session_token_hash(&self, cookie_value: &str) -> Option<String> {
        self.session_token_of(cookie_value)
            .map(|token| hash_token(&token))
    }

    /// Resolves a session cookie's value to its username.
    pub fn auth_verify(&self, cookie_value: &str) -> Option<String> {
        self.auth_verify_user(cookie_value)
            .map(|(_, username)| username)
    }

    pub fn auth_verify_user(&self, cookie_value: &str) -> Option<(u64, String)> {
        let token = self.session_token_of(cookie_value)?;
        let user_id = self
            .storage()
            .lookup_auth_session(&hash_token(&token), now_unix_ms())
            .ok()??;
        self.storage()
            .get_username(user_id)
            .ok()?
            .map(|username| (user_id, username))
    }

    /// The session token inside a cookie value, or `None` when the signature
    /// over it does not hold.
    ///
    /// A cookie is keyed by name, domain and path, and the port is no part of
    /// that, so anything sharing the dashboard's host can overwrite
    /// `pm_session` with a value of its own and the browser will send that one.
    /// A signature the writer cannot compute makes such a value fail here,
    /// before it reaches a lookup. `__Host-` would also stop it, but only over
    /// HTTPS, and this has to hold on a plain-HTTP deployment too.
    pub(crate) fn session_token_of(&self, cookie_value: &str) -> Option<String> {
        let (token, tag) = cookie_value.rsplit_once('.')?;
        let tag = hex::decode(tag).ok()?;
        // Pinned, not merely bounded: a truncated-prefix check would accept a
        // one-byte tag, which is 256 guesses rather than a forgery.
        if tag.len() != SESSION_COOKIE_MAC_BYTES {
            return None;
        }
        self.session_cookie_mac(token)
            .verify_truncated_left(&tag)
            .ok()?;
        Some(token.to_string())
    }

    /// The cookie value a client is given: the token and a signature over it.
    fn signed_session_cookie(&self, token: &str) -> String {
        let tag = hex::encode(
            &self.session_cookie_mac(token).finalize().into_bytes()[..SESSION_COOKIE_MAC_BYTES],
        );
        format!("{token}.{tag}")
    }

    fn session_cookie_mac(&self, token: &str) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.installation_secret().as_bytes())
            .expect("hmac accepts a key of any length");
        mac.update(SESSION_COOKIE_MAC_LABEL);
        mac.update(token.as_bytes());
        mac
    }

    fn issue_token(&self, user_id: u64) -> Result<String, AuthError> {
        let token = generate_token();
        let now = now_unix_ms();
        self.storage()
            .create_auth_session(&hash_token(&token), user_id, now, now + SESSION_TTL_MS)
            .map_err(|_| AuthError::Internal)?;
        Ok(self.signed_session_cookie(&token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::DaemonConfig;

    /// A username that does not exist used to return before any argon2 work, so
    /// it cost a SQLite point lookup against a full hash verification — measured
    /// at forty to fifty times apart, far above any network jitter, which let one
    /// request per guess enumerate valid usernames.
    ///
    /// Asserted as an order of magnitude rather than a tight ratio, because a
    /// loaded machine moves both numbers and a flaky timing test is worse than
    /// none. The failure this guards against is a hundredfold gap, not a few
    /// percent.
    #[test]
    fn an_unknown_username_costs_what_a_known_one_costs() {
        use std::time::Instant;
        let daemon = daemon();
        daemon.auth_setup("auditor", "hunter2hunter2").unwrap();
        // Warm both paths, so neither sample carries a one-off initialization.
        let _ = daemon.auth_login("auditor", "x");
        let _ = daemon.auth_login("nobody", "x");

        let mut known = Vec::new();
        let mut absent = Vec::new();
        for round in 0..9 {
            // Alternate the order so neither pays for a cache the other warmed.
            let names = if round % 2 == 0 {
                ["auditor", "nobody"]
            } else {
                ["nobody", "auditor"]
            };
            for name in names {
                let start = Instant::now();
                assert!(daemon.auth_login(name, "wrong-password").is_err());
                let elapsed = start.elapsed().as_secs_f64();
                if name == "auditor" {
                    known.push(elapsed)
                } else {
                    absent.push(elapsed)
                }
            }
        }
        let median = |mut values: Vec<f64>| {
            values.sort_by(|a, b| a.partial_cmp(b).expect("no NaN timings"));
            values[values.len() / 2]
        };
        let known = median(known);
        let absent = median(absent);
        assert!(
            absent * 8.0 > known,
            "an absent username at {absent:.6}s against a known one at {known:.6}s tells a \
             caller which usernames exist"
        );
    }

    /// Cookie tossing, which `__Host-` would stop only over HTTPS. Anything
    /// sharing the dashboard's host can write `pm_session`, so a value the
    /// writer could not have signed has to fail before it is looked up.
    #[test]
    fn a_session_cookie_a_neighbour_wrote_does_not_verify() {
        let daemon = daemon();
        let cookie = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
        assert_eq!(daemon.auth_verify(&cookie).as_deref(), Some("testuser"));

        let (token, tag) = cookie.rsplit_once('.').expect("the cookie carries a tag");
        for forged in [
            // A bare token, which is what the cookie used to be.
            token.to_string(),
            // A token of the attacker's choosing, unsigned and signed wrongly.
            generate_token(),
            format!("{}.{tag}", generate_token()),
            // The real token under a tag that is not its own.
            format!("{token}.{}", "0".repeat(tag.len())),
            format!("{token}."),
            // A truncated tag, which must not pass as a prefix of a real one.
            format!("{token}.{}", &tag[..tag.len() - 2]),
            String::new(),
        ] {
            assert!(
                daemon.auth_verify(&forged).is_none(),
                "{forged:?} authenticated"
            );
        }
    }

    /// The key is the installation's own, so a cookie minted against one
    /// installation does not open another even if its token were guessed.
    #[test]
    fn a_session_cookie_does_not_cross_installations() {
        let first = daemon();
        let cookie = first.auth_setup("testuser", "hunter2hunter2").unwrap();
        let second = daemon();
        second.auth_setup("testuser", "hunter2hunter2").unwrap();
        assert!(second.auth_verify(&cookie).is_none());
    }

    /// Logging out must find the session behind a signed value rather than
    /// silently deleting nothing.
    #[test]
    fn logout_ends_the_session_the_signed_cookie_names() {
        let daemon = daemon();
        let cookie = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
        daemon.auth_logout(&cookie);
        assert!(daemon.auth_verify(&cookie).is_none());
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

    #[test]
    fn a_password_change_takes_effect_and_keeps_the_caller_signed_in() {
        let d = daemon();
        let token = d.auth_setup("testuser", "hunter2hunter2").unwrap();

        d.change_password(&token, "hunter2hunter2", "correct horse battery")
            .unwrap();

        assert_eq!(d.auth_verify(&token), Some("testuser".into()));
        assert!(d.auth_login("testuser", "correct horse battery").is_ok());
        assert_eq!(
            d.auth_login("testuser", "hunter2hunter2").unwrap_err(),
            AuthError::InvalidCredentials
        );
    }

    /// A password is changed because the old one may be known to someone
    /// else, so the sessions it opened must not outlive it.
    #[test]
    fn a_password_change_signs_out_the_other_sessions() {
        let d = daemon();
        let first = d.auth_setup("testuser", "hunter2hunter2").unwrap();
        let second = d.auth_login("testuser", "hunter2hunter2").unwrap();
        assert_eq!(d.auth_verify(&second), Some("testuser".into()));

        d.change_password(&first, "hunter2hunter2", "correct horse battery")
            .unwrap();

        assert_eq!(d.auth_verify(&second), None);
        assert_eq!(d.auth_verify(&first), Some("testuser".into()));
    }

    #[test]
    fn a_password_change_needs_the_current_password() {
        let d = daemon();
        let token = d.auth_setup("testuser", "hunter2hunter2").unwrap();

        assert_eq!(
            d.change_password(&token, "not-the-one", "correct horse battery")
                .unwrap_err(),
            AuthError::InvalidCredentials
        );
        assert!(d.auth_login("testuser", "hunter2hunter2").is_ok());
    }

    #[test]
    fn a_password_change_holds_the_length_floor() {
        let d = daemon();
        let token = d.auth_setup("testuser", "hunter2hunter2").unwrap();

        assert_eq!(
            d.change_password(&token, "hunter2hunter2", "short")
                .unwrap_err(),
            AuthError::PasswordTooShort
        );
        assert_eq!(
            d.change_password(&token, "hunter2hunter2", "hunter2hunter2")
                .unwrap_err(),
            AuthError::PasswordUnchanged
        );
        assert!(d.auth_login("testuser", "hunter2hunter2").is_ok());
    }

    #[test]
    fn an_unknown_token_cannot_change_a_password() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();

        assert_eq!(
            d.change_password("not-a-token", "hunter2hunter2", "correct horse battery")
                .unwrap_err(),
            AuthError::InvalidCredentials
        );
        assert!(d.auth_login("testuser", "hunter2hunter2").is_ok());
    }

    #[test]
    fn setup_once_then_login_flow() {
        let d = daemon();
        assert!(d.needs_setup());

        let token = d.auth_setup("testuser", "hunter2hunter2").unwrap();
        assert!(!d.needs_setup());
        assert_eq!(d.auth_verify(&token), Some("testuser".into()));

        assert_eq!(
            d.auth_setup("mallory", "password123").unwrap_err(),
            AuthError::AlreadySetUp
        );

        let token2 = d.auth_login("testuser", "hunter2hunter2").unwrap();
        assert_eq!(d.auth_verify(&token2), Some("testuser".into()));
        assert_eq!(
            d.auth_login("testuser", "wrong-password").unwrap_err(),
            AuthError::InvalidCredentials
        );
        assert_eq!(
            d.auth_login("nobody", "hunter2hunter2").unwrap_err(),
            AuthError::InvalidCredentials
        );
    }

    #[test]
    fn setup_validates_inputs() {
        let d = daemon();
        assert_eq!(
            d.auth_setup("", "longenoughpw").unwrap_err(),
            AuthError::EmptyUsername
        );
        assert_eq!(
            d.auth_setup("testuser", "short").unwrap_err(),
            AuthError::PasswordTooShort
        );
        assert!(d.needs_setup());
    }

    #[test]
    fn logout_invalidates_the_token() {
        let d = daemon();
        let token = d.auth_setup("testuser", "hunter2hunter2").unwrap();
        d.auth_logout(&token);
        assert_eq!(d.auth_verify(&token), None);
    }

    #[test]
    fn bogus_tokens_do_not_verify() {
        let d = daemon();
        d.auth_setup("testuser", "hunter2hunter2").unwrap();
        assert_eq!(d.auth_verify("not-a-token"), None);
    }
}
