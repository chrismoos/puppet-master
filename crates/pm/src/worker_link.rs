//! The host's end of the mutually authenticated worker plane.
//!
//! A controller can start processes on this machine, so the host pins the
//! controller's public key and refuses to speak to anything else. The pin is
//! established once, during enrollment, and every later connection is
//! authenticated by the handshake alone.

use anyhow::{anyhow, Context};
use pm_tls::{Identity, KeyHash, PeerPolicy};
use tokio::net::TcpStream;
use tokio_rustls::TlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, info};

use crate::paths;

/// One worker-plane connection. The variant is which end opened the socket;
/// everything above this cares only that it carries frames.
pub type Link = WebSocketStream<TlsStream<TcpStream>>;

/// One established connection, and what the handshake proved about the peer.
pub struct Connected {
    pub link: Link,
    pub controller_key: KeyHash,
    pub exporter: [u8; 32],
}

/// This host's long-lived keypair, generated on first run.
pub fn identity(name: Option<&str>) -> anyhow::Result<Identity> {
    let path = paths::worker_key_path(name);
    if let Ok(pem) = std::fs::read_to_string(&path) {
        return Ok(Identity::from_key_pem(&pem)?);
    }
    let identity = Identity::generate()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, identity.key_pem()?)
        .with_context(|| format!("writing {}", path.display()))?;
    restrict(&path)?;
    Ok(identity)
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}

/// Why a dial failed. Only a refused key says anything about who answered:
/// a controller that is restarting, unreachable, or resetting the socket
/// looks the same as one that was rebuilt, and re-pairing on that would
/// spend an enrollment the host did not need to spend.
#[derive(Debug)]
pub struct DialError {
    error: anyhow::Error,
    refused_key: bool,
}

impl DialError {
    /// The handshake ran and this host refused the key the controller
    /// presented.
    pub fn refused_key(&self) -> bool {
        self.refused_key
    }
}

impl From<DialError> for anyhow::Error {
    fn from(failure: DialError) -> Self {
        failure.error
    }
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

#[cfg(test)]
impl DialError {
    pub(crate) fn for_test(refused_key: bool) -> Self {
        Self {
            error: anyhow!("dial failed"),
            refused_key,
        }
    }
}

/// Dials one worker-plane endpoint. `pinned` is the controller key this host
/// already trusts; without one the connection is an enrollment attempt and
/// the caller must complete the pairing proof before acting on anything the
/// controller says.
pub async fn dial(
    identity: &Identity,
    pinned: Option<KeyHash>,
    url: &str,
    bearer: Option<&str>,
) -> Result<Connected, DialError> {
    debug!(
        url,
        policy = if pinned.is_some() {
            "pinned"
        } else {
            "pairing"
        },
        "dialing the controller"
    );
    let started = std::time::Instant::now();
    match connect(identity, pinned, url, bearer).await {
        Ok(connected) => {
            info!(
                url,
                controller_key = %connected.controller_key,
                took_ms = started.elapsed().as_millis(),
                "opened a link to the controller"
            );
            Ok(connected)
        }
        Err(error) => {
            let refused_key = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(pm_tls::refused_key);
            // The caller reports the failure in the terms of whatever it
            // was opening; this adds the handshake detail that only the
            // dial knows.
            debug!(
                url,
                refused_key,
                took_ms = started.elapsed().as_millis(),
                error = %error,
                "could not open a link to the controller"
            );
            Err(DialError { error, refused_key })
        }
    }
}

/// SO_SNDBUF for sockets the worker dials to its controller.
const WORKER_PLANE_SOCKET_BUFFER_BYTES: usize = 32 * 1024;

async fn connect(
    identity: &Identity,
    pinned: Option<KeyHash>,
    url: &str,
    bearer: Option<&str>,
) -> anyhow::Result<Connected> {
    let policy = match pinned {
        Some(hash) => PeerPolicy::Pinned(hash),
        None => PeerPolicy::Pairing,
    };
    let config = pm_tls::client_config(identity, &policy)?;
    let address = authority(url)?;
    let tcp = TcpStream::connect(&address)
        .await
        .with_context(|| format!("connecting to {address}"))?;
    tcp.set_nodelay(true)?;
    // A small send buffer keeps a flood's in-flight bytes, and so a
    // keystroke echo's wait behind them, bounded by the budget rather
    // than by what the kernel autotunes for throughput.
    let _ = socket2::SockRef::from(&tcp).set_send_buffer_size(WORKER_PLANE_SOCKET_BUFFER_BYTES);
    let stream = tokio_rustls::TlsConnector::from(config)
        .connect(pm_tls::peer_server_name(), tcp)
        .await
        .context("controller did not present the expected key")?;
    let (_, connection) = stream.get_ref();
    let controller_key = pm_tls::peer_key_hash(connection.peer_certificates())
        .ok_or_else(|| anyhow!("controller presented no key"))?;
    let exporter = pm_tls::pairing::exporter(connection)?;

    let mut request = url.into_client_request()?;
    if let Some(token) = bearer {
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse()?,
        );
    }
    let (link, _) = tokio_tungstenite::client_async(request, TlsStream::from(stream)).await?;
    Ok(Connected {
        link,
        controller_key,
        exporter,
    })
}

/// The host:port to open a socket to. Only `wss://` is accepted: the worker
/// plane is mutually authenticated end to end, and silently accepting a
/// plaintext URL would make a typo indistinguishable from a downgrade.
fn authority(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("wss://")
        .ok_or_else(|| anyhow!("controller URL must start with wss://, got {url}"))?;
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(anyhow!("controller URL has no host: {url}"));
    }
    Ok(match authority.rsplit_once(':') {
        Some((_, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => {
            authority.to_string()
        }
        _ => format!("{authority}:{DEFAULT_WORKER_PORT}"),
    })
}

/// Matches the controller's default `--worker-listen` port.
const DEFAULT_WORKER_PORT: u16 = 7677;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plaintext_controller_url_is_refused() {
        assert!(authority("ws://host:7677/worker").is_err());
        assert!(authority("http://host/worker").is_err());
        assert!(authority("wss://").is_err());
    }

    #[test]
    fn the_authority_carries_the_default_port_when_none_is_given() {
        assert_eq!(authority("wss://host/worker").unwrap(), "host:7677");
        assert_eq!(
            authority("wss://host:9000/worker/terminal").unwrap(),
            "host:9000"
        );
        assert_eq!(authority("wss://10.0.0.4:22").unwrap(), "10.0.0.4:22");
    }

    /// Serves one TLS connection as a controller holding `identity`, then
    /// stops. What it says afterwards does not matter: the dial under test
    /// fails, or does not, on the key alone.
    async fn controller_with_key(identity: Identity) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let config = pm_tls::server_config(&identity, &PeerPolicy::Pairing).unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            let _ = tokio_rustls::TlsAcceptor::from(config).accept(tcp).await;
        });
        addr
    }

    /// The distinction the reconnect path depends on: a controller that was
    /// rebuilt behind the same address is a refused key, and re-pairing is
    /// the way back.
    #[tokio::test]
    async fn a_controller_holding_another_key_is_refused_on_its_key() {
        let host = Identity::generate().unwrap();
        let pinned = Identity::generate().unwrap().key_hash();
        let addr = controller_with_key(Identity::generate().unwrap()).await;
        let failure = dial(&host, Some(pinned), &format!("wss://{addr}/worker"), None)
            .await
            .err()
            .expect("a key this host did not pin must not connect");
        assert!(failure.refused_key(), "{failure}");
    }

    /// The failure this classification exists for: a controller that is
    /// restarting still holds the key this host pinned, so nothing about it
    /// says the enrollment should be spent again.
    #[tokio::test]
    async fn a_controller_that_is_not_listening_is_not_a_refused_key() {
        let pinned = Identity::generate().unwrap().key_hash();
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        let failure = dial(
            &Identity::generate().unwrap(),
            Some(pinned),
            &format!("wss://{addr}/worker"),
            None,
        )
        .await
        .err()
        .expect("nothing is listening");
        assert!(!failure.refused_key(), "{failure}");
    }

    #[test]
    fn a_generated_identity_is_stable_across_reloads() {
        let first = Identity::generate().unwrap();
        let pem = first.key_pem().unwrap();
        assert_eq!(
            Identity::from_key_pem(&pem).unwrap().key_hash(),
            first.key_hash()
        );
    }
}
