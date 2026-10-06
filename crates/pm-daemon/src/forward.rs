//! Published forward lifecycle and worker streams for HTTP routes and raw TCP listeners.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::forward_proxy::ForwardStream;

/// How long an accepted connection waits for the worker's dial outcome
/// and stream dial-back before it is dropped.
pub const STREAM_DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a proxied request waits for the target's response head
/// before the client is told the target did not answer.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Read buffer for the WebSocket-to-TCP splice.
const SPLICE_BUF_BYTES: usize = 16 * 1024;
pub(crate) const HTTP_ROUTE_LISTENER_PORT: u16 = 0;

/// Listener configuration, all optional: bind falls back to the HTTP
/// interface and the port range to OS-assigned ephemeral ports. The
/// host published in URLs is not configured here — see
/// [`resolve_forward_host`].
#[derive(Debug, Clone, Default)]
pub struct ForwardConfig {
    pub bind: Option<IpAddr>,
    pub port_range: Option<(u16, u16)>,
    /// Wildcard domain that mounts each HTTP forward on its own
    /// subdomain, `PM_SHARE_DOMAIN`. Set, it selects the share-domain
    /// mount for every forward.
    pub share_domain: Option<String>,
    /// Port range that mounts each HTTP forward on a listener of its
    /// own, `PM_SHARE_PORT_RANGE`. Read only when no share domain is
    /// set. Separate from `port_range`, which is raw TCP's.
    pub share_port_range: Option<(u16, u16)>,
    /// How long a proxied request waits for the target's response head,
    /// [`RESPONSE_TIMEOUT`] when unset. Not a flag: tests shorten it so a
    /// target that stops answering is reported within their time bound.
    pub response_timeout: Option<Duration>,
}

impl ForwardConfig {
    /// The mount every published HTTP forward uses, per
    /// [`crate::forward_mount::MountMode::resolve`].
    pub fn mount_mode(&self) -> crate::forward_mount::MountMode {
        crate::forward_mount::MountMode::resolve(
            self.share_domain.as_deref(),
            self.share_port_range,
        )
    }
}

/// Why the controller cannot name a host for a forward URL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForwardHostError {
    #[error("--public-url {0:?} has no host, so forward URLs cannot be composed from it")]
    MalformedPublicUrl(String),
    #[error(
        "forward listeners bind {0}, which no other device can reach, so no \
         URL composed from it would work. Pass --public-url with the address \
         users reach this controller at."
    )]
    UnreachableBind(IpAddr),
}

/// The host published forward URLs carry.
///
/// `--public-url` is the canonical answer and wins outright: it is the
/// one absolute URL an agent can print into chat and have work from
/// anywhere. `client_host` is the Host header of the request being
/// answered, used only when no canonical origin is configured so a
/// browser or phone gets a link back to the origin it already reached.
/// `listener_bind` is the last resort, and a loopback or wildcard bind
/// is a refusal rather than a guess: the controller genuinely does not
/// know a name any other device could open.
pub fn resolve_forward_host(
    public_url: Option<&str>,
    client_host: Option<&str>,
    listener_bind: IpAddr,
) -> Result<String, ForwardHostError> {
    if let Some(public_url) = public_url.map(str::trim).filter(|u| !u.is_empty()) {
        return authority_of(public_url)
            .and_then(host_of_authority)
            .ok_or_else(|| ForwardHostError::MalformedPublicUrl(public_url.to_string()));
    }
    if let Some(host) = client_host
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .and_then(host_of_authority)
    {
        return Ok(host);
    }
    if listener_bind.is_loopback() || listener_bind.is_unspecified() {
        return Err(ForwardHostError::UnreachableBind(listener_bind));
    }
    Ok(render_host_ip(listener_bind))
}

/// The interface one forward's own listener binds.
///
/// `authenticated` says whether reaching the listener is enough to reach the
/// agent's server. An HTTP forward requires a dashboard session, a forward
/// cookie or a scoped token, so following the dashboard's own bind exposes it
/// exactly where the dashboard already is. A raw TCP forward is a byte splice
/// with no check, and `publish_port` takes the target port from the agent
/// without the session having to own it, so an agent's tool call would
/// otherwise decide how far into the network a local service is exposed. It
/// stays on loopback until the operator names an interface.
pub fn listener_bind_ip(
    configured: Option<IpAddr>,
    dashboard: Option<IpAddr>,
    authenticated: bool,
) -> IpAddr {
    const LOOPBACK: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    if let Some(configured) = configured {
        return configured;
    }
    if !authenticated {
        return LOOPBACK;
    }
    dashboard.unwrap_or(LOOPBACK)
}

/// An IP as it appears in a URL, IPv6 bracketed.
fn render_host_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

fn authority_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().filter(|a| !a.is_empty())
}

/// The host of an authority (`host`, `host:port`, `[v6]:port`), kept in
/// the bracketed form a URL needs.
fn host_of_authority(authority: &str) -> Option<String> {
    let authority = authority.rsplit('@').next()?;
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &rest[..end];
        return (!host.is_empty()).then(|| format!("[{host}]"));
    }
    if let Ok(v6) = authority.parse::<std::net::Ipv6Addr>() {
        return Some(format!("[{v6}]"));
    }
    let host = authority.split(':').next()?;
    (!host.is_empty()).then(|| host.to_string())
}

struct BoundListener {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

/// An accepted connection parked until the worker dials back the
/// matching stream, keyed by the hash of its single-use dial token.
struct PendingStream {
    stream: ForwardStream,
    expires_at: Instant,
}

/// How long a scoped forward token is valid. Long enough to reopen and
/// reload a forwarded page across a working session rather than only to
/// survive the handoff into a browser.
pub const FORWARD_TOKEN_TTL: Duration = Duration::from_secs(48 * 60 * 60);

/// A short-lived token scoped to one forward, minted for the iOS
/// native-browser open flow.
struct ForwardToken {
    _user_id: u64,
    username: String,
    forward_id: u64,
    expires_at: Instant,
}

#[derive(Default)]
pub struct ForwardPool {
    listeners: Mutex<HashMap<u64, BoundListener>>,
    routes: Mutex<HashSet<u64>>,
    pending: Mutex<HashMap<String, PendingStream>>,
    target_reachable: Mutex<HashMap<u64, bool>>,
    forward_tokens: Mutex<HashMap<String, ForwardToken>>,
    /// The proxy's reusable upstream connections, so that every path
    /// which takes a forward out of service drops its connections with
    /// it rather than relying on each caller to remember.
    upstreams: std::sync::Arc<crate::forward_upstream::UpstreamPool>,
}

impl ForwardPool {
    pub fn new(upstreams: std::sync::Arc<crate::forward_upstream::UpstreamPool>) -> Self {
        Self {
            upstreams,
            ..Default::default()
        }
    }

    /// Records a bound listener for a forward, replacing (and aborting)
    /// any previous one.
    pub fn insert_listener(&self, forward_id: u64, port: u16, task: tokio::task::JoinHandle<()>) {
        if let Some(old) = self
            .listeners
            .lock()
            .unwrap()
            .insert(forward_id, BoundListener { port, task })
        {
            old.task.abort();
        }
    }

    pub fn insert_route(&self, forward_id: u64) {
        self.routes.lock().unwrap().insert(forward_id);
    }

    pub fn is_active(&self, forward_id: u64) -> bool {
        self.routes.lock().unwrap().contains(&forward_id)
            || self.listener_port(forward_id).is_some()
    }

    pub fn listener_port(&self, forward_id: u64) -> Option<u16> {
        self.listeners
            .lock()
            .unwrap()
            .get(&forward_id)
            .map(|l| l.port)
    }

    /// Unbinds a forward's listener. In-flight streams keep running;
    /// only new accepts stop.
    pub fn remove_listener(&self, forward_id: u64) {
        self.routes.lock().unwrap().remove(&forward_id);
        if let Some(l) = self.listeners.lock().unwrap().remove(&forward_id) {
            l.task.abort();
        }
        self.target_reachable.lock().unwrap().remove(&forward_id);
        self.upstreams.forget(forward_id);
    }

    /// Aborts every listener task and returns their handles so the
    /// caller can await full teardown. Routes are cleared too.
    pub fn drain_listeners(&self) -> Vec<tokio::task::JoinHandle<()>> {
        self.routes.lock().unwrap().clear();
        self.target_reachable.lock().unwrap().clear();
        self.upstreams.clear();
        let mut listeners = self.listeners.lock().unwrap();
        let handles: Vec<_> = listeners
            .drain()
            .map(|(_, l)| {
                l.task.abort();
                l.task
            })
            .collect();
        handles
    }

    /// Parks an accepted connection until the worker dials back with
    /// the token whose hash keys it.
    pub fn park_stream(&self, token_hash: String, stream: ForwardStream) {
        let mut pending = self.pending.lock().unwrap();
        let now = Instant::now();
        pending.retain(|_, p| p.expires_at > now);
        pending.insert(
            token_hash,
            PendingStream {
                stream,
                expires_at: now + STREAM_DIAL_TIMEOUT,
            },
        );
    }

    /// Claims a parked connection. Single-use: a second claim with the
    /// same token gets nothing, and an expired park is gone.
    pub fn claim_stream(&self, token_hash: &str) -> Option<ForwardStream> {
        let mut pending = self.pending.lock().unwrap();
        let stream = pending.remove(token_hash)?;
        (stream.expires_at > Instant::now()).then_some(stream.stream)
    }

    /// Mints a short-lived forward token scoped to one forward id.
    pub fn mint_forward_token(&self, user_id: u64, username: String, forward_id: u64) -> String {
        let token = crate::auth::generate_token();
        let hash = crate::auth::hash_token(&token);
        let mut tokens = self.forward_tokens.lock().unwrap();
        let now = Instant::now();
        tokens.retain(|_, t| t.expires_at > now);
        tokens.insert(
            hash,
            ForwardToken {
                _user_id: user_id,
                username,
                forward_id,
                expires_at: now + FORWARD_TOKEN_TTL,
            },
        );
        token
    }

    /// Verifies a forward token. Returns the username if the token is
    /// live and scoped to the given forward.
    ///
    /// The token is deliberately not consumed. A page load is many
    /// requests, and only the first carries the token from the address
    /// bar: consuming it left every stylesheet, script, and reload
    /// unauthenticated, which is what made an opened forward fall apart
    /// as soon as it fetched anything.
    pub fn verify_forward_token(&self, token: &str, forward_id: u64) -> Option<String> {
        let hash = crate::auth::hash_token(token);
        let mut tokens = self.forward_tokens.lock().unwrap();
        let now = Instant::now();
        tokens.retain(|_, t| t.expires_at > now);
        let entry = tokens.get(&hash)?;
        (entry.forward_id == forward_id).then(|| entry.username.clone())
    }

    /// Records the latest stream-open outcome, i.e. whether the worker
    /// could reach the target port. Returns true when the recorded
    /// value changed.
    pub fn record_target_reachable(&self, forward_id: u64, reachable: bool) -> bool {
        self.target_reachable
            .lock()
            .unwrap()
            .insert(forward_id, reachable)
            != Some(reachable)
    }

    /// Drops a forward's recorded outcome, for when its target moved
    /// and the verdict describes a server that is no longer there.
    pub fn forget_target_reachable(&self, forward_id: u64) {
        self.target_reachable.lock().unwrap().remove(&forward_id);
    }

    pub fn target_reachable(&self, forward_id: u64) -> Option<bool> {
        self.target_reachable
            .lock()
            .unwrap()
            .get(&forward_id)
            .copied()
    }
}

/// Binds a forward listener: the persisted port when possible so a
/// previously handed-out URL keeps working, else the configured range,
/// else an OS-assigned ephemeral port.
pub async fn bind_listener(
    ip: IpAddr,
    preferred_port: u16,
    range: Option<(u16, u16)>,
) -> std::io::Result<(TcpListener, u16)> {
    if preferred_port != 0 {
        if let Ok(listener) = TcpListener::bind((ip, preferred_port)).await {
            return Ok((listener, preferred_port));
        }
    }
    if let Some((lo, hi)) = range {
        for port in lo..=hi {
            if let Ok(listener) = TcpListener::bind((ip, port)).await {
                return Ok((listener, port));
            }
        }
        return Err(std::io::Error::other(format!(
            "no free forward port in {lo}-{hi}"
        )));
    }
    let listener = TcpListener::bind((ip, 0)).await?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Splices a worker's dialed-back stream WebSocket to the parked
/// browser connection until either side closes.
pub async fn splice_ws_tcp(frames: crate::worker_plane::FrameLink, fwd: ForwardStream) {
    let crate::worker_plane::FrameLink {
        out: sink,
        inbound: mut stream,
    } = frames;
    let (mut tcp_read, mut tcp_write) = tokio::io::split(fwd);

    let up = async {
        let mut buf = vec![0u8; SPLICE_BUF_BYTES];
        loop {
            let n = tcp_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            sink.send(Bytes::copy_from_slice(&buf[..n]))
                .await
                .map_err(std::io::Error::other)?;
        }
        Ok::<_, std::io::Error>(())
    };
    let down = async {
        while let Some(data) = stream.recv().await {
            tcp_write.write_all(&data).await?;
        }
        let _ = tcp_write.shutdown().await;
        Ok::<_, std::io::Error>(())
    };
    let _ = tokio::try_join!(up, down);
}

/// Keeps listeners aligned with the session lifecycle: a session
/// leaving the live states unbinds its forwards and stops the servers
/// behind its directory shares, one coming back (resume, worker
/// restore) rebinds and restarts them.
pub async fn run_reconciler(daemon: std::sync::Arc<crate::daemon::Daemon>) {
    use pm_protocol::domain::Event;
    let (_snapshot, mut events) = daemon.subscribe();
    loop {
        match events.recv().await {
            Ok(Event::SessionChanged(session)) => {
                if session.state.is_live() {
                    daemon.bind_session_forwards(session.id).await;
                    daemon.start_session_dir_shares(session.id).await;
                } else {
                    daemon.unbind_session_forwards(session.id);
                    daemon.stop_session_dir_shares(session.id);
                }
            }
            Ok(Event::SessionRemoved(session_id)) => {
                daemon.unbind_session_forwards(session_id);
                daemon.stop_session_dir_shares(session_id);
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parked_pool() -> (ForwardPool, ForwardStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dial = tokio::spawn(async move { tokio::net::TcpStream::connect(addr).await.unwrap() });
        let (accepted, _) = listener.accept().await.unwrap();
        let _client = dial.await.unwrap();
        (ForwardPool::default(), ForwardStream::plain(accepted))
    }

    #[tokio::test]
    async fn a_parked_stream_is_claimed_exactly_once() {
        let (pool, tcp) = parked_pool().await;
        pool.park_stream("hash".into(), tcp);
        assert!(pool.claim_stream("wrong").is_none());
        assert!(pool.claim_stream("hash").is_some());
        assert!(pool.claim_stream("hash").is_none());
    }

    fn ip(addr: &str) -> IpAddr {
        addr.parse().unwrap()
    }

    /// The deployment the item is about: the operator binds the dashboard to a
    /// tailnet or LAN address so a laptop or the phone can reach the web UI,
    /// which SECURITY.md contemplates, and sets no --forward-bind because they
    /// had no reason to. An authenticated preview follows that bind, because
    /// reaching it still takes a credential. A raw TCP splice must not, or an
    /// agent calling publish_port decides what of this host the network sees.
    #[test]
    fn only_an_authenticated_forward_follows_the_dashboard_off_loopback() {
        let lan: IpAddr = "192.168.1.10".parse().unwrap();
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();

        assert_eq!(listener_bind_ip(None, Some(lan), true), lan);
        assert_eq!(listener_bind_ip(None, Some(lan), false), loopback);

        // A wildcard bind is the same question with a worse answer.
        let wildcard: IpAddr = "0.0.0.0".parse().unwrap();
        assert_eq!(listener_bind_ip(None, Some(wildcard), true), wildcard);
        assert_eq!(listener_bind_ip(None, Some(wildcard), false), loopback);

        // The default bind exposes nothing either way.
        assert_eq!(listener_bind_ip(None, Some(loopback), true), loopback);
        assert_eq!(listener_bind_ip(None, Some(loopback), false), loopback);

        // And before the dashboard has bound anything there is nothing to follow.
        assert_eq!(listener_bind_ip(None, None, true), loopback);
        assert_eq!(listener_bind_ip(None, None, false), loopback);
    }

    /// --forward-bind is the operator saying where forward listeners go, so it
    /// answers for both kinds. It is the only way a raw TCP forward leaves this
    /// host.
    #[test]
    fn a_configured_forward_bind_wins_for_both_kinds() {
        let chosen: IpAddr = "10.0.0.7".parse().unwrap();
        let lan: IpAddr = "192.168.1.10".parse().unwrap();
        assert_eq!(listener_bind_ip(Some(chosen), Some(lan), true), chosen);
        assert_eq!(listener_bind_ip(Some(chosen), Some(lan), false), chosen);
        assert_eq!(listener_bind_ip(Some(chosen), None, false), chosen);
    }

    #[test]
    fn public_url_wins_over_every_other_source() {
        let host = resolve_forward_host(
            Some("https://pm.example.com:8443/dash"),
            Some("laptop.local:7676"),
            ip("192.168.1.5"),
        )
        .unwrap();
        assert_eq!(host, "pm.example.com");
    }

    #[test]
    fn public_url_keeps_a_bracketed_v6_literal() {
        let host =
            resolve_forward_host(Some("http://[2001:db8::5]:7676"), None, ip("127.0.0.1")).unwrap();
        assert_eq!(host, "[2001:db8::5]");
    }

    #[test]
    fn a_public_url_without_a_host_is_an_error() {
        assert_eq!(
            resolve_forward_host(Some("http:///dash"), None, ip("192.168.1.5")),
            Err(ForwardHostError::MalformedPublicUrl("http:///dash".into()))
        );
    }

    #[test]
    fn the_request_host_answers_when_no_public_url_is_set() {
        let host =
            resolve_forward_host(None, Some("laptop.local:7676"), ip("192.168.1.5")).unwrap();
        assert_eq!(host, "laptop.local");
    }

    #[test]
    fn a_loopback_request_host_is_right_for_that_client() {
        let host = resolve_forward_host(None, Some("localhost:7676"), ip("192.168.1.5")).unwrap();
        assert_eq!(host, "localhost");
    }

    #[test]
    fn the_bind_answers_when_nothing_else_does() {
        assert_eq!(
            resolve_forward_host(None, None, ip("192.168.1.5")).unwrap(),
            "192.168.1.5"
        );
        assert_eq!(
            resolve_forward_host(None, None, ip("2001:db8::5")).unwrap(),
            "[2001:db8::5]"
        );
    }

    #[test]
    fn a_loopback_bind_without_a_public_url_refuses() {
        assert_eq!(
            resolve_forward_host(None, None, ip("127.0.0.1")),
            Err(ForwardHostError::UnreachableBind(ip("127.0.0.1")))
        );
        assert_eq!(
            resolve_forward_host(None, None, ip("::1")),
            Err(ForwardHostError::UnreachableBind(ip("::1")))
        );
    }

    #[test]
    fn a_wildcard_bind_without_a_public_url_refuses() {
        assert_eq!(
            resolve_forward_host(None, None, ip("0.0.0.0")),
            Err(ForwardHostError::UnreachableBind(ip("0.0.0.0")))
        );
    }

    #[test]
    fn a_request_host_rescues_a_loopback_bind_for_that_client() {
        let host = resolve_forward_host(None, Some("localhost:7676"), ip("127.0.0.1")).unwrap();
        assert_eq!(host, "localhost");
    }

    #[test]
    fn an_empty_public_url_falls_through_instead_of_erroring() {
        let host = resolve_forward_host(Some("  "), None, ip("192.168.1.5")).unwrap();
        assert_eq!(host, "192.168.1.5");
    }

    #[tokio::test]
    async fn target_reachability_reports_changes_only() {
        let pool = ForwardPool::default();
        assert_eq!(pool.target_reachable(4), None);
        assert!(pool.record_target_reachable(4, true));
        assert!(!pool.record_target_reachable(4, true));
        assert!(pool.record_target_reachable(4, false));
        assert_eq!(pool.target_reachable(4), Some(false));
    }

    #[tokio::test]
    async fn preferred_port_wins_and_a_taken_port_falls_back() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let (first, port) = bind_listener(ip, 0, None).await.unwrap();
        let (_second, second_port) = bind_listener(ip, port, None).await.unwrap();
        // The preferred port is held by `first`, so the bind falls back
        // to a fresh ephemeral port instead of failing.
        assert_ne!(second_port, port);
        drop(first);
        let (_third, third_port) = bind_listener(ip, port, None).await.unwrap();
        assert_eq!(third_port, port);
    }

    #[test]
    fn forward_token_verifies_for_correct_forward() {
        let pool = ForwardPool::default();
        let token = pool.mint_forward_token(1, "alice".into(), 42);
        assert_eq!(pool.verify_forward_token(&token, 42), Some("alice".into()));
    }

    #[test]
    fn a_forward_token_verifies_repeatedly_within_its_lifetime() {
        let pool = ForwardPool::default();
        let token = pool.mint_forward_token(1, "alice".into(), 42);
        for _ in 0..3 {
            assert_eq!(
                pool.verify_forward_token(&token, 42).as_deref(),
                Some("alice")
            );
        }
    }

    /// The lifetime is what bounds a forward token, so it is worth
    /// stating: a link handed to a browser has to outlive the tab.
    #[test]
    fn a_forward_token_lives_for_two_days() {
        assert_eq!(FORWARD_TOKEN_TTL, Duration::from_secs(48 * 60 * 60));
    }

    #[test]
    fn forward_token_rejects_wrong_forward() {
        let pool = ForwardPool::default();
        let token = pool.mint_forward_token(1, "alice".into(), 42);
        assert!(pool.verify_forward_token(&token, 99).is_none());
    }

    #[test]
    fn forward_token_rejects_bogus() {
        let pool = ForwardPool::default();
        pool.mint_forward_token(1, "alice".into(), 42);
        assert!(pool.verify_forward_token("bogus", 42).is_none());
    }
}
