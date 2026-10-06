//! Dialing hosts that cannot dial back.
//!
//! A host on a private interface may be routable from the controller while
//! having no path out to it. For those the controller opens the socket, and
//! only that inverts: the host still sends the first control message and the
//! controller still dispatches its work.
//!
//! Because the controller is the one asking for access here, it is the one
//! that proves the enrollment token first, mirroring what a host does when
//! it dials in.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pm_protocol::worker_frame::{self, WorkerFrame};
use pm_tls::pairing::{Side, Transcript};
use pm_tls::{KeyHash, PeerPolicy};
use tokio::net::TcpStream;
use tokio_rustls::TlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tracing::{debug, info, warn};

use crate::daemon::Daemon;
use crate::storage::DialTarget;
use pm_protocol::domain::ControllerMsg;

/// Matches the reconnect backoff a dialing host uses, so neither direction
/// hammers the other after an outage.
const BACKOFF_START: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(8);
/// How often the controller looks for hosts it should be dialing but is not.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

pub type Link = WebSocketStream<TlsStream<TcpStream>>;

/// Keeps one connection attempt running per address. A host is dialed by
/// exactly one task, however many times it appears in the registry.
#[derive(Default)]
pub struct Dialers(Mutex<HashMap<String, tokio::task::JoinHandle<()>>>);

impl Dialers {
    fn retain_live(&self) {
        self.0
            .lock()
            .unwrap()
            .retain(|_, handle| !handle.is_finished());
    }

    fn is_dialing(&self, endpoint: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .get(endpoint)
            .is_some_and(|handle| !handle.is_finished())
    }

    fn track(&self, endpoint: String, handle: tokio::task::JoinHandle<()>) {
        self.0.lock().unwrap().insert(endpoint, handle);
    }
}

/// Watches for hosts the controller should be connected to and keeps one
/// dialer running for each. New enrollments are picked up without a restart,
/// which matters because a host cannot announce itself.
pub async fn run(daemon: Arc<Daemon>) {
    let dialers = Arc::new(Dialers::default());
    loop {
        dialers.retain_live();
        for target in daemon.accept_mode_dial_targets() {
            if dialers.is_dialing(&target.endpoint) {
                continue;
            }
            let endpoint = target.endpoint.clone();
            let daemon = daemon.clone();
            let handle = tokio::spawn(keep_connected(daemon, target));
            dialers.track(endpoint, handle);
        }
        tokio::time::sleep(SWEEP_INTERVAL).await;
    }
}

/// Dials one host until it stops being one the controller should reach.
async fn keep_connected(daemon: Arc<Daemon>, target: DialTarget) {
    let mut backoff = BACKOFF_START;
    loop {
        // Re-read rather than trusting the target this task started with: an
        // enrollment becomes a pinned host, and a removed host stops being
        // dialed at all.
        let Some(current) = daemon
            .accept_mode_dial_targets()
            .into_iter()
            .find(|candidate| candidate.endpoint == target.endpoint)
        else {
            info!(endpoint = %target.endpoint, "host is no longer dialed");
            return;
        };
        match connect(&daemon, &current).await {
            Ok(()) => {
                info!(endpoint = %current.endpoint, "host closed the link");
                backoff = BACKOFF_START;
            }
            Err(e) => debug!(endpoint = %current.endpoint, error = %e, "dialing the host failed"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn connect(daemon: &Arc<Daemon>, target: &DialTarget) -> anyhow::Result<()> {
    let identity = daemon
        .worker_identity()
        .map_err(|e| anyhow!("controller key: {e}"))?;
    let pinned = target.peer_key_hash.as_deref().and_then(KeyHash::from_hex);
    let policy = match pinned {
        Some(hash) => PeerPolicy::Pinned(hash),
        None => PeerPolicy::Pairing,
    };

    let tls = pm_tls::client_config(&identity, &policy)?;
    let tcp = TcpStream::connect(&target.endpoint)
        .await
        .with_context(|| format!("connecting to {}", target.endpoint))?;
    tcp.set_nodelay(true)?;
    let stream = tokio_rustls::TlsConnector::from(tls)
        .connect(pm_tls::peer_server_name(), tcp)
        .await
        .context("host did not present the expected key")?;
    let (host_key, exporter) = {
        let (_, connection) = stream.get_ref();
        (
            pm_tls::peer_key_hash(connection.peer_certificates())
                .ok_or_else(|| anyhow!("host presented no key"))?,
            pm_tls::pairing::exporter(connection)?,
        )
    };

    let request = format!("wss://{}/worker", target.endpoint)
        .as_str()
        .into_client_request()?;
    let (link, _) = tokio_tungstenite::client_async(request, TlsStream::from(stream)).await?;
    let (mut sink, mut stream) = link.split();

    let token = target
        .token_ciphertext
        .as_deref()
        .and_then(|sealed| daemon.open_enrollment_token(sealed));
    pair(
        &mut sink,
        &mut stream,
        identity.key_hash(),
        host_key,
        &exporter,
        token.as_deref(),
    )
    .await?;

    info!(endpoint = %target.endpoint, host_key = %host_key, "opened a link to a dialed host");
    let (frames, pumps) = crate::worker_plane::FrameLink::dialed(
        sink.reunite(stream).expect("same socket"),
        crate::worker_plane::Keepalive::CONTROL,
        crate::worker_plane::LinkId::new("control", host_key),
    );
    crate::worker_plane::run_session(daemon, &host_key.to_hex(), frames).await;
    pumps.finish().await;
    Ok(())
}

/// Proves the enrollment token to a host that does not yet know this
/// controller. The host speaks first because it is the listener, and the
/// controller proves first because it is the one asking for access.
async fn pair(
    sink: &mut futures::stream::SplitSink<Link, Message>,
    stream: &mut futures::stream::SplitStream<Link>,
    controller_key: KeyHash,
    host_key: KeyHash,
    exporter: &[u8; 32],
    token: Option<&str>,
) -> anyhow::Result<()> {
    let listener_nonce = match stream.next().await {
        Some(Ok(Message::Binary(buf))) => match worker_frame::decode(&buf) {
            Some(WorkerFrame::Ready) => return Ok(()),
            Some(WorkerFrame::PairHello { nonce }) => nonce,
            _ => return Err(anyhow!("host opened the link with an unexpected frame")),
        },
        _ => return Err(anyhow!("host did not open the link")),
    };
    let Some(token) = token else {
        return Err(anyhow!(
            "host does not recognize this controller and no enrollment is pending"
        ));
    };

    let dialer_nonce = pm_tls::pairing::nonce();
    let transcript = Transcript {
        exporter: *exporter,
        dialer_key: controller_key,
        listener_key: host_key,
        dialer_nonce,
        listener_nonce,
    };
    sink.send(Message::Binary(Bytes::from(
        worker_frame::encode_pair_proof(&dialer_nonce, &transcript.mac(token, Side::Dialer)),
    )))
    .await?;

    match stream.next().await {
        Some(Ok(Message::Binary(buf))) => match worker_frame::decode(&buf) {
            Some(WorkerFrame::PairAccept { mac })
                if transcript.verify(token, Side::Listener, &mac) =>
            {
                Ok(())
            }
            _ => {
                warn!("host failed the enrollment proof, refusing to enroll it");
                Err(anyhow!("host failed the enrollment proof"))
            }
        },
        _ => Err(anyhow!("host did not prove the enrollment token")),
    }
}

/// Opens the stream a control message just announced. A host the
/// controller dials cannot dial back, so the connection it would have made
/// is made from this end instead, carrying the same single-use token so the
/// host can tell which request it belongs to.
pub fn open_announced_stream(dial: &Arc<crate::workers::StreamDial>, msg: &ControllerMsg) {
    let (path, token) = match msg {
        ControllerMsg::TerminalAttach { token, .. } => ("/worker/terminal", token.clone()),
        ControllerMsg::Transcript { token, .. } => ("/worker/transcript", token.clone()),
        ControllerMsg::ForwardOpen { token, .. } => ("/worker/stream", token.clone()),
        _ => return,
    };
    let dial = dial.clone();
    tokio::spawn(async move {
        if let Err(e) = open_stream(&dial, path, &token).await {
            debug!(endpoint = %dial.endpoint, path, error = %e, "opening an announced stream failed");
        }
    });
}

async fn open_stream(
    dial: &Arc<crate::workers::StreamDial>,
    path: &str,
    token: &str,
) -> anyhow::Result<()> {
    let daemon = dial
        .daemon
        .upgrade()
        .ok_or_else(|| anyhow!("controller is shutting down"))?;
    let identity = daemon
        .worker_identity()
        .map_err(|e| anyhow!("controller key: {e}"))?;
    let tls = pm_tls::client_config(&identity, &PeerPolicy::Pinned(dial.host_key))?;
    let tcp = TcpStream::connect(&dial.endpoint).await?;
    tcp.set_nodelay(true)?;
    let stream = tokio_rustls::TlsConnector::from(tls)
        .connect(pm_tls::peer_server_name(), tcp)
        .await?;
    let mut request = format!("wss://{}{path}", dial.endpoint)
        .as_str()
        .into_client_request()?;
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        format!("Bearer {token}").parse()?,
    );
    let (link, _) = tokio_tungstenite::client_async(request, TlsStream::from(stream)).await?;
    let (frames, pumps) = crate::worker_plane::FrameLink::dialed(
        link,
        crate::worker_plane::Keepalive::for_path(path),
        crate::worker_plane::LinkId::for_path(path, dial.host_key),
    );
    crate::worker_plane::serve_stream(&daemon, path, token, frames).await;
    pumps.finish().await;
    Ok(())
}

impl Daemon {
    pub fn accept_mode_dial_targets(&self) -> Vec<DialTarget> {
        self.storage()
            .accept_mode_dial_targets(crate::daemon::now_unix_ms())
            .unwrap_or_default()
    }

    pub fn open_enrollment_token(&self, sealed: &str) -> Option<String> {
        crate::secrets::open_secret(&self.installation_secret(), sealed)
    }
}
