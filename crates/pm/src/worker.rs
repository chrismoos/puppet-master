//! `pm worker`: joins this machine to a controller and runs the agent
//! sessions it dispatches. The controller owns state and the web UI; the
//! worker owns the local PTYs and relays their output, lifecycle, and
//! hooks up the connection. PTY output is next to the agent, so only the
//! fan-out to viewers crosses the network.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context};
use futures::{SinkExt, StreamExt};
use pm_adapters::{AdapterRegistry, Integration, SpawnCtx};
use pm_daemon::mux::{Mux, MuxChannels};
use pm_protocol::domain::{
    AgentInboxMode, AgentInboxOutcome, ClientEnvelope, ClientMsg, ControllerMsg, ServerMsg,
    WorkerMsg,
};
use pm_protocol::frame;
use pm_protocol::worker_frame::{self, WorkerFrame};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, trace, warn};

use crate::paths;
use crate::worker_transcripts::WorkerTranscriptStore;

/// Reconnect backoff bounds.
const BACKOFF_START: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(8);

/// The credential and worker id one controller issued this worker. Each
/// controller enrolls the worker separately and mints its own credential,
/// so switching controllers never presents controller A's credential to
/// controller B.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct ControllerCreds {
    credential: Option<String>,
    worker_id: Option<u64>,
    /// The controller's public key, pinned at enrollment. Without it this
    /// host has no way to tell its controller from anything else that can
    /// reach the same address, so a connection that cannot present it is
    /// refused rather than trusted.
    controller_key: Option<String>,
}

impl ControllerCreds {
    /// The command-line token remains available across reconnect attempts,
    /// but it is only registration material until a controller has issued
    /// this host its durable credential.
    fn enrollment_token<'a>(
        &self,
        token: Option<&'a str>,
        explicit_enrollment: bool,
    ) -> Option<&'a str> {
        token.filter(|_| explicit_enrollment || self.credential.is_none())
    }
}

/// How a listening host was last told to bind, so a plain `pm worker`
/// comes back the same way instead of falling through to dialing.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
struct ListenSettings {
    addr: String,
    #[serde(default)]
    allow_any: bool,
    #[serde(default)]
    allow_from: Vec<String>,
}

/// How a worker is launched. Present means a container holds it, and
/// everything here is what the launcher needs to build that container.
/// Absent means the worker runs on this machine directly.
///
/// This is the record of the worker's configuration, not a cache of the
/// last command line: `pm worker` launches from it and `pm worker
/// modify` is what changes it. Nothing else writes it, which is what
/// makes "what is this worker running with" a question with one answer.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SandboxProfile {
    #[serde(default)]
    pub incus_shifted_home: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::sandbox::RuntimeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub restart: crate::sandbox::RestartPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "crate::sandbox::SandboxLimits::is_unset"
    )]
    pub limits: crate::sandbox::SandboxLimits,
}

/// One worker identity: how it last connected and what it enrolled as.
/// A machine can hold several, told apart by `--name`, each with its own
/// key so a controller sees them as separate hosts.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct WorkerProfile {
    default_controller: Option<String>,
    /// Present when the last enrollment was a listening one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listen: Option<ListenSettings>,
    /// Present when this worker runs in a container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sandbox: Option<SandboxProfile>,
    #[serde(default)]
    controllers: HashMap<String, ControllerCreds>,
}

/// The name a run without `--name` enrolls under.
const DEFAULT_PROFILE: &str = "default";

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct WorkerConfig {
    #[serde(default)]
    profiles: HashMap<String, WorkerProfile>,

    /// Everything below is read from older files and folded into the
    /// profile map on load, then written back in the new shape.
    #[serde(default, skip_serializing)]
    default_controller: Option<String>,
    #[serde(default, skip_serializing)]
    listen: Option<ListenSettings>,
    #[serde(default, skip_serializing)]
    controllers: HashMap<String, ControllerCreds>,
    #[serde(default, skip_serializing)]
    controller: Option<String>,
    #[serde(default, skip_serializing)]
    credential: Option<String>,
    #[serde(default, skip_serializing)]
    worker_id: Option<u64>,
}

impl WorkerConfig {
    fn load() -> Self {
        let mut cfg: WorkerConfig = std::fs::read_to_string(paths::worker_config_path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        cfg.migrate_legacy();
        cfg
    }

    /// Folds an older flat `worker.toml` (single controller and credential)
    /// into the per-controller map so an upgrade keeps its enrollment.
    fn migrate_legacy(&mut self) {
        if let Some(controller) = self.controller.take() {
            let key = normalize_controller(&controller);
            let entry = self.controllers.entry(key.clone()).or_default();
            if entry.credential.is_none() {
                entry.credential = self.credential.take();
            }
            if entry.worker_id.is_none() {
                entry.worker_id = self.worker_id.take();
            }
            self.default_controller.get_or_insert(key);
        }
        self.credential = None;
        self.worker_id = None;
        self.migrate_into_profiles();
    }

    /// Folds a file written before names existed into the default
    /// profile, so a host enrolled then keeps its enrollment and its
    /// identity key.
    fn migrate_into_profiles(&mut self) {
        if self.default_controller.is_none() && self.controllers.is_empty() {
            return;
        }
        let profile = self
            .profiles
            .entry(DEFAULT_PROFILE.to_string())
            .or_default();
        if profile.default_controller.is_none() {
            profile.default_controller = self.default_controller.take();
        }
        if profile.listen.is_none() {
            profile.listen = self.listen.take();
        }
        for (key, creds) in std::mem::take(&mut self.controllers) {
            profile.controllers.entry(key).or_insert(creds);
        }
        self.default_controller = None;
        self.listen = None;
    }

    /// The profile a run works with. Named or not, one always exists:
    /// a first run enrolls into it.
    fn profile(&mut self, name: Option<&str>) -> &mut WorkerProfile {
        self.profiles
            .entry(name.unwrap_or(DEFAULT_PROFILE).to_string())
            .or_default()
    }

    fn save(&self) -> anyhow::Result<()> {
        let path = paths::worker_config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// What `pm worker list` shows for one locally configured worker.
pub(crate) struct ProfileSummary {
    pub name: String,
    /// The controller it dials, or [`LISTENING_CONTROLLER`] when the
    /// controller dials it. None before its first enrollment.
    pub controller: Option<String>,
    pub sandbox: Option<SandboxProfile>,
    /// True once a controller has issued it a durable credential.
    pub enrolled: bool,
}

/// Every worker configured on this machine, in name order.
pub(crate) fn summaries() -> Vec<ProfileSummary> {
    let config = WorkerConfig::load();
    let mut out: Vec<ProfileSummary> = config
        .profiles
        .iter()
        .map(|(name, profile)| ProfileSummary {
            name: name.clone(),
            controller: profile.default_controller.clone(),
            sandbox: profile.sandbox.clone(),
            enrolled: profile
                .controllers
                .values()
                .any(|creds| creds.credential.is_some()),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The worker a command without `--name` means. One configured worker is
/// unambiguous and needs no name. Several are not, and picking the one
/// called `default` would silently act on the wrong host, so the names
/// are listed and the choice handed back.
pub(crate) fn select(explicit: Option<&str>) -> anyhow::Result<String> {
    if let Some(name) = explicit {
        return Ok(name.to_string());
    }
    let names: Vec<String> = summaries().into_iter().map(|p| p.name).collect();
    match names.len() {
        0 => Ok(DEFAULT_PROFILE.to_string()),
        1 => Ok(names.into_iter().next().expect("one name")),
        _ => Err(anyhow!(
            "this machine has {} workers configured, so --name says which: {}",
            names.len(),
            names.join(", ")
        )),
    }
}

pub(crate) fn exists(name: &str) -> bool {
    WorkerConfig::load().profiles.contains_key(name)
}

/// The container settings recorded for a worker, or None when it runs on
/// this machine directly.
pub(crate) fn sandbox_of(name: &str) -> Option<SandboxProfile> {
    WorkerConfig::load()
        .profiles
        .get(name)
        .and_then(|profile| profile.sandbox.clone())
}

/// The controller a worker dials, read from its own profile.
pub(crate) fn stored_controller(name: &str) -> Option<String> {
    WorkerConfig::load()
        .profiles
        .get(name)
        .and_then(|profile| profile.default_controller.clone())
        .filter(|controller| controller != LISTENING_CONTROLLER)
}

/// How a launched worker reaches its controller, for a profile the
/// launcher owns. A sandboxed worker enrolls inside its container, so
/// nothing else writes this and a rebuild would have no controller to
/// dial without it.
pub(crate) fn remember_launch_connection(
    name: &str,
    controller: Option<&str>,
    listen: Option<std::net::SocketAddr>,
    listen_any: bool,
    allow_from: &[String],
) -> anyhow::Result<()> {
    let Some((default_controller, listen)) =
        connection_fields(controller, listen, listen_any, allow_from)
    else {
        return Ok(());
    };
    let mut config = WorkerConfig::load();
    let profile = config.profile(Some(name));
    profile.default_controller = Some(default_controller);
    profile.listen = listen;
    config.save()
}

/// What a launch records about how its worker connects, or None when
/// the launch said nothing and the profile should keep what it has.
fn connection_fields(
    controller: Option<&str>,
    listen: Option<std::net::SocketAddr>,
    listen_any: bool,
    allow_from: &[String],
) -> Option<(String, Option<ListenSettings>)> {
    match (listen, controller) {
        (Some(addr), _) => Some((
            LISTENING_CONTROLLER.to_string(),
            Some(ListenSettings {
                addr: addr.to_string(),
                allow_any: listen_any,
                allow_from: allow_from.to_vec(),
            }),
        )),
        (None, Some(controller)) => Some((normalize_controller(controller), None)),
        (None, None) => None,
    }
}

/// How a worker was last told to listen, so a rebuild binds the same
/// address instead of silently becoming a dialing worker.
pub(crate) fn stored_listen(name: &str) -> Option<(std::net::SocketAddr, bool, Vec<String>)> {
    let config = WorkerConfig::load();
    let profile = config.profiles.get(name)?;
    if profile.default_controller.as_deref() != Some(LISTENING_CONTROLLER) {
        return None;
    }
    let saved = profile.listen.as_ref()?;
    let addr = saved.addr.parse().ok()?;
    Some((addr, saved.allow_any, saved.allow_from.clone()))
}

/// Records a worker's container settings, creating its profile if this
/// is the first thing known about it.
pub(crate) fn set_sandbox(name: &str, sandbox: Option<SandboxProfile>) -> anyhow::Result<()> {
    let mut config = WorkerConfig::load();
    config.profile(Some(name)).sandbox = sandbox;
    config.save()
}

/// Forgets a worker's local configuration. Its enrollment goes with it,
/// so the host has to enroll again to come back.
pub(crate) fn delete_profile(name: &str) -> anyhow::Result<bool> {
    let mut config = WorkerConfig::load();
    let removed = config.profiles.remove(name).is_some();
    if removed {
        config.save()?;
    }
    Ok(removed)
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    verbosity: u8,
    controller: Option<String>,
    token: Option<String>,
    listen: Option<std::net::SocketAddr>,
    listen_any: bool,
    allow_from: Vec<crate::worker_listener::Cidr>,
    name: Option<String>,
    enroll_only: bool,
) -> anyhow::Result<()> {
    crate::logging::init(verbosity);
    crate::fdlimit::raise_open_files();

    let mut config = WorkerConfig::load();
    let name = name.as_deref();
    let mode = resolve_start_mode(
        listen,
        listen_any,
        &allow_from,
        controller.as_deref(),
        config.profile(name),
    )?;
    // A listening host is not addressed by a controller URL, so its
    // per-controller entries are keyed by the key the handshake proves.
    // Recording how it started is what lets a bare `pm worker` come back
    // the same way rather than falling through to dialing.
    remember_start_mode(config.profile(name), &mode);
    config.save()?;
    let controller = match &mode {
        StartMode::Listen { .. } => LISTENING_CONTROLLER.to_string(),
        StartMode::Dial(controller) => controller.clone(),
    };

    // The mux, hook socket, and their relays live for the whole process,
    // so sessions survive a reconnect. Only the WebSocket is per-connection.
    let (mux, channels) = Mux::new();
    let mux = Arc::new(mux);
    // tempfile does not narrow a directory it creates, so this lands 0775 under
    // a common umask. It holds the hook socket and every session's MCP bearer
    // token, so it gets the same treatment as the daemon's own state.
    let runtime_dir = tempfile::tempdir().context("worker runtime dir")?;
    pm_daemon::fsperm::create_private_dir(runtime_dir.path())
        .context("restricting the worker runtime directory")?;
    let hook_socket = runtime_dir.path().join("hook.sock");
    let files_dir = runtime_dir.path().join("session-files");
    pm_daemon::fsperm::create_private_dir(&files_dir)
        .context("restricting the worker session-files directory")?;
    let transcripts = Arc::new(WorkerTranscriptStore::new(
        &paths::worker_transcript_dir(name),
        &controller,
    )?);
    let out_slot: OutSlot = Arc::new(Mutex::new(None));
    let spawn_faults = SpawnFaults::default();
    tokio::spawn(relay_mux_events(
        mux.clone(),
        channels,
        out_slot.clone(),
        transcripts.clone(),
        spawn_faults.clone(),
    ));
    let hook_relay = HookRelay::default();
    tokio::spawn(relay_hooks(
        hook_socket.clone(),
        out_slot.clone(),
        hook_relay.clone(),
    ));
    let mcp_relay = McpRelayEndpoint::bind(&out_slot).await?;
    let pending_update = Arc::new(crate::worker_update::PendingUpdate::default());
    crate::worker_update::watch(pending_update.clone(), mux.clone());
    let rt = WorkerRuntime {
        identity: crate::worker_link::identity(name)?,
        pending_update,
        mcp_relay,
        spawn_faults,
        hook_relay,
        mux,
        hook_socket,
        files_dir,
        out_slot,
        transcripts,
        dir_shares: Arc::new(Mutex::new(HashMap::new())),
        _runtime_dir: runtime_dir,
    };

    info!(
        name = name.unwrap_or(DEFAULT_PROFILE),
        key = %rt.identity.key_hash(),
        pm_version = pm_daemon::pm_build_version(),
        "worker started"
    );

    let serving = async {
        if let StartMode::Listen {
            addr,
            allow_any,
            allow_from,
        } = mode
        {
            return listen_for_controller(
                ListenMode {
                    addr,
                    allow_any,
                    allow_from,
                },
                token,
                &mut config,
                name,
                &rt,
                enroll_only,
            )
            .await;
        }

        if enroll_only {
            return connect(&controller, token.as_deref(), &mut config, name, &rt, true).await;
        }

        let mut backoff = BACKOFF_START;
        loop {
            match connect(&controller, token.as_deref(), &mut config, name, &rt, false).await {
                Ok(()) => info!("worker connection closed, reconnecting"),
                Err(e) => warn!(error = %e, "worker connection failed"),
            }
            info!(
                retry_in_ms = backoff.as_millis(),
                "waiting before the next attempt to reach the controller"
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    };

    tokio::select! {
        served = serving => served,
        signal = wait_for_signal() => {
            info!(
                signal,
                sessions = rt.mux.live_session_ids().len(),
                "worker exiting on a signal"
            );
            Ok(())
        }
    }
}

/// Resolves with the name of the signal that asked this process to stop.
/// A signalled worker otherwise exits with no output at all, which reads
/// from the log exactly like one that crashed or was never running.
#[cfg(unix)]
async fn wait_for_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    let installed = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
    );
    let (mut interrupt, mut terminate, mut hangup) = match installed {
        (Ok(interrupt), Ok(terminate), Ok(hangup)) => (interrupt, terminate, hangup),
        _ => {
            warn!("this host cannot install signal handlers, so a signalled exit says nothing");
            std::future::pending::<()>().await;
            unreachable!()
        }
    };
    tokio::select! {
        _ = interrupt.recv() => "SIGINT",
        _ = terminate.recv() => "SIGTERM",
        _ = hangup.recv() => "SIGHUP",
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() -> &'static str {
    if tokio::signal::ctrl_c().await.is_err() {
        warn!("this host cannot install a signal handler, so a signalled exit says nothing");
        std::future::pending::<()>().await;
    }
    "ctrl-c"
}

/// What the controller answered a registration with.
struct RegisteredControl {
    worker_id: u64,
    credential: String,
}

/// One of the launcher's container markers, or empty off a container.
fn sandbox_marker(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

fn register_frame(
    stored: &ControllerCreds,
    enroll: Option<&str>,
    rt: &WorkerRuntime,
) -> anyhow::Result<WorkerMsg> {
    Ok(WorkerMsg::Register {
        protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        enrollment_token: enroll.unwrap_or_default().to_string(),
        credential: stored.credential.clone().unwrap_or_default(),
        hostname: hostname(),
        platform: std::env::consts::OS.to_string(),
        pm_version: pm_daemon::pm_build_version().to_string(),
        // Set by the sandbox launcher in the container's environment.
        // The worker cannot work out either for itself, and a worker on
        // the host reports neither.
        runtime: sandbox_marker(crate::sandbox::RUNTIME_MARKER),
        container: sandbox_marker(crate::sandbox::CONTAINER_MARKER),
        default_project_root: default_project_root(),
        // Re-announce still-running sessions so the controller re-adopts
        // survivors of a blip rather than failing them.
        live_sessions: rt.mux.live_session_ids(),
        live_terminals: rt.mux.live_terminals(),
        pending_transcripts: rt.transcripts.pending()?,
        // A share's server survives a control-link blip, so the
        // controller adopts the survivors on the ports they already
        // hold rather than restarting them under new ones.
        live_dir_shares: rt
            .dir_shares
            .lock()
            .unwrap()
            .iter()
            .map(|(share_id, server)| pm_protocol::domain::WorkerDirShare {
                share_id: *share_id,
                port: server.port(),
            })
            .collect(),
    })
}

/// The first reply must be the registration outcome.
async fn read_registration(
    ws_read: &mut WsRead,
    pending: &crate::worker_update::PendingUpdate,
) -> anyhow::Result<RegisteredControl> {
    let registered = match ws_read.next().await {
        Some(Ok(Message::Binary(buf))) => match worker_frame::decode(&buf) {
            Some(WorkerFrame::Control(p)) => ControllerMsg::decode(p).ok(),
            _ => None,
        },
        _ => None,
    };
    let Some(ControllerMsg::Registered {
        worker_id,
        credential,
        error,
        mcp_base_url: _,
        http_port: _,
        pm_version,
    }) = registered
    else {
        return Err(anyhow!("controller did not acknowledge registration"));
    };
    if !error.is_empty() {
        // A refusal names the controller's build, which is the build to
        // follow whatever the reason for the refusal.
        pending.note_controller_build(&pm_version);
        return Err(anyhow!("registration refused: {error}"));
    }
    pending.note_controller_build(&pm_version);
    Ok(RegisteredControl {
        worker_id,
        credential,
    })
}

/// Registers with a controller that dialed in, then serves it. The register
/// exchange is identical to the dialing case: only who opened the socket
/// differed, and that is settled by the time this runs.
#[allow(clippy::too_many_arguments)]
async fn serve_controller(
    entry_key: &str,
    accepted: crate::worker_listener::ControlLink,
    streams: crate::worker_listener::PendingStreams,
    token: Option<&str>,
    config: &mut WorkerConfig,
    name: Option<&str>,
    rt: &WorkerRuntime,
    superseded: Superseded,
    enroll_only: bool,
) -> anyhow::Result<()> {
    let stored = config
        .profile(name)
        .controllers
        .get(entry_key)
        .cloned()
        .unwrap_or_default();
    // Pairing proves the controller may reach the host, but the controller
    // deliberately consumes the enrollment only when it handles Register.
    // Send the same token there until it has issued this host a credential.
    let enroll = stored.enrollment_token(token, enroll_only);
    let (mut ws_write, ws_read) = accepted.link.split();
    ws_write
        .send(control(register_frame(&stored, enroll, rt)?))
        .await?;
    let mut ws_read = ws_read;
    let registered = tokio::time::timeout(
        REGISTER_DEADLINE,
        read_registration(&mut ws_read, &rt.pending_update),
    )
    .await
    .map_err(|_| anyhow!("controller did not answer registration"))??;
    let RegisteredControl {
        worker_id,
        credential,
    } = registered;
    record_registration(
        config.profile(name),
        entry_key,
        credential,
        worker_id,
        accepted.controller_key,
    );
    config.save()?;
    info!(
        worker = worker_id,
        "registered with the controller that dialed in"
    );
    if enroll_only {
        return Ok(());
    }
    crate::worker_update::apply_before_serving(&rt.pending_update, &rt.mux).await;

    serve(
        rt,
        controller_links(&rt.mcp_relay, &rt.hook_relay, Streams::Accept(streams)),
        ws_write,
        ws_read,
        superseded,
    )
    .await
}

/// Config key for a host the controller dials. There is no URL to key on,
/// so entries hang off the controller key the handshake proved.
const LISTENING_CONTROLLER: &str = "listening";

/// What this run connects as, once flags and saved config are combined.
#[derive(Debug, Clone, PartialEq)]
enum StartMode {
    Dial(String),
    Listen {
        addr: std::net::SocketAddr,
        allow_any: bool,
        allow_from: Vec<crate::worker_listener::Cidr>,
    },
}

/// Records how this run connected, so a later bare `pm worker` resumes
/// the same way. Paired with `resolve_start_mode`: what one writes the
/// other has to read back.
fn remember_start_mode(config: &mut WorkerProfile, mode: &StartMode) {
    match mode {
        StartMode::Listen {
            addr,
            allow_any,
            allow_from,
        } => {
            config.default_controller = Some(LISTENING_CONTROLLER.to_string());
            config.listen = Some(ListenSettings {
                addr: addr.to_string(),
                allow_any: *allow_any,
                allow_from: allow_from.iter().map(ToString::to_string).collect(),
            });
        }
        StartMode::Dial(controller) => {
            config.default_controller = Some(controller.clone());
            config.listen = None;
        }
    }
}

/// Resolves how a run connects. Flags win; otherwise the last enrollment
/// decides, including whether this host listens rather than dials. A
/// listening host is not addressed by a URL, so without this it would
/// fall through to dialing and complain that it has no controller.
fn resolve_start_mode(
    listen: Option<std::net::SocketAddr>,
    listen_any: bool,
    allow_from: &[crate::worker_listener::Cidr],
    controller: Option<&str>,
    config: &WorkerProfile,
) -> anyhow::Result<StartMode> {
    if let Some(addr) = listen {
        return Ok(StartMode::Listen {
            addr,
            allow_any: listen_any,
            allow_from: allow_from.to_vec(),
        });
    }
    if let Some(controller) = controller {
        return Ok(StartMode::Dial(normalize_controller(controller)));
    }
    if config.default_controller.as_deref() == Some(LISTENING_CONTROLLER) {
        if let Some(saved) = &config.listen {
            let addr = saved.addr.parse().with_context(|| {
                format!("saved listen address {} is not an address", saved.addr)
            })?;
            let allow_from = saved
                .allow_from
                .iter()
                .map(|range| crate::worker_listener::Cidr::parse(range))
                .collect::<anyhow::Result<Vec<_>>>()?;
            return Ok(StartMode::Listen {
                addr,
                allow_any: saved.allow_any,
                allow_from,
            });
        }
    }
    if let Some(controller) = config.default_controller.clone() {
        return Ok(StartMode::Dial(controller));
    }
    Err(anyhow!(
        "no enrollment saved: pass --controller to dial a controller, or --listen to accept one"
    ))
}

/// Whether a failed pinned dial should be retried as a fresh pairing.
/// Only a token passed on the command line asks for that: without one a
/// pin stays authoritative, so a key change is still refused. The dial
/// must also have failed on the key itself, because re-pairing spends an
/// enrollment, and a controller that was merely restarting still holds the
/// key this host pinned.
fn should_repair_pin(
    pinned: Option<pm_tls::KeyHash>,
    cli_token: Option<&str>,
    failure: &crate::worker_link::DialError,
) -> bool {
    failure.refused_key() && pinned.is_some() && cli_token.is_some()
}

/// Stores what a completed registration ties this host to. It lives on the
/// profile, not on the legacy top-level map, because only the profile is
/// serialized — recording it anywhere else loses the enrollment on save.
fn record_registration(
    profile: &mut WorkerProfile,
    key: &str,
    credential: String,
    worker_id: u64,
    controller_key: pm_tls::KeyHash,
) {
    let entry = profile.controllers.entry(key.to_string()).or_default();
    if !credential.is_empty() {
        entry.credential = Some(credential);
    }
    entry.worker_id = Some(worker_id);
    entry.controller_key = Some(controller_key.to_hex());
}

/// Forgets what ties this host to one controller. Both have to go: the
/// pin blocks the handshake, and a surviving credential would suppress
/// the enrollment token that the retry depends on.
fn clear_for_reenrollment(config: &mut WorkerProfile, controller: &str) {
    let entry = config
        .controllers
        .entry(controller.to_string())
        .or_default();
    entry.controller_key = None;
    entry.credential = None;
}

fn controller_entry_key(controller_key: pm_tls::KeyHash) -> String {
    format!("key:{controller_key}")
}

/// Where a dialled-in controller is admitted, which travels together
/// because `StartMode::Listen` is the only thing that produces it.
struct ListenMode {
    addr: std::net::SocketAddr,
    allow_any: bool,
    allow_from: Vec<crate::worker_listener::Cidr>,
}

/// Serves controllers that dial in, one control link at a time.
async fn listen_for_controller(
    listen: ListenMode,
    token: Option<String>,
    config: &mut WorkerConfig,
    name: Option<&str>,
    rt: &WorkerRuntime,
    enroll_only: bool,
) -> anyhow::Result<()> {
    let pinned: Vec<pm_tls::KeyHash> = config
        .profile(name)
        .controllers
        .keys()
        .filter_map(|key| key.strip_prefix("key:"))
        .filter_map(pm_tls::KeyHash::from_hex)
        .collect();
    let streams = crate::worker_listener::PendingStreams::default();
    let mut listener = crate::worker_listener::bind(
        crate::worker_listener::ListenConfig {
            addr: listen.addr,
            allow_any: listen.allow_any,
            allow_from: listen.allow_from,
            pinned,
            token: token.clone(),
        },
        rt.identity.clone(),
        streams.clone(),
    )
    .await?;
    info!(
        addr = %listener.local_addr(),
        key = %rt.identity.key_hash(),
        "host listening for its controller"
    );

    let mut links: u64 = 0;
    loop {
        // A host whose link drops comes back here and, without this, says
        // nothing ever again: waiting for a controller and having died are
        // the same empty log.
        info!(
            addr = %listener.local_addr(),
            key = %rt.identity.key_hash(),
            links,
            "waiting for a controller to dial in"
        );
        let Some(accepted) = listener.accept_control().await else {
            break;
        };
        links += 1;
        let opened = std::time::Instant::now();
        let epoch = accepted.epoch;
        let entry = controller_entry_key(accepted.link.controller_key);
        if accepted.link.enrolled {
            config
                .profile(name)
                .controllers
                .entry(entry.clone())
                .or_default()
                .controller_key = Some(accepted.link.controller_key.to_hex());
            config.save()?;
        }
        match serve_controller(
            &entry,
            accepted.link,
            streams.clone(),
            token.as_deref(),
            config,
            name,
            rt,
            Box::pin(listener.superseded(epoch)),
            enroll_only,
        )
        .await
        {
            Ok(()) if enroll_only => return Ok(()),
            Ok(()) if listener.is_superseded(epoch) => {
                info!("a newer controller link replaced this one")
            }
            Ok(()) => info!("controller closed the link"),
            Err(e) => warn!(error = %e, "controller link failed"),
        }
        debug!(
            links,
            lifetime_ms = opened.elapsed().as_millis(),
            "the control link ended"
        );
        // The link that would carry the answers is gone, so nothing still
        // waiting on it can be served.
        streams.clear();
        rt.mcp_relay.relay.abandon();
        rt.hook_relay.abandon();
    }
    Err(anyhow!("host stopped accepting controller connections"))
}

/// Worker state that persists across reconnects: the mux and its running
/// agents, the local hook socket, and the slot the event relays write the
/// current connection's outbound channel into.
type OutSlot = Arc<Mutex<Option<mpsc::Sender<Message>>>>;

/// How long a relayed hook waits for the controller's answer. Strictly
/// under the hook client's own delivery budget, so a controller that goes
/// quiet costs the agent a plain stop rather than a stalled harness.
const HOOK_RESULT_TIMEOUT: Duration = Duration::from_secs(3);

/// Pause after an accept that failed for want of descriptors or memory, so
/// the relay yields to whatever frees them instead of spinning.
const HOOK_ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Correlates a relayed hook with the controller's answer. Without one the
/// relay is fire-and-forget and a remote agent never receives the Stop-hook
/// report nudge the controller-local path returns.
#[derive(Clone, Default)]
struct HookRelay {
    pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<String>>>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

impl HookRelay {
    /// Claims a correlation id and the receiver its answer arrives on. Ids
    /// start at 1 because zero means "no reply wanted" on the wire.
    fn register(&self) -> (u64, tokio::sync::oneshot::Receiver<String>) {
        let req_id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(req_id, tx);
        (req_id, rx)
    }

    fn resolve(&self, req_id: u64, nudge: String) {
        if let Some(waiter) = self.pending.lock().unwrap().remove(&req_id) {
            let _ = waiter.send(nudge);
        }
    }

    fn forget(&self, req_id: u64) {
        self.pending.lock().unwrap().remove(&req_id);
    }

    /// Drops every waiter because the link that would answer them is gone.
    fn abandon(&self) {
        self.pending.lock().unwrap().clear();
    }
}

/// The loopback endpoint agents post to, and the relay that carries their
/// requests up the control link.
struct McpRelayEndpoint {
    url: String,
    relay: crate::worker_mcp::McpRelay,
}

impl McpRelayEndpoint {
    async fn bind(out_slot: &OutSlot) -> anyhow::Result<Self> {
        let out = out_slot.clone();
        let relay = crate::worker_mcp::McpRelay::new(move |msg| send_via(&out, control(msg)));
        let url = crate::worker_mcp::serve(relay.clone()).await?;
        Ok(Self { url, relay })
    }
}

/// Why a spawned agent was stopped before it could do any work, keyed by
/// terminal, so its exit is reported as a failure with that reason.
type SpawnFaults = Arc<Mutex<HashMap<u64, String>>>;

struct WorkerRuntime {
    identity: pm_tls::Identity,
    pending_update: Arc<crate::worker_update::PendingUpdate>,
    mcp_relay: McpRelayEndpoint,
    spawn_faults: SpawnFaults,
    hook_relay: HookRelay,
    mux: Arc<Mux>,
    hook_socket: PathBuf,
    files_dir: PathBuf,
    out_slot: OutSlot,
    transcripts: Arc<WorkerTranscriptStore>,
    /// Directory shares this host serves, kept on the runtime rather
    /// than the connection: a share outlives the control link that
    /// asked for it, and is re-announced to whichever link comes next.
    dir_shares: DirShares,
    _runtime_dir: tempfile::TempDir,
}

/// Agents always post to the local relay, whichever end opened the control
/// link: that link is the one route to the controller this host has proven,
/// and the controller's browser plane may not be reachable from here at all.
fn controller_links(
    mcp_relay: &McpRelayEndpoint,
    hook_relay: &HookRelay,
    streams: Streams,
) -> ControllerLinks {
    ControllerLinks {
        streams,
        mcp_url: mcp_relay.url.clone(),
        mcp_relay: mcp_relay.relay.clone(),
        hook_relay: hook_relay.clone(),
    }
}

fn send_via(out_slot: &OutSlot, msg: Message) {
    let _ = try_send_via(out_slot, msg);
}

/// Whether the message reached the link's outgoing queue. A caller that
/// tells the agent its signal was accepted has to know the difference.
fn try_send_via(out_slot: &OutSlot, msg: Message) -> bool {
    let Some(tx) = out_slot.lock().unwrap().clone() else {
        return false;
    };
    tx.try_send(msg).is_ok()
}

fn control(msg: WorkerMsg) -> Message {
    Message::Binary(worker_frame::encode_control(&msg.encode_to_vec()).into())
}

/// Canonical form of a controller URL, used both to connect and as the
/// key under which its credential is stored. Accepts http(s):// too,
/// mapping to ws(s)://, and drops a trailing slash so different spellings
/// of the same controller resolve to one entry.
fn normalize_controller(controller: &str) -> String {
    let base = controller.trim_end_matches('/');
    base.strip_prefix("https://")
        .map(|r| format!("wss://{r}"))
        .unwrap_or_else(|| base.to_string())
}

/// Opens the per-stream connections the controller asks for. Each one is a
/// fresh mutually authenticated link to the same pinned controller, so a
/// terminal, transcript, or forward stream is no more trusted than the
/// control link that requested it.
#[derive(Clone)]
struct StreamDialer {
    identity: pm_tls::Identity,
    controller_key: pm_tls::KeyHash,
}

/// Which stream the controller asked for. Each is its own connection, so
/// one busy terminal cannot stall a transcript upload or a forward.
#[derive(Clone, Copy)]
enum StreamKind {
    Terminal,
    Transcript,
    Forward,
}

/// How the per-stream connections get opened. Only the direction differs:
/// either this host dials the controller for each one, or the controller
/// dials in and the host matches the arrival against a request it announced.
#[derive(Clone)]
enum Streams {
    Dial {
        dialer: StreamDialer,
        stream_url: String,
        terminal_url: String,
        transcript_url: String,
    },
    Accept(crate::worker_listener::PendingStreams),
}

/// How long to wait for a stream the controller said it would open. Long
/// enough for a slow link, short enough that an abandoned request does not
/// hold its slot for the life of the connection.
const STREAM_DEADLINE: Duration = Duration::from_secs(30);

impl Streams {
    async fn open(
        &self,
        kind: StreamKind,
        token: &str,
    ) -> anyhow::Result<crate::worker_link::Link> {
        match self {
            Streams::Dial {
                dialer,
                stream_url,
                terminal_url,
                transcript_url,
            } => {
                let url = match kind {
                    StreamKind::Terminal => terminal_url,
                    StreamKind::Transcript => transcript_url,
                    StreamKind::Forward => stream_url,
                };
                dialer.open(url, token).await
            }
            Streams::Accept(pending) => {
                let waiting = pending.expect(token);
                // A retired forwarder is aborted mid-wait, which would
                // otherwise leave its request registered with nothing behind
                // it.
                let _guard = Forget {
                    pending: pending.clone(),
                    token: token.to_string(),
                };
                match tokio::time::timeout(STREAM_DEADLINE, waiting).await {
                    Ok(Ok(link)) => Ok(link),
                    _ => Err(anyhow!("controller did not open the stream it asked for")),
                }
            }
        }
    }
}

/// Drops a stream request when the task that made it goes away.
struct Forget {
    pending: crate::worker_listener::PendingStreams,
    token: String,
}

impl Drop for Forget {
    fn drop(&mut self) {
        self.pending.forget(&self.token);
    }
}

/// Where this host reaches its controller, and how its streams are opened.
struct ControllerLinks {
    streams: Streams,
    mcp_url: String,
    mcp_relay: crate::worker_mcp::McpRelay,
    hook_relay: HookRelay,
}

impl StreamDialer {
    async fn open(&self, url: &str, token: &str) -> anyhow::Result<crate::worker_link::Link> {
        Ok(
            crate::worker_link::dial(&self.identity, Some(self.controller_key), url, Some(token))
                .await?
                .link,
        )
    }
}

fn worker_ws_url(controller: &str) -> String {
    format!("{}/worker", normalize_controller(controller))
}

/// The controller's per-stream forward endpoint. Each forwarded TCP
/// connection gets its own WebSocket here, so forward traffic never
/// shares a pipe with PTY frames.
fn worker_stream_url(controller: &str) -> String {
    format!("{}/stream", worker_ws_url(controller))
}

fn worker_terminal_url(controller: &str) -> String {
    format!("{}/terminal", worker_ws_url(controller))
}

fn worker_transcript_url(controller: &str) -> String {
    format!("{}/transcript", worker_ws_url(controller))
}

fn hostname() -> String {
    pm_daemon::hostname::machine_hostname("worker")
}

fn default_project_root() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Settles trust before this host acts on anything the controller sends.
///
/// A controller that already knows this host says so and the pinned
/// handshake has done the work. Otherwise it opens enrollment, and the two
/// prove the token to each other over material exported from this TLS
/// session. Only after the controller's own proof checks out does the host
/// pin its key, because until then "the controller" is just whatever
/// answered on that address.
async fn pair(
    ws_write: &mut WsWrite,
    ws_read: &mut WsRead,
    identity: &pm_tls::Identity,
    controller_key: pm_tls::KeyHash,
    exporter: &[u8; 32],
    token: Option<&str>,
) -> anyhow::Result<pm_tls::KeyHash> {
    use pm_tls::pairing::{Side, Transcript};

    let listener_nonce = match ws_read.next().await {
        Some(Ok(Message::Binary(buf))) => match worker_frame::decode(&buf) {
            // The controller recognizes this host, so the pinned handshake
            // already authenticated it and there is nothing left to prove.
            Some(WorkerFrame::Ready) => return Ok(controller_key),
            Some(WorkerFrame::PairHello { nonce }) => nonce,
            _ => {
                return Err(anyhow!(
                    "controller opened the link with an unexpected frame"
                ))
            }
        },
        _ => return Err(anyhow!("controller did not open the link")),
    };

    let Some(token) = token else {
        return Err(anyhow!(
            "controller does not recognize this host: pass --token to enroll again"
        ));
    };
    let dialer_nonce = pm_tls::pairing::nonce();
    let transcript = Transcript {
        exporter: *exporter,
        dialer_key: identity.key_hash(),
        listener_key: controller_key,
        dialer_nonce,
        listener_nonce,
    };
    ws_write
        .send(Message::Binary(
            worker_frame::encode_pair_proof(&dialer_nonce, &transcript.mac(token, Side::Dialer))
                .into(),
        ))
        .await?;

    let accepted = match ws_read.next().await {
        Some(Ok(Message::Binary(buf))) => match worker_frame::decode(&buf) {
            Some(WorkerFrame::PairAccept { mac }) => Some(mac),
            _ => None,
        },
        _ => None,
    };
    let Some(mac) = accepted else {
        return Err(anyhow!("controller did not prove the enrollment token"));
    };
    if !transcript.verify(token, Side::Listener, &mac) {
        return Err(anyhow!(
            "controller failed the enrollment proof, refusing to enroll"
        ));
    }
    info!(controller_key = %controller_key, "enrolled and pinned controller key");
    Ok(controller_key)
}

async fn connect(
    controller: &str,
    token: Option<&str>,
    config: &mut WorkerConfig,
    name: Option<&str>,
    rt: &WorkerRuntime,
    enroll_only: bool,
) -> anyhow::Result<()> {
    // The enrollment token is only for the first join with a controller.
    // Once it has issued a credential, reconnect with that instead.
    let mut stored = config
        .profile(name)
        .controllers
        .get(controller)
        .cloned()
        .unwrap_or_default();
    let pinned = stored
        .controller_key
        .as_deref()
        .and_then(pm_tls::KeyHash::from_hex);
    if pinned.is_none() && stored.enrollment_token(token, enroll_only).is_none() {
        return Err(anyhow!(
            "no enrollment for {controller}: pass --token to enroll this host"
        ));
    }

    let url = worker_ws_url(controller);
    info!(url = %url, "connecting to controller");
    let dialed = match crate::worker_link::dial(&rt.identity, pinned, &url, None).await {
        Ok(connected) => connected,
        // A controller rebuilt behind the same URL presents a new key, which
        // the pin rejects during the handshake. That is before registration,
        // so the stale-credential recovery below never runs and the host can
        // never rejoin. A token on the command line is an operator asking to
        // pair again, and the pairing proof still has to succeed, so this
        // reopens pairing rather than trusting the new key on sight.
        Err(error) if should_repair_pin(pinned, token, &error) => {
            warn!(error = %error, "controller presented a different key, re-enrolling with the supplied token");
            clear_for_reenrollment(config.profile(name), controller);
            config.save()?;
            stored = config
                .profile(name)
                .controllers
                .get(controller)
                .cloned()
                .unwrap_or_default();
            crate::worker_link::dial(&rt.identity, None, &url, None).await?
        }
        Err(error) if error.refused_key() => {
            return Err(anyhow::Error::from(error)).with_context(|| {
                format!(
                    "the controller at {controller} presented a different key than this host \
                     pinned. If it was rebuilt, re-enroll with a fresh --token, or remove its \
                     entry from {}",
                    crate::paths::worker_config_path().display()
                )
            });
        }
        Err(error) => return Err(error.into()),
    };
    let enroll = stored.enrollment_token(token, enroll_only);
    let crate::worker_link::Connected {
        link,
        controller_key,
        exporter,
    } = dialed;
    let (mut ws_write, mut ws_read) = link.split();

    let controller_key = pair(
        &mut ws_write,
        &mut ws_read,
        &rt.identity,
        controller_key,
        &exporter,
        enroll,
    )
    .await?;

    ws_write
        .send(control(register_frame(&stored, enroll, rt)?))
        .await?;

    // A silent controller is not a rejected credential, so this returns
    // before the recovery below rather than through it.
    let Ok(registered) = tokio::time::timeout(
        REGISTER_DEADLINE,
        read_registration(&mut ws_read, &rt.pending_update),
    )
    .await
    else {
        return Err(anyhow!("controller did not answer registration"));
    };
    let RegisteredControl {
        worker_id,
        credential,
    } = match registered {
        Ok(registered) => registered,
        Err(error) => {
            // A stored credential the controller no longer recognizes (its
            // DB was reset, or the worker was removed) would otherwise fail
            // every reconnect. If we still hold an enrollment token, drop the
            // dead credential so the next attempt re-enrolls instead.
            if stored.credential.is_some() && token.is_some() {
                config
                    .profile(name)
                    .controllers
                    .entry(controller.to_string())
                    .or_default()
                    .credential = None;
                config.save()?;
                warn!(error = %error, "controller rejected stored credential, will re-enroll");
            }
            return Err(error);
        }
    };
    record_registration(
        config.profile(name),
        controller,
        credential,
        worker_id,
        controller_key,
    );
    config.save()?;
    info!(worker = worker_id, "registered with controller");
    if enroll_only {
        return Ok(());
    }
    crate::worker_update::apply_before_serving(&rt.pending_update, &rt.mux).await;

    serve(
        rt,
        controller_links(
            &rt.mcp_relay,
            &rt.hook_relay,
            Streams::Dial {
                dialer: StreamDialer {
                    identity: rt.identity.clone(),
                    controller_key,
                },
                stream_url: worker_stream_url(controller),
                terminal_url: worker_terminal_url(controller),
                transcript_url: worker_transcript_url(controller),
            },
        ),
        ws_write,
        ws_read,
        // Nothing can replace a link this host opened: the next attempt only
        // starts once this one has ended.
        Box::pin(std::future::pending()),
    )
    .await
}

type WsWrite = futures::stream::SplitSink<crate::worker_link::Link, Message>;
type WsRead = futures::stream::SplitStream<crate::worker_link::Link>;

/// Ends the link this host is serving when something outside it decides so.
/// A host the controller dials uses it to drop a link a newer one replaced.
type Superseded = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// How long the controller has to answer registration. Past this the link is
/// no more useful than a closed one, and holding it keeps the host from
/// serving the controller's next attempt.
const REGISTER_DEADLINE: Duration = Duration::from_secs(30);

/// What the control link produced next. The variants a link ends on are
/// kept apart because they are the only account of a dropped link this
/// host has: a controller that closed, one whose socket failed, and one
/// that stopped answering all leave the host back at the accept loop.
#[derive(Debug, PartialEq)]
enum Next {
    Frame(bytes::Bytes),
    /// A ping or a pong: proof the controller is alive and nothing else.
    Alive,
    /// Something this link does not act on.
    Ignore,
    /// Nothing arrived within the keepalive deadline, so the peer is gone
    /// even though the socket never said so.
    Quiet,
    /// The controller closed the link.
    Closed,
    /// The socket failed.
    Failed(String),
}

/// Reads the next message, holding the controller to the keepalive deadline.
/// A half-open socket delivers nothing and fails nothing, so without the
/// deadline this waits on a dead controller for as long as the process runs.
async fn next_control<S, E>(stream: &mut S, idle: Option<Duration>) -> Next
where
    S: futures::Stream<Item = Result<Message, E>> + Unpin,
    E: std::fmt::Display,
{
    let next = match idle {
        Some(deadline) => match tokio::time::timeout(deadline, stream.next()).await {
            Ok(next) => next,
            Err(_) => return Next::Quiet,
        },
        None => stream.next().await,
    };
    match next {
        Some(Ok(Message::Binary(buf))) => Next::Frame(buf),
        Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => Next::Alive,
        Some(Ok(Message::Close(_))) | None => Next::Closed,
        Some(Err(error)) => Next::Failed(error.to_string()),
        Some(Ok(_)) => Next::Ignore,
    }
}

/// Serves one WebSocket connection against the persistent runtime. The
/// mux and its sessions outlive this call, so a disconnect pauses the
/// stream without killing the agents.
async fn serve(
    rt: &WorkerRuntime,
    links: ControllerLinks,
    ws_write: WsWrite,
    mut ws_read: WsRead,
    mut superseded: Superseded,
) -> anyhow::Result<()> {
    let registry = AdapterRegistry::standard();
    let keepalive = pm_daemon::worker_plane::Keepalive::CONTROL;
    // A worker with a healthy link and nothing to say logs nothing at all,
    // so a wedged one looks exactly like a quiet one. This pulse is what
    // makes silence mean something: if it stops, the worker stopped.
    const HEARTBEAT_EVERY: std::time::Duration = std::time::Duration::from_secs(300);

    let (out_tx, mut out_rx) = mpsc::channel::<Message>(4096);
    let writer = tokio::spawn(async move {
        let mut ws_write = ws_write;
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
            let msg = tokio::select! {
                msg = out_rx.recv() => match msg {
                    Some(msg) => msg,
                    None => {
                        debug!("this host released the control link");
                        break;
                    }
                },
                _ = due => {
                    trace!("keepalive ping sent to the controller");
                    Message::Ping(bytes::Bytes::new())
                }
            };
            if let Err(error) = ws_write.send(msg).await {
                warn!(error = %error, "writing to the control link failed, dropping it");
                break;
            }
        }
        let _ = ws_write.close().await;
    });
    let heartbeat = {
        let mux = rt.mux.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval_at(
                tokio::time::Instant::now() + HEARTBEAT_EVERY,
                HEARTBEAT_EVERY,
            );
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                timer.tick().await;
                let sessions = mux.live_session_ids();
                info!(
                    sessions = sessions.len(),
                    session_ids = ?sessions,
                    "worker heartbeat, control link up"
                );
            }
        })
    };

    // The persistent event relays now write to this connection.
    *rt.out_slot.lock().unwrap() = Some(out_tx.clone());

    let forwarders: Forwarders = Arc::new(Mutex::new(HashMap::new()));
    let pm_exe = std::env::current_exe()?;
    if pm_adapters::path_needs_shell_quoting(&pm_exe) {
        warn!(
            pm_exe = %pm_exe.display(),
            "pm binary path contains shell-hazardous characters, lifecycle hook commands embed it quoted"
        );
    }
    let idle = keepalive.idle_deadline();
    let mut last = tokio::time::Instant::now();
    loop {
        let next = tokio::select! {
            next = next_control(&mut ws_read, idle) => next,
            () = &mut superseded => {
                debug!("a newer control link arrived, releasing this one");
                break;
            }
        };
        let since = last.elapsed();
        last = tokio::time::Instant::now();
        let buf = match next {
            Next::Frame(buf) => buf,
            Next::Alive => {
                trace!(
                    since_ms = since.as_millis(),
                    "controller keepalive received"
                );
                // Half the deadline is the point where the link is closer
                // to being dropped than to healthy, and a controller that
                // later drops has been late here for a while first.
                if let Some(deadline) = idle.filter(|deadline| since * 2 > *deadline) {
                    debug!(
                        since_ms = since.as_millis(),
                        idle_ms = deadline.as_millis(),
                        "controller keepalive arrived past half the link's idle deadline"
                    );
                }
                continue;
            }
            Next::Ignore => continue,
            Next::Quiet => {
                warn!(
                    quiet_ms = since.as_millis(),
                    idle_ms = idle.map(|deadline| deadline.as_millis() as u64),
                    "controller went quiet past the keepalive deadline, dropping the link"
                );
                break;
            }
            Next::Closed => {
                debug!("the controller closed the control link");
                break;
            }
            Next::Failed(error) => {
                warn!(error, "reading the control link failed, dropping it");
                break;
            }
        };
        let Some(WorkerFrame::Control(payload)) = worker_frame::decode(&buf) else {
            debug!(
                bytes = buf.len(),
                "ignoring a frame this host does not speak"
            );
            continue;
        };
        let Ok(command) = ControllerMsg::decode(payload) else {
            warn!(
                "the controller sent a control message this host cannot decode, dropping the link"
            );
            break;
        };
        handle_command(
            &registry,
            &rt.mux,
            &out_tx,
            &rt.hook_socket,
            &rt.files_dir,
            &pm_exe,
            &links,
            &rt.transcripts,
            &forwarders,
            &rt.dir_shares,
            &rt.pending_update,
            &rt.spawn_faults,
            command,
        )
        .await;
    }

    // Disconnected: stop routing events to this dead connection and drop
    // its forwarders, but keep the sessions running for a reconnect.
    *rt.out_slot.lock().unwrap() = None;
    for (_, handle) in forwarders.lock().unwrap().drain() {
        handle.abort();
    }
    writer.abort();
    heartbeat.abort();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_command(
    registry: &AdapterRegistry,
    mux: &Arc<Mux>,
    out_tx: &mpsc::Sender<Message>,
    hook_socket: &std::path::Path,
    files_dir: &std::path::Path,
    pm_exe: &std::path::Path,
    links: &ControllerLinks,
    transcripts: &Arc<WorkerTranscriptStore>,
    forwarders: &Forwarders,
    dir_shares: &DirShares,
    pending_update: &crate::worker_update::PendingUpdate,
    spawn_faults: &SpawnFaults,
    command: ControllerMsg,
) {
    match command {
        ControllerMsg::UpdateNow => {
            pending_update.force();
        }
        ControllerMsg::Spawn {
            session_id,
            agent,
            task_prompt,
            permission_mode,
            cwd,
            session_token,
            resume_agent_session_id,
            terminal_id,
            generation,
            truecolor,
            fullscreen,
            compiled_instructions,
            model_endpoint,
            initial_cols,
            initial_rows,
        } => {
            let initial_size = match (initial_cols, initial_rows) {
                (Some(cols), Some(rows)) => Some((cols, rows)),
                _ => None,
            };
            // Only the failure was logged, so a controller that thinks a
            // session is running and a worker that never started one read
            // the same on this side.
            info!(
                session = session_id,
                terminal = terminal_id,
                generation,
                agent = ?agent,
                resuming = !resume_agent_session_id.is_empty(),
                cwd = %cwd,
                "spawning a session"
            );
            if let Err(e) = spawn_local(
                registry,
                mux,
                spawn_faults,
                hook_socket,
                files_dir,
                pm_exe,
                &links.mcp_url,
                session_id,
                terminal_id,
                generation,
                agent,
                &task_prompt,
                permission_mode,
                &cwd,
                &session_token,
                &resume_agent_session_id,
                &compiled_instructions,
                model_endpoint.map(|endpoint| *endpoint),
                truecolor,
                fullscreen,
                initial_size,
            )
            .await
            {
                error!(session = session_id, error = %e, "failed to spawn session");
                let _ = out_tx
                    .send(control(WorkerMsg::SessionState {
                        session_id,
                        state: pm_protocol::domain::SessionState::Failed,
                        detail: e.to_string(),
                    }))
                    .await;
            }
        }
        ControllerMsg::SpawnShell {
            terminal_id,
            generation,
            cwd,
            truecolor,
            initial_cols,
            initial_rows,
        } => {
            let initial_size = match (initial_cols, initial_rows) {
                (Some(cols), Some(rows)) => Some((cols, rows)),
                _ => None,
            };
            let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            let spec = pm_adapters::CommandSpec {
                program,
                args: Vec::new(),
                env: Vec::new(),
                cwd: PathBuf::from(cwd),
            };
            if mux
                .spawn(
                    terminal_id,
                    generation,
                    0,
                    &spec,
                    false,
                    true,
                    truecolor,
                    initial_size,
                )
                .is_err()
            {
                let _ = out_tx
                    .send(control(WorkerMsg::TerminalExit {
                        terminal_id,
                        generation,
                        exit_code: None,
                        state: pm_protocol::domain::TerminalRunState::Failed,
                        transcript_available: false,
                        transcript_size: 0,
                        detail: String::new(),
                    }))
                    .await;
            }
        }
        ControllerMsg::Interrupt { session_id } => {
            let _ = mux.interrupt(session_id);
        }
        ControllerMsg::Kill { session_id } => {
            if kill_answers_unknown(mux, session_id) {
                let _ = out_tx
                    .send(control(WorkerMsg::SessionExit {
                        session_id,
                        exit_code: None,
                    }))
                    .await;
            }
        }
        ControllerMsg::RepoRequest { req_id, op } => {
            // A capture reads whole files, so it runs off the control
            // task rather than stalling every other message behind it.
            let tx = out_tx.clone();
            tokio::task::spawn_blocking(move || {
                let msg = match pm_daemon::review_repo::handle_encoded(&op) {
                    Ok(answer) => WorkerMsg::RepoResponse {
                        req_id,
                        ok: true,
                        error: String::new(),
                        answer,
                    },
                    Err(error) => WorkerMsg::RepoResponse {
                        req_id,
                        ok: false,
                        error,
                        answer: Vec::new(),
                    },
                };
                let _ = tx.blocking_send(control(msg));
            });
        }
        ControllerMsg::FsList { req_id, path } => {
            let tx = out_tx.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.blocking_send(control(list_directory(req_id, &path)));
            });
        }
        ControllerMsg::HarnessRequest {
            req_id,
            agent,
            install,
        } => {
            let status = pm_daemon::harness_install::request(agent, install);
            let _ = out_tx
                .send(control(WorkerMsg::HarnessStatus { req_id, status }))
                .await;
        }
        ControllerMsg::AgentInbox {
            req_id,
            session_id,
            agent_terminal_id,
            agent,
            agent_session_id,
            agent_port,
            text,
            mode,
        } => {
            // Resolving the address is a pid lookup and a stat, so it
            // stays on the control task. The delivery itself waits on
            // the agent, so it does not.
            let channel = pm_daemon::inbox::local_inbound_channel(
                registry,
                mux,
                agent_terminal_id,
                agent,
                Some(agent_session_id),
                agent_port,
            );
            let tx = out_tx.clone();
            tokio::spawn(async move {
                let msg = deliver_to_agent_inbox(req_id, session_id, channel, &text, mode).await;
                let _ = tx.send(control(msg)).await;
            });
        }
        ControllerMsg::PathCheck { req_id, path } => {
            let tx = out_tx.clone();
            tokio::task::spawn_blocking(move || {
                let (status, detail) = pm_daemon::project_host::check_path(&path);
                info!(path = %path, status = status.as_str(), "checked a configured project path");
                let _ = tx.blocking_send(control(WorkerMsg::PathChecked {
                    req_id,
                    status,
                    detail,
                }));
            });
        }
        ControllerMsg::FileRead {
            req_id,
            root,
            path,
            max_bytes,
        } => {
            let tx = out_tx.clone();
            tokio::task::spawn_blocking(move || {
                let _ =
                    tx.blocking_send(control(read_scoped_file(req_id, &root, &path, max_bytes)));
            });
        }
        ControllerMsg::Registered { .. } => {}
        ControllerMsg::TerminalInterrupt { terminal_id, .. } => {
            let _ = mux.interrupt(terminal_id);
        }
        ControllerMsg::TerminalKill {
            terminal_id,
            generation,
        } => {
            if kill_answers_unknown(mux, terminal_id) {
                let _ = out_tx
                    .send(control(WorkerMsg::TerminalExit {
                        terminal_id,
                        generation,
                        exit_code: None,
                        state: pm_protocol::domain::TerminalRunState::Exited,
                        transcript_available: false,
                        transcript_size: 0,
                        detail: String::new(),
                    }))
                    .await;
            }
        }
        ControllerMsg::TerminalAttach {
            terminal_id,
            generation,
            token,
            replay_bytes,
            size: (cols, rows),
        } => {
            if cols > 0 && rows > 0 {
                let _ = mux.resize(terminal_id, cols, rows);
            }
            start_forwarding(
                mux,
                links,
                terminal_id,
                generation,
                &token,
                replay_bytes,
                forwarders,
            );
        }
        ControllerMsg::TerminalDetach { terminal_id, .. } => {
            stop_forwarding(terminal_id, forwarders);
        }
        ControllerMsg::Transcript {
            terminal_id,
            generation,
            token,
        } => {
            let path = transcripts.path(terminal_id, generation);
            let streams = links.streams.clone();
            tokio::spawn(async move {
                if let Err(error) = upload_transcript(&streams, &token, &path).await {
                    warn!(terminal = terminal_id, error = %error, "transcript upload failed");
                }
            });
        }
        ControllerMsg::TranscriptAck {
            terminal_id,
            generation,
        } => {
            if let Err(error) = transcripts.acknowledge(terminal_id, generation) {
                warn!(terminal = terminal_id, error = %error, "failed to acknowledge transcript");
            }
        }
        ControllerMsg::McpResponse {
            req_id,
            status,
            body,
        } => links.mcp_relay.resolve(req_id, status, body),
        ControllerMsg::HookResult { req_id, nudge } => {
            links.hook_relay.resolve(req_id, nudge);
        }
        ControllerMsg::DirShareServe {
            share_id,
            root,
            path,
        } => {
            // Resolving the root stats a filesystem that may be slow,
            // so it stays off the control loop.
            let out_tx = out_tx.clone();
            let dir_shares = dir_shares.clone();
            tokio::spawn(async move {
                let bound = |port| WorkerMsg::DirShareBound {
                    share_id,
                    ok: true,
                    error: String::new(),
                    port,
                };
                // A share already being served answers on the port it
                // already holds. Binding a second one and replacing the
                // first would abort it, and the controller may have
                // recorded that port as the share's target.
                if let Some(port) = serving_port(&dir_shares, share_id) {
                    let _ = out_tx.send(control(bound(port))).await;
                    return;
                }
                let reply = match pm_daemon::dir_server::resolve_share_root(&root, &path) {
                    Ok(resolved) => match pm_daemon::dir_server::serve(resolved).await {
                        Ok(server) => bound(keep_dir_share(&dir_shares, share_id, server)),
                        Err(error) => WorkerMsg::DirShareBound {
                            share_id,
                            ok: false,
                            error: format!("cannot serve {path}: {error}"),
                            port: 0,
                        },
                    },
                    Err(error) => WorkerMsg::DirShareBound {
                        share_id,
                        ok: false,
                        error: error.to_string(),
                        port: 0,
                    },
                };
                let _ = out_tx.send(control(reply)).await;
            });
        }
        ControllerMsg::DirShareStop { share_id } => {
            if dir_shares.lock().unwrap().remove(&share_id).is_some() {
                info!(share = share_id, "stopped serving a shared directory");
            }
        }
        ControllerMsg::ForwardOpen {
            req_id,
            port,
            token,
        } => {
            // The stream rides its own WebSocket, deliberately not
            // registered in `forwarders`: a control-link drop must not
            // kill in-flight forward streams.
            let out_tx = out_tx.clone();
            let streams = links.streams.clone();
            tokio::spawn(async move {
                match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    Ok(tcp) => {
                        let _ = out_tx
                            .send(control(WorkerMsg::ForwardOpened {
                                req_id,
                                ok: true,
                                error: String::new(),
                            }))
                            .await;
                        if let Err(e) = splice_forward_stream(&streams, &token, tcp).await {
                            info!(port, error = %e, "forward stream closed");
                        }
                    }
                    Err(e) => {
                        let _ = out_tx
                            .send(control(WorkerMsg::ForwardOpened {
                                req_id,
                                ok: false,
                                error: e.to_string(),
                            }))
                            .await;
                    }
                }
            });
        }
    }
}

/// Output queued behind an in-flight send goes out as one frame up to this size.
const OUTPUT_COALESCE_LIMIT_BYTES: usize = 16 * 1024;

/// Read buffer for the target-to-WebSocket half of a forward splice.
const FORWARD_SPLICE_BUF_BYTES: usize = 16 * 1024;

/// Splices one forwarded TCP connection over a dedicated WebSocket to
/// the controller, authenticated by the single-use stream token.
async fn splice_forward_stream(
    streams: &Streams,
    token: &str,
    tcp: tokio::net::TcpStream,
) -> anyhow::Result<()> {
    let link = streams.open(StreamKind::Forward, token).await?;
    splice_link(link, tcp, pm_daemon::worker_plane::Keepalive::STREAM).await
}

/// Carries bytes both ways between a forward stream and its target,
/// pinging the controller on the keepalive interval and holding it to
/// the deadline. A directory share's tunnel is open for as long as the
/// share is, and a controller that vanished half-open would otherwise
/// keep this task and the target connection for good.
async fn splice_link<L, T>(
    link: tokio_tungstenite::WebSocketStream<L>,
    target: T,
    keepalive: pm_daemon::worker_plane::Keepalive,
) -> anyhow::Result<()>
where
    L: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut ws_write, mut ws_read) = link.split();
    let (mut target_read, mut target_write) = tokio::io::split(target);
    let mut pings = keepalive.ping_interval().map(|every| {
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        timer
    });
    let idle = keepalive.idle_deadline();

    let up = async {
        let mut buf = vec![0u8; FORWARD_SPLICE_BUF_BYTES];
        loop {
            let due = async {
                match pings.as_mut() {
                    Some(timer) => timer.tick().await,
                    None => std::future::pending().await,
                }
            };
            let message = tokio::select! {
                read = target_read.read(&mut buf) => {
                    let n = read?;
                    if n == 0 {
                        break;
                    }
                    Message::Binary(bytes::Bytes::copy_from_slice(&buf[..n]))
                }
                _ = due => Message::Ping(bytes::Bytes::new()),
            };
            ws_write.send(message).await?;
        }
        let _ = ws_write.send(Message::Close(None)).await;
        Ok::<_, anyhow::Error>(())
    };
    let down = async {
        loop {
            let next = match idle {
                Some(deadline) => tokio::time::timeout(deadline, ws_read.next())
                    .await
                    .map_err(|_| {
                        anyhow!(
                            "the controller went quiet on the forward stream for {} ms",
                            deadline.as_millis()
                        )
                    })?,
                None => ws_read.next().await,
            };
            let Some(msg) = next else {
                break;
            };
            match msg? {
                Message::Binary(data) => target_write.write_all(&data).await?,
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = target_write.shutdown().await;
        Ok::<_, anyhow::Error>(())
    };
    tokio::try_join!(up, down)?;
    Ok(())
}

async fn upload_transcript(
    streams: &Streams,
    token: &str,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;

    let mut socket = streams.open(StreamKind::Transcript, token).await?;
    let mut file = tokio::fs::File::open(path).await?;
    let mut chunk = vec![0u8; pm_protocol::terminal_frame::MAX_REPLAY_CHUNK_BYTES];
    loop {
        let read = file.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        socket
            .send(Message::Binary(bytes::Bytes::copy_from_slice(
                &chunk[..read],
            )))
            .await?;
    }
    socket.send(Message::Close(None)).await?;
    Ok(())
}

/// Hands a controller's message to a session's agent over the agent's
/// own inbound channel.
///
/// The reply names the transport and the mode the channel honoured, or
/// why nothing was delivered. The message itself is neither logged nor
/// reported back: the controller already holds it, and the outcome is
/// all it needs.
async fn deliver_to_agent_inbox(
    req_id: u64,
    session_id: u64,
    channel: Option<pm_adapters::InboundChannel>,
    text: &str,
    mode: AgentInboxMode,
) -> WorkerMsg {
    let Some(channel) = channel else {
        return WorkerMsg::AgentInboxResult {
            req_id,
            outcome: AgentInboxOutcome::NoChannel,
            transport: String::new(),
            mode,
            detail: "this agent has no inbound channel on this host".into(),
        };
    };
    let transport = channel.transport().to_string();
    match pm_daemon::inbox::deliver(&channel, text, pm_daemon::inbox::delivery_mode(mode)).await {
        Ok(delivered) => {
            info!(
                session = session_id,
                transport = delivered.transport,
                bytes = text.len(),
                "handed a controller message to an agent's own inbox"
            );
            WorkerMsg::AgentInboxResult {
                req_id,
                outcome: AgentInboxOutcome::Delivered,
                transport,
                mode: pm_daemon::inbox::inbox_mode(delivered.mode),
                detail: String::new(),
            }
        }
        Err(e) => {
            warn!(
                session = session_id,
                transport = %transport,
                error = %e,
                "an agent's inbox did not take a controller message"
            );
            WorkerMsg::AgentInboxResult {
                req_id,
                outcome: AgentInboxOutcome::Failed,
                transport,
                mode,
                detail: e.to_string(),
            }
        }
    }
}

/// Lists a directory's immediate subdirectories for the controller's path
/// picker. Directories only, hidden entries omitted; an empty path lists
/// the worker user's home.
fn list_directory(req_id: u64, path: &str) -> WorkerMsg {
    let base = if path.trim().is_empty() {
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
    } else {
        PathBuf::from(path)
    };
    let dir = if base.is_dir() {
        base.clone()
    } else {
        base.parent().map(|p| p.to_path_buf()).unwrap_or(base)
    };
    let parent = dir.parent().map(|p| p.to_string_lossy().to_string());
    match std::fs::read_dir(&dir) {
        Ok(rd) => {
            let mut entries: Vec<pm_protocol::domain::FsEntry> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| pm_protocol::domain::FsEntry {
                    name: e.file_name().to_string_lossy().to_string(),
                    path: e.path().to_string_lossy().to_string(),
                })
                .collect();
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            WorkerMsg::FsListing {
                req_id,
                ok: true,
                error: String::new(),
                dir: dir.to_string_lossy().to_string(),
                parent,
                entries,
            }
        }
        Err(e) => WorkerMsg::FsListing {
            req_id,
            ok: false,
            error: e.to_string(),
            dir: dir.to_string_lossy().to_string(),
            parent,
            entries: Vec::new(),
        },
    }
}

/// Reads a regular file below the session cwd. Root is controller-owned session state;
/// user input may only select a descendant and no descendant symlink is followed.
fn read_scoped_file(req_id: u64, root: &str, path: &str, max_bytes: u64) -> WorkerMsg {
    let fail = |error: String| WorkerMsg::FileRead {
        req_id,
        ok: false,
        error,
        content: Vec::new(),
        filename: String::new(),
    };
    let root = match std::fs::canonicalize(root) {
        Ok(path) if path.is_dir() => path,
        Ok(_) => return fail("attachment root is not a directory".into()),
        Err(error) => return fail(format!("cannot access attachment root: {error}")),
    };
    let requested = std::path::Path::new(path);
    let relative = match if requested.is_absolute() {
        requested.strip_prefix(&root)
    } else {
        Ok(requested)
    } {
        Ok(relative) if !relative.as_os_str().is_empty() => relative,
        _ => return fail("attachment path is outside the session working directory".into()),
    };
    let mut checked = root.clone();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(_) => checked.push(component),
            std::path::Component::CurDir => continue,
            _ => return fail("attachment path traversal is not allowed".into()),
        }
        match std::fs::symlink_metadata(&checked) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return fail("attachment path must not traverse symlinks".into())
            }
            Ok(_) => {}
            Err(error) => return fail(format!("cannot inspect attachment path: {error}")),
        }
    }
    let canonical = match std::fs::canonicalize(&checked) {
        Ok(path) if path.starts_with(&root) => path,
        Ok(_) => return fail("attachment path is outside the session working directory".into()),
        Err(error) => return fail(format!("cannot access attachment file: {error}")),
    };
    let metadata = match std::fs::metadata(&canonical) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return fail("attachment path is not a regular file".into()),
        Err(error) => return fail(format!("cannot inspect attachment file: {error}")),
    };
    if metadata.len() > max_bytes {
        return fail(format!("attachment exceeds the {max_bytes}-byte limit"));
    }
    let mut file = match std::fs::File::open(&canonical) {
        Ok(file) => file,
        Err(error) => return fail(format!("cannot open attachment file: {error}")),
    };
    let mut content = Vec::with_capacity(metadata.len() as usize);
    if let Err(error) = std::io::Read::read_to_end(
        &mut std::io::Read::take(&mut file, max_bytes.saturating_add(1)),
        &mut content,
    ) {
        return fail(format!("cannot read attachment file: {error}"));
    }
    if content.len() as u64 > max_bytes {
        return fail(format!("attachment exceeds the {max_bytes}-byte limit"));
    }
    WorkerMsg::FileRead {
        req_id,
        ok: true,
        error: String::new(),
        content,
        filename: canonical
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn spawn_local(
    registry: &AdapterRegistry,
    mux: &Arc<Mux>,
    spawn_faults: &SpawnFaults,
    hook_socket: &std::path::Path,
    files_dir: &std::path::Path,
    pm_exe: &std::path::Path,
    mcp_url: &str,
    session_id: u64,
    terminal_id: u64,
    generation: u64,
    agent: pm_protocol::domain::AgentKind,
    task_prompt: &str,
    permission_mode: pm_protocol::domain::PermissionMode,
    cwd: &str,
    session_token: &str,
    resume_agent_session_id: &str,
    compiled_instructions: &str,
    model_endpoint: Option<pm_protocol::domain::ResolvedModelEndpoint>,
    truecolor: bool,
    fullscreen: bool,
    initial_size: Option<(u16, u16)>,
) -> anyhow::Result<()> {
    let adapter = registry.get(agent).map_err(|e| anyhow!("{e}"))?;
    let ctx = spawn_ctx(
        hook_socket,
        files_dir,
        pm_exe,
        mcp_url,
        session_id,
        task_prompt,
        permission_mode,
        cwd,
        session_token,
        compiled_instructions,
        model_endpoint,
        fullscreen,
    );
    let plan = if resume_agent_session_id.is_empty() {
        adapter.spawn_command(&ctx)?
    } else {
        adapter.resume_command(&ctx, resume_agent_session_id)?
    };
    mux.spawn(
        terminal_id,
        generation,
        session_id,
        &plan.spec,
        plan.detect_osc9_needs_input,
        false,
        truecolor,
        initial_size,
    )?;
    tokio::spawn(probe_agent_tools(
        mcp_url.to_string(),
        session_token.to_string(),
        session_id,
        terminal_id,
        mux.clone(),
        spawn_faults.clone(),
    ));
    // Output is not relayed until the controller attaches (a viewer opens
    // the session), so an unwatched session sends nothing over the link.
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn spawn_ctx(
    hook_socket: &std::path::Path,
    files_dir: &std::path::Path,
    pm_exe: &std::path::Path,
    mcp_url: &str,
    session_id: u64,
    task_prompt: &str,
    permission_mode: pm_protocol::domain::PermissionMode,
    cwd: &str,
    session_token: &str,
    compiled_instructions: &str,
    model_endpoint: Option<pm_protocol::domain::ResolvedModelEndpoint>,
    fullscreen: bool,
) -> SpawnCtx {
    SpawnCtx {
        cwd: PathBuf::from(cwd),
        task_prompt: task_prompt.to_string(),
        permission_mode,
        compiled_instructions: compiled_instructions.to_string(),
        model_endpoint,
        fullscreen,
        integration: Integration {
            session_id,
            session_token: session_token.to_string(),
            socket_path: hook_socket.to_path_buf(),
            pm_exe: pm_exe.to_path_buf(),
            files_dir: files_dir.to_path_buf(),
            mcp_url: Some(mcp_url.to_string()),
            // An agent API pinned here would only be addressable from
            // this host, and the controller is where a supervisor sends
            // from, so a worker-hosted session keeps the terminal as its
            // way in until the address can travel.
            agent_port: None,
        },
    }
}

/// The tool every session is granted, so its absence means the tool surface
/// as a whole is missing.
const REQUIRED_AGENT_TOOL: &str = "report";
/// A control link that blips at spawn reconnects within this window, so a
/// session is only failed once the tools stay unreachable across it.
const TOOL_PROBE_ATTEMPTS: u32 = 3;
const TOOL_PROBE_RETRY: Duration = Duration::from_secs(5);
const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Confirms, from where the agent runs, that its MCP endpoint answers with
/// the puppet-master tools. An agent without them cannot report, read the
/// board, or hand off, and nothing else would make that visible: it is
/// stopped and its session failed with the reason instead.
async fn probe_agent_tools(
    mcp_url: String,
    session_token: String,
    session_id: u64,
    terminal_id: u64,
    mux: Arc<Mux>,
    spawn_faults: SpawnFaults,
) {
    let mut last_error = String::new();
    for attempt in 1..=TOOL_PROBE_ATTEMPTS {
        match list_agent_tools(&mcp_url, &session_token).await {
            Ok(tools) => match require_agent_tool(&tools) {
                Ok(()) => {
                    info!(session = session_id, url = %mcp_url, "agent tool surface verified");
                    return;
                }
                Err(error) => last_error = error,
            },
            Err(error) => last_error = format!("{error:#}"),
        }
        warn!(
            session = session_id,
            attempt,
            url = %mcp_url,
            error = %last_error,
            "agent tool surface probe failed"
        );
        if attempt < TOOL_PROBE_ATTEMPTS {
            tokio::time::sleep(TOOL_PROBE_RETRY).await;
        }
    }
    let detail = format!("agent cannot reach its puppet-master tools at {mcp_url}: {last_error}");
    spawn_faults
        .lock()
        .unwrap()
        .insert(terminal_id, detail.clone());
    if mux.kill(session_id).is_err() {
        spawn_faults.lock().unwrap().remove(&terminal_id);
        return;
    }
    error!(
        session = session_id,
        "stopping an agent that started without its tools: {detail}"
    );
}

fn require_agent_tool(tools: &[String]) -> Result<(), String> {
    if tools.iter().any(|tool| tool == REQUIRED_AGENT_TOOL) {
        Ok(())
    } else {
        Err(format!(
            "the tool list has no {REQUIRED_AGENT_TOOL:?} (got {tools:?})"
        ))
    }
}

async fn list_agent_tools(mcp_url: &str, session_token: &str) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(TOOL_PROBE_TIMEOUT)
        .build()?;
    let response = client
        .post(mcp_url)
        .bearer_auth(session_token)
        .json(&serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .context("posting tools/list")?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("the MCP endpoint answered {status}"));
    }
    let body: serde_json::Value = response.json().await.context("decoding tools/list")?;
    if let Some(error) = body.get("error") {
        return Err(anyhow!("the MCP endpoint refused tools/list: {error}"));
    }
    Ok(body["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

type Forwarders = Arc<Mutex<HashMap<u64, tokio::task::JoinHandle<()>>>>;

/// Running directory-share servers, keyed by share id.
type DirShares = Arc<Mutex<HashMap<u64, pm_daemon::dir_server::DirShareServer>>>;

fn serving_port(shares: &DirShares, share_id: u64) -> Option<u16> {
    shares
        .lock()
        .unwrap()
        .get(&share_id)
        .map(|server| server.port())
}

/// Files a server under its share and returns the port that survives,
/// which is the incumbent's when one is already there. Replacing it
/// would abort a server the controller may already be dialing, and two
/// binds for one share land here whenever a restart brings both share
/// restore paths through at once.
fn keep_dir_share(
    shares: &DirShares,
    share_id: u64,
    server: pm_daemon::dir_server::DirShareServer,
) -> u16 {
    use std::collections::hash_map::Entry;
    match shares.lock().unwrap().entry(share_id) {
        Entry::Occupied(existing) => existing.get().port(),
        Entry::Vacant(slot) => slot.insert(server).port(),
    }
}

/// Begins relaying a session's PTY: sends its scrollback as a replay
/// frame, then streams live output, until stopped. The worker's command
/// loop is sequential, so no two attaches race for one session.
fn start_forwarding(
    mux: &Arc<Mux>,
    links: &ControllerLinks,
    terminal_id: u64,
    generation: u64,
    token: &str,
    replay_bytes: u64,
    forwarders: &Forwarders,
) {
    forwarders
        .lock()
        .unwrap()
        .retain(|_, handle| !handle.is_finished());
    // The controller announces an attach only when it holds no stream for
    // the session, so a forwarder still running here is writing into a socket
    // it has let go. Terminal streams carry no keepalive, so keeping that one
    // would ignore every later attach and strand the session.
    stop_forwarding(terminal_id, forwarders);
    // A state snapshot is bounded by the model's scrollback and must not
    // be tail-trimmed, so the requested byte cap no longer applies.
    let _ = replay_bytes;
    let (replay, mut rx) = match mux.attach_snapshot(terminal_id) {
        Ok(snapshot) => (snapshot.bytes, snapshot.output),
        Err(_) => return,
    };
    let Ok(flow) = mux.output_flow(terminal_id) else {
        return;
    };
    let flow = flow.attach();
    let mux = mux.clone();
    let token = token.to_string();
    let streams = links.streams.clone();
    let handle = tokio::spawn(async move {
        let socket = match streams.open(StreamKind::Terminal, &token).await {
            Ok(socket) => socket,
            Err(error) => {
                warn!(
                    terminal = terminal_id,
                    error = %error,
                    "could not open the terminal stream, this terminal has no output route"
                );
                return;
            }
        };
        let (mut sink, mut stream) = socket.split();
        for frame in pm_protocol::terminal_frame::replay_frames_with_flags(
            generation,
            &replay,
            pm_protocol::terminal_frame::FLAG_REPLAY_SNAPSHOT,
        ) {
            if sink.send(Message::Binary(frame)).await.is_err() {
                return;
            }
        }
        // Output goes out on its own task so a flood never holds up the
        // keystrokes arriving on the other side of the socket, and what
        // has queued while a send was in flight goes out as one frame.
        let outbound = async {
            loop {
                let mut chunk = match rx.recv().await {
                    Ok(chunk) => chunk,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                let mut gathered: Option<Vec<u8>> = None;
                while gathered.as_ref().map_or(chunk.len(), Vec::len) < OUTPUT_COALESCE_LIMIT_BYTES
                {
                    match rx.try_recv() {
                        Ok(more) => gathered
                            .get_or_insert_with(|| chunk.to_vec())
                            .extend_from_slice(&more),
                        Err(_) => break,
                    }
                }
                if let Some(gathered) = gathered {
                    chunk = bytes::Bytes::from(gathered);
                }
                pm_daemon::probe_trace::mark("w_stream_tx", &chunk);
                let sent = chunk.len();
                let frame = pm_protocol::terminal_frame::encode_output(generation, 0, &chunk);
                if sink.send(Message::Binary(frame)).await.is_err() {
                    break;
                }
                flow.release(sent);
            }
        };
        let inbound = async {
            loop {
                match stream.next().await {
                    Some(Ok(Message::Binary(frame))) => {
                        match pm_protocol::terminal_frame::decode(&frame) {
                            Some(pm_protocol::terminal_frame::TerminalFrame::Input {
                                generation: incoming,
                                data,
                                ..
                            }) if incoming == generation => {
                                pm_daemon::probe_trace::mark("w_stream_rx", data);
                                let _ = mux.input(terminal_id, bytes::Bytes::copy_from_slice(data));
                            }
                            Some(pm_protocol::terminal_frame::TerminalFrame::Resize {
                                generation: incoming,
                                cols,
                                rows,
                            }) if incoming == generation => {
                                let _ = mux.resize(terminal_id, cols, rows);
                            }
                            _ => break,
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    _ => break,
                }
            }
        };
        tokio::select! {
            _ = outbound => {}
            _ = inbound => {}
        }
        let _ = sink.close().await;
    });
    forwarders.lock().unwrap().insert(terminal_id, handle);
}

/// Kills a terminal and reports whether the controller needs an exit from
/// this call. A terminal the mux never had, or has already reported and
/// dropped, will produce no exit of its own, so the controller would keep
/// its session open forever.
fn kill_answers_unknown(mux: &Mux, terminal_id: u64) -> bool {
    match mux.kill(terminal_id) {
        Err(pm_daemon::mux::MuxError::NotRunning(_)) => !mux.contains(terminal_id),
        Err(error) => {
            warn!(terminal = terminal_id, error = %error, "killing a terminal failed");
            false
        }
        Ok(()) => false,
    }
}

fn stop_forwarding(session_id: u64, forwarders: &Forwarders) {
    if let Some(handle) = forwarders.lock().unwrap().remove(&session_id) {
        handle.abort();
    }
}

async fn relay_mux_events(
    mux: Arc<Mux>,
    mut channels: MuxChannels,
    out_slot: OutSlot,
    transcripts: Arc<WorkerTranscriptStore>,
    spawn_faults: SpawnFaults,
) {
    loop {
        tokio::select! {
            exit = channels.exit_rx.recv() => match exit {
                Some(exit) => {
                    let store = transcripts.clone();
                    let terminal_id = exit.terminal_id;
                    let generation = exit.generation;
                    let scrollback = exit.scrollback.clone();
                    let available = match tokio::task::spawn_blocking(move || {
                        store.persist(terminal_id, generation, &scrollback)
                    }).await {
                        Ok(Ok(())) => true,
                        Ok(Err(error)) => {
                            error!(terminal = terminal_id, error = %error, "failed to persist worker transcript");
                            false
                        }
                        Err(error) => {
                            error!(terminal = terminal_id, error = %error, "worker transcript task failed");
                            false
                        }
                    };
                    let fault = spawn_faults.lock().unwrap().remove(&exit.terminal_id);
                    send_via(&out_slot, control(WorkerMsg::TerminalExit {
                        terminal_id: exit.terminal_id,
                        generation: exit.generation,
                        exit_code: exit.exit_code,
                        state: exit_run_state(fault.as_deref()),
                        transcript_available: available,
                        transcript_size: if available { exit.scrollback.len() as u64 } else { 0 },
                        detail: fault.unwrap_or_default(),
                    }));
                    mux.remove(exit.terminal_id);
                }
                None => break,
            },
            needs = channels.needs_input_rx.recv() => match needs {
                Some(n) => {
                    send_via(&out_slot, control(WorkerMsg::NeedsInput {
                        session_id: n.session_id,
                    }));
                }
                None => break,
            },
            activity = channels.activity_rx.recv() => match activity {
                Some(activity) if activity.session_id != 0 => {
                    send_via(&out_slot, control(WorkerMsg::TerminalActivity {
                        terminal_id: activity.terminal_id,
                        generation: activity.generation,
                    }));
                }
                Some(_) => {}
                None => break,
            },
        }
    }
}

/// An agent this host stopped itself exited, but its session did not end
/// the way the controller should read a plain exit.
fn exit_run_state(fault: Option<&str>) -> pm_protocol::domain::TerminalRunState {
    match fault {
        Some(_) => pm_protocol::domain::TerminalRunState::Failed,
        None => pm_protocol::domain::TerminalRunState::Exited,
    }
}

/// Accepts `pm _hook` connections on the worker's local socket and
/// relays each hook up to the controller, replying so the hook client
/// does not block.
async fn relay_hooks(socket_path: PathBuf, out_slot: OutSlot, hook_relay: HookRelay) {
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            error!(error = %e, "failed to bind the worker hook socket, agent lifecycle hooks will not reach the controller");
            return;
        }
    };
    // The directory above it is already owner-only, which is the control the
    // daemon's own socket relies on. This narrows the socket too, so a later
    // widening of the directory does not hand the hook channel to every local
    // account at once.
    if let Err(e) = pm_daemon::fsperm::restrict_file(&socket_path) {
        warn!(error = %e, "could not restrict the worker hook socket");
    }
    serve_hook_clients(|| listener.accept(), out_slot, hook_relay).await;
}

/// A failed accept is one lost connection, never the end of the listener:
/// dropping it would leave the socket file behind and every later hook from
/// every session on this worker would be refused for the life of the process.
async fn serve_hook_clients<A, F>(mut accept: A, out_slot: OutSlot, hook_relay: HookRelay)
where
    A: FnMut() -> F,
    F: std::future::Future<Output = std::io::Result<(UnixStream, tokio::net::unix::SocketAddr)>>,
{
    loop {
        match accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle_hook_client(
                    stream,
                    out_slot.clone(),
                    hook_relay.clone(),
                ));
            }
            Err(e) => {
                warn!(error = %e, "hook socket accept failed, the listener stays up");
                if let Some(delay) = hook_accept_backoff(&e) {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

fn hook_accept_backoff(err: &std::io::Error) -> Option<Duration> {
    match err.raw_os_error() {
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM) => {
            Some(HOOK_ACCEPT_BACKOFF)
        }
        _ => None,
    }
}

async fn handle_hook_client(stream: UnixStream, out_slot: OutSlot, hook_relay: HookRelay) {
    let (mut read, mut write) = stream.into_split();
    while let Ok(Some(buf)) = frame::read_frame(&mut read).await {
        let Ok(envelope) = ClientEnvelope::decode(&buf) else {
            break;
        };
        let seq = envelope.seq;
        let mut nudge = bytes::Bytes::new();
        let mut undelivered: Option<&str> = None;
        if let ClientMsg::HookEvent {
            session_token,
            kind,
            detail,
            agent_session_id,
            transcript_path,
            background_work,
        } = envelope.msg
        {
            let (req_id, wait) = hook_relay.register();
            let queued = try_send_via(
                &out_slot,
                control(WorkerMsg::HookReport {
                    session_token,
                    kind,
                    detail,
                    agent_session_id,
                    transcript_path,
                    req_id,
                    background_work,
                }),
            );
            if !queued {
                hook_relay.forget(req_id);
                undelivered = Some("no control link to the controller");
            } else {
                // A controller that never answers — because it is older than
                // the reply, or gone — must cost the agent this deadline and
                // nothing more.
                match tokio::time::timeout(HOOK_RESULT_TIMEOUT, wait).await {
                    Ok(Ok(reason)) => nudge = bytes::Bytes::from(reason),
                    Ok(Err(_)) => {
                        undelivered = Some("the control link dropped before the hook was applied")
                    }
                    Err(_) => {
                        hook_relay.forget(req_id);
                        warn!(
                            kind = kind.as_str(),
                            "controller did not answer a relayed hook in time; \
                             the session may still read as mid-turn"
                        );
                    }
                }
            }
            if let Some(reason) = undelivered {
                warn!(
                    kind = kind.as_str(),
                    reason, "an agent lifecycle hook never reached the controller"
                );
            }
        }
        // Answering a hook the controller never applied with success hides
        // the loss from the agent, its transcript and the operator, and a
        // lost turn-end leaves the session reading as mid-turn.
        let reply = ServerMsg::CommandResult {
            seq,
            result: match undelivered {
                Some(reason) => Err(reason.to_string()),
                None => Ok(None),
            },
            data: nudge,
        };
        if frame::write_frame(&mut write, &reply.encode_to_vec())
            .await
            .is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{connection_fields, splice_link, LISTENING_CONTROLLER};
    use futures::{SinkExt, StreamExt};
    use pm_daemon::worker_plane::Keepalive;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::WebSocketStream;

    const PING: Duration = Duration::from_millis(20);
    const IDLE: Duration = Duration::from_millis(150);
    const QUICK: Keepalive = Keepalive::On {
        ping: PING,
        idle: IDLE,
    };

    /// A forward stream and the controller's end of it, over a pipe
    /// rather than a socket.
    async fn linked() -> (
        WebSocketStream<tokio::io::DuplexStream>,
        WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        (
            WebSocketStream::from_raw_socket(ours, Role::Client, None).await,
            WebSocketStream::from_raw_socket(theirs, Role::Server, None).await,
        )
    }

    /// The failure the keepalive is for: a controller whose machine went
    /// away holds a socket that delivers nothing and fails nothing, and
    /// the splice would wait on it forever.
    #[tokio::test]
    async fn a_forward_stream_whose_controller_stops_answering_is_closed_within_the_deadline() {
        let (link, silent) = linked().await;
        let (target, _target_peer) = tokio::io::duplex(1024);
        let started = tokio::time::Instant::now();
        let ended = tokio::time::timeout(IDLE * 4, splice_link(link, target, QUICK))
            .await
            .expect("the splice must end rather than wait on a controller that never answers");
        assert!(
            ended.is_err(),
            "a quiet controller is an error, not a clean close"
        );
        assert!(started.elapsed() >= IDLE);
        drop(silent);
    }

    /// An idle forward is not a dead one. The controller answers the
    /// pings this end sends, which is all the deadline asks of it, and
    /// the stream still carries bytes both ways afterwards.
    #[tokio::test]
    async fn an_idle_forward_stream_whose_controller_answers_stays_open_and_still_carries_bytes() {
        let (link, controller) = linked().await;
        let (target, mut target_peer) = tokio::io::duplex(1024);
        let (mut controller_write, mut controller_read) = controller.split();
        let (inbound_tx, mut inbound) = tokio::sync::mpsc::unbounded_channel();
        // Reading is what answers the pings: the controller says nothing of its own.
        let answering = tokio::spawn(async move {
            while let Some(Ok(message)) = controller_read.next().await {
                if let Message::Binary(data) = message {
                    let _ = inbound_tx.send(data);
                }
            }
        });
        let splice = tokio::spawn(splice_link(link, target, QUICK));
        tokio::time::sleep(IDLE * 3).await;
        assert!(!splice.is_finished(), "an idle stream was dropped");

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        target_peer.write_all(b"from the target").await.unwrap();
        let carried = tokio::time::timeout(IDLE, inbound.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&carried[..], b"from the target");
        controller_write
            .send(Message::Binary(bytes::Bytes::from_static(
                b"from the controller",
            )))
            .await
            .unwrap();
        let mut buf = vec![0u8; 32];
        let n = tokio::time::timeout(IDLE, target_peer.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"from the controller");

        splice.abort();
        answering.abort();
    }

    #[test]
    fn sandbox_profiles_default_to_unshifted_and_preserve_opt_in() {
        let old: SandboxProfile = serde_json::from_str("{}").unwrap();
        assert!(!old.incus_shifted_home);
        let opted_in = SandboxProfile {
            incus_shifted_home: true,
            ..Default::default()
        };
        let restored: SandboxProfile =
            serde_json::from_str(&serde_json::to_string(&opted_in).unwrap()).unwrap();
        assert!(restored.incus_shifted_home);
        assert!(
            crate::sandbox::args_from_profile("repos", restored, false, true).incus_shifted_home
        );
    }

    #[test]
    fn a_dialing_launch_records_the_controller_it_was_given() {
        let (controller, listen) =
            connection_fields(Some("wss://host:7677"), None, false, &[]).expect("recorded");
        assert_eq!(controller, "wss://host:7677");
        assert!(listen.is_none());
    }

    /// A listening worker has to come back on the address it was
    /// enrolled at instead of falling through to dialing.
    #[test]
    fn a_listening_launch_records_the_address_and_who_may_reach_it() {
        let addr = "10.30.2.35:7677".parse().unwrap();
        let (controller, listen) =
            connection_fields(None, Some(addr), true, &["10.30.4.0/24".to_string()])
                .expect("recorded");
        assert_eq!(controller, LISTENING_CONTROLLER);
        let listen = listen.expect("listening");
        assert_eq!(listen.addr, "10.30.2.35:7677");
        assert!(listen.allow_any);
        assert_eq!(listen.allow_from, vec!["10.30.4.0/24".to_string()]);
    }

    /// A launch that names neither leaves the profile alone, so a
    /// rebuild does not erase how the worker already connects.
    #[test]
    fn a_launch_that_names_neither_records_nothing() {
        assert!(connection_fields(None, None, false, &[]).is_none());
    }

    use super::*;

    /// A stand-in for an agent listening on its own inbox socket. It
    /// reads whatever arrives and reports it, which is all the delivery
    /// needs to count as taken.
    fn accepting_inbox(path: &std::path::Path) -> tokio::sync::oneshot::Receiver<Vec<u8>> {
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf).await;
            let _ = tx.send(buf);
        });
        rx
    }

    #[tokio::test]
    async fn an_agent_with_no_channel_is_reported_rather_than_retried() {
        let msg = deliver_to_agent_inbox(7, 42, None, "a notice", AgentInboxMode::Queue).await;
        match msg {
            WorkerMsg::AgentInboxResult {
                req_id,
                outcome,
                transport,
                detail,
                ..
            } => {
                assert_eq!(req_id, 7);
                assert_eq!(outcome, AgentInboxOutcome::NoChannel);
                assert!(transport.is_empty());
                assert!(!detail.contains("a notice"));
            }
            other => panic!("expected an inbox result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_delivered_message_names_the_transport_that_carried_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.sock");
        let received = accepting_inbox(&path);
        let channel = pm_adapters::InboundChannel::ClaudeSocket {
            path: path.clone(),
            token: None,
            // The test holds the socket, so its own pid is the peer the
            // delivery must find.
            expect_pid: std::process::id(),
        };
        let msg = deliver_to_agent_inbox(
            1,
            42,
            Some(channel),
            "two of your sessions are parked",
            AgentInboxMode::Queue,
        )
        .await;
        match msg {
            WorkerMsg::AgentInboxResult {
                outcome,
                transport,
                mode,
                detail,
                ..
            } => {
                assert_eq!(outcome, AgentInboxOutcome::Delivered);
                assert_eq!(transport, "claude-socket");
                assert_eq!(mode, AgentInboxMode::Queue);
                assert!(detail.is_empty());
            }
            other => panic!("expected an inbox result, got {other:?}"),
        }
        let wire = String::from_utf8(received.await.unwrap()).unwrap();
        assert!(wire.contains("two of your sessions are parked"));
    }

    /// A refused delivery is the controller's cue to write to the
    /// terminal, so the reason travels back. The message itself never
    /// does: the controller already has it, and nothing downstream
    /// should be able to log it from here.
    #[tokio::test]
    async fn an_unreachable_agent_is_reported_without_repeating_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let channel = pm_adapters::InboundChannel::ClaudeSocket {
            path: dir.path().join("nobody-is-listening.sock"),
            token: None,
            expect_pid: std::process::id(),
        };
        let secret = "the notice nobody should echo";
        let msg = deliver_to_agent_inbox(2, 42, Some(channel), secret, AgentInboxMode::Queue).await;
        match msg {
            WorkerMsg::AgentInboxResult {
                outcome,
                transport,
                detail,
                ..
            } => {
                assert_eq!(outcome, AgentInboxOutcome::Failed);
                assert_eq!(transport, "claude-socket");
                assert!(!detail.is_empty(), "the controller is told why");
                assert!(!detail.contains(secret));
            }
            other => panic!("expected an inbox result, got {other:?}"),
        }
    }

    async fn relay_endpoint() -> (McpRelayEndpoint, mpsc::Receiver<Message>) {
        let (tx, rx) = mpsc::channel(8);
        let out_slot: OutSlot = Arc::new(Mutex::new(Some(tx)));
        (McpRelayEndpoint::bind(&out_slot).await.unwrap(), rx)
    }

    fn dialed_streams() -> Streams {
        let identity = pm_tls::Identity::generate().unwrap();
        Streams::Dial {
            dialer: StreamDialer {
                controller_key: identity.key_hash(),
                identity,
            },
            stream_url: "wss://controller.internal:7677/worker/stream".into(),
            terminal_url: "wss://controller.internal:7677/worker/terminal".into(),
            transcript_url: "wss://controller.internal:7677/worker/transcript".into(),
        }
    }

    fn is_loopback_relay(url: &str) -> bool {
        url.starts_with("http://127.0.0.1:") && url.ends_with("/mcp")
    }

    /// A host that dialed the controller may still have no route to its
    /// browser plane, so it hands agents the same local relay a host the
    /// controller dialed does.
    #[tokio::test]
    async fn agents_get_the_local_relay_whichever_end_opened_the_link() {
        let (endpoint, _rx) = relay_endpoint().await;
        let hook_relay = HookRelay::default();
        let dialed = controller_links(&endpoint, &hook_relay, dialed_streams());
        let accepted = controller_links(
            &endpoint,
            &hook_relay,
            Streams::Accept(crate::worker_listener::PendingStreams::default()),
        );
        assert!(is_loopback_relay(&dialed.mcp_url), "{}", dialed.mcp_url);
        assert_eq!(dialed.mcp_url, accepted.mcp_url);
        assert_eq!(dialed.mcp_url, endpoint.url);
    }

    async fn holding(streams: &crate::worker_listener::PendingStreams, token: &str) {
        for _ in 0..2000 {
            if streams.holds(token) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("the request for {token} was never announced");
    }

    /// The controller announces an attach only when it has no stream for the
    /// session, so a forwarder still running here is writing into a socket it
    /// has let go. Terminal streams carry no keepalive and a half-open one
    /// never ends, so the attach retires it instead of being ignored, which
    /// would strand the session for as long as the worker ran.
    #[tokio::test]
    async fn a_fresh_attach_retires_the_forwarder_the_controller_let_go() {
        let (endpoint, _rx) = relay_endpoint().await;
        let hook_relay = HookRelay::default();
        let streams = crate::worker_listener::PendingStreams::default();
        let links = controller_links(&endpoint, &hook_relay, Streams::Accept(streams.clone()));
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);
        let spec = pm_adapters::CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(7, 1, 7, &spec, false, false, false, None)
            .unwrap();
        let forwarders: Forwarders = Arc::new(Mutex::new(HashMap::new()));

        start_forwarding(&mux, &links, 7, 1, "token-one", 0, &forwarders);
        holding(&streams, "token-one").await;
        start_forwarding(&mux, &links, 7, 1, "token-two", 0, &forwarders);
        holding(&streams, "token-two").await;

        assert!(
            !streams.holds("token-one"),
            "the retired forwarder left its request behind"
        );
        assert_eq!(forwarders.lock().unwrap().len(), 1);
        let _ = mux.kill(7);
    }

    /// Runs one controller command through the worker's dispatcher and
    /// returns what it sent back up the control link.
    async fn dispatch(mux: &Arc<Mux>, command: ControllerMsg) -> Vec<WorkerMsg> {
        dispatch_with_shares(mux, command, &Arc::new(Mutex::new(HashMap::new()))).await
    }

    async fn dispatch_with_shares(
        mux: &Arc<Mux>,
        command: ControllerMsg,
        dir_shares: &DirShares,
    ) -> Vec<WorkerMsg> {
        let (endpoint, _relay_rx) = relay_endpoint().await;
        let hook_relay = HookRelay::default();
        let links = controller_links(&endpoint, &hook_relay, dialed_streams());
        let dir = tempfile::tempdir().unwrap();
        let transcripts =
            Arc::new(WorkerTranscriptStore::new(dir.path(), "wss://controller.internal").unwrap());
        let forwarders: Forwarders = Arc::new(Mutex::new(HashMap::new()));
        let spawn_faults: SpawnFaults = Arc::new(Mutex::new(HashMap::new()));
        let pending_update = crate::worker_update::PendingUpdate::default();
        let (out_tx, mut out_rx) = mpsc::channel(8);
        handle_command(
            &AdapterRegistry::standard(),
            mux,
            &out_tx,
            &dir.path().join("hook.sock"),
            dir.path(),
            std::path::Path::new("/usr/bin/pm"),
            &links,
            &transcripts,
            &forwarders,
            dir_shares,
            &pending_update,
            &spawn_faults,
            command,
        )
        .await;
        drop(out_tx);
        let mut sent = Vec::new();
        while let Some(Message::Binary(buf)) = out_rx.recv().await {
            let Some(WorkerFrame::Control(payload)) = worker_frame::decode(&buf) else {
                panic!("expected a control frame");
            };
            sent.push(WorkerMsg::decode(payload).unwrap());
        }
        sent
    }

    fn sleeping_terminal(mux: &Mux, terminal_id: u64) {
        let spec = pm_adapters::CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(
            terminal_id,
            1,
            terminal_id,
            &spec,
            false,
            false,
            false,
            None,
        )
        .unwrap();
    }

    /// The controller parses the attach snapshot into a model of the size it
    /// asked for, so the PTY must already be that size when it is taken.
    #[tokio::test]
    async fn an_attach_sizes_the_pty_before_the_snapshot_and_zero_keeps_it() {
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);
        sleeping_terminal(&mux, 9);
        let attach = |size| ControllerMsg::TerminalAttach {
            terminal_id: 9,
            generation: 1,
            token: "attach-token".into(),
            replay_bytes: 0,
            size,
        };

        dispatch(&mux, attach((150, 30))).await;
        assert_eq!(mux.current_size(9).unwrap(), (150, 30));
        dispatch(&mux, attach((0, 0))).await;
        assert_eq!(mux.current_size(9).unwrap(), (150, 30));
        let _ = mux.kill(9);
    }

    /// After a restart the worker has no record of a terminal the controller
    /// still thinks is live, and without an answer that session could never
    /// be closed.
    #[tokio::test]
    async fn a_kill_for_a_terminal_this_host_does_not_have_is_answered_with_an_exit() {
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);

        let sent = dispatch(
            &mux,
            ControllerMsg::TerminalKill {
                terminal_id: 41,
                generation: 3,
            },
        )
        .await;
        assert!(
            matches!(
                sent.as_slice(),
                [WorkerMsg::TerminalExit {
                    terminal_id: 41,
                    generation: 3,
                    exit_code: None,
                    state: pm_protocol::domain::TerminalRunState::Exited,
                    transcript_available: false,
                    ..
                }]
            ),
            "unexpected reply {sent:?}"
        );

        let sent = dispatch(&mux, ControllerMsg::Kill { session_id: 42 }).await;
        assert!(
            matches!(
                sent.as_slice(),
                [WorkerMsg::SessionExit {
                    session_id: 42,
                    exit_code: None,
                }]
            ),
            "unexpected reply {sent:?}"
        );
    }

    #[tokio::test]
    async fn serving_a_directory_answers_with_the_port_it_bound() {
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("out")).unwrap();
        std::fs::write(root.path().join("out/report.html"), "<p>hi</p>").unwrap();
        let shares: DirShares = Arc::new(Mutex::new(HashMap::new()));

        let sent = dispatch_with_shares(
            &mux,
            ControllerMsg::DirShareServe {
                share_id: 3,
                root: root.path().to_string_lossy().into_owned(),
                path: "out".into(),
            },
            &shares,
        )
        .await;

        let [WorkerMsg::DirShareBound {
            share_id: 3,
            ok: true,
            port,
            ..
        }] = sent.as_slice()
        else {
            panic!("unexpected reply {sent:?}");
        };
        assert_ne!(*port, 0);
        assert_eq!(shares.lock().unwrap().len(), 1);

        let body = reqwest::get(format!("http://127.0.0.1:{port}/report.html"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "<p>hi</p>");

        dispatch_with_shares(&mux, ControllerMsg::DirShareStop { share_id: 3 }, &shares).await;
        assert!(shares.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn serving_refuses_a_directory_outside_the_session_root() {
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);
        let root = tempfile::tempdir().unwrap();
        let shares: DirShares = Arc::new(Mutex::new(HashMap::new()));

        let sent = dispatch_with_shares(
            &mux,
            ControllerMsg::DirShareServe {
                share_id: 4,
                root: root.path().to_string_lossy().into_owned(),
                path: "../escape".into(),
            },
            &shares,
        )
        .await;

        let [WorkerMsg::DirShareBound {
            ok: false, error, ..
        }] = sent.as_slice()
        else {
            panic!("unexpected reply {sent:?}");
        };
        assert!(
            error.contains("inside the session working directory"),
            "{error}"
        );
        assert!(shares.lock().unwrap().is_empty());
    }

    /// Two serve requests for one share arrive whenever a restart runs
    /// both of the controller's share-restore paths, and the second must
    /// answer on the port the first bound. The controller may already
    /// have recorded that port as the share's target.
    #[tokio::test]
    async fn serving_a_share_twice_answers_on_the_port_it_already_holds() {
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("out")).unwrap();
        std::fs::write(root.path().join("out/report.html"), "<p>hi</p>").unwrap();
        let shares: DirShares = Arc::new(Mutex::new(HashMap::new()));
        let serve = || ControllerMsg::DirShareServe {
            share_id: 7,
            root: root.path().to_string_lossy().into_owned(),
            path: "out".into(),
        };
        let answered = |sent: Vec<WorkerMsg>| match sent.as_slice() {
            [WorkerMsg::DirShareBound {
                share_id: 7,
                ok: true,
                port,
                ..
            }] => *port,
            other => panic!("unexpected reply {other:?}"),
        };

        let first = answered(dispatch_with_shares(&mux, serve(), &shares).await);
        let second = answered(dispatch_with_shares(&mux, serve(), &shares).await);

        assert_ne!(first, 0);
        assert_eq!(second, first, "a share keeps the port it is serving on");
        assert_eq!(shares.lock().unwrap().len(), 1);
        let body = reqwest::get(format!("http://127.0.0.1:{first}/report.html"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(
            body, "<p>hi</p>",
            "the port the controller dials still answers"
        );
    }

    /// The case a look before binding cannot catch: two binds for one
    /// share finish at once. The one that arrives second is dropped
    /// rather than filed, because filing it would abort the server the
    /// first already answered with.
    #[tokio::test]
    async fn a_second_server_for_a_share_does_not_displace_the_first() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("report.html"), "<p>hi</p>").unwrap();
        let shares: DirShares = Arc::new(Mutex::new(HashMap::new()));
        let first = pm_daemon::dir_server::serve(root.path().to_path_buf())
            .await
            .unwrap();
        let second = pm_daemon::dir_server::serve(root.path().to_path_buf())
            .await
            .unwrap();
        let (first_port, second_port) = (first.port(), second.port());
        assert_ne!(first_port, second_port);

        assert_eq!(keep_dir_share(&shares, 9, first), first_port);
        assert_eq!(keep_dir_share(&shares, 9, second), first_port);

        assert_eq!(shares.lock().unwrap().len(), 1);
        assert_eq!(serving_port(&shares, 9), Some(first_port));
        let body = reqwest::get(format!("http://127.0.0.1:{first_port}/report.html"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "<p>hi</p>");
    }

    /// Stopping a share the worker never had is not an error: the
    /// controller sends it to clear servers it does not recognize.
    #[tokio::test]
    async fn stopping_an_unknown_share_is_quiet() {
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);
        let shares: DirShares = Arc::new(Mutex::new(HashMap::new()));
        let sent =
            dispatch_with_shares(&mux, ControllerMsg::DirShareStop { share_id: 99 }, &shares).await;
        assert!(sent.is_empty(), "unexpected reply {sent:?}");
    }

    /// A terminal that is still dying reports its own exit once it has gone,
    /// so the kill itself must not answer for it.
    #[tokio::test]
    async fn a_kill_for_a_live_terminal_leaves_the_exit_to_the_terminal() {
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);
        sleeping_terminal(&mux, 7);
        sleeping_terminal(&mux, 8);

        let sent = dispatch(
            &mux,
            ControllerMsg::TerminalKill {
                terminal_id: 7,
                generation: 1,
            },
        )
        .await;
        assert!(sent.is_empty(), "unexpected reply {sent:?}");
        let sent = dispatch(&mux, ControllerMsg::Kill { session_id: 8 }).await;
        assert!(sent.is_empty(), "unexpected reply {sent:?}");
    }

    /// Between a terminal exiting and the relay reporting it, the mux still
    /// holds the entry, and the relay's exit is the one that carries the code.
    #[tokio::test]
    async fn a_kill_racing_an_unreported_exit_leaves_the_exit_to_the_relay() {
        let (mux, mut channels) = Mux::new();
        let mux = Arc::new(mux);
        let spec = pm_adapters::CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 3".into()],
            env: Vec::new(),
            cwd: std::env::temp_dir(),
        };
        mux.spawn(9, 1, 9, &spec, false, false, false, None)
            .unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(10), channels.exit_rx.recv())
            .await
            .expect("the terminal exits")
            .unwrap();
        assert_eq!(exit.terminal_id, 9);

        let sent = dispatch(
            &mux,
            ControllerMsg::TerminalKill {
                terminal_id: 9,
                generation: 1,
            },
        )
        .await;
        assert!(sent.is_empty(), "unexpected reply {sent:?}");
    }

    /// Filesystem requests run off the control task, so their answers come
    /// from a separate task and must still reach the link.
    #[tokio::test]
    async fn filesystem_requests_are_answered_from_off_the_control_task() {
        let (mux, _channels) = Mux::new();
        let mux = Arc::new(mux);
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("notes.txt"), b"hello").unwrap();
        let root_path = root.path().to_string_lossy().to_string();

        let sent = dispatch(
            &mux,
            ControllerMsg::FsList {
                req_id: 1,
                path: root_path.clone(),
            },
        )
        .await;
        match sent.as_slice() {
            [WorkerMsg::FsListing {
                req_id: 1,
                ok: true,
                entries,
                ..
            }] => assert_eq!(
                entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
                ["src"]
            ),
            other => panic!("unexpected reply {other:?}"),
        }

        let sent = dispatch(
            &mux,
            ControllerMsg::PathCheck {
                req_id: 2,
                path: root_path.clone(),
            },
        )
        .await;
        assert!(
            matches!(
                sent.as_slice(),
                [WorkerMsg::PathChecked {
                    req_id: 2,
                    status: pm_protocol::domain::PathCheck::Ok,
                    ..
                }]
            ),
            "unexpected reply {sent:?}"
        );

        let sent = dispatch(
            &mux,
            ControllerMsg::FileRead {
                req_id: 3,
                root: root_path,
                path: "notes.txt".into(),
                max_bytes: 1024,
            },
        )
        .await;
        match sent.as_slice() {
            [WorkerMsg::FileRead {
                req_id: 3,
                ok: true,
                content,
                ..
            }] => assert_eq!(content, b"hello"),
            other => panic!("unexpected reply {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_adapter_is_handed_the_relay_url() {
        let (endpoint, _rx) = relay_endpoint().await;
        let hook_relay = HookRelay::default();
        let links = controller_links(&endpoint, &hook_relay, dialed_streams());
        let ctx = spawn_ctx(
            std::path::Path::new("/run/hook.sock"),
            std::path::Path::new("/run/files"),
            std::path::Path::new("/usr/bin/pm"),
            &links.mcp_url,
            7,
            "do the thing",
            pm_protocol::domain::PermissionMode::Default,
            "/work",
            "session-token",
            "",
            None,
            false,
        );
        assert_eq!(
            ctx.integration.mcp_url.as_deref(),
            Some(links.mcp_url.as_str())
        );
        assert_eq!(ctx.integration.session_token, "session-token");
    }

    /// What the agent posts to the relay reaches the control link as a
    /// request and the controller's answer comes back as the HTTP reply.
    #[tokio::test]
    async fn a_post_to_the_relay_is_carried_up_the_control_link() {
        let (endpoint, mut rx) = relay_endpoint().await;
        let hook_relay = HookRelay::default();
        let links = controller_links(&endpoint, &hook_relay, dialed_streams());
        let url = links.mcp_url.clone();
        let posted = tokio::spawn(async move { list_agent_tools(&url, "session-token").await });
        let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the request reaches the link")
            .unwrap();
        let Message::Binary(buf) = frame else {
            panic!("expected a control frame, got {frame:?}");
        };
        let Some(WorkerFrame::Control(payload)) = worker_frame::decode(&buf) else {
            panic!("expected a control frame");
        };
        let WorkerMsg::McpRequest {
            req_id,
            bearer,
            body,
        } = WorkerMsg::decode(payload).unwrap()
        else {
            panic!("expected a relayed MCP request");
        };
        assert_eq!(bearer, "session-token");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["method"],
            "tools/list"
        );
        links.mcp_relay.resolve(
            req_id,
            200,
            r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"report"}]}}"#.into(),
        );
        assert_eq!(posted.await.unwrap().unwrap(), vec!["report".to_string()]);
    }

    async fn stub_mcp(status: axum::http::StatusCode, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/mcp",
            axum::routing::post(move || async move {
                (
                    status,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/mcp")
    }

    #[tokio::test]
    async fn the_probe_reads_the_tool_list() {
        let url = stub_mcp(
            axum::http::StatusCode::OK,
            r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"report"},{"name":"flag_blocked"}]}}"#,
        )
        .await;
        let tools = list_agent_tools(&url, "tok").await.unwrap();
        assert_eq!(tools, ["report", "flag_blocked"]);
        assert!(require_agent_tool(&tools).is_ok());
    }

    #[tokio::test]
    async fn the_probe_fails_on_a_refused_endpoint_or_an_error_reply() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let unreachable = format!("http://{}/mcp", listener.local_addr().unwrap());
        drop(listener);
        assert!(list_agent_tools(&unreachable, "tok").await.is_err());

        let gateway = stub_mcp(axum::http::StatusCode::BAD_GATEWAY, "").await;
        assert!(list_agent_tools(&gateway, "tok").await.is_err());

        let refused = stub_mcp(
            axum::http::StatusCode::OK,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}}"#,
        )
        .await;
        assert!(list_agent_tools(&refused, "tok").await.is_err());
    }

    #[test]
    fn a_tool_list_without_report_is_not_a_tool_surface() {
        assert!(require_agent_tool(&["publish_port".to_string()]).is_err());
        assert!(require_agent_tool(&[]).is_err());
    }

    #[test]
    fn an_agent_the_host_stopped_exits_as_failed() {
        assert_eq!(
            exit_run_state(Some("no tools")),
            pm_protocol::domain::TerminalRunState::Failed
        );
        assert_eq!(
            exit_run_state(None),
            pm_protocol::domain::TerminalRunState::Exited
        );
    }

    #[tokio::test]
    async fn a_hook_waiter_receives_the_controllers_answer() {
        let relay = HookRelay::default();
        let (req_id, wait) = relay.register();
        assert!(req_id > 0, "zero means no reply is wanted");
        relay.resolve(req_id, "set a headline".into());
        assert_eq!(wait.await.unwrap(), "set a headline");
    }

    async fn hook_reply(out_slot: OutSlot, relay: HookRelay) -> ServerMsg {
        let (client, server) = UnixStream::pair().unwrap();
        tokio::spawn(handle_hook_client(server, out_slot, relay));
        hook_reply_over(client).await
    }

    async fn hook_reply_over(client: UnixStream) -> ServerMsg {
        let envelope = ClientEnvelope {
            seq: 7,
            msg: ClientMsg::HookEvent {
                session_token: "token".into(),
                kind: pm_protocol::domain::HookKind::TurnEnded,
                detail: String::new(),
                agent_session_id: String::new(),
                transcript_path: String::new(),
                background_work: false,
            },
        };
        let (mut read, mut write) = client.into_split();
        frame::write_frame(&mut write, &envelope.encode_to_vec())
            .await
            .unwrap();
        let buf = frame::read_frame(&mut read).await.unwrap().unwrap();
        ServerMsg::decode(&buf).unwrap()
    }

    /// One failed accept used to end the relay for the life of the worker,
    /// while the socket file stayed behind and refused every hook after it.
    #[tokio::test]
    async fn the_hook_listener_outlives_a_failed_accept() {
        let (client, server) = UnixStream::pair().unwrap();
        let attempts = Arc::new(Mutex::new(std::collections::VecDeque::from([
            Err(std::io::Error::from_raw_os_error(libc::EMFILE)),
            Ok(server),
        ])));
        let accept = move || {
            let attempts = attempts.clone();
            async move {
                let next = attempts.lock().unwrap().pop_front();
                match next {
                    Some(Ok(stream)) => {
                        let addr = stream.local_addr().unwrap();
                        Ok((stream, addr))
                    }
                    Some(Err(e)) => Err(e),
                    None => std::future::pending().await,
                }
            }
        };
        let out_slot: OutSlot = Arc::new(Mutex::new(None));
        tokio::spawn(serve_hook_clients(accept, out_slot, HookRelay::default()));
        let reply = hook_reply_over(client).await;
        assert!(
            matches!(reply, ServerMsg::CommandResult { seq: 7, .. }),
            "the client accepted after the failure was served: {reply:?}"
        );
    }

    #[test]
    fn only_resource_exhaustion_backs_the_accept_loop_off() {
        for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
            let err = std::io::Error::from_raw_os_error(code);
            assert_eq!(
                hook_accept_backoff(&err),
                Some(HOOK_ACCEPT_BACKOFF),
                "{err}"
            );
        }
        for err in [
            std::io::Error::from_raw_os_error(libc::ECONNABORTED),
            std::io::Error::other("not an os error"),
        ] {
            assert_eq!(hook_accept_backoff(&err), None, "{err}");
        }
    }

    /// Answering a hook the controller never got with success is how a lost
    /// turn end becomes invisible: the agent records a clean hook, its
    /// transcript shows no error, and the session reads as mid-turn for as
    /// long as nobody types into it.
    #[tokio::test]
    async fn a_hook_that_never_reached_the_controller_is_not_reported_as_delivered() {
        let out_slot: OutSlot = Arc::new(Mutex::new(None));
        let reply = hook_reply(out_slot, HookRelay::default()).await;
        let ServerMsg::CommandResult { seq, result, .. } = reply else {
            panic!("expected a command result, got {reply:?}");
        };
        assert_eq!(seq, 7);
        assert!(result.is_err(), "a hook with no link to carry it failed");
    }

    /// The same is true of a link that drops between the relay and the
    /// answer: the controller never applied the hook.
    #[tokio::test]
    async fn a_hook_whose_link_drops_before_the_answer_is_not_reported_as_delivered() {
        let (tx, mut rx) = mpsc::channel(8);
        let out_slot: OutSlot = Arc::new(Mutex::new(Some(tx)));
        let relay = HookRelay::default();
        let abandoning = relay.clone();
        tokio::spawn(async move {
            let _ = rx.recv().await;
            abandoning.abandon();
        });
        let reply = hook_reply(out_slot, relay).await;
        let ServerMsg::CommandResult { result, .. } = reply else {
            panic!("expected a command result, got {reply:?}");
        };
        assert!(result.is_err(), "the hook died with the link");
    }

    /// And a hook the controller does answer still reports success, with
    /// the nudge it sent back.
    #[tokio::test]
    async fn an_answered_hook_still_reports_success_and_carries_the_nudge() {
        let (tx, mut rx) = mpsc::channel(8);
        let out_slot: OutSlot = Arc::new(Mutex::new(Some(tx)));
        let relay = HookRelay::default();
        let answering = relay.clone();
        tokio::spawn(async move {
            let _ = rx.recv().await;
            answering.resolve(1, "set a headline".into());
        });
        let reply = hook_reply(out_slot, relay).await;
        let ServerMsg::CommandResult { result, data, .. } = reply else {
            panic!("expected a command result, got {reply:?}");
        };
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(data.as_ref(), b"set a headline");
    }

    /// A controller that is older than the reply, or gone, must cost the
    /// agent its deadline and nothing more.
    #[tokio::test]
    async fn an_unanswered_hook_waiter_is_dropped_when_the_link_goes() {
        let relay = HookRelay::default();
        let (_req_id, wait) = relay.register();
        relay.abandon();
        assert!(wait.await.is_err());
    }

    #[tokio::test]
    async fn an_answer_for_an_unknown_request_is_ignored() {
        let relay = HookRelay::default();
        let (req_id, wait) = relay.register();
        relay.forget(req_id);
        relay.resolve(req_id, "too late".into());
        assert!(wait.await.is_err());
    }

    /// The failure this deadline exists for: a controller whose machine went
    /// away leaves a socket that delivers nothing and fails nothing.
    #[tokio::test]
    async fn a_controller_that_goes_quiet_ends_the_link() {
        let mut quiet = futures::stream::pending::<Result<Message, String>>();
        let next = next_control(&mut quiet, Some(Duration::from_millis(20))).await;
        assert!(matches!(next, Next::Quiet));
    }

    #[tokio::test]
    async fn a_link_without_a_deadline_waits_for_its_controller() {
        let mut quiet = futures::stream::pending::<Result<Message, String>>();
        let waited =
            tokio::time::timeout(Duration::from_millis(50), next_control(&mut quiet, None)).await;
        assert!(waited.is_err());
    }

    #[tokio::test]
    async fn keepalives_are_not_commands_and_a_close_ends_the_link() {
        let mut link = futures::stream::iter(vec![
            Ok::<_, String>(Message::Pong(bytes::Bytes::new())),
            Ok(Message::Binary(bytes::Bytes::from_static(b"frame"))),
            Ok(Message::Close(None)),
        ]);
        let idle = Some(Duration::from_secs(5));
        assert!(matches!(next_control(&mut link, idle).await, Next::Alive));
        assert!(matches!(
            next_control(&mut link, idle).await,
            Next::Frame(buf) if buf == "frame"
        ));
        assert!(matches!(next_control(&mut link, idle).await, Next::Closed));
    }

    /// The three ways a control link ends have to stay apart, because
    /// they are the whole account a host gets of why it is back at the
    /// accept loop: a controller that closed, one whose socket failed,
    /// and one that stopped answering are three different problems.
    #[tokio::test]
    async fn every_way_a_control_link_ends_names_itself() {
        let idle = Some(Duration::from_secs(5));

        let mut closed = futures::stream::iter(vec![Ok::<_, String>(Message::Close(None))]);
        assert_eq!(next_control(&mut closed, idle).await, Next::Closed);

        let mut gone = futures::stream::empty::<Result<Message, String>>();
        assert_eq!(next_control(&mut gone, idle).await, Next::Closed);

        let mut broken =
            futures::stream::iter(vec![Err::<Message, _>("connection reset".to_string())]);
        assert_eq!(
            next_control(&mut broken, idle).await,
            Next::Failed("connection reset".to_string()),
            "a socket failure has to carry its reason, not just end the link"
        );

        let mut quiet = futures::stream::pending::<Result<Message, String>>();
        assert_eq!(
            next_control(&mut quiet, Some(Duration::from_millis(20))).await,
            Next::Quiet
        );
    }

    /// A keepalive is proof the controller is alive, and telling it apart
    /// from a frame this host merely ignores is what makes link health
    /// observable at all.
    #[tokio::test]
    async fn a_keepalive_is_distinguishable_from_a_frame_the_host_ignores() {
        let idle = Some(Duration::from_secs(5));
        let mut link = futures::stream::iter(vec![
            Ok::<_, String>(Message::Ping(bytes::Bytes::new())),
            Ok(Message::Pong(bytes::Bytes::new())),
            Ok(Message::Text("not a frame".into())),
        ]);
        assert_eq!(next_control(&mut link, idle).await, Next::Alive);
        assert_eq!(next_control(&mut link, idle).await, Next::Alive);
        assert_eq!(next_control(&mut link, idle).await, Next::Ignore);
    }

    #[test]
    fn worker_url_maps_schemes_and_appends_path() {
        assert_eq!(worker_ws_url("wss://host:7676"), "wss://host:7676/worker");
        assert_eq!(
            worker_ws_url("https://host:7676/"),
            "wss://host:7676/worker"
        );
        assert_eq!(worker_ws_url("wss://h/"), "wss://h/worker");
    }

    /// A plaintext URL is carried through rather than quietly upgraded, so
    /// the operator gets an explicit error at dial time instead of a config
    /// that looks migrated but points at the wrong plane.
    #[test]
    fn a_legacy_plaintext_controller_url_is_left_as_written() {
        let mut cfg: WorkerConfig = toml::from_str("controller = \"http://host:7676\"\n").unwrap();
        cfg.migrate_legacy();
        assert_eq!(
            cfg.profile(None).default_controller.as_deref(),
            Some("http://host:7676")
        );
    }

    #[test]
    fn normalize_controller_maps_schemes_and_trims_slash() {
        assert_eq!(normalize_controller("https://host"), "wss://host");
        assert_eq!(normalize_controller("wss://host:7676/"), "wss://host:7676");
        assert_eq!(normalize_controller("wss://host:7676"), "wss://host:7676");
    }

    /// A pin is what stops a stranger answering for the controller, so a
    /// key change is refused on its own. Only an operator passing a token
    /// reopens pairing, and the proof still has to succeed.
    #[test]
    fn only_an_operator_supplied_token_reopens_pairing() {
        let pinned = pm_tls::KeyHash::from_hex(&"ab".repeat(32));
        assert!(pinned.is_some(), "test key should parse");
        let refused = crate::worker_link::DialError::for_test(true);

        assert!(should_repair_pin(pinned, Some("fresh-token"), &refused));
        assert!(
            !should_repair_pin(pinned, None, &refused),
            "a key change alone is refused"
        );
        assert!(
            !should_repair_pin(None, Some("fresh-token"), &refused),
            "nothing to repair when this host pinned nothing"
        );
    }

    /// Re-pairing spends a single-use enrollment. A controller that was
    /// restarting, or unreachable, still holds the key this host pinned, so
    /// reading that as a rebuilt controller would strand the host with a
    /// token the controller has already burned.
    #[test]
    fn a_controller_that_never_answered_does_not_reopen_pairing() {
        let pinned = pm_tls::KeyHash::from_hex(&"ab".repeat(32));
        let unreachable = crate::worker_link::DialError::for_test(false);
        assert!(!should_repair_pin(
            pinned,
            Some("fresh-token"),
            &unreachable
        ));
    }

    /// Clearing only the pin would get through the handshake and then fail
    /// to enroll, because a surviving credential suppresses the token the
    /// retry needs.
    #[test]
    fn re_enrolling_forgets_the_credential_as_well_as_the_pin() {
        let mut config = WorkerProfile::default();
        config.controllers.insert(
            "wss://controller:7676".into(),
            ControllerCreds {
                credential: Some("durable-credential".into()),
                controller_key: Some("ab".repeat(32)),
                worker_id: Some(7),
            },
        );

        clear_for_reenrollment(&mut config, "wss://controller:7676");

        let entry = &config.controllers["wss://controller:7676"];
        assert_eq!(entry.controller_key, None);
        assert_eq!(entry.credential, None);
        assert_eq!(
            entry.enrollment_token(Some("fresh-token"), false),
            Some("fresh-token"),
            "the token has to become registration material again"
        );
    }

    fn cidr(text: &str) -> crate::worker_listener::Cidr {
        crate::worker_listener::Cidr::parse(text).unwrap()
    }

    /// The regression: a host enrolled with --listen saved its credential
    /// but nothing recorded that it listens, so a bare `pm worker` fell
    /// through to dialing and complained it had no controller.
    #[test]
    fn a_listening_host_comes_back_listening_without_flags() {
        let config = WorkerProfile {
            default_controller: Some(LISTENING_CONTROLLER.into()),
            listen: Some(ListenSettings {
                addr: "0.0.0.0:7677".into(),
                allow_any: true,
                allow_from: vec!["10.0.0.0/8".into()],
            }),
            ..WorkerProfile::default()
        };
        assert_eq!(
            resolve_start_mode(None, false, &[], None, &config).unwrap(),
            StartMode::Listen {
                addr: "0.0.0.0:7677".parse().unwrap(),
                allow_any: true,
                allow_from: vec![cidr("10.0.0.0/8")],
            }
        );
    }

    #[test]
    fn a_dialing_host_comes_back_to_its_controller_without_flags() {
        let config = WorkerProfile {
            default_controller: Some("wss://host:7677".into()),
            ..WorkerProfile::default()
        };
        assert_eq!(
            resolve_start_mode(None, false, &[], None, &config).unwrap(),
            StartMode::Dial("wss://host:7677".into())
        );
    }

    /// Flags are how an operator changes their mind, so they win over
    /// whatever the last run saved, in both directions.
    #[test]
    fn flags_override_what_was_saved() {
        let listening = WorkerProfile {
            default_controller: Some(LISTENING_CONTROLLER.into()),
            listen: Some(ListenSettings {
                addr: "0.0.0.0:7677".into(),
                ..ListenSettings::default()
            }),
            ..WorkerProfile::default()
        };
        assert_eq!(
            resolve_start_mode(None, false, &[], Some("https://host"), &listening).unwrap(),
            StartMode::Dial("wss://host".into())
        );

        let dialing = WorkerProfile {
            default_controller: Some("wss://host".into()),
            ..WorkerProfile::default()
        };
        assert_eq!(
            resolve_start_mode(
                Some("0.0.0.0:9000".parse().unwrap()),
                false,
                &[],
                None,
                &dialing
            )
            .unwrap(),
            StartMode::Listen {
                addr: "0.0.0.0:9000".parse().unwrap(),
                allow_any: false,
                allow_from: Vec::new(),
            }
        );
    }

    /// The whole point, end to end at the config layer: enrol by
    /// listening, then start with no flags at all and come back
    /// listening on the same address and ranges. Through the file, so a
    /// field that fails to serialize is caught too.
    #[test]
    fn enrolling_by_listening_then_starting_bare_resumes_listening() {
        let enrolled = resolve_start_mode(
            Some("0.0.0.0:7677".parse().unwrap()),
            true,
            &[cidr("10.0.0.0/8")],
            None,
            &WorkerProfile::default(),
        )
        .unwrap();

        let mut config = WorkerProfile::default();
        remember_start_mode(&mut config, &enrolled);
        let saved: WorkerProfile = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();

        assert_eq!(
            resolve_start_mode(None, false, &[], None, &saved).unwrap(),
            enrolled,
            "a bare run has to come back exactly as it enrolled"
        );
    }

    /// The same for a dialing host, including that switching to dialing
    /// clears the listen settings rather than leaving both readable.
    #[test]
    fn enrolling_by_dialing_then_starting_bare_resumes_dialing() {
        let mut config = WorkerProfile {
            default_controller: Some(LISTENING_CONTROLLER.into()),
            listen: Some(ListenSettings {
                addr: "0.0.0.0:7677".into(),
                ..ListenSettings::default()
            }),
            ..WorkerProfile::default()
        };
        let enrolled =
            resolve_start_mode(None, false, &[], Some("https://host:7677"), &config).unwrap();
        remember_start_mode(&mut config, &enrolled);
        let saved: WorkerProfile = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();

        assert_eq!(saved.listen, None, "the old listening enrollment is gone");
        assert_eq!(
            resolve_start_mode(None, false, &[], None, &saved).unwrap(),
            StartMode::Dial("wss://host:7677".into())
        );
    }

    /// Two names on one machine are two workers: separate enrollments,
    /// and separate keys so a controller sees them as separate hosts.
    #[test]
    fn names_enrol_separately_and_do_not_share_a_key() {
        let mut config = WorkerConfig::default();
        remember_start_mode(
            config.profile(Some("build")),
            &StartMode::Dial("wss://a".into()),
        );
        remember_start_mode(
            config.profile(Some("test")),
            &StartMode::Dial("wss://b".into()),
        );

        let saved: WorkerConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        let mut saved = saved;
        assert_eq!(
            resolve_start_mode(None, false, &[], None, saved.profile(Some("build"))).unwrap(),
            StartMode::Dial("wss://a".into())
        );
        assert_eq!(
            resolve_start_mode(None, false, &[], None, saved.profile(Some("test"))).unwrap(),
            StartMode::Dial("wss://b".into())
        );

        // Sharing one key would make them the same host to a controller,
        // which identifies hosts by the key the handshake proves.
        assert_ne!(
            crate::paths::worker_key_path(Some("build")),
            crate::paths::worker_key_path(Some("test"))
        );
        assert_ne!(
            crate::paths::worker_key_path(Some("build")),
            crate::paths::worker_key_path(None)
        );
    }

    /// A named worker resumes its own enrollment, not whatever the
    /// unnamed one last did.
    #[test]
    fn a_named_worker_resumes_its_own_enrollment() {
        let mut config = WorkerConfig::default();
        remember_start_mode(
            config.profile(None),
            &StartMode::Dial("wss://default-host".into()),
        );
        remember_start_mode(
            config.profile(Some("edge")),
            &StartMode::Listen {
                addr: "0.0.0.0:7677".parse().unwrap(),
                allow_any: false,
                allow_from: vec![cidr("10.0.0.0/8")],
            },
        );

        let mut saved: WorkerConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            resolve_start_mode(None, false, &[], None, saved.profile(Some("edge"))).unwrap(),
            StartMode::Listen {
                addr: "0.0.0.0:7677".parse().unwrap(),
                allow_any: false,
                allow_from: vec![cidr("10.0.0.0/8")],
            },
            "the named worker listens even though the unnamed one dials"
        );
        assert_eq!(
            resolve_start_mode(None, false, &[], None, saved.profile(None)).unwrap(),
            StartMode::Dial("wss://default-host".into())
        );
    }

    /// A name nobody has enrolled is a first run for that worker, not a
    /// fallback onto someone else's enrollment.
    #[test]
    fn an_unknown_name_is_a_first_run() {
        let mut config = WorkerConfig::default();
        remember_start_mode(config.profile(None), &StartMode::Dial("wss://host".into()));
        let error = resolve_start_mode(None, false, &[], None, config.profile(Some("new")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("--controller"), "{error}");
    }

    /// A file written before names existed keeps its enrollment, and
    /// keeps the original key path, so an upgrade does not silently
    /// re-enrol the host as a stranger.
    #[test]
    fn a_config_from_before_names_becomes_the_default_worker() {
        let legacy = r#"
            default_controller = "wss://host:7677"
            [controllers."wss://host:7677"]
            credential = "cred"
            worker_id = 4
        "#;
        let mut cfg: WorkerConfig = toml::from_str(legacy).unwrap();
        cfg.migrate_legacy();

        assert_eq!(
            resolve_start_mode(None, false, &[], None, cfg.profile(None)).unwrap(),
            StartMode::Dial("wss://host:7677".into())
        );
        assert_eq!(
            cfg.profile(None)
                .controllers
                .get("wss://host:7677")
                .and_then(|c| c.credential.as_deref()),
            Some("cred"),
            "the credential has to come with it or the host re-enrols"
        );
        assert!(
            crate::paths::worker_key_path(None).ends_with("worker-key.pem"),
            "the unnamed worker keeps the original key file: {}",
            crate::paths::worker_key_path(None).display()
        );
    }

    /// A first run has nothing to resume, and the error has to name both
    /// ways of enrolling rather than only dialing.
    #[test]
    fn nothing_saved_names_both_ways_to_enroll() {
        let error = resolve_start_mode(None, false, &[], None, &WorkerProfile::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("--controller"), "{error}");
        assert!(error.contains("--listen"), "{error}");
    }

    /// The saved settings have to survive the file, including the ranges,
    /// or a restart quietly widens what may connect.
    #[test]
    fn listen_settings_survive_the_config_file() {
        let config = WorkerProfile {
            default_controller: Some(LISTENING_CONTROLLER.into()),
            listen: Some(ListenSettings {
                addr: "127.0.0.1:7677".into(),
                allow_any: false,
                allow_from: vec!["192.168.1.0/24".into(), "10.0.0.1/32".into()],
            }),
            ..WorkerProfile::default()
        };
        let back: WorkerProfile = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            resolve_start_mode(None, false, &[], None, &back).unwrap(),
            StartMode::Listen {
                addr: "127.0.0.1:7677".parse().unwrap(),
                allow_any: false,
                allow_from: vec![cidr("192.168.1.0/24"), cidr("10.0.0.1/32")],
            }
        );
    }

    #[test]
    fn enrollment_token_is_registration_material_until_a_credential_arrives() {
        let pending = ControllerCreds::default();
        assert_eq!(
            pending.enrollment_token(Some("one-time-token"), false),
            Some("one-time-token")
        );

        let registered = ControllerCreds {
            credential: Some("durable-credential".into()),
            ..ControllerCreds::default()
        };
        assert_eq!(
            registered.enrollment_token(Some("one-time-token"), false),
            None
        );
        assert_eq!(
            registered.enrollment_token(Some("replacement-token"), true),
            Some("replacement-token")
        );
    }

    /// The enrollment a completed registration produces has to survive the
    /// save, or a later bare `pm worker` reports no enrollment for a
    /// controller it just paired with. Only the profile is serialized, so
    /// recording onto the legacy top-level map silently drops it.
    #[test]
    fn a_registration_survives_the_config_file() {
        let key = "wss://host.lima.internal:7677";
        let mut cfg = WorkerConfig::default();
        cfg.profile(None).default_controller = Some(key.to_string());
        record_registration(
            cfg.profile(None),
            key,
            "cred".to_string(),
            9,
            pm_tls::KeyHash::from_hex(&"bb".repeat(32)).unwrap(),
        );

        let text = toml::to_string_pretty(&cfg).unwrap();
        let mut back: WorkerConfig = toml::from_str(&text).unwrap();
        back.migrate_legacy();

        let creds = back
            .profile(None)
            .controllers
            .get(key)
            .expect("the enrollment is still there after a save and load");
        assert_eq!(creds.credential.as_deref(), Some("cred"));
        assert_eq!(creds.worker_id, Some(9));
        assert_eq!(creds.controller_key.as_deref(), Some(&*"bb".repeat(32)));
    }

    /// A host that dialed in and registered resumes without a token: the
    /// saved credential is what `connect` looks for before it demands one.
    #[test]
    fn a_registered_host_no_longer_needs_a_token() {
        let key = "wss://host.lima.internal:7677";
        let mut cfg = WorkerConfig::default();
        record_registration(
            cfg.profile(None),
            key,
            "cred".to_string(),
            9,
            pm_tls::KeyHash::from_hex(&"bb".repeat(32)).unwrap(),
        );
        let text = toml::to_string_pretty(&cfg).unwrap();
        let mut back: WorkerConfig = toml::from_str(&text).unwrap();
        back.migrate_legacy();

        let stored = back
            .profile(None)
            .controllers
            .get(key)
            .cloned()
            .unwrap_or_default();
        let pinned = stored
            .controller_key
            .as_deref()
            .and_then(pm_tls::KeyHash::from_hex);
        assert!(
            pinned.is_some() || stored.enrollment_token(None, false).is_some(),
            "a bare `pm worker` would report no enrollment for {key}"
        );
    }

    #[test]
    fn config_round_trips_through_toml() {
        let mut cfg = WorkerConfig::default();
        let profile = cfg.profile(None);
        profile.default_controller = Some("wss://host".into());
        profile.controllers.insert(
            "wss://host".into(),
            ControllerCreds {
                controller_key: Some("aa".repeat(32)),
                credential: Some("cred".into()),
                worker_id: Some(3),
            },
        );
        let text = toml::to_string_pretty(&cfg).unwrap();
        let mut back: WorkerConfig = toml::from_str(&text).unwrap();
        back.migrate_legacy();
        let profile = back.profile(None);
        assert_eq!(profile.default_controller.as_deref(), Some("wss://host"));
        let creds = profile.controllers.get("wss://host").unwrap();
        assert_eq!(creds.credential.as_deref(), Some("cred"));
        assert_eq!(creds.worker_id, Some(3));
    }

    #[test]
    fn legacy_flat_config_migrates_into_the_controller_map() {
        let legacy = r#"
            controller = "https://host.lima.internal:7676/"
            credential = "cred"
            worker_id = 7
        "#;
        let mut cfg: WorkerConfig = toml::from_str(legacy).unwrap();
        cfg.migrate_legacy();

        let key = "wss://host.lima.internal:7676";
        let profile = cfg.profile(None);
        assert_eq!(profile.default_controller.as_deref(), Some(key));
        let creds = profile.controllers.get(key).expect("migrated entry");
        assert_eq!(creds.credential.as_deref(), Some("cred"));
        assert_eq!(creds.worker_id, Some(7));

        // Legacy scalars are dropped so they never round-trip back out.
        let text = toml::to_string_pretty(&cfg).unwrap();
        let reparsed: WorkerConfig = toml::from_str(&text).unwrap();
        assert!(reparsed.controller.is_none());
        assert!(reparsed.credential.is_none());
        assert!(reparsed.worker_id.is_none());
    }

    #[test]
    fn separate_controllers_keep_separate_credentials() {
        let mut cfg = WorkerConfig::default();
        let cfg = cfg.profile(None);
        cfg.controllers.insert(
            "ws://a".into(),
            ControllerCreds {
                controller_key: Some("aa".repeat(32)),
                credential: Some("cred-a".into()),
                worker_id: Some(1),
            },
        );
        cfg.controllers.insert(
            "ws://b".into(),
            ControllerCreds {
                controller_key: Some("aa".repeat(32)),
                credential: Some("cred-b".into()),
                worker_id: Some(2),
            },
        );
        assert_eq!(
            cfg.controllers.get("ws://a").unwrap().credential.as_deref(),
            Some("cred-a")
        );
        assert_eq!(
            cfg.controllers.get("ws://b").unwrap().credential.as_deref(),
            Some("cred-b")
        );
    }

    #[test]
    fn scoped_worker_file_read_accepts_empty_unicode_and_rejects_escape_and_symlink() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("résumé.txt"), b"").unwrap();
        let WorkerMsg::FileRead {
            ok,
            content,
            filename,
            ..
        } = read_scoped_file(1, root.path().to_str().unwrap(), "résumé.txt", 10)
        else {
            panic!("file result")
        };
        assert!(ok);
        assert!(content.is_empty());
        assert_eq!(filename, "résumé.txt");

        let WorkerMsg::FileRead { ok, error, .. } =
            read_scoped_file(2, root.path().to_str().unwrap(), "../secret", 10)
        else {
            panic!("file result")
        };
        assert!(!ok);
        assert!(error.contains("traversal") || error.contains("outside"));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("résumé.txt"), root.path().join("link"))
                .unwrap();
            let WorkerMsg::FileRead { ok, error, .. } =
                read_scoped_file(3, root.path().to_str().unwrap(), "link", 10)
            else {
                panic!("file result")
            };
            assert!(!ok);
            assert!(error.contains("symlink"));
        }
    }
}
