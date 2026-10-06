//! At-rest sealing for the credentials the daemon stores on the
//! operator's behalf: device push tokens, model-profile API keys, and
//! the credential settings in `push::SECRET_SETTINGS`. Values are
//! sealed with ChaCha20-Poly1305 under a key derived from the
//! generated installation secret, so the database holds only
//! ciphertext.
//!
//! The secret lives in its own owner-only file beside the database
//! rather than inside it, so reading the database is not by itself
//! enough to open what it holds. Both files are owner-only, so this
//! defends a database that travels without its secret — a copy, a
//! backup, a support bundle — not an attacker who already reads the
//! daemon's files.

use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

use crate::daemon::Daemon;
use crate::fsperm::{create_private_dir, restrict_file, PRIVATE_FILE_MODE};

/// Settings key that held the sealing secret before it moved to its own
/// file. Still read once, to carry an existing installation across.
const INSTALLATION_SECRET_KEY: &str = "installation.secret";
const INSTALLATION_SECRET_BYTES: usize = 32;

/// The sealing secret sits beside the database it protects, so a daemon
/// pointed at a different database finds that one's secret.
/// What the sealing secret's file is called, relative to the database.
///
/// Named so a directory share can refuse to serve it by deriving the name from
/// here rather than repeating it, which would drift.
pub(crate) const SECRET_FILE_SUFFIX: &str = ".secret";

pub(crate) fn secret_path_for_db(db_path: &Path) -> PathBuf {
    let mut name = db_path.file_name().unwrap_or_default().to_os_string();
    name.push(SECRET_FILE_SUFFIX);
    db_path.with_file_name(name)
}

fn generate_secret() -> String {
    let mut bytes = [0u8; INSTALLATION_SECRET_BYTES];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn read_secret_file(path: &Path) -> Option<String> {
    let secret = std::fs::read_to_string(path).ok()?;
    let secret = secret.trim().to_string();
    (!secret.is_empty()).then_some(secret)
}

fn write_secret_file(path: &Path, secret: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)?;
    file.write_all(secret.as_bytes())?;
    file.sync_all()?;
    restrict_file(path)
}

pub(crate) const SEALED_PREFIX: &str = "v1:";
pub(crate) const PLAIN_PREFIX: &str = "plain:";
const SEAL_NONCE_BYTES: usize = 12;

/// Domain separator baked into every value already sealed on disk; it
/// must not change or stored credentials stop opening.
const SEAL_DOMAIN: &[u8] = b"pm-push-token";

fn seal_key(secret: &str) -> chacha20poly1305::Key {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(SEAL_DOMAIN);
    let digest = hasher.finalize();
    *chacha20poly1305::Key::from_slice(&digest)
}

/// Seals a credential for storage. Without an installation secret the
/// value is stored base64-marked so reads stay uniform.
pub(crate) fn seal_secret(secret: &str, value: &str) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    if secret.is_empty() {
        return format!("{PLAIN_PREFIX}{}", engine.encode(value.as_bytes()));
    }
    use chacha20poly1305::aead::{Aead, KeyInit};
    let cipher = chacha20poly1305::ChaCha20Poly1305::new(&seal_key(secret));
    let mut nonce = [0u8; SEAL_NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            chacha20poly1305::Nonce::from_slice(&nonce),
            value.as_bytes(),
        )
        .unwrap_or_default();
    let mut sealed = nonce.to_vec();
    sealed.extend_from_slice(&ciphertext);
    format!("{SEALED_PREFIX}{}", engine.encode(sealed))
}

/// Whether a stored value carries a seal, rather than being credential
/// material written before these values were sealed. No credential the
/// daemon stores can start with either prefix: a .p8 key opens with
/// `-----BEGIN`, an FCM service account is a JSON object, and a gateway
/// signing key is hex.
pub(crate) fn is_sealed(stored: &str) -> bool {
    stored.starts_with(SEALED_PREFIX) || stored.starts_with(PLAIN_PREFIX)
}

/// Opens a stored credential, passing through one that predates
/// sealing so an existing installation keeps working.
pub(crate) fn open_stored(secret: &str, stored: &str) -> Option<String> {
    if is_sealed(stored) {
        open_secret(secret, stored)
    } else {
        Some(stored.to_string())
    }
}

pub(crate) fn open_secret(secret: &str, sealed: &str) -> Option<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    if let Some(plain) = sealed.strip_prefix(PLAIN_PREFIX) {
        return String::from_utf8(engine.decode(plain).ok()?).ok();
    }
    let sealed = sealed.strip_prefix(SEALED_PREFIX)?;
    let sealed = engine.decode(sealed).ok()?;
    if sealed.len() <= SEAL_NONCE_BYTES {
        return None;
    }
    use chacha20poly1305::aead::{Aead, KeyInit};
    let cipher = chacha20poly1305::ChaCha20Poly1305::new(&seal_key(secret));
    let (nonce, ciphertext) = sealed.split_at(SEAL_NONCE_BYTES);
    let plaintext = cipher
        .decrypt(chacha20poly1305::Nonce::from_slice(nonce), ciphertext)
        .ok()?;
    String::from_utf8(plaintext).ok()
}

/// Set when the sealing secret has been moved out of the database, and cleared
/// once the pages that held it have been overwritten.
///
/// Dropping a row does not erase it. SQLite frees the space the row occupied
/// and, with `secure_delete` off as it is in the bundled build, leaves the bytes
/// in the page until something writes over them. Nothing necessarily does: the
/// settings table is small and rarely rewritten, so the 64 hex characters of the
/// secret stay legible in the file through every later write, every checkpoint
/// and every restart.
const SECRET_PAGES_PENDING_KEY: &str = "installation.secret_pages_pending";

impl Daemon {
    /// Overwrites the database pages a migrated sealing secret was written to,
    /// once, for an installation created before it moved to its own file.
    ///
    /// Until this runs, the promise that a copy of the database alone does not
    /// yield the secret is false for that installation, and the way it is
    /// broken is a copy an operator makes deliberately: a database attached to
    /// a bug report, or a support bundle.
    pub(crate) fn erase_migrated_secret_pages(&self) {
        let pending = self
            .storage()
            .get_setting(SECRET_PAGES_PENDING_KEY)
            .ok()
            .flatten()
            .is_some();
        if !pending {
            return;
        }
        let bytes = self.storage().size_on_disk().unwrap_or_default();
        warn!(
            database_bytes = bytes,
            "this installation kept the sealing secret in the database before it moved to \
             its own file, and dropping that row did not erase it. Rebuilding the database \
             now to overwrite the pages it was in, which happens once. Startup pauses until \
             it finishes, roughly in proportion to the size above"
        );
        let started = std::time::Instant::now();
        if let Err(error) = self.storage().rebuild_erasing_freed_pages() {
            error!(
                %error,
                "could not rebuild the database, so it still holds the sealing secret in a \
                 freed page. Treat a copy of it as carrying that secret, and restart to try \
                 again"
            );
            return;
        }
        // Cleared only on success, so a failed or interrupted rebuild is
        // attempted again rather than recorded as done.
        if let Err(error) = self.storage().set_setting(SECRET_PAGES_PENDING_KEY, None) {
            warn!(
                %error,
                "rebuilt the database but could not record that it is done, so it will be \
                 rebuilt again on the next start"
            );
            return;
        }
        info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "rebuilt the database, so the sealing secret it used to hold is no longer in it"
        );
    }

    /// The at-rest sealing secret; generated once, then permanent. Every
    /// sealed read and write needs it, so it is resolved once per run.
    pub(crate) fn installation_secret(&self) -> String {
        self.installation_secret
            .get_or_init(|| match self.secret_path.as_deref() {
                Some(path) => self.secret_from_file(path),
                None => self.secret_from_database(),
            })
            .clone()
    }

    /// Reads the secret from its own file, carrying across one an older
    /// version left in the database. The database copy is dropped only
    /// once the file is durable, so an interrupted move cannot strand
    /// credentials that no longer open.
    fn secret_from_file(&self, path: &Path) -> String {
        if let Some(existing) = read_secret_file(path) {
            return existing;
        }
        let stored = self
            .storage()
            .get_setting(INSTALLATION_SECRET_KEY)
            .ok()
            .flatten()
            .filter(|secret| !secret.is_empty());
        let secret = stored.clone().unwrap_or_else(generate_secret);
        if let Err(error) = write_secret_file(path, &secret) {
            error!(
                path = %path.display(),
                %error,
                "cannot write the sealing secret to its own file, keeping it in the database"
            );
            return self.secret_from_database();
        }
        if stored.is_some() {
            if let Err(error) = self.storage().set_setting(INSTALLATION_SECRET_KEY, None) {
                warn!(
                    %error,
                    "sealing secret now lives in its own file but the database still holds a copy"
                );
            } else if let Err(error) = self
                .storage()
                .set_setting(SECRET_PAGES_PENDING_KEY, Some("1"))
            {
                warn!(
                    %error,
                    "cannot record that the database still holds the sealing secret in a freed page"
                );
            }
        }
        secret
    }

    /// A setting's usable value: credential settings are sealed at
    /// rest, so reading one needs the installation secret.
    pub(crate) fn setting_value(&self, key: &str) -> Option<String> {
        let stored = self.storage().get_setting(key).ok().flatten()?;
        if !crate::push::SECRET_SETTINGS.contains(&key) {
            return Some(stored);
        }
        let opened = open_stored(&self.installation_secret(), &stored);
        if opened.is_none() {
            warn!(
                key,
                "stored credential does not open with this installation's sealing secret, \
                 treating it as unset"
            );
        }
        opened
    }

    /// Seals credential settings that an earlier version wrote in the
    /// clear. Rewriting the row does not undo the exposure: SQLite
    /// keeps the superseded page in the write-ahead log and in freed
    /// pages, and any backup taken before this still holds it, which
    /// is what the warning tells the operator.
    pub fn seal_stored_credential_settings(&self) {
        let secret = self.installation_secret();
        for key in crate::push::SECRET_SETTINGS {
            let Some(stored) = self.storage().get_setting(key).ok().flatten() else {
                continue;
            };
            if stored.is_empty() || is_sealed(&stored) {
                continue;
            }
            match self
                .storage()
                .set_setting(key, Some(&seal_secret(&secret, &stored)))
            {
                Ok(()) => warn!(
                    key,
                    "credential was stored in the clear and is now sealed, but the old value \
                     stays readable in the database write-ahead log, in freed pages, and in \
                     every backup taken before now. Treat it as exposed: revoke and replace \
                     it with its issuer, then set it again"
                ),
                Err(error) => warn!(
                    key,
                    %error,
                    "cannot seal a credential that is stored in the clear"
                ),
            }
        }
    }

    fn secret_from_database(&self) -> String {
        self.storage()
            .ensure_setting(INSTALLATION_SECRET_KEY, &generate_secret())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_values_round_trip_and_reject_tampering() {
        let sealed = seal_secret("secret-a", "tok-123");
        assert!(sealed.starts_with(SEALED_PREFIX));
        assert!(!sealed.contains("tok-123"));
        assert_eq!(open_secret("secret-a", &sealed).as_deref(), Some("tok-123"));
        assert_eq!(open_secret("secret-b", &sealed), None);
        assert_eq!(open_secret("secret-a", "v1:AAAA"), None);

        let plain = seal_secret("", "tok-123");
        assert!(plain.starts_with(PLAIN_PREFIX));
        assert_eq!(open_secret("", &plain).as_deref(), Some("tok-123"));
    }

    /// Push tokens and model-profile keys share one sealing scheme, so
    /// a value sealed for either opens for the other.
    #[test]
    fn one_scheme_serves_every_stored_credential() {
        let sealed = seal_secret("inst", "sk-live-abc");
        assert!(!sealed.contains("sk-live-abc"));
        assert_eq!(open_secret("inst", &sealed).as_deref(), Some("sk-live-abc"));
    }

    /// Credential settings stored before sealing existed are bare, and
    /// must keep opening or an upgrade takes push down.
    #[test]
    fn a_value_stored_before_sealing_still_opens() {
        let p8 = "-----BEGIN PRIVATE KEY-----\nMIGH\n-----END PRIVATE KEY-----";
        assert!(!is_sealed(p8));
        assert_eq!(open_stored("inst", p8).as_deref(), Some(p8));

        let account = r#"{"type":"service_account","project_id":"p"}"#;
        assert!(!is_sealed(account));
        assert_eq!(open_stored("inst", account).as_deref(), Some(account));

        let hex_key = "a".repeat(64);
        assert!(!is_sealed(&hex_key));
        assert_eq!(open_stored("inst", &hex_key).as_deref(), Some(&*hex_key));

        let sealed = seal_secret("inst", p8);
        assert!(is_sealed(&sealed));
        assert_eq!(open_stored("inst", &sealed).as_deref(), Some(p8));
        assert_eq!(open_stored("wrong", &sealed), None);
    }
}
