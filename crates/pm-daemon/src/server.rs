//! Unix socket transport: one length-prefixed protobuf frame per
//! message, one connection per client. Message semantics live in
//! `connection`, shared with the WebSocket transport.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use pm_protocol::domain::{ClientEnvelope, ServerMsg};
use pm_protocol::frame;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::connection::{handle_message, ConnState, OUTGOING_CHANNEL_CAPACITY};
use crate::daemon::Daemon;

const SOCKET_DIR_MODE: u32 = 0o700;

/// Terminal input/output is hot-path data. Persist its recency in one
/// batch on this cadence instead of writing SQLite for every chunk.
const ACTIVITY_CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// How often alerts held for a Program Status session are checked for
/// having settled, which bounds how late past the debounce one is raised.
const PROGRAM_STATUS_ALERT_FLUSH_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(250);

/// Poll floor for queued push deliveries; the enqueue wakeup makes
/// fresh events deliver immediately, this only paces retries.
const PUSH_DELIVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Poll floor for supervision wakes. Lifecycle changes notify this pass
/// directly; the interval reconciles a supervisor that was mid-turn when
/// its child parked, since that leaves no later transition to observe.
const SUPERVISOR_WAKE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);
/// How often turns are checked for a missing end. Two passes are required
/// before one is inferred, so this also sets the confirmation gap.
const STALE_TURN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

pub struct ServerHandle {
    daemon: Arc<Daemon>,
    pub socket_path: std::path::PathBuf,
    pub http_addr: Option<std::net::SocketAddr>,
    pub worker_addr: Option<std::net::SocketAddr>,
    listener_task: JoinHandle<()>,
    exit_task: JoinHandle<()>,
    needs_input_task: JoinHandle<()>,
    program_status_task: JoinHandle<()>,
    program_status_alert_task: JoinHandle<()>,
    activity_task: JoinHandle<()>,
    activity_checkpoint_task: JoinHandle<()>,
    push_delivery_task: JoinHandle<()>,
    supervisor_wake_task: JoinHandle<()>,
    stale_turn_task: JoinHandle<()>,
    http_task: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub async fn shutdown(self) {
        let mut tasks = vec![
            self.listener_task,
            self.exit_task,
            self.needs_input_task,
            self.program_status_task,
            self.program_status_alert_task,
            self.activity_task,
            self.activity_checkpoint_task,
            self.push_delivery_task,
            self.supervisor_wake_task,
            self.stale_turn_task,
        ];
        if let Some(t) = self.http_task {
            tasks.push(t);
        }
        for t in &tasks {
            t.abort();
        }

        self.daemon.checkpoint_session_activity();
        let mut forward_tasks = self.daemon.forwards.drain_listeners();

        tasks.append(&mut forward_tasks);
        for t in tasks {
            let _ = t.await;
        }

        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Readies the socket path for binding. Creates the parent directory,
/// and when a socket file is already present, probes it: a daemon that
/// answers means we would hijack a live socket, so refuse rather than
/// silently steal it; a refused connection means the file is stale and
/// safe to replace.
async fn prepare_socket(socket_path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(dir) = socket_path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(SOCKET_DIR_MODE))?;
    }
    if socket_path.exists() {
        match UnixStream::connect(socket_path).await {
            Ok(_) => anyhow::bail!(
                "another puppet-master daemon is already listening on {}; \
                 stop it first or pass a different --socket",
                socket_path.display()
            ),
            Err(e) => {
                warn!(
                    path = %socket_path.display(),
                    error = %e,
                    "replacing stale socket file left by a previous daemon"
                );
                std::fs::remove_file(socket_path)?;
            }
        }
    }
    Ok(())
}

pub async fn start(
    daemon: Arc<Daemon>,
    socket_path: std::path::PathBuf,
    http_addr: Option<std::net::SocketAddr>,
    http_tls: Option<crate::daemon::HttpTls>,
    worker_addr: Option<std::net::SocketAddr>,
    channels: crate::mux::MuxChannels,
) -> anyhow::Result<ServerHandle> {
    prepare_socket(&socket_path).await?;
    let listener = UnixListener::bind(&socket_path)?;
    info!(path = %socket_path.display(), "listening on unix socket");
    daemon.seal_stored_credential_settings();
    daemon.log_push_configuration();

    let mut exit_rx = channels.exit_rx;
    let mut needs_input_rx = channels.needs_input_rx;
    let mut activity_rx = channels.activity_rx;
    let mut program_status_rx = channels.program_status_rx;

    let exit_daemon = daemon.clone();
    let exit_task = tokio::spawn(async move {
        while let Some(exit) = exit_rx.recv().await {
            let d = exit_daemon.clone();
            tokio::task::spawn_blocking(move || d.handle_session_exit(exit))
                .await
                .ok();
        }
    });

    let needs_input_daemon = daemon.clone();
    let needs_input_task = tokio::spawn(async move {
        while let Some(signal) = needs_input_rx.recv().await {
            needs_input_daemon.handle_pty_needs_input(signal.session_id);
        }
    });

    let program_status_daemon = daemon.clone();
    let program_status_task = tokio::spawn(async move {
        while let Some(update) = program_status_rx.recv().await {
            let daemon = program_status_daemon.clone();
            tokio::task::spawn_blocking(move || {
                daemon.handle_program_status(update.terminal_id, update.generation, &update.changes)
            })
            .await
            .ok();
        }
    });

    let activity_daemon = daemon.clone();
    let activity_task = tokio::spawn(async move {
        while let Some(signal) = activity_rx.recv().await {
            if signal.session_id != 0 {
                activity_daemon.handle_terminal_activity(signal.terminal_id, signal.generation);
            }
        }
    });

    let alert_daemon = daemon.clone();
    let program_status_alert_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(PROGRAM_STATUS_ALERT_FLUSH_INTERVAL);
        loop {
            interval.tick().await;
            alert_daemon.flush_program_status_alerts();
        }
    });

    let checkpoint_daemon = daemon.clone();
    let activity_checkpoint_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(ACTIVITY_CHECKPOINT_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            checkpoint_daemon.checkpoint_session_activity();
        }
    });

    let push_daemon = daemon.clone();
    let push_delivery_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = push_daemon.push_wakeup().notified() => {}
                _ = tokio::time::sleep(PUSH_DELIVERY_INTERVAL) => {}
            }
            push_daemon
                .process_push_deliveries(crate::daemon::now_unix_ms())
                .await;
        }
    });

    let supervisor_wake_daemon = daemon.clone();
    let supervisor_wake_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = supervisor_wake_daemon.supervisor_wake_wakeup().notified() => {}
                _ = tokio::time::sleep(SUPERVISOR_WAKE_INTERVAL) => {}
            }
            // The pass delivers over an agent's own channel where it has
            // one, which is async, so it runs on the runtime rather than
            // on a blocking thread.
            supervisor_wake_daemon.process_supervisor_wakes().await;
        }
    });

    let stale_turn_daemon = daemon.clone();
    let stale_turn_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(STALE_TURN_INTERVAL).await;
            let daemon = stale_turn_daemon.clone();
            tokio::task::spawn_blocking(move || daemon.process_stale_turns())
                .await
                .ok();
        }
    });

    let listener_daemon = daemon.clone();
    let listener_task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let d = listener_daemon.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(d, stream).await {
                            debug!(error = %e, "connection ended with error");
                        }
                    });
                }
                Err(e) => {
                    warn!(error = %e, "accept failed");
                }
            }
        }
    });

    // The browser plane's address is reserved now and answered on below,
    // after forwards have recovered. Forward listeners and forward URLs
    // are both derived from this address, so it has to be known first.
    let (http_pending, bound_addr) = match http_addr {
        Some(addr) => {
            let tls = http_tls.map(crate::http::load_tls).transpose()?;
            if let Some(tls) = tls.clone() {
                daemon.set_http_tls_config(tls);
            }
            let (listener, bound) = crate::http::bind(addr, tls.is_some()).await?;
            daemon.set_http_addr(bound);
            (Some((listener, bound, tls)), Some(bound))
        }
        None => (None, None),
    };

    let worker_plane_addr = match worker_addr {
        Some(addr) => {
            let bound = crate::worker_plane::serve(daemon.clone(), addr).await?;
            daemon.set_worker_plane_addr(bound);
            Some(bound)
        }
        None => None,
    };

    // Recovery before the browser plane answers: a request that arrives
    // for a persisted forward whose route is not bound yet would be told
    // the forward is stopped, which is the wrong answer and a lasting one
    // for a page that only loads once.
    daemon.recover_forwards().await;
    daemon.recover_local_dir_shares().await;
    let http_task = http_pending
        .map(|(listener, bound, tls)| crate::http::serve(daemon.clone(), listener, bound, tls));

    Ok(ServerHandle {
        daemon,
        needs_input_task,
        program_status_task,
        program_status_alert_task,
        activity_task,
        activity_checkpoint_task,
        push_delivery_task,
        supervisor_wake_task,
        stale_turn_task,
        socket_path,
        http_addr: bound_addr,
        worker_addr: worker_plane_addr,
        listener_task,
        exit_task,
        http_task,
    })
}

async fn handle_connection(daemon: Arc<Daemon>, stream: UnixStream) -> anyhow::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let (out_tx, out_rx) = mpsc::channel::<ServerMsg>(OUTGOING_CHANNEL_CAPACITY);

    let writer_task = tokio::spawn(write_loop(write_half, out_rx));
    let result = read_loop(daemon, read_half, out_tx).await;
    writer_task.abort();
    result
}

async fn write_loop<W: AsyncWrite + Unpin>(mut w: W, mut rx: mpsc::Receiver<ServerMsg>) {
    while let Some(msg) = rx.recv().await {
        if frame::write_frame(&mut w, &msg.encode_to_vec())
            .await
            .is_err()
        {
            break;
        }
    }
    let _ = w.shutdown().await;
}

async fn read_loop<R: AsyncRead + Unpin>(
    daemon: Arc<Daemon>,
    mut r: R,
    out_tx: mpsc::Sender<ServerMsg>,
) -> anyhow::Result<()> {
    let mut conn = ConnState::default();

    loop {
        let buf = match frame::read_frame(&mut r).await? {
            Some(b) => b,
            None => break,
        };
        let envelope = match ClientEnvelope::decode(&buf) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "undecodable client frame, closing connection");
                break;
            }
        };
        handle_message(&daemon, envelope, &out_tx, &mut conn).await;
    }

    conn.abort_all();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::prepare_socket;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn fresh_path_binds_without_complaint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        prepare_socket(&path).await.unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        // A listener that is dropped leaves its socket file behind; a
        // connection to it is refused, so it is stale and removable.
        let listener = UnixListener::bind(&path).unwrap();
        drop(listener);
        assert!(path.exists());
        prepare_socket(&path).await.unwrap();
        assert!(!path.exists(), "stale socket should be removed");
    }

    #[tokio::test]
    async fn live_socket_is_not_hijacked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let err = prepare_socket(&path).await.unwrap_err();
        assert!(err.to_string().contains("already listening"), "{err}");
    }
}
