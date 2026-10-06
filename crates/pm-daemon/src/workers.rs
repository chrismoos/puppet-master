//! Worker transports and the shared terminal relay used by both local and
//! remote sessions. A local worker calls the in-process mux while a remote
//! worker crosses the worker link; viewer gating, replay, fan-out, input,
//! resize, and detach are otherwise identical.

use crate::term_model::Retained;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use pm_protocol::domain::{ControllerMsg, FsEntry, WorkerTranscript};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::mux::SCROLLBACK_CAP_BYTES;

/// A worker's reply to a filesystem listing request.
/// A worker's answer to a repository read: the encoded `RepoAnswer`,
/// or the reason it could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoResponseResult {
    pub ok: bool,
    pub error: String,
    pub answer: Vec<u8>,
}

pub struct FsListingResult {
    pub ok: bool,
    pub error: String,
    pub dir: String,
    pub parent: Option<String>,
    pub entries: Vec<FsEntry>,
}

/// One message bound for a session's agent inbox on a worker's host.
///
/// The worker contributes the PTY child's pid and the directory the
/// agent binds its socket in, because those are facts about its own
/// host. Everything here is what only the controller records.
pub struct AgentInboxRequest {
    pub session_id: u64,
    /// The session's agent terminal. The worker's mux is keyed by
    /// terminal, so a session id would find an unrelated child there.
    pub agent_terminal_id: u64,
    pub agent: pm_protocol::domain::AgentKind,
    pub agent_session_id: String,
    pub agent_port: Option<u16>,
    pub text: String,
    pub mode: pm_protocol::domain::AgentInboxMode,
}

/// What a worker's host did with a message bound for a session's
/// agent inbox.
pub struct AgentInboxResult {
    pub outcome: pm_protocol::domain::AgentInboxOutcome,
    pub transport: String,
    pub mode: pm_protocol::domain::AgentInboxMode,
    pub detail: String,
}

/// A host's verdict on one configured project path.
pub struct PathCheckResult {
    pub status: pm_protocol::domain::PathCheck,
    pub detail: String,
}

pub struct FileReadResult {
    pub ok: bool,
    pub error: String,
    pub content: Vec<u8>,
    pub filename: String,
}

/// Outcome of a worker's local dial for one forwarded stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardOpenResult {
    pub ok: bool,
    pub error: String,
}

/// Outcome of a worker binding a server for one directory share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirShareBoundResult {
    pub ok: bool,
    pub error: String,
    pub port: u16,
}

/// Buffered PTY chunks a slow viewer may fall behind before being
/// force-detached, matching the local mux.
const VIEWER_CHANNEL_CHUNKS: usize = 1024;

/// Buffered size echoes per viewer. Echoes are sent only when the PTY
/// size actually changes, so this depth is never reached in practice.
const VIEWER_SIZE_CHANNEL_EVENTS: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker {0} is offline")]
    Offline(u64),
    #[error("worker link closed")]
    Closed,
    #[error("worker terminal input is busy")]
    Busy,
}

/// A terminal's controller-side scrollback and viewer fan-out. The
/// ring lock guards both the buffer and the broadcast send so attach's
/// replay-then-live handoff loses no bytes, exactly as the mux does.
struct RelaySession {
    retained: Mutex<Retained>,
    out_tx: broadcast::Sender<Bytes>,
    size_tx: broadcast::Sender<(u16, u16)>,
    /// Tells viewers the mirror's history was rewritten, which makes the
    /// snapshot each one holds stale.
    rewrite_tx: broadcast::Sender<()>,
    relay: Mutex<RelayState>,
    /// Live output bytes fed so far, against which each viewer's progress
    /// says how far behind it is.
    fed: std::sync::atomic::AtomicU64,
    viewer_progress: Mutex<
        Vec<(
            Arc<std::sync::atomic::AtomicU64>,
            Arc<std::sync::atomic::AtomicBool>,
        )>,
    >,
    drained: tokio::sync::Notify,
}

/// Output a viewer may have unsent before the relay stops taking more
/// from the worker, which TCP then turns into the worker's own wait.
pub const VIEWER_BACKLOG_BUDGET_BYTES: u64 = 16 * 1024;

/// How long the relay waits on a viewer that is behind and making no
/// progress before it stops pacing output to that viewer. A hidden browser
/// tab throttles its timers to once a second or slower, and without this
/// bound it would stall the terminal for every viewer and the agent too.
pub const VIEWER_STALL_LIMIT: std::time::Duration = std::time::Duration::from_millis(150);

/// A viewer's progress counter, unregistered when the viewer goes.
/// It holds the relay session weakly: a strong reference would keep the
/// session's broadcast alive after the stream ended and the viewer would
/// never see it close.
pub struct ViewerProgress {
    session: Option<std::sync::Weak<RelaySession>>,
    pub sent: Arc<std::sync::atomic::AtomicU64>,
    /// Cleared when the viewer stalls the relay, set again by its next credit.
    gating: Arc<std::sync::atomic::AtomicBool>,
    /// Set once the viewer has acknowledged parsed bytes, after which
    /// socket sends no longer count: the viewer's own parse pace does.
    acks: std::sync::atomic::AtomicBool,
}

impl ViewerProgress {
    /// Credits bytes the viewer's socket accepted, for a viewer that
    /// sends no acknowledgements.
    pub fn sent(&self, bytes: usize) {
        if self.acks.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        self.credit(bytes as u64);
    }

    /// Credits bytes the viewer has parsed.
    pub fn acked(&self, bytes: u32) {
        self.acks.store(true, std::sync::atomic::Ordering::Relaxed);
        self.credit(u64::from(bytes));
    }

    fn credit(&self, bytes: u64) {
        self.sent
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        self.gating
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(session) = self.session.as_ref().and_then(std::sync::Weak::upgrade) {
            session.drained.notify_waiters();
        }
    }
}

impl Drop for ViewerProgress {
    fn drop(&mut self) {
        if let Some(session) = self.session.as_ref().and_then(std::sync::Weak::upgrade) {
            session
                .viewer_progress
                .lock()
                .unwrap()
                .retain(|(p, _)| !Arc::ptr_eq(p, &self.sent));
            session.drained.notify_waiters();
        }
    }
}

/// Per-session gating state: how many viewers are attached, whether an
/// attach is waiting for the worker's replay, and an epoch that
/// invalidates a pending debounced detach once a viewer returns.
#[derive(Default)]
struct RelayState {
    viewers: usize,
    detach_epoch: u64,
    generation: u64,
    replay_pending: bool,
    replay_started: bool,
    replay_complete_waiters: Vec<oneshot::Sender<Bytes>>,
    replay_stream_waiters: Vec<mpsc::UnboundedSender<ReplayChunk>>,
    replay: Vec<u8>,
    upstream: Option<mpsc::Sender<Bytes>>,
    stream_epoch: u64,
}

impl RelaySession {
    fn new() -> Self {
        let (out_tx, _) = broadcast::channel(VIEWER_CHANNEL_CHUNKS);
        let (size_tx, _) = broadcast::channel(VIEWER_SIZE_CHANNEL_EVENTS);
        let (rewrite_tx, _) = broadcast::channel(VIEWER_SIZE_CHANNEL_EVENTS);
        RelaySession {
            fed: std::sync::atomic::AtomicU64::new(0),
            viewer_progress: Mutex::new(Vec::new()),
            drained: tokio::sync::Notify::new(),
            retained: Mutex::new(Retained::new(
                SCROLLBACK_CAP_BYTES,
                crate::mux::DEFAULT_COLS,
                crate::mux::DEFAULT_ROWS,
            )),
            out_tx,
            size_tx,
            rewrite_tx,
            relay: Mutex::new(RelayState::default()),
        }
    }
}

/// The result of a viewer attaching through a worker relay.
pub enum ViewerAttach {
    /// The first viewer: the caller asks the worker to start relaying and
    /// awaits the replay on this channel.
    First {
        rx: broadcast::Receiver<Bytes>,
        replay: oneshot::Receiver<Bytes>,
    },
    Waiting {
        rx: broadcast::Receiver<Bytes>,
        replay: oneshot::Receiver<Bytes>,
    },
    /// A later viewer, while the session is already relayed: replay comes
    /// from the warm ring, no worker round-trip.
    Joined {
        rx: broadcast::Receiver<Bytes>,
        replay: Bytes,
    },
}

pub struct ReplayChunk {
    pub flags: u8,
    pub data: Bytes,
}

pub enum StreamingViewerAttach {
    First {
        rx: broadcast::Receiver<Bytes>,
        replay: mpsc::UnboundedReceiver<ReplayChunk>,
    },
    Waiting {
        rx: broadcast::Receiver<Bytes>,
        replay: mpsc::UnboundedReceiver<ReplayChunk>,
    },
    Joined {
        rx: broadcast::Receiver<Bytes>,
        replay: Bytes,
    },
}

enum WorkerTransport {
    Remote(mpsc::Sender<ControllerMsg>),
    Local(Arc<crate::mux::Mux>),
}

/// One worker transport. Local and remote workers share the same terminal
/// relay; only delivery to the PTY owner differs.
/// Where to reach a host the controller dials, so the per-stream
/// connections can go the same way the control link did.
pub struct StreamDial {
    pub endpoint: String,
    pub host_key: pm_tls::KeyHash,
    pub daemon: std::sync::Weak<crate::daemon::Daemon>,
}

pub struct WorkerLink {
    pub worker_id: u64,
    transport: WorkerTransport,
    /// Set for a host the controller dials. Absent means the host opens its
    /// own streams, which is the other direction.
    dial: Mutex<Option<Arc<StreamDial>>>,
    sessions: Mutex<HashMap<u64, Arc<RelaySession>>>,
    fs_seq: AtomicU64,
    /// What the peer announced it can do; 0 until it registers.
    protocol_version: AtomicU64,
    repo_seq: AtomicU64,
    repo_pending: Mutex<HashMap<u64, oneshot::Sender<RepoResponseResult>>>,
    fs_pending: Mutex<HashMap<u64, oneshot::Sender<FsListingResult>>>,
    harness_seq: AtomicU64,
    harness_pending: Mutex<HashMap<u64, oneshot::Sender<pm_protocol::domain::HarnessStatus>>>,
    path_check_seq: AtomicU64,
    path_check_pending: Mutex<HashMap<u64, oneshot::Sender<PathCheckResult>>>,
    agent_inbox_seq: AtomicU64,
    agent_inbox_pending: Mutex<HashMap<u64, oneshot::Sender<AgentInboxResult>>>,
    file_seq: AtomicU64,
    file_pending: Mutex<HashMap<u64, oneshot::Sender<FileReadResult>>>,
    forward_seq: AtomicU64,
    forward_pending: Mutex<HashMap<u64, oneshot::Sender<ForwardOpenResult>>>,
    /// Keyed by share id rather than a request sequence: a share has one
    /// server at a time, and the worker answers for the share it bound
    /// rather than for a particular asking. Every waiter on a share is
    /// kept, because the worker serves a share it already holds on the
    /// port it already holds, so one reply answers all of them — and
    /// dropping a waiter to make room would strand its caller.
    dir_share_pending: Mutex<HashMap<u64, Vec<oneshot::Sender<DirShareBoundResult>>>>,
    /// Set when a newer connection for the same worker took this link's
    /// place. The old socket is not always dead when the new one
    /// arrives, so a superseded link has to be told to stop rather than
    /// left relaying beside its replacement.
    superseded: AtomicBool,
    /// Wakes whoever is relaying this link once it is superseded.
    closed: tokio::sync::Notify,
}

impl WorkerLink {
    pub fn local(mux: Arc<crate::mux::Mux>) -> Arc<Self> {
        Arc::new(Self {
            worker_id: pm_protocol::domain::LOCAL_WORKER_ID,
            transport: WorkerTransport::Local(mux),
            dial: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            fs_seq: AtomicU64::new(0),
            protocol_version: AtomicU64::new(0),
            repo_seq: AtomicU64::new(0),
            repo_pending: Mutex::new(HashMap::new()),
            fs_pending: Mutex::new(HashMap::new()),
            harness_seq: AtomicU64::new(0),
            harness_pending: Mutex::new(HashMap::new()),
            path_check_seq: AtomicU64::new(0),
            path_check_pending: Mutex::new(HashMap::new()),
            agent_inbox_seq: AtomicU64::new(0),
            agent_inbox_pending: Mutex::new(HashMap::new()),
            file_seq: AtomicU64::new(0),
            file_pending: Mutex::new(HashMap::new()),
            forward_seq: AtomicU64::new(0),
            forward_pending: Mutex::new(HashMap::new()),
            dir_share_pending: Mutex::new(HashMap::new()),
            superseded: AtomicBool::new(false),
            closed: tokio::sync::Notify::new(),
        })
    }

    /// Retires this link because a newer connection replaced it.
    pub fn supersede(&self) {
        self.superseded.store(true, Ordering::SeqCst);
        self.closed.notify_waiters();
    }

    /// Whether a newer connection has taken this link's place.
    pub fn is_superseded(&self) -> bool {
        self.superseded.load(Ordering::SeqCst)
    }

    /// Resolves once a newer connection replaces this link. The waiter is
    /// registered before the flag is read, so a supersede landing between
    /// the two is still observed rather than waited on forever.
    pub async fn superseded(&self) {
        let waiting = self.closed.notified();
        if self.is_superseded() {
            return;
        }
        waiting.await;
    }

    pub fn is_local(&self) -> bool {
        matches!(self.transport, WorkerTransport::Local(_))
    }

    pub fn set_protocol_version(&self, version: u32) {
        self.protocol_version
            .store(version as u64, Ordering::Relaxed);
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version.load(Ordering::Relaxed) as u32
    }

    /// Ships an encoded repository op to the worker and returns a
    /// receiver for its answer. The bytes are passed through untouched,
    /// so the worker decodes exactly what the controller encoded.
    pub fn request_repo(
        &self,
        op: Vec<u8>,
    ) -> Result<oneshot::Receiver<RepoResponseResult>, WorkerError> {
        let req_id = self.repo_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.repo_pending.lock().unwrap().insert(req_id, tx);
        if let Err(e) = self.send(ControllerMsg::RepoRequest { req_id, op }) {
            self.repo_pending.lock().unwrap().remove(&req_id);
            return Err(e);
        }
        Ok(rx)
    }

    pub fn resolve_repo(&self, req_id: u64, result: RepoResponseResult) {
        if let Some(tx) = self.repo_pending.lock().unwrap().remove(&req_id) {
            let _ = tx.send(result);
        }
    }

    /// Sends a directory listing request to the worker and returns a
    /// receiver for its reply, correlated by a fresh req id.
    pub fn request_fs(
        &self,
        path: String,
    ) -> Result<oneshot::Receiver<FsListingResult>, WorkerError> {
        let req_id = self.fs_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.fs_pending.lock().unwrap().insert(req_id, tx);
        if let Err(e) = self.send(ControllerMsg::FsList { req_id, path }) {
            self.fs_pending.lock().unwrap().remove(&req_id);
            return Err(e);
        }
        Ok(rx)
    }

    /// Completes a pending listing request with the worker's reply.
    pub fn resolve_fs(&self, req_id: u64, result: FsListingResult) {
        if let Some(tx) = self.fs_pending.lock().unwrap().remove(&req_id) {
            let _ = tx.send(result);
        }
    }

    pub fn request_harness(
        &self,
        agent: pm_protocol::domain::AgentKind,
        install: bool,
    ) -> Result<oneshot::Receiver<pm_protocol::domain::HarnessStatus>, WorkerError> {
        let req_id = self.harness_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let mut pending = self.harness_pending.lock().unwrap();
        pending.retain(|_, sender| !sender.is_closed());
        pending.insert(req_id, tx);
        if let Err(e) = self.send(ControllerMsg::HarnessRequest {
            req_id,
            agent,
            install,
        }) {
            pending.remove(&req_id);
            return Err(e);
        }
        Ok(rx)
    }

    pub fn resolve_harness(&self, req_id: u64, status: pm_protocol::domain::HarnessStatus) {
        if let Some(tx) = self.harness_pending.lock().unwrap().remove(&req_id) {
            let _ = tx.send(status);
        }
    }

    pub fn request_path_check(
        &self,
        path: String,
    ) -> Result<oneshot::Receiver<PathCheckResult>, WorkerError> {
        let req_id = self.path_check_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.path_check_pending.lock().unwrap().insert(req_id, tx);
        if let Err(e) = self.send(ControllerMsg::PathCheck { req_id, path }) {
            self.path_check_pending.lock().unwrap().remove(&req_id);
            return Err(e);
        }
        Ok(rx)
    }

    /// Completes a pending path check with the worker's verdict.
    pub fn resolve_path_check(&self, req_id: u64, result: PathCheckResult) {
        if let Some(tx) = self.path_check_pending.lock().unwrap().remove(&req_id) {
            let _ = tx.send(result);
        }
    }

    /// Asks the worker's host to hand a message to a session's agent
    /// over the agent's own inbound channel.
    ///
    /// The caller gates this on the peer announcing
    /// `WORKER_PROTOCOL_AGENT_INBOX`: an older worker drops a message
    /// it cannot decode and would never answer.
    pub fn request_agent_inbox(
        &self,
        request: AgentInboxRequest,
    ) -> Result<oneshot::Receiver<AgentInboxResult>, WorkerError> {
        let AgentInboxRequest {
            session_id,
            agent_terminal_id,
            agent,
            agent_session_id,
            agent_port,
            text,
            mode,
        } = request;
        let req_id = self.agent_inbox_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.agent_inbox_pending.lock().unwrap().insert(req_id, tx);
        if let Err(e) = self.send(ControllerMsg::AgentInbox {
            req_id,
            session_id,
            agent_terminal_id,
            agent,
            agent_session_id,
            agent_port,
            text,
            mode,
        }) {
            self.agent_inbox_pending.lock().unwrap().remove(&req_id);
            return Err(e);
        }
        Ok(rx)
    }

    /// Completes a pending inbox delivery with the worker's outcome.
    pub fn resolve_agent_inbox(&self, req_id: u64, result: AgentInboxResult) {
        if let Some(tx) = self.agent_inbox_pending.lock().unwrap().remove(&req_id) {
            let _ = tx.send(result);
        }
    }

    pub fn request_file(
        &self,
        root: String,
        path: String,
        max_bytes: u64,
    ) -> Result<oneshot::Receiver<FileReadResult>, WorkerError> {
        let req_id = self.file_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.file_pending.lock().unwrap().insert(req_id, tx);
        if let Err(error) = self.send(ControllerMsg::FileRead {
            req_id,
            root,
            path,
            max_bytes,
        }) {
            self.file_pending.lock().unwrap().remove(&req_id);
            return Err(error);
        }
        Ok(rx)
    }

    pub fn resolve_file(&self, req_id: u64, result: FileReadResult) {
        if let Some(tx) = self.file_pending.lock().unwrap().remove(&req_id) {
            let _ = tx.send(result);
        }
    }

    /// Asks the worker to open one forwarded stream to `port`, dialing
    /// back with `token`, and returns a receiver for the dial outcome.
    pub fn request_forward_open(
        &self,
        port: u16,
        token: String,
    ) -> Result<oneshot::Receiver<ForwardOpenResult>, WorkerError> {
        let req_id = self.forward_seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.forward_pending.lock().unwrap().insert(req_id, tx);
        if let Err(e) = self.send(ControllerMsg::ForwardOpen {
            req_id,
            port,
            token,
        }) {
            self.forward_pending.lock().unwrap().remove(&req_id);
            return Err(e);
        }
        Ok(rx)
    }

    /// Completes a pending forward open with the worker's dial outcome.
    pub fn resolve_forward_open(&self, req_id: u64, result: ForwardOpenResult) {
        if let Some(tx) = self.forward_pending.lock().unwrap().remove(&req_id) {
            let _ = tx.send(result);
        }
    }

    /// Asks the worker to serve a share's directory and returns a
    /// receiver for the port it bound.
    pub fn request_dir_share_serve(
        &self,
        share_id: u64,
        root: String,
        path: String,
    ) -> Result<oneshot::Receiver<DirShareBoundResult>, WorkerError> {
        let (tx, rx) = oneshot::channel();
        self.dir_share_pending
            .lock()
            .unwrap()
            .entry(share_id)
            .or_default()
            .push(tx);
        if let Err(e) = self.send(ControllerMsg::DirShareServe {
            share_id,
            root,
            path,
        }) {
            self.dir_share_pending.lock().unwrap().remove(&share_id);
            return Err(e);
        }
        Ok(rx)
    }

    pub fn send_dir_share_stop(&self, share_id: u64) -> Result<(), WorkerError> {
        self.dir_share_pending.lock().unwrap().remove(&share_id);
        self.send(ControllerMsg::DirShareStop { share_id })
    }

    /// Completes the pending serves for a share with the port the worker
    /// bound.
    pub fn resolve_dir_share_bound(&self, share_id: u64, result: DirShareBoundResult) {
        let waiting = self
            .dir_share_pending
            .lock()
            .unwrap()
            .remove(&share_id)
            .unwrap_or_default();
        for tx in waiting {
            let _ = tx.send(result.clone());
        }
    }

    fn session(&self, session_id: u64) -> Arc<RelaySession> {
        self.sessions
            .lock()
            .unwrap()
            .entry(session_id)
            .or_insert_with(|| Arc::new(RelaySession::new()))
            .clone()
    }

    /// Sends a control message down to the worker.
    /// Marks this link as one the controller opened, so its streams are
    /// opened the same way rather than waiting for a dial-back that a host
    /// with no route out could never make.
    pub fn dials_streams(&self, dial: Arc<StreamDial>) {
        *self.dial.lock().unwrap() = Some(dial);
    }

    pub fn send(&self, msg: ControllerMsg) -> Result<(), WorkerError> {
        if let Some(dial) = self.dial.lock().unwrap().clone() {
            crate::worker_dialer::open_announced_stream(&dial, &msg);
        }
        match &self.transport {
            WorkerTransport::Remote(to_worker) => to_worker.try_send(msg).map_err(|e| match e {
                mpsc::error::TrySendError::Closed(_) => WorkerError::Closed,
                // A full control channel means the worker link is wedged;
                // treat it as closed so the caller surfaces an error.
                mpsc::error::TrySendError::Full(_) => WorkerError::Closed,
            }),
            WorkerTransport::Local(_) => match msg {
                ControllerMsg::TerminalDetach { terminal_id, .. } => {
                    self.remove_session(terminal_id);
                    Ok(())
                }
                _ => Err(WorkerError::Closed),
            },
        }
    }

    /// Starts the local transport for the first viewer. Replay and live output
    /// are injected into the same relay protocol remote workers use. Input and
    /// resize stay direct in-process calls; the channel marks stream lifetime.
    pub fn start_local_terminal_stream(
        self: &Arc<Self>,
        terminal_id: u64,
        generation: u64,
    ) -> Result<(), WorkerError> {
        let WorkerTransport::Local(mux) = &self.transport else {
            return Err(WorkerError::Closed);
        };
        let crate::mux::TerminalSnapshot {
            bytes: replay,
            size: (cols, rows),
            mut output,
        } = mux
            .attach_snapshot(terminal_id)
            .map_err(|_| WorkerError::Closed)?;
        let (upstream, mut stream_lifetime) = mpsc::channel(1);
        let stream_epoch = self.connect_terminal_stream(terminal_id, generation, upstream);
        self.mirror_terminal_resize(terminal_id, cols, rows);
        let replay_flags = pm_protocol::terminal_frame::FLAG_REPLAY
            | pm_protocol::terminal_frame::FLAG_REPLAY_START
            | pm_protocol::terminal_frame::FLAG_REPLAY_END
            | pm_protocol::terminal_frame::FLAG_REPLAY_SNAPSHOT;
        self.feed_terminal_output(terminal_id, generation, replay_flags, replay);
        // A resize that raced the snapshot reflows the mirror the way it
        // reflowed the PTY's own model, before any later output reaches it.
        if let Ok((cols, rows)) = mux.current_size(terminal_id) {
            self.mirror_terminal_resize(terminal_id, cols, rows);
        }

        let link = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = stream_lifetime.recv() => break,
                    chunk = output.recv() => match chunk {
                        Ok(chunk) => {
                            let Some(link) = link.upgrade() else { break; };
                            if !link.feed_live_if_current(terminal_id, stream_epoch, chunk) {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                }
            }
            if let Some(link) = link.upgrade() {
                link.disconnect_terminal_stream(terminal_id, stream_epoch);
            }
        });
        Ok(())
    }

    fn feed_live_if_current(&self, terminal_id: u64, stream_epoch: u64, data: Bytes) -> bool {
        let session = self.sessions.lock().unwrap().get(&terminal_id).cloned();
        let Some(session) = session else {
            return false;
        };
        if session.relay.lock().unwrap().stream_epoch != stream_epoch {
            return false;
        }
        let mut retained = session.retained.lock().unwrap();
        retained.push(&data);
        // A repaint the mirror recognised removed lines every viewer already
        // has, so each takes a fresh snapshot.
        if retained.model.take_rewritten() {
            let _ = session.rewrite_tx.send(());
        }
        let _ = session.out_tx.send(data);
        true
    }

    /// Applies a relayed PTY chunk. A replay chunk (the worker's
    /// scrollback when it starts relaying) resets the ring and is handed
    /// to the viewer awaiting it; live chunks append and fan out.
    pub fn feed_terminal_output(&self, session_id: u64, generation: u64, flags: u8, data: Bytes) {
        let session = self.session(session_id);
        let replay = flags & pm_protocol::terminal_frame::FLAG_REPLAY != 0;
        let mut relay = session.relay.lock().unwrap();
        if generation < relay.generation {
            return;
        }
        if generation > relay.generation {
            relay.generation = generation;
            relay.replay.clear();
        }
        if replay && flags & pm_protocol::terminal_frame::FLAG_REPLAY_START != 0 {
            relay.replay.clear();
            relay.replay_started = true;
        }
        if replay {
            let overflow = (relay.replay.len() + data.len()).saturating_sub(SCROLLBACK_CAP_BYTES);
            if overflow > 0 {
                relay.replay.drain(..overflow);
            }
            relay.replay.extend_from_slice(&data);
            relay.replay_stream_waiters.retain(|tx| {
                tx.send(ReplayChunk {
                    flags,
                    data: data.clone(),
                })
                .is_ok()
            });
            if flags & pm_protocol::terminal_frame::FLAG_REPLAY_END == 0 {
                return;
            }
            let snapshot = Bytes::copy_from_slice(&relay.replay);
            relay.replay.clear();
            relay.replay_pending = false;
            relay.replay_started = false;
            relay.replay_stream_waiters.clear();
            let waiters = std::mem::take(&mut relay.replay_complete_waiters);
            session
                .retained
                .lock()
                .unwrap()
                .reset_from_replay(&snapshot);
            drop(relay);
            for tx in waiters {
                let _ = tx.send(snapshot.clone());
            }
            return;
        }
        drop(relay);
        let mut retained = session.retained.lock().unwrap();
        retained.push(&data);
        // A repaint the mirror recognised removed lines every viewer already
        // has, so each takes a fresh snapshot.
        if retained.model.take_rewritten() {
            let _ = session.rewrite_tx.send(());
        }
        session
            .fed
            .fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed);
        let _ = session.out_tx.send(data);
    }

    /// A counter a web viewer advances as its socket accepts output. A
    /// local terminal has no relay to pace, so its counter pacing nothing.
    pub fn viewer_progress(&self, session_id: u64) -> ViewerProgress {
        let fed = |session: &RelaySession| session.fed.load(std::sync::atomic::Ordering::Relaxed);
        match &self.transport {
            WorkerTransport::Local(_) => ViewerProgress {
                session: None,
                sent: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                gating: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                acks: std::sync::atomic::AtomicBool::new(false),
            },
            WorkerTransport::Remote(_) => {
                let session = self.session(session_id);
                let sent = Arc::new(std::sync::atomic::AtomicU64::new(fed(&session)));
                let gating = Arc::new(std::sync::atomic::AtomicBool::new(true));
                session
                    .viewer_progress
                    .lock()
                    .unwrap()
                    .push((sent.clone(), gating.clone()));
                ViewerProgress {
                    session: Some(Arc::downgrade(&session)),
                    sent,
                    gating,
                    acks: std::sync::atomic::AtomicBool::new(false),
                }
            }
        }
    }

    /// A fresh snapshot of the terminal plus a new output subscription,
    /// taken under the lock live output is sent under, so nothing is lost
    /// or repeated across the swap. The viewer's progress moves to what the
    /// relay has fed, since the chunks the old subscription held are in the
    /// snapshot now.
    pub fn resnapshot_viewer(
        &self,
        session_id: u64,
        progress: &ViewerProgress,
    ) -> Option<(Bytes, broadcast::Receiver<Bytes>, (u16, u16))> {
        let session = self.sessions.lock().unwrap().get(&session_id)?.clone();
        let retained = session.retained.lock().unwrap();
        let rx = session.out_tx.subscribe();
        progress.sent.store(
            session.fed.load(std::sync::atomic::Ordering::Relaxed),
            std::sync::atomic::Ordering::Relaxed,
        );
        session.drained.notify_waiters();
        Some((Bytes::from(retained.model.snapshot()), rx, retained.size()))
    }

    /// Waits until every viewer is within the backlog budget of what the
    /// relay has fed, so a flood is paced by the slowest viewer rather
    /// than queued without bound.
    pub async fn wait_for_viewers(&self, session_id: u64) {
        let session = self.session(session_id);
        use std::sync::atomic::Ordering::Relaxed;
        loop {
            let notified = session.drained.notified();
            let fed = session.fed.load(Relaxed);
            let behind: Vec<_> = session
                .viewer_progress
                .lock()
                .unwrap()
                .iter()
                .filter(|(sent, gating)| {
                    gating.load(Relaxed)
                        && fed.saturating_sub(sent.load(Relaxed)) > VIEWER_BACKLOG_BUDGET_BYTES
                })
                .map(|(sent, gating)| (sent.clone(), gating.clone(), sent.load(Relaxed)))
                .collect();
            if behind.is_empty() {
                return;
            }
            if tokio::time::timeout(VIEWER_STALL_LIMIT, notified)
                .await
                .is_err()
            {
                for (sent, gating, before) in behind {
                    if sent.load(Relaxed) == before {
                        gating.store(false, Relaxed);
                    }
                }
            }
        }
    }

    pub fn connect_terminal_stream(
        &self,
        terminal_id: u64,
        generation: u64,
        upstream: mpsc::Sender<Bytes>,
    ) -> u64 {
        let session = self.session(terminal_id);
        let mut relay = session.relay.lock().unwrap();
        relay.generation = generation;
        relay.stream_epoch = relay.stream_epoch.wrapping_add(1);
        relay.upstream = Some(upstream);
        relay.stream_epoch
    }

    pub fn disconnect_terminal_stream(&self, terminal_id: u64, stream_epoch: u64) {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get(&terminal_id) else {
            return;
        };
        let current = session.relay.lock().unwrap().stream_epoch == stream_epoch;
        if current {
            sessions.remove(&terminal_id);
        }
    }

    pub fn terminal_input(
        &self,
        terminal_id: u64,
        generation: u64,
        data: Bytes,
    ) -> Result<(), WorkerError> {
        match &self.transport {
            WorkerTransport::Local(mux) => mux
                .input(terminal_id, data)
                .map_err(|_| WorkerError::Closed),
            WorkerTransport::Remote(_) => self.send_terminal_frame(
                terminal_id,
                generation,
                pm_protocol::terminal_frame::encode_input(generation, &data),
            ),
        }
    }

    pub fn terminal_resize(
        &self,
        terminal_id: u64,
        generation: u64,
        cols: u16,
        rows: u16,
    ) -> Result<(), WorkerError> {
        match &self.transport {
            WorkerTransport::Local(mux) => mux
                .resize(terminal_id, cols, rows)
                .map_err(|_| WorkerError::Closed),
            WorkerTransport::Remote(_) => self.send_terminal_frame(
                terminal_id,
                generation,
                pm_protocol::terminal_frame::encode_resize(generation, cols, rows),
            ),
        }
    }

    fn send_terminal_frame(
        &self,
        terminal_id: u64,
        generation: u64,
        frame: Bytes,
    ) -> Result<(), WorkerError> {
        crate::probe_trace::mark("d_stream_q", &frame);
        let result = {
            let sessions = self.sessions.lock().unwrap();
            let session = sessions.get(&terminal_id).ok_or(WorkerError::Closed)?;
            let relay = session.relay.lock().unwrap();
            if relay.generation != generation {
                return Err(WorkerError::Closed);
            }
            relay
                .upstream
                .as_ref()
                .ok_or(WorkerError::Closed)?
                .try_send(frame)
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => WorkerError::Busy,
                    mpsc::error::TrySendError::Closed(_) => WorkerError::Closed,
                })
        };
        if matches!(result, Err(WorkerError::Closed)) {
            self.remove_session(terminal_id);
        }
        result
    }

    /// Registers a viewer. The first viewer of a session arms a replay
    /// waiter (the caller then sends TerminalAttach); a later
    /// viewer joins the warm mirror with no worker round-trip. Subscribing
    /// happens under the ring lock so no live chunk is missed.
    ///
    /// A joiner is served the mirror model rather than the retained ring.
    /// The ring holds raw PTY bytes, and any prefix of it that a joiner
    /// does not receive leaves the rest meaningless: it resumes mid
    /// escape sequence and carries absolute cursor moves computed against
    /// a screen the joiner never saw.
    pub fn viewer_attach(&self, session_id: u64) -> ViewerAttach {
        let session = self.session(session_id);
        let mut relay = session.relay.lock().unwrap();
        let retained = session.retained.lock().unwrap();
        let rx = session.out_tx.subscribe();
        relay.viewers += 1;
        relay.detach_epoch += 1;
        if relay.replay_pending {
            let (tx, replay) = oneshot::channel();
            relay.replay_complete_waiters.push(tx);
            ViewerAttach::Waiting { rx, replay }
        } else if relay.viewers == 1 && relay.upstream.is_none() {
            let (tx, replay) = oneshot::channel();
            relay.replay_pending = true;
            relay.replay_complete_waiters.push(tx);
            ViewerAttach::First { rx, replay }
        } else {
            ViewerAttach::Joined {
                rx,
                replay: Bytes::from(retained.model.snapshot()),
            }
        }
    }

    pub fn streaming_viewer_attach(&self, session_id: u64) -> StreamingViewerAttach {
        let session = self.session(session_id);
        let mut relay = session.relay.lock().unwrap();
        let retained = session.retained.lock().unwrap();
        let rx = session.out_tx.subscribe();
        relay.viewers += 1;
        relay.detach_epoch += 1;
        if relay.replay_pending {
            let replay = Self::add_streaming_replay_waiter(&mut relay);
            StreamingViewerAttach::Waiting { rx, replay }
        } else if relay.viewers == 1 && relay.upstream.is_none() {
            relay.replay_pending = true;
            let replay = Self::add_streaming_replay_waiter(&mut relay);
            StreamingViewerAttach::First { rx, replay }
        } else {
            StreamingViewerAttach::Joined {
                rx,
                replay: Bytes::from(retained.model.snapshot()),
            }
        }
    }

    /// Keeps the mirror model's dimensions in step with resizes the
    /// controller forwards to the worker, and echoes each actual size
    /// change to attached viewers. Sending under the retained lock keeps
    /// echoes ordered against `viewer_size_feed` subscriptions.
    pub fn mirror_terminal_resize(&self, session_id: u64, cols: u16, rows: u16) {
        if let Some(session) = self.sessions.lock().unwrap().get(&session_id) {
            let mut retained = session.retained.lock().unwrap();
            if retained.size() == (cols, rows) {
                return;
            }
            retained.resize(cols, rows);
            let _ = session.size_tx.send((cols, rows));
        }
    }

    /// The size of the relay's mirror model, which a remote worker is asked
    /// to give its PTY before snapshotting so the snapshot parses into the
    /// mirror unchanged.
    pub fn relay_size(&self, terminal_id: u64) -> (u16, u16) {
        self.session(terminal_id).retained.lock().unwrap().size()
    }

    /// The PTY's current size plus a receiver for later size changes.
    /// Subscribing before reading the size means no change is missed: a
    /// resize landing in between is read now and delivered again.
    /// Notices that the terminal's history was rewritten.
    pub fn viewer_rewrite_feed(&self, terminal_id: u64) -> broadcast::Receiver<()> {
        self.session(terminal_id).rewrite_tx.subscribe()
    }

    pub fn viewer_size_feed(
        &self,
        terminal_id: u64,
    ) -> ((u16, u16), broadcast::Receiver<(u16, u16)>) {
        let session = self.session(terminal_id);
        let rx = session.size_tx.subscribe();
        let size = match &self.transport {
            WorkerTransport::Local(mux) => mux.current_size(terminal_id).ok(),
            WorkerTransport::Remote(_) => None,
        };
        let size = size.unwrap_or_else(|| session.retained.lock().unwrap().size());
        (size, rx)
    }

    fn add_streaming_replay_waiter(relay: &mut RelayState) -> mpsc::UnboundedReceiver<ReplayChunk> {
        let (tx, replay) = mpsc::unbounded_channel();
        if relay.replay_started {
            let _ = tx.send(ReplayChunk {
                flags: pm_protocol::terminal_frame::FLAG_REPLAY
                    | pm_protocol::terminal_frame::FLAG_REPLAY_START,
                data: Bytes::copy_from_slice(&relay.replay),
            });
        }
        relay.replay_stream_waiters.push(tx);
        replay
    }

    /// Drops a viewer. Returns the current detach epoch when the last
    /// viewer left, so the caller can schedule a debounced detach that a
    /// returning viewer cancels by bumping the epoch.
    pub fn viewer_detach(&self, session_id: u64) -> Option<u64> {
        let session = self.session(session_id);
        let mut relay = session.relay.lock().unwrap();
        relay.viewers = relay.viewers.saturating_sub(1);
        if relay.viewers == 0 {
            if relay.upstream.is_none() {
                relay.replay_pending = false;
                relay.replay_started = false;
                relay.replay.clear();
                relay.replay_complete_waiters.clear();
                relay.replay_stream_waiters.clear();
            }
            relay.detach_epoch += 1;
            Some(relay.detach_epoch)
        } else {
            None
        }
    }

    /// Whether a debounced detach should still fire: no viewer returned
    /// (same epoch) and none is attached.
    pub fn detach_is_current(&self, session_id: u64, epoch: u64) -> bool {
        let session = self.session(session_id);
        let relay = session.relay.lock().unwrap();
        relay.viewers == 0 && relay.detach_epoch == epoch
    }

    /// The relayed scrollback for a session, used when its PTY owner sends
    /// lifecycle state separately from terminal output.
    pub fn scrollback(&self, session_id: u64) -> Bytes {
        self.sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|s| s.retained.lock().unwrap().ring.snapshot())
            .unwrap_or_default()
    }

    pub fn remove_session(&self, session_id: u64) {
        self.sessions.lock().unwrap().remove(&session_id);
    }
}

const TERMINAL_STREAM_TOKEN_TTL: Duration = Duration::from_secs(10);
const MAX_TERMINAL_STREAMS_PER_WORKER: usize = 256;

struct PendingTerminalStream {
    link: Weak<WorkerLink>,
    worker_id: u64,
    terminal_id: u64,
    generation: u64,
    expires_at: Instant,
}

#[derive(Default)]
pub struct TerminalStreamPool {
    pending: Mutex<HashMap<String, PendingTerminalStream>>,
    active: Mutex<HashMap<u64, usize>>,
}

pub struct TerminalStreamClaim {
    pub link: Arc<WorkerLink>,
    pub terminal_id: u64,
    pub generation: u64,
    pool: Arc<TerminalStreamPool>,
}

impl TerminalStreamPool {
    pub fn issue(
        &self,
        token_hash: String,
        link: &Arc<WorkerLink>,
        terminal_id: u64,
        generation: u64,
    ) -> bool {
        let now = Instant::now();
        let mut pending = self.pending.lock().unwrap();
        pending.retain(|_, item| item.expires_at > now);
        let active = self
            .active
            .lock()
            .unwrap()
            .get(&link.worker_id)
            .copied()
            .unwrap_or(0);
        let waiting = pending
            .values()
            .filter(|item| item.worker_id == link.worker_id)
            .count();
        if active + waiting >= MAX_TERMINAL_STREAMS_PER_WORKER {
            return false;
        }
        pending.insert(
            token_hash,
            PendingTerminalStream {
                link: Arc::downgrade(link),
                worker_id: link.worker_id,
                terminal_id,
                generation,
                expires_at: now + TERMINAL_STREAM_TOKEN_TTL,
            },
        );
        true
    }

    pub fn revoke(&self, token_hash: &str) {
        self.pending.lock().unwrap().remove(token_hash);
    }

    pub fn claim(
        self: &Arc<Self>,
        token_hash: &str,
        registry: &WorkerRegistry,
    ) -> Option<TerminalStreamClaim> {
        let pending = self.pending.lock().unwrap().remove(token_hash)?;
        if pending.expires_at <= Instant::now() {
            return None;
        }
        let link = pending.link.upgrade()?;
        let current = registry.get(pending.worker_id)?;
        if !Arc::ptr_eq(&link, &current) {
            return None;
        }
        let mut active = self.active.lock().unwrap();
        let count = active.entry(pending.worker_id).or_default();
        if *count >= MAX_TERMINAL_STREAMS_PER_WORKER {
            return None;
        }
        *count += 1;
        Some(TerminalStreamClaim {
            link,
            terminal_id: pending.terminal_id,
            generation: pending.generation,
            pool: self.clone(),
        })
    }
}

impl Drop for TerminalStreamClaim {
    fn drop(&mut self) {
        let mut active = self.pool.active.lock().unwrap();
        let Some(count) = active.get_mut(&self.link.worker_id) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            active.remove(&self.link.worker_id);
        }
    }
}

const TRANSCRIPT_TOKEN_TTL: Duration = Duration::from_secs(10);
const MAX_TRANSCRIPT_TRANSFERS: usize = 8;

struct QueuedTranscript {
    link: Weak<WorkerLink>,
    worker_id: u64,
    transcript: WorkerTranscript,
}

struct PendingTranscript {
    transfer: QueuedTranscript,
    expires_at: Instant,
}

#[derive(Default)]
struct TranscriptTransferState {
    queued: VecDeque<QueuedTranscript>,
    pending: HashMap<String, PendingTranscript>,
    transfers: usize,
}

#[derive(Default)]
pub struct TranscriptTransferPool {
    state: Mutex<TranscriptTransferState>,
}

pub struct TranscriptTransferClaim {
    pub link: Arc<WorkerLink>,
    pub transcript: WorkerTranscript,
    pool: Arc<TranscriptTransferPool>,
    finished: bool,
}

impl TranscriptTransferPool {
    pub fn enqueue(self: &Arc<Self>, link: &Arc<WorkerLink>, transcripts: Vec<WorkerTranscript>) {
        let mut state = self.state.lock().unwrap();
        state.queued.retain(|item| item.worker_id != link.worker_id);
        let removed = state
            .pending
            .values()
            .filter(|item| item.transfer.worker_id == link.worker_id)
            .count();
        state
            .pending
            .retain(|_, item| item.transfer.worker_id != link.worker_id);
        state.transfers = state.transfers.saturating_sub(removed);
        state
            .queued
            .extend(transcripts.into_iter().map(|transcript| QueuedTranscript {
                link: Arc::downgrade(link),
                worker_id: link.worker_id,
                transcript,
            }));
        drop(state);
        self.schedule();
    }

    pub fn claim(
        self: &Arc<Self>,
        token_hash: &str,
        registry: &WorkerRegistry,
    ) -> Option<TranscriptTransferClaim> {
        let pending = self.state.lock().unwrap().pending.remove(token_hash)?;
        let transfer = pending.transfer;
        if pending.expires_at <= Instant::now() {
            self.release(transfer, true);
            return None;
        }
        let Some(link) = transfer.link.upgrade() else {
            self.release(transfer, false);
            return None;
        };
        let Some(current) = registry.get(transfer.worker_id) else {
            self.release(transfer, false);
            return None;
        };
        if !Arc::ptr_eq(&link, &current) {
            self.release(transfer, false);
            return None;
        }
        Some(TranscriptTransferClaim {
            link,
            transcript: transfer.transcript,
            pool: self.clone(),
            finished: false,
        })
    }

    fn schedule(self: &Arc<Self>) {
        loop {
            let next = {
                let mut state = self.state.lock().unwrap();
                if state.transfers >= MAX_TRANSCRIPT_TRANSFERS {
                    return;
                }
                let Some(transfer) = state.queued.pop_front() else {
                    return;
                };
                let Some(link) = transfer.link.upgrade() else {
                    continue;
                };
                let token = crate::auth::generate_token();
                let token_hash = crate::auth::hash_token(&token);
                let command = ControllerMsg::Transcript {
                    terminal_id: transfer.transcript.terminal_id,
                    generation: transfer.transcript.generation,
                    token,
                };
                state.pending.insert(
                    token_hash.clone(),
                    PendingTranscript {
                        transfer,
                        expires_at: Instant::now() + TRANSCRIPT_TOKEN_TTL,
                    },
                );
                state.transfers += 1;
                (link, token_hash, command)
            };
            if next.0.send(next.2).is_err() {
                let mut state = self.state.lock().unwrap();
                if state.pending.remove(&next.1).is_some() {
                    state.transfers = state.transfers.saturating_sub(1);
                }
                continue;
            }
            let pool = self.clone();
            let token_hash = next.1;
            tokio::spawn(async move {
                tokio::time::sleep(TRANSCRIPT_TOKEN_TTL).await;
                let expired = pool.state.lock().unwrap().pending.remove(&token_hash);
                if let Some(expired) = expired {
                    pool.release(expired.transfer, true);
                }
            });
        }
    }

    fn release(self: &Arc<Self>, transfer: QueuedTranscript, retry: bool) {
        let mut state = self.state.lock().unwrap();
        state.transfers = state.transfers.saturating_sub(1);
        if retry {
            state.queued.push_back(transfer);
        }
        drop(state);
        self.schedule();
    }
}

impl TranscriptTransferClaim {
    pub fn complete(mut self) {
        self.finished = true;
        let transfer = QueuedTranscript {
            link: Arc::downgrade(&self.link),
            worker_id: self.link.worker_id,
            transcript: self.transcript.clone(),
        };
        self.pool.release(transfer, false);
    }
}

impl Drop for TranscriptTransferClaim {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let transfer = QueuedTranscript {
            link: Arc::downgrade(&self.link),
            worker_id: self.link.worker_id,
            transcript: self.transcript.clone(),
        };
        self.pool.release(transfer, true);
    }
}

/// The set of currently connected remote workers.
#[derive(Default)]
pub struct WorkerRegistry {
    links: Mutex<HashMap<u64, Arc<WorkerLink>>>,
}

impl WorkerRegistry {
    /// Registers a freshly connected worker and returns its link. The
    /// caller drives the returned receiver to write control messages down
    /// the connection.
    pub fn connect(&self, worker_id: u64) -> (Arc<WorkerLink>, mpsc::Receiver<ControllerMsg>) {
        let (to_worker, rx) = mpsc::channel(OUTGOING_CONTROL_CAPACITY);
        let link = Arc::new(WorkerLink {
            worker_id,
            transport: WorkerTransport::Remote(to_worker),
            dial: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            fs_seq: AtomicU64::new(0),
            protocol_version: AtomicU64::new(0),
            repo_seq: AtomicU64::new(0),
            repo_pending: Mutex::new(HashMap::new()),
            fs_pending: Mutex::new(HashMap::new()),
            harness_seq: AtomicU64::new(0),
            harness_pending: Mutex::new(HashMap::new()),
            path_check_seq: AtomicU64::new(0),
            path_check_pending: Mutex::new(HashMap::new()),
            agent_inbox_seq: AtomicU64::new(0),
            agent_inbox_pending: Mutex::new(HashMap::new()),
            file_seq: AtomicU64::new(0),
            file_pending: Mutex::new(HashMap::new()),
            forward_seq: AtomicU64::new(0),
            forward_pending: Mutex::new(HashMap::new()),
            dir_share_pending: Mutex::new(HashMap::new()),
            superseded: AtomicBool::new(false),
            closed: tokio::sync::Notify::new(),
        });
        // One connection per worker. A worker that reconnects while its
        // previous socket is still open would otherwise have both live:
        // the registry routes outbound down the newer link while the
        // older one goes on applying inbound messages under the same
        // worker id. Retire the old one here, before the new link is
        // handed to the caller, so only one is ever relaying.
        let previous = self.links.lock().unwrap().insert(worker_id, link.clone());
        if let Some(previous) = previous {
            previous.supersede();
        }
        (link, rx)
    }

    /// Removes a worker's link if the given one is still the active
    /// connection, so a stale disconnect cannot evict a newer session.
    pub fn disconnect(&self, link: &Arc<WorkerLink>) {
        let mut links = self.links.lock().unwrap();
        if let Some(current) = links.get(&link.worker_id) {
            if Arc::ptr_eq(current, link) {
                links.remove(&link.worker_id);
            }
        }
    }

    pub fn get(&self, worker_id: u64) -> Option<Arc<WorkerLink>> {
        self.links.lock().unwrap().get(&worker_id).cloned()
    }

    pub fn is_online(&self, worker_id: u64) -> bool {
        self.links.lock().unwrap().contains_key(&worker_id)
    }

    pub fn online_ids(&self) -> Vec<u64> {
        self.links.lock().unwrap().keys().copied().collect()
    }
}

/// Control messages buffered per worker connection before it is treated
/// as wedged.
const OUTGOING_CONTROL_CAPACITY: usize = 1024;

/// How long a session keeps relaying after its last viewer leaves, so a
/// quick re-open does not restart the stream.
const DETACH_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(3);

/// Held for a viewer's attach lifetime. On drop it releases the viewer
/// and, if it was the last, schedules a debounced detach so the worker
/// stops relaying an unwatched session. Dropping (including task abort on
/// disconnect) is what pauses the stream.
pub struct ViewerGuard {
    link: Arc<WorkerLink>,
    session_id: u64,
    generation: u64,
}

impl ViewerGuard {
    pub fn new(link: Arc<WorkerLink>, session_id: u64, generation: u64) -> Self {
        ViewerGuard {
            link,
            session_id,
            generation,
        }
    }
}

impl Drop for ViewerGuard {
    fn drop(&mut self) {
        let Some(epoch) = self.link.viewer_detach(self.session_id) else {
            return;
        };
        let link = self.link.clone();
        let session_id = self.session_id;
        let generation = self.generation;
        // A failure to schedule (no runtime) simply leaves the session
        // relaying, which is safe.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(DETACH_DEBOUNCE).await;
                if link.detach_is_current(session_id, epoch) {
                    let _ = link.send(ControllerMsg::TerminalDetach {
                        terminal_id: session_id,
                        generation,
                    });
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When the mirror drops lines a repaint printed again, every viewer's
    /// snapshot is stale, including one sent before the repaint arrived, so
    /// the relay tells all of them.
    #[test]
    fn a_repaint_that_drops_spilled_lines_tells_every_viewer() {
        let link = link();
        let mut rewrites = link.viewer_rewrite_feed(1);
        link.mirror_terminal_resize(1, 140, 10);
        link.feed_terminal_output(1, 0, 0, Bytes::from_static(b"HEADER\r\n"));
        for _ in 0..6 {
            link.feed_terminal_output(1, 0, 0, Bytes::from(vec![b'-'; 138]));
            link.feed_terminal_output(1, 0, 0, Bytes::from_static(b"\r\n"));
        }
        link.mirror_terminal_resize(1, 90, 10);
        assert!(rewrites.try_recv().is_err());
        link.feed_terminal_output(1, 0, 0, Bytes::from_static(b"\x1b[HHEADER\r\n"));
        assert!(
            rewrites.try_recv().is_ok(),
            "the dropped spill reached no viewer"
        );
    }

    /// A viewer's in-band snapshot and its new subscription are taken under
    /// the lock live output is sent under, so the output already in the
    /// snapshot never reaches the viewer again and nothing after it is lost.
    /// The viewer's progress moves to what has been fed, or the chunks its
    /// old subscription dropped would hold the relay forever.
    #[tokio::test]
    async fn a_resnapshot_swaps_the_subscription_without_gaps_or_repeats() {
        let link = link();
        let progress = link.viewer_progress(1);
        let mut old = link.session(1).out_tx.subscribe();
        link.feed_terminal_output(1, 0, 0, Bytes::from_static(b"before "));
        let (snapshot, mut fresh, _) = link.resnapshot_viewer(1, &progress).unwrap();
        link.feed_terminal_output(1, 0, 0, Bytes::from_static(b"after"));
        assert!(String::from_utf8_lossy(&snapshot).contains("before"));
        assert_eq!(fresh.try_recv().unwrap(), Bytes::from_static(b"after"));
        assert!(fresh.try_recv().is_err());
        assert_eq!(old.try_recv().unwrap(), Bytes::from_static(b"before "));
        assert_eq!(
            progress.sent.load(std::sync::atomic::Ordering::Relaxed),
            "before ".len() as u64
        );
    }

    /// A viewer that stops crediting, such as a hidden browser tab whose
    /// timers are throttled, must not pace the relay for long: past the
    /// stall limit it stops gating, and its next credit makes it gate again.
    #[tokio::test]
    async fn a_viewer_that_stops_crediting_stops_gating_the_relay() {
        let link = link();
        let stalled = link.viewer_progress(1);
        let chunk = Bytes::from(vec![b'x'; VIEWER_BACKLOG_BUDGET_BYTES as usize + 1]);
        link.feed_terminal_output(1, 0, 0, chunk.clone());

        let waited = std::time::Instant::now();
        tokio::time::timeout(VIEWER_STALL_LIMIT * 4, link.wait_for_viewers(1))
            .await
            .expect("a stalled viewer must not hold the relay past the stall limit");
        assert!(waited.elapsed() >= VIEWER_STALL_LIMIT);

        link.feed_terminal_output(1, 0, 0, chunk.clone());
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            link.wait_for_viewers(1),
        )
        .await
        .expect("once it has stopped gating, the stalled viewer costs nothing");

        stalled.acked(1);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                link.wait_for_viewers(1)
            )
            .await
            .is_err(),
            "a viewer that credits again paces the relay again"
        );
    }

    fn link() -> WorkerLink {
        let (to_worker, _rx) = mpsc::channel(8);
        WorkerLink {
            dial: Mutex::new(None),
            worker_id: 1,
            transport: WorkerTransport::Remote(to_worker),
            sessions: Mutex::new(HashMap::new()),
            fs_seq: AtomicU64::new(0),
            protocol_version: AtomicU64::new(0),
            repo_seq: AtomicU64::new(0),
            repo_pending: Mutex::new(HashMap::new()),
            fs_pending: Mutex::new(HashMap::new()),
            harness_seq: AtomicU64::new(0),
            harness_pending: Mutex::new(HashMap::new()),
            path_check_seq: AtomicU64::new(0),
            path_check_pending: Mutex::new(HashMap::new()),
            agent_inbox_seq: AtomicU64::new(0),
            agent_inbox_pending: Mutex::new(HashMap::new()),
            file_seq: AtomicU64::new(0),
            file_pending: Mutex::new(HashMap::new()),
            forward_seq: AtomicU64::new(0),
            forward_pending: Mutex::new(HashMap::new()),
            dir_share_pending: Mutex::new(HashMap::new()),
            superseded: AtomicBool::new(false),
            closed: tokio::sync::Notify::new(),
        }
    }

    #[test]
    fn a_snapshot_replay_seeds_the_mirror_and_joined_viewers_get_state() {
        let link = link();
        let StreamingViewerAttach::First { .. } = link.streaming_viewer_attach(7) else {
            panic!("first viewer starts the stream");
        };
        let (upstream, _input) = mpsc::channel(1);
        link.connect_terminal_stream(7, 1, upstream);
        let snapshot_flags = pm_protocol::terminal_frame::FLAG_REPLAY
            | pm_protocol::terminal_frame::FLAG_REPLAY_START
            | pm_protocol::terminal_frame::FLAG_REPLAY_END
            | pm_protocol::terminal_frame::FLAG_REPLAY_SNAPSHOT;
        link.feed_terminal_output(
            7,
            1,
            snapshot_flags,
            Bytes::from_static(b"\x1b[?1049h\x1b[?1003h\x1b[3;5Hboxed"),
        );
        link.feed_terminal_output(7, 1, 0, Bytes::from_static(b" live"));

        let StreamingViewerAttach::Joined { replay, .. } = link.streaming_viewer_attach(7) else {
            panic!("second viewer joins the running stream");
        };
        let mut restored = crate::term_model::TerminalModel::new(
            crate::mux::DEFAULT_COLS,
            crate::mux::DEFAULT_ROWS,
        );
        restored.advance(&replay);
        assert!(restored
            .mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN));
        assert!(restored
            .mode()
            .contains(alacritty_terminal::term::TermMode::MOUSE_MOTION));
        let screen = restored.visible_text().join("\n");
        assert!(screen.contains("boxed live"), "screen was: {screen}");
    }

    /// An upstream that replays raw bytes instead of a snapshot still only
    /// reaches a joiner through the mirror. Handing the joiner those bytes
    /// is what put half-parsed escape sequences on screen.
    #[test]
    fn a_raw_upstream_replay_still_reaches_joiners_as_state() {
        let link = link();
        let StreamingViewerAttach::First { .. } = link.streaming_viewer_attach(7) else {
            panic!("first viewer starts the stream");
        };
        let (upstream, _input) = mpsc::channel(1);
        link.connect_terminal_stream(7, 1, upstream);
        link.feed_terminal_output(
            7,
            1,
            pm_protocol::terminal_frame::FLAG_REPLAY
                | pm_protocol::terminal_frame::FLAG_REPLAY_START
                | pm_protocol::terminal_frame::FLAG_REPLAY_END,
            Bytes::from_static(b"\x1b[2J\x1b[1;1Hlegacy tail"),
        );
        let StreamingViewerAttach::Joined { replay, .. } = link.streaming_viewer_attach(7) else {
            panic!("second viewer joins the running stream");
        };
        assert_ne!(
            replay,
            Bytes::from_static(b"\x1b[2J\x1b[1;1Hlegacy tail"),
            "a joiner is never handed the retained byte stream"
        );
        let mut restored = crate::term_model::TerminalModel::new(
            crate::mux::DEFAULT_COLS,
            crate::mux::DEFAULT_ROWS,
        );
        restored.advance(&replay);
        let screen = restored.visible_text().join("\n");
        assert!(screen.contains("legacy tail"), "screen was: {screen}");
    }

    /// A full-screen agent enters the alternate buffer once, at startup,
    /// and then writes for hours. Once its output passes the ring cap the
    /// retained bytes no longer contain that switch, so serving them
    /// replayed the transcript into the wrong buffer.
    #[test]
    fn a_joiner_keeps_screen_state_after_the_ring_has_wrapped() {
        let link = link();
        let StreamingViewerAttach::First { .. } = link.streaming_viewer_attach(7) else {
            panic!("first viewer starts the stream");
        };
        let (upstream, _input) = mpsc::channel(1);
        link.connect_terminal_stream(7, 1, upstream);
        link.feed_terminal_output(
            7,
            1,
            pm_protocol::terminal_frame::FLAG_REPLAY
                | pm_protocol::terminal_frame::FLAG_REPLAY_START
                | pm_protocol::terminal_frame::FLAG_REPLAY_END
                | pm_protocol::terminal_frame::FLAG_REPLAY_SNAPSHOT,
            Bytes::from_static(b"\x1b[?1049h\x1b[?1003h\x1b[2J"),
        );
        let filler = Bytes::from(vec![b'.'; 4096]);
        let mut written = 0usize;
        while written <= SCROLLBACK_CAP_BYTES {
            link.feed_terminal_output(7, 1, 0, filler.clone());
            written += filler.len();
        }
        link.feed_terminal_output(7, 1, 0, Bytes::from_static(b"\x1b[5;1Hlast frame"));

        let StreamingViewerAttach::Joined { replay, .. } = link.streaming_viewer_attach(7) else {
            panic!("second viewer joins the running stream");
        };
        let mut restored = crate::term_model::TerminalModel::new(
            crate::mux::DEFAULT_COLS,
            crate::mux::DEFAULT_ROWS,
        );
        restored.advance(&replay);
        assert!(
            restored
                .mode()
                .contains(alacritty_terminal::term::TermMode::ALT_SCREEN),
            "the alt-screen switch was pushed out of the ring but is still screen state"
        );
        assert!(
            restored
                .mode()
                .contains(alacritty_terminal::term::TermMode::MOUSE_MOTION),
            "mouse tracking is enabled once per process and must survive too"
        );
        let screen = restored.visible_text().join("\n");
        assert!(screen.contains("last frame"), "screen was: {screen}");
    }

    #[test]
    fn mirror_resize_shapes_later_snapshots() {
        let link = link();
        let StreamingViewerAttach::First { .. } = link.streaming_viewer_attach(7) else {
            panic!("first viewer starts the stream");
        };
        let (upstream, _input) = mpsc::channel(1);
        link.connect_terminal_stream(7, 1, upstream);
        link.mirror_terminal_resize(7, 60, 10);
        let snapshot_flags = pm_protocol::terminal_frame::FLAG_REPLAY
            | pm_protocol::terminal_frame::FLAG_REPLAY_START
            | pm_protocol::terminal_frame::FLAG_REPLAY_END
            | pm_protocol::terminal_frame::FLAG_REPLAY_SNAPSHOT;
        link.feed_terminal_output(7, 1, snapshot_flags, Bytes::from_static(b"sized"));
        let StreamingViewerAttach::Joined { replay, .. } = link.streaming_viewer_attach(7) else {
            panic!("second viewer joins the running stream");
        };
        let mut restored = crate::term_model::TerminalModel::new(60, 10);
        restored.advance(&replay);
        assert!(restored.visible_text().join("").contains("sized"));
    }

    #[test]
    fn mirror_resize_echoes_only_actual_size_changes() {
        let link = link();
        let StreamingViewerAttach::First { .. } = link.streaming_viewer_attach(7) else {
            panic!("first viewer starts the stream");
        };
        let (size, mut sizes) = link.viewer_size_feed(7);
        assert_eq!(size, (crate::mux::DEFAULT_COLS, crate::mux::DEFAULT_ROWS));
        link.mirror_terminal_resize(7, 60, 10);
        assert_eq!(sizes.try_recv().unwrap(), (60, 10));
        link.mirror_terminal_resize(7, 60, 10);
        assert!(sizes.try_recv().is_err(), "an unchanged size is not echoed");
        link.mirror_terminal_resize(7, 61, 10);
        assert_eq!(sizes.try_recv().unwrap(), (61, 10));
        let (size, _) = link.viewer_size_feed(7);
        assert_eq!(size, (61, 10));
    }

    #[test]
    fn only_the_last_viewer_leaving_arms_a_detach() {
        let link = link();
        assert!(matches!(link.viewer_attach(7), ViewerAttach::First { .. }));
        assert!(matches!(
            link.viewer_attach(7),
            ViewerAttach::Waiting { .. }
        ));
        // First of two viewers leaves: still watched, no detach.
        assert_eq!(link.viewer_detach(7), None);
        // Last leaves: a detach epoch is returned.
        let epoch = link.viewer_detach(7).expect("last viewer arms a detach");
        assert!(link.detach_is_current(7, epoch));
    }

    #[test]
    fn a_returning_viewer_cancels_a_pending_detach() {
        let link = link();
        let ViewerAttach::First { mut replay, .. } = link.viewer_attach(7) else {
            panic!("first viewer");
        };
        let (upstream, _input) = mpsc::channel(1);
        link.connect_terminal_stream(7, 1, upstream);
        link.feed_terminal_output(
            7,
            1,
            pm_protocol::terminal_frame::FLAG_REPLAY
                | pm_protocol::terminal_frame::FLAG_REPLAY_START
                | pm_protocol::terminal_frame::FLAG_REPLAY_END,
            Bytes::from_static(b"warm"),
        );
        assert_eq!(replay.try_recv().unwrap(), Bytes::from_static(b"warm"));
        let epoch = link.viewer_detach(7).unwrap();
        assert!(matches!(link.viewer_attach(7), ViewerAttach::Joined { .. }));
        assert!(
            !link.detach_is_current(7, epoch),
            "the pending detach is stale once a viewer returns"
        );
    }

    #[test]
    fn a_replay_frame_resets_the_ring_and_wakes_the_waiter() {
        let link = link();
        let ViewerAttach::First { mut replay, .. } = link.viewer_attach(7) else {
            panic!("first viewer");
        };
        link.feed_terminal_output(7, 1, 0, Bytes::from_static(b"stale"));
        link.feed_terminal_output(
            7,
            1,
            pm_protocol::terminal_frame::FLAG_REPLAY
                | pm_protocol::terminal_frame::FLAG_REPLAY_START
                | pm_protocol::terminal_frame::FLAG_REPLAY_END,
            Bytes::from_static(b"fresh"),
        );
        assert_eq!(&replay.try_recv().unwrap()[..], b"fresh");
    }

    #[test]
    fn stale_generation_output_is_ignored() {
        let registry = WorkerRegistry::default();
        let (link, _) = registry.connect(2);
        link.feed_terminal_output(
            7,
            2,
            pm_protocol::terminal_frame::FLAG_REPLAY
                | pm_protocol::terminal_frame::FLAG_REPLAY_START
                | pm_protocol::terminal_frame::FLAG_REPLAY_END,
            Bytes::from_static(b"new"),
        );
        link.feed_terminal_output(7, 1, 0, Bytes::from_static(b"stale"));
        assert_eq!(link.scrollback(7), Bytes::from_static(b"new"));
    }

    #[test]
    fn replay_waits_for_the_final_chunk() {
        let link = link();
        let ViewerAttach::First { mut replay, .. } = link.viewer_attach(7) else {
            panic!("first viewer");
        };
        let ViewerAttach::Waiting {
            replay: mut joined, ..
        } = link.viewer_attach(7)
        else {
            panic!("joining viewer");
        };
        link.feed_terminal_output(
            7,
            1,
            pm_protocol::terminal_frame::FLAG_REPLAY
                | pm_protocol::terminal_frame::FLAG_REPLAY_START,
            Bytes::from_static(b"first"),
        );
        assert!(matches!(
            replay.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        link.feed_terminal_output(
            7,
            1,
            pm_protocol::terminal_frame::FLAG_REPLAY | pm_protocol::terminal_frame::FLAG_REPLAY_END,
            Bytes::from_static(b"second"),
        );
        assert_eq!(&replay.try_recv().unwrap()[..], b"firstsecond");
        assert_eq!(&joined.try_recv().unwrap()[..], b"firstsecond");
    }

    #[test]
    fn streaming_replay_delivers_each_chunk_before_completion() {
        let link = link();
        let StreamingViewerAttach::First {
            replay: mut first, ..
        } = link.streaming_viewer_attach(7)
        else {
            panic!("first viewer");
        };
        let start = pm_protocol::terminal_frame::FLAG_REPLAY
            | pm_protocol::terminal_frame::FLAG_REPLAY_START;
        link.feed_terminal_output(7, 1, start, Bytes::from_static(b"first"));
        let first_chunk = first.try_recv().unwrap();
        assert_eq!(first_chunk.flags, start);
        assert_eq!(first_chunk.data, Bytes::from_static(b"first"));
        let StreamingViewerAttach::Waiting {
            replay: mut joined, ..
        } = link.streaming_viewer_attach(7)
        else {
            panic!("joining viewer");
        };
        let joined_start = joined.try_recv().unwrap();
        assert_eq!(joined_start.flags, start);
        assert_eq!(joined_start.data, Bytes::from_static(b"first"));

        let end =
            pm_protocol::terminal_frame::FLAG_REPLAY | pm_protocol::terminal_frame::FLAG_REPLAY_END;
        link.feed_terminal_output(7, 1, end, Bytes::from_static(b"second"));
        let first_end = first.try_recv().unwrap();
        assert_eq!(first_end.flags, end);
        assert_eq!(first_end.data, Bytes::from_static(b"second"));
        let joined_end = joined.try_recv().unwrap();
        assert_eq!(joined_end.flags, end);
        assert_eq!(joined_end.data, Bytes::from_static(b"second"));
    }

    #[test]
    fn terminal_stream_tokens_are_single_use_and_bound_to_the_link() {
        let registry = WorkerRegistry::default();
        let (link, _) = registry.connect(2);
        let pool = Arc::new(TerminalStreamPool::default());
        assert!(pool.issue("one".into(), &link, 7, 3));
        let claim = pool.claim("one", &registry).unwrap();
        assert_eq!((claim.terminal_id, claim.generation), (7, 3));
        assert!(pool.claim("one", &registry).is_none());

        assert!(pool.issue("stale".into(), &link, 8, 4));
        let _ = registry.connect(2);
        assert!(pool.claim("stale", &registry).is_none());
    }

    #[test]
    fn terminal_streams_are_capped_per_worker() {
        let registry = WorkerRegistry::default();
        let (link, _) = registry.connect(2);
        let pool = TerminalStreamPool::default();
        for index in 0..MAX_TERMINAL_STREAMS_PER_WORKER {
            assert!(pool.issue(index.to_string(), &link, index as u64, 1));
        }
        assert!(!pool.issue("over-cap".into(), &link, 999, 1));
    }

    #[test]
    fn terminal_input_uses_only_its_terminal_stream() {
        let link = link();
        let (first_tx, mut first_rx) = mpsc::channel(2);
        let (second_tx, mut second_rx) = mpsc::channel(2);
        link.connect_terminal_stream(7, 1, first_tx);
        link.connect_terminal_stream(8, 2, second_tx);
        link.terminal_input(8, 2, Bytes::from_static(b"target"))
            .unwrap();
        assert!(first_rx.try_recv().is_err());
        let frame = second_rx.try_recv().unwrap();
        assert!(matches!(
            pm_protocol::terminal_frame::decode(&frame),
            Some(pm_protocol::terminal_frame::TerminalFrame::Input {
                generation: 2,
                submitted: false,
                data: b"target",
            })
        ));
    }

    #[test]
    fn a_busy_terminal_input_stream_stays_connected() {
        let link = link();
        let (input_tx, mut input_rx) = mpsc::channel(1);
        link.connect_terminal_stream(7, 1, input_tx);
        link.terminal_input(7, 1, Bytes::from_static(b"first"))
            .unwrap();
        assert!(matches!(
            link.terminal_input(7, 1, Bytes::from_static(b"blocked")),
            Err(WorkerError::Busy)
        ));
        input_rx.try_recv().unwrap();
        link.terminal_input(7, 1, Bytes::from_static(b"accepted"))
            .unwrap();
    }

    #[tokio::test]
    async fn transcript_uploads_are_limited_and_the_queue_advances() {
        let registry = WorkerRegistry::default();
        let (link, mut rx) = registry.connect(2);
        let pool = Arc::new(TranscriptTransferPool::default());
        pool.enqueue(
            &link,
            (0..9)
                .map(|terminal_id| WorkerTranscript {
                    terminal_id,
                    generation: 1,
                    size: 1,
                })
                .collect(),
        );
        let mut commands = Vec::new();
        while let Ok(command) = rx.try_recv() {
            commands.push(command);
        }
        assert_eq!(commands.len(), MAX_TRANSCRIPT_TRANSFERS);
        let token = match commands.remove(0) {
            ControllerMsg::Transcript { token, .. } => token,
            command => panic!("unexpected command {command:?}"),
        };
        pool.claim(&crate::auth::hash_token(&token), &registry)
            .unwrap()
            .complete();
        assert!(matches!(
            rx.recv().await,
            Some(ControllerMsg::Transcript { .. })
        ));
    }

    #[tokio::test]
    async fn a_second_connection_retires_the_first() {
        let registry = WorkerRegistry::default();
        let (first, _first_rx) = registry.connect(7);
        assert!(!first.is_superseded());

        let (second, _second_rx) = registry.connect(7);

        // The newer link is the one anything routing to worker 7 gets,
        // and the older one knows it has been replaced.
        assert!(first.is_superseded());
        assert!(!second.is_superseded());
        assert!(Arc::ptr_eq(
            &registry.get(7).expect("worker 7 online"),
            &second
        ));
        // One worker, one connection: the map never holds both.
        assert_eq!(registry.online_ids(), vec![7]);
    }

    #[tokio::test]
    async fn a_retired_link_stops_waiting_immediately() {
        let registry = WorkerRegistry::default();
        let (first, _first_rx) = registry.connect(7);

        // Nothing has replaced it, so a relay on this link keeps waiting.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), first.superseded())
                .await
                .is_err()
        );

        let (_second, _second_rx) = registry.connect(7);

        // Already superseded before the wait begins, which is the race
        // the flag is read for: this must return rather than hang.
        tokio::time::timeout(std::time::Duration::from_secs(1), first.superseded())
            .await
            .expect("a superseded link stops relaying");
    }

    #[tokio::test]
    async fn a_retired_link_disconnecting_does_not_evict_its_replacement() {
        let registry = WorkerRegistry::default();
        let (first, _first_rx) = registry.connect(7);
        let (second, _second_rx) = registry.connect(7);

        // The old socket noticing later must not take the live one down.
        registry.disconnect(&first);

        assert!(registry.is_online(7));
        assert!(Arc::ptr_eq(
            &registry.get(7).expect("worker 7 online"),
            &second
        ));
    }

    #[tokio::test]
    async fn a_worker_that_never_reconnected_disconnects_normally() {
        let registry = WorkerRegistry::default();
        let (only, _rx) = registry.connect(7);

        registry.disconnect(&only);

        assert!(!registry.is_online(7));
        assert!(registry.online_ids().is_empty());
    }
}
