//! Push gateway relay for self-hosted controllers.
//!
//! Accepts sealed push requests from controllers, wraps the sealed
//! payload in an APNs envelope, and relays to Apple. The relay never
//! reads the notification content and keeps no per-controller or
//! per-device state: the controller sends the device token with each
//! push, so there is nothing here to register, hijack or back up.
//!
//! It authenticates nothing. The device token is the bearer credential
//! and rate limits bound abuse, matching the Matrix and Home Assistant
//! push gateways. An operator who needs to restrict who may relay must
//! do it in front of the process.
//!
//! NOT a high-availability service: controllers retry across ~42
//! minutes, so a restart loses nothing. Rate counters and source
//! blocks live in memory and are bounded.
//!
//! Driven by `pm pushgw`.

pub mod client_ip;
pub mod limits;
pub mod relay;

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{info, warn};

/// Everything the relay needs to serve. The caller resolves flags and
/// environment into this, so the CLI layer stays thin.
pub struct ServeConfig {
    pub listen: SocketAddr,
    pub apns_key_p8_path: PathBuf,
    pub apns_key_id: String,
    /// A second signing key used only for sandbox sends. Both halves
    /// are set together or not at all. Left unset, the key above
    /// serves both environments.
    pub apns_sandbox_key_p8_path: Option<PathBuf>,
    pub apns_sandbox_key_id: Option<String>,
    pub apns_team_id: String,
    pub apns_topic: String,
    pub apns_endpoint: Option<String>,
    /// Reverse proxies whose `X-Forwarded-For` header names the client.
    /// Empty means every request is attributed to its TCP peer.
    pub trusted_proxies: Vec<IpAddr>,
    pub limits: limits::LimitPolicy,
}

/// The APNs identity the relay signs and sends under, kept for the
/// startup line because `ApnsConfig` consumes the strings.
struct ApnsIdentity {
    topic: String,
    key_id: String,
    sandbox_key_id: Option<String>,
    team_id: String,
    endpoint_override: Option<String>,
}

impl ApnsIdentity {
    /// An override replaces Apple's per-environment hosts for every
    /// send, so the startup line says when one is in force.
    fn endpoint_field(&self) -> &str {
        self.endpoint_override
            .as_deref()
            .unwrap_or(DEFAULT_ENDPOINT)
    }

    /// Which key signs sandbox sends, so an operator can tell a
    /// two-key relay from a one-key one without reading the process
    /// arguments. A key id identifies a key and is not key material.
    fn sandbox_key_field(&self) -> &str {
        self.sandbox_key_id.as_deref().unwrap_or(SHARES_PRIMARY_KEY)
    }
}

/// Stands in on the startup line when no endpoint override is set.
const DEFAULT_ENDPOINT: &str = "default";

/// Stands in on the startup line when sandbox sends are signed by the
/// primary key because no sandbox key was configured.
const SHARES_PRIMARY_KEY: &str = "same-as-primary";

/// Reads the .p8 file. Earlier versions took the PEM itself, or an
/// `@`-prefixed path, so both are recognized well enough to say what to
/// pass instead.
fn read_key_file(path: &Path, flag: &str) -> Result<String, String> {
    let raw = path.to_string_lossy();
    if raw.contains("-----BEGIN") {
        return Err(format!(
            "{flag} takes the path to the .p8 file, not its contents"
        ));
    }
    let path = match raw.strip_prefix('@') {
        Some(stripped) if !path.exists() => Path::new(stripped),
        _ => path,
    };
    std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read APNs key file {}: {e}", path.display()))
}

const KEY_FLAG: &str = "--apns-key-p8";
const SANDBOX_KEY_FLAG: &str = "--apns-sandbox-key-p8";
const SANDBOX_KEY_ID_FLAG: &str = "--apns-sandbox-key-id";

/// Resolves the optional sandbox key pair. Half a pair is a
/// misconfiguration rather than a silent fallback to the primary key,
/// which would leave sandbox pushes failing exactly as before.
fn read_sandbox_key(config: &ServeConfig) -> Result<Option<pm_push::ApnsKey>, String> {
    match (
        &config.apns_sandbox_key_p8_path,
        &config.apns_sandbox_key_id,
    ) {
        (Some(path), Some(key_id)) => Ok(Some(pm_push::ApnsKey {
            key_p8: read_key_file(path, SANDBOX_KEY_FLAG)?,
            key_id: key_id.clone(),
        })),
        (None, None) => Ok(None),
        (Some(_), None) => Err(format!(
            "{SANDBOX_KEY_FLAG} also needs {SANDBOX_KEY_ID_FLAG}"
        )),
        (None, Some(_)) => Err(format!(
            "{SANDBOX_KEY_ID_FLAG} also needs {SANDBOX_KEY_FLAG}"
        )),
    }
}

/// Binds the listener and serves until the process ends.
pub async fn serve(config: ServeConfig) -> Result<(), String> {
    let key_p8 = read_key_file(&config.apns_key_p8_path, KEY_FLAG)?;
    let sandbox_key = read_sandbox_key(&config)?;

    let identity = ApnsIdentity {
        topic: config.apns_topic.clone(),
        key_id: config.apns_key_id.clone(),
        sandbox_key_id: config.apns_sandbox_key_id.clone(),
        team_id: config.apns_team_id.clone(),
        endpoint_override: config.apns_endpoint.clone(),
    };

    let apns = pm_push::ApnsProvider::new(pm_push::ApnsConfig {
        key_p8,
        key_id: config.apns_key_id,
        sandbox_key,
        team_id: config.apns_team_id,
        topic: config.apns_topic,
        endpoint_override: config.apns_endpoint,
    })
    .map_err(|e| format!("failed to initialize APNs provider: {e}"))?;

    let trusted_proxies = client_ip::TrustedProxies::new(config.trusted_proxies.iter().copied());
    if trusted_proxies.is_empty() && config.listen.ip().is_loopback() {
        warn!(
            "listening on loopback with no trusted proxy, so every push forwarded by a \
             local reverse proxy shares one source rate limit. Set --trusted-proxy"
        );
    }
    let state = Arc::new(relay::RelayState::new(
        apns,
        trusted_proxies,
        config.limits.clone(),
    ));
    let app = relay::router(state).into_make_service_with_connect_info::<SocketAddr>();

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("failed to bind {}: {e}", config.listen))?;
    info!(
        addr = %config.listen,
        topic = %identity.topic,
        key_id = %identity.key_id,
        sandbox_key_id = %identity.sandbox_key_field(),
        team_id = %identity.team_id,
        apns_endpoint = %identity.endpoint_field(),
        trusted_proxies = ?config.trusted_proxies,
        pushes_per_source = config.limits.pushes_per_source,
        pushes_per_token = config.limits.pushes_per_token,
        rate_window_secs = config.limits.window.as_secs(),
        bad_tokens_per_source = config.limits.bad_tokens_per_source,
        bad_token_window_secs = config.limits.bad_token_window.as_secs(),
        block_secs = config.limits.block_duration.as_secs(),
        "push gateway listening"
    );
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("gateway server stopped: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_key_file_at_the_given_path() {
        let dir = std::env::temp_dir().join("pm-pushgw-key-read");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("AuthKey.p8");
        std::fs::write(&path, "-----BEGIN PRIVATE KEY-----\nabc\n").unwrap();
        assert!(read_key_file(&path, KEY_FLAG)
            .unwrap()
            .contains("BEGIN PRIVATE KEY"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_at_prefixed_path_still_resolves() {
        let dir = std::env::temp_dir().join("pm-pushgw-key-at");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("AuthKey.p8");
        std::fs::write(&path, "key-bytes").unwrap();
        let at = PathBuf::from(format!("@{}", path.display()));
        assert_eq!(read_key_file(&at, KEY_FLAG).unwrap(), "key-bytes");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inline_pem_says_to_pass_a_path() {
        let pem = PathBuf::from("-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----");
        let err = read_key_file(&pem, KEY_FLAG).unwrap_err();
        assert!(err.contains("takes the path to the .p8 file"), "{err}");
    }

    #[test]
    fn a_missing_file_names_the_path() {
        let err = read_key_file(Path::new("/nonexistent/AuthKey.p8"), KEY_FLAG).unwrap_err();
        assert!(err.contains("/nonexistent/AuthKey.p8"), "{err}");
    }

    /// The guard against passing the PEM itself covers the sandbox key
    /// too, and names the flag the operator actually used.
    #[test]
    fn inline_sandbox_pem_says_to_pass_a_path() {
        let pem = PathBuf::from("-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----");
        let err = read_key_file(&pem, SANDBOX_KEY_FLAG).unwrap_err();
        assert!(
            err.starts_with("--apns-sandbox-key-p8 takes the path"),
            "{err}"
        );
    }

    fn config(sandbox_p8: Option<PathBuf>, sandbox_key_id: Option<String>) -> ServeConfig {
        ServeConfig {
            listen: "127.0.0.1:8400".parse().unwrap(),
            apns_key_p8_path: PathBuf::from("/keys/AuthKey.p8"),
            apns_key_id: "ABCDE12345".into(),
            apns_sandbox_key_p8_path: sandbox_p8,
            apns_sandbox_key_id: sandbox_key_id,
            apns_team_id: "TEAM123456".into(),
            apns_topic: "com.example.app".into(),
            apns_endpoint: None,
            trusted_proxies: Vec::new(),
            limits: limits::LimitPolicy::default(),
        }
    }

    /// The existing single-key deployment must keep starting untouched.
    #[test]
    fn no_sandbox_key_configured_resolves_to_none() {
        assert!(read_sandbox_key(&config(None, None)).unwrap().is_none());
    }

    #[test]
    fn the_sandbox_key_is_read_from_its_own_file() {
        let dir = std::env::temp_dir().join("pm-pushgw-sandbox-key");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("AuthKey_Sandbox.p8");
        std::fs::write(&path, "sandbox-key-bytes").unwrap();

        let key = read_sandbox_key(&config(Some(path), Some("SANDB67890".into())))
            .unwrap()
            .expect("a configured sandbox key resolves");
        assert_eq!(key.key_p8, "sandbox-key-bytes");
        assert_eq!(key.key_id, "SANDB67890");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Half a pair must fail loudly: falling back to the primary key
    /// would leave sandbox pushes rejected exactly as before.
    #[test]
    fn half_a_sandbox_key_pair_names_the_missing_flag() {
        let err = read_sandbox_key(&config(Some(PathBuf::from("/keys/Sandbox.p8")), None))
            .err()
            .expect("a sandbox key path without an id is rejected");
        assert!(err.contains("--apns-sandbox-key-id"), "{err}");

        let err = read_sandbox_key(&config(None, Some("SANDB67890".into())))
            .err()
            .expect("a sandbox key id without a path is rejected");
        assert!(err.contains("--apns-sandbox-key-p8"), "{err}");
    }

    /// The startup line has to distinguish a two-key relay from a
    /// one-key one without printing key material.
    #[test]
    fn the_startup_line_reports_which_key_signs_sandbox_sends() {
        let mut identity = ApnsIdentity {
            topic: "com.example.app".into(),
            key_id: "ABCDE12345".into(),
            sandbox_key_id: None,
            team_id: "TEAM123456".into(),
            endpoint_override: None,
        };
        assert_eq!(identity.sandbox_key_field(), SHARES_PRIMARY_KEY);
        assert_eq!(identity.endpoint_field(), DEFAULT_ENDPOINT);

        identity.sandbox_key_id = Some("SANDB67890".into());
        assert_eq!(identity.sandbox_key_field(), "SANDB67890");
    }
}
