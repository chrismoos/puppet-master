//! Saved logins to remote controllers: where they live on disk, how one
//! is made, and how its access token is kept current.
//!
//! A login is a device enrollment on the controller, the same kind the
//! phone holds: a short-lived access token and a rotating refresh token.
//! Every `pm` invocation is its own process, so rotation is serialized
//! through a lock file. Without it two commands started together would
//! both present the same refresh token, and the controller treats a
//! token presented too often as stolen and revokes the device.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pm_tls::{KeyHash, WebTrust};
use serde::{Deserialize, Serialize};

/// The name that always means the daemon on this machine's unix socket,
/// so it can never be given to a remote.
pub const LOCAL_NAME: &str = "local";

const NAME_MAX_CHARS: usize = 40;
const PROFILE_EXTENSION: &str = "json";
const LOCK_EXTENSION: &str = "lock";
const STORE_DIR_MODE: u32 = 0o700;
const PROFILE_FILE_MODE: u32 = 0o600;

/// A token this close to expiry is refreshed before use, so a request
/// started now does not arrive after the controller stopped honoring it.
const ACCESS_TOKEN_EXPIRY_MARGIN_MS: i64 = 60 * 1000;

const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const DEVICE_PLATFORM: &str = "cli";
const APP_INSTALLATION_ID_BYTES: usize = 16;

const HTTPS_SCHEME: &str = "https";
const HTTP_SCHEME: &str = "http";
const HTTPS_DEFAULT_PORT: u16 = 443;
const HTTP_DEFAULT_PORT: u16 = 80;

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("{0}")]
    InvalidName(String),
    #[error("{0}")]
    InvalidUrl(String),
    #[error("no saved login named {0:?}, sign in with `pm login <url> --name {0}`")]
    NotLoggedIn(String),
    #[error("the login for {0:?} is no longer valid, sign in again with `pm login`")]
    LoginExpired(String),
    #[error("the controller refused the username or password")]
    InvalidCredentials,
    #[error("{0}")]
    Rejected(String),
    #[error("cannot reach {url}: {detail}")]
    Unreachable { url: String, detail: String },
    #[error("{0} did not answer as a Puppet Master controller")]
    NotAController(String),
    #[error("saved login {name:?} is unreadable: {detail}")]
    CorruptProfile { name: String, detail: String },
    #[error("TLS setup failed: {0}")]
    Tls(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Checks a remote's name: it becomes a file name and a command-line
/// argument, and must not shadow the name of the local daemon.
pub fn validate_name(name: &str) -> Result<(), RemoteError> {
    let well_formed = !name.is_empty()
        && name.chars().count() <= NAME_MAX_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && name.starts_with(|c: char| c.is_ascii_alphanumeric());
    if !well_formed {
        return Err(RemoteError::InvalidName(format!(
            "a controller name is 1 to {NAME_MAX_CHARS} letters, digits, '-' or '_', \
             starting with a letter or digit, got {name:?}"
        )));
    }
    if name == LOCAL_NAME {
        return Err(RemoteError::InvalidName(format!(
            "{LOCAL_NAME:?} always names the daemon on this machine, pick another name"
        )));
    }
    Ok(())
}

/// Where a controller's dashboard listens, reduced to what a connection
/// needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerUrl {
    pub secure: bool,
    pub host: String,
    pub port: u16,
}

impl ControllerUrl {
    pub fn parse(text: &str) -> Result<Self, RemoteError> {
        let invalid = |why: &str| RemoteError::InvalidUrl(format!("{text:?} {why}"));
        let url = reqwest::Url::parse(text.trim())
            .map_err(|_| invalid("is not a URL, expected https://host[:port]"))?;
        let secure = match url.scheme() {
            HTTPS_SCHEME => true,
            HTTP_SCHEME => false,
            _ => return Err(invalid("must start with https:// or http://")),
        };
        if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return Err(invalid("must name only a host and port, with no path"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid("must not carry a username or password"));
        }
        let host = url
            .host_str()
            .ok_or_else(|| invalid("has no host"))?
            .to_string();
        let default_port = if secure {
            HTTPS_DEFAULT_PORT
        } else {
            HTTP_DEFAULT_PORT
        };
        Ok(Self {
            secure,
            host,
            port: url.port().unwrap_or(default_port),
        })
    }

    fn authority(&self) -> String {
        let default_port = if self.secure {
            HTTPS_DEFAULT_PORT
        } else {
            HTTP_DEFAULT_PORT
        };
        if self.port == default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The base URL API requests are made against, without a trailing slash.
    pub fn http_base(&self) -> String {
        let scheme = if self.secure {
            HTTPS_SCHEME
        } else {
            HTTP_SCHEME
        };
        format!("{scheme}://{}", self.authority())
    }

    pub(crate) fn ws_url(&self, path_and_query: &str) -> String {
        let scheme = if self.secure { "wss" } else { "ws" };
        format!("{scheme}://{}{path_and_query}", self.authority())
    }

    /// The host as a URL writes it, which brackets an IPv6 address; a
    /// socket address and a TLS server name both want it bare.
    pub(crate) fn bare_host(&self) -> &str {
        self.host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(&self.host)
    }

    /// Whether traffic to this host never leaves the machine, which is the
    /// one case a plaintext URL exposes nothing.
    pub fn is_loopback(&self) -> bool {
        self.bare_host() == "localhost"
            || self
                .bare_host()
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }
}

/// One saved login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProfile {
    pub url: String,
    pub username: String,
    /// The key the controller's certificate must carry, set when no system
    /// root vouches for it. Absent means system roots decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_key: Option<String>,
    /// This login's own identity to the controller. Kept across sign-ins
    /// under the same name so they land on one device record.
    pub app_installation_id: String,
    pub device_id: String,
    pub access_token: String,
    pub access_token_expires_at_unix_ms: i64,
    pub refresh_token: String,
    pub refresh_token_expires_at_unix_ms: i64,
}

impl RemoteProfile {
    pub fn controller_url(&self) -> Result<ControllerUrl, RemoteError> {
        ControllerUrl::parse(&self.url)
    }

    pub fn trust(&self) -> Result<WebTrust, RemoteError> {
        match self.pinned_key.as_deref() {
            None => Ok(WebTrust::SystemRoots),
            Some(hex) => KeyHash::from_hex(hex)
                .map(WebTrust::Pinned)
                .ok_or_else(|| RemoteError::Tls("the saved pinned key is not a key hash".into())),
        }
    }

    fn access_token_is_current(&self, now_unix_ms: i64) -> bool {
        now_unix_ms + ACCESS_TOKEN_EXPIRY_MARGIN_MS < self.access_token_expires_at_unix_ms
    }
}

/// The directory saved logins live in, one file per name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStore {
    dir: PathBuf,
}

impl RemoteStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, name: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{name}.{extension}"))
    }

    /// Every saved login's name, sorted.
    pub fn names(&self) -> Result<Vec<String>, RemoteError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                if path.extension()?.to_str()? != PROFILE_EXTENSION {
                    return None;
                }
                let name = path.file_stem()?.to_str()?.to_string();
                validate_name(&name).ok().map(|_| name)
            })
            .collect();
        names.sort();
        Ok(names)
    }

    pub fn load(&self, name: &str) -> Result<RemoteProfile, RemoteError> {
        validate_name(name)?;
        let bytes = match std::fs::read(self.path(name, PROFILE_EXTENSION)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(RemoteError::NotLoggedIn(name.to_string()))
            }
            Err(e) => return Err(e.into()),
        };
        serde_json::from_slice(&bytes).map_err(|e| RemoteError::CorruptProfile {
            name: name.to_string(),
            detail: e.to_string(),
        })
    }

    /// Replaces the saved login in one rename, so a command reading it
    /// while another rotates the tokens sees the old file or the new one
    /// and never a partial write.
    pub fn save(&self, name: &str, profile: &RemoteProfile) -> Result<(), RemoteError> {
        validate_name(name)?;
        self.ensure_dir()?;
        let mut staged = tempfile::Builder::new()
            .prefix(&format!(".{name}."))
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(
                PROFILE_FILE_MODE,
            ))
            .tempfile_in(&self.dir)?;
        staged.write_all(&serde_json::to_vec_pretty(profile).expect("a profile serializes"))?;
        staged.as_file().sync_all()?;
        staged
            .persist(self.path(name, PROFILE_EXTENSION))
            .map_err(|e| e.error)?;
        Ok(())
    }

    /// Forgets a saved login. Returns whether there was one.
    pub fn remove(&self, name: &str) -> Result<bool, RemoteError> {
        validate_name(name)?;
        let _ = std::fs::remove_file(self.path(name, LOCK_EXTENSION));
        match std::fs::remove_file(self.path(name, PROFILE_EXTENSION)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn ensure_dir(&self) -> Result<(), RemoteError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(STORE_DIR_MODE)
            .create(&self.dir)?;
        Ok(())
    }

    /// Holds the per-login lock until the returned file is dropped.
    async fn lock(&self, name: &str) -> Result<std::fs::File, RemoteError> {
        self.ensure_dir()?;
        let path = self.path(name, LOCK_EXTENSION);
        let file = tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(PROFILE_FILE_MODE)
                .open(path)?;
            file.lock()?;
            Ok::<_, std::io::Error>(file)
        })
        .await
        .map_err(std::io::Error::other)??;
        Ok(file)
    }

    /// The saved login with an access token the controller will honor,
    /// rotating the tokens first when the saved one has run out.
    pub async fn current(&self, name: &str) -> Result<RemoteProfile, RemoteError> {
        let profile = self.load(name)?;
        if profile.access_token_is_current(now_unix_ms()) {
            return Ok(profile);
        }
        let _lock = self.lock(name).await?;
        // Whoever held the lock before this may have rotated already, and
        // the token read above is then spent.
        let profile = self.load(name)?;
        let now = now_unix_ms();
        if profile.access_token_is_current(now) {
            return Ok(profile);
        }
        if now >= profile.refresh_token_expires_at_unix_ms {
            return Err(RemoteError::LoginExpired(name.to_string()));
        }
        let refreshed = refresh(name, &profile).await?;
        self.save(name, &refreshed)?;
        Ok(refreshed)
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn tls_config(trust: &WebTrust) -> Result<rustls::ClientConfig, RemoteError> {
    pm_tls::web_client_config(trust).map_err(|e| RemoteError::Tls(e.to_string()))
}

fn http_client(url: &ControllerUrl, trust: &WebTrust) -> Result<reqwest::Client, RemoteError> {
    let mut builder = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("pm/", env!("CARGO_PKG_VERSION")));
    if url.secure {
        builder = builder.use_preconfigured_tls(tls_config(trust)?);
    }
    builder.build().map_err(|e| RemoteError::Tls(e.to_string()))
}

fn unreachable(url: &ControllerUrl, error: &dyn std::error::Error) -> RemoteError {
    let mut detail = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        detail = format!("{detail}: {cause}");
        source = cause.source();
    }
    RemoteError::Unreachable {
        url: url.http_base(),
        detail,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokensBody {
    access_token: String,
    access_token_expires_at_unix_ms: i64,
    refresh_token: String,
    refresh_token_expires_at_unix_ms: i64,
}

#[derive(Deserialize)]
struct DeviceBody {
    id: String,
}

#[derive(Deserialize)]
struct EnrollBody {
    device: DeviceBody,
    tokens: TokensBody,
}

#[derive(Deserialize)]
struct RefreshBody {
    tokens: TokensBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

async fn error_text(response: reqwest::Response) -> String {
    let status = response.status();
    match response.json::<ErrorBody>().await {
        Ok(body) => body.error,
        Err(_) => format!("the controller answered {status}"),
    }
}

async fn refresh(name: &str, profile: &RemoteProfile) -> Result<RemoteProfile, RemoteError> {
    let url = profile.controller_url()?;
    let response = http_client(&url, &profile.trust()?)?
        .post(format!("{}/api/mobile/devices/refresh", url.http_base()))
        .json(&serde_json::json!({ "refreshToken": profile.refresh_token }))
        .send()
        .await
        .map_err(|e| unreachable(&url, &e))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(RemoteError::LoginExpired(name.to_string()));
    }
    if !response.status().is_success() {
        return Err(RemoteError::Rejected(error_text(response).await));
    }
    let body: RefreshBody = response
        .json()
        .await
        .map_err(|_| RemoteError::NotAController(url.http_base()))?;
    Ok(RemoteProfile {
        access_token: body.tokens.access_token,
        access_token_expires_at_unix_ms: body.tokens.access_token_expires_at_unix_ms,
        refresh_token: body.tokens.refresh_token,
        refresh_token_expires_at_unix_ms: body.tokens.refresh_token_expires_at_unix_ms,
        ..profile.clone()
    })
}

/// What a first look at a controller's listener found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustProbe {
    /// Plain HTTP: there is no certificate to judge.
    Plaintext,
    /// The certificate chains to a system root.
    Trusted,
    /// No system root vouches for the certificate. Carries the key it
    /// presented, for a person to compare before pinning it.
    Untrusted { key: KeyHash },
}

/// Finds out whether system roots vouch for the controller, and reads the
/// key it presents when they do not. Sends nothing secret.
pub async fn probe_trust(url: &ControllerUrl) -> Result<TrustProbe, RemoteError> {
    if !url.secure {
        return Ok(TrustProbe::Plaintext);
    }
    match crate::transport::tls_handshake(url, &WebTrust::SystemRoots).await {
        Ok(_) => Ok(TrustProbe::Trusted),
        Err(crate::transport::DialError::Certificate(_)) => {
            let key = crate::transport::tls_handshake(url, &WebTrust::ReadKeyOnly)
                .await
                .map_err(|e| unreachable(url, &e))?;
            Ok(TrustProbe::Untrusted { key })
        }
        Err(e) => Err(unreachable(url, &e)),
    }
}

pub struct LoginRequest<'a> {
    pub name: &'a str,
    pub url: &'a ControllerUrl,
    pub trust: WebTrust,
    pub username: &'a str,
    pub password: &'a str,
    /// What the controller's device list shows this login as.
    pub device_name: &'a str,
}

/// Signs in with a username and password and saves the login under its
/// name, replacing an earlier one with that name.
pub async fn login(
    store: &RemoteStore,
    request: LoginRequest<'_>,
) -> Result<RemoteProfile, RemoteError> {
    validate_name(request.name)?;
    let url = request.url;
    let client = http_client(url, &request.trust)?;
    let version = client
        .get(format!("{}/api/version", url.http_base()))
        .send()
        .await
        .map_err(|e| unreachable(url, &e))?;
    let is_controller = version.status().is_success()
        && version
            .json::<serde_json::Value>()
            .await
            .is_ok_and(|body| body.get("installationId").is_some());
    if !is_controller {
        return Err(RemoteError::NotAController(url.http_base()));
    }

    // Signing in again under a name reuses its installation id only for
    // the same controller: a name pointed somewhere new is a new device.
    let app_installation_id = store
        .load(request.name)
        .ok()
        .filter(|previous| previous.url == url.http_base())
        .map(|previous| previous.app_installation_id)
        .unwrap_or_else(new_app_installation_id);
    let response = client
        .post(format!("{}/api/mobile/devices/enroll", url.http_base()))
        .json(&serde_json::json!({
            "deviceId": app_installation_id,
            "name": request.device_name,
            "platform": DEVICE_PLATFORM,
            "username": request.username,
            "password": request.password,
        }))
        .send()
        .await
        .map_err(|e| unreachable(url, &e))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(RemoteError::InvalidCredentials);
    }
    if !response.status().is_success() {
        return Err(RemoteError::Rejected(error_text(response).await));
    }
    let body: EnrollBody = response
        .json()
        .await
        .map_err(|_| RemoteError::NotAController(url.http_base()))?;
    let profile = RemoteProfile {
        url: url.http_base(),
        username: request.username.to_string(),
        pinned_key: match request.trust {
            WebTrust::Pinned(key) => Some(key.to_hex()),
            WebTrust::SystemRoots | WebTrust::ReadKeyOnly => None,
        },
        app_installation_id,
        device_id: body.device.id,
        access_token: body.tokens.access_token,
        access_token_expires_at_unix_ms: body.tokens.access_token_expires_at_unix_ms,
        refresh_token: body.tokens.refresh_token,
        refresh_token_expires_at_unix_ms: body.tokens.refresh_token_expires_at_unix_ms,
    };
    store.save(request.name, &profile)?;
    Ok(profile)
}

fn new_app_installation_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; APP_INSTALLATION_ID_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Whether the controller was told to drop the device, which a logout
/// reports but does not depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revocation {
    Revoked,
    /// The controller could not be reached or refused, so the device
    /// record there outlives the saved login.
    NotConfirmed,
}

/// Forgets a saved login, first asking the controller to revoke the
/// device so its tokens stop working there too.
pub async fn logout(store: &RemoteStore, name: &str) -> Result<Revocation, RemoteError> {
    let revocation = match store.current(name).await {
        Ok(profile) => revoke(&profile).await,
        Err(RemoteError::NotLoggedIn(name)) => return Err(RemoteError::NotLoggedIn(name)),
        Err(_) => Revocation::NotConfirmed,
    };
    store.remove(name)?;
    Ok(revocation)
}

async fn revoke(profile: &RemoteProfile) -> Revocation {
    let attempt = async {
        let url = profile.controller_url().ok()?;
        let response = http_client(&url, &profile.trust().ok()?)
            .ok()?
            .delete(format!(
                "{}/api/mobile/devices/{}",
                url.http_base(),
                profile.device_id
            ))
            .bearer_auth(&profile.access_token)
            .send()
            .await
            .ok()?;
        response.status().is_success().then_some(())
    };
    match attempt.await {
        Some(()) => Revocation::Revoked,
        None => Revocation::NotConfirmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn profile(access_expires_at_unix_ms: i64) -> RemoteProfile {
        RemoteProfile {
            url: "https://pm.example".into(),
            username: "testuser".into(),
            pinned_key: None,
            app_installation_id: "a1".into(),
            device_id: "7".into(),
            access_token: "access".into(),
            access_token_expires_at_unix_ms: access_expires_at_unix_ms,
            refresh_token: "refresh".into(),
            refresh_token_expires_at_unix_ms: i64::MAX,
        }
    }

    #[test]
    fn names_are_file_safe_and_never_the_local_name() {
        for good in ["prod", "home-2", "a_b", "9lives"] {
            validate_name(good).unwrap();
        }
        for bad in [
            "",
            "local",
            "-x",
            "a/b",
            "a b",
            "..",
            "a.b",
            &"x".repeat(41),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_controller_url_keeps_only_scheme_host_and_port() {
        let url = ControllerUrl::parse("https://pm.example/").unwrap();
        assert_eq!(url.http_base(), "https://pm.example");
        assert_eq!(url.port, 443);
        assert_eq!(url.ws_url("/ws"), "wss://pm.example/ws");

        let url = ControllerUrl::parse("http://127.0.0.1:7676").unwrap();
        assert_eq!(url.http_base(), "http://127.0.0.1:7676");
        assert_eq!(url.ws_url("/ws"), "ws://127.0.0.1:7676/ws");
        assert!(url.is_loopback());

        let url = ControllerUrl::parse("https://[::1]:8443").unwrap();
        assert_eq!(url.bare_host(), "::1");
        assert_eq!(url.http_base(), "https://[::1]:8443");
        assert!(url.is_loopback());
        assert!(!ControllerUrl::parse("http://pm.example")
            .unwrap()
            .is_loopback());
    }

    #[test]
    fn a_controller_url_refuses_what_a_connection_cannot_use() {
        for bad in [
            "pm.example",
            "ftp://pm.example",
            "https://pm.example/base",
            "https://pm.example/?x=1",
            "https://user:pw@pm.example",
            "wss://pm.example",
        ] {
            assert!(
                matches!(ControllerUrl::parse(bad), Err(RemoteError::InvalidUrl(_))),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn a_saved_login_round_trips_and_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RemoteStore::new(tmp.path().join("remotes"));
        assert!(store.names().unwrap().is_empty());
        assert!(matches!(
            store.load("prod"),
            Err(RemoteError::NotLoggedIn(name)) if name == "prod"
        ));

        store.save("prod", &profile(10)).unwrap();
        store.save("home", &profile(20)).unwrap();
        assert_eq!(store.names().unwrap(), ["home", "prod"]);
        assert_eq!(store.load("prod").unwrap(), profile(10));

        let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(tmp.path().join("remotes")), STORE_DIR_MODE);
        assert_eq!(
            mode(tmp.path().join("remotes/prod.json")),
            PROFILE_FILE_MODE
        );

        assert!(store.remove("prod").unwrap());
        assert!(!store.remove("prod").unwrap());
        assert_eq!(store.names().unwrap(), ["home"]);
    }

    #[test]
    fn an_access_token_is_spent_a_margin_before_it_expires() {
        let now = 1_000_000;
        assert!(profile(now + ACCESS_TOKEN_EXPIRY_MARGIN_MS + 1).access_token_is_current(now));
        assert!(!profile(now + ACCESS_TOKEN_EXPIRY_MARGIN_MS).access_token_is_current(now));
        assert!(!profile(now - 1).access_token_is_current(now));
    }

    #[tokio::test]
    async fn a_current_token_is_used_without_touching_the_controller() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RemoteStore::new(tmp.path().to_path_buf());
        let saved = profile(i64::MAX);
        store.save("prod", &saved).unwrap();
        assert_eq!(store.current("prod").await.unwrap(), saved);
    }

    #[tokio::test]
    async fn a_login_whose_refresh_token_ran_out_asks_for_a_new_sign_in() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RemoteStore::new(tmp.path().to_path_buf());
        let mut saved = profile(0);
        saved.refresh_token_expires_at_unix_ms = 1;
        store.save("prod", &saved).unwrap();
        assert!(matches!(
            store.current("prod").await,
            Err(RemoteError::LoginExpired(name)) if name == "prod"
        ));
    }
}
