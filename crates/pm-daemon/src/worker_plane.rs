//! The controller's listener for the worker plane.
//!
//! Hosts run agent processes, so the link that carries `SpawnLocal` is the
//! most sensitive one the daemon has. It gets its own listener with its own
//! mutual TLS rather than sharing the browser plane's port, because that
//! port's confidentiality depends on a reverse proxy the daemon does not
//! control. Nothing on this plane is reachable without a key the controller
//! has pinned or an unexpired enrollment token.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use pm_tls::{Identity, KeyHash, PeerPolicy};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, trace, warn};

use crate::daemon::Daemon;

/// Settings key holding the controller's sealed worker-plane private key.
const WORKER_KEY_SETTING: &str = "worker.tls_key";

/// What one accepted connection proved about its peer. The key hash comes
/// from the completed handshake, never from anything the peer claims, and
/// the exported material binds an enrollment proof to this exact session.
#[derive(Clone, Debug)]
pub struct WorkerPeer {
    pub remote: SocketAddr,
    pub key_hash: KeyHash,
    /// This controller's own key, which an enrollment proof is bound to.
    pub local_key_hash: KeyHash,
    pub exporter: [u8; 32],
}

impl Connected<IncomingStream<'_, WorkerListener>> for WorkerPeer {
    fn connect_info(stream: IncomingStream<'_, WorkerListener>) -> Self {
        stream.remote_addr().clone()
    }
}

/// Accepts TCP, completes the mutual handshake, and hands axum a stream that
/// already knows who is on the other end. A peer that fails the handshake is
/// dropped here and never reaches a route.
pub struct WorkerListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
    local: SocketAddr,
    local_key_hash: KeyHash,
}

impl WorkerListener {
    pub async fn bind(addr: SocketAddr, identity: &Identity) -> std::io::Result<Self> {
        // Any client key completes the handshake: a host enrolling for the
        // first time has no pinned key yet. It proves the enrollment token
        // before the control plane tells it anything.
        let config = pm_tls::server_config(identity, &PeerPolicy::Pairing)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let tcp = TcpListener::bind(addr).await?;
        let local = tcp.local_addr()?;
        Ok(Self {
            tcp,
            acceptor: TlsAcceptor::from(config),
            local,
            local_key_hash: identity.key_hash(),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    async fn accept_one(&self) -> Option<(TlsStream<TcpStream>, WorkerPeer)> {
        let (tcp, remote) = self.tcp.accept().await.ok()?;
        let _ = tcp.set_nodelay(true);
        let _ = socket2::SockRef::from(&tcp).set_recv_buffer_size(WORKER_PLANE_SOCKET_BUFFER_BYTES);
        let stream = match self.acceptor.accept(tcp).await {
            Ok(stream) => stream,
            Err(e) => {
                debug!(%remote, error = %e, "worker plane handshake failed");
                return None;
            }
        };
        let (_, connection) = stream.get_ref();
        let key_hash = pm_tls::peer_key_hash(connection.peer_certificates())?;
        let exporter = pm_tls::pairing::exporter(connection).ok()?;
        Some((
            stream,
            WorkerPeer {
                remote,
                key_hash,
                local_key_hash: self.local_key_hash,
                exporter,
            },
        ))
    }
}

impl axum::serve::Listener for WorkerListener {
    type Io = TlsStream<TcpStream>;
    type Addr = WorkerPeer;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Some(accepted) = self.accept_one().await {
                return accepted;
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(WorkerPeer {
            remote: self.local,
            key_hash: self.local_key_hash,
            local_key_hash: self.local_key_hash,
            exporter: [0u8; 32],
        })
    }
}

/// A worker-plane connection reduced to binary frames. Past the handshake
/// the two directions are the same conversation, so nothing above this
/// learns which end opened the socket. Control links, terminal streams,
/// transcript uploads, and forwards all arrive here.
pub struct FrameLink {
    pub out: tokio::sync::mpsc::Sender<bytes::Bytes>,
    pub inbound: tokio::sync::mpsc::Receiver<bytes::Bytes>,
}

/// Frames are small and infrequent next to terminal traffic, so a short
/// queue absorbs a burst without hiding a stalled link.
const FRAME_QUEUE: usize = 1024;
/// SO_RCVBUF for worker-plane sockets, so a worker's flood stops at the
/// window rather than piling up megabytes the viewer must parse first.
pub const WORKER_PLANE_SOCKET_BUFFER_BYTES: usize = 32 * 1024;
/// A terminal stream's inbound queue stays short so a viewer that is
/// behind reaches the socket, and through it the worker, within a few
/// frames rather than a thousand.
pub const TERMINAL_STREAM_QUEUE: usize = 2;

/// How a link notices a peer that went away without closing the socket. A
/// half-open connection delivers nothing and fails nothing, so a reader
/// waiting on one waits forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keepalive {
    /// A stream that ends with the transfer that opened it: a transcript
    /// upload sends until the file is done and nothing idles on it.
    Off,
    On {
        ping: std::time::Duration,
        idle: std::time::Duration,
    },
}

impl Keepalive {
    /// A control link carries nothing for long stretches, so each end pings
    /// and holds the other to answering. The deadline spans several pings so
    /// a slow link is not mistaken for a gone one.
    pub const CONTROL: Self = Self::On {
        ping: std::time::Duration::from_secs(15),
        idle: std::time::Duration::from_secs(50),
    };

    /// A forward or terminal stream lives as long as the connection it
    /// carries, which for a directory share is until the share closes. An
    /// idle one is not a dead one, and pings are what tell the two apart:
    /// a ping or a pong keeps the deadline, so only a peer that has
    /// vanished is dropped.
    pub const STREAM: Self = Self::CONTROL;

    /// The keepalive for an auxiliary stream dialed to a host, by the
    /// endpoint it was dialed to.
    pub fn for_path(path: &str) -> Self {
        match path {
            "/worker/transcript" => Self::Off,
            _ => Self::STREAM,
        }
    }

    pub fn ping_interval(self) -> Option<std::time::Duration> {
        match self {
            Self::On { ping, .. } => Some(ping),
            Self::Off => None,
        }
    }

    pub fn idle_deadline(self) -> Option<std::time::Duration> {
        match self {
            Self::On { idle, .. } => Some(idle),
            Self::Off => None,
        }
    }
}

/// What an inbound message means to the pumps.
pub enum Payload {
    Frame(bytes::Bytes),
    /// A ping or a pong: proof the peer is alive and nothing else.
    Alive,
    /// The peer closed the link, which is an ordinary end.
    Closed,
    /// Anything this plane does not speak, which ends the link.
    Unspoken,
}

/// One message, as each socket flavour on this plane expresses it. The
/// controller accepts axum sockets and dials tungstenite ones; past the
/// handshake it treats both the same.
pub trait PlaneMessage: Send + 'static {
    fn frame(bytes: bytes::Bytes) -> Self;
    fn ping() -> Self;
    fn payload(self) -> Payload;
}

impl PlaneMessage for axum::extract::ws::Message {
    fn frame(bytes: bytes::Bytes) -> Self {
        Self::Binary(bytes)
    }

    fn ping() -> Self {
        Self::Ping(bytes::Bytes::new())
    }

    fn payload(self) -> Payload {
        match self {
            Self::Binary(buf) => Payload::Frame(buf),
            Self::Ping(_) | Self::Pong(_) => Payload::Alive,
            Self::Close(_) => Payload::Closed,
            _ => Payload::Unspoken,
        }
    }
}

impl PlaneMessage for tokio_tungstenite::tungstenite::Message {
    fn frame(bytes: bytes::Bytes) -> Self {
        Self::Binary(bytes)
    }

    fn ping() -> Self {
        Self::Ping(bytes::Bytes::new())
    }

    fn payload(self) -> Payload {
        match self {
            Self::Binary(buf) => Payload::Frame(buf),
            Self::Ping(_) | Self::Pong(_) => Payload::Alive,
            Self::Close(_) => Payload::Closed,
            _ => Payload::Unspoken,
        }
    }
}

/// Names one link in the log. A link that ends is diagnosed from what it
/// carried and which peer held the other end, so both travel with the
/// socket rather than being reconstructed from timestamps afterwards.
#[derive(Clone, Debug)]
pub struct LinkId {
    role: &'static str,
    peer: String,
}

impl LinkId {
    pub fn new(role: &'static str, peer: impl std::fmt::Display) -> Self {
        Self {
            role,
            peer: peer.to_string(),
        }
    }

    /// The label for an auxiliary stream, named by the route that opened it.
    pub fn for_path(path: &str, peer: impl std::fmt::Display) -> Self {
        let role = match path {
            "/worker/terminal" => "terminal",
            "/worker/transcript" => "transcript",
            "/worker/stream" => "forward",
            _ => "stream",
        };
        Self::new(role, peer)
    }
}

/// Why one pump stopped, which is the whole account either end gets of a
/// link that ended. A host whose link drops goes back to waiting, so
/// without this the difference between a closed socket, a rejected frame
/// and a peer that stopped answering is not recoverable from the log.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LinkExit {
    /// The peer closed the link, or its stream simply ended.
    PeerClosed,
    /// Reading the socket failed.
    ReadFailed(String),
    /// The peer sent something this plane does not speak.
    Unspoken,
    /// Nothing arrived within the keepalive deadline.
    Quiet {
        idle: std::time::Duration,
        since: std::time::Duration,
    },
    /// This end let the link go: the session that owned the frames returned.
    Released,
    /// Writing to the socket failed.
    SendFailed(String),
}

impl LinkExit {
    fn log(&self, link: &LinkId) {
        let role = link.role;
        let peer = link.peer.as_str();
        match self {
            Self::PeerClosed => debug!(role, peer, "the peer closed the worker link"),
            Self::ReadFailed(error) => {
                warn!(
                    role,
                    peer, error, "reading the worker link failed, dropping it"
                )
            }
            Self::Unspoken => warn!(
                role,
                peer, "the peer sent a frame this plane does not speak, dropping the link"
            ),
            Self::Quiet { idle, since } => warn!(
                role,
                peer,
                idle_ms = idle.as_millis(),
                quiet_ms = since.as_millis(),
                "the peer went quiet past the keepalive deadline, dropping the link"
            ),
            Self::Released => debug!(role, peer, "this end released the worker link"),
            Self::SendFailed(error) => warn!(
                role,
                peer, error, "writing to the worker link failed, dropping it"
            ),
        }
    }
}

/// The tasks moving bytes between a socket and its frames.
pub struct Pumps {
    out: tokio::task::JoinHandle<()>,
    inbound: tokio::task::JoinHandle<()>,
}

/// A refused registration is answered and then the link ends, so the reply
/// has to leave before the socket does.
const FLUSH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

impl Pumps {
    /// Lets whatever is already queued reach the peer, then stops reading.
    /// The session drops its sender when it returns, so the outbound pump
    /// ends on its own once the queue is empty.
    pub async fn finish(self) {
        let _ = tokio::time::timeout(FLUSH_DEADLINE, self.out).await;
        self.inbound.abort();
    }
}

impl FrameLink {
    /// Frames over a socket this controller accepted.
    pub fn accepted(
        socket: axum::extract::ws::WebSocket,
        keepalive: Keepalive,
        link: LinkId,
    ) -> (Self, Pumps) {
        pumps(socket, keepalive, link)
    }

    /// As `accepted`, with its own inbound queue depth.
    pub fn accepted_with_queue(
        socket: axum::extract::ws::WebSocket,
        keepalive: Keepalive,
        link: LinkId,
        queue: usize,
    ) -> (Self, Pumps) {
        pumps_with_queue(socket, keepalive, link, queue)
    }

    /// Frames over a socket this controller opened.
    pub fn dialed(
        socket: crate::worker_dialer::Link,
        keepalive: Keepalive,
        link: LinkId,
    ) -> (Self, Pumps) {
        pumps(socket, keepalive, link)
    }
}

fn pumps<S, M, E>(socket: S, keepalive: Keepalive, link: LinkId) -> (FrameLink, Pumps)
where
    S: futures::Sink<M> + futures::Stream<Item = Result<M, E>> + Send + Unpin + 'static,
    <S as futures::Sink<M>>::Error: Send + std::fmt::Display,
    M: PlaneMessage,
    E: Send + std::fmt::Display,
{
    pumps_with_queue(socket, keepalive, link, FRAME_QUEUE)
}

fn pumps_with_queue<S, M, E>(
    socket: S,
    keepalive: Keepalive,
    link: LinkId,
    queue: usize,
) -> (FrameLink, Pumps)
where
    S: futures::Sink<M> + futures::Stream<Item = Result<M, E>> + Send + Unpin + 'static,
    <S as futures::Sink<M>>::Error: Send + std::fmt::Display,
    M: PlaneMessage,
    E: Send + std::fmt::Display,
{
    use futures::StreamExt;
    let (sink, stream) = socket.split();
    let (out, outbound) = tokio::sync::mpsc::channel(FRAME_QUEUE);
    let (inbound_tx, inbound) = tokio::sync::mpsc::channel(queue);
    debug!(
        role = link.role,
        peer = %link.peer,
        idle_ms = keepalive.idle_deadline().map(|idle| idle.as_millis() as u64),
        "worker link is up"
    );
    let writing = link.clone();
    let out_pump = tokio::spawn(async move {
        let mut sink = sink;
        write_pump(&mut sink, outbound, keepalive, &writing)
            .await
            .log(&writing);
        let _ = futures::SinkExt::close(&mut sink).await;
    });
    let reading = link;
    let in_pump = tokio::spawn(async move {
        read_pump(stream, inbound_tx, keepalive, &reading)
            .await
            .log(&reading);
    });
    (
        FrameLink { out, inbound },
        Pumps {
            out: out_pump,
            inbound: in_pump,
        },
    )
}

/// Carries queued frames to the peer and pings it on the keepalive
/// interval, until one of those fails or the session lets the link go.
async fn write_pump<W, M>(
    sink: &mut W,
    mut outbound: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    keepalive: Keepalive,
    link: &LinkId,
) -> LinkExit
where
    W: futures::Sink<M> + Unpin,
    <W as futures::Sink<M>>::Error: std::fmt::Display,
    M: PlaneMessage,
{
    use futures::SinkExt;
    let mut pings = keepalive.ping_interval().map(|every| {
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        timer
    });
    loop {
        let due = async {
            match pings.as_mut() {
                Some(timer) => timer.tick().await,
                None => std::future::pending().await,
            }
        };
        let message = tokio::select! {
            frame = outbound.recv() => match frame {
                Some(frame) => M::frame(frame),
                None => return LinkExit::Released,
            },
            _ = due => {
                trace!(role = link.role, peer = %link.peer, "keepalive ping sent");
                M::ping()
            }
        };
        if let Err(error) = sink.send(message).await {
            return LinkExit::SendFailed(error.to_string());
        }
    }
}

/// Delivers inbound frames to the session and holds the peer to the
/// keepalive deadline. Returning is how the link ends, and the value
/// returned is the only reason anyone gets for it.
async fn read_pump<R, M, E>(
    mut stream: R,
    inbound_tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
    keepalive: Keepalive,
    link: &LinkId,
) -> LinkExit
where
    R: futures::Stream<Item = Result<M, E>> + Unpin,
    M: PlaneMessage,
    E: std::fmt::Display,
{
    use futures::StreamExt;
    let idle = keepalive.idle_deadline();
    let mut last = tokio::time::Instant::now();
    loop {
        let next = match idle {
            Some(deadline) => match tokio::time::timeout(deadline, stream.next()).await {
                Ok(next) => next,
                Err(_) => {
                    return LinkExit::Quiet {
                        idle: deadline,
                        since: last.elapsed(),
                    }
                }
            },
            None => stream.next().await,
        };
        let message = match next {
            Some(Ok(message)) => message,
            Some(Err(error)) => return LinkExit::ReadFailed(error.to_string()),
            None => return LinkExit::PeerClosed,
        };
        let since = last.elapsed();
        last = tokio::time::Instant::now();
        match message.payload() {
            Payload::Frame(buf) => {
                if inbound_tx.send(buf).await.is_err() {
                    return LinkExit::Released;
                }
            }
            Payload::Alive => {
                trace!(
                    role = link.role,
                    peer = %link.peer,
                    since_ms = since.as_millis(),
                    "keepalive answer received"
                );
                // Half the deadline is the point where a link is closer to
                // being dropped than to healthy, and the peer that later
                // drops has been late here for a while first.
                if let Some(deadline) = idle.filter(|deadline| since * 2 > *deadline) {
                    debug!(
                        role = link.role,
                        peer = %link.peer,
                        since_ms = since.as_millis(),
                        idle_ms = deadline.as_millis(),
                        "keepalive answer arrived past half the link's idle deadline"
                    );
                }
            }
            Payload::Closed => return LinkExit::PeerClosed,
            Payload::Unspoken => return LinkExit::Unspoken,
        }
    }
}

pub async fn run_session(daemon: &Arc<Daemon>, peer_key_hash: &str, frames: FrameLink) {
    crate::http::worker_session(daemon, peer_key_hash, frames).await
}

/// Serves one auxiliary stream, whichever end opened it. The token claims
/// the same pending request the accept path claims, so a dialed stream and
/// a dialed-back one are handled identically from here on.
pub async fn serve_stream(daemon: &Arc<Daemon>, path: &str, token: &str, frames: FrameLink) {
    let hashed = crate::auth::hash_token(token);
    match path {
        "/worker/terminal" => {
            if let Some(claim) = daemon.terminal_streams.claim(&hashed, &daemon.workers) {
                crate::http::worker_terminal_connection(claim, frames).await;
            }
        }
        "/worker/transcript" => {
            if let Some(claim) = daemon.transcript_transfers.claim(&hashed, &daemon.workers) {
                crate::http::worker_transcript_connection(daemon.clone(), claim, frames).await;
            }
        }
        "/worker/stream" => {
            if let Some(tcp) = daemon.forwards.claim_stream(&hashed) {
                crate::forward::splice_ws_tcp(frames, tcp).await;
            }
        }
        _ => {}
    }
}

impl Daemon {
    /// The controller's long-lived worker-plane keypair, generated once and
    /// sealed at rest like the other credentials the daemon holds.
    pub fn worker_identity(&self) -> Result<Identity, pm_tls::TlsError> {
        let secret = self.installation_secret();
        let fresh = Identity::generate()?;
        let sealed_fresh = crate::secrets::seal_secret(&secret, &fresh.key_pem()?);
        let stored = self
            .storage()
            .ensure_setting(WORKER_KEY_SETTING, &sealed_fresh)
            .unwrap_or_else(|_| sealed_fresh.clone());
        if let Some(pem) = crate::secrets::open_secret(&secret, &stored) {
            return Identity::from_key_pem(&pem);
        }
        // The stored key was sealed with a secret this installation no
        // longer holds — a database carried away from its secret file, or a
        // secret that was replaced. Keeping the unreadable row would mint a
        // new key on every call, so this controller would present a
        // different identity to every host on every connection and none of
        // them could ever pin it. Replacing it costs the enrolled hosts one
        // re-enrollment, which they need either way, and leaves an identity
        // that holds still.
        warn!("the stored worker-plane key cannot be opened, replacing it: enrolled hosts must re-enroll");
        if self
            .storage()
            .set_setting(WORKER_KEY_SETTING, Some(&sealed_fresh))
            .is_err()
        {
            return Identity::from_key_pem(&fresh.key_pem()?);
        }
        // Read back rather than trusting the write, so callers that raced
        // here settle on one key instead of each keeping its own.
        match self
            .storage()
            .get_setting(WORKER_KEY_SETTING)
            .ok()
            .flatten()
            .and_then(|stored| crate::secrets::open_secret(&secret, &stored))
        {
            Some(pem) => Identity::from_key_pem(&pem),
            None => Ok(fresh),
        }
    }
}

/// Binds the worker plane and serves it until the process ends.
pub async fn serve(daemon: Arc<Daemon>, addr: SocketAddr) -> std::io::Result<SocketAddr> {
    let identity = daemon
        .worker_identity()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    info!(
        %addr,
        key = %identity.key_hash(),
        "worker plane listening"
    );
    let listener = WorkerListener::bind(addr, &identity).await?;
    let local = listener.local_addr();
    let app =
        crate::http::worker_router(daemon).into_make_service_with_connect_info::<WorkerPeer>();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!(error = %e, "worker plane stopped");
        }
    });
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use futures::StreamExt;
    use tokio::io::DuplexStream;
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::WebSocketStream;

    const PING: Duration = Duration::from_millis(20);
    const IDLE: Duration = Duration::from_millis(150);

    fn labelled(peer: &str) -> LinkId {
        LinkId::new("control", peer)
    }

    async fn linked() -> (WebSocketStream<DuplexStream>, WebSocketStream<DuplexStream>) {
        let (host, peer) = tokio::io::duplex(4096);
        (
            WebSocketStream::from_raw_socket(host, Role::Server, None).await,
            WebSocketStream::from_raw_socket(peer, Role::Client, None).await,
        )
    }

    fn still_open(link: &mut FrameLink) -> bool {
        matches!(
            link.inbound.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        )
    }

    /// Drives the read pump over one scripted stream and returns why it
    /// stopped.
    async fn read_until_exit(
        messages: Vec<Result<Message, String>>,
        keepalive: Keepalive,
    ) -> LinkExit {
        let (inbound_tx, _inbound) = tokio::sync::mpsc::channel(FRAME_QUEUE);
        read_pump(
            futures::stream::iter(messages),
            inbound_tx,
            keepalive,
            &labelled("scripted"),
        )
        .await
    }

    /// The failure this exists for: a peer whose machine went away holds a
    /// socket that delivers nothing and fails nothing.
    #[tokio::test(start_paused = true)]
    async fn a_link_whose_peer_stops_answering_ends() {
        let (host, silent) = linked().await;
        let (mut frames, _pumps) = pumps(
            host,
            Keepalive::On {
                ping: PING,
                idle: IDLE,
            },
            labelled("silent"),
        );
        let ended = tokio::time::timeout(Duration::from_secs(5), frames.inbound.recv())
            .await
            .expect("the link must end rather than wait on a peer that never answers");
        assert!(ended.is_none());
        drop(silent);
    }

    #[tokio::test(start_paused = true)]
    async fn a_peer_that_answers_keeps_its_link() {
        let (host, mut peer) = linked().await;
        let (mut frames, _pumps) = pumps(
            host,
            Keepalive::On {
                ping: PING,
                idle: IDLE,
            },
            labelled("answering"),
        );
        // Reading is what sends the pong: the peer says nothing of its own.
        let answering = tokio::spawn(async move { while let Some(Ok(_)) = peer.next().await {} });
        tokio::time::sleep(IDLE * 3).await;
        assert!(still_open(&mut frames));
        answering.abort();
    }

    /// A stream without keepalive must outlive any deadline the control
    /// link would apply.
    #[tokio::test]
    async fn a_stream_without_keepalive_outlives_an_idle_stretch() {
        let (host, peer) = linked().await;
        let (mut frames, _pumps) = pumps(host, Keepalive::Off, labelled("idle-upload"));
        tokio::time::sleep(IDLE * 3).await;
        assert!(still_open(&mut frames));
        drop(peer);
    }

    /// A forward stream whose peer went away half-open delivers nothing and
    /// fails nothing, so only the keepalive deadline can end it. An idle
    /// one whose peer still answers pings is left alone.
    #[tokio::test(start_paused = true)]
    async fn a_forward_stream_whose_peer_stops_answering_ends_within_the_deadline() {
        let (host, silent) = linked().await;
        let keepalive = Keepalive::On {
            ping: PING,
            idle: IDLE,
        };
        let (mut frames, _pumps) =
            pumps(host, keepalive, LinkId::for_path("/worker/stream", "gone"));
        let started = tokio::time::Instant::now();
        let ended = tokio::time::timeout(IDLE * 4, frames.inbound.recv())
            .await
            .expect("a forward stream whose peer vanished must end within the deadline");
        assert!(ended.is_none());
        assert!(started.elapsed() >= IDLE);
        drop(silent);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_forward_stream_whose_peer_answers_pings_stays_open() {
        let (host, mut peer) = linked().await;
        let (mut frames, _pumps) = pumps(
            host,
            Keepalive::On {
                ping: PING,
                idle: IDLE,
            },
            LinkId::for_path("/worker/stream", "idle"),
        );
        let answering = tokio::spawn(async move { while let Some(Ok(_)) = peer.next().await {} });
        tokio::time::sleep(IDLE * 3).await;
        assert!(still_open(&mut frames));
        answering.abort();
    }

    /// Forward and terminal streams carry the stream keepalive, and the
    /// deadline spans several pings so one late answer does not drop a
    /// live stream. A transcript upload ends with its file and carries
    /// none.
    #[test]
    fn streams_dialed_to_a_host_carry_a_keepalive_except_uploads() {
        assert_eq!(Keepalive::for_path("/worker/stream"), Keepalive::STREAM);
        assert_eq!(Keepalive::for_path("/worker/terminal"), Keepalive::STREAM);
        assert_eq!(Keepalive::for_path("/worker/transcript"), Keepalive::Off);
        let Keepalive::On { ping, idle } = Keepalive::STREAM else {
            panic!("a forward stream must carry a keepalive");
        };
        assert!(idle >= ping * 3);
    }

    /// The four ways a link ends have to stay apart. A host whose link
    /// drops goes back to waiting, so this value is the only account
    /// anyone gets of what happened, and a peer that closed, one that
    /// spoke nonsense, and one that stopped answering are three
    /// different problems with three different answers.
    #[tokio::test]
    async fn every_way_a_link_ends_names_itself() {
        assert_eq!(
            read_until_exit(vec![Ok(Message::Close(None))], Keepalive::Off).await,
            LinkExit::PeerClosed
        );
        assert_eq!(
            read_until_exit(Vec::new(), Keepalive::Off).await,
            LinkExit::PeerClosed
        );
        assert_eq!(
            read_until_exit(vec![Ok(Message::Text("hello".into()))], Keepalive::Off).await,
            LinkExit::Unspoken
        );
        assert_eq!(
            read_until_exit(vec![Err("connection reset".to_string())], Keepalive::Off).await,
            LinkExit::ReadFailed("connection reset".to_string()),
            "a read failure has to carry its reason, not just end the link"
        );
    }

    /// The deadline that dropped a link is half the diagnosis: without it
    /// nobody can tell a peer that is slow from one that is gone.
    #[tokio::test]
    async fn a_quiet_peer_reports_the_deadline_it_missed() {
        let (inbound_tx, _inbound) = tokio::sync::mpsc::channel(FRAME_QUEUE);
        let exit = read_pump(
            futures::stream::pending::<Result<Message, String>>(),
            inbound_tx,
            Keepalive::On {
                ping: PING,
                idle: IDLE,
            },
            &labelled("quiet"),
        )
        .await;
        match exit {
            LinkExit::Quiet { idle, since } => {
                assert_eq!(idle, IDLE);
                assert!(since >= IDLE, "quiet for {since:?}, deadline was {IDLE:?}");
            }
            other => panic!("a peer that never answers must report the deadline: {other:?}"),
        }
    }

    /// An end that let the link go is not a failure, and reading it as
    /// one would put a warning in the log on every ordinary shutdown.
    #[tokio::test]
    async fn a_session_that_returns_releases_the_link_rather_than_failing_it() {
        let (inbound_tx, inbound) = tokio::sync::mpsc::channel(FRAME_QUEUE);
        drop(inbound);
        let exit = read_pump(
            futures::stream::iter(vec![Ok::<_, String>(Message::Binary(
                bytes::Bytes::from_static(b"frame"),
            ))]),
            inbound_tx,
            Keepalive::Off,
            &labelled("released"),
        )
        .await;
        assert_eq!(exit, LinkExit::Released);

        let (out, outbound) = tokio::sync::mpsc::channel(FRAME_QUEUE);
        drop(out);
        let mut sink = futures::sink::drain::<Message>();
        assert_eq!(
            write_pump(&mut sink, outbound, Keepalive::Off, &labelled("released")).await,
            LinkExit::Released
        );
    }

    /// The outbound half fails on its own, and a link that ends because
    /// this end could not write says something different from one whose
    /// peer went away.
    #[tokio::test]
    async fn a_write_failure_carries_its_reason() {
        let (out, outbound) = tokio::sync::mpsc::channel(FRAME_QUEUE);
        out.send(bytes::Bytes::from_static(b"frame")).await.unwrap();
        let mut sink = futures::sink::unfold((), |(), _: Message| {
            futures::future::ready(Err::<(), String>("broken pipe".to_string()))
        });
        assert_eq!(
            write_pump(&mut sink, outbound, Keepalive::Off, &labelled("broken")).await,
            LinkExit::SendFailed("broken pipe".to_string())
        );
    }

    /// The line the whole investigation turned on. A dropped link was a
    /// `debug!` with no peer and no numbers, so an operator reading the
    /// worker's log at default level saw nothing at all.
    #[tokio::test]
    async fn a_peer_that_goes_quiet_is_a_warning_naming_the_peer_and_the_deadline() {
        capture_logs();
        let peer = "keepalive-deadline-case";
        let (host, silent) = linked().await;
        let (_frames, pumps) = pumps(
            host,
            Keepalive::On {
                ping: PING,
                idle: IDLE,
            },
            labelled(peer),
        );
        pumps.inbound.await.unwrap();
        drop(silent);

        let line = logged_line(peer, "went quiet");
        assert!(
            line.contains("WARN"),
            "a dropped link is not a debug: {line}"
        );
        assert!(line.contains("role=\"control\""), "{line}");
        assert!(line.contains("idle_ms=150"), "{line}");
        assert!(line.contains("quiet_ms="), "{line}");
    }

    /// Keepalive traffic is what makes "is the link healthy" answerable
    /// at all, and it has to stay at trace so a default run is unchanged.
    #[tokio::test(start_paused = true)]
    async fn keepalive_traffic_is_visible_at_trace() {
        capture_logs();
        let peer = "keepalive-traffic-case";
        let (host, mut answering) = linked().await;
        let (_frames, _pumps) = pumps(
            host,
            Keepalive::On {
                ping: PING,
                idle: IDLE,
            },
            labelled(peer),
        );
        let pumping =
            tokio::spawn(async move { while let Some(Ok(_)) = answering.next().await {} });
        tokio::time::sleep(PING * 4).await;
        pumping.abort();

        assert!(logged_line(peer, "keepalive ping sent").contains("TRACE"));
        assert!(logged_line(peer, "keepalive answer received").contains("TRACE"));
    }

    /// Everything this test module logged, so a test can assert on the
    /// level and fields an operator actually reads.
    fn captured() -> &'static std::sync::Mutex<Vec<u8>> {
        static LOG: std::sync::OnceLock<std::sync::Mutex<Vec<u8>>> = std::sync::OnceLock::new();
        LOG.get_or_init(Default::default)
    }

    #[derive(Clone)]
    struct CaptureWriter;

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            captured().lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CaptureWriter {
        type Writer = Self;

        fn make_writer(&self) -> Self::Writer {
            Self
        }
    }

    /// The pumps run on spawned tasks, so the capture has to be the
    /// process-wide subscriber rather than a scoped one.
    fn capture_logs() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(CaptureWriter)
                .with_ansi(false)
                .with_env_filter(tracing_subscriber::EnvFilter::new(
                    "pm_daemon::worker_plane=trace",
                ))
                .finish();
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    /// One captured line, found by the peer the test used so parallel
    /// tests sharing the subscriber do not read each other's logs.
    fn logged_line(peer: &str, message: &str) -> String {
        let captured = captured().lock().unwrap();
        let text = String::from_utf8_lossy(&captured);
        text.lines()
            .find(|line| line.contains(peer) && line.contains(message))
            .unwrap_or_else(|| panic!("nothing logged for {peer} matching {message:?} in:\n{text}"))
            .to_string()
    }
}
