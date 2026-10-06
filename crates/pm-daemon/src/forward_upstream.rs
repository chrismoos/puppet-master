//! Upstream connection reuse for the HTTP forward proxy.
//!
//! A forwarded request reaches its target over a connection the
//! controller opens, and for a remote worker opening one costs a
//! control-link round trip, a mutual-TLS handshake, a WebSocket upgrade
//! and a loopback connect on the worker before the first byte of HTTP
//! crosses. The pool keeps those connections, one set per forward and
//! target port, so a page's worth of requests pays for them once.
//!
//! A connection goes back only when the response body it carried has
//! ended, because an HTTP/1 connection carries one exchange at a time.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use bytes::Bytes;
use hyper::body::{Body as HttpBody, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::SendRequest;
use hyper::client::conn::http2::SendRequest as SendRequestH2;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use tracing::debug;

/// Idle upstreams kept per forward and target port. A browser opens up
/// to six connections to one HTTP/1.1 origin, so this covers a page's
/// parallel fetches and a navigation on top without letting a single
/// forward pin an unbounded number of worker streams: each pooled
/// remote connection holds a worker-plane WebSocket and two descriptors
/// on the worker for as long as it sits here.
pub const MAX_IDLE_PER_TARGET: usize = 8;

/// How long an idle upstream may sit before it is dropped. Long enough
/// that the fetches behind one page load, and a user clicking through
/// it, reuse a connection, short enough to bound how long a target that
/// has gone away keeps a worker stream parked. Servers that close
/// sooner than this are covered by the retry in the proxy.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long acquiring waits for a pooled sender to report ready. An
/// idle connection's task reports it as soon as it re-enters its read
/// loop, so this covers a scheduling hop rather than a round trip.
const READY_TIMEOUT: Duration = Duration::from_millis(500);

/// A forward and the target port it currently dials. The port is part
/// of the key because a directory share's forward is re-pointed at a
/// new port when its server is rebound, and connections to the old one
/// lead nowhere.
type Target = (u64, u16);

struct Idle {
    sender: SendRequest<Body>,
    alive: Arc<AtomicBool>,
    since: Instant,
}

/// The single HTTP/2 connection a directory share's forward carries all
/// its requests on. Nothing idles here: the connection is held from the
/// first request until the forward goes away, and concurrent requests
/// share it by cloning the sender.
struct Multiplexed {
    sender: SendRequestH2<Body>,
    alive: Arc<AtomicBool>,
}

/// HTTP/2 pings on a directory share's connection. The tunnel under it
/// can die half-open, and a connection that is never written to notices
/// nothing, so the connection pings while idle and ends when a ping goes
/// unanswered for the timeout.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShareKeepalive {
    pub interval: Duration,
    pub timeout: Duration,
}

impl ShareKeepalive {
    /// In step with the worker-plane stream keepalive under the tunnel,
    /// so a dead path is noticed by both within the same minute.
    pub(crate) const DEFAULT: Self = Self {
        interval: Duration::from_secs(15),
        timeout: Duration::from_secs(20),
    };
}

/// Opens the HTTP/2 connection a share's requests multiplex on and runs
/// it on its own task. The flag goes false when the connection ends, and
/// a dead path ends it: with nothing answering the pings the connection
/// errors out rather than sitting installed and half-open.
pub(crate) async fn multiplexed_handshake<T>(
    io: T,
    keepalive: ShareKeepalive,
) -> hyper::Result<(SendRequestH2<Body>, Arc<AtomicBool>)>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (sender, connection) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .timer(TokioTimer::new())
        .keep_alive_interval(keepalive.interval)
        .keep_alive_timeout(keepalive.timeout)
        .keep_alive_while_idle(true)
        .handshake(io)
        .await?;
    let alive = Arc::new(AtomicBool::new(true));
    let ended = alive.clone();
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            debug!(error = %error, "a directory share's connection ended");
        }
        ended.store(false, Ordering::Relaxed);
    });
    Ok((sender, alive))
}

/// Upstream connections held per forward target: a pool of HTTP/1
/// connections, or the one HTTP/2 connection a directory share uses
/// instead.
#[derive(Default)]
pub struct UpstreamPool {
    idle: Mutex<HashMap<Target, Vec<Idle>>>,
    multiplexed: Mutex<HashMap<Target, Multiplexed>>,
    /// One gate per target, so concurrent first requests to a share open
    /// one connection between them rather than one each.
    opening: Mutex<HashMap<Target, Arc<tokio::sync::Mutex<()>>>>,
}

impl UpstreamPool {
    /// Takes a reusable upstream for a forward's target, if the pool
    /// holds one that is still good for a request.
    pub(crate) async fn acquire(self: &Arc<Self>, forward_id: u64, port: u16) -> Option<Lease> {
        let target = (forward_id, port);
        loop {
            let mut idle = self.take(target)?;
            if !idle.alive.load(Ordering::Relaxed) || idle.sender.is_closed() {
                continue;
            }
            if !idle.sender.is_ready()
                && !matches!(
                    tokio::time::timeout(READY_TIMEOUT, idle.sender.ready()).await,
                    Ok(Ok(()))
                )
            {
                continue;
            }
            return Some(Lease {
                pool: self.clone(),
                target,
                sender: idle.sender,
                alive: idle.alive,
                reused: true,
            });
        }
    }

    /// Leases a connection that has just been opened. It is not marked
    /// reused, because a failure on a connection that has never carried
    /// a request is the target's answer rather than a stale handle.
    pub(crate) fn lease_new(
        self: &Arc<Self>,
        forward_id: u64,
        port: u16,
        sender: SendRequest<Body>,
        alive: Arc<AtomicBool>,
    ) -> Lease {
        Lease {
            pool: self.clone(),
            target: (forward_id, port),
            sender,
            alive,
            reused: false,
        }
    }

    /// Drops every upstream held for a forward. A forward that has
    /// stopped must not leave a connection anyone can still send a
    /// request down: a closed directory share whose tunnel survived
    /// would stay readable through it.
    pub(crate) fn forget(&self, forward_id: u64) {
        self.idle
            .lock()
            .unwrap()
            .retain(|(id, _), _| *id != forward_id);
        self.multiplexed
            .lock()
            .unwrap()
            .retain(|(id, _), _| *id != forward_id);
        self.opening
            .lock()
            .unwrap()
            .retain(|(id, _), _| *id != forward_id);
    }

    pub(crate) fn clear(&self) {
        self.idle.lock().unwrap().clear();
        self.multiplexed.lock().unwrap().clear();
        self.opening.lock().unwrap().clear();
    }

    /// How many upstream connections are held for a forward and could
    /// carry another request: the idle HTTP/1 ones across every target
    /// port pooled for it, plus the HTTP/2 connection a directory share
    /// holds, which counts whether or not it is busy because it is
    /// always the one the next request uses.
    pub fn idle_count(&self, forward_id: u64) -> usize {
        let pooled: usize = self
            .idle
            .lock()
            .unwrap()
            .iter()
            .filter(|((id, _), _)| *id == forward_id)
            .map(|(_, entries)| entries.len())
            .sum();
        let held = self
            .multiplexed
            .lock()
            .unwrap()
            .iter()
            .filter(|((id, _), entry)| {
                *id == forward_id
                    && entry.alive.load(Ordering::Relaxed)
                    && !entry.sender.is_closed()
            })
            .count();
        pooled + held
    }

    /// The live HTTP/2 connection for a forward's target, if one is
    /// held. The sender is cloned, which is how concurrent requests
    /// share the connection, and the flag names the connection so a
    /// request that gives up on it can evict that one and no other.
    pub(crate) fn multiplexed(
        &self,
        forward_id: u64,
        port: u16,
    ) -> Option<(SendRequestH2<Body>, Arc<AtomicBool>)> {
        let mut held = self.multiplexed.lock().unwrap();
        let target = (forward_id, port);
        let entry = held.get(&target)?;
        if !entry.alive.load(Ordering::Relaxed) || entry.sender.is_closed() {
            held.remove(&target);
            return None;
        }
        Some((entry.sender.clone(), entry.alive.clone()))
    }

    pub(crate) fn install_multiplexed(
        &self,
        forward_id: u64,
        port: u16,
        sender: SendRequestH2<Body>,
        alive: Arc<AtomicBool>,
    ) {
        self.multiplexed
            .lock()
            .unwrap()
            .insert((forward_id, port), Multiplexed { sender, alive });
    }

    /// Drops the HTTP/2 connection for a target, so the next request
    /// opens a new one. For a connection that failed or stopped answering
    /// mid-request. Only the connection the request was on is dropped:
    /// concurrent requests give up on the same dead one, and the first to
    /// do so must not have its replacement evicted by the rest.
    pub(crate) fn forget_multiplexed(&self, forward_id: u64, port: u16, alive: &Arc<AtomicBool>) {
        let mut held = self.multiplexed.lock().unwrap();
        let target = (forward_id, port);
        if held
            .get(&target)
            .is_some_and(|entry| Arc::ptr_eq(&entry.alive, alive))
        {
            held.remove(&target);
        }
    }

    /// The gate a caller holds while it opens the one connection for a
    /// target.
    pub(crate) fn opening_gate(&self, forward_id: u64, port: u16) -> Arc<tokio::sync::Mutex<()>> {
        self.opening
            .lock()
            .unwrap()
            .entry((forward_id, port))
            .or_default()
            .clone()
    }

    /// Pops one candidate, evicting whatever has gone stale on the way.
    fn take(&self, target: Target) -> Option<Idle> {
        let mut idle = self.idle.lock().unwrap();
        let entries = idle.get_mut(&target)?;
        evict_stale(entries);
        let taken = entries.pop();
        if entries.is_empty() {
            idle.remove(&target);
        }
        taken
    }

    fn put(&self, target: Target, entry: Idle) {
        let mut idle = self.idle.lock().unwrap();
        let entries = idle.entry(target).or_default();
        evict_stale(entries);
        if entries.len() < MAX_IDLE_PER_TARGET {
            entries.push(entry);
        }
    }
}

fn evict_stale(entries: &mut Vec<Idle>) {
    let now = Instant::now();
    entries.retain(|entry| {
        now.duration_since(entry.since) < IDLE_TIMEOUT
            && entry.alive.load(Ordering::Relaxed)
            && !entry.sender.is_closed()
    });
}

/// A checked-out upstream connection.
///
/// Dropping it discards the connection, which is what should happen to
/// one whose response never finished: HTTP/1 has no way to abandon a
/// half-read body and keep the connection.
pub(crate) struct Lease {
    pool: Arc<UpstreamPool>,
    target: Target,
    sender: SendRequest<Body>,
    alive: Arc<AtomicBool>,
    reused: bool,
}

impl Lease {
    pub(crate) fn sender(&mut self) -> &mut SendRequest<Body> {
        &mut self.sender
    }

    /// Whether this connection came from the pool. Only a pooled
    /// connection can have gone stale while it sat there, so only a
    /// pooled one is worth retrying a request on.
    pub(crate) fn reused(&self) -> bool {
        self.reused
    }

    fn release(self) {
        if !self.alive.load(Ordering::Relaxed) || self.sender.is_closed() {
            return;
        }
        self.pool.clone().put(
            self.target,
            Idle {
                sender: self.sender,
                alive: self.alive,
                since: Instant::now(),
            },
        );
    }
}

/// A proxied response body that carries its upstream's lease and hands
/// it back when the stream ends. Returning it when the handler returned
/// would hand out a connection that is still writing this response.
pub(crate) struct PooledBody {
    inner: Incoming,
    lease: Option<Lease>,
}

impl PooledBody {
    pub(crate) fn new(inner: Incoming, lease: Lease) -> Self {
        let mut body = Self {
            inner,
            lease: Some(lease),
        };
        // A response with no body is already complete, and nothing
        // downstream has to poll it for the connection to be free.
        if body.inner.is_end_stream() {
            body.finished();
        }
        body
    }

    fn finished(&mut self) {
        if let Some(lease) = self.lease.take() {
            lease.release();
        }
    }
}

impl HttpBody for PooledBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.as_mut().get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        match polled {
            Poll::Ready(None) => this.finished(),
            Poll::Ready(Some(Err(_))) => drop(this.lease.take()),
            _ => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper_util::rt::TokioIo;
    use tokio::net::{TcpListener, TcpStream};

    /// A loopback HTTP/1.1 server that answers every request with a
    /// fixed body and keeps the connection open.
    async fn keep_alive_server() -> (u16, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = sock.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if sock
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });
        (port, task)
    }

    async fn open(pool: &Arc<UpstreamPool>, forward_id: u64, port: u16) -> Lease {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
            .await
            .unwrap();
        let alive = Arc::new(AtomicBool::new(true));
        let ended = alive.clone();
        tokio::spawn(async move {
            let _ = connection.await;
            ended.store(false, Ordering::Relaxed);
        });
        pool.lease_new(forward_id, port, sender, alive)
    }

    /// Drives one exchange to completion the way the proxy does: send,
    /// read the body to its end through [`PooledBody`], which is what
    /// returns the lease.
    async fn exchange(mut lease: Lease, port: u16) {
        let request = axum::http::Request::builder()
            .uri("/")
            .header("host", format!("127.0.0.1:{port}"))
            .body(Body::empty())
            .unwrap();
        let response = lease.sender().send_request(request).await.unwrap();
        let (_, incoming) = response.into_parts();
        let body = PooledBody::new(incoming, lease);
        let collected = axum::body::to_bytes(Body::new(body), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&collected[..], b"hi");
    }

    #[tokio::test]
    async fn a_finished_exchange_goes_back_and_comes_out_again() {
        let (port, _server) = keep_alive_server().await;
        let pool = Arc::new(UpstreamPool::default());
        exchange(open(&pool, 1, port).await, port).await;
        assert_eq!(pool.idle_count(1), 1);
        let reused = pool.acquire(1, port).await.expect("the pooled upstream");
        assert!(reused.reused());
        assert_eq!(pool.idle_count(1), 0);
    }

    #[tokio::test]
    async fn a_lease_dropped_before_its_body_ends_is_discarded() {
        let (port, _server) = keep_alive_server().await;
        let pool = Arc::new(UpstreamPool::default());
        let mut lease = open(&pool, 1, port).await;
        let request = axum::http::Request::builder()
            .uri("/")
            .header("host", format!("127.0.0.1:{port}"))
            .body(Body::empty())
            .unwrap();
        let response = lease.sender().send_request(request).await.unwrap();
        let (_, incoming) = response.into_parts();
        drop(PooledBody::new(incoming, lease));
        assert_eq!(pool.idle_count(1), 0);
    }

    #[tokio::test]
    async fn the_idle_cap_bounds_what_one_target_holds() {
        let (port, _server) = keep_alive_server().await;
        let pool = Arc::new(UpstreamPool::default());
        for _ in 0..MAX_IDLE_PER_TARGET + 4 {
            exchange(open(&pool, 1, port).await, port).await;
        }
        assert_eq!(pool.idle_count(1), MAX_IDLE_PER_TARGET);
    }

    #[tokio::test]
    async fn forgetting_a_forward_drops_its_upstreams_and_leaves_others() {
        let (port, _server) = keep_alive_server().await;
        let pool = Arc::new(UpstreamPool::default());
        exchange(open(&pool, 1, port).await, port).await;
        exchange(open(&pool, 2, port).await, port).await;
        pool.forget(1);
        assert_eq!(pool.idle_count(1), 0);
        assert_eq!(pool.idle_count(2), 1);
        assert!(pool.acquire(1, port).await.is_none());
    }

    #[tokio::test]
    async fn an_upstream_the_target_closed_is_not_handed_out() {
        let (port, server) = keep_alive_server().await;
        let pool = Arc::new(UpstreamPool::default());
        exchange(open(&pool, 1, port).await, port).await;
        assert_eq!(pool.idle_count(1), 1);
        server.abort();
        // The connection task notices the close and clears the flag the
        // pool checks, which takes a scheduling hop to happen.
        for _ in 0..100 {
            if pool.idle_count(1) == 0 || pool.acquire(1, port).await.is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("a closed upstream stayed reusable");
    }

    #[tokio::test]
    async fn a_forward_repointed_at_a_new_port_does_not_reuse_the_old_one() {
        let (port, _server) = keep_alive_server().await;
        let pool = Arc::new(UpstreamPool::default());
        exchange(open(&pool, 1, port).await, port).await;
        assert!(pool.acquire(1, port + 1).await.is_none());
        assert_eq!(pool.idle_count(1), 1);
    }

    /// Short enough that a dead path shows within a test's time bound.
    const QUICK: ShareKeepalive = ShareKeepalive {
        interval: Duration::from_millis(50),
        timeout: Duration::from_millis(100),
    };

    async fn h2_target() -> (crate::dir_server::DirShareServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<p>shared</p>").unwrap();
        let server = crate::dir_server::serve(dir.path().to_path_buf())
            .await
            .unwrap();
        (server, dir)
    }

    async fn wait_until_dead(alive: &AtomicBool, within: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < within {
            if !alive.load(Ordering::Relaxed) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        !alive.load(Ordering::Relaxed)
    }

    /// The share's connection rides a tunnel that can die without closing.
    /// Nothing is ever written to an idle connection, so only its pings
    /// can tell: a peer that stops answering them ends the connection and
    /// clears the flag the pool checks.
    #[tokio::test]
    async fn a_share_connection_whose_peer_stops_answering_pings_ends() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let (_sender, alive) = multiplexed_handshake(TokioIo::new(ours), QUICK)
            .await
            .unwrap();
        assert!(
            wait_until_dead(&alive, Duration::from_secs(5)).await,
            "a connection nothing answers must end on its keepalive"
        );
        drop(theirs);
    }

    #[tokio::test]
    async fn a_share_connection_whose_peer_answers_pings_stays_up_while_idle() {
        let (server, _dir) = h2_target().await;
        let tcp = TcpStream::connect(("127.0.0.1", server.port()))
            .await
            .unwrap();
        let (mut sender, alive) = multiplexed_handshake(TokioIo::new(tcp), QUICK)
            .await
            .unwrap();
        tokio::time::sleep(QUICK.interval * 10).await;
        assert!(
            alive.load(Ordering::Relaxed),
            "an idle connection whose peer answers is kept"
        );
        let request = axum::http::Request::builder()
            .uri(format!("http://127.0.0.1:{}/", server.port()))
            .body(Body::empty())
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    /// A request that gives up on a connection evicts that connection and
    /// no other: by the time a slow request gives up, a faster one may
    /// have installed the replacement it is about to use.
    #[tokio::test]
    async fn forgetting_a_share_connection_leaves_its_replacement_installed() {
        let (server, _dir) = h2_target().await;
        let pool = UpstreamPool::default();
        let connect = || TcpStream::connect(("127.0.0.1", server.port()));
        let (first, first_alive) =
            multiplexed_handshake(TokioIo::new(connect().await.unwrap()), QUICK)
                .await
                .unwrap();
        pool.install_multiplexed(1, server.port(), first, first_alive.clone());
        let (second, second_alive) =
            multiplexed_handshake(TokioIo::new(connect().await.unwrap()), QUICK)
                .await
                .unwrap();
        pool.install_multiplexed(1, server.port(), second, second_alive.clone());

        pool.forget_multiplexed(1, server.port(), &first_alive);
        let (_, held) = pool
            .multiplexed(1, server.port())
            .expect("the replacement is still held");
        assert!(Arc::ptr_eq(&held, &second_alive));

        pool.forget_multiplexed(1, server.port(), &second_alive);
        assert!(pool.multiplexed(1, server.port()).is_none());
    }
}
