//! The host's listener, for a controller that dials in.
//!
//! A host reachable on a private interface but unable to open connections
//! back inverts who dials. Nothing else inverts: the host still registers
//! and the controller still dispatches work.
//!
//! What arrives here is a controller asking to run processes on this
//! machine, so the bar is the same one the controller applies in the other
//! direction. Only a key this host has pinned gets through the handshake,
//! and the one exception — enrollment — is a window the operator opens
//! deliberately with a token, in which the caller proves it holds that token
//! before this host answers anything.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use futures::{SinkExt, StreamExt};
use pm_protocol::worker_frame::{self, WorkerFrame};
use pm_tls::pairing::{Side, Transcript};
use pm_tls::{Identity, KeyHash, PeerPolicy};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_rustls::TlsStream;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, trace, warn};

use crate::worker_link::Link;

/// A caller that stalls before proving itself holds a slot, so the number of
/// unproven connections is bounded rather than whatever reaches the port.
const MAX_UNPROVEN: usize = 8;
/// How long one connection has to finish TLS, the WebSocket upgrade, and its
/// proof. Generous for a slow link, far short of holding a slot indefinitely.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(20);
/// Connection attempts allowed from one address per window.
const RATE_LIMIT: usize = 30;
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// How long an enrollment stays open. The controller's own token expires on
/// its side, but the host cannot see that, and a window left open is a
/// window something else can keep knocking at.
const ENROLLMENT_WINDOW: Duration = Duration::from_secs(15 * 60);

/// An address range permitted to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u32,
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl Cidr {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (
                addr,
                prefix
                    .parse::<u32>()
                    .with_context(|| format!("prefix length in {text}"))?,
            ),
            None => (text, u32::MAX),
        };
        let addr: IpAddr = addr.parse().with_context(|| format!("address in {text}"))?;
        let width = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = if prefix == u32::MAX { width } else { prefix };
        if prefix > width {
            return Err(anyhow!("prefix /{prefix} is too long for {addr}"));
        }
        Ok(Self { addr, prefix })
    }

    pub fn contains(&self, other: IpAddr) -> bool {
        fn masked(octets: &[u8], prefix: u32) -> Vec<u8> {
            octets
                .iter()
                .enumerate()
                .map(|(i, byte)| {
                    let bit = i as u32 * 8;
                    match prefix.saturating_sub(bit).min(8) {
                        0 => 0,
                        8 => *byte,
                        keep => *byte & (0xffu8 << (8 - keep)),
                    }
                })
                .collect()
        }
        match (self.addr, other) {
            (IpAddr::V4(net), IpAddr::V4(peer)) => {
                masked(&net.octets(), self.prefix) == masked(&peer.octets(), self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(peer)) => {
                masked(&net.octets(), self.prefix) == masked(&peer.octets(), self.prefix)
            }
            _ => false,
        }
    }
}

/// How long a stream that arrived before its request may wait to be claimed.
/// The controller opens the connection and announces the request at the same
/// moment, so either can land first, and this host may be a command behind on
/// its control link when the connection lands. Long enough to cover that,
/// short enough that a stream nothing claims does not sit here.
const EARLY_STREAM_GRACE: Duration = Duration::from_secs(10);
/// Streams held unclaimed at once. Each one is an authenticated controller's
/// connection, so the cap only keeps a controller that announces nothing from
/// parking sockets here without limit.
const MAX_EARLY_STREAMS: usize = 32;

/// One side of a stream rendezvous, whichever side got here first.
enum Pending<T> {
    /// A request this host announced, waiting for the controller to dial.
    Expected(oneshot::Sender<T>),
    /// A stream the controller dialed before this host reached the request
    /// it belongs to.
    Arrived { link: T, at: Instant },
}

/// Streams the controller has announced but not yet dialed, and streams it
/// dialed before this host got to the announcement. An inbound stream is
/// matched against a request rather than trusted on its own, but the two can
/// arrive in either order, so each waits for the other here.
/// The payload is a parameter so the rendezvous can be exercised without a
/// live socket; every caller outside the tests carries a `Link`.
pub struct PendingStreams<T = Link>(Arc<Mutex<HashMap<String, Pending<T>>>>);

impl<T> Clone for PendingStreams<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Default for PendingStreams<T> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }
}

impl<T> PendingStreams<T> {
    pub fn expect(&self, token: &str) -> oneshot::Receiver<T> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.0.lock().unwrap();
        prune(&mut pending);
        match pending.remove(token) {
            Some(Pending::Arrived { link, .. }) => {
                let _ = tx.send(link);
            }
            _ => {
                pending.insert(token.to_string(), Pending::Expected(tx));
            }
        }
        rx
    }

    pub fn forget(&self, token: &str) {
        self.0.lock().unwrap().remove(token);
    }

    /// Whether this host is holding anything under that token.
    #[cfg(test)]
    pub fn holds(&self, token: &str) -> bool {
        self.0.lock().unwrap().contains_key(token)
    }

    /// Hands an accepted stream to whoever is waiting for it, or holds it
    /// briefly for a request this host has not reached yet. A second stream
    /// on a live token is refused: a token names one request and is spent by
    /// the stream that arrives on it.
    fn deliver(&self, token: &str, link: T) -> bool {
        let mut pending = self.0.lock().unwrap();
        prune(&mut pending);
        match pending.remove(token) {
            Some(Pending::Expected(waiter)) => waiter.send(link).is_ok(),
            Some(held @ Pending::Arrived { .. }) => {
                pending.insert(token.to_string(), held);
                false
            }
            None => {
                if pending
                    .values()
                    .filter(|entry| matches!(entry, Pending::Arrived { .. }))
                    .count()
                    >= MAX_EARLY_STREAMS
                {
                    return false;
                }
                pending.insert(
                    token.to_string(),
                    Pending::Arrived {
                        link,
                        at: Instant::now(),
                    },
                );
                true
            }
        }
    }

    /// Drops every expectation. A control link that ends takes its pending
    /// stream requests with it.
    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

fn prune<T>(pending: &mut HashMap<String, Pending<T>>) {
    let now = Instant::now();
    pending.retain(|_, entry| match entry {
        Pending::Arrived { at, .. } => now.duration_since(*at) < EARLY_STREAM_GRACE,
        Pending::Expected(_) => true,
    });
}

/// What this host will accept, and from whom.
pub struct ListenConfig {
    pub addr: SocketAddr,
    /// Binding every interface exposes process spawning to whatever can reach
    /// the machine, so it is opt-in rather than a default.
    pub allow_any: bool,
    pub allow_from: Vec<Cidr>,
    /// Controllers already enrolled, by pinned key.
    pub pinned: Vec<KeyHash>,
    /// Opens the enrollment window. Without one, an unpinned caller is
    /// refused at the handshake.
    pub token: Option<String>,
}

/// Who this host currently trusts. It changes exactly once per enrollment,
/// and the window closes with it: a token that has been used is no longer a
/// way in.
#[derive(Debug)]
struct Trusted {
    pinned: Vec<KeyHash>,
    token: Option<String>,
    opened_at: Instant,
}

impl Trusted {
    fn enrolled(&mut self, key: KeyHash) {
        if !self.pinned.contains(&key) {
            self.pinned.push(key);
        }
        self.token = None;
    }

    /// The token, while the window is still open. Expiry is checked on use
    /// rather than on a timer, so a host that nothing connects to still
    /// stops accepting unknown keys once the window has passed.
    fn open_enrollment(&mut self) -> Option<&str> {
        if self.token.is_some() && self.opened_at.elapsed() >= ENROLLMENT_WINDOW {
            self.token = None;
        }
        self.token.as_deref()
    }
}

/// One control link, after the caller proved it may open one.
pub struct ControlLink {
    pub link: Link,
    pub controller_key: KeyHash,
    /// True when this link enrolled rather than presenting a known key.
    pub enrolled: bool,
}

/// A control link and the epoch that names it. Epochs only ever increase, so
/// a link is superseded exactly when a later one has arrived.
pub struct Accepted {
    pub link: ControlLink,
    pub epoch: u64,
}

/// The control link a controller most recently proved. A host serves one
/// controller at a time, so a link that arrives while another is being
/// served replaces it rather than queueing behind it: a link that just
/// completed its proof is live, while the one being served may be a socket
/// whose far end died without closing it.
struct ControlSlot {
    pending: Mutex<Option<Accepted>>,
    epoch: tokio::sync::watch::Sender<u64>,
}

impl ControlSlot {
    fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            epoch: tokio::sync::watch::Sender::new(0),
        }
    }

    fn offer(&self, link: ControlLink) {
        let mut pending = self.pending.lock().unwrap();
        let mut epoch = 0;
        self.epoch.send_modify(|latest| {
            *latest += 1;
            epoch = *latest;
        });
        if pending.replace(Accepted { link, epoch }).is_some() {
            debug!("a control link was replaced before the host served it");
        }
    }
}

pub struct HostListener {
    slot: Arc<ControlSlot>,
    local_addr: SocketAddr,
}

impl std::fmt::Debug for HostListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostListener")
            .field("local_addr", &self.local_addr)
            .finish()
    }
}

impl HostListener {
    /// The next controller to open a control link. Auxiliary streams are
    /// matched against `streams` without surfacing here.
    pub async fn accept_control(&mut self) -> Option<Accepted> {
        let mut epoch = self.slot.epoch.subscribe();
        loop {
            if let Some(accepted) = self.slot.pending.lock().unwrap().take() {
                return Some(accepted);
            }
            epoch.changed().await.ok()?;
        }
    }

    pub fn is_superseded(&self, epoch: u64) -> bool {
        *self.slot.epoch.borrow() > epoch
    }

    /// Resolves once a controller has opened a control link later than this
    /// one, which is the host's cue to stop serving it.
    pub fn superseded(&self, epoch: u64) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut latest = self.slot.epoch.subscribe();
        async move {
            while *latest.borrow_and_update() <= epoch {
                if latest.changed().await.is_err() {
                    return;
                }
            }
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

pub async fn bind(
    config: ListenConfig,
    identity: Identity,
    streams: PendingStreams,
) -> anyhow::Result<HostListener> {
    if config.addr.ip().is_unspecified() && !config.allow_any {
        return Err(anyhow!(
            "refusing to bind {} because it accepts process spawns on every interface: \
             pass --listen-any to mean it, or bind the interface the controller reaches",
            config.addr
        ));
    }
    if config.pinned.is_empty() && config.token.is_none() {
        return Err(anyhow!(
            "this host has no enrolled controller: pass --token to enroll one"
        ));
    }
    let tcp = TcpListener::bind(config.addr)
        .await
        .with_context(|| format!("binding {}", config.addr))?;
    let local_addr = tcp.local_addr()?;
    let slot = Arc::new(ControlSlot::new());
    let trusted = Arc::new(Mutex::new(Trusted {
        pinned: config.pinned.clone(),
        token: config.token.clone(),
        opened_at: Instant::now(),
    }));
    tokio::spawn(accept_loop(
        tcp,
        Arc::new(config),
        trusted,
        identity,
        streams,
        slot.clone(),
    ));
    Ok(HostListener { slot, local_addr })
}

/// Attempts per source address, so a caller that cannot get in cannot keep
/// trying at full speed either.
#[derive(Default)]
struct RateLimiter(Mutex<HashMap<IpAddr, (Instant, usize)>>);

impl RateLimiter {
    fn allow(&self, peer: IpAddr) -> bool {
        let mut seen = self.0.lock().unwrap();
        let now = Instant::now();
        seen.retain(|_, (started, _)| now.duration_since(*started) < RATE_WINDOW);
        let entry = seen.entry(peer).or_insert((now, 0));
        entry.1 += 1;
        entry.1 <= RATE_LIMIT
    }
}

async fn accept_loop(
    tcp: TcpListener,
    config: Arc<ListenConfig>,
    trusted: Arc<Mutex<Trusted>>,
    identity: Identity,
    streams: PendingStreams,
    control: Arc<ControlSlot>,
) {
    let limiter = Arc::new(RateLimiter::default());
    let unproven = Arc::new(tokio::sync::Semaphore::new(MAX_UNPROVEN));
    loop {
        let Ok((stream, peer)) = tcp.accept().await else {
            continue;
        };
        trace!(%peer, "accepted a connection on the worker plane");
        if !config.allow_from.is_empty()
            && !config.allow_from.iter().any(|net| net.contains(peer.ip()))
        {
            debug!(%peer, "refused a connection from outside the allowed range");
            continue;
        }
        if !limiter.allow(peer.ip()) {
            debug!(%peer, "refused a connection over the rate limit");
            continue;
        }
        let Ok(slot) = unproven.clone().try_acquire_owned() else {
            debug!(%peer, "refused a connection while too many are unproven");
            continue;
        };
        let _ = stream.set_nodelay(true);
        let (identity, streams, control, trusted) = (
            identity.clone(),
            streams.clone(),
            control.clone(),
            trusted.clone(),
        );
        tokio::spawn(async move {
            let served = tokio::time::timeout(
                HANDSHAKE_DEADLINE,
                serve_one(stream, peer, &trusted, &identity, &streams, &control),
            )
            .await;
            drop(slot);
            match served {
                Ok(Err(e)) => debug!(%peer, error = %e, "inbound controller connection refused"),
                Err(_) => debug!(%peer, "inbound controller connection timed out handshaking"),
                Ok(Ok(())) => {}
            }
        });
    }
}

#[allow(clippy::result_large_err)]
fn requested_route(
    request: &tokio_tungstenite::tungstenite::handshake::server::Request,
) -> (String, String) {
    (
        request.uri().path().to_string(),
        request
            .headers()
            .get(tokio_tungstenite::tungstenite::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_string(),
    )
}

#[allow(clippy::result_large_err)]
async fn serve_one(
    stream: TcpStream,
    peer: SocketAddr,
    trusted: &Arc<Mutex<Trusted>>,
    identity: &Identity,
    streams: &PendingStreams,
    control: &ControlSlot,
) -> anyhow::Result<()> {
    // Outside an enrollment window only a pinned key completes the
    // handshake, so an unknown caller never reaches the protocol at all.
    let policy = {
        let mut trusted = trusted.lock().unwrap();
        match trusted.open_enrollment() {
            Some(_) => PeerPolicy::Pairing,
            None => PeerPolicy::PinnedAny(trusted.pinned.clone()),
        }
    };
    let tls = pm_tls::server_config(identity, &policy)?;
    let stream = tokio_rustls::TlsAcceptor::from(tls).accept(stream).await?;
    let (controller_key, exporter) = {
        let (_, connection) = stream.get_ref();
        (
            pm_tls::peer_key_hash(connection.peer_certificates())
                .ok_or_else(|| anyhow!("caller presented no key"))?,
            pm_tls::pairing::exporter(connection)?,
        )
    };

    let mut requested = (String::new(), String::new());
    let link =
        tokio_tungstenite::accept_hdr_async(TlsStream::from(stream), |request: &_, response| {
            requested = requested_route(request);
            Ok(response)
        })
        .await?;
    let (path, bearer) = requested;

    match path.as_str() {
        "/worker" => {
            let (link, enrolled) =
                open_control(link, trusted, identity, controller_key, &exporter).await?;
            if enrolled {
                trusted.lock().unwrap().enrolled(controller_key);
                info!(
                    %peer,
                    controller_key = %controller_key,
                    "enrolled a controller and pinned its key"
                );
            }
            info!(
                %peer,
                controller_key = %controller_key,
                enrolled,
                "controller opened a control link"
            );
            control.offer(ControlLink {
                link,
                controller_key,
                enrolled,
            });
            Ok(())
        }
        "/worker/terminal" | "/worker/transcript" | "/worker/stream" => {
            // The handshake already proved the controller. The token only
            // says which announced request this stream belongs to.
            if !trusted.lock().unwrap().pinned.contains(&controller_key) {
                return Err(anyhow!("stream from a controller that has not enrolled"));
            }
            if !streams.deliver(&bearer, link) {
                return Err(anyhow!(
                    "stream for a request this host is not expecting on {path}"
                ));
            }
            Ok(())
        }
        other => Err(anyhow!("unknown worker-plane path {other}")),
    }
}

/// Settles trust on an inbound control link. This host is the listener here,
/// so the controller proves the enrollment token first — it is the one asking
/// for access to a machine that runs what it is told.
async fn open_control(
    link: Link,
    trusted: &Arc<Mutex<Trusted>>,
    identity: &Identity,
    controller_key: KeyHash,
    exporter: &[u8; 32],
) -> anyhow::Result<(Link, bool)> {
    let mut link = link;
    let token = {
        let mut trusted = trusted.lock().unwrap();
        if trusted.pinned.contains(&controller_key) {
            None
        } else {
            Some(
                trusted
                    .open_enrollment()
                    .ok_or_else(|| anyhow!("caller is not enrolled and no enrollment is open"))?
                    .to_string(),
            )
        }
    };
    let Some(token) = token else {
        link.send(Message::Binary(worker_frame::encode_ready().into()))
            .await?;
        return Ok((link, false));
    };
    let token = token.as_str();

    let listener_nonce = pm_tls::pairing::nonce();
    link.send(Message::Binary(
        worker_frame::encode_pair_hello(&listener_nonce).into(),
    ))
    .await?;
    let Some(Ok(Message::Binary(buf))) = link.next().await else {
        return Err(anyhow!("caller did not present an enrollment proof"));
    };
    let Some(WorkerFrame::PairProof {
        nonce: dialer_nonce,
        mac,
    }) = worker_frame::decode(&buf)
    else {
        return Err(anyhow!("caller did not present an enrollment proof"));
    };
    let transcript = Transcript {
        exporter: *exporter,
        dialer_key: controller_key,
        listener_key: identity.key_hash(),
        dialer_nonce,
        listener_nonce,
    };
    if !transcript.verify(token, Side::Dialer, &mac) {
        warn!("an enrollment proof did not match this host's token");
        return Err(anyhow!("enrollment proof did not match"));
    }
    link.send(Message::Binary(
        worker_frame::encode_pair_accept(&transcript.mac(token, Side::Listener)).into(),
    ))
    .await?;
    Ok((link, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_contains_only_its_own_addresses() {
        let net = Cidr::parse("10.1.0.0/16").unwrap();
        assert!(net.contains("10.1.0.1".parse().unwrap()));
        assert!(net.contains("10.1.255.254".parse().unwrap()));
        assert!(!net.contains("10.2.0.1".parse().unwrap()));
        assert!(!net.contains("::1".parse().unwrap()));
    }

    #[test]
    fn a_bare_address_is_a_range_of_one() {
        let net = Cidr::parse("192.168.4.7").unwrap();
        assert!(net.contains("192.168.4.7".parse().unwrap()));
        assert!(!net.contains("192.168.4.8".parse().unwrap()));
    }

    #[test]
    fn ranges_that_do_not_describe_addresses_are_refused() {
        assert!(Cidr::parse("10.0.0.0/33").is_err());
        assert!(Cidr::parse("not-an-address").is_err());
        assert!(Cidr::parse("10.0.0.0/x").is_err());
    }

    #[test]
    fn a_v6_range_masks_on_the_right_boundary() {
        let net = Cidr::parse("fd00::/8").unwrap();
        assert!(net.contains("fd00::1".parse().unwrap()));
        assert!(net.contains("fdff::9".parse().unwrap()));
        assert!(!net.contains("fe00::1".parse().unwrap()));
    }

    /// A stream is matched to the request its token names, so another
    /// token's stream never satisfies a waiting request.
    #[test]
    fn a_stream_reaches_only_the_request_that_expected_it() {
        let streams = PendingStreams::<u8>::default();
        let mut waiting = streams.expect("token-a");
        assert!(streams.deliver("token-b", 2));
        assert_eq!(waiting.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        assert!(streams.deliver("token-a", 1));
        assert_eq!(waiting.try_recv(), Ok(1));
        streams.forget("token-b");
        assert!(streams.0.lock().unwrap().is_empty());
    }

    /// The controller dials the stream and announces the request at the same
    /// moment, so the stream can land first. Refusing it there drops a
    /// request both ends believe is under way, so it waits instead.
    #[test]
    fn a_stream_that_arrives_first_waits_for_its_request() {
        let streams = PendingStreams::<u8>::default();
        assert!(streams.deliver("token-a", 1));
        let mut waiting = streams.expect("token-a");
        assert_eq!(waiting.try_recv(), Ok(1));
        assert!(streams.0.lock().unwrap().is_empty());
    }

    /// A token names one request, so the stream that arrives on it spends it.
    #[test]
    fn a_second_stream_on_a_held_token_is_refused() {
        let streams = PendingStreams::<u8>::default();
        assert!(streams.deliver("token-a", 1));
        assert!(!streams.deliver("token-a", 2));
        let mut waiting = streams.expect("token-a");
        assert_eq!(waiting.try_recv(), Ok(1));
    }

    /// Holding a stream costs a socket, so a controller that announces
    /// nothing cannot park them without limit.
    #[test]
    fn held_streams_are_capped() {
        let streams = PendingStreams::<u8>::default();
        for i in 0..MAX_EARLY_STREAMS {
            assert!(streams.deliver(&format!("token-{i}"), 1), "held {i}");
        }
        assert!(!streams.deliver("one-too-many", 1));
    }

    /// A request that was announced is not a held stream, so it does not
    /// consume the cap.
    #[test]
    fn expected_requests_do_not_fill_the_hold() {
        let streams = PendingStreams::<u8>::default();
        let waiting: Vec<_> = (0..MAX_EARLY_STREAMS)
            .map(|i| streams.expect(&format!("token-{i}")))
            .collect();
        assert!(streams.deliver("held", 1));
        drop(waiting);
    }

    /// A stream nothing ever claims is dropped rather than held for the life
    /// of the link.
    #[test]
    fn a_held_stream_expires() {
        let streams = PendingStreams::<u8>::default();
        assert!(streams.deliver("token-a", 1));
        streams.0.lock().unwrap().insert(
            "token-a".into(),
            Pending::Arrived {
                link: 1,
                at: Instant::now() - EARLY_STREAM_GRACE,
            },
        );
        let mut waiting = streams.expect("token-a");
        assert_eq!(waiting.try_recv(), Err(oneshot::error::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn binding_every_interface_needs_saying_so() {
        let identity = Identity::generate().unwrap();
        let config = ListenConfig {
            addr: "0.0.0.0:0".parse().unwrap(),
            allow_any: false,
            allow_from: Vec::new(),
            pinned: vec![identity.key_hash()],
            token: None,
        };
        let error = bind(config, identity, PendingStreams::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("--listen-any"), "{error}");
    }

    #[tokio::test]
    async fn a_host_with_no_controller_and_no_token_does_not_listen() {
        let identity = Identity::generate().unwrap();
        let config = ListenConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            allow_any: false,
            allow_from: Vec::new(),
            pinned: Vec::new(),
            token: None,
        };
        let error = bind(config, identity, PendingStreams::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("--token"), "{error}");
    }

    /// Dials the host the way a controller does, and completes enrollment.
    /// Proves the inverted direction end to end: the host is the listener,
    /// so the controller is the one that proves the token first.
    async fn dial_as_controller(
        addr: SocketAddr,
        controller: &Identity,
        token: Option<&str>,
    ) -> anyhow::Result<Link> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let tls = pm_tls::client_config(controller, &PeerPolicy::Pairing)?;
        let tcp = TcpStream::connect(addr).await?;
        let stream = tokio_rustls::TlsConnector::from(tls)
            .connect(pm_tls::peer_server_name(), tcp)
            .await?;
        let (host_key, exporter) = {
            let (_, connection) = stream.get_ref();
            (
                pm_tls::peer_key_hash(connection.peer_certificates()).expect("host key"),
                pm_tls::pairing::exporter(connection)?,
            )
        };
        let request = format!("wss://{addr}/worker")
            .as_str()
            .into_client_request()?;
        let (mut link, _) =
            tokio_tungstenite::client_async(request, TlsStream::from(stream)).await?;

        let Some(Ok(Message::Binary(opening))) = link.next().await else {
            return Err(anyhow!("host did not open the link"));
        };
        let listener_nonce = match worker_frame::decode(&opening) {
            Some(WorkerFrame::Ready) => return Ok(link),
            Some(WorkerFrame::PairHello { nonce }) => nonce,
            other => return Err(anyhow!("unexpected opening frame {other:?}")),
        };
        let token = token.ok_or_else(|| anyhow!("host opened enrollment but no token"))?;
        let dialer_nonce = pm_tls::pairing::nonce();
        let transcript = Transcript {
            exporter,
            dialer_key: controller.key_hash(),
            listener_key: host_key,
            dialer_nonce,
            listener_nonce,
        };
        link.send(Message::Binary(
            worker_frame::encode_pair_proof(&dialer_nonce, &transcript.mac(token, Side::Dialer))
                .into(),
        ))
        .await?;
        let Some(Ok(Message::Binary(accept))) = link.next().await else {
            return Err(anyhow!("host did not prove the token back"));
        };
        match worker_frame::decode(&accept) {
            Some(WorkerFrame::PairAccept { mac })
                if transcript.verify(token, Side::Listener, &mac) =>
            {
                Ok(link)
            }
            _ => Err(anyhow!("host failed its half of the proof")),
        }
    }

    async fn listening_host(token: Option<&str>, pinned: Vec<KeyHash>) -> (HostListener, Identity) {
        listening_host_with_streams(token, pinned, PendingStreams::default()).await
    }

    async fn listening_host_with_streams(
        token: Option<&str>,
        pinned: Vec<KeyHash>,
        streams: PendingStreams,
    ) -> (HostListener, Identity) {
        let identity = Identity::generate().unwrap();
        let listener = bind(
            ListenConfig {
                addr: "127.0.0.1:0".parse().unwrap(),
                allow_any: false,
                allow_from: Vec::new(),
                pinned,
                token: token.map(str::to_string),
            },
            identity.clone(),
            streams,
        )
        .await
        .unwrap();
        (listener, identity)
    }

    /// Dials one of the stream paths the way a controller does once it has
    /// enrolled. The handshake proves the controller; the bearer token only
    /// says which request the stream belongs to.
    async fn dial_stream(
        addr: SocketAddr,
        controller: &Identity,
        path: &str,
        token: &str,
    ) -> anyhow::Result<Link> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let tls = pm_tls::client_config(controller, &PeerPolicy::Pairing)?;
        let tcp = TcpStream::connect(addr).await?;
        let stream = tokio_rustls::TlsConnector::from(tls)
            .connect(pm_tls::peer_server_name(), tcp)
            .await?;
        let mut request = format!("wss://{addr}{path}")
            .as_str()
            .into_client_request()?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse()?,
        );
        let (link, _) = tokio_tungstenite::client_async(request, TlsStream::from(stream)).await?;
        Ok(link)
    }

    async fn held_stream(streams: &PendingStreams, token: &str) {
        for _ in 0..2000 {
            if streams.0.lock().unwrap().contains_key(token) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("the host never took the stream");
    }

    /// The controller dials a stream at the same moment it announces the
    /// request, and this host may still be a command behind on its control
    /// link when the connection lands. The stream is held until the request
    /// reaches the host, and the same live socket is handed over.
    #[tokio::test]
    async fn a_stream_dialed_before_its_request_is_served_when_it_arrives() {
        let controller = Identity::generate().unwrap();
        let streams = PendingStreams::default();
        let (host, _identity) =
            listening_host_with_streams(None, vec![controller.key_hash()], streams.clone()).await;
        let addr = host.local_addr();

        let mut dialed = dial_stream(addr, &controller, "/worker/terminal", "token-a")
            .await
            .expect("the controller's half");
        held_stream(&streams, "token-a").await;

        let link = streams.expect("token-a").await.expect("the held stream");
        dialed
            .send(Message::Binary(vec![7u8].into()))
            .await
            .unwrap();
        let (_, mut read) = link.split();
        assert_eq!(
            read.next().await.unwrap().unwrap(),
            Message::Binary(vec![7u8].into())
        );
    }

    #[tokio::test]
    async fn a_controller_holding_the_token_enrolls_and_gets_a_control_link() {
        let (mut host, _identity) = listening_host(Some("shared-token"), Vec::new()).await;
        let controller = Identity::generate().unwrap();
        let addr = host.local_addr();
        let dialing = tokio::spawn(async move {
            dial_as_controller(addr, &controller, Some("shared-token")).await
        });
        let accepted = host.accept_control().await.expect("a control link");
        assert!(accepted.link.enrolled, "first contact enrolls");
        dialing.await.unwrap().expect("the controller's half");
    }

    /// A controller whose link died without closing the socket dials again,
    /// and the host has no way to tell that the link it is still serving is
    /// gone. The newer link is that proof, so it supersedes the older one
    /// rather than waiting behind it.
    #[tokio::test]
    async fn a_newer_control_link_supersedes_the_one_being_served() {
        let (mut host, _identity) = listening_host(Some("shared-token"), Vec::new()).await;
        let controller = Identity::generate().unwrap();
        let addr = host.local_addr();

        let dialing = {
            let controller = controller.clone();
            tokio::spawn(async move {
                dial_as_controller(addr, &controller, Some("shared-token")).await
            })
        };
        let serving = host.accept_control().await.expect("a control link");
        let _first = dialing.await.unwrap().expect("the controller's half");

        let superseded = host.superseded(serving.epoch);
        let dialing =
            tokio::spawn(async move { dial_as_controller(addr, &controller, None).await });
        tokio::time::timeout(Duration::from_secs(5), superseded)
            .await
            .expect("the host must stop serving a link a newer one replaced");
        let _second = dialing.await.unwrap().expect("the controller's half");
        let newer = host.accept_control().await.expect("the newer control link");
        assert!(newer.epoch > serving.epoch);
    }

    /// Links do not queue: one that arrives while another is unserved takes
    /// its place. A queued link is one the controller has already given up
    /// on by the time the host reaches it.
    #[tokio::test]
    async fn a_control_link_the_host_has_not_served_is_replaced_not_queued() {
        let (mut host, _identity) = listening_host(Some("shared-token"), Vec::new()).await;
        let controller = Identity::generate().unwrap();
        let addr = host.local_addr();

        for token in [Some("shared-token"), None, None] {
            let controller = controller.clone();
            tokio::spawn(async move { dial_as_controller(addr, &controller, token).await })
                .await
                .unwrap()
                .expect("the controller's half");
        }
        // Nothing has been accepted yet, so all three landed in the slot.
        tokio::time::timeout(Duration::from_secs(5), host.superseded(2))
            .await
            .expect("every proven link reaches the host");

        let accepted = host.accept_control().await.expect("a control link");
        assert_eq!(accepted.epoch, 3, "the host serves the newest link");
        let queued = tokio::time::timeout(Duration::from_millis(200), host.accept_control()).await;
        assert!(queued.is_err(), "the links it replaced are not waiting");
    }

    /// The whole point of proving in both directions: something that reaches
    /// the port without the token gets a control link out of it either way.
    #[tokio::test]
    async fn a_controller_without_the_token_gets_nothing() {
        let (mut host, _identity) = listening_host(Some("shared-token"), Vec::new()).await;
        let controller = Identity::generate().unwrap();
        let addr = host.local_addr();
        tokio::spawn(async move { dial_as_controller(addr, &controller, Some("wrong")).await });
        let accepted =
            tokio::time::timeout(Duration::from_millis(500), host.accept_control()).await;
        assert!(
            accepted.is_err(),
            "a caller that cannot prove the token must not reach the control plane"
        );
    }

    /// With no enrollment open, an unpinned key does not even complete the
    /// handshake, so the protocol is never reached.
    #[tokio::test]
    async fn an_unpinned_controller_is_refused_at_the_handshake() {
        let stranger = Identity::generate().unwrap();
        let enrolled = Identity::generate().unwrap();
        let (mut host, _identity) = listening_host(None, vec![enrolled.key_hash()]).await;
        let addr = host.local_addr();
        let refused = dial_as_controller(addr, &stranger, None).await;
        assert!(refused.is_err(), "an unpinned key must not get a link");
        let accepted =
            tokio::time::timeout(Duration::from_millis(500), host.accept_control()).await;
        assert!(accepted.is_err());
    }

    /// An enrollment nobody used must not stay open for the life of the
    /// process, or the host keeps accepting unknown keys indefinitely.
    #[test]
    fn an_enrollment_window_closes_on_its_own() {
        let mut trusted = Trusted {
            pinned: Vec::new(),
            token: Some("shared".into()),
            opened_at: Instant::now(),
        };
        assert_eq!(trusted.open_enrollment(), Some("shared"));
        trusted.opened_at = Instant::now() - ENROLLMENT_WINDOW;
        assert_eq!(trusted.open_enrollment(), None);
        assert!(
            trusted.token.is_none(),
            "an expired window stays closed once observed"
        );
    }

    #[test]
    fn enrolling_closes_the_window_and_pins_the_caller() {
        let key = Identity::generate().unwrap().key_hash();
        let mut trusted = Trusted {
            pinned: Vec::new(),
            token: Some("shared".into()),
            opened_at: Instant::now(),
        };
        trusted.enrolled(key);
        assert_eq!(trusted.pinned, vec![key]);
        assert_eq!(trusted.open_enrollment(), None);
    }

    #[test]
    fn the_rate_limiter_stops_a_source_that_keeps_trying() {
        let limiter = RateLimiter::default();
        let peer: IpAddr = "10.0.0.5".parse().unwrap();
        for _ in 0..RATE_LIMIT {
            assert!(limiter.allow(peer));
        }
        assert!(!limiter.allow(peer));
        assert!(
            limiter.allow("10.0.0.6".parse().unwrap()),
            "one noisy source must not lock out another"
        );
    }
}
