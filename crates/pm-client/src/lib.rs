//! Client library for the daemon's protocol, shared by the CLI, the TUI,
//! and integration tests. It reaches a daemon over its unix socket, or a
//! remote controller over the WebSockets its dashboard uses.

pub mod remote;
mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pm_protocol::domain::{ClientEnvelope, ClientMsg, ServerMsg};
use pm_protocol::frame;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

pub use transport::Remote;

/// Server messages buffered before backpressure applies; sized for
/// bursty PTY output.
const INCOMING_CHANNEL_CAPACITY: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection closed")]
    Closed,
    #[error("daemon error: {0}")]
    Daemon(String),
    /// A remote controller ended the connection because it does not accept
    /// the saved login.
    #[error("{0}")]
    Unauthenticated(String),
}

/// Which daemon a client talks to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The daemon on this machine, over its unix socket.
    Unix(PathBuf),
    /// A controller signed in to with `pm login`.
    Remote(Remote),
}

type CommandReply = Result<(Option<u64>, bytes::Bytes), String>;

#[derive(Default)]
struct Pending {
    waiters: HashMap<u64, oneshot::Sender<CommandReply>>,
    /// Set once the connection is over, with the reason when the
    /// controller refused the login. A request made afterwards fails at
    /// once instead of waiting for a reply nothing will send.
    closed: Option<Option<String>>,
}

/// Where a transport hands what the daemon sent: command results to the
/// request waiting on them, everything else to `next_msg`.
#[derive(Clone)]
pub(crate) struct Dispatch {
    pending: Arc<Mutex<Pending>>,
    incoming_tx: mpsc::Sender<ServerMsg>,
}

impl Dispatch {
    /// Returns false once the client has been dropped.
    pub(crate) async fn deliver(&self, msg: ServerMsg) -> bool {
        if let ServerMsg::CommandResult { seq, result, data } = msg {
            if let Some(waiter) = self.pending.lock().unwrap().waiters.remove(&seq) {
                let _ = waiter.send(result.map(|id| (id, data)));
            }
            return !self.incoming_tx.is_closed();
        }
        self.incoming_tx.send(msg).await.is_ok()
    }

    pub(crate) fn close(&self, refusal: Option<String>) {
        let mut pending = self.pending.lock().unwrap();
        pending.closed = Some(refusal);
        pending.waiters.clear();
    }
}

pub struct Client {
    out_tx: mpsc::UnboundedSender<ClientEnvelope>,
    pending: Arc<Mutex<Pending>>,
    incoming_rx: mpsc::Receiver<ServerMsg>,
    next_seq: AtomicU64,
}

impl Client {
    /// Connects to the daemon on this machine.
    pub async fn connect(socket_path: &Path) -> anyhow::Result<Self> {
        Self::open(&Target::Unix(socket_path.to_path_buf())).await
    }

    pub async fn open(target: &Target) -> anyhow::Result<Self> {
        let (out_tx, out_rx) = mpsc::unbounded_channel::<ClientEnvelope>();
        let (incoming_tx, incoming_rx) = mpsc::channel(INCOMING_CHANNEL_CAPACITY);
        let pending = Arc::new(Mutex::new(Pending::default()));
        let dispatch = Dispatch {
            pending: pending.clone(),
            incoming_tx,
        };
        match target {
            Target::Unix(socket_path) => start_unix(socket_path, out_rx, dispatch).await?,
            Target::Remote(remote) => transport::start(remote, out_rx, dispatch).await?,
        }
        Ok(Client {
            out_tx,
            pending,
            incoming_rx,
            next_seq: AtomicU64::new(1),
        })
    }

    /// Sends a message and waits for its CommandResult. Returns the
    /// created entity id for creates/spawns.
    pub async fn request(&self, msg: ClientMsg) -> Result<Option<u64>, ClientError> {
        Ok(self.request_full(msg).await?.0)
    }

    /// Like `request`, but returns the reply's data payload (used by
    /// queries such as ListWorkspaces).
    pub async fn request_data(&self, msg: ClientMsg) -> Result<bytes::Bytes, ClientError> {
        Ok(self.request_full(msg).await?.1)
    }

    fn closed_error(&self) -> ClientError {
        match self.pending.lock().unwrap().closed.clone().flatten() {
            Some(refusal) => ClientError::Unauthenticated(refusal),
            None => ClientError::Closed,
        }
    }

    async fn request_full(
        &self,
        msg: ClientMsg,
    ) -> Result<(Option<u64>, bytes::Bytes), ClientError> {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.closed.is_some() {
                drop(pending);
                return Err(self.closed_error());
            }
            pending.waiters.insert(seq, tx);
        }
        self.out_tx
            .send(ClientEnvelope { seq, msg })
            .map_err(|_| ClientError::Closed)?;
        match rx.await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(e)) => Err(ClientError::Daemon(e)),
            Err(_) => Err(self.closed_error()),
        }
    }

    /// Fire-and-forget send for PtyInput/PtyResize, which the server
    /// does not acknowledge.
    pub fn send(&self, msg: ClientMsg) -> Result<(), ClientError> {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        self.out_tx
            .send(ClientEnvelope { seq, msg })
            .map_err(|_| ClientError::Closed)
    }

    /// Next non-result server message (Snapshot, Event, PtyOutput).
    /// None when the connection is closed.
    pub async fn next_msg(&mut self) -> Option<ServerMsg> {
        self.incoming_rx.recv().await
    }
}

async fn start_unix(
    socket_path: &Path,
    mut out_rx: mpsc::UnboundedReceiver<ClientEnvelope>,
    dispatch: Dispatch,
) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!(
            "cannot connect to daemon at {} ({e}), is `pm daemon` running?",
            socket_path.display()
        )
    })?;
    let (mut read_half, mut write_half) = stream.into_split();

    tokio::spawn(async move {
        while let Some(envelope) = out_rx.recv().await {
            if frame::write_frame(&mut write_half, &envelope.encode_to_vec())
                .await
                .is_err()
            {
                break;
            }
        }
    });

    tokio::spawn(async move {
        loop {
            let buf = match frame::read_frame(&mut read_half).await {
                Ok(Some(b)) => b,
                _ => break,
            };
            let msg = match ServerMsg::decode(&buf) {
                Ok(m) => m,
                Err(_) => break,
            };
            if !dispatch.deliver(msg).await {
                break;
            }
        }
        dispatch.close(None);
    });
    Ok(())
}
