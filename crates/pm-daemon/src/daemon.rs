use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use pm_adapters::{select_endpoint, AdapterRegistry, Integration, SpawnCtx};
use pm_protocol::domain::{
    AgentDialects, AgentKind, AgentSelectionSource, BucketBriefing, ConnectMode, ContextField,
    ControllerMsg, Event, HookKind, InstructionLayer, InstructionRevision, InstructionTarget, Item,
    ItemQuery, ItemRef, ItemWrite, ModelDialect, ModelProfile, ModelProfileSource, PermissionMode,
    ResolvedModelEndpoint, RespondTarget, Scope, Session, SessionForward, SessionRole,
    SessionState, Snapshot, UserSetting, UserSettingChanged, WorkerMsg, WorkerTerminal,
    LOCAL_WORKER_ID,
};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::mux::{Mux, SessionExit};
use crate::project_host::ProjectHostState;
use crate::storage::{
    ActivityReport, ItemNote, ItemOutcome, ItemQueryCounts, ItemUpsert, SessionActivityUpdate,
    Storage, StorageError, Workspace,
};

/// Events buffered per subscriber before it is considered wedged and
/// disconnected.
const EVENT_CHANNEL_CAPACITY: usize = 4096;
const USER_SETTING_CHANNEL_CAPACITY: usize = 256;
const WAIT_EVENT_CHANNEL_CAPACITY: usize = 256;
const WAIT_JOURNAL_CAPACITY: usize = 65_536;

/// How long a worker enrollment token stays valid before it must be
/// reissued. Short, since a worker is enrolled right after minting.
const WORKER_ENROLLMENT_TTL_MS: i64 = 15 * 60 * 1000;

/// How long to wait for a worker to answer a directory listing.
/// A capture reads every file in scope, so it is allowed longer than a
/// directory listing.
const REPO_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const FS_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long the controller waits for a worker to report what it did
/// with a message bound for a session's agent inbox. The worker bounds
/// the delivery itself at ten seconds, so this leaves room for that
/// answer to come back rather than cutting it short and pasting a
/// message the agent is about to receive.
const AGENT_INBOX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// A spawn waits on this before it dispatches, and the host only has to
/// stat one directory, so it is much shorter than a directory listing.
/// Running out means the path is reported unchecked, never broken.
const PATH_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a first viewer waits for a complete worker replay.
const ATTACH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a disconnected worker's sessions stay alive awaiting a
/// reconnect before they are failed.
pub const WORKER_RECONNECT_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

const MAX_WEB_TERMINAL_STREAMS_PER_USER: usize = 128;

/// Keeps typing from publishing one full session event per keypress while
/// still making the sidebar feel immediate.
const ACTIVITY_PUBLISH_INTERVAL_MS: i64 = 1_000;
const RECOVERED_STATE_DETAIL: &str = "recovered after daemon restart";
const ORPHANED_SESSION_DETAIL: &str = "worker reconnected without this session";
const AWAITING_WORKER_STATE_DETAIL: &str = "worker offline, resumes when it reconnects";
/// Substring of the notification Claude sends once its prompt has gone
/// unanswered for a while, which carries no news about the session itself.
const PROMPT_UNANSWERED_NOTICE: &str = "waiting for your input";
const WORKER_INSTRUCTIONS: &str = include_str!("worker_instructions.md");
const SUPERVISOR_INSTRUCTIONS: &str = include_str!("supervisor_instructions.md");
const SYSTEM_FALLBACK_AGENT: AgentKind = AgentKind::ClaudeCode;

#[derive(Debug, Clone, Copy, Default)]
struct PendingSessionActivity {
    last_agent_activity_at_unix_ms: i64,
    last_user_interaction_at_unix_ms: i64,
    /// Submitted lines only. This is the user's half of the activity clock
    /// the session list shows, so keystrokes and terminal replies stay out.
    last_user_submit_at_unix_ms: i64,
    /// True after terminal input that did not submit a line. Automated
    /// notices must not be appended to text the user is still composing.
    user_input_pending: bool,
    checkpointed_agent_activity_at_unix_ms: i64,
    checkpointed_user_interaction_at_unix_ms: i64,
    checkpointed_user_submit_at_unix_ms: i64,
    last_published_at_unix_ms: i64,
}

/// Semantic-state authority for one terminal generation. Hook-integrated
/// adapters never infer work from output. A hookless generation recovered
/// after a daemon restart gets one explicitly consumed PTY fallback so it
/// can leave the conservative recovered-idle state. The stale-turn
/// watchdog arms the same fallback when it infers a turn end, so output
/// that resumes afterwards puts the session back to work.
#[derive(Debug, Clone, Copy)]
struct GenerationLifecycle {
    generation: u64,
    hook_authoritative: bool,
    terminal_fallback_available: bool,
    registered_at_unix_ms: i64,
    /// The last lifecycle hook seen on this generation. `None` on a
    /// hook-integrated adapter means its hooks have never fired, which is
    /// a broken launch rather than a quiet agent.
    last_hook: Option<(HookKind, i64)>,
    /// Set once the hook-silence grace period has elapsed with no hook, so
    /// the warning and the hookless downgrade happen once per generation.
    hook_silence_reported: bool,
}

#[derive(Debug, Clone)]
struct SessionLifecycleEvent {
    cursor: u64,
    session_id: u64,
    generation: u64,
    from: SessionState,
    to: SessionState,
    timestamp_unix_ms: i64,
    headline: String,
}

#[derive(Debug, Clone, Copy)]
struct LastSessionLifecycle {
    bucket_id: u64,
    generation: u64,
    state: SessionState,
}

/// One committed lifecycle change, as recorded in the wait journal;
/// the push pipeline derives notification events from it.
#[derive(Debug, Clone, Copy)]
struct SessionStateTransition {
    bucket_id: u64,
    generation: u64,
    from: SessionState,
    to: SessionState,
}

#[derive(Debug, Default)]
struct BucketWaitJournal {
    latest_cursor: u64,
    events: VecDeque<SessionLifecycleEvent>,
}

#[derive(Debug, Default)]
struct SessionWaitJournal {
    buckets: HashMap<u64, BucketWaitJournal>,
    last: HashMap<u64, LastSessionLifecycle>,
}

type SupervisorWaitScope = (u64, Vec<serde_json::Value>, Option<(&'static str, u64)>);

/// Block reason handed to a Stop hook when a turn ends and the agent has
/// never named the session on the dashboard.
const STOP_REPORT_NUDGE: &str = "This session has no name on the dashboard yet. Before you \
finish, call the `report` tool with a `headline` — a terse present-tense line describing what \
you just did — so the human watching can see this session's state. Set a final headline even \
if the task is complete.";

/// A certificate chain and private key, both PEM files, that make the
/// browser plane an HTTPS origin.
#[derive(Clone, Debug)]
pub struct HttpTls {
    pub cert_chain: PathBuf,
    pub key: PathBuf,
}

pub struct DaemonConfig {
    pub db_path: Option<PathBuf>,
    pub socket_path: PathBuf,
    /// HTTP surface (web UI, /api, /ws); None disables it.
    pub http_addr: Option<std::net::SocketAddr>,
    /// Serves the HTTP surface over TLS. None keeps plain HTTP, where
    /// browsers treat every origin but localhost as insecure.
    pub http_tls: Option<HttpTls>,
    /// Mutually authenticated listener for the worker plane. Workers reach
    /// the controller here and nowhere else, so a daemon without one serves
    /// no remote worker at all.
    pub worker_addr: Option<std::net::SocketAddr>,
    /// Base URL users and remote workers reach the browser plane at, when it
    /// differs from what the daemon binds (a reverse proxy, a tailnet name).
    pub public_url: Option<String>,
    pub scrollback_dir: PathBuf,
    pub registry: AdapterRegistry,
    pub local_worker_enabled: bool,
    /// The release channel this host follows, as `pm update --channel`
    /// saved it. None means stable.
    pub release_channel: Option<String>,
    pub forward: crate::forward::ForwardConfig,
    /// PTY silence after which a hook-integrated turn is treated as
    /// finished without a reported end. None uses the default.
    pub stale_turn_quiet_ms: Option<i64>,
    /// Grace period for a generation's first lifecycle hook. None uses
    /// the default.
    pub hook_silence_grace_ms: Option<i64>,
}

pub enum WebTerminalReplay {
    Complete { bytes: bytes::Bytes },
    Streaming(tokio::sync::mpsc::UnboundedReceiver<crate::workers::ReplayChunk>),
}

pub struct WebTerminalAttach {
    pub replay: WebTerminalReplay,
    pub output: tokio::sync::broadcast::Receiver<bytes::Bytes>,
    /// The PTY's size at attach, echoed to the viewer before replay.
    pub pty_size: (u16, u16),
    /// Later PTY size changes, echoed to the viewer as resize frames.
    pub size_rx: tokio::sync::broadcast::Receiver<(u16, u16)>,
    /// History rewrites that make this viewer's snapshot stale.
    pub rewrite_rx: tokio::sync::broadcast::Receiver<()>,
    /// The attach changed the PTY's size, so the program is about to redraw
    /// and the first snapshot should wait for that redraw.
    pub resized: bool,
    pub guard: Option<crate::workers::ViewerGuard>,
    pub progress: crate::workers::ViewerProgress,
}

/// What a request's `Host` names under the share-domain mount. A name
/// under that domain is a preview hostname whatever it resolves to, so
/// the dashboard never answers on one.
pub(crate) enum ShareHost {
    /// A forward, to be served at the root of this origin.
    Forward(u64),
    /// A label under the share domain that names no forward.
    Unknown,
}

pub struct Daemon {
    pub(crate) storage: Storage,
    pub(crate) connection_locks: Mutex<HashMap<u64, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// Bumped whenever a connection call settles, to wake held tool calls.
    pub(crate) connection_call_settled: tokio::sync::watch::Sender<u64>,
    /// Calls whose own session is holding a tool call open for the result.
    pub(crate) connection_call_waiters: Mutex<HashMap<String, usize>>,
    /// The newest published release, refreshed in the background. Only
    /// reported: replacing a running daemon ends its agent sessions.
    pub(crate) latest_release: std::sync::Arc<crate::update::LatestRelease>,
    /// Where the at-rest sealing secret lives. `None` for an in-memory
    /// database, which has no file to keep it beside.
    pub(crate) secret_path: Option<PathBuf>,
    pub(crate) installation_secret: std::sync::OnceLock<String>,
    /// A PHC string no password matches, so a login for a username that does
    /// not exist costs the same argon2 work as one that does.
    pub(crate) absent_user_hash: std::sync::OnceLock<String>,
    pub mux: std::sync::Arc<Mux>,
    /// The in-process worker uses the same relay contract as connected
    /// workers; its transport calls the mux instead of crossing a socket.
    local_worker: std::sync::Arc<crate::workers::WorkerLink>,
    /// Connected remote workers, keyed by worker id.
    pub workers: crate::workers::WorkerRegistry,
    /// In-memory session -> worker routing, so the hot input path never
    /// touches storage. A session never changes worker, so this only
    /// grows on spawn and shrinks on exit; a miss falls back to storage.
    terminal_workers: Mutex<std::collections::HashMap<u64, u64>>,
    /// Hot-path PTY observations waiting for the periodic SQLite
    /// checkpoint. Values are maxima, so coalescing loses no recency.
    pending_session_activity: Mutex<std::collections::HashMap<u64, PendingSessionActivity>>,
    /// Terminal-id keyed because a session's agent terminal is stable while
    /// its generation increments on resume/recovery.
    generation_lifecycle: Mutex<std::collections::HashMap<u64, GenerationLifecycle>>,
    program_status: crate::session_program_status::ProgramStatusSessions,
    /// Per-worker connection counter, bumped on each registration. A
    /// disconnect captures it so a grace-period fail only fires if no
    /// newer connection arrived.
    worker_epochs: Mutex<std::collections::HashMap<u64, u64>>,
    registry: AdapterRegistry,
    /// Held across every state mutation + event publish, and across
    /// snapshot + subscribe, so no event is lost or duplicated
    /// between a Snapshot and the stream that follows it.
    state_lock: Mutex<()>,
    events_tx: broadcast::Sender<Event>,
    /// Lifecycle-only journal used by Supervisor waits. It deliberately
    /// excludes report, context, terminal-output, and activity-only updates.
    session_wait_journal: Mutex<SessionWaitJournal>,
    wait_events_tx: broadcast::Sender<()>,
    /// A submitted user prompt cancels a pending wait owned by that session.
    wait_cancel_tx: broadcast::Sender<u64>,
    /// User preferences travel on a separate channel and are filtered by
    /// authenticated user id before they enter any client socket.
    user_setting_channels:
        Mutex<std::collections::HashMap<u64, broadcast::Sender<UserSettingChanged>>>,
    scrollback_dir: PathBuf,
    session_files_dir: PathBuf,
    socket_path: PathBuf,
    pm_exe: PathBuf,
    /// Bound HTTP address, set once the listener is up; sessions
    /// spawned before/without HTTP get no MCP reporting channel.
    http_bound: std::sync::OnceLock<std::net::SocketAddr>,
    http_tls: bool,
    /// The loaded browser-plane TLS config, so a listener bound for one
    /// forward serves the same certificate as the dashboard.
    http_tls_config: std::sync::OnceLock<std::sync::Arc<tokio_rustls::rustls::ServerConfig>>,
    public_url: Option<String>,
    worker_plane_bound: std::sync::OnceLock<std::net::SocketAddr>,
    shutdown_terminals: Mutex<std::collections::HashSet<u64>>,
    web_terminal_streams: Mutex<std::collections::HashMap<u64, usize>>,
    pub(crate) viewer_owners: crate::viewer_ownership::ViewerOwners,
    pub(crate) terminal_streams: std::sync::Arc<crate::workers::TerminalStreamPool>,
    pub(crate) transcript_transfers: std::sync::Arc<crate::workers::TranscriptTransferPool>,
    local_worker_enabled: bool,
    release_channel: Option<String>,
    forward_config: crate::forward::ForwardConfig,
    pub(crate) forwards: crate::forward::ForwardPool,
    /// Upstream connections the HTTP forward proxy reuses. Shared with
    /// `forwards`, which drops a forward's connections when it unbinds.
    pub(crate) upstreams: std::sync::Arc<crate::forward_upstream::UpstreamPool>,
    /// Directory-share servers this controller runs itself, for sessions
    /// on the in-process worker. A remote session's server lives on its
    /// worker and is reached through the same forward path as any port.
    local_dir_servers: Mutex<std::collections::HashMap<u64, crate::dir_server::DirShareServer>>,
    /// One gate per share, so the two drivers that restore shares after
    /// a restart — worker registration and the session reconciler —
    /// cannot bind the same share at once. Concurrent binds race to
    /// write the forward's port, and the loser's port is a server the
    /// winner already replaced.
    dir_share_binds: Mutex<std::collections::HashMap<u64, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    push: crate::push::PushRuntime,
    pub(crate) supervisor_wake_runtime: crate::supervisor_wake::SupervisorWakeRuntime,
    pub(crate) review_wake_runtime: crate::review::ReviewWakeRuntime,
    pub(crate) stale_turn_runtime: crate::stale_turn::StaleTurnRuntime,
    stale_turn_quiet_ms: i64,
    hook_silence_grace_ms: i64,
    report_freshness: crate::report_freshness::ReportFreshness,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("{0}")]
    Storage(#[from] StorageError),
    #[error("{0}")]
    Adapter(#[from] pm_adapters::AdapterError),
    #[error("{0}")]
    Mux(#[from] crate::mux::MuxError),
    #[error("{0}")]
    Worker(#[from] crate::workers::WorkerError),
    #[error("resolved {agent} agent from {selection_source}, but that agent is unavailable")]
    AgentUnavailable {
        agent: &'static str,
        selection_source: &'static str,
    },
    #[error("{0}")]
    Rejected(String),
    #[error(
        "worker {requested:?} is not allowed for this project; valid workers: {}",
        format_host_choices(valid)
    )]
    HostNotConfigured {
        requested: String,
        /// The selectable (worker id, worker name) pairs for the project.
        valid: Vec<(u64, String)>,
    },
    /// The worker is allowed for the project but cannot run it. A
    /// reachable worker with an unusable path is not an outage, so this
    /// never reads as offline unless the worker really is.
    #[error("{}", state.message(project, host, *worker_id))]
    ProjectHost {
        state: crate::project_host::ProjectHostState,
        project: String,
        host: String,
        worker_id: u64,
    },
}

fn format_host_choices(choices: &[(u64, String)]) -> String {
    if choices.is_empty() {
        return "none".to_string();
    }
    choices
        .iter()
        .map(|(id, name)| format!("{name} (id {id})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Longest accepted profile name and credential. Both are operator
/// input arriving over the control protocol.
const MODEL_PROFILE_NAME_MAX: usize = 120;
const MODEL_PROFILE_KEY_MAX: usize = 4096;

fn validated_profile_name(name: &str) -> Result<String, DaemonError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MODEL_PROFILE_NAME_MAX {
        return Err(DaemonError::Rejected(format!(
            "model profile name must be 1 to {MODEL_PROFILE_NAME_MAX} characters"
        )));
    }
    Ok(name.to_string())
}

fn validated_api_key(key: &str) -> Result<String, DaemonError> {
    let key = key.trim();
    if key.is_empty() || key.chars().count() > MODEL_PROFILE_KEY_MAX {
        return Err(DaemonError::Rejected(format!(
            "api key must be 1 to {MODEL_PROFILE_KEY_MAX} characters"
        )));
    }
    Ok(key.to_string())
}

/// The outcome of a worker connection handshake: which worker it is, the
/// credential to hand back on a fresh enrollment, and the channels the
/// connection then drives.
pub struct WorkerRegistration {
    pub worker_id: u64,
    pub credential: String,
    pub link: std::sync::Arc<crate::workers::WorkerLink>,
    pub rx: tokio::sync::mpsc::Receiver<ControllerMsg>,
    /// This connection's epoch, passed back to a grace-period fail so a
    /// reconnect cancels it.
    pub epoch: u64,
}

/// A shared path as it is stored and shown: relative to the session
/// working directory when it names something inside it. Identity only —
/// the host that serves the share re-resolves the path under its own
/// confinement rules, and this never stands in for that.
fn relative_to_cwd(cwd: &str, path: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    let Some(rest) = path
        .strip_prefix(trimmed)
        .filter(|_| !trimmed.is_empty())
        .map(|rest| rest.trim_start_matches('/'))
    else {
        return path.to_string();
    };
    if rest.is_empty() {
        ".".into()
    } else {
        rest.into()
    }
}

#[cfg(test)]
mod dir_share_path_tests {
    use super::relative_to_cwd;

    #[test]
    fn a_shared_path_is_stored_relative_to_the_session_directory() {
        assert_eq!(relative_to_cwd("/srv/api", "/srv/api/out"), "out");
        assert_eq!(
            relative_to_cwd("/srv/api/", "/srv/api/out/report"),
            "out/report"
        );
        assert_eq!(relative_to_cwd("/srv/api", "/srv/api"), ".");
        assert_eq!(relative_to_cwd("/srv/api", "out"), "out");
        // Not inside it, and not this function's job to refuse: the host
        // that serves the share resolves the path under confinement.
        assert_eq!(relative_to_cwd("/srv/api", "/etc/passwd"), "/etc/passwd");
        assert_eq!(relative_to_cwd("", "/srv/api/out"), "/srv/api/out");
    }
}

/// How long a worker has to answer that it bound a directory share's
/// server before the publish is refused.
const DIR_SHARE_BIND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct WorkerHello<'a> {
    pub enrollment_token: &'a str,
    pub credential: &'a str,
    /// The public key the TLS handshake proved this peer holds. Registration
    /// resolves the host from this, so it is never taken from the worker's
    /// own claims.
    pub peer_key_hash: &'a str,
    pub hostname: &'a str,
    pub platform: &'a str,
    /// The pm build the worker runs; empty from builds that predate
    /// version reporting.
    pub pm_version: &'a str,
    /// The container runtime holding the worker, empty on the host
    /// itself and from a worker below
    /// [`pm_protocol::WORKER_PROTOCOL_WORKER_RUNTIME`].
    pub runtime: &'a str,
    /// The runtime's name for that container, empty off a container.
    pub container: &'a str,
    pub default_project_root: &'a str,
    pub live_sessions: &'a [u64],
    pub live_terminals: &'a [WorkerTerminal],
    /// What the peer announced it can do. Capabilities are gated on it
    /// so a worker that predates one keeps the older path.
    pub protocol_version: u32,
}

/// An agent self-report delivered over the MCP channel. One `Report`
/// carries a whole status update; the optional bags are applied only
/// when present so a bare headline update never disturbs the rest.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentReport {
    Report {
        /// What the session is about; empty keeps the current goal.
        goal: String,
        /// The current step, or the outcome once the turn ends; empty
        /// keeps the current headline.
        headline: String,
        /// Fuller detail-view text; `None` leaves the current summary.
        summary: Option<String>,
        /// Timeline note; empty adds none beyond the headline entry.
        note: String,
        /// Replaces the list-row chips; `None` leaves them untouched.
        glance: Option<Vec<ContextField>>,
        /// Upserts detail fields by key; `None` leaves them untouched.
        context: Option<Vec<ContextField>>,
        /// Detail keys to drop.
        clear: Vec<String>,
        /// Where the session sits in git; empty fields leave the stored
        /// values alone. Boxed to keep the enum's variants a similar size.
        git: Box<crate::storage::SessionGitUpdate>,
    },
    /// The agent is waiting on the user.
    Blocked { question: String },
}

/// Truncates on a char boundary so multi-byte values never split.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Caps a field set's key, label, and value lengths to the storage
/// limits so a misbehaving agent cannot store oversized rows, and drops
/// fields whose key or value is blank — the tool schema requires the
/// strings but an empty string satisfies required, and a stored blank
/// renders as an empty chip.
fn bound_fields(fields: Vec<ContextField>, value_max: usize) -> Vec<ContextField> {
    fields
        .into_iter()
        .filter(|f| !f.key.trim().is_empty() && !f.value.trim().is_empty())
        .map(|f| ContextField {
            key: truncate_chars(&f.key, crate::storage::CONTEXT_KEY_MAX),
            label: truncate_chars(
                &crate::text::unescape_html_entities(&f.label),
                crate::storage::CONTEXT_LABEL_MAX,
            ),
            value: truncate_chars(&crate::text::unescape_html_entities(&f.value), value_max),
            kind: f.kind,
            severity: f.severity,
        })
        .collect()
}

fn overlay_session_activity(
    session: &mut pm_protocol::domain::Session,
    activity: PendingSessionActivity,
) {
    session.last_agent_activity_at_unix_ms = session
        .last_agent_activity_at_unix_ms
        .max(activity.last_agent_activity_at_unix_ms);
    session.last_user_interaction_at_unix_ms = session
        .last_user_interaction_at_unix_ms
        .max(activity.last_user_interaction_at_unix_ms);
    session.last_activity_at_unix_ms = session
        .last_activity_at_unix_ms
        .max(activity.last_user_submit_at_unix_ms);
}

/// Settings key: whether spawned PTYs advertise 24-bit color via
/// COLORTERM. Off drops agents to the 256-color palette, for attach
/// terminals that cannot render RGB.
pub const SETTING_SPAWN_TRUECOLOR: &str = "spawn.truecolor";

/// Settings key: whether spawned agents may take over the alternate
/// screen. Off keeps their output in the PTY scrollback, which is what
/// the web terminal and an attach both scroll.
pub const SETTING_SPAWN_FULLSCREEN: &str = "spawn.fullscreen";

/// Settings key: whether agent terminals consume the Program Status
/// Protocol (OSC 7501): answer its query, remove its sequences from the
/// output, and let the agent's root record drive the session state ahead
/// of lifecycle hooks.
pub const SETTING_SPAWN_PROGRAM_STATUS: &str = "spawn.program_status";

/// Settings key: how many live sessions one supervisor session may
/// have spawned at a time; further spawns are rejected until one ends.
pub const SETTING_SUPERVISOR_MAX_CHILDREN: &str = "supervisor.max_children";

const SUPERVISOR_MAX_CHILDREN_DEFAULT: usize = 8;

/// Largest single write a supervisor may send to a child's PTY.
pub const SUPERVISOR_INPUT_MAX: usize = 4096;

/// Characters of a supervisor input kept in the item audit note.
const SUPERVISOR_NOTE_PREVIEW: usize = 80;

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct SupervisorInputOutcome {
    pub session_id: u64,
    pub delivery: &'static str,
    pub input_state: &'static str,
    pub submit_requested: bool,
    pub submitted: bool,
    pub bytes_queued: usize,
    pub bytes_sent: usize,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    pub message: String,
    /// Set when the sender asked for an answer: the message to wait on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<u64>,
}

/// What a session shows while its turn is over but its own background
/// work is not.
pub(crate) const BACKGROUND_WORK_DETAIL: &str = "working in the background";
pub(crate) const INTERRUPTED_DETAIL: &str = "interrupted by user";

/// How long a message stays answerable. Long enough for a worker to
/// finish what it was doing and come back to the question, short enough
/// that a capability does not sit spendable in a transcript forever.
const REPLY_CAPABILITY_LIFETIME_MS: i64 = 6 * 60 * 60 * 1000;

/// How often a waiting sender re-reads the message. The answer arrives
/// through another session's tool call, so there is nothing to subscribe
/// to and the wait is a poll on a single indexed row.
const REPLY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// The capability that lets one message be answered once. Minted from
/// the OS generator because possession of it is what authorizes the
/// reply alongside the session's own identity.
fn mint_reply_token() -> String {
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The message as the receiving agent sees it: the sender's text, then
/// how to answer it.
///
/// The instructions are appended rather than wrapped so the text arrives
/// exactly as it was written, and they name the message and capability
/// explicitly because the agent has to pass both back.
fn reply_instructions(text: &str, message_id: u64, token: &str) -> String {
    format!(
        "{text}\n\n---\nThis message is waiting for your answer. Reply once by calling \
         reply_message with message_id {message_id} and reply_token {token}. The token works \
         a single time, so send your whole answer in one call."
    )
}

impl SupervisorInputOutcome {
    /// The agent took the message on its own inbound channel, so no
    /// keystrokes were written and none of the terminal's confirmation
    /// machinery applies: the agent acknowledged the handoff itself.
    fn delivered_to_inbox(
        session_id: u64,
        logical_bytes: usize,
        transport: &'static str,
        steered: bool,
    ) -> Self {
        Self {
            session_id,
            delivery: "delivered",
            input_state: if steered {
                "steered"
            } else {
                "submission_requested"
            },
            submit_requested: true,
            submitted: true,
            bytes_queued: logical_bytes,
            bytes_sent: logical_bytes,
            retryable: false,
            reason: None,
            message_id: None,
            message: format!(
                "session {session_id} took the message on its {transport} channel, \
                 so nothing was typed at its terminal"
            ),
        }
    }

    fn queued(
        session_id: u64,
        submit_requested: bool,
        submission_requested: bool,
        logical_bytes: usize,
    ) -> Self {
        Self {
            session_id,
            delivery: "queued",
            input_state: if submission_requested {
                "submission_requested"
            } else {
                "input_queued"
            },
            submit_requested,
            // Queue acceptance cannot prove that the TUI consumed the input or
            // accepted a turn. Agent lifecycle hooks confirm that separately.
            submitted: false,
            bytes_queued: logical_bytes,
            // Retained for response compatibility. The nonblocking local and
            // remote transports do not synchronously acknowledge a PTY write.
            bytes_sent: 0,
            retryable: false,
            reason: None,
            message_id: None,
            message: if submission_requested {
                format!("input and one submission action queued for session {session_id}; await agent lifecycle confirmation")
            } else {
                format!("input queued without a submission action for session {session_id}")
            },
        }
    }

    fn submitted(session_id: u64, logical_bytes: usize) -> Self {
        Self {
            session_id,
            delivery: "delivered",
            input_state: "submitted",
            submit_requested: true,
            submitted: true,
            bytes_queued: logical_bytes,
            bytes_sent: 0,
            retryable: false,
            reason: None,
            message_id: None,
            message: format!("input submitted for session {session_id}; a turn began"),
        }
    }

    fn queued_behind_turn(session_id: u64, logical_bytes: usize) -> Self {
        Self {
            session_id,
            delivery: "queued",
            input_state: "queued_behind_turn",
            submit_requested: true,
            submitted: false,
            bytes_queued: logical_bytes,
            bytes_sent: 0,
            retryable: false,
            reason: Some("agent_mid_turn"),
            message_id: None,
            message: format!(
                "session {session_id} is mid-turn; the message was queued in its composer and \
                 submission cannot be confirmed until the current turn ends. Watch wait_sessions \
                 for the next transition"
            ),
        }
    }

    fn submission_unconfirmed(session_id: u64, logical_bytes: usize) -> Self {
        Self {
            session_id,
            delivery: "queued",
            input_state: "submission_unconfirmed",
            submit_requested: true,
            submitted: false,
            bytes_queued: logical_bytes,
            bytes_sent: 0,
            retryable: true,
            reason: Some("submission_unconfirmed"),
            message_id: None,
            message: format!(
                "input and Enter were delivered to session {session_id} (Enter re-sent once), \
                 but no turn was observed to begin. Call read_terminal to inspect the composer; \
                 an empty text with submit true retries only the Enter without duplicating the \
                 message"
            ),
        }
    }

    fn submit_undelivered(
        session_id: u64,
        reason: &'static str,
        retryable: bool,
        logical_bytes: usize,
    ) -> Self {
        Self {
            session_id,
            delivery: "partial",
            input_state: "submit_undelivered",
            submit_requested: true,
            submitted: false,
            bytes_queued: logical_bytes,
            bytes_sent: 0,
            retryable,
            reason: Some(reason),
            message_id: None,
            message: format!(
                "the text was delivered to session {session_id} but the Enter was not \
                 ({reason}). Do not resend the text; retry with empty text and submit true to \
                 deliver only the Enter"
            ),
        }
    }

    fn not_delivered(
        session_id: u64,
        submit_requested: bool,
        reason: &'static str,
        retryable: bool,
        message: String,
    ) -> Self {
        Self {
            session_id,
            delivery: "not_delivered",
            input_state: "not_delivered",
            submit_requested,
            submitted: false,
            bytes_queued: 0,
            bytes_sent: 0,
            retryable,
            reason: Some(reason),
            message_id: None,
            message,
        }
    }
}

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// How long a pasted message is left to settle before the Enter that submits
/// it is written. Claude Code (and Codex) run a paste-burst heuristic on top
/// of bracketed paste: bytes that arrive within a short quiescence window
/// after the paste-end marker are folded into the paste, so an Enter written
/// in the same PTY write as a large multi-line paste becomes pasted content
/// and the message sits unsubmitted in the composer. Measured against a real
/// Claude Code TUI: an Enter written 0 ms after paste-end is swallowed, one
/// written 100 ms after submits; this margin covers loaded hosts, and the
/// submission confirmation below backstops the residual timing risk.
pub const SUPERVISOR_PASTE_SETTLE: std::time::Duration = std::time::Duration::from_millis(300);

/// How long each submission-confirmation attempt waits for the target
/// session's Working transition before the Enter is re-sent or the outcome
/// is reported unconfirmed.
pub const SUBMIT_CONFIRM_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a lifecycle wait sleeps before re-reading the journal after
/// losing its place on the event channel.
const WAIT_JOURNAL_RECHECK: std::time::Duration = std::time::Duration::from_millis(50);

/// Why a message did not reach an agent.
pub(crate) enum AgentMessageFailure {
    /// The write to the PTY failed.
    Terminal(TerminalInputFailure),
    /// The agent's inbox did not take it and the caller rules the
    /// terminal out, so nothing was written and nothing is pending.
    InboxOnly,
}

/// What putting a message into an agent did, so a caller can report the
/// route it actually took rather than the one it asked for.
pub(crate) struct AgentMessageDelivery {
    pub(crate) inbox: Option<crate::inbox::Delivered>,
    pub(crate) plan: SupervisorInputPlan,
    /// Set when the message landed on the PTY but the Enter that submits
    /// it did not. Recoverable: the composer is holding the text.
    pub(crate) submit_failure: Option<TerminalInputFailure>,
    /// Held so later writes of the same exchange reuse the stream, and
    /// released when the caller drops this.
    pub(crate) viewer: Option<crate::workers::ViewerGuard>,
}

pub(crate) struct SupervisorInputPlan {
    pub(crate) transport: bytes::Bytes,
    /// A submission keystroke that must be written only after the pasted
    /// transport bytes have settled in the agent TUI.
    pub(crate) deferred_submit: Option<bytes::Bytes>,
    logical_bytes: usize,
    submission_requested: bool,
}

pub(crate) fn supervisor_input_plan(
    submits_after_bracketed_paste: bool,
    text: &str,
    submit: bool,
) -> SupervisorInputPlan {
    let mut normalized = text.as_bytes().to_vec();
    if submit {
        while matches!(normalized.last(), Some(b'\r' | b'\n')) {
            normalized.pop();
        }
    }
    let submission_requested =
        submit || normalized.iter().any(|byte| matches!(byte, b'\r' | b'\n'));
    let logical_bytes = normalized.len() + usize::from(submit);

    // An agent TUI treats rapid raw characters plus Enter as a paste burst,
    // where Enter becomes a newline instead of submitting. Establish an
    // explicit paste boundary and keep the one logical Enter out of it as a
    // separate deferred write, because such a TUI also folds bytes that
    // arrive immediately after the paste-end marker back into the paste.
    let (transport, deferred_submit) =
        if submit && !normalized.is_empty() && submits_after_bracketed_paste {
            let mut data = Vec::with_capacity(
                BRACKETED_PASTE_START.len() + normalized.len() + BRACKETED_PASTE_END.len(),
            );
            data.extend_from_slice(BRACKETED_PASTE_START);
            data.extend_from_slice(&normalized);
            data.extend_from_slice(BRACKETED_PASTE_END);
            (data.into(), Some(bytes::Bytes::from_static(b"\r")))
        } else {
            if submit {
                normalized.push(b'\r');
            }
            (normalized.into(), None)
        };
    SupervisorInputPlan {
        transport,
        deferred_submit,
        logical_bytes,
        submission_requested,
    }
}

fn user_input_acknowledges_state(
    input_state: SessionState,
    current_state: SessionState,
    submitted: bool,
    adapter_has_lifecycle_hooks: bool,
) -> bool {
    input_state == current_state
        && submitted
        && (input_state == SessionState::NeedsInput
            || (input_state == SessionState::Idle && !adapter_has_lifecycle_hooks))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalInputFailure {
    Busy,
    MissingPty,
    Transport,
}

impl TerminalInputFailure {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::MissingPty => "missing_pty",
            Self::Transport => "transport_failure",
        }
    }
}

fn supervisor_input_failure_details(
    failure: TerminalInputFailure,
) -> (&'static str, bool, &'static str) {
    match failure {
        TerminalInputFailure::Busy => ("busy", true, "agent PTY is busy; retry the same input"),
        TerminalInputFailure::MissingPty => ("missing_pty", false, "agent PTY is unavailable"),
        TerminalInputFailure::Transport => (
            "transport_failure",
            true,
            "agent PTY stream could not be established; retry once its worker reconnects",
        ),
    }
}

/// Report timeline entries included in a supervisor status reply.
const SUPERVISOR_STATUS_REPORTS: usize = 5;

const SPAWN_ON_REPLY_TIMELINE_NOTES: usize = 10;

fn spawn_on_reply_prompt(item: &Item, notes: &[ItemNote], reply: &str) -> String {
    let mut prompt = format!(
        "You are the supervisor for item {}. The user was asked: {}\nThey replied: {}\nRead the item and continue.\n\nItem title:\n{}\n\nItem body:\n{}\n\nRecent timeline:",
        item.id, item.question, reply, item.title, item.body
    );
    let recent = &notes[notes.len().saturating_sub(SPAWN_ON_REPLY_TIMELINE_NOTES)..];
    if recent.is_empty() {
        prompt.push_str("\n(no timeline entries)");
    } else {
        for note in recent {
            prompt.push_str(&format!("\n- [{}] {}", note.kind, note.text));
        }
    }
    prompt
}

/// Every known daemon setting as (key, default, description). Unknown
/// keys and values that do not parse for the key are rejected, so a
/// typo cannot silently configure nothing.
pub const KNOWN_SETTINGS: &[(&str, &str, &str)] = &[
    (
        SETTING_SPAWN_TRUECOLOR,
        "true",
        "advertise 24-bit color (COLORTERM=truecolor) to spawned agents; \
         set false when attaching from terminals without truecolor support",
    ),
    (
        SETTING_SPAWN_FULLSCREEN,
        "false",
        "let spawned Claude Code sessions take over the alternate screen; \
         off keeps their output in the scrollback. OpenCode always uses \
         its default alternate-screen TUI",
    ),
    (
        SETTING_SPAWN_PROGRAM_STATUS,
        "false",
        "let spawned agents report their state through the Program Status \
         Protocol (OSC 7501): the agent terminal answers the protocol's \
         feature query, keeps the agent's records, and removes the \
         sequences from what viewers, attach and transcripts receive. \
         While the agent holds a root record its state decides the \
         session state (working, blocked as needs input, idle, done or \
         error as idle) and lifecycle hooks no longer change it. Off \
         leaves the terminal and hook-driven state exactly as before",
    ),
    (
        SETTING_SUPERVISOR_MAX_CHILDREN,
        "8",
        "how many live sessions one supervisor session may have spawned \
         at a time; further supervisor spawns are rejected until one ends",
    ),
    (
        crate::mobile::SETTING_MOBILE_ACCESS_TTL_MINUTES,
        "15",
        "minutes a mobile bearer access token stays valid before the \
         app must refresh it",
    ),
    (
        crate::mobile::SETTING_MOBILE_REFRESH_TTL_DAYS,
        "90",
        "days a mobile refresh token stays valid; each refresh issues a \
         new token, so a device stays enrolled while it is used at least \
         once per window",
    ),
    (
        crate::mobile::SETTING_MOBILE_ENROLL_TTL_MINUTES,
        "10",
        "minutes a one-use mobile enrollment token stays valid",
    ),
    (
        crate::mobile::SETTING_MOBILE_SOCKET_TICKET_TTL_SECONDS,
        "30",
        "seconds a one-use mobile socket ticket stays valid between its \
         HTTPS mint and the WebSocket upgrade that consumes it",
    ),
    (
        crate::push::SETTING_PUSH_EVENTS,
        crate::push::PUSH_EVENTS_DEFAULT,
        "which session transitions send mobile push notifications, as a \
         comma-separated subset of needs-input, failed, completed; empty \
         disables all classes",
    ),
    (
        crate::push::SETTING_PUSH_SCOPE,
        crate::push::PUSH_SCOPE_DEFAULT,
        "which sessions send push notifications: default (supervisors plus \
         workers with no supervising session), all, supervisors, or none; \
         per-bucket policies override this",
    ),
    (
        crate::push::SETTING_PUSH_DEDUPE_WINDOW_HOURS,
        "48",
        "hours settled push deliveries are kept so a repeated transition, \
         daemon restart, or provider retry cannot alert twice",
    ),
    (
        crate::push::SETTING_PUSH_GATEWAY_URL,
        crate::push::PUSH_GATEWAY_URL_DEFAULT,
        "base URL of the push gateway relay; set it to empty to deliver \
         nothing, or unset it to go back to the hosted relay",
    ),
];

/// JSON shape for one item, shared by the query command replies, the
/// HTTP surface, and the MCP list tool.
pub fn item_json(item: &Item) -> serde_json::Value {
    serde_json::json!({
        "id": item.id,
        "bucket_id": item.bucket_id,
        "ref": format!("pm:item/{}/{}", item.bucket_id, item.id),
        "project_id": item.project_id,
        "external_key": item.external_key,
        "title": item.title,
        "body": item.body,
        "question": item.question,
        "status": item.status.as_str(),
        "priority": item.priority.as_str(),
        "source_kind": item.source_kind.as_str(),
        "source_detail": item.source_detail,
        "url": item.url,
        "due_at_unix_ms": item.due_at_unix_ms,
        "snoozed_until_unix_ms": item.snoozed_until_unix_ms,
        "created_by_session_id": item.created_by_session_id,
        "created_at_unix_ms": item.created_at_unix_ms,
        "updated_at_unix_ms": item.updated_at_unix_ms,
        "done_at_unix_ms": item.done_at_unix_ms,
        "blocked_by": item.blocked_by,
        "session_ids": item.session_ids,
    })
}

/// One row of an item listing: enough to recognise, rank, and pick an item
/// without its text. Absent and empty fields are omitted, and `body_chars`
/// tells the reader whether `get_items` has anything more to show.
pub fn item_summary_json(item: &Item) -> serde_json::Value {
    let mut row = serde_json::Map::new();
    row.insert("id".into(), item.id.into());
    row.insert("bucket_id".into(), item.bucket_id.into());
    row.insert(
        "ref".into(),
        format!("pm:item/{}/{}", item.bucket_id, item.id).into(),
    );
    if let Some(project_id) = item.project_id {
        row.insert("project_id".into(), project_id.into());
    }
    if let Some(key) = &item.external_key {
        row.insert("external_key".into(), key.as_str().into());
    }
    row.insert("title".into(), item.title.as_str().into());
    row.insert("status".into(), item.status.as_str().into());
    row.insert("priority".into(), item.priority.as_str().into());
    row.insert("source_kind".into(), item.source_kind.as_str().into());
    row.insert("body_chars".into(), item.body.chars().count().into());
    if !item.question.is_empty() {
        row.insert("has_question".into(), true.into());
    }
    if let Some(due) = item.due_at_unix_ms {
        row.insert("due_at_unix_ms".into(), due.into());
    }
    if let Some(snoozed) = item.snoozed_until_unix_ms {
        row.insert("snoozed_until_unix_ms".into(), snoozed.into());
    }
    row.insert("updated_at_unix_ms".into(), item.updated_at_unix_ms.into());
    if let Some(done) = item.done_at_unix_ms {
        row.insert("done_at_unix_ms".into(), done.into());
    }
    if !item.blocked_by.is_empty() {
        row.insert("blocked_by".into(), item.blocked_by.clone().into());
    }
    if !item.session_ids.is_empty() {
        row.insert("session_ids".into(), item.session_ids.clone().into());
    }
    serde_json::Value::Object(row)
}

/// JSON shape for one session, shared by the supervisor MCP tools.
pub fn session_json(session: &pm_protocol::domain::Session) -> serde_json::Value {
    serde_json::json!({
        "id": session.id,
        "project_id": session.project_id,
        "agent": session.agent.as_str(),
        "agent_source": session.agent_source.as_str(),
        "state": session.state.as_str(),
        "state_detail": session.state_detail,
        "task_title": session.task_title,
        "goal": session.goal,
        "headline": session.headline,
        "summary": session.summary,
        "activity": session.activity,
        "worker_id": session.worker_id,
        "cwd": session.cwd,
        "created_at_unix_ms": session.created_at_unix_ms,
        "ended_at_unix_ms": session.ended_at_unix_ms,
        "exit_code": session.exit_code,
        "last_activity_at_unix_ms": session.last_activity_at_unix_ms,
        "spawned_by_session_id": session.spawned_by_session_id,
    })
}

fn context_fields_json(fields: &[ContextField]) -> serde_json::Value {
    serde_json::json!(fields
        .iter()
        .map(|f| {
            serde_json::json!({
                "key": f.key,
                "label": f.label,
                "value": f.value,
                "kind": f.kind.as_str(),
                "severity": f.severity.as_str(),
            })
        })
        .collect::<Vec<_>>())
}

pub fn item_note_json(note: &ItemNote) -> serde_json::Value {
    serde_json::json!({
        "id": note.id,
        "session_id": note.session_id,
        "ts_unix_ms": note.ts_unix_ms,
        "kind": note.kind,
        "text": note.text,
    })
}

/// A worker that stops a run itself says why, and that reason is what the
/// session shows. A failure it does not explain is the spawn itself.
fn remote_exit_detail(failed: bool, detail: &str) -> &str {
    match (failed, detail.is_empty()) {
        (false, _) => "",
        (true, false) => detail,
        (true, true) => "worker failed to spawn terminal",
    }
}

pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}

impl Daemon {
    pub fn new(config: DaemonConfig) -> anyhow::Result<(Self, crate::mux::MuxChannels)> {
        let storage = match &config.db_path {
            Some(p) => {
                if let Some(parent) = p.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                    crate::fsperm::create_private_dir(parent)?;
                }
                let storage = Storage::open(p)?;
                crate::fsperm::restrict_database(p)?;
                storage
            }
            None => Storage::open_in_memory()?,
        };
        let secret_path = config
            .db_path
            .as_deref()
            .map(crate::secrets::secret_path_for_db);
        let dashboard_host = config.public_url.as_deref().unwrap_or_default();
        let share_domain_site = match config.forward.share_domain.as_deref() {
            Some(share_domain) => {
                crate::forward_mount::check_share_domain(share_domain, dashboard_host)?
            }
            None => None,
        };
        match config.forward.mount_mode() {
            crate::forward_mount::MountMode::PathPrefix => warn!(
                "previews are served under /forwards/<id>/ on the dashboard's own \
                 origin, so a page an agent publishes runs with the signed-in \
                 user's authority over this controller: it can drive any agent \
                 terminal and read anything the dashboard can. Publish only work \
                 you would run yourself. Pass --share-domain, on a registrable \
                 domain of its own, to give each preview a separate origin"
            ),
            crate::forward_mount::MountMode::PerForwardPort { .. } => warn!(
                "each preview gets a listener of its own, so it is a separate \
                 origin: it cannot read a dashboard response or reach dashboard \
                 storage. A port is no part of a cookie's scope, though, so \
                 previews and the dashboard share one cookie jar and a preview \
                 can set cookies the browser then sends here. The session cookie \
                 is signed and a request carrying two is refused, so a preview \
                 cannot take over or forge a session, but it can invalidate one. \
                 Only --share-domain, on a registrable domain of its own, makes a \
                 preview a separate site"
            ),
            crate::forward_mount::MountMode::ShareDomain(domain) => {
                if dashboard_host.is_empty() {
                    warn!(
                        share_domain = domain,
                        "a share domain is configured but --public-url is not, so the \
                         controller does not know the name it is reached by and cannot \
                         say whether previews are on a different site from the dashboard. \
                         A share domain under the dashboard's own registrable domain \
                         leaves previews sharing the dashboard's cookie jar"
                    );
                } else if let Some(reason) = &share_domain_site {
                    warn!(
                        share_domain = domain,
                        dashboard_host,
                        reason = %reason,
                        "the share domain is on the same site as the dashboard, so each \
                         preview gets an origin of its own and not a cookie jar of its \
                         own: it cannot read a dashboard response or reach dashboard \
                         storage, but the browser attaches the session cookie to \
                         requests it makes to the dashboard. The dashboard's routes take \
                         a bearer token, and a state-changing request carrying the cookie \
                         is checked against the dashboard's own origin, as is every \
                         WebSocket upgrade, so a preview can neither spend nor forge a \
                         session. It can still invalidate one by setting a session cookie \
                         of its own on the shared parent domain. A share domain on a \
                         registrable domain of its own makes a preview a separate site"
                    );
                }
            }
        }
        crate::fsperm::create_private_dir(&config.scrollback_dir)?;
        let session_files_dir = config.scrollback_dir.join("session-files");
        crate::fsperm::create_private_dir(&session_files_dir)?;
        let (mux, channels) = Mux::new();
        let mux = std::sync::Arc::new(mux);
        let local_worker = crate::workers::WorkerLink::local(mux.clone());
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (wait_events_tx, _) = broadcast::channel(WAIT_EVENT_CHANNEL_CAPACITY);
        let (wait_cancel_tx, _) = broadcast::channel(WAIT_EVENT_CHANNEL_CAPACITY);
        let pm_exe = std::env::current_exe()?;
        if pm_adapters::path_needs_shell_quoting(&pm_exe) {
            warn!(
                pm_exe = %pm_exe.display(),
                "pm binary path contains shell-hazardous characters, lifecycle hook commands embed it quoted"
            );
        }
        let upstreams = std::sync::Arc::new(crate::forward_upstream::UpstreamPool::default());
        let daemon = Daemon {
            storage,
            connection_locks: Mutex::new(HashMap::new()),
            connection_call_settled: tokio::sync::watch::Sender::new(0),
            connection_call_waiters: Mutex::new(HashMap::new()),
            latest_release: std::sync::Arc::new(crate::update::LatestRelease::default()),
            secret_path,
            installation_secret: std::sync::OnceLock::new(),
            absent_user_hash: std::sync::OnceLock::new(),
            mux,
            local_worker,
            workers: crate::workers::WorkerRegistry::default(),
            terminal_workers: Mutex::new(std::collections::HashMap::new()),
            pending_session_activity: Mutex::new(std::collections::HashMap::new()),
            generation_lifecycle: Mutex::new(std::collections::HashMap::new()),
            program_status: Default::default(),
            worker_epochs: Mutex::new(std::collections::HashMap::new()),
            registry: config.registry,
            state_lock: Mutex::new(()),
            events_tx,
            session_wait_journal: Mutex::new(SessionWaitJournal::default()),
            wait_events_tx,
            wait_cancel_tx,
            user_setting_channels: Mutex::new(std::collections::HashMap::new()),
            scrollback_dir: config.scrollback_dir,
            session_files_dir,
            socket_path: config.socket_path,
            pm_exe,
            http_bound: std::sync::OnceLock::new(),
            http_tls: config.http_tls.is_some(),
            http_tls_config: std::sync::OnceLock::new(),
            worker_plane_bound: std::sync::OnceLock::new(),
            shutdown_terminals: Mutex::new(std::collections::HashSet::new()),
            web_terminal_streams: Mutex::new(std::collections::HashMap::new()),
            viewer_owners: crate::viewer_ownership::ViewerOwners::default(),
            terminal_streams: std::sync::Arc::new(crate::workers::TerminalStreamPool::default()),
            transcript_transfers: std::sync::Arc::new(
                crate::workers::TranscriptTransferPool::default(),
            ),
            local_worker_enabled: config.local_worker_enabled,
            release_channel: config.release_channel.clone(),
            public_url: config.public_url,
            forward_config: config.forward,
            forwards: crate::forward::ForwardPool::new(upstreams.clone()),
            upstreams,
            local_dir_servers: Mutex::new(std::collections::HashMap::new()),
            dir_share_binds: Mutex::new(std::collections::HashMap::new()),
            push: crate::push::PushRuntime::default(),
            supervisor_wake_runtime: crate::supervisor_wake::SupervisorWakeRuntime::default(),
            review_wake_runtime: crate::review::ReviewWakeRuntime::default(),
            stale_turn_runtime: crate::stale_turn::StaleTurnRuntime::default(),
            stale_turn_quiet_ms: config
                .stale_turn_quiet_ms
                .unwrap_or(crate::stale_turn::DEFAULT_STALE_TURN_QUIET_MS),
            hook_silence_grace_ms: config
                .hook_silence_grace_ms
                .unwrap_or(crate::stale_turn::DEFAULT_HOOK_SILENCE_GRACE_MS),
            report_freshness: crate::report_freshness::ReportFreshness::default(),
        };
        if daemon.local_worker_enabled {
            for session in daemon.storage.snapshot(now_unix_ms())?.sessions {
                if !session.state.is_live() {
                    daemon.refresh_local_agent_resumability(session.id);
                }
            }
        }
        daemon.seed_session_wait_journal();
        daemon.seed_supervisor_wake_marks();
        // Establishes the sealing secret, and moves one an older version
        // left in the database, at startup rather than on the first
        // credential a session happens to store.
        let _ = daemon.installation_secret();
        // Then erases what that move left behind, which the move itself cannot
        // do: it runs once per installation and has to happen before anything
        // else can be told the database is safe to copy.
        daemon.erase_migrated_secret_pages();
        Ok((daemon, channels))
    }

    pub(crate) fn acquire_web_terminal_stream(&self, user_id: u64) -> bool {
        let mut streams = self.web_terminal_streams.lock().unwrap();
        let count = streams.entry(user_id).or_default();
        if *count >= MAX_WEB_TERMINAL_STREAMS_PER_USER {
            return false;
        }
        *count += 1;
        true
    }

    pub(crate) fn release_web_terminal_stream(&self, user_id: u64) {
        let mut streams = self.web_terminal_streams.lock().unwrap();
        let Some(count) = streams.get_mut(&user_id) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            streams.remove(&user_id);
        }
    }

    pub(crate) fn enqueue_worker_transcripts(
        &self,
        link: &std::sync::Arc<crate::workers::WorkerLink>,
        transcripts: Vec<pm_protocol::domain::WorkerTranscript>,
    ) {
        let valid = transcripts
            .into_iter()
            .filter(|transcript| {
                if transcript.size > crate::mux::SCROLLBACK_CAP_BYTES as u64 {
                    return false;
                }
                let Ok(terminal) = self.storage.get_terminal(transcript.terminal_id) else {
                    return false;
                };
                terminal.generation == transcript.generation
                    && self
                        .storage
                        .get_session(terminal.session_id)
                        .is_ok_and(|session| session.worker_id == link.worker_id)
            })
            .collect();
        self.transcript_transfers.enqueue(link, valid);
    }

    pub(crate) fn mark_worker_transcript_received(&self, terminal_id: u64, generation: u64) {
        let Ok(terminal) = self.storage.get_terminal(terminal_id) else {
            return;
        };
        if terminal.generation != generation {
            return;
        }
        if let Ok(updated) = self
            .storage
            .set_terminal_scrollback_available(terminal_id, generation)
        {
            self.publish(Event::TerminalChanged(updated));
        }
    }

    /// Applies a needs-input signal the mux detected in a session's PTY
    /// output. Best-effort: silently ignores sessions that have ended.
    pub fn handle_pty_needs_input(&self, session_id: u64) {
        let _guard = self.state_lock.lock().unwrap();
        let live = self
            .storage
            .get_session(session_id)
            .map(|s| s.state.is_live())
            .unwrap_or(false);
        if !live {
            return;
        }
        if let Ok(updated) = self.storage.update_session_state(
            session_id,
            SessionState::NeedsInput,
            "waiting for approval",
        ) {
            info!(session = session_id, "pty needs-input signal");
            self.publish(Event::SessionChanged(updated));
        }
    }

    pub(crate) fn set_session_needs_input(
        &self,
        session_id: u64,
        detail: &str,
    ) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        let updated =
            self.storage
                .update_session_state(session_id, SessionState::NeedsInput, detail)?;
        self.publish(Event::SessionChanged(updated));
        Ok(())
    }

    pub(crate) fn storage(&self) -> &Storage {
        &self.storage
    }

    pub(crate) fn mux_is_running(&self, terminal_id: u64) -> bool {
        self.mux.is_running(terminal_id)
    }

    pub(crate) fn program_status(&self) -> &crate::session_program_status::ProgramStatusSessions {
        &self.program_status
    }

    /// Held across a session state read, its write and the publish.
    pub(crate) fn lock_session_state(&self) -> std::sync::MutexGuard<'_, ()> {
        self.state_lock.lock().unwrap()
    }

    pub(crate) fn stale_turn_quiet_ms(&self) -> i64 {
        self.stale_turn_quiet_ms
    }

    pub(crate) fn hook_silence_grace_ms(&self) -> i64 {
        self.hook_silence_grace_ms
    }

    /// Hook authority for one generation: whether any hook has arrived,
    /// when the generation was registered, and whether its hook silence
    /// has already been acted on. `None` when the generation is stale.
    pub(crate) fn generation_hook_status(
        &self,
        terminal_id: u64,
        generation: u64,
    ) -> Option<(bool, i64, bool)> {
        let remembered = {
            let generations = self.generation_lifecycle.lock().unwrap();
            generations.get(&terminal_id).and_then(|lifecycle| {
                (lifecycle.generation == generation).then_some((
                    lifecycle.hook_authoritative,
                    lifecycle.registered_at_unix_ms,
                    lifecycle.hook_silence_reported,
                ))
            })
        };
        if let Some(status) = remembered {
            return Some(status);
        }
        // Agents keep running on their hosts across a daemon restart, which
        // empties this map while leaving their sessions mid-turn. Answering
        // from the run row instead of not answering at all is what keeps
        // those sessions watched, since a caller that gets nothing here has
        // no way to tell an unknown generation from a healthy one.
        let (started_at, hook_seen) = self
            .storage
            .generation_hook_mark(terminal_id, generation)
            .ok()??;
        Some((hook_seen.is_some(), hook_seen.unwrap_or(started_at), false))
    }

    pub(crate) fn mark_hook_silence_reported(&self, terminal_id: u64, generation: u64) {
        self.materialize_generation_lifecycle(terminal_id, generation);
        let mut generations = self.generation_lifecycle.lock().unwrap();
        if let Some(lifecycle) = generations.get_mut(&terminal_id) {
            if lifecycle.generation == generation {
                lifecycle.hook_silence_reported = true;
            }
        }
    }

    /// Rebuilds this generation's in-memory lifecycle from the run row when
    /// the map has none, so the per-generation flags a restart erased can
    /// still be set. Reads storage before taking the lock, since every
    /// other holder of it is on a hot path.
    fn materialize_generation_lifecycle(&self, terminal_id: u64, generation: u64) {
        let present = self
            .generation_lifecycle
            .lock()
            .unwrap()
            .get(&terminal_id)
            .is_some_and(|lifecycle| lifecycle.generation == generation);
        if present {
            return;
        }
        let Ok(Some((started_at, hook_seen))) =
            self.storage.generation_hook_mark(terminal_id, generation)
        else {
            return;
        };
        let mut generations = self.generation_lifecycle.lock().unwrap();
        let lifecycle = generations
            .entry(terminal_id)
            .or_insert_with(|| GenerationLifecycle {
                generation,
                hook_authoritative: hook_seen.is_some(),
                terminal_fallback_available: false,
                registered_at_unix_ms: hook_seen.unwrap_or(started_at),
                last_hook: None,
                hook_silence_reported: false,
            });
        if lifecycle.generation != generation {
            *lifecycle = GenerationLifecycle {
                generation,
                hook_authoritative: hook_seen.is_some(),
                terminal_fallback_available: false,
                registered_at_unix_ms: hook_seen.unwrap_or(started_at),
                last_hook: None,
                hook_silence_reported: false,
            };
        }
    }

    pub(crate) fn arm_terminal_fallback_for(&self, terminal_id: u64, generation: u64) {
        self.arm_terminal_fallback(terminal_id, generation);
    }

    /// Writes a session state and publishes it, both under the state
    /// lock, for callers outside this module. Splitting the two would let
    /// a snapshot land between them and lose or duplicate the event.
    pub(crate) fn commit_session_state(
        &self,
        session_id: u64,
        state: SessionState,
        detail: &str,
    ) -> Result<Session, DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        let updated = self
            .storage
            .update_session_state(session_id, state, detail)?;
        self.publish(Event::SessionChanged(updated.clone()));
        Ok(updated)
    }

    pub(crate) fn push_runtime(&self) -> &crate::push::PushRuntime {
        &self.push
    }

    /// Reduces visual urgency after a user views a blocked or freshly
    /// finished session while preserving its lifecycle state for every
    /// wait/filter consumer.
    pub fn mark_session_seen(&self, session_id: u64) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        let before = self.storage.get_session(session_id)?;
        let unseen_needs_input =
            before.state == SessionState::NeedsInput && before.needs_input_unseen;
        let unseen_idle = before.state == SessionState::Idle && before.idle_unseen;
        if !unseen_needs_input && !unseen_idle {
            return Ok(());
        }
        let updated = self.storage.mark_session_seen(session_id, now_unix_ms())?;
        self.publish(Event::SessionChanged(updated));
        Ok(())
    }

    pub fn set_http_addr(&self, addr: std::net::SocketAddr) {
        let _ = self.http_bound.set(addr);
    }

    pub fn set_http_tls_config(&self, config: std::sync::Arc<tokio_rustls::rustls::ServerConfig>) {
        let _ = self.http_tls_config.set(config);
    }

    pub(crate) fn http_tls_config(
        &self,
    ) -> Option<std::sync::Arc<tokio_rustls::rustls::ServerConfig>> {
        self.http_tls_config.get().cloned()
    }

    pub fn set_worker_plane_addr(&self, addr: std::net::SocketAddr) {
        let _ = self.worker_plane_bound.set(addr);
    }

    pub fn worker_plane_addr(&self) -> Option<std::net::SocketAddr> {
        self.worker_plane_bound.get().copied()
    }

    /// The base URL a worker that predates the local MCP relay sends its
    /// agents' reports to, when the operator configured one. The daemon
    /// knows what it binds, not the name it is reached by, so a bound
    /// address is not an answer here.
    pub fn agent_mcp_base_url(&self) -> Option<String> {
        self.public_url
            .as_deref()
            .map(|base| base.trim_end_matches('/').to_string())
    }

    /// The canonical base URL the operator configured, when they did.
    /// The release channel this host follows, or None for stable.
    pub fn release_channel(&self) -> Option<&str> {
        self.release_channel.as_deref()
    }

    pub fn public_url(&self) -> Option<&str> {
        self.public_url.as_deref()
    }

    /// The browser plane's port, which such a worker pairs with the
    /// address it actually reached.
    pub fn http_port(&self) -> u16 {
        self.http_bound.get().map(|a| a.port()).unwrap_or_default()
    }

    /// The URL scheme the bound HTTP listener speaks.
    pub fn http_scheme(&self) -> &'static str {
        if self.http_tls {
            "https"
        } else {
            "http"
        }
    }

    fn mcp_url(&self) -> Option<String> {
        self.http_bound
            .get()
            .map(|a| format!("{}://{a}/mcp", self.http_scheme()))
    }

    /// The dashboard login URL, used for forward auth redirects.
    pub fn login_url(&self) -> String {
        if let Some(base) = self.public_url.as_deref() {
            format!("{}/login", base.trim_end_matches('/'))
        } else if let Some(addr) = self.http_bound.get() {
            format!("{}://{addr}/login", self.http_scheme())
        } else {
            "/login".to_string()
        }
    }

    /// Verifies a scoped forward token and returns the username.
    pub fn verify_forward_token(&self, token: &str, forward_id: u64) -> Option<String> {
        self.forwards.verify_forward_token(token, forward_id)
    }

    /// Mints a scoped forward token for the given user and forward.
    pub fn mint_forward_token(&self, user_id: u64, username: String, forward_id: u64) -> String {
        self.forwards
            .mint_forward_token(user_id, username, forward_id)
    }

    pub fn subscribe(&self) -> (Snapshot, broadcast::Receiver<Event>) {
        let _guard = self.state_lock.lock().unwrap();
        let snapshot = self.subscription_snapshot();
        let rx = self.events_tx.subscribe();
        (snapshot, rx)
    }

    /// Atomically captures shared state, this authenticated user's validated
    /// preferences, and both event receivers. The shared state lock is also
    /// held by preference writes, so startup cannot miss an update between
    /// its snapshot and live stream.
    pub(crate) fn subscribe_for_user(
        &self,
        user_id: u64,
    ) -> (
        Snapshot,
        broadcast::Receiver<Event>,
        broadcast::Receiver<UserSettingChanged>,
    ) {
        let _guard = self.state_lock.lock().unwrap();
        let mut snapshot = self.subscription_snapshot();
        snapshot.user_settings = self.valid_user_settings(user_id);
        // Where this reader is in each review, including what they have
        // already seen. Per-user, so it rides the snapshot rather than
        // the broadcast event bus.
        snapshot.review_viewer_states = snapshot
            .reviews
            .iter()
            .filter_map(|review| self.storage.review_viewer_state(review.id, user_id).ok())
            .collect();
        let events = self.events_tx.subscribe();
        let user_settings = self.user_setting_sender(user_id).subscribe();
        (snapshot, events, user_settings)
    }

    fn subscription_snapshot(&self) -> Snapshot {
        let mut snapshot = self.storage.snapshot(now_unix_ms()).unwrap_or_default();
        let activity = self.pending_session_activity.lock().unwrap();
        for session in &mut snapshot.sessions {
            if let Some(pending) = activity.get(&session.id) {
                overlay_session_activity(session, *pending);
            }
        }
        drop(activity);
        for session in &mut snapshot.sessions {
            session.program_status = self.program_status_records(session.id);
        }
        if !self.local_worker_enabled {
            snapshot
                .workers
                .retain(|worker| worker.id != LOCAL_WORKER_ID);
        }
        for worker in &mut snapshot.workers {
            self.overlay_worker_runtime(worker);
        }
        self.overlay_awaiting_worker_sessions(&mut snapshot.sessions);
        for forward in &mut snapshot.forwards {
            *forward = self.forward_view(forward.clone());
        }
        snapshot.model_profiles = self.storage.list_model_profiles().unwrap_or_default();
        snapshot.agent_dialects = self.agent_dialects();
        snapshot
    }

    /// The agent names a spawn may pick, from the registered adapters.
    /// Published schemas and error messages read it from here so they
    /// cannot advertise an agent the registry would reject.
    pub fn spawnable_agents(&self) -> Vec<&'static str> {
        self.registry
            .kinds()
            .into_iter()
            .map(|kind| kind.as_str())
            .collect()
    }

    /// The dialects each registered adapter speaks, so clients derive
    /// which agents a profile covers from the daemon's answer instead
    /// of duplicating the matrix.
    pub fn agent_dialects(&self) -> Vec<AgentDialects> {
        let mut dialects: Vec<AgentDialects> = AgentKind::ALL
            .iter()
            .copied()
            .filter_map(|agent| {
                let adapter = self.registry.get(agent).ok()?;
                Some(AgentDialects {
                    agent,
                    dialects: adapter.dialects().to_vec(),
                    supports_background_model: adapter.supports_background_model(),
                })
            })
            .collect();
        dialects.sort_by_key(|entry| entry.agent.as_str());
        dialects
    }

    pub(crate) fn overlay_awaiting_worker_sessions(&self, sessions: &mut [Session]) {
        let worker_ids = sessions
            .iter()
            .map(|session| session.worker_id)
            .filter(|worker_id| {
                *worker_id != LOCAL_WORKER_ID
                    && !self.worker_is_online(*worker_id)
                    && self.storage.get_worker(*worker_id).is_ok()
            })
            .collect::<std::collections::HashSet<_>>();
        let eligible = worker_ids
            .into_iter()
            .flat_map(|worker_id| {
                self.storage
                    .auto_resume_sessions_on_worker(worker_id)
                    .unwrap_or_default()
            })
            .map(|session| session.id)
            .collect::<std::collections::HashSet<_>>();
        for session in sessions {
            if eligible.contains(&session.id) {
                session.state = SessionState::AwaitingWorker;
                session.state_detail = AWAITING_WORKER_STATE_DETAIL.into();
            }
        }
    }

    fn valid_user_settings(&self, user_id: u64) -> Vec<UserSetting> {
        let settings = match self.storage.list_user_settings(user_id) {
            Ok(settings) => settings,
            Err(error) => {
                error!(user_id, error = %error, "failed to load user settings");
                return Vec::new();
            }
        };
        settings
            .into_iter()
            .filter_map(|(key, value)| {
                let normalized = match key.as_str() {
                    crate::terminal_theme::USER_TERMINAL_THEME_KEY => {
                        crate::terminal_theme::normalize_terminal_theme(value.as_bytes())
                            .map(|(_theme, normalized)| normalized)
                            .map_err(|error| error.to_string())
                    }
                    crate::appearance::USER_APPEARANCE_KEY => {
                        crate::appearance::normalize_appearance(value.as_bytes())
                            .map(|(_appearance, normalized)| normalized)
                            .map_err(|error| error.to_string())
                    }
                    crate::ui_theme::USER_UI_THEME_KEY => {
                        crate::ui_theme::normalize_ui_theme(value.as_bytes())
                            .map(|(_theme, normalized)| normalized)
                            .map_err(|error| error.to_string())
                    }
                    crate::push::SETTING_PUSH_WEB_IDLE_MINUTES => {
                        crate::push::normalize_web_idle_minutes(value.as_bytes())
                            .map(|(_minutes, normalized)| normalized)
                            .map_err(|error| error.to_string())
                    }
                    _ => {
                        warn!(user_id, key, "ignoring unknown stored user setting");
                        return None;
                    }
                };
                match normalized {
                    Ok(value_json) => Some(UserSetting { key, value_json }),
                    Err(parse_error) => {
                        error!(
                            user_id,
                            key,
                            error = %parse_error,
                            "stored user setting is corrupt; using built-in fallback without overwriting it"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    pub fn user_settings(&self, user_id: u64) -> Vec<UserSetting> {
        self.valid_user_settings(user_id)
    }

    fn user_setting_sender(&self, user_id: u64) -> broadcast::Sender<UserSettingChanged> {
        self.user_setting_channels
            .lock()
            .unwrap()
            .entry(user_id)
            .or_insert_with(|| broadcast::channel(USER_SETTING_CHANNEL_CAPACITY).0)
            .clone()
    }

    /// Validates and stores the user's native terminal theme. Passing `None`
    /// deletes the row, restoring the built-in theme. The update is published
    /// only after persistence succeeds and while holding the snapshot lock.
    pub fn set_user_terminal_theme(
        &self,
        user_id: u64,
        input: Option<&[u8]>,
    ) -> Result<Option<String>, DaemonError> {
        let normalized = input
            .map(crate::terminal_theme::normalize_terminal_theme)
            .transpose()
            .map_err(|error| DaemonError::Rejected(error.to_string()))?
            .map(|(_theme, normalized)| normalized);
        self.store_user_setting(
            user_id,
            crate::terminal_theme::USER_TERMINAL_THEME_KEY,
            normalized,
            "user terminal theme changed",
        )
    }

    /// Validates and stores the user's explicit appearance choice. Passing
    /// `None` deletes the row, which is how the browser returns to
    /// following the operating system.
    pub fn set_user_appearance(
        &self,
        user_id: u64,
        input: Option<&[u8]>,
    ) -> Result<Option<String>, DaemonError> {
        let normalized = input
            .map(crate::appearance::normalize_appearance)
            .transpose()
            .map_err(|error| DaemonError::Rejected(error.to_string()))?
            .map(|(_appearance, normalized)| normalized);
        self.store_user_setting(
            user_id,
            crate::appearance::USER_APPEARANCE_KEY,
            normalized,
            "user appearance changed",
        )
    }

    pub fn set_user_ui_theme(
        &self,
        user_id: u64,
        input: Option<&[u8]>,
    ) -> Result<Option<String>, DaemonError> {
        let normalized = input
            .map(crate::ui_theme::normalize_ui_theme)
            .transpose()
            .map_err(|error| DaemonError::Rejected(error.to_string()))?
            .map(|(_theme, normalized)| normalized);
        self.store_user_setting(
            user_id,
            crate::ui_theme::USER_UI_THEME_KEY,
            normalized,
            "user ui theme changed",
        )
    }

    /// Validates and stores how long the user's own web use holds back
    /// push to their devices. Passing `None` restores the default.
    pub fn set_push_web_idle_minutes(
        &self,
        user_id: u64,
        input: Option<&[u8]>,
    ) -> Result<Option<String>, DaemonError> {
        let normalized = input
            .map(crate::push::normalize_web_idle_minutes)
            .transpose()
            .map_err(|error| DaemonError::Rejected(error.to_string()))?
            .map(|(_minutes, normalized)| normalized);
        self.store_user_setting(
            user_id,
            crate::push::SETTING_PUSH_WEB_IDLE_MINUTES,
            normalized,
            "push web idle threshold changed",
        )
    }

    fn store_user_setting(
        &self,
        user_id: u64,
        key: &str,
        normalized: Option<String>,
        message: &'static str,
    ) -> Result<Option<String>, DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        self.storage
            .set_user_setting(user_id, key, normalized.as_deref())?;
        let setting = UserSettingChanged {
            key: key.to_string(),
            value_json: normalized.clone(),
        };
        let _ = self.user_setting_sender(user_id).send(setting);
        info!(user_id, key, reset = normalized.is_none(), "{}", message);
        Ok(normalized)
    }

    /// A disabled local worker stays stored but is not reachable.
    pub(crate) fn worker_is_online(&self, worker_id: u64) -> bool {
        (worker_id == LOCAL_WORKER_ID && self.local_worker_enabled)
            || self.workers.is_online(worker_id)
    }

    pub fn local_worker_enabled(&self) -> bool {
        self.local_worker_enabled
    }

    fn ensure_local_worker_enabled(&self, worker_id: u64) -> Result<(), DaemonError> {
        if worker_id == LOCAL_WORKER_ID && !self.local_worker_enabled {
            return Err(DaemonError::Rejected(
                "local worker is disabled for this daemon; select a remote worker".into(),
            ));
        }
        Ok(())
    }

    /// Loads a worker and overlays its live state for publishing.
    fn worker_snapshot(&self, worker_id: u64) -> Result<pm_protocol::domain::Worker, DaemonError> {
        let mut worker = self.storage.get_worker(worker_id)?;
        self.overlay_worker_runtime(&mut worker);
        Ok(worker)
    }

    /// Overlays live online state and, for the embedded local worker,
    /// this controller's own build version.
    fn overlay_worker_runtime(&self, worker: &mut pm_protocol::domain::Worker) {
        worker.online = self.worker_is_online(worker.id);
        if worker.id == LOCAL_WORKER_ID {
            worker.pm_version = crate::pm_build_version().to_string();
        }
    }

    fn seed_session_wait_journal(&self) {
        let Ok(sessions) = self.storage.snapshot(now_unix_ms()).map(|s| s.sessions) else {
            return;
        };
        let mut journal = self.session_wait_journal.lock().unwrap();
        for session in sessions {
            let Ok(project) = self.storage.get_project(session.project_id) else {
                continue;
            };
            let generation = self
                .storage
                .agent_terminal(session.id)
                .map(|terminal| terminal.generation)
                .unwrap_or_default();
            journal.last.insert(
                session.id,
                LastSessionLifecycle {
                    bucket_id: project.bucket_id,
                    generation,
                    state: session.state,
                },
            );
        }
    }

    fn record_session_lifecycle(&self, session: &Session) -> Option<SessionStateTransition> {
        let Ok(project) = self.storage.get_project(session.project_id) else {
            return None;
        };
        let generation = self
            .storage
            .agent_terminal(session.id)
            .map(|terminal| terminal.generation)
            .unwrap_or_default();
        let mut journal = self.session_wait_journal.lock().unwrap();
        let current = LastSessionLifecycle {
            bucket_id: project.bucket_id,
            generation,
            state: session.state,
        };
        let previous = journal.last.insert(session.id, current);
        let previous = previous?;
        if previous.bucket_id == current.bucket_id
            && previous.generation == current.generation
            && previous.state == current.state
        {
            return None;
        }
        let bucket = journal.buckets.entry(project.bucket_id).or_default();
        bucket.latest_cursor = bucket.latest_cursor.saturating_add(1);
        bucket.events.push_back(SessionLifecycleEvent {
            cursor: bucket.latest_cursor,
            session_id: session.id,
            generation,
            from: previous.state,
            to: session.state,
            timestamp_unix_ms: now_unix_ms(),
            headline: session.headline.clone(),
        });
        if bucket.events.len() > WAIT_JOURNAL_CAPACITY {
            bucket.events.pop_front();
        }
        Some(SessionStateTransition {
            bucket_id: project.bucket_id,
            generation,
            from: previous.state,
            to: session.state,
        })
    }

    /// Tells connected clients that something granted or moved a credential.
    ///
    /// The HTTP API mints and never reads back, so anything holding a session
    /// cannot steal an existing secret — it makes a new one, keyed to itself.
    /// That new identity outlives the password and the cookie it was minted
    /// with, so revocation is the only undo and revocation needs the user to
    /// know. A log line and a list they have to think to open is detection in
    /// principle and not in practice.
    pub(crate) fn publish_security_notice(
        &self,
        kind: pm_protocol::domain::SecurityNoticeKind,
        subject: &str,
        detail: &str,
    ) {
        warn!(kind = kind.as_str(), subject, "security notice: {detail}");
        self.publish(Event::SecurityNotice(pm_protocol::domain::SecurityNotice {
            kind,
            subject: subject.to_string(),
            detail: detail.to_string(),
        }));
    }

    pub(crate) fn send_session_alert(&self, alert: pm_protocol::domain::SessionAlert) {
        let _ = self.events_tx.send(Event::SessionAlert(alert));
    }

    pub(crate) fn publish(&self, mut event: Event) {
        if let Event::SessionChanged(session) = &mut event {
            session.program_status = self.program_status_records(session.id);
        }
        let mut alert = None;
        let lifecycle_changed = match &event {
            Event::SessionChanged(session) => {
                let transition = self.record_session_lifecycle(session);
                if let Some(t) = &transition {
                    if t.from != t.to
                        && !self.debounce_program_status_alert(
                            session.id,
                            t.bucket_id,
                            t.generation,
                            t.from,
                            t.to,
                            now_unix_ms(),
                        )
                    {
                        alert = self.observe_push_transition(
                            session,
                            t.bucket_id,
                            t.generation,
                            t.from,
                            t.to,
                        );
                    }
                }
                transition.is_some()
            }
            Event::SessionRemoved(session_id) => {
                self.forget_program_status(*session_id);
                self.supervisor_wake().forget_session(*session_id);
                self.stale_turn().forget_session(*session_id);
                let mut journal = self.session_wait_journal.lock().unwrap();
                if let Some(previous) = journal.last.remove(session_id) {
                    let bucket = journal.buckets.entry(previous.bucket_id).or_default();
                    bucket.latest_cursor = bucket.latest_cursor.saturating_add(1);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if let Event::SessionChanged(session) = &mut event {
            if let Some(activity) = self
                .pending_session_activity
                .lock()
                .unwrap()
                .get(&session.id)
            {
                overlay_session_activity(session, *activity);
            }
        }
        let _ = self.events_tx.send(event);
        // After the change that caused it, so a client rendering the alert
        // reads the session in the state the alert is about.
        if let Some(alert) = alert {
            let _ = self.events_tx.send(Event::SessionAlert(alert));
        }
        if lifecycle_changed {
            let _ = self.wait_events_tx.send(());
            // Both a child parking and a supervisor's own turn ending can
            // leave work unattended, and both are lifecycle changes.
            self.note_supervisor_wake_candidate();
        }
    }

    fn observe_agent_activity(&self, session_id: u64) {
        self.observe_session_activity(session_id, true, None);
    }

    fn observe_user_interaction(&self, session_id: u64, submitted: bool) {
        self.observe_session_activity(session_id, false, Some(submitted));
    }

    /// Whether an automated terminal write can be kept separate from user
    /// input and recent PTY output. The persisted clocks cover activity from
    /// before the latest checkpoint; the pending clocks cover every event
    /// observed since then without forcing a database write per keystroke.
    /// The activity clocks for a session, kept apart. `terminal_is_quiet_for`
    /// merges them, which is right for input that must not collide with
    /// anything; a reminder aimed at a parked agent has to tell a human at
    /// the keyboard apart from the agent's own output.
    pub(crate) fn session_activity_clocks(&self, session: &Session) -> (i64, i64, bool) {
        let mut agent = session.last_agent_activity_at_unix_ms;
        let mut user = session.last_user_interaction_at_unix_ms;
        let mut pending_input = false;
        if let Some(pending) = self
            .pending_session_activity
            .lock()
            .unwrap()
            .get(&session.id)
        {
            agent = agent.max(pending.last_agent_activity_at_unix_ms);
            user = user.max(pending.last_user_interaction_at_unix_ms);
            pending_input = pending.user_input_pending;
        }
        (agent, user, pending_input)
    }

    pub(crate) fn terminal_is_quiet_for(
        &self,
        session: &Session,
        now_unix_ms: i64,
        quiet_for_ms: i64,
    ) -> bool {
        let mut last_activity = session
            .last_agent_activity_at_unix_ms
            .max(session.last_user_interaction_at_unix_ms);
        if let Some(pending) = self
            .pending_session_activity
            .lock()
            .unwrap()
            .get(&session.id)
        {
            if pending.user_input_pending {
                return false;
            }
            last_activity = last_activity
                .max(pending.last_agent_activity_at_unix_ms)
                .max(pending.last_user_interaction_at_unix_ms);
        }
        last_activity == 0 || now_unix_ms.saturating_sub(last_activity) >= quiet_for_ms
    }

    pub(crate) fn adapter_has_lifecycle_hooks(&self, agent: AgentKind) -> bool {
        self.registry
            .get(agent)
            .is_ok_and(|adapter| adapter.has_lifecycle_hooks())
    }

    pub(crate) fn adapter_submits_after_bracketed_paste(&self, agent: AgentKind) -> bool {
        self.registry
            .get(agent)
            .is_ok_and(|adapter| adapter.submits_after_bracketed_paste())
    }

    /// How one message to this session's agent has to be framed for its
    /// TUI, so every caller frames it the same way.
    pub(crate) fn agent_message_plan(
        &self,
        session_id: u64,
        text: &str,
        submit: bool,
    ) -> Result<SupervisorInputPlan, DaemonError> {
        let session = self.storage.get_session(session_id)?;
        Ok(supervisor_input_plan(
            self.adapter_submits_after_bracketed_paste(session.agent),
            text,
            submit,
        ))
    }

    fn register_agent_generation(
        &self,
        terminal: &pm_protocol::domain::Terminal,
        agent: AgentKind,
        recovered_idle: bool,
    ) {
        if let Some(activity) = self
            .pending_session_activity
            .lock()
            .unwrap()
            .get_mut(&terminal.session_id)
        {
            // A fresh PTY generation cannot retain a line that was partially
            // composed in the previous one.
            activity.user_input_pending = false;
        }
        self.generation_lifecycle.lock().unwrap().insert(
            terminal.id,
            GenerationLifecycle {
                generation: terminal.generation,
                hook_authoritative: false,
                terminal_fallback_available: recovered_idle
                    && !self.adapter_has_lifecycle_hooks(agent),
                registered_at_unix_ms: now_unix_ms(),
                last_hook: None,
                hook_silence_reported: false,
            },
        );
    }

    /// A current session token is minted per terminal generation, so resolving
    /// the token before this call also rejects late hooks from older runs.
    fn establish_hook_authority(&self, session_id: u64, kind: HookKind) -> Result<(), DaemonError> {
        let terminal = self.storage.agent_terminal(session_id)?;
        let fresh = |generation: u64| GenerationLifecycle {
            generation,
            hook_authoritative: false,
            terminal_fallback_available: false,
            registered_at_unix_ms: now_unix_ms(),
            last_hook: None,
            hook_silence_reported: false,
        };
        let mut generations = self.generation_lifecycle.lock().unwrap();
        let lifecycle = generations
            .entry(terminal.id)
            .or_insert_with(|| fresh(terminal.generation));
        if lifecycle.generation != terminal.generation {
            *lifecycle = fresh(terminal.generation);
        }
        let seen_at = now_unix_ms();
        lifecycle.hook_authoritative = true;
        lifecycle.terminal_fallback_available = false;
        lifecycle.last_hook = Some((kind, seen_at));
        lifecycle.hook_silence_reported = false;
        drop(generations);
        if let Err(error) =
            self.storage
                .record_generation_hook_seen(terminal.id, terminal.generation, seen_at)
        {
            warn!(
                terminal = terminal.id,
                %error,
                "failed to record that this generation's hooks are working"
            );
        }
        Ok(())
    }

    /// When the last lifecycle hook arrived on a session's current agent
    /// terminal generation, for diagnostics that must tell a quiet agent
    /// apart from one whose hooks never reached the daemon.
    pub fn last_hook_seen(&self, session_id: u64) -> Option<(HookKind, i64)> {
        let terminal = self.storage.agent_terminal(session_id).ok()?;
        let generations = self.generation_lifecycle.lock().unwrap();
        let lifecycle = generations.get(&terminal.id)?;
        (lifecycle.generation == terminal.generation)
            .then_some(lifecycle.last_hook)
            .flatten()
    }

    /// The fallback is armed explicitly — by hookless recovery, by the
    /// hook-silence downgrade, or by the stale-turn watchdog — so an armed
    /// generation may consume it even after a hook was once authoritative.
    fn consume_terminal_fallback(&self, terminal: &pm_protocol::domain::Terminal) -> bool {
        let mut generations = self.generation_lifecycle.lock().unwrap();
        let Some(lifecycle) = generations.get_mut(&terminal.id) else {
            return false;
        };
        if lifecycle.generation != terminal.generation || !lifecycle.terminal_fallback_available {
            return false;
        }
        lifecycle.terminal_fallback_available = false;
        true
    }

    /// Arms one PTY-output promotion back to Working for a generation whose
    /// turn end was inferred rather than reported.
    fn arm_terminal_fallback(&self, terminal_id: u64, generation: u64) {
        self.materialize_generation_lifecycle(terminal_id, generation);
        let mut generations = self.generation_lifecycle.lock().unwrap();
        if let Some(lifecycle) = generations.get_mut(&terminal_id) {
            if lifecycle.generation == generation {
                lifecycle.terminal_fallback_available = true;
            }
        }
    }

    fn disable_terminal_fallback(&self, terminal: &pm_protocol::domain::Terminal) {
        let mut generations = self.generation_lifecycle.lock().unwrap();
        if let Some(lifecycle) = generations.get_mut(&terminal.id) {
            if lifecycle.generation == terminal.generation {
                lifecycle.terminal_fallback_available = false;
            }
        }
    }

    fn observe_session_activity(&self, session_id: u64, agent: bool, submitted: Option<bool>) {
        let now = now_unix_ms();
        let (activity, publish) = {
            let mut pending = self.pending_session_activity.lock().unwrap();
            let activity = pending.entry(session_id).or_default();
            if agent {
                activity.last_agent_activity_at_unix_ms =
                    activity.last_agent_activity_at_unix_ms.max(now);
            } else {
                activity.last_user_interaction_at_unix_ms =
                    activity.last_user_interaction_at_unix_ms.max(now);
                if let Some(submitted) = submitted {
                    activity.user_input_pending = !submitted;
                    if submitted {
                        activity.last_user_submit_at_unix_ms =
                            activity.last_user_submit_at_unix_ms.max(now);
                    }
                }
            }
            let publish = now.saturating_sub(activity.last_published_at_unix_ms)
                >= ACTIVITY_PUBLISH_INTERVAL_MS;
            if publish {
                activity.last_published_at_unix_ms = now;
            }
            (*activity, publish)
        };
        if publish {
            let _guard = self.state_lock.lock().unwrap();
            if let Ok(mut session) = self.storage.get_session(session_id) {
                overlay_session_activity(&mut session, activity);
                self.publish(Event::SessionChanged(session));
            }
        }
    }

    /// Accepts a coalesced local/worker output signal after verifying it
    /// still belongs to the current generation of an agent terminal. Output
    /// feeds the internal quiet-detection clock, never the activity clock
    /// the session list shows, and only the one-shot recovered hookless
    /// fallback may change semantic state.
    pub fn handle_terminal_activity(&self, terminal_id: u64, generation: u64) {
        let Ok(terminal) = self.storage.get_terminal(terminal_id) else {
            return;
        };
        if terminal.generation != generation
            || terminal.kind != pm_protocol::domain::TerminalKind::Agent
        {
            return;
        }
        self.observe_agent_activity(terminal.session_id);
        if self.program_status_decides(terminal.session_id) {
            return;
        }
        if !self.consume_terminal_fallback(&terminal) {
            return;
        }
        let _guard = self.state_lock.lock().unwrap();
        let Ok(session) = self.storage.get_session(terminal.session_id) else {
            return;
        };
        if session.state != SessionState::Idle {
            return;
        }
        if let Ok(updated) =
            self.storage
                .update_session_state(terminal.session_id, SessionState::Working, "")
        {
            self.publish(Event::SessionChanged(updated));
        }
    }

    /// Flushes all dirty terminal-activity clocks in one transaction and
    /// publishes at most one session event per checkpoint interval.
    pub fn checkpoint_session_activity(&self) {
        let updates: Vec<_> = self
            .pending_session_activity
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(&session_id, activity)| {
                let agent = (activity.last_agent_activity_at_unix_ms
                    > activity.checkpointed_agent_activity_at_unix_ms)
                    .then_some(activity.last_agent_activity_at_unix_ms);
                let user = (activity.last_user_interaction_at_unix_ms
                    > activity.checkpointed_user_interaction_at_unix_ms)
                    .then_some(activity.last_user_interaction_at_unix_ms);
                let submit = (activity.last_user_submit_at_unix_ms
                    > activity.checkpointed_user_submit_at_unix_ms)
                    .then_some(activity.last_user_submit_at_unix_ms);
                (agent.is_some() || user.is_some() || submit.is_some()).then_some(
                    SessionActivityUpdate {
                        session_id,
                        last_agent_activity_at_unix_ms: agent,
                        last_user_interaction_at_unix_ms: user,
                        last_user_submit_at_unix_ms: submit,
                    },
                )
            })
            .collect();
        if updates.is_empty() {
            return;
        }
        let _guard = self.state_lock.lock().unwrap();
        match self.storage.checkpoint_session_activity(&updates) {
            Ok(mut sessions) => {
                let pending = self.pending_session_activity.lock().unwrap();
                for (update, session) in updates.iter().zip(&mut sessions) {
                    if let Some(activity) = pending.get(&update.session_id) {
                        overlay_session_activity(session, *activity);
                    }
                }
                drop(pending);
                for session in sessions {
                    self.publish(Event::SessionChanged(session));
                }
                let mut pending = self.pending_session_activity.lock().unwrap();
                for update in updates {
                    if let Some(activity) = pending.get_mut(&update.session_id) {
                        if let Some(agent) = update.last_agent_activity_at_unix_ms {
                            activity.checkpointed_agent_activity_at_unix_ms =
                                activity.checkpointed_agent_activity_at_unix_ms.max(agent);
                        }
                        if let Some(user) = update.last_user_interaction_at_unix_ms {
                            activity.checkpointed_user_interaction_at_unix_ms =
                                activity.checkpointed_user_interaction_at_unix_ms.max(user);
                        }
                        if let Some(submit) = update.last_user_submit_at_unix_ms {
                            activity.checkpointed_user_submit_at_unix_ms =
                                activity.checkpointed_user_submit_at_unix_ms.max(submit);
                        }
                    }
                }
            }
            Err(error) => {
                warn!(error = %error, "failed to checkpoint session activity");
            }
        }
    }

    pub fn create_bucket(&self, name: &str) -> Result<u64, DaemonError> {
        self.create_bucket_with_workers(name, &[LOCAL_WORKER_ID], LOCAL_WORKER_ID, false)
    }

    pub fn create_bucket_with_workers(
        &self,
        name: &str,
        allowed_worker_ids: &[u64],
        default_worker_id: u64,
        is_default: bool,
    ) -> Result<u64, DaemonError> {
        for worker_id in allowed_worker_ids {
            if *worker_id != LOCAL_WORKER_ID {
                self.storage.get_worker(*worker_id)?;
            }
        }
        let _guard = self.state_lock.lock().unwrap();
        let bucket = self.storage.create_bucket_with_workers(
            name,
            allowed_worker_ids,
            default_worker_id,
            is_default,
        )?;
        let id = bucket.id;
        self.publish(Event::BucketChanged(bucket));
        Ok(id)
    }

    pub fn delete_bucket(&self, id: u64) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        self.storage.delete_bucket(id)?;
        self.publish(Event::BucketRemoved(id));
        Ok(())
    }

    pub fn create_project(
        &self,
        bucket_id: u64,
        name: &str,
        path: &str,
    ) -> Result<u64, DaemonError> {
        self.create_project_with_worker(bucket_id, name, path, None)
    }

    pub fn create_project_with_worker(
        &self,
        bucket_id: u64,
        name: &str,
        path: &str,
        worker_id: Option<u64>,
    ) -> Result<u64, DaemonError> {
        let allowed = self.storage.get_bucket(bucket_id)?.allowed_worker_ids;
        self.create_project_with_workers(bucket_id, name, path, worker_id, &allowed)
    }

    pub fn create_project_with_workers(
        &self,
        bucket_id: u64,
        name: &str,
        path: &str,
        worker_id: Option<u64>,
        allowed_worker_ids: &[u64],
    ) -> Result<u64, DaemonError> {
        let inherited_allowed;
        let allowed_worker_ids = if allowed_worker_ids.is_empty() {
            inherited_allowed = self.storage.get_bucket(bucket_id)?.allowed_worker_ids;
            &inherited_allowed
        } else {
            allowed_worker_ids
        };
        if let Some(worker_id) = worker_id {
            if worker_id != LOCAL_WORKER_ID {
                self.storage.get_worker(worker_id)?;
            }
        }
        let _guard = self.state_lock.lock().unwrap();
        let project = self.storage.create_project_with_workers(
            bucket_id,
            name,
            path,
            worker_id,
            allowed_worker_ids,
        )?;
        let id = project.id;
        self.publish(Event::ProjectChanged(project));
        Ok(id)
    }

    pub fn update_project(
        &self,
        id: u64,
        path: Option<&str>,
        permission_mode: Option<PermissionMode>,
        worker_id: Option<Option<u64>>,
    ) -> Result<(), DaemonError> {
        if let Some(Some(worker_id)) = worker_id {
            if worker_id != LOCAL_WORKER_ID {
                self.storage.get_worker(worker_id)?;
            }
        }
        let _guard = self.state_lock.lock().unwrap();
        let project =
            self.storage
                .update_project(id, path.map(str::trim), permission_mode, worker_id)?;
        self.publish(Event::ProjectChanged(project));
        Ok(())
    }

    pub fn set_bucket_permission_mode(
        &self,
        id: u64,
        mode: PermissionMode,
    ) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        let bucket = self.storage.set_bucket_permission_mode(id, mode)?;
        self.publish(Event::BucketChanged(bucket));
        Ok(())
    }

    pub fn set_project_permission_mode(
        &self,
        id: u64,
        mode: PermissionMode,
    ) -> Result<(), DaemonError> {
        self.update_project(id, None, Some(mode), None)
    }

    pub fn set_bucket_default_agent(
        &self,
        id: u64,
        agent: Option<AgentKind>,
    ) -> Result<(), DaemonError> {
        self.validate_default_agent(agent)?;
        let _guard = self.state_lock.lock().unwrap();
        let bucket = self.storage.set_bucket_default_agent(id, agent)?;
        self.publish(Event::BucketChanged(bucket));
        Ok(())
    }

    pub fn set_project_default_agent(
        &self,
        id: u64,
        agent: Option<AgentKind>,
    ) -> Result<(), DaemonError> {
        self.validate_default_agent(agent)?;
        let _guard = self.state_lock.lock().unwrap();
        let project = self.storage.set_project_default_agent(id, agent)?;
        self.publish(Event::ProjectChanged(project));
        Ok(())
    }

    /// Creates a provider account. The key is sealed before it reaches
    /// storage, and no API path ever returns it again.
    pub fn create_model_profile(
        &self,
        name: &str,
        api_key: Option<&str>,
    ) -> Result<u64, DaemonError> {
        let name = validated_profile_name(name)?;
        let sealed = api_key
            .map(validated_api_key)
            .transpose()?
            .map(|key| crate::secrets::seal_secret(&self.installation_secret(), &key));
        let _guard = self.state_lock.lock().unwrap();
        let profile = self
            .storage
            .create_model_profile(&name, sealed.as_deref(), now_unix_ms())?;
        let id = profile.id;
        info!(
            profile = id,
            name,
            key = sealed.is_some(),
            "model profile created"
        );
        self.publish(Event::ModelProfileChanged(profile));
        Ok(id)
    }

    /// Renames a profile and/or replaces its credential. Absent fields
    /// leave the stored value alone, so an edit never drops the key.
    pub fn update_model_profile(
        &self,
        id: u64,
        name: Option<&str>,
        api_key: Option<&str>,
        clear_api_key: bool,
    ) -> Result<(), DaemonError> {
        if api_key.is_some() && clear_api_key {
            return Err(DaemonError::Rejected(
                "cannot set and clear the api key in one update".into(),
            ));
        }
        let name = name.map(validated_profile_name).transpose()?;
        let sealed = match (api_key, clear_api_key) {
            (Some(key), _) => Some(Some(crate::secrets::seal_secret(
                &self.installation_secret(),
                &validated_api_key(key)?,
            ))),
            (None, true) => Some(None),
            (None, false) => None,
        };
        let _guard = self.state_lock.lock().unwrap();
        let profile = self.storage.update_model_profile(
            id,
            name.as_deref(),
            sealed.as_ref().map(|value| value.as_deref()),
            now_unix_ms(),
        )?;
        info!(
            profile = id,
            renamed = name.is_some(),
            key = match (&sealed, clear_api_key) {
                (Some(_), false) => "replaced",
                (Some(_), true) => "cleared",
                _ => "unchanged",
            },
            "model profile updated"
        );
        self.publish(Event::ModelProfileChanged(profile));
        Ok(())
    }

    /// Refused while anything still points at the profile: silently
    /// unsetting those references would drop their sessions back to the
    /// agent CLI's own account.
    pub fn delete_model_profile(&self, id: u64) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        let profile = self.storage.get_model_profile(id)?;
        let mut referents = self.storage.model_profile_referents(id)?;
        referents.extend(
            self.storage
                .sessions_holding_model_profile(id)?
                .into_iter()
                .map(|session| format!("session {session}")),
        );
        if !referents.is_empty() {
            return Err(DaemonError::Rejected(format!(
                "model profile {:?} is still used by {}",
                profile.name,
                referents.join(", ")
            )));
        }
        self.storage.delete_model_profile(id)?;
        info!(profile = id, name = profile.name, "model profile deleted");
        self.publish(Event::ModelProfileRemoved(id));
        Ok(())
    }

    pub fn set_model_profile_endpoint(
        &self,
        profile_id: u64,
        dialect: ModelDialect,
        model: &str,
        base_url: &str,
        background_model: &str,
    ) -> Result<(), DaemonError> {
        let model = model.trim();
        if model.is_empty() {
            return Err(DaemonError::Rejected(
                "endpoint model must not be empty".into(),
            ));
        }
        let base_url = base_url.trim();
        if !base_url.is_empty()
            && !base_url.starts_with("http://")
            && !base_url.starts_with("https://")
        {
            return Err(DaemonError::Rejected(format!(
                "endpoint base url must be an http(s) URL, not {base_url:?}"
            )));
        }
        let _guard = self.state_lock.lock().unwrap();
        let profile = self.storage.set_model_profile_endpoint(
            profile_id,
            dialect,
            model,
            base_url,
            background_model.trim(),
            now_unix_ms(),
        )?;
        if !base_url.is_empty() && !profile.key_set {
            warn!(
                profile = profile_id,
                dialect = dialect.as_str(),
                "endpoint sets a base url but the profile has no api key"
            );
        }
        self.publish(Event::ModelProfileChanged(profile));
        Ok(())
    }

    /// Refused while a session that would reselect this entry on resume
    /// still exists.
    pub fn delete_model_profile_endpoint(
        &self,
        profile_id: u64,
        dialect: ModelDialect,
    ) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        let current = self.storage.get_model_profile(profile_id)?;
        let holders: Vec<String> = self
            .storage
            .sessions_holding_model_profile_agents(profile_id)?
            .into_iter()
            .filter(|(_, agent)| {
                self.registry
                    .get(*agent)
                    .ok()
                    .and_then(|adapter| select_endpoint(adapter, &current.endpoints).ok())
                    .is_some_and(|entry| entry.dialect == dialect)
            })
            .map(|(session, _)| format!("session {session}"))
            .collect();
        if !holders.is_empty() {
            return Err(DaemonError::Rejected(format!(
                "the {} endpoint of model profile {:?} is still used by {}",
                dialect.as_str(),
                current.name,
                holders.join(", ")
            )));
        }
        let profile = self
            .storage
            .delete_model_profile_endpoint(profile_id, dialect)?;
        self.publish(Event::ModelProfileChanged(profile));
        Ok(())
    }

    pub fn set_bucket_model_profile(
        &self,
        id: u64,
        profile_id: Option<u64>,
    ) -> Result<(), DaemonError> {
        let bucket = self.storage.get_bucket(id)?;
        let agent = bucket.default_agent.unwrap_or(SYSTEM_FALLBACK_AGENT);
        self.validate_attached_profile(profile_id, agent)?;
        let _guard = self.state_lock.lock().unwrap();
        let bucket = self.storage.set_bucket_model_profile(id, profile_id)?;
        self.publish(Event::BucketChanged(bucket));
        Ok(())
    }

    pub fn set_project_model_profile(
        &self,
        id: u64,
        profile_id: Option<u64>,
    ) -> Result<(), DaemonError> {
        let project = self.storage.get_project(id)?;
        let (agent, _) = self.resolve_agent(&project, None)?;
        self.validate_attached_profile(profile_id, agent)?;
        let _guard = self.state_lock.lock().unwrap();
        let project = self.storage.set_project_model_profile(id, profile_id)?;
        self.publish(Event::ProjectChanged(project));
        Ok(())
    }

    /// Attach-time check against the currently resolved agent. The
    /// resolved agent can change later, so spawn re-checks; this only
    /// catches an obviously wrong attach up front.
    fn validate_attached_profile(
        &self,
        profile_id: Option<u64>,
        agent: AgentKind,
    ) -> Result<(), DaemonError> {
        let Some(profile_id) = profile_id else {
            return Ok(());
        };
        let profile = self.storage.get_model_profile(profile_id)?;
        self.select_profile_endpoint(&profile, agent).map(|_| ())
    }

    /// The profile for a session: the spawn override, else the
    /// project's, else the bucket's. Mirrors `resolve_agent`.
    fn resolve_model_profile(
        &self,
        project: &pm_protocol::domain::Project,
        spawn_override: Option<u64>,
    ) -> Result<Option<(u64, ModelProfileSource)>, DaemonError> {
        if let Some(id) = spawn_override {
            return Ok(Some((id, ModelProfileSource::Explicit)));
        }
        if let Some(id) = project.model_profile_id {
            return Ok(Some((id, ModelProfileSource::Project)));
        }
        Ok(self
            .storage
            .get_bucket(project.bucket_id)?
            .model_profile_id
            .map(|id| (id, ModelProfileSource::Bucket)))
    }

    /// The entry the agent can speak to, or a rejection naming the
    /// agent and what the profile does cover. Never falls back to the
    /// agent's own account.
    fn select_profile_endpoint(
        &self,
        profile: &ModelProfile,
        agent: AgentKind,
    ) -> Result<pm_protocol::domain::ModelProfileEndpoint, DaemonError> {
        let adapter = self.registry.get(agent)?;
        select_endpoint(adapter, &profile.endpoints)
            .cloned()
            .map_err(|covered| {
                let spoken = adapter
                    .dialects()
                    .iter()
                    .map(|d| d.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let covered = if covered.is_empty() {
                    "no dialects".to_string()
                } else {
                    covered
                        .iter()
                        .map(|d| d.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                DaemonError::Rejected(format!(
                    "model profile {:?} has no endpoint the {} agent can use: it speaks {spoken}, \
                     the profile covers {covered}",
                    profile.name,
                    agent.as_str()
                ))
            })
    }

    /// The selected entry plus the profile's opened credential, ready
    /// for an adapter or a remote worker.
    fn resolve_model_endpoint(
        &self,
        profile_id: Option<u64>,
        agent: AgentKind,
    ) -> Result<Option<ResolvedModelEndpoint>, DaemonError> {
        let Some(profile_id) = profile_id else {
            return Ok(None);
        };
        // An agent whose CLI can apply no endpoint at all always runs on
        // its own account, so an inherited profile passes it by instead
        // of rejecting every spawn it could never serve.
        if self
            .registry
            .get(agent)
            .is_ok_and(|adapter| adapter.dialects().is_empty())
        {
            return Ok(None);
        }
        let profile = self.storage.get_model_profile(profile_id)?;
        let entry = self.select_profile_endpoint(&profile, agent)?;
        let api_key = self
            .storage
            .model_profile_key_ciphertext(profile_id)?
            .and_then(|sealed| crate::secrets::open_secret(&self.installation_secret(), &sealed))
            .unwrap_or_default();
        if !entry.base_url.is_empty() && api_key.is_empty() {
            return Err(DaemonError::Rejected(format!(
                "model profile {:?} sets a base url for {} but has no api key",
                profile.name,
                entry.dialect.as_str()
            )));
        }
        Ok(Some(ResolvedModelEndpoint {
            dialect: entry.dialect,
            model: entry.model,
            base_url: entry.base_url,
            background_model: entry.background_model,
            api_key,
            provider_name: profile.name,
        }))
    }

    /// The effective permission mode for a session: the spawn override,
    /// else the project's, else the bucket's default.
    fn resolve_permission_mode(
        &self,
        project: &pm_protocol::domain::Project,
        spawn_override: PermissionMode,
    ) -> Result<PermissionMode, DaemonError> {
        let bucket = self.storage.get_bucket(project.bucket_id)?;
        Ok(spawn_override
            .or(project.permission_mode)
            .or(bucket.permission_mode))
    }

    /// The worker for a session with no explicit choice: the project
    /// override, else the bucket default, else the local worker. Mirrors
    /// the permission-mode cascade.
    fn resolve_worker(&self, project: &pm_protocol::domain::Project) -> Result<u64, DaemonError> {
        if let Some(worker_id) = project.worker_id {
            return Ok(worker_id);
        }
        Ok(self
            .storage
            .get_bucket(project.bucket_id)?
            .default_worker_id)
    }

    /// The workers a spawn may explicitly select for a project, as
    /// (worker id, worker name) pairs: every worker the project allows.
    /// A worker without a per-worker path is still selectable, because
    /// it inherits the project's own path, so selectability is about the
    /// allow-list and an unusable path is reported as such instead.
    pub fn spawn_host_choices(
        &self,
        project: &pm_protocol::domain::Project,
    ) -> Result<Vec<(u64, String)>, DaemonError> {
        let mut ids: Vec<u64> = project.allowed_worker_ids.clone();
        if !self.local_worker_enabled {
            ids.retain(|id| *id != LOCAL_WORKER_ID);
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .map(|id| Ok((id, self.storage.get_worker(id)?.name)))
            .collect()
    }

    /// The directory a session for this project starts in on one host:
    /// an explicit override, else the host's own mapping, else the
    /// project's configured path, else the home directory that host
    /// reported. A project with no path at all therefore runs somewhere
    /// sensible instead of refusing, and naming a path stays the way to
    /// put it anywhere else.
    pub fn effective_project_path(
        &self,
        project: &pm_protocol::domain::Project,
        worker_id: u64,
        cwd_override: Option<&str>,
    ) -> Result<String, DaemonError> {
        if let Some(cwd) = cwd_override.filter(|cwd| !cwd.trim().is_empty()) {
            return Ok(cwd.to_string());
        }
        let configured = if worker_id == LOCAL_WORKER_ID {
            project.path.clone()
        } else {
            self.storage
                .project_path_for_worker(project.id, worker_id)?
                .filter(|path| !path.trim().is_empty())
                .unwrap_or_else(|| project.path.clone())
        };
        if !configured.trim().is_empty() {
            return Ok(configured);
        }
        Ok(self.worker_home(worker_id))
    }

    /// The home directory a worker reported at registration, which is
    /// where a project with no path of its own runs. Empty from a worker
    /// that reported none, which leaves the path unset as before.
    fn worker_home(&self, worker_id: u64) -> String {
        // The local worker is this process, which never registers over
        // the wire and so never reported a root.
        if worker_id == LOCAL_WORKER_ID {
            return std::env::var("HOME").unwrap_or_default();
        }
        self.storage
            .get_worker(worker_id)
            .map(|worker| worker.default_project_root)
            .unwrap_or_default()
    }

    /// What the controller can settle about a project on a host without
    /// asking that host: whether it is reachable at all, and whether a
    /// path resolves for it. `None` means the path has to be checked on
    /// the host itself.
    fn local_project_host_state(
        &self,
        project: &pm_protocol::domain::Project,
        worker_id: u64,
        cwd_override: Option<&str>,
    ) -> Result<Option<ProjectHostState>, DaemonError> {
        if !self.worker_is_online(worker_id) {
            return Ok(Some(ProjectHostState::HostOffline));
        }
        let path = self.effective_project_path(project, worker_id, cwd_override)?;
        if path.trim().is_empty() {
            return Ok(Some(ProjectHostState::PathUnset));
        }
        Ok(None)
    }

    /// Whether a project can run on a host, and if not, why. The path is
    /// checked on the worker that holds it, so a remote worker answers for
    /// its own filesystem.
    pub async fn project_host_state(
        &self,
        project_id: u64,
        worker_id: u64,
        cwd_override: Option<&str>,
    ) -> Result<ProjectHostState, DaemonError> {
        let project = self.storage.get_project(project_id)?;
        if let Some(state) = self.local_project_host_state(&project, worker_id, cwd_override)? {
            return Ok(state);
        }
        let path = self.effective_project_path(&project, worker_id, cwd_override)?;
        if worker_id == LOCAL_WORKER_ID {
            let (status, detail) = crate::project_host::check_path(&path);
            return Ok(ProjectHostState::from_check(&path, status, detail));
        }
        let link = self.worker_link(worker_id)?;
        if link.protocol_version() < pm_protocol::WORKER_PROTOCOL_PATH_CHECK {
            return Ok(ProjectHostState::PathUnchecked {
                path,
                reason: "that host runs a pm too old to answer".into(),
            });
        }
        let rx = link.request_path_check(path.clone())?;
        match tokio::time::timeout(PATH_CHECK_TIMEOUT, rx).await {
            Ok(Ok(result)) => Ok(ProjectHostState::from_check(
                &path,
                result.status,
                result.detail,
            )),
            // No answer is no evidence against the path, so the spawn is
            // still allowed and the report says the check did not run.
            _ => Ok(ProjectHostState::PathUnchecked {
                path,
                reason: "that host did not answer in time".into(),
            }),
        }
    }

    /// Turns a verdict into the error a spawn is refused with, naming the
    /// project and the host the way an unconfigured host already is.
    pub fn project_host_error(
        &self,
        project: &pm_protocol::domain::Project,
        worker_id: u64,
        state: ProjectHostState,
    ) -> DaemonError {
        let host = self
            .storage
            .get_worker(worker_id)
            .map(|worker| worker.name)
            .unwrap_or_else(|_| format!("worker {worker_id}"));
        DaemonError::ProjectHost {
            state,
            project: project.name.clone(),
            host,
            worker_id,
        }
    }

    /// Refuses a launch the controller can already tell will not work,
    /// without asking the host. Every spawn goes through this, so no
    /// surface can start a session in a directory that does not resolve.
    fn ensure_project_runs_on_host(
        &self,
        project: &pm_protocol::domain::Project,
        worker_id: u64,
        cwd_override: Option<&str>,
    ) -> Result<(), DaemonError> {
        match self.local_project_host_state(project, worker_id, cwd_override)? {
            Some(state) => Err(self.project_host_error(project, worker_id, state)),
            None => Ok(()),
        }
    }

    /// Refuses a capability an older remote worker cannot decode. Local
    /// launches always use this daemon's own adapter registry.
    fn ensure_worker_supports_agent(
        &self,
        worker_id: u64,
        agent: AgentKind,
    ) -> Result<(), DaemonError> {
        if worker_id == LOCAL_WORKER_ID || agent != AgentKind::Antigravity {
            return Ok(());
        }
        if self.worker_link(worker_id)?.protocol_version()
            < pm_protocol::WORKER_PROTOCOL_ANTIGRAVITY
        {
            return Err(DaemonError::Rejected(format!(
                "this worker runs an older Puppet Master that cannot launch Antigravity; \
                 update it to a build speaking worker protocol {} or newer",
                pm_protocol::WORKER_PROTOCOL_ANTIGRAVITY
            )));
        }
        Ok(())
    }

    /// The host a spawn will use: the explicit choice, else the
    /// project's cascade default.
    pub fn resolve_spawn_worker(
        &self,
        project: &pm_protocol::domain::Project,
        worker_id: Option<u64>,
    ) -> Result<u64, DaemonError> {
        match worker_id {
            Some(worker_id) => Ok(worker_id),
            None => self.resolve_worker(project),
        }
    }

    /// Refuses a spawn whose project cannot run on the host it would
    /// use, asking that host about its own filesystem. Host resolution
    /// matches the spawn's own, so the verdict is about the host the
    /// session would actually start on.
    pub async fn ensure_spawnable(
        &self,
        project_id: u64,
        worker_id: Option<u64>,
        cwd_override: Option<&str>,
    ) -> Result<(), DaemonError> {
        let project = self.storage.get_project(project_id)?;
        let worker_id = self.resolve_spawn_worker(&project, worker_id)?;
        let state = self
            .project_host_state(project_id, worker_id, cwd_override)
            .await?;
        if state.can_launch() {
            return Ok(());
        }
        Err(self.project_host_error(&project, worker_id, state))
    }

    /// Resolves an explicit spawn host, given as a worker id or name,
    /// against the project's selectable hosts. Anything else fails with
    /// the valid choices.
    pub fn resolve_spawn_host(&self, project_id: u64, requested: &str) -> Result<u64, DaemonError> {
        let project = self.storage.get_project(project_id)?;
        let requested = requested.trim();
        let choices = self.spawn_host_choices(&project)?;
        let matched = match requested.parse::<u64>() {
            Ok(id) => choices.iter().find(|(choice_id, _)| *choice_id == id),
            Err(_) => choices.iter().find(|(_, name)| name == requested),
        };
        match matched {
            Some((id, _)) => Ok(*id),
            None => Err(DaemonError::HostNotConfigured {
                requested: requested.to_string(),
                valid: choices,
            }),
        }
    }

    fn resolve_agent(
        &self,
        project: &pm_protocol::domain::Project,
        spawn_override: Option<AgentKind>,
    ) -> Result<(AgentKind, AgentSelectionSource), DaemonError> {
        let bucket = self.storage.get_bucket(project.bucket_id)?;
        let (agent, source) = if let Some(agent) = spawn_override {
            (agent, AgentSelectionSource::Explicit)
        } else if let Some(agent) = project.default_agent {
            (agent, AgentSelectionSource::Project)
        } else if let Some(agent) = bucket.default_agent {
            (agent, AgentSelectionSource::Bucket)
        } else {
            (SYSTEM_FALLBACK_AGENT, AgentSelectionSource::Fallback)
        };
        self.registry
            .get(agent)
            .map_err(|_| DaemonError::AgentUnavailable {
                agent: agent.as_str(),
                selection_source: source.as_str(),
            })?;
        Ok((agent, source))
    }

    fn validate_default_agent(&self, agent: Option<AgentKind>) -> Result<(), DaemonError> {
        let Some(agent) = agent else { return Ok(()) };
        if agent == AgentKind::Test {
            return Err(DaemonError::Rejected(
                "default agent must be claude or codex".into(),
            ));
        }
        self.registry
            .get(agent)
            .map_err(|_| DaemonError::AgentUnavailable {
                agent: agent.as_str(),
                selection_source: "saved default",
            })?;
        Ok(())
    }

    pub fn delete_project(&self, id: u64) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        self.storage.delete_project(id)?;
        self.publish(Event::ProjectRemoved(id));
        Ok(())
    }

    pub fn list_workers(&self) -> Result<Vec<pm_protocol::domain::Worker>, DaemonError> {
        let mut workers = self.storage.list_workers()?;
        if !self.local_worker_enabled {
            workers.retain(|worker| worker.id != LOCAL_WORKER_ID);
        }
        for worker in &mut workers {
            self.overlay_worker_runtime(worker);
        }
        Ok(workers)
    }

    pub fn get_project(&self, id: u64) -> Result<pm_protocol::domain::Project, DaemonError> {
        Ok(self.storage.get_project(id)?)
    }

    pub fn worker_name(&self, id: u64) -> Result<String, DaemonError> {
        Ok(self.storage.get_worker(id)?.name)
    }

    /// Mints a one-time, time-limited enrollment token an operator runs on
    /// a worker machine to join it to this controller. Returns the token
    /// (shown once) and its expiry.
    pub fn create_worker_enrollment(&self, label: &str) -> Result<(String, i64), DaemonError> {
        self.mint_worker_enrollment(label, None, ConnectMode::Dial, "", &[])
    }

    /// Enrolls a host the controller dials instead of one that dials it.
    /// The endpoint is recorded on the enrollment because the controller has
    /// to reach the host before it can register at all.
    pub fn create_dialed_worker_enrollment(
        &self,
        label: &str,
        endpoint: &str,
    ) -> Result<(String, i64), DaemonError> {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            return Err(DaemonError::Rejected(
                "a host the controller dials needs an address to dial".into(),
            ));
        }
        self.mint_worker_enrollment(label, None, ConnectMode::Accept, endpoint, &[])
    }

    /// Mints an enrollment token that re-enrolls an existing host. It rotates
    /// that host's credential and pinned key in place, so its bucket
    /// defaults, project overrides, and session history survive.
    pub fn create_worker_reenrollment(&self, worker_id: u64) -> Result<(String, i64), DaemonError> {
        if worker_id == LOCAL_WORKER_ID {
            return Err(DaemonError::Rejected(
                "the local host does not enroll".into(),
            ));
        }
        let worker = self.storage.get_worker(worker_id)?;
        self.mint_worker_enrollment(
            &worker.name,
            Some(worker_id),
            worker.connect_mode,
            &worker.endpoint,
            &[],
        )
    }

    /// Re-enrolls an existing host and changes how it connects. Switching to
    /// a dialed host needs the address the controller will reach it at.
    pub fn create_worker_reenrollment_with_mode(
        &self,
        worker_id: u64,
        connect_mode: ConnectMode,
        endpoint: &str,
    ) -> Result<(String, i64), DaemonError> {
        if worker_id == LOCAL_WORKER_ID {
            return Err(DaemonError::Rejected(
                "the local host does not enroll".into(),
            ));
        }
        let endpoint = endpoint.trim();
        if connect_mode == ConnectMode::Accept && endpoint.is_empty() {
            return Err(DaemonError::Rejected(
                "a host the controller dials needs an address to dial".into(),
            ));
        }
        let worker = self.storage.get_worker(worker_id)?;
        self.mint_worker_enrollment(&worker.name, Some(worker_id), connect_mode, endpoint, &[])
    }

    pub fn create_worker_enrollment_with_buckets(
        &self,
        label: &str,
        connect_mode: ConnectMode,
        endpoint: &str,
        bucket_ids: &[u64],
    ) -> Result<(String, i64), DaemonError> {
        let endpoint = endpoint.trim();
        if connect_mode == ConnectMode::Accept && endpoint.is_empty() {
            return Err(DaemonError::Rejected(
                "a host the controller dials needs an address to dial".into(),
            ));
        }
        self.mint_worker_enrollment(label, None, connect_mode, endpoint, bucket_ids)
    }

    fn mint_worker_enrollment(
        &self,
        label: &str,
        worker_id: Option<u64>,
        connect_mode: ConnectMode,
        endpoint: &str,
        bucket_ids: &[u64],
    ) -> Result<(String, i64), DaemonError> {
        let token = crate::auth::generate_token();
        let hash = crate::auth::hash_token(&token);
        // The controller has to prove it holds this token before a host will
        // accept commands from it, and a MAC needs the token itself, so the
        // row keeps a sealed copy beside the hash it is looked up by.
        let sealed = crate::secrets::seal_secret(&self.installation_secret(), &token);
        let now = now_unix_ms();
        let expires = now + WORKER_ENROLLMENT_TTL_MS;
        match worker_id {
            Some(worker_id) => {
                warn!(
                    worker = worker_id,
                    host = label,
                    mode = connect_mode.as_str(),
                    endpoint,
                    "minted a re-enrollment token: whichever machine redeems it becomes this host"
                );
                self.storage.create_worker_enrollment(
                    &hash,
                    &sealed,
                    label,
                    Some(worker_id),
                    connect_mode,
                    endpoint,
                    now,
                    expires,
                )?
            }
            None => {
                let name = if label.trim().is_empty() {
                    match connect_mode {
                        ConnectMode::Accept => endpoint,
                        ConnectMode::Dial => "pending Host",
                    }
                } else {
                    label.trim()
                };
                let _guard = self.state_lock.lock().unwrap();
                let (worker, buckets, projects) =
                    self.storage.create_pending_worker_enrollment_with_buckets(
                        &hash,
                        &sealed,
                        name,
                        label,
                        connect_mode,
                        endpoint,
                        now,
                        expires,
                        bucket_ids,
                    )?;
                self.publish(Event::WorkerChanged(worker));
                for bucket in buckets {
                    self.publish(Event::BucketChanged(bucket));
                }
                for project in projects {
                    self.publish(Event::ProjectChanged(project));
                }
            }
        }
        Ok((token, expires))
    }

    /// The enrollment tokens a first-contact peer could be presenting. Rows
    /// are single-use and short-lived, so this is normally one or none.
    pub fn live_enrollment_tokens(&self) -> Vec<String> {
        let secret = self.installation_secret();
        self.storage
            .live_worker_enrollments(now_unix_ms())
            .unwrap_or_default()
            .iter()
            .filter_map(|sealed| crate::secrets::open_secret(&secret, sealed))
            .collect()
    }

    pub fn set_bucket_default_worker(&self, id: u64, worker_id: u64) -> Result<(), DaemonError> {
        if worker_id != LOCAL_WORKER_ID {
            self.storage.get_worker(worker_id)?;
        }
        let mut allowed = self.storage.get_bucket(id)?.allowed_worker_ids;
        if !allowed.contains(&worker_id) {
            allowed.push(worker_id);
        }
        self.set_bucket_workers(id, &allowed, worker_id, None)
    }

    /// Sets a bucket's allowed Hosts and default. Projects that reference
    /// a Host this removes are repaired onto `replacement`, which defaults
    /// to the bucket's new default Host.
    pub fn set_bucket_workers(
        &self,
        id: u64,
        allowed: &[u64],
        default_worker_id: u64,
        replacement: Option<u64>,
    ) -> Result<(), DaemonError> {
        let inherited;
        let allowed = if allowed.is_empty() {
            inherited = self.storage.get_bucket(id)?.allowed_worker_ids;
            &inherited
        } else {
            allowed
        };
        for worker_id in allowed {
            if *worker_id != LOCAL_WORKER_ID {
                self.storage.get_worker(*worker_id)?;
            }
        }
        let _guard = self.state_lock.lock().unwrap();
        let (bucket, projects) =
            self.storage
                .set_bucket_workers(id, allowed, default_worker_id, replacement)?;
        self.publish(Event::BucketChanged(bucket));
        for project in projects {
            self.publish(Event::ProjectChanged(project));
        }
        Ok(())
    }

    pub fn set_default_bucket(&self, id: u64) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        for bucket in self.storage.set_default_bucket(id)? {
            self.publish(Event::BucketChanged(bucket));
        }
        Ok(())
    }

    pub fn set_project_workers(
        &self,
        id: u64,
        allowed: &[u64],
        worker_id: Option<u64>,
    ) -> Result<(), DaemonError> {
        let inherited;
        let allowed = if allowed.is_empty() {
            inherited = self.storage.get_project(id)?.allowed_worker_ids;
            &inherited
        } else {
            allowed
        };
        let _guard = self.state_lock.lock().unwrap();
        let project = self.storage.set_project_workers(id, allowed, worker_id)?;
        self.publish(Event::ProjectChanged(project));
        Ok(())
    }

    pub fn set_project_worker(&self, id: u64, worker_id: Option<u64>) -> Result<(), DaemonError> {
        let project = self.storage.get_project(id)?;
        let bucket = self.storage.get_bucket(project.bucket_id)?;
        match worker_id {
            Some(worker_id) => {
                let mut bucket_allowed = bucket.allowed_worker_ids;
                if !bucket_allowed.contains(&worker_id) {
                    bucket_allowed.push(worker_id);
                }
                self.set_bucket_workers(
                    project.bucket_id,
                    &bucket_allowed,
                    bucket.default_worker_id,
                    None,
                )?;
                self.set_project_workers(id, &[worker_id], Some(worker_id))
            }
            None => {
                let mut allowed = project.allowed_worker_ids;
                if !allowed.contains(&bucket.default_worker_id) {
                    allowed.push(bucket.default_worker_id);
                }
                self.set_project_workers(id, &allowed, None)
            }
        }
    }

    /// Resolves a worker reference, given as a worker id or name,
    /// against the project's allowed workers for per-worker path
    /// configuration. Anything else fails with the allowed workers.
    pub fn resolve_project_path_worker(
        &self,
        project_id: u64,
        requested: &str,
    ) -> Result<u64, DaemonError> {
        let project = self.storage.get_project(project_id)?;
        let requested = requested.trim();
        let choices = project
            .allowed_worker_ids
            .iter()
            .map(|id| Ok((*id, self.storage.get_worker(*id)?.name)))
            .collect::<Result<Vec<(u64, String)>, StorageError>>()?;
        let matched = match requested.parse::<u64>() {
            Ok(id) => choices.iter().find(|(choice_id, _)| *choice_id == id),
            Err(_) => choices.iter().find(|(_, name)| name == requested),
        };
        match matched {
            Some((id, _)) => Ok(*id),
            None => Err(DaemonError::Rejected(format!(
                "worker {requested:?} is not an allowed worker for this project; allowed workers: {}",
                format_host_choices(&choices)
            ))),
        }
    }

    /// Sets or clears a project's launch path on one worker. Clearing
    /// falls back to the project's configured path, never an empty
    /// string.
    pub fn set_project_worker_path(
        &self,
        project_id: u64,
        worker_id: u64,
        path: Option<&str>,
    ) -> Result<(), DaemonError> {
        let project = self.storage.get_project(project_id)?;
        if worker_id != LOCAL_WORKER_ID {
            self.storage.get_worker(worker_id)?;
        }
        if !project.allowed_worker_ids.contains(&worker_id) {
            return Err(DaemonError::Rejected(format!(
                "worker {worker_id} is not allowed for project {project_id}"
            )));
        }
        let path = match path {
            Some(value) if value.trim().is_empty() => None,
            other => other,
        };
        let _guard = self.state_lock.lock().unwrap();
        let project = self
            .storage
            .set_project_worker_path(project_id, worker_id, path)?;
        self.publish(Event::ProjectChanged(project));
        Ok(())
    }

    pub fn remove_worker(&self, id: u64) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        if let Some(link) = self.workers.get(id) {
            self.workers.disconnect(&link);
        }
        self.storage.delete_worker(id, now_unix_ms())?;
        for session in self.storage.sessions_on_worker(id)? {
            self.publish(Event::SessionChanged(session));
        }
        self.publish(Event::WorkerRemoved(id));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_session(
        &self,
        project_id: u64,
        agent: AgentKind,
        task_title: &str,
        task_prompt: &str,
        cwd_override: Option<&str>,
        permission_mode: PermissionMode,
        worker_id: Option<u64>,
        items_api: bool,
        supervisor_api: bool,
        spawned_by_session_id: Option<u64>,
    ) -> Result<u64, DaemonError> {
        self.spawn_session_with_agent_override(
            project_id,
            Some(agent),
            task_title,
            task_prompt,
            cwd_override,
            permission_mode,
            worker_id,
            items_api,
            supervisor_api,
            spawned_by_session_id,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_session_with_agent_override(
        &self,
        project_id: u64,
        agent_override: Option<AgentKind>,
        task_title: &str,
        task_prompt: &str,
        cwd_override: Option<&str>,
        permission_mode: PermissionMode,
        worker_id: Option<u64>,
        items_api: bool,
        supervisor_api: bool,
        spawned_by_session_id: Option<u64>,
        model_profile_override: Option<u64>,
        initial_size: Option<(u16, u16)>,
    ) -> Result<u64, DaemonError> {
        let project = self.storage.get_project(project_id)?;
        let (agent, agent_source) = self.resolve_agent(&project, agent_override)?;
        let model_profile = self.resolve_model_profile(&project, model_profile_override)?;
        // The controller resolves the worker completely; the worker never
        // chooses. An explicit request wins, else the cascade default.
        let worker_id = self.resolve_spawn_worker(&project, worker_id)?;
        if !project.allowed_worker_ids.contains(&worker_id) {
            return Err(DaemonError::Rejected(format!(
                "worker {worker_id} is not allowed for project {project_id}"
            )));
        }
        self.ensure_local_worker_enabled(worker_id)?;
        if worker_id != LOCAL_WORKER_ID {
            self.storage.get_worker(worker_id)?;
        }
        self.ensure_project_runs_on_host(&project, worker_id, cwd_override)?;
        self.ensure_worker_supports_agent(worker_id, agent)?;
        self.launch_session(
            project_id,
            agent,
            agent_source,
            task_title,
            task_prompt,
            None,
            cwd_override,
            permission_mode,
            worker_id,
            items_api,
            supervisor_api,
            spawned_by_session_id,
            model_profile,
            initial_size,
        )
    }

    /// Resumes an ended session's agent conversation in place, reusing
    /// the session's own entry rather than spawning a successor, so the
    /// session list does not grow an extra row on every resume.
    pub fn resume_session(&self, id: u64) -> Result<u64, DaemonError> {
        // A resume replays the conversation without submitting a prompt,
        // so the agent comes back waiting for input rather than working.
        self.resume_session_with_state(id, SessionState::Starting, "")
    }

    fn resume_session_with_state(
        &self,
        id: u64,
        resumed_state: SessionState,
        state_detail: &str,
    ) -> Result<u64, DaemonError> {
        let session = self.storage.get_session(id)?;
        self.ensure_local_worker_enabled(session.worker_id)?;
        self.ensure_worker_supports_agent(session.worker_id, session.agent)?;
        let recovering =
            resumed_state == SessionState::Idle && state_detail == RECOVERED_STATE_DETAIL;
        if session.state.is_live() && !recovering {
            return Err(DaemonError::Rejected(format!(
                "session {id} is still live, attach to it instead"
            )));
        }
        self.forget_program_status(id);
        let transcript_path = self.storage.get_transcript_path(id)?;
        let has_recorded_path = transcript_path.as_ref().is_some_and(|p| !p.is_empty());
        let worker_reports_resumable = if session.worker_id == LOCAL_WORKER_ID && has_recorded_path
        {
            transcript_path
                .as_ref()
                .is_some_and(|p| std::path::Path::new(p).exists())
        } else {
            session.resumable
        };
        self.storage
            .set_agent_resumable(id, worker_reports_resumable)?;
        let resume_agent_session_id = if worker_reports_resumable
            || (!has_recorded_path && session.agent_session_id.is_some())
        {
            Some(session.agent_session_id.clone().ok_or_else(|| {
                DaemonError::Rejected(format!("session {id} has no agent session id to resume"))
            })?)
        } else if session.task_prompt.is_empty() {
            None
        } else {
            // Three different situations reach here and they are fixed in
            // different ways, so the refusal names which one it was rather
            // than reporting the shape they share.
            let cause = if session.agent_session_id.is_none() {
                "no agent session id was ever recorded for it"
            } else if session.worker_id == LOCAL_WORKER_ID {
                "its recorded transcript file is missing from disk"
            } else {
                "the worker holding it reports it is no longer resumable"
            };
            let recorded = transcript_path.as_deref().unwrap_or("");
            warn!(
                session = id,
                worker = session.worker_id,
                agent_session_id = session.agent_session_id.as_deref().unwrap_or(""),
                transcript_path = recorded,
                cause,
                "refusing to resume a session"
            );
            return Err(DaemonError::Rejected(format!(
                "session {id} cannot resume on worker {}: {cause}",
                session.worker_id
            )));
        };
        let adapter = self.registry.get(session.agent)?;
        // Reselected from the stored profile id, so an edit since the
        // spawn heals the resume instead of replaying stale values.
        let model_endpoint =
            self.resolve_model_endpoint(session.model_profile_id, session.agent)?;
        let permission_mode = session.permission_mode;
        self.remember_terminal_worker(self.storage.agent_terminal(id)?.id, session.worker_id);

        let token = crate::auth::generate_token();
        self.storage.set_session_token(id, &token)?;
        let terminal = self
            .storage
            .restart_terminal(self.storage.agent_terminal(id)?.id, now_unix_ms())?;
        self.register_agent_generation(
            &terminal,
            session.agent,
            resumed_state == SessionState::Idle && state_detail == RECOVERED_STATE_DETAIL,
        );
        {
            let _guard = self.state_lock.lock().unwrap();
            let reactivated = self.storage.reactivate_session(id, resumed_state)?;
            self.publish(Event::SessionChanged(reactivated));
        }
        self.publish(Event::TerminalChanged(terminal.clone()));
        let compiled_instructions = self.instructions_with_forward_inventory(
            id,
            session.agent,
            self.compile_and_snapshot_instructions(&session, terminal.generation)?,
        );
        if session.worker_id != LOCAL_WORKER_ID {
            let link = self.worker_link(session.worker_id)?;
            let initial_size = link.relay_size(terminal.id);
            let (initial_cols, initial_rows) =
                if link.protocol_version() >= pm_protocol::WORKER_PROTOCOL_SPAWN_INITIAL_SIZE {
                    (Some(initial_size.0), Some(initial_size.1))
                } else {
                    (None, None)
                };
            link.send(ControllerMsg::Spawn {
                session_id: id,
                terminal_id: terminal.id,
                generation: terminal.generation,
                agent: session.agent,
                task_prompt: session.task_prompt.clone(),
                permission_mode,
                cwd: session.cwd.clone(),
                session_token: token,
                resume_agent_session_id: resume_agent_session_id.unwrap_or_default(),
                truecolor: self.spawn_truecolor(),
                fullscreen: self.spawn_fullscreen(),
                compiled_instructions: compiled_instructions.clone(),
                model_endpoint: model_endpoint.clone().map(Box::new),
                initial_cols,
                initial_rows,
                program_status: self.spawn_program_status_on(&link),
            })?;
            let updated = self
                .storage
                .update_session_state(id, resumed_state, state_detail)?;
            self.publish(Event::SessionChanged(updated));
            return Ok(id);
        }
        let agent_port = adapter
            .wants_agent_port()
            .then(crate::inbox::reserve_agent_port)
            .flatten();
        if let Some(port) = agent_port {
            self.storage.set_agent_port(id, port)?;
        }
        let ctx = SpawnCtx {
            cwd: PathBuf::from(&session.cwd),
            task_prompt: session.task_prompt.clone(),
            permission_mode,
            compiled_instructions,
            model_endpoint,
            fullscreen: self.spawn_fullscreen(),
            integration: Integration {
                session_id: id,
                session_token: token,
                socket_path: self.socket_path.clone(),
                pm_exe: self.pm_exe.clone(),
                files_dir: self.session_files_dir.clone(),
                mcp_url: self.mcp_url(),
                agent_port,
            },
        };
        let plan = match &resume_agent_session_id {
            Some(agent_session_id) => match adapter.resume_command(&ctx, agent_session_id) {
                Ok(plan) => plan,
                Err(e) => return Err(self.fail_session(id, e.into())),
            },
            None => match adapter.spawn_command(&ctx) {
                Ok(plan) => plan,
                Err(e) => return Err(self.fail_session(id, e.into())),
            },
        };
        if let Some(agent_session_id) = &plan.agent_session_id {
            self.storage.set_agent_session_id(id, agent_session_id)?;
        }

        let initial_size = self.mux.current_size(terminal.id).ok();
        match self.mux.spawn(
            terminal.id,
            terminal.generation,
            id,
            &plan.spec,
            plan.detect_osc9_needs_input,
            false,
            self.spawn_truecolor(),
            initial_size,
            self.spawn_program_status(),
        ) {
            Ok(()) => {
                let _guard = self.state_lock.lock().unwrap();
                let updated = self
                    .storage
                    .update_session_state(id, resumed_state, state_detail)?;
                self.publish(Event::SessionChanged(updated));
                info!(
                    session = id,
                    agent = session.agent.as_str(),
                    fresh = resume_agent_session_id.is_none(),
                    "resumed session"
                );
                Ok(id)
            }
            Err(e) => Err(self.fail_session(id, e.into())),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn launch_session(
        &self,
        project_id: u64,
        agent: AgentKind,
        agent_source: AgentSelectionSource,
        task_title: &str,
        task_prompt: &str,
        resume_agent_session_id: Option<String>,
        cwd_override: Option<&str>,
        permission_mode: PermissionMode,
        worker_id: u64,
        items_api: bool,
        supervisor_api: bool,
        spawned_by_session_id: Option<u64>,
        model_profile: Option<(u64, ModelProfileSource)>,
        initial_size: Option<(u16, u16)>,
    ) -> Result<u64, DaemonError> {
        let project = self.storage.get_project(project_id)?;
        let adapter = self.registry.get(agent)?;
        // Selected before the session row exists so a profile that
        // cannot serve this agent rejects the spawn outright rather
        // than leaving a failed session behind.
        let model_endpoint = self.resolve_model_endpoint(model_profile.map(|(id, _)| id), agent)?;
        let permission_mode = self.resolve_permission_mode(&project, permission_mode)?;
        // Only a launch that submits a prompt puts the agent to work. An
        // interactive or resumed launch comes up waiting for the user, so
        // it stays in Starting until a lifecycle hook resolves it.
        let launched_state = if task_prompt.is_empty() || resume_agent_session_id.is_some() {
            SessionState::Starting
        } else {
            SessionState::Working
        };
        let cwd = match cwd_override {
            Some(c) if !c.trim().is_empty() => c.to_string(),
            _ => project.path.clone(),
        };

        let session = {
            let _guard = self.state_lock.lock().unwrap();
            let session = self.storage.create_session_with_model_profile(
                project_id,
                agent,
                agent_source,
                task_title,
                task_prompt,
                permission_mode,
                worker_id,
                items_api,
                supervisor_api,
                spawned_by_session_id,
                model_profile,
                now_unix_ms(),
            )?;
            self.publish(Event::SessionChanged(session.clone()));
            // Subscribed clients need the new agent terminal too: the web
            // opens the session by terminal identity right after spawn.
            self.publish(Event::TerminalChanged(
                self.storage.agent_terminal(session.id)?,
            ));
            session
        };
        self.remember_terminal_worker(self.storage.agent_terminal(session.id)?.id, worker_id);

        let token = crate::auth::generate_token();
        self.storage.set_session_token(session.id, &token)?;
        self.register_agent_generation(
            &self.storage.agent_terminal(session.id)?,
            session.agent,
            false,
        );
        let compiled_instructions = self.compile_and_snapshot_instructions(&session, 1)?;

        // A session on a remote worker is built and run there: the
        // controller sends the logical spawn and the worker's own adapter
        // injects worker-local hook and MCP integration.
        if worker_id != LOCAL_WORKER_ID {
            let link = match self.worker_link(worker_id) {
                Ok(l) => l,
                Err(e) => return Err(self.fail_session(session.id, e)),
            };
            // An explicit request is authoritative. Without one, use the
            // persisted worker mapping and finally the configured project
            // path—the same precedence exposed to clients in Project.
            let remote_cwd = match cwd_override {
                Some(value) if !value.trim().is_empty() => value.to_owned(),
                _ => self.effective_project_path(&project, worker_id, None)?,
            };
            self.storage.set_session_cwd(session.id, &remote_cwd)?;
            let spawn = ControllerMsg::Spawn {
                session_id: session.id,
                terminal_id: self.storage.agent_terminal(session.id)?.id,
                generation: 1,
                agent,
                task_prompt: task_prompt.to_string(),
                permission_mode,
                cwd: remote_cwd,
                session_token: token,
                resume_agent_session_id: resume_agent_session_id.clone().unwrap_or_default(),
                truecolor: self.spawn_truecolor(),
                fullscreen: self.spawn_fullscreen(),
                compiled_instructions: compiled_instructions.clone(),
                model_endpoint: model_endpoint.clone().map(Box::new),
                initial_cols: if link.protocol_version()
                    >= pm_protocol::WORKER_PROTOCOL_SPAWN_INITIAL_SIZE
                {
                    initial_size.map(|(c, _)| c)
                } else {
                    None
                },
                initial_rows: if link.protocol_version()
                    >= pm_protocol::WORKER_PROTOCOL_SPAWN_INITIAL_SIZE
                {
                    initial_size.map(|(_, r)| r)
                } else {
                    None
                },
                program_status: self.spawn_program_status_on(&link),
            };
            if let Err(e) = link.send(spawn) {
                return Err(self.fail_session(session.id, e.into()));
            }
            let _guard = self.state_lock.lock().unwrap();
            let updated = self
                .storage
                .update_session_state(session.id, launched_state, "")?;
            self.publish(Event::SessionChanged(updated));
            info!(
                session = session.id,
                worker = worker_id,
                agent = agent.as_str(),
                "spawned remote session"
            );
            return Ok(session.id);
        }

        let agent_port = adapter
            .wants_agent_port()
            .then(crate::inbox::reserve_agent_port)
            .flatten();
        if let Some(port) = agent_port {
            self.storage.set_agent_port(session.id, port)?;
        }
        let ctx = SpawnCtx {
            cwd: PathBuf::from(&cwd),
            task_prompt: task_prompt.to_string(),
            permission_mode,
            compiled_instructions,
            model_endpoint,
            fullscreen: self.spawn_fullscreen(),
            integration: Integration {
                session_id: session.id,
                session_token: token,
                socket_path: self.socket_path.clone(),
                pm_exe: self.pm_exe.clone(),
                files_dir: self.session_files_dir.clone(),
                mcp_url: self.mcp_url(),
                agent_port,
            },
        };
        self.storage.set_session_cwd(session.id, &cwd)?;
        let plan_result = match &resume_agent_session_id {
            Some(agent_session_id) => adapter.resume_command(&ctx, agent_session_id),
            None => adapter.spawn_command(&ctx),
        };
        let plan = match plan_result {
            Ok(plan) => plan,
            Err(e) => return Err(self.fail_session(session.id, e.into())),
        };
        if let Some(agent_session_id) = &plan.agent_session_id {
            self.storage
                .set_agent_session_id(session.id, agent_session_id)?;
        }

        let terminal = self.storage.agent_terminal(session.id)?;
        match self.mux.spawn(
            terminal.id,
            terminal.generation,
            session.id,
            &plan.spec,
            plan.detect_osc9_needs_input,
            false,
            self.spawn_truecolor(),
            initial_size,
            self.spawn_program_status(),
        ) {
            Ok(()) => {
                let _guard = self.state_lock.lock().unwrap();
                let updated = self
                    .storage
                    .update_session_state(session.id, launched_state, "")?;
                self.publish(Event::SessionChanged(updated));
                info!(
                    session = session.id,
                    agent = agent.as_str(),
                    resumed = resume_agent_session_id.is_some(),
                    "spawned session"
                );
                Ok(session.id)
            }
            Err(e) => Err(self.fail_session(session.id, e.into())),
        }
    }

    /// Marks a session failed with the error as detail and returns the
    /// error for propagation.
    fn fail_session(&self, session_id: u64, e: DaemonError) -> DaemonError {
        if let Ok(terminal) = self.storage.agent_terminal(session_id) {
            let _ = self
                .storage
                .set_agent_desired_running(session_id, terminal.id, false);
        }
        let _guard = self.state_lock.lock().unwrap();
        match self.storage.set_session_ended(
            session_id,
            SessionState::Failed,
            &e.to_string(),
            None,
            now_unix_ms(),
        ) {
            Ok(updated) => self.publish(Event::SessionChanged(updated)),
            Err(persist) => {
                error!(session = session_id, error = %persist, "failed to persist failure")
            }
        }
        e
    }

    /// Applies a lifecycle signal from an agent harness hook, and
    /// captures the agent's own session id and transcript path when the
    /// payload carried them (needed for later agent-native resume).
    /// Applies an agent lifecycle hook. Returns a Stop-hook block reason
    /// when a turn ends on a session that has never set a dashboard
    /// headline, so the caller can nudge the agent to report before it
    /// finishes. Delivered only on the controller-local hook path.
    pub fn handle_hook_event(
        &self,
        session_token: &str,
        kind: HookKind,
        detail: &str,
        agent_session_id: &str,
        transcript_path: &str,
        background_work: bool,
    ) -> Result<Option<String>, DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        if kind == HookKind::PromptSubmitted {
            // A new user turn supersedes a Supervisor's blocking wait. This
            // signal is independent of activity accounting and lifecycle
            // journaling, so merely waiting never makes an agent look busy.
            let _ = self.wait_cancel_tx.send(session_id);
            // The agent accepting a prompt is authoritative evidence that no
            // partially composed terminal input remains, including clients
            // whose submission key arrived in a separate transport write.
            if let Some(activity) = self
                .pending_session_activity
                .lock()
                .unwrap()
                .get_mut(&session_id)
            {
                activity.user_input_pending = false;
            }
        }
        self.establish_hook_authority(session_id, kind)?;
        self.storage
            .set_agent_identity(session_id, agent_session_id, transcript_path)?;
        if !transcript_path.is_empty() {
            self.storage
                .set_agent_resumable(session_id, std::path::Path::new(transcript_path).exists())?;
        }
        // Claude's notification hook fires both when a tool wants approval and
        // when its prompt has simply sat unanswered, and only the message tells
        // them apart. The unanswered one reports that the user went quiet, not
        // that the session wants anything, so it must not overwrite the idle
        // state the turn-ended hook already recorded. Anything unrecognized
        // still asks for attention, so a reworded message costs a spurious flag
        // rather than a swallowed approval prompt.
        if kind == HookKind::NeedsInput && detail.contains(PROMPT_UNANSWERED_NOTICE) {
            return Ok(None);
        }
        let (state, detail) = match kind {
            HookKind::NeedsInput => (SessionState::NeedsInput, detail),
            // A turn can finish while the agent still has a subagent,
            // shell command, monitor or workflow running. The turn is
            // over, but the session is not: reporting it idle invites a
            // supervisor to hand it more work, or a human to conclude it
            // stopped. The agent hooks again when the last of it lands.
            HookKind::TurnEnded if background_work => {
                (SessionState::Working, BACKGROUND_WORK_DETAIL)
            }
            HookKind::TurnEnded => (SessionState::Idle, ""),
            HookKind::TurnFailed => (SessionState::Idle, detail),
            HookKind::PromptSubmitted => (SessionState::Working, ""),
            HookKind::Started => {
                let _guard = self.state_lock.lock().unwrap();
                let session = self.storage.get_session(session_id)?;
                // The agent is up. A launch that submitted no prompt has
                // nothing running, so it settles into idle; anything
                // already working keeps its turn, since this hook also
                // fires on `/clear` and `/compact` mid-turn.
                let session = match self.deferred_hook_state(session_id) {
                    Some((SessionState::Starting, _)) => {
                        self.defer_hook_state(session_id, SessionState::Idle, "");
                        session
                    }
                    Some(_) => session,
                    None if session.state == SessionState::Starting => self
                        .storage
                        .update_session_state(session_id, SessionState::Idle, "")?,
                    None => session,
                };
                info!(session = session_id, "session identity captured");
                self.publish(Event::SessionChanged(session));
                return Ok(None);
            }
        };
        let _guard = self.state_lock.lock().unwrap();
        let current = self.storage.get_session(session_id)?;
        // A live Program Status root record decides the session state, so
        // the hook's own state is kept aside and its bookkeeping runs on that.
        let deferred = self.deferred_hook_state(session_id);
        let (previous_state, previous_detail) = deferred
            .clone()
            .unwrap_or_else(|| (current.state, current.state_detail.clone()));
        if kind == HookKind::PromptSubmitted {
            self.storage.clear_supervision_completion(session_id)?;
        }
        if matches!(
            kind,
            HookKind::TurnEnded | HookKind::TurnFailed | HookKind::NeedsInput
        ) {
            self.storage.record_agent_turn(session_id, now_unix_ms())?;
        }
        let preserves_needs_input = matches!(kind, HookKind::TurnEnded | HookKind::TurnFailed)
            && previous_state == SessionState::NeedsInput;
        let (hook_state, hook_detail) = if preserves_needs_input {
            (previous_state, previous_detail)
        } else {
            (state, detail.to_string())
        };
        let mut updated = if deferred.is_some() {
            self.defer_hook_state(session_id, hook_state, &hook_detail);
            info!(
                session = session_id,
                kind = kind.as_str(),
                deferred_state = hook_state.as_str(),
                "program status decides the session state, hook state kept aside"
            );
            current
        } else if preserves_needs_input {
            current
        } else {
            self.storage
                .update_session_state(session_id, state, detail)?
        };
        if matches!(kind, HookKind::TurnEnded | HookKind::TurnFailed) {
            let generation = self.storage.agent_terminal(session_id)?.generation;
            let revision = self.storage.session_transition_marks(session_id)?.0;
            let clean_completion = kind == HookKind::TurnEnded
                && hook_state == SessionState::Idle
                && hook_detail.is_empty();
            let silent = self.storage.finish_supervision_turn(
                session_id,
                generation,
                clean_completion.then_some(revision),
            )?;
            if kind == HookKind::TurnEnded
                && previous_state == SessionState::Working
                && !preserves_needs_input
                && !silent
            {
                self.storage
                    .record_turn_finished(session_id, now_unix_ms())?;
                updated = self.storage.get_session(session_id)?;
            }
        }
        // An agent whose activity signal is a level rather than an edge
        // reports the same state several times within one turn, and a
        // turn-ended hook that preserves needs-input reaches no new state
        // either. Calling those transitions reads as one event arriving
        // twice, so they say what they are while still logging that the
        // hook was received, which is the question a session that looks
        // stuck actually raises.
        if deferred.is_none() {
            if updated.state == previous_state {
                info!(
                    session = session_id,
                    kind = kind.as_str(),
                    state = updated.state.as_str(),
                    "hook event left the session state unchanged"
                );
            } else {
                info!(
                    session = session_id,
                    state = updated.state.as_str(),
                    "hook transition"
                );
            }
        }
        let nudge = (kind == HookKind::TurnEnded && updated.headline.trim().is_empty())
            .then(|| STOP_REPORT_NUDGE.to_string());
        self.publish(Event::SessionChanged(updated));
        Ok(nudge)
    }

    /// The per-session hook/report token, for tests and diagnostics.
    pub fn session_token(&self, id: u64) -> Result<Option<String>, DaemonError> {
        Ok(self.storage.get_session_token(id)?)
    }

    pub fn activity_reports(&self, session_id: u64) -> Result<Vec<ActivityReport>, DaemonError> {
        Ok(self.storage.activity_reports(session_id)?)
    }

    /// Closes a published forward and deletes it. The agent republishes
    /// if it wants the port back.
    /// Closes a forward. A directory share's forward cannot be closed on
    /// its own — the share would outlive it, with a server still running
    /// for a URL that had gone — so closing it closes the share, which
    /// is what the dashboard's close button means either way.
    pub fn close_forward(&self, forward_id: u64) -> Result<(), DaemonError> {
        if let Ok(Some(share)) = self.storage.dir_share_of_forward(forward_id) {
            return self.close_dir_share(&share);
        }
        self.forwards.remove_listener(forward_id);
        let _guard = self.state_lock.lock().unwrap();
        let forward = self.storage.delete_session_forward_by_id(forward_id)?;
        self.publish(Event::ForwardRemoved(forward.id));
        Ok(())
    }

    fn forward_bind_ip(&self, authenticated: bool) -> std::net::IpAddr {
        crate::forward::listener_bind_ip(
            self.forward_config.bind,
            self.http_bound.get().map(|a| a.ip()),
            authenticated,
        )
    }

    /// Tells a worker to apply its pending update now rather than waiting
    /// to go idle. Restarting kills the agent processes and they are
    /// resumed afterwards, so what an update costs is the turn each one
    /// was in the middle of, not the session.
    pub fn force_worker_update(&self, worker_id: u64) -> Result<(), DaemonError> {
        let link = self.workers.get(worker_id).ok_or(DaemonError::Worker(
            crate::workers::WorkerError::Offline(worker_id),
        ))?;
        link.send(pm_protocol::domain::ControllerMsg::UpdateNow)
            .map_err(DaemonError::Worker)
    }

    /// The host put into forward URLs, per [`crate::forward::resolve_forward_host`].
    /// `client_host` is the Host header of the request being answered,
    /// when the answer is going to a client over HTTP.
    pub(crate) fn forward_host(
        &self,
        client_host: Option<&str>,
    ) -> Result<String, crate::forward::ForwardHostError> {
        crate::forward::resolve_forward_host(
            self.public_url.as_deref(),
            client_host,
            self.forward_bind_ip(true),
        )
    }

    /// Overlays live listener state (URL, target reachability) on a
    /// stored forward row. The URL is left empty when the controller
    /// cannot name a host rather than guessing one.
    pub(crate) fn forward_view(&self, forward: SessionForward) -> SessionForward {
        self.forward_view_for_client(forward, None)
    }

    /// [`Self::forward_view`] for a client that reached the daemon over
    /// HTTP, so a link it renders can point back at the origin it used.
    /// The rewrite is per response and never reaches a stored row.
    pub(crate) fn forward_view_for_client(
        &self,
        mut forward: SessionForward,
        client_host: Option<&str>,
    ) -> SessionForward {
        if self.forwards.is_active(forward.id) {
            let port = self
                .forwards
                .listener_port(forward.id)
                .unwrap_or(crate::forward::HTTP_ROUTE_LISTENER_PORT);
            forward.listener_port = port;
            let url = match self.forward_host(client_host) {
                Ok(host) => {
                    if crate::forward_proxy::is_http_scheme(&forward.scheme) {
                        self.forward_mount(&forward, client_host)
                            .map(|mount| mount.url)
                            .unwrap_or_default()
                    } else {
                        format!("{}://{}:{}", forward.scheme, host, port)
                    }
                }
                Err(e) => {
                    debug!(forward = forward.id, error = %e, "no host for forward URL");
                    String::new()
                }
            };
            forward.url = url;
        }
        forward.target_reachable = self.forwards.target_reachable(forward.id);
        forward
    }

    /// How published HTTP forwards are mounted on this controller, from
    /// `PM_SHARE_DOMAIN` and `PM_SHARE_PORT_RANGE`.
    pub fn forward_mount_mode(&self) -> crate::forward_mount::MountMode {
        self.forward_config.mount_mode()
    }

    /// The public mount of one HTTP forward: the URL a client opens and
    /// the prefix its requests arrive under. This is the single source
    /// for what `publish_port` returns, what the dashboard and iOS
    /// render, and what the proxy rewrites, so none of them can disagree
    /// about where a forward lives.
    pub(crate) fn forward_mount(
        &self,
        forward: &SessionForward,
        client_host: Option<&str>,
    ) -> Option<crate::forward_mount::ForwardMount> {
        let base = self.forward_base_url(client_host)?;
        crate::forward_mount::forward_mount(
            &self.forward_mount_mode(),
            &crate::forward_mount::MountOrigin::parse(&base)?,
            forward.id,
            &forward.slug,
            self.forwards
                .listener_port(forward.id)
                .unwrap_or(crate::forward::HTTP_ROUTE_LISTENER_PORT),
        )
    }

    /// What a request's `Host` names under the share-domain mount, or
    /// `None` when the host is not a name under the share domain at all
    /// and the dashboard should answer it.
    pub(crate) fn share_host(&self, host: Option<&str>) -> Option<ShareHost> {
        let label = self.forward_mount_mode().host_forward_label(host?)?;
        let resolved = match crate::forward_mount::legacy_forward_label_id(&label) {
            // A named forward answers under its name alone, so the id
            // form is left to the rows that never took a name.
            Some(id) => self
                .storage
                .session_forward(id)
                .ok()
                .filter(|forward| forward.slug.is_empty()),
            None => self.storage.session_forward_by_slug(&label).ok().flatten(),
        };
        Some(match resolved {
            Some(forward) => ShareHost::Forward(forward.id),
            None => ShareHost::Unknown,
        })
    }

    pub(crate) fn forward_base_url(&self, client_host: Option<&str>) -> Option<String> {
        if let Some(base) = &self.public_url {
            return Some(base.trim_end_matches('/').to_string());
        }
        let bound = self.http_bound.get()?;
        let authority = client_host
            .map(str::to_string)
            .unwrap_or_else(|| bound.to_string());
        Some(format!("{}://{authority}", self.http_scheme()))
    }

    /// Rewrites forward URLs in a snapshot for the client receiving it.
    pub(crate) fn overlay_client_forward_urls(
        &self,
        forwards: &mut [SessionForward],
        client_host: Option<&str>,
    ) {
        if client_host.is_none() {
            return;
        }
        for forward in forwards {
            *forward = self.forward_view_for_client(forward.clone(), client_host);
        }
    }

    /// The same rewrite for a broadcast event, which carries the
    /// canonical view until it reaches a particular client.
    pub(crate) fn event_for_client(&self, event: Event, client_host: Option<&str>) -> Event {
        match (event, client_host) {
            (Event::ForwardChanged(forward), Some(_)) => {
                Event::ForwardChanged(self.forward_view_for_client(forward, client_host))
            }
            (event, _) => event,
        }
    }

    /// Publishes a worker-loopback port for the session the token
    /// authenticates: records the forward under its slug and binds its
    /// listener. Idempotent per (session, port, slug), which doubles as
    /// the recovery path for a resumed agent.
    pub async fn publish_port(
        self: &std::sync::Arc<Self>,
        session_token: &str,
        worker_port: u16,
        slug: &str,
        label: &str,
        scheme: &str,
    ) -> Result<SessionForward, DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        let slug = slug.trim();
        crate::forward_mount::validate_slug(slug)
            .map_err(|e| DaemonError::Rejected(format!("slug {slug:?} is not usable: {e}")))?;
        if matches!(scheme.to_ascii_lowercase().as_str(), "https" | "wss") {
            return Err(DaemonError::Rejected(
                "serve the forwarded target over loopback HTTP; the public URL supplies HTTPS"
                    .into(),
            ));
        }
        // Refuse before anything is recorded: a forward whose URL the
        // controller cannot name is worse than no forward, because the
        // agent hands the guess to the user as if it worked.
        self.forward_host(None).map_err(|e| {
            DaemonError::Rejected(format!("cannot publish port {worker_port}: {e}"))
        })?;
        let forward = {
            let _guard = self.state_lock.lock().unwrap();
            match self.storage.get_session_forward(session_id, worker_port)? {
                Some(existing) if existing.slug == slug => existing,
                // A slug is a hostname the agent has already handed to
                // the user, so it names the forward for as long as the
                // forward lives rather than being an editable field.
                Some(existing) if !existing.slug.is_empty() => {
                    return Err(DaemonError::Rejected(format!(
                        "port {worker_port} is already published as {}: close that forward \
                         before republishing the port as {slug}",
                        existing.slug
                    )));
                }
                Some(existing) => {
                    self.reject_taken_slug(slug)?;
                    self.storage.set_session_forward_slug(existing.id, slug)?;
                    self.storage.session_forward(existing.id)?
                }
                None => {
                    self.reject_taken_slug(slug)?;
                    let scheme = if scheme.is_empty() { "http" } else { scheme };
                    let label = if label.trim().is_empty() { slug } else { label };
                    self.storage.create_session_forward(
                        session_id,
                        worker_port,
                        0,
                        slug,
                        label,
                        scheme,
                        now_unix_ms(),
                    )?
                }
            }
        };
        self.ensure_forward_bound(forward).await
    }

    /// Publishes a directory the session's worker serves, so the agent
    /// hands over a path instead of running a server of its own. The
    /// share is the durable record and the forward carries it: the
    /// forward's id and slug are in the URL and survive every rebind,
    /// while the loopback port underneath is the worker's to choose.
    pub async fn publish_dir(
        self: &std::sync::Arc<Self>,
        session_token: &str,
        path: &str,
        slug: &str,
        label: &str,
    ) -> Result<SessionForward, DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        let slug = slug.trim();
        crate::forward_mount::validate_slug(slug)
            .map_err(|e| DaemonError::Rejected(format!("slug {slug:?} is not usable: {e}")))?;
        let path = path.trim();
        if path.is_empty() {
            return Err(DaemonError::Rejected(
                "name the directory to share, relative to the session working directory".into(),
            ));
        }
        self.forward_host(None)
            .map_err(|e| DaemonError::Rejected(format!("cannot publish {path}: {e}")))?;
        let session = self.storage.get_session(session_id)?;
        self.dir_share_capable(session.worker_id)?;
        let path = relative_to_cwd(&session.cwd, path);
        let share = {
            let _guard = self.state_lock.lock().unwrap();
            if let Some(existing) = self.storage.session_forward_by_slug(slug)? {
                return Err(DaemonError::Rejected(format!(
                    "slug {slug} is already published by forward {} on session {}",
                    existing.id, existing.session_id
                )));
            }
            self.storage
                .create_session_dir_share(session_id, &path, slug, label, now_unix_ms())?
                .0
        };
        // A share whose directory the worker will not serve is worse
        // than no share: the agent would hand the user a URL for it.
        match self.bind_dir_share(&share).await {
            Ok(forward) => Ok(forward),
            Err(e) => {
                let _guard = self.state_lock.lock().unwrap();
                let _ = self.storage.delete_session_dir_share(share.id);
                Err(e)
            }
        }
    }

    /// The directories a session publishes, each with the forward that
    /// carries its URL.
    pub fn list_dir_shares(
        &self,
        session_token: &str,
    ) -> Result<Vec<(crate::storage::SessionDirShare, SessionForward)>, DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        let mut shares = Vec::new();
        for share in self.storage.list_session_dir_shares(session_id)? {
            let forward = self.storage.dir_share_forward(share.id)?;
            shares.push((share, self.forward_view(forward)));
        }
        Ok(shares)
    }

    /// Closes one of the calling session's directory shares. The files
    /// are never touched: the share is a way to reach them, not a copy.
    pub fn unpublish_dir(&self, session_token: &str, slug: &str) -> Result<(), DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        let share = self
            .storage
            .session_dir_share_by_slug(slug.trim())?
            .filter(|share| share.session_id == session_id)
            .ok_or_else(|| {
                DaemonError::Rejected(format!("this session publishes no directory named {slug}"))
            })?;
        self.close_dir_share(&share)
    }

    fn close_dir_share(&self, share: &crate::storage::SessionDirShare) -> Result<(), DaemonError> {
        let forward_id = self.storage.dir_share_forward(share.id).map(|f| f.id).ok();
        self.stop_dir_share_server(share);
        let _guard = self.state_lock.lock().unwrap();
        if let Some(forward_id) = forward_id {
            self.forwards.remove_listener(forward_id);
        }
        self.storage.delete_session_dir_share(share.id)?;
        self.dir_share_binds.lock().unwrap().remove(&share.id);
        if let Some(forward_id) = forward_id {
            self.publish(Event::ForwardRemoved(forward_id));
        }
        info!(share = share.id, slug = %share.slug, "closed a directory share");
        Ok(())
    }

    /// Whether a forward's target answers HTTP/2 over cleartext, which
    /// is true only of a directory share's own server: it is the one
    /// target whose build this controller knows. A published port is
    /// somebody else's server, and may need an upgrade HTTP/2 cannot
    /// carry.
    pub(crate) fn dir_share_speaks_h2(&self, forward_id: u64, worker_id: u64) -> bool {
        if !self
            .storage
            .dir_share_of_forward(forward_id)
            .is_ok_and(|share| share.is_some())
        {
            return false;
        }
        if worker_id == LOCAL_WORKER_ID {
            return true;
        }
        self.workers.get(worker_id).is_some_and(|link| {
            link.protocol_version() >= pm_protocol::WORKER_PROTOCOL_DIR_SHARE_H2
        })
    }

    /// Refuses a worker that cannot serve a directory, rather than
    /// sending it a message it will never decode.
    fn dir_share_capable(&self, worker_id: u64) -> Result<(), DaemonError> {
        if worker_id == LOCAL_WORKER_ID {
            return Ok(());
        }
        let link = self.workers.get(worker_id).ok_or(DaemonError::Worker(
            crate::workers::WorkerError::Offline(worker_id),
        ))?;
        if link.protocol_version() < pm_protocol::WORKER_PROTOCOL_DIR_SHARE {
            return Err(DaemonError::Rejected(format!(
                "the host running this session speaks worker protocol {}, which cannot serve a \
                 published directory. Update that host, or start a server yourself and publish \
                 its port.",
                link.protocol_version()
            )));
        }
        Ok(())
    }

    /// Starts a share's server on whichever host holds the files and
    /// moves its forward to the port that server answers on, one caller
    /// at a time per share.
    async fn bind_dir_share(
        self: &std::sync::Arc<Self>,
        share: &crate::storage::SessionDirShare,
    ) -> Result<SessionForward, DaemonError> {
        let gate = self
            .dir_share_binds
            .lock()
            .unwrap()
            .entry(share.id)
            .or_default()
            .clone();
        // Bounded, because a bind waits on the host that holds the
        // files: a queue of callers behind an unresponsive one would
        // hold up the reconciler that restores the rest of the shares.
        let Ok(_serialized) = tokio::time::timeout(DIR_SHARE_BIND_TIMEOUT, gate.lock()).await
        else {
            return Err(DaemonError::Rejected(format!(
                "another caller is still binding the published directory {}",
                share.slug
            )));
        };
        self.bind_dir_share_serialized(share).await
    }

    async fn bind_dir_share_serialized(
        self: &std::sync::Arc<Self>,
        share: &crate::storage::SessionDirShare,
    ) -> Result<SessionForward, DaemonError> {
        let session = self.storage.get_session(share.session_id)?;
        let port = if session.worker_id == LOCAL_WORKER_ID {
            match self.local_dir_share_port(share.id) {
                Some(port) => port,
                None => {
                    let root = crate::dir_server::resolve_share_root(&session.cwd, &share.path)
                        .map_err(|e| DaemonError::Rejected(e.to_string()))?;
                    let server = crate::dir_server::serve(root).await.map_err(|e| {
                        DaemonError::Rejected(format!("serving {}: {e}", share.path))
                    })?;
                    self.keep_local_dir_server(share.id, server)
                }
            }
        } else {
            self.dir_share_capable(session.worker_id)?;
            let link = self
                .workers
                .get(session.worker_id)
                .ok_or(DaemonError::Worker(crate::workers::WorkerError::Offline(
                    session.worker_id,
                )))?;
            let rx =
                link.request_dir_share_serve(share.id, session.cwd.clone(), share.path.clone())?;
            let bound = tokio::time::timeout(DIR_SHARE_BIND_TIMEOUT, rx)
                .await
                .map_err(|_| {
                    DaemonError::Rejected(format!(
                        "the host running this session did not answer within {}s",
                        DIR_SHARE_BIND_TIMEOUT.as_secs()
                    ))
                })?
                .map_err(|_| {
                    DaemonError::Rejected("the host running this session disconnected".into())
                })?;
            if !bound.ok {
                return Err(DaemonError::Rejected(bound.error));
            }
            bound.port
        };
        self.adopt_dir_share_port(share.id, port).await
    }

    fn local_dir_share_port(&self, share_id: u64) -> Option<u16> {
        self.local_dir_servers
            .lock()
            .unwrap()
            .get(&share_id)
            .map(|server| server.port())
    }

    /// Files a freshly bound local server under its share, keeping any
    /// server already there and returning the port that survives.
    /// Replacing one would abort it, and the port it holds may already
    /// be the one the share's forward dials.
    fn keep_local_dir_server(
        &self,
        share_id: u64,
        server: crate::dir_server::DirShareServer,
    ) -> u16 {
        use std::collections::hash_map::Entry;
        match self.local_dir_servers.lock().unwrap().entry(share_id) {
            Entry::Occupied(existing) => existing.get().port(),
            Entry::Vacant(slot) => slot.insert(server).port(),
        }
    }

    /// Points a share's forward at the port its server now answers on.
    /// The forward row is updated rather than replaced, because its id
    /// is in the URL and the auth cookie already handed to the user.
    async fn adopt_dir_share_port(
        self: &std::sync::Arc<Self>,
        share_id: u64,
        port: u16,
    ) -> Result<SessionForward, DaemonError> {
        let forward = self.storage.dir_share_forward(share_id)?;
        if forward.worker_port != port {
            {
                let _guard = self.state_lock.lock().unwrap();
                self.storage
                    .set_session_forward_worker_port(forward.id, port)?;
            }
            // The listener dials the old port, so it cannot carry the
            // share any more.
            self.forwards.remove_listener(forward.id);
        }
        // A newly bound server has not been dialed yet, whatever a
        // verdict recorded against the previous one says.
        self.forwards.forget_target_reachable(forward.id);
        let forward = self.storage.session_forward(forward.id)?;
        let view = self.ensure_forward_bound(forward).await?;
        info!(
            share = share_id,
            forward = view.id,
            port,
            url = %view.url,
            "directory share bound"
        );
        Ok(view)
    }

    fn stop_dir_share_server(&self, share: &crate::storage::SessionDirShare) {
        // The forward's route stays registered, so nothing else here
        // drops the connections that reach the server being stopped.
        if let Ok(forward) = self.storage.dir_share_forward(share.id) {
            self.upstreams.forget(forward.id);
        }
        if self
            .local_dir_servers
            .lock()
            .unwrap()
            .remove(&share.id)
            .is_some()
        {
            return;
        }
        let Ok(session) = self.storage.get_session(share.session_id) else {
            return;
        };
        if session.worker_id == LOCAL_WORKER_ID {
            return;
        }
        self.send_dir_share_stop(session.worker_id, share.id);
    }

    /// Tells a worker to stop one share's server, and says whether it
    /// was sent. Gated on the worker supporting directory shares: a host
    /// that rolled back to an older build cannot decode the message, and
    /// would drop its control link over it rather than ignore it.
    fn send_dir_share_stop(&self, worker_id: u64, share_id: u64) -> bool {
        if self.dir_share_capable(worker_id).is_err() {
            return false;
        }
        self.workers
            .get(worker_id)
            .is_some_and(|link| link.send_dir_share_stop(share_id).is_ok())
    }

    /// Starts the servers for every directory a session publishes, on
    /// resume and after a controller restart.
    pub async fn start_session_dir_shares(self: &std::sync::Arc<Self>, session_id: u64) {
        let Ok(shares) = self.storage.list_session_dir_shares(session_id) else {
            return;
        };
        for share in shares {
            if self
                .storage
                .dir_share_forward(share.id)
                .is_ok_and(|forward| self.forwards.is_active(forward.id))
            {
                continue;
            }
            if let Err(e) = self.bind_dir_share(&share).await {
                warn!(share = share.id, slug = %share.slug, error = %e, "failed to serve a published directory");
            }
        }
    }

    /// Stops those servers when the session stops. The share rows stay,
    /// so resuming the session brings the same URLs back.
    pub fn stop_session_dir_shares(&self, session_id: u64) {
        let Ok(shares) = self.storage.list_session_dir_shares(session_id) else {
            return;
        };
        for share in shares {
            self.stop_dir_share_server(&share);
        }
    }

    /// Brings a reconnecting worker's directory servers in line with
    /// what the controller wants served on it: adopt the survivors on
    /// the ports they report, start what is missing, and stop anything
    /// it announced that no share still claims. That last case is the
    /// one that matters — a server nothing tracks would keep a
    /// directory readable after its share was closed.
    pub async fn reconcile_worker_dir_shares(
        self: &std::sync::Arc<Self>,
        worker_id: u64,
        announced: &[pm_protocol::domain::WorkerDirShare],
    ) {
        let Ok(desired) = self.storage.desired_dir_shares_on_worker(worker_id) else {
            return;
        };
        let unclaimed = announced
            .iter()
            .filter(|announced| !desired.iter().any(|share| share.id == announced.share_id));
        for announced in unclaimed {
            if self.send_dir_share_stop(worker_id, announced.share_id) {
                info!(
                    worker = worker_id,
                    share = announced.share_id,
                    "stopped a directory server the controller no longer publishes"
                );
            }
        }
        for share in desired {
            let bound = announced.iter().find(|a| a.share_id == share.id);
            let outcome = match bound {
                Some(bound) => self
                    .adopt_dir_share_port(share.id, bound.port)
                    .await
                    .map(|_| ()),
                None => self.bind_dir_share(&share).await.map(|_| ()),
            };
            if let Err(e) = outcome {
                warn!(
                    worker = worker_id,
                    share = share.id,
                    slug = %share.slug,
                    error = %e,
                    "failed to restore a published directory"
                );
            }
        }
    }

    /// Restarts the directory servers this controller runs itself, for
    /// the sessions the user wants running. A remote worker's shares are
    /// restored when it reconnects instead.
    pub async fn recover_local_dir_shares(self: &std::sync::Arc<Self>) {
        self.reconcile_worker_dir_shares(LOCAL_WORKER_ID, &[]).await;
    }

    /// Refuses a slug a wanted session already holds, and takes it back
    /// from one the user has shut down. A slug is a hostname, so the check
    /// spans every session on the controller rather than the publishing
    /// session alone.
    fn reject_taken_slug(&self, slug: &str) -> Result<(), DaemonError> {
        if let Some(other) = self.storage.desired_session_forward_by_slug(slug)? {
            return Err(DaemonError::Rejected(format!(
                "slug {slug} is already published by forward {} on session {}",
                other.id, other.session_id
            )));
        }
        if let Some(stale) = self.storage.session_forward_by_slug(slug)? {
            self.release_forward_name(&stale)?;
        }
        Ok(())
    }

    /// Gives up one forward's name. A published directory is reached only
    /// by its name, so nothing useful survives losing it and the whole
    /// share closes, while a port forward keeps its row for a resume.
    fn release_forward_name(&self, forward: &SessionForward) -> Result<(), DaemonError> {
        if let Some(share) = self.storage.dir_share_of_forward(forward.id)? {
            return self.close_dir_share(&share);
        }
        // Unlike a share, this keeps its row and its route, so nothing
        // else here would drop the connections the proxy holds to a
        // target the user has stopped publishing.
        self.upstreams.forget(forward.id);
        self.storage.clear_session_forward_slug(forward.id)?;
        let row = self.storage.session_forward(forward.id)?;
        info!(
            forward = forward.id,
            session = forward.session_id,
            slug = %forward.slug,
            "released a forward's name because its session is not wanted running"
        );
        self.publish(Event::ForwardChanged(self.forward_view(row)));
        Ok(())
    }

    /// Releases everything a session published, because the user has shut
    /// it down. Names a later session asks for are free from this moment
    /// rather than only when something tries to take them.
    fn release_published_names(&self, session_id: u64) {
        for share in self
            .storage
            .list_session_dir_shares(session_id)
            .unwrap_or_default()
        {
            if let Err(e) = self.close_dir_share(&share) {
                warn!(share = share.id, slug = %share.slug, error = %e,
                      "failed to close a published directory of a session being shut down");
            }
        }
        for forward in self
            .storage
            .list_session_forwards(session_id)
            .unwrap_or_default()
        {
            // Named or not, a session the user shut down keeps no
            // reusable connection to anything it published.
            self.upstreams.forget(forward.id);
            if forward.slug.is_empty() {
                continue;
            }
            if let Err(e) = self.release_forward_name(&forward) {
                warn!(forward = forward.id, slug = %forward.slug, error = %e,
                      "failed to release the name of a forward of a session being shut down");
            }
        }
    }

    /// Closes a session's published forward by worker port, MCP-side.
    pub fn unpublish_port(&self, session_token: &str, worker_port: u16) -> Result<(), DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        let forward = self
            .storage
            .get_session_forward(session_id, worker_port)?
            .ok_or_else(|| DaemonError::Rejected(format!("port {worker_port} is not published")))?;
        if !forward.source_path.is_empty() {
            return Err(DaemonError::Rejected(format!(
                "port {worker_port} belongs to the published directory {}: close it with \
                 unpublish_dir and its slug, {}",
                forward.source_path, forward.slug
            )));
        }
        self.close_forward(forward.id)
    }

    /// Binds a forward's listener when none is up, preferring its
    /// persisted port so a URL the agent already handed out keeps
    /// working across restarts.
    pub async fn ensure_forward_bound(
        self: &std::sync::Arc<Self>,
        forward: SessionForward,
    ) -> Result<SessionForward, DaemonError> {
        if self.forwards.is_active(forward.id) {
            return Ok(self.forward_view(forward));
        }
        let session = self.storage.get_session(forward.session_id)?;
        let mode = self.forward_mount_mode();
        let http = crate::forward_proxy::is_http_scheme(&forward.scheme);
        if http && !mode.binds_a_listener_per_forward() {
            self.forwards.insert_route(forward.id);
            let view = self.forward_view(forward);
            self.publish(Event::ForwardChanged(view.clone()));
            return Ok(view);
        }
        let range = if http {
            mode.port_range()
        } else {
            self.forward_config.port_range
        };
        let (listener, port) =
            crate::forward::bind_listener(self.forward_bind_ip(http), forward.listener_port, range)
                .await
                .map_err(|e| DaemonError::Rejected(format!("binding forward listener: {e}")))?;
        let task = if http {
            tokio::spawn(crate::http::serve_forward_listener(
                self.clone(),
                listener,
                forward.id,
            ))
        } else {
            tokio::spawn(run_forward_listener(
                std::sync::Arc::downgrade(self),
                listener,
                forward.id,
                session.worker_id,
                forward.worker_port,
                forward.scheme.clone(),
            ))
        };
        self.forwards.insert_listener(forward.id, port, task);
        let _guard = self.state_lock.lock().unwrap();
        if port != forward.listener_port {
            self.storage
                .set_session_forward_listener_port(forward.id, port)?;
        }
        let view = self.forward_view(self.storage.session_forward(forward.id)?);
        info!(
            forward = forward.id,
            session = forward.session_id,
            port = forward.worker_port,
            url = %view.url,
            "forward listener bound"
        );
        self.publish(Event::ForwardChanged(view.clone()));
        Ok(view)
    }

    /// What a session is told about the forwards it published, empty when
    /// it published none. Built from the same views the dashboard renders,
    /// so the URL and the probe result the agent reads are the live ones.
    pub(crate) fn session_forward_inventory(&self, session_id: u64) -> String {
        let Ok(forwards) = self.storage.list_session_forwards(session_id) else {
            return String::new();
        };
        let views: Vec<SessionForward> = forwards
            .into_iter()
            .filter(|forward| forward.source_path.is_empty())
            .map(|forward| self.forward_view(forward))
            .collect();
        pm_adapters::forward_inventory(&views)
    }

    /// The instructions a resumed session starts with: its compiled
    /// layers, with the inventory of the forwards it published under
    /// them. Skipped for an agent that carries its own session-start
    /// context, which reads the same inventory from its session-start
    /// hook. The snapshot keeps the layers alone, because hashing this
    /// resume's runtime state would make every resume read as an
    /// instruction change.
    fn instructions_with_forward_inventory(
        &self,
        session_id: u64,
        agent: AgentKind,
        compiled: String,
    ) -> String {
        if self
            .registry
            .get(agent)
            .is_ok_and(|adapter| adapter.injects_session_start_context())
        {
            return compiled;
        }
        let inventory = self.session_forward_inventory(session_id);
        if inventory.is_empty() {
            return compiled;
        }
        info!(
            session = session_id,
            agent = agent.as_str(),
            "gave a resumed session its forwards in its instructions"
        );
        if compiled.is_empty() {
            return inventory;
        }
        format!("{compiled}\n\n{inventory}")
    }

    /// The calling session's own inventory, for the Claude session-start
    /// hook. The token is the authorization: it resolves to one live
    /// session, and no other session's forwards are reachable through it.
    pub fn forward_inventory_for_token(&self, session_token: &str) -> Result<String, DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        Ok(self.session_forward_inventory(session_id))
    }

    /// Binds every forward a session published as a port (no-ops for
    /// already-bound ones). A directory share's forward is bound by
    /// [`Self::start_session_dir_shares`] instead, because its port is
    /// not known until its server is running.
    pub async fn bind_session_forwards(self: &std::sync::Arc<Self>, session_id: u64) {
        let Ok(forwards) = self.storage.list_session_forwards(session_id) else {
            return;
        };
        for forward in forwards
            .into_iter()
            .filter(|forward| forward.source_path.is_empty())
        {
            let id = forward.id;
            if let Err(e) = self.ensure_forward_bound(forward).await {
                warn!(forward = id, error = %e, "failed to bind forward listener");
            }
        }
    }

    /// Unbinds every listener a session owns. Rows stay, so resuming
    /// the session brings the same URLs back.
    pub fn unbind_session_forwards(&self, session_id: u64) {
        let Ok(forwards) = self.storage.list_session_forwards(session_id) else {
            return;
        };
        let _guard = self.state_lock.lock().unwrap();
        for forward in forwards {
            if self.forwards.is_active(forward.id) {
                self.forwards.remove_listener(forward.id);
                if let Ok(row) = self.storage.session_forward(forward.id) {
                    self.publish(Event::ForwardChanged(self.forward_view(row)));
                }
            }
        }
    }

    /// Rebinds the forwards of every desired-running session, run at
    /// startup so persisted URLs survive a controller restart.
    pub async fn recover_forwards(self: &std::sync::Arc<Self>) {
        let Ok(forwards) = self.storage.desired_session_forwards() else {
            return;
        };
        for forward in forwards
            .into_iter()
            .filter(|forward| forward.source_path.is_empty())
        {
            let id = forward.id;
            if let Err(e) = self.ensure_forward_bound(forward).await {
                warn!(forward = id, error = %e, "failed to rebind forward listener");
            }
        }
    }

    /// Records a stream-open outcome and publishes the forward when the
    /// outcome changed, so the UI can show a dead target. This measures
    /// the controller-to-worker hop only: whether the agent's server is
    /// listening, not whether any client can reach the listener.
    /// Reusable upstream connections the proxy currently holds for a
    /// forward, across every target port pooled for it.
    pub fn idle_upstreams(&self, forward_id: u64) -> usize {
        self.upstreams.idle_count(forward_id)
    }

    pub(crate) fn forward_response_timeout(&self) -> std::time::Duration {
        self.forward_config
            .response_timeout
            .unwrap_or(crate::forward::RESPONSE_TIMEOUT)
    }

    pub(crate) fn record_forward_target_reachable(&self, forward_id: u64, reachable: bool) {
        if !self.forwards.record_target_reachable(forward_id, reachable) {
            return;
        }
        let _guard = self.state_lock.lock().unwrap();
        if let Ok(row) = self.storage.session_forward(forward_id) {
            self.publish(Event::ForwardChanged(self.forward_view(row)));
        }
    }

    pub fn list_workspaces(&self, user_id: u64) -> Result<Vec<Workspace>, DaemonError> {
        Ok(self.storage.list_workspaces(user_id)?)
    }

    /// Every user's workspaces, for owner-trusted socket clients.
    pub fn all_workspaces(&self) -> Result<Vec<Workspace>, DaemonError> {
        Ok(self.storage.list_all_workspaces()?)
    }

    pub fn reorder_workspaces(
        &self,
        user_id: u64,
        workspace_ids: &[u64],
    ) -> Result<(), DaemonError> {
        Ok(self.storage.reorder_workspaces(user_id, workspace_ids)?)
    }

    pub fn create_workspace(
        &self,
        user_id: u64,
        name: &str,
        layout_json: &str,
    ) -> Result<Workspace, DaemonError> {
        Ok(self
            .storage
            .create_workspace(user_id, name, layout_json, now_unix_ms())?)
    }

    pub fn update_workspace(
        &self,
        user_id: u64,
        id: u64,
        name: &str,
        layout_json: &str,
    ) -> Result<Workspace, DaemonError> {
        Ok(self
            .storage
            .update_workspace(user_id, id, name, layout_json, now_unix_ms())?)
    }

    pub fn delete_workspace(&self, user_id: u64, id: u64) -> Result<(), DaemonError> {
        Ok(self.storage.delete_workspace(user_id, id)?)
    }

    /// Creates or updates one item. `actor_session_id` is `Some` for
    /// agent (MCP) writes, which are subject to the storage sticky
    /// rules; human writes pass `None`. Returns the non-body fields that
    /// were truncated to their caps so the MCP path can tell the agent.
    pub fn upsert_item(
        &self,
        bucket_id: u64,
        write: &ItemWrite,
        actor_session_id: Option<u64>,
    ) -> Result<(Item, ItemOutcome, Vec<&'static str>), DaemonError> {
        // Item numbers are never meaningful without a bucket. In particular,
        // bucket 0 must not become a legacy-surrogate escape hatch.
        let bucket_id = match bucket_id {
            0 => {
                return Err(DaemonError::Rejected(
                    "a bucket is required; unqualified legacy item ids are unsupported".into(),
                ))
            }
            bucket_id => bucket_id,
        };
        crate::storage::validate_item_body(write.body.as_deref())?;
        let mut truncated = Vec::new();
        let mut cap = |field: &'static str, value: Option<&String>, max: usize| {
            let value = value?;
            if value.chars().count() > max {
                truncated.push(field);
            }
            Some(truncate_chars(value, max))
        };
        let up = ItemUpsert {
            id: write.id,
            external_key: cap(
                "external_key",
                write.external_key.as_ref(),
                crate::storage::ITEM_KEY_MAX,
            ),
            title: cap(
                "title",
                write.title.as_ref(),
                crate::storage::ITEM_TITLE_MAX,
            ),
            body: write.body.clone(),
            question: cap(
                "question",
                write.question.as_ref(),
                crate::storage::ITEM_NOTE_MAX,
            ),
            status: write.status,
            priority: write.priority,
            source_kind: write.source_kind,
            source_detail: cap(
                "source_detail",
                write.source_detail.as_ref(),
                crate::storage::ITEM_SOURCE_DETAIL_MAX,
            ),
            url: cap("url", write.url.as_ref(), crate::storage::ITEM_URL_MAX),
            project_id: write.project_id,
            clear_project: write.clear_project,
            due_at_unix_ms: write.due_at_unix_ms,
            clear_due: write.clear_due,
            blocked_by: write.blocked_by.clone(),
            note: cap("note", write.note.as_ref(), crate::storage::ITEM_NOTE_MAX),
            link_session_id: write.link_session_id,
        };
        let _guard = self.state_lock.lock().unwrap();
        let create_project_id = if write.project_id.is_none() {
            actor_session_id
                .and_then(|session_id| self.storage.get_session(session_id).ok())
                .and_then(|session| self.storage.get_project(session.project_id).ok())
                .filter(|project| project.bucket_id == bucket_id)
                .map(|project| project.id)
        } else {
            None
        };
        let (item, outcome) = self.storage.upsert_item_with_project_default(
            bucket_id,
            &up,
            actor_session_id,
            create_project_id,
            now_unix_ms(),
        )?;
        if outcome != ItemOutcome::Unchanged {
            info!(
                item = item.id,
                bucket = bucket_id,
                outcome = outcome.as_str(),
                "item upsert"
            );
            self.publish(Event::ItemChanged(item.clone()));
        }
        Ok((item, outcome, truncated))
    }

    pub async fn respond_to_item(
        &self,
        bucket_id: u64,
        item_id: u64,
        text: &str,
        target: RespondTarget,
    ) -> Result<Option<u64>, DaemonError> {
        if text.trim().is_empty() {
            return Err(DaemonError::Rejected("reply must not be empty".into()));
        }
        if text.chars().count() > crate::storage::ITEM_NOTE_MAX {
            return Err(DaemonError::Rejected(format!(
                "reply exceeds {} characters",
                crate::storage::ITEM_NOTE_MAX
            )));
        }

        if bucket_id == 0 {
            return Err(DaemonError::Rejected(
                "a bucket is required; unqualified legacy item ids are unsupported".into(),
            ));
        }
        let item = self.storage.get_item(bucket_id, item_id)?;
        let new_supervisor = if let RespondTarget::NewSupervisor { project_id } = &target {
            let project = self.storage.get_project(*project_id)?;
            if project.bucket_id != item.bucket_id {
                return Err(DaemonError::Rejected(format!(
                    "project {project_id} is not in item {item_id}'s bucket"
                )));
            }
            let notes = self.storage.item_notes(bucket_id, item_id)?;
            Some((*project_id, spawn_on_reply_prompt(&item, &notes, text)))
        } else {
            None
        };
        let routed = match target {
            RespondTarget::Session(session_id) => {
                let session = self.storage.get_session(session_id)?;
                if !session.supervisor_api {
                    return Err(DaemonError::Rejected(format!(
                        "session {session_id} does not offer the supervisor API"
                    )));
                }
                if !session.state.is_live() {
                    return Err(DaemonError::Rejected(format!(
                        "session {session_id} already ended"
                    )));
                }
                let project = self.storage.get_project(session.project_id)?;
                if project.bucket_id != item.bucket_id {
                    return Err(DaemonError::Rejected(format!(
                        "session {session_id} is not in item {item_id}'s bucket"
                    )));
                }
                Some((session_id, self.storage.agent_terminal(session_id)?))
            }
            RespondTarget::ReplyOnly => None,
            RespondTarget::NewSupervisor { .. } => None,
        };

        let updated = {
            let _guard = self.state_lock.lock().unwrap();
            let updated = self.storage.respond_to_item(
                bucket_id,
                item_id,
                text,
                routed.is_some() || new_supervisor.is_some(),
                now_unix_ms(),
            )?;
            self.publish(Event::ItemChanged(updated.clone()));
            updated
        };
        if let Some((session_id, terminal)) = routed {
            let input_state = self.storage.get_session(session_id)?.state;
            let notice = format!(
                "User replied on item {}. Read the item and continue.",
                updated.id
            );
            let plan = self.agent_message_plan(session_id, &notice, true)?;
            let delivery = self
                .deliver_agent_message(session_id, &notice, true, plan, false, true)
                .await
                .map_err(|_| {
                    DaemonError::Rejected(format!(
                        "session {session_id}'s agent did not accept the reply notice"
                    ))
                })?;
            self.observe_user_interaction(session_id, true);
            if delivery.inbox.is_none() {
                self.acknowledge_user_input(&terminal, true, input_state);
            }
            Ok(Some(session_id))
        } else if let Some((project_id, task_prompt)) = new_supervisor {
            let agent = self.spawn_on_reply_agent();
            let task_title = format!("Supervisor for item {}: {}", item.id, item.title);
            let session_id = self.spawn_session(
                project_id,
                agent,
                &task_title,
                &task_prompt,
                None,
                PermissionMode::Inherit,
                None,
                true,
                true,
                None,
            )?;
            let write = ItemWrite {
                bucket_id: item.bucket_id,
                id: Some(item.id),
                note: Some(format!(
                    "user spawned supervisor session {session_id} ({}) for this item",
                    agent.as_str()
                )),
                link_session_id: Some(session_id),
                ..ItemWrite::default()
            };
            if let Err(error) = self.upsert_item(item.bucket_id, &write, None) {
                warn!(item = item.id, session = session_id, %error, "failed to link spawned supervisor to item");
            }
            Ok(Some(session_id))
        } else {
            Ok(None)
        }
    }

    fn spawn_on_reply_agent(&self) -> AgentKind {
        [
            AgentKind::Codex,
            AgentKind::ClaudeCode,
            AgentKind::Gemini,
            AgentKind::Test,
        ]
        .into_iter()
        .find(|agent| self.registry.get(*agent).is_ok())
        .unwrap_or(AgentKind::Codex)
    }

    pub fn delete_item(&self, bucket_id: u64, id: u64) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        if bucket_id == 0 {
            return Err(DaemonError::Rejected(
                "unqualified legacy item ids are unsupported".into(),
            ));
        }
        let item = self.storage.delete_item(bucket_id, id)?;
        self.publish(Event::ItemRemoved(ItemRef {
            bucket_id: item.bucket_id,
            item_id: item.id,
        }));
        Ok(())
    }

    /// Parks an item until a time (`None` clears). Human-only: nothing
    /// on the MCP path reaches this.
    pub fn snooze_item(
        &self,
        bucket_id: u64,
        id: u64,
        until_unix_ms: Option<i64>,
    ) -> Result<(), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        if bucket_id == 0 {
            return Err(DaemonError::Rejected(
                "unqualified legacy item ids are unsupported".into(),
            ));
        }
        let item = self
            .storage
            .snooze_item(bucket_id, id, until_unix_ms, now_unix_ms())?;
        self.publish(Event::ItemChanged(item));
        Ok(())
    }

    pub fn list_items(&self, query: &ItemQuery) -> Result<Vec<Item>, DaemonError> {
        Ok(self.storage.list_items(query, now_unix_ms())?)
    }

    pub fn item_query_counts(&self, query: &ItemQuery) -> Result<ItemQueryCounts, DaemonError> {
        Ok(self.storage.item_query_counts(query, now_unix_ms())?)
    }

    pub fn list_items_with_counts(
        &self,
        query: &ItemQuery,
    ) -> Result<(Vec<Item>, ItemQueryCounts), DaemonError> {
        Ok(self.storage.list_items_with_counts(query, now_unix_ms())?)
    }

    pub fn item_notes(&self, bucket_id: u64, item_id: u64) -> Result<Vec<ItemNote>, DaemonError> {
        if bucket_id == 0 {
            return Err(DaemonError::Rejected(
                "unqualified legacy item ids are unsupported".into(),
            ));
        }
        Ok(self.storage.item_notes(bucket_id, item_id)?)
    }

    pub fn get_item(&self, bucket_id: u64, id: u64) -> Result<Item, DaemonError> {
        if bucket_id == 0 {
            return Err(DaemonError::Rejected(
                "unqualified legacy item ids are unsupported".into(),
            ));
        }
        Ok(self.storage.get_item(bucket_id, id)?)
    }

    /// Stores a new briefing for a bucket. Returns whether the markdown
    /// was truncated to its cap.
    pub fn post_briefing(
        &self,
        bucket_id: u64,
        session_id: Option<u64>,
        markdown: &str,
    ) -> Result<(BucketBriefing, bool), DaemonError> {
        let truncated = markdown.chars().count() > crate::storage::BRIEFING_MAX;
        let markdown = truncate_chars(markdown, crate::storage::BRIEFING_MAX);
        let _guard = self.state_lock.lock().unwrap();
        let briefing =
            self.storage
                .create_briefing(bucket_id, session_id, &markdown, now_unix_ms())?;
        info!(bucket = bucket_id, "briefing posted");
        self.publish(Event::BriefingChanged(briefing.clone()));
        Ok((briefing, truncated))
    }

    pub fn briefings(
        &self,
        bucket_id: u64,
        limit: usize,
    ) -> Result<Vec<BucketBriefing>, DaemonError> {
        Ok(self.storage.list_briefings(bucket_id, limit)?)
    }

    /// Writes one setting after validating the key and value; `None`
    /// clears the row so the built-in default applies again.
    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<(), DaemonError> {
        let known = KNOWN_SETTINGS.iter().find(|(k, ..)| *k == key);
        if known.is_none() {
            return Err(DaemonError::Rejected(format!(
                "unknown setting {key:?}; known settings: {}",
                KNOWN_SETTINGS
                    .iter()
                    .map(|(k, ..)| *k)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some(value) = value {
            let (valid, expected) = match key {
                SETTING_SUPERVISOR_MAX_CHILDREN
                | crate::mobile::SETTING_MOBILE_ACCESS_TTL_MINUTES
                | crate::mobile::SETTING_MOBILE_REFRESH_TTL_DAYS
                | crate::mobile::SETTING_MOBILE_ENROLL_TTL_MINUTES
                | crate::mobile::SETTING_MOBILE_SOCKET_TICKET_TTL_SECONDS
                | crate::push::SETTING_PUSH_DEDUPE_WINDOW_HOURS => (
                    value.parse::<usize>().is_ok_and(|n| n >= 1),
                    "a positive integer",
                ),
                crate::push::SETTING_PUSH_EVENTS => (
                    crate::push::parse_events_csv(value).is_some(),
                    "a comma-separated subset of needs-input, failed, completed",
                ),
                crate::push::SETTING_PUSH_SCOPE => (
                    crate::push::PushScope::parse(value).is_some(),
                    "default, all, supervisors, or none",
                ),
                crate::push::SETTING_PUSH_GATEWAY_URL => (
                    value.is_empty()
                        || value.starts_with("http://")
                        || value.starts_with("https://"),
                    "an http(s) URL or empty",
                ),
                _ => (value == "true" || value == "false", "true or false"),
            };
            if !valid {
                return Err(DaemonError::Rejected(format!(
                    "setting {key:?} takes {expected}, not {value:?}"
                )));
            }
        }
        let stored = match value {
            Some(value) if crate::push::SECRET_SETTINGS.contains(&key) => Some(
                crate::secrets::seal_secret(&self.installation_secret(), value),
            ),
            other => other.map(str::to_string),
        };
        self.storage.set_setting(key, stored.as_deref())?;
        let logged = if crate::push::SECRET_SETTINGS.contains(&key) {
            value.map(|_| "(secret)")
        } else {
            value
        };
        info!(
            key,
            value = logged.unwrap_or("(default)"),
            "setting changed"
        );
        Ok(())
    }

    /// Every known setting with its current and default values.
    /// Credential values are never returned; only their set state is.
    pub fn settings(&self) -> Result<Vec<serde_json::Value>, DaemonError> {
        KNOWN_SETTINGS
            .iter()
            .map(|(key, default, description)| {
                let value = self.storage.get_setting(key)?;
                let secret = crate::push::SECRET_SETTINGS.contains(key);
                let shown = if secret {
                    ""
                } else {
                    value.as_deref().unwrap_or(default)
                };
                Ok(serde_json::json!({
                    "key": key,
                    "value": shown,
                    "default": default,
                    "set": value.is_some(),
                    "secret": secret,
                    "description": description.split_whitespace().collect::<Vec<_>>().join(" "),
                }))
            })
            .collect()
    }

    /// Whether spawned PTYs advertise truecolor; the stored setting,
    /// else on.
    fn spawn_truecolor(&self) -> bool {
        self.storage
            .get_setting(SETTING_SPAWN_TRUECOLOR)
            .ok()
            .flatten()
            .map(|v| v != "false")
            .unwrap_or(true)
    }

    /// Whether agent terminals consume the Program Status Protocol; the
    /// stored setting, else off.
    fn spawn_program_status(&self) -> bool {
        self.storage
            .get_setting(SETTING_SPAWN_PROGRAM_STATUS)
            .ok()
            .flatten()
            .is_some_and(|v| v == "true")
    }

    /// [`Self::spawn_program_status`] for a worker that can honor it.
    fn spawn_program_status_on(&self, link: &crate::workers::WorkerLink) -> bool {
        if !self.spawn_program_status() {
            return false;
        }
        let supported = link.protocol_version() >= pm_protocol::WORKER_PROTOCOL_PROGRAM_STATUS;
        if !supported {
            info!(
                worker_protocol = link.protocol_version(),
                "worker predates program status, its agent keeps hook-driven state"
            );
        }
        supported
    }

    /// Whether spawned agents may take over the alternate screen; the
    /// stored setting, else off.
    fn spawn_fullscreen(&self) -> bool {
        self.storage
            .get_setting(SETTING_SPAWN_FULLSCREEN)
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(false)
    }

    pub fn handle_agent_report(
        &self,
        session_token: &str,
        report: AgentReport,
    ) -> Result<Option<String>, DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        match report {
            AgentReport::Report {
                goal,
                headline,
                summary,
                note,
                glance,
                context,
                clear,
                git,
            } => self.apply_report(
                session_id, goal, headline, summary, note, glance, context, clear, git,
            ),
            AgentReport::Blocked { question } => self.apply_blocked(session_id, question),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_report(
        &self,
        session_id: u64,
        goal: String,
        headline: String,
        summary: Option<String>,
        note: String,
        glance: Option<Vec<ContextField>>,
        context: Option<Vec<ContextField>>,
        clear: Vec<String>,
        git: Box<crate::storage::SessionGitUpdate>,
    ) -> Result<Option<String>, DaemonError> {
        let current = self.storage.get_session(session_id)?;
        // An empty headline keeps the current name rather than blanking it.
        let headline = if headline.trim().is_empty() {
            current.headline.clone()
        } else {
            truncate_chars(
                &crate::text::unescape_html_entities(&headline),
                crate::storage::HEADLINE_MAX,
            )
        };
        let summary = summary
            .map(|s| crate::text::unescape_html_entities(&s))
            .unwrap_or(current.summary);
        let note = crate::text::unescape_html_entities(&note);
        let mut notes: Vec<String> = Vec::new();

        let _guard = self.state_lock.lock().unwrap();
        let now = now_unix_ms();
        if !git.is_empty() {
            self.storage.set_session_git(session_id, &git)?;
        }
        let goal = crate::text::unescape_html_entities(goal.trim());
        let goal_changed = !goal.is_empty() && goal != current.goal;
        if goal_changed {
            self.storage
                .set_goal(session_id, &truncate_chars(&goal, crate::storage::GOAL_MAX))?;
        }
        let headline_changed = headline != current.headline;
        let updated = self
            .storage
            .set_headline_summary(session_id, &headline, &summary)?;
        notes.extend(self.report_freshness.observe(
            session_id,
            now,
            crate::report_freshness::Observed {
                goal: &updated.goal,
                goal_changed,
                headline: &updated.headline,
                headline_changed,
            },
        ));
        let payload = serde_json::json!({
            "headline": headline, "summary": summary, "note": note,
        })
        .to_string();
        self.storage.append_checkpoint(session_id, now, &payload)?;

        let mut context_changed = false;
        if let Some(fields) = glance {
            let original = fields.len();
            let mut fields = bound_fields(fields, crate::storage::GLANCE_VALUE_MAX);
            if fields.len() > crate::storage::GLANCE_MAX_FIELDS {
                fields.truncate(crate::storage::GLANCE_MAX_FIELDS);
                notes.push(format!(
                    "kept {} of {} glance fields; put the rest in context",
                    crate::storage::GLANCE_MAX_FIELDS,
                    original
                ));
            }
            self.storage.replace_glance(session_id, &fields)?;
            context_changed = true;
        }
        if let Some(fields) = context {
            let fields = bound_fields(fields, crate::storage::CONTEXT_VALUE_MAX);
            let dropped = self.storage.upsert_context(session_id, &fields, now)?;
            if dropped > 0 {
                notes.push(format!(
                    "context is full at {} fields; dropped {} new key(s), clear stale ones",
                    crate::storage::CONTEXT_MAX_FIELDS,
                    dropped
                ));
            }
            context_changed = true;
        }
        if !clear.is_empty() {
            self.storage.clear_context(session_id, &clear)?;
            context_changed = true;
        }

        info!(session = session_id, "agent report");
        self.publish(Event::SessionChanged(updated));
        if context_changed {
            self.publish_context(session_id)?;
        }
        Ok((!notes.is_empty()).then(|| notes.join("; ")))
    }

    /// Public so a test can shorten how long a repeated goal or headline
    /// goes unremarked before a report's reply asks about it.
    pub fn set_report_freshness_thresholds(&self, headline_ms: i64, goal_ms: i64) {
        self.report_freshness.set_thresholds(headline_ms, goal_ms);
    }

    /// Public so a test can drive the flag_blocked route directly. The
    /// supervisor wake it triggers has no other entry point that does not
    /// require a live agent.
    #[doc(hidden)]
    pub fn apply_blocked(
        &self,
        session_id: u64,
        question: String,
    ) -> Result<Option<String>, DaemonError> {
        let question = crate::text::unescape_html_entities(&question);
        let payload = serde_json::json!({ "question": question }).to_string();
        let current = self.storage.get_session(session_id)?;
        let _guard = self.state_lock.lock().unwrap();
        let updated = self.storage.record_activity(
            session_id,
            &crate::storage::ActivityUpdate {
                state: SessionState::NeedsInput,
                state_detail: &question,
                activity: &current.activity,
                progress_percent: current.progress_percent,
                kind: crate::storage::BLOCKED_KIND,
                payload: &payload,
                ts_unix_ms: now_unix_ms(),
            },
        )?;
        info!(session = session_id, "agent blocked");
        self.publish(Event::SessionChanged(updated));
        Ok(None)
    }

    /// Reads a session's current context bags and publishes them.
    fn publish_context(&self, session_id: u64) -> Result<(), DaemonError> {
        let context = self.storage.session_context(session_id)?;
        self.publish(Event::ContextChanged(context));
        Ok(())
    }

    fn compile_instructions(
        &self,
        bucket_id: u64,
        project_id: Option<u64>,
        role: SessionRole,
    ) -> Result<(String, String, String), DaemonError> {
        if let Some(project_id) = project_id {
            let project = self.storage.get_project(project_id)?;
            if project.bucket_id != bucket_id {
                return Err(DaemonError::Rejected(format!(
                    "project {project_id} is not in bucket {bucket_id}"
                )));
            }
        }
        let mut sections = vec![(
            "Puppet Master contract".to_string(),
            pm_adapters::REPORTING_BRIEF.to_string(),
        )];
        sections.push((
            format!("Built-in {} role", role.as_str()),
            match role {
                SessionRole::Worker => WORKER_INSTRUCTIONS,
                SessionRole::Supervisor => SUPERVISOR_INSTRUCTIONS,
            }
            .to_string(),
        ));
        let layers = self.storage.instruction_layers(bucket_id, project_id)?;
        let mut sources = Vec::new();
        for layer in layers
            .into_iter()
            .filter(|l| l.target.applies_to(role) && !l.markdown.trim().is_empty())
        {
            let scope = layer
                .project_id
                .map(|id| format!("project {id}"))
                .unwrap_or_else(|| format!("bucket {bucket_id}"));
            sections.push((
                format!("{scope} / {}", layer.target.as_str()),
                layer.markdown.clone(),
            ));
            sources.push(serde_json::json!({"layer_id":layer.id,"revision":layer.revision}));
        }
        let compiled = sections
            .into_iter()
            .map(|(title, body)| format!("## {title}\n\n{}", body.trim()))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
            + "\n";
        let sources_json = serde_json::to_string(&sources).unwrap_or_else(|_| "[]".into());
        let hash = hex::encode(Sha256::digest(compiled.as_bytes()));
        Ok((compiled, sources_json, hash))
    }

    fn compile_and_snapshot_instructions(
        &self,
        session: &Session,
        generation: u64,
    ) -> Result<String, DaemonError> {
        let bucket_id = self.storage.get_project(session.project_id)?.bucket_id;
        let (compiled, sources, hash) =
            self.compile_instructions(bucket_id, Some(session.project_id), session.role)?;
        self.storage.save_instruction_snapshot(
            session.id,
            generation,
            &compiled,
            &sources,
            &hash,
            now_unix_ms(),
        )?;
        Ok(compiled)
    }

    pub fn list_instructions(
        &self,
        bucket_id: u64,
        project_id: Option<u64>,
    ) -> Result<Vec<InstructionLayer>, DaemonError> {
        if let Some(id) = project_id {
            if self.storage.get_project(id)?.bucket_id != bucket_id {
                return Err(DaemonError::Rejected(format!(
                    "project {id} is not in bucket {bucket_id}"
                )));
            }
        }
        Ok(self.storage.instruction_layers(bucket_id, project_id)?)
    }
    pub fn resolve_instruction_project(
        &self,
        bucket_id: u64,
        project: &serde_json::Value,
    ) -> Result<u64, DaemonError> {
        self.resolve_project_in_bucket(bucket_id, project)
    }
    pub fn effective_instructions(
        &self,
        bucket_id: u64,
        project_id: Option<u64>,
        role: SessionRole,
    ) -> Result<String, DaemonError> {
        Ok(self.compile_instructions(bucket_id, project_id, role)?.0)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn set_instructions(
        &self,
        bucket_id: u64,
        project_id: Option<u64>,
        target: InstructionTarget,
        markdown: &str,
        expected_revision: u64,
        note: &str,
        updated_by: Option<u64>,
    ) -> Result<InstructionLayer, DaemonError> {
        let layer = self.storage.set_instruction_layer(
            bucket_id,
            project_id,
            target,
            markdown,
            expected_revision,
            note,
            updated_by,
            now_unix_ms(),
        )?;
        self.publish(Event::InstructionLayerChanged(layer.clone()));
        // A session wrote it rather than the person at the keyboard. This is the
        // product's durable prompt-injection surface: one injected page becomes
        // the standing instructions every session in the bucket is launched
        // with, the supervisor's own included, and it survives every restart.
        // The capability is legitimate and the history is already kept, so what
        // was missing is that it happened without anything being said.
        if let Some(session_id) = updated_by {
            let scope = if project_id.is_some() {
                "this project's"
            } else {
                "every project in this bucket's"
            };
            let subject = self
                .storage
                .get_bucket(bucket_id)
                .map(|bucket| bucket.name)
                .unwrap_or_else(|_| bucket_id.to_string());
            self.publish_security_notice(
                pm_protocol::domain::SecurityNoticeKind::InstructionsRewritten,
                &subject,
                &format!(
                    "session {session_id} rewrote {scope} standing instructions for the \
                     {} role, so every session launched there follows them. Revert it from \
                     the instruction history if that was not asked for.",
                    target.as_str()
                ),
            );
        }
        Ok(layer)
    }
    pub fn instruction_history(
        &self,
        layer_id: u64,
    ) -> Result<Vec<InstructionRevision>, DaemonError> {
        Ok(self.storage.instruction_history(layer_id)?)
    }
    pub fn instruction_snapshot(
        &self,
        session_id: u64,
        generation: u64,
    ) -> Result<(String, String, String), DaemonError> {
        Ok(self.storage.instruction_snapshot(session_id, generation)?)
    }
    pub fn revert_instructions(
        &self,
        layer_id: u64,
        revision: u64,
        expected_revision: u64,
        note: &str,
        updated_by: Option<u64>,
    ) -> Result<InstructionLayer, DaemonError> {
        let layer = self.storage.revert_instruction_layer(
            layer_id,
            revision,
            expected_revision,
            note,
            updated_by,
            now_unix_ms(),
        )?;
        self.publish(Event::InstructionLayerChanged(layer.clone()));
        Ok(layer)
    }

    /// Resolves an MCP bearer token to (session, bucket) for the item
    /// tools, rejecting sessions spawned with the Items API disabled.
    pub fn items_session(&self, session_token: &str) -> Result<(u64, u64), DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        let session = self.storage.get_session(session_id)?;
        if !session.items_api {
            return Err(DaemonError::Rejected(
                "the Items API is disabled for this session".into(),
            ));
        }
        let project = self.storage.get_project(session.project_id)?;
        Ok((session_id, project.bucket_id))
    }

    /// Whether the token's session offers the item tools, for the MCP
    /// tools/list reply. Unknown tokens see the base tools only.
    pub fn session_offers_items_api(&self, session_token: &str) -> bool {
        self.items_session(session_token).is_ok()
    }

    /// Enables or disables a session's MCP tool grants. Only the
    /// human-authenticated surfaces reach this — there is deliberately
    /// no MCP path, so a session can never grant itself anything. The
    /// MCP server re-checks the flags on every call, so a revocation
    /// applies immediately; a grant applies as soon as the agent's MCP
    /// client re-lists tools.
    pub fn update_session_apis(
        &self,
        session_id: u64,
        items_api: Option<bool>,
        supervisor_api: Option<bool>,
    ) -> Result<(), DaemonError> {
        let role = supervisor_api.map(|enabled| {
            if enabled {
                SessionRole::Supervisor
            } else {
                SessionRole::Worker
            }
        });
        self.update_session_role_apis(session_id, items_api, supervisor_api, role)
    }

    pub fn update_session_role_apis(
        &self,
        session_id: u64,
        items_api: Option<bool>,
        supervisor_api: Option<bool>,
        role: Option<SessionRole>,
    ) -> Result<(), DaemonError> {
        if items_api.is_none() && supervisor_api.is_none() && role.is_none() {
            return Ok(());
        }
        {
            let _guard = self.state_lock.lock().unwrap();
            let session =
                self.storage
                    .set_session_apis(session_id, items_api, supervisor_api, role)?;
            self.publish(Event::SessionChanged(session));
        }
        let mut changes = Vec::new();
        if let Some(v) = items_api {
            changes.push(format!(
                "item tools {}",
                if v { "enabled" } else { "disabled" }
            ));
        }
        if let Some(v) = supervisor_api {
            changes.push(format!(
                "supervisor tools {}",
                if v { "enabled" } else { "disabled" }
            ));
        }
        if let Some(v) = role {
            changes.push(format!("role changed to {}", v.as_str()));
        }
        let note = format!("user changed session APIs: {}", changes.join(", "));
        let payload =
            serde_json::json!({ "headline": "", "summary": serde_json::Value::Null, "note": note })
                .to_string();
        if let Err(e) = self
            .storage
            .append_user_note(session_id, now_unix_ms(), &payload)
        {
            warn!(session = session_id, error = %e, "failed to record session API change");
        }
        info!(
            session = session_id,
            items_api = ?items_api,
            supervisor_api = ?supervisor_api,
            role = ?role,
            "session APIs updated"
        );
        Ok(())
    }

    pub fn snooze_supervision(&self, supervisor_id: u64, minutes: u64) -> Result<i64, DaemonError> {
        if !(crate::supervisor_wake::MIN_SNOOZE_MINUTES
            ..=crate::supervisor_wake::MAX_SNOOZE_MINUTES)
            .contains(&minutes)
        {
            return Err(DaemonError::Rejected(format!(
                "minutes must be an integer in {}-{}",
                crate::supervisor_wake::MIN_SNOOZE_MINUTES,
                crate::supervisor_wake::MAX_SNOOZE_MINUTES
            )));
        }
        let _guard = self.state_lock.lock().unwrap();
        let session = self.storage().get_session(supervisor_id)?;
        if session.role != SessionRole::Supervisor || !session.supervisor_api {
            return Err(DaemonError::Rejected(
                "only supervisors can snooze supervision".into(),
            ));
        }
        let can_snooze = session.state == SessionState::NeedsInput
            || (session.state == SessionState::Working && session.state_detail.is_empty());
        if !can_snooze {
            return Err(DaemonError::Rejected(
                "snooze_supervision requires a working or needs-input supervisor".into(),
            ));
        }
        let generation = self.storage().agent_terminal(supervisor_id)?.generation;
        let until = now_unix_ms() + minutes as i64 * crate::supervisor_wake::MINUTE_MS;
        self.storage()
            .snooze_supervision(supervisor_id, generation, until)?;
        self.note_supervision_activity(supervisor_id);
        info!(
            supervisor = supervisor_id,
            minutes, "supervisor snoozed idle reminders and its current completion alert"
        );
        Ok(until)
    }

    /// Resolves an MCP bearer token to (session, bucket) for the
    /// supervisor tools, rejecting sessions spawned without them.
    pub fn supervisor_session(&self, session_token: &str) -> Result<(u64, u64), DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        let session = self.storage.get_session(session_id)?;
        if session.role != SessionRole::Supervisor || !session.supervisor_api {
            return Err(DaemonError::Rejected(
                "the supervisor tools are disabled for this session".into(),
            ));
        }
        let project = self.storage.get_project(session.project_id)?;
        Ok((session_id, project.bucket_id))
    }

    /// Whether the token's session offers the supervisor tools, for
    /// the MCP tools/list reply.
    pub fn session_offers_supervisor_api(&self, session_token: &str) -> bool {
        self.supervisor_session(session_token).is_ok()
    }

    fn supervisor_max_children(&self) -> usize {
        self.storage
            .get_setting(SETTING_SUPERVISOR_MAX_CHILDREN)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(SUPERVISOR_MAX_CHILDREN_DEFAULT)
    }

    /// Spawns a session for a board item on behalf of a supervisor
    /// session. The chosen project fixes the working directory, the
    /// worker cascade, and the permission cascade; the supervisor
    /// cannot override any of them, so the handler never forwards
    /// caller-supplied values for those.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub async fn supervisor_spawn(
        &self,
        supervisor_id: u64,
        bucket_id: u64,
        project_ref: &serde_json::Value,
        agent: Option<AgentKind>,
        task_title: &str,
        task_prompt: &str,
        item_ref: &serde_json::Value,
        host: &serde_json::Value,
    ) -> Result<(u64, u64), DaemonError> {
        let project_id = self.resolve_project_in_bucket(bucket_id, project_ref)?;
        let worker_override = match host {
            serde_json::Value::Null => None,
            serde_json::Value::String(requested) => {
                Some(self.resolve_spawn_host(project_id, requested)?)
            }
            serde_json::Value::Number(requested) => {
                Some(self.resolve_spawn_host(project_id, &requested.to_string())?)
            }
            _ => {
                return Err(DaemonError::Rejected(
                    "host must be a worker id or name".into(),
                ))
            }
        };
        self.ensure_spawnable(project_id, worker_override, None)
            .await?;
        let item_id = self.resolve_item_ref(bucket_id, item_ref)?;
        let item = self.storage.get_item(bucket_id, item_id)?;
        if item.bucket_id != bucket_id {
            return Err(DaemonError::Rejected(format!(
                "item {item_id} is not in this bucket"
            )));
        }
        let live = self.storage.live_spawned_by_count(supervisor_id)?;
        let max = self.supervisor_max_children();
        if live >= max {
            return Err(DaemonError::Rejected(format!(
                "this supervisor already has {live} live sessions (limit {max}); \
                 wait for one to end or raise the {SETTING_SUPERVISOR_MAX_CHILDREN} setting"
            )));
        }
        let session_id = self.spawn_session_with_agent_override(
            project_id,
            agent,
            task_title,
            task_prompt,
            None,
            PermissionMode::Inherit,
            worker_override,
            true,
            false,
            Some(supervisor_id),
            None,
            None,
        )?;
        let resolved = self.storage.get_session(session_id)?;
        let host_name = self.storage.get_worker(resolved.worker_id)?.name;
        let write = ItemWrite {
            bucket_id,
            id: Some(item_id),
            note: Some(format!(
                "supervisor session {supervisor_id} spawned session {session_id} \
                 ({}; source={}; host={host_name}) for this item",
                resolved.agent.as_str(),
                resolved.agent_source.as_str()
            )),
            link_session_id: Some(session_id),
            ..ItemWrite::default()
        };
        // The session is already running, so a bookkeeping failure
        // must not fail the spawn.
        if let Err(e) = self.upsert_item(bucket_id, &write, Some(supervisor_id)) {
            warn!(item = item_id, error = %e, "failed to link supervised session to item");
        }
        info!(
            supervisor = supervisor_id,
            session = session_id,
            item = item_id,
            "supervisor spawned session"
        );
        Ok((session_id, item_id))
    }

    /// The session, provided this supervisor spawned it. Every
    /// supervisor operation on an existing session goes through this
    /// guard.
    /// The bucket a session belongs to, or `None` when that cannot be
    /// established. A caller must treat `None` as "not the same bucket"
    /// rather than as a match, or two unresolvable sessions authorize
    /// each other.
    fn session_bucket(&self, session: &Session) -> Option<u64> {
        self.storage
            .get_project(session.project_id)
            .ok()
            .map(|project| project.bucket_id)
    }

    /// Authorizes a supervisor to observe and message a session that
    /// shares its bucket, whoever spawned it.
    ///
    /// A bucket is already the unit a supervisor sees: `list_sessions`
    /// has always returned all of it. Reading and messaging follow that
    /// boundary so a supervisor can coordinate with the peers it can
    /// already see. Acting on a session — interrupting, resuming,
    /// killing it — stays with whoever spawned it, because those destroy
    /// work rather than observe it.
    fn supervised_in_bucket(
        &self,
        supervisor_id: u64,
        session_id: u64,
    ) -> Result<Session, DaemonError> {
        let session = self.storage.get_session(session_id)?;
        if session.spawned_by_session_id == Some(supervisor_id) {
            return Ok(session);
        }
        let supervisor = self.storage.get_session(supervisor_id)?;
        match (
            self.session_bucket(&session),
            self.session_bucket(&supervisor),
        ) {
            (Some(theirs), Some(ours)) if theirs == ours => Ok(session),
            _ => Err(DaemonError::Rejected(format!(
                "session {session_id} is not in this supervisor's bucket"
            ))),
        }
    }

    fn supervised(&self, supervisor_id: u64, session_id: u64) -> Result<Session, DaemonError> {
        let session = self.storage.get_session(session_id)?;
        if session.spawned_by_session_id != Some(supervisor_id) {
            return Err(DaemonError::Rejected(format!(
                "session {session_id} was not spawned by this supervisor session"
            )));
        }
        Ok(session)
    }

    /// Appends an audit note to every item linked to a supervised
    /// session; failures only log, they never fail the action itself.
    pub(crate) fn supervised_item_note(&self, supervisor_id: u64, session_id: u64, note: &str) {
        let Ok(items) = self.storage.item_refs_for_session(session_id) else {
            return;
        };
        for item in items {
            let write = ItemWrite {
                bucket_id: item.bucket_id,
                id: Some(item.item_id),
                note: Some(note.to_string()),
                ..ItemWrite::default()
            };
            if let Err(e) = self.upsert_item(item.bucket_id, &write, Some(supervisor_id)) {
                warn!(item = item.item_id, error = %e, "failed to append supervisor audit note");
            }
        }
    }

    /// How a host reads to a supervisor: which host it is and whether
    /// the controller can reach it. Reachability is the host's own
    /// state and says nothing about the projects configured on it.
    pub fn host_json(&self, worker_id: u64) -> serde_json::Value {
        serde_json::json!({
            "id": worker_id,
            "name": self.worker_name(worker_id).unwrap_or_default(),
            "online": self.worker_is_online(worker_id),
        })
    }

    /// How a project stands on a host, as a supervisor reads it: the
    /// verdict, the path it is about, and the sentence naming what to
    /// change.
    pub fn project_host_json(
        &self,
        project: &pm_protocol::domain::Project,
        worker_id: u64,
        state: &ProjectHostState,
    ) -> serde_json::Value {
        let host = self
            .worker_name(worker_id)
            .unwrap_or_else(|_| format!("worker {worker_id}"));
        serde_json::json!({
            "status": state.status(),
            "path": state.path(),
            "detail": state.message(&project.name, &host, worker_id),
        })
    }

    /// A supervised session's current state, dashboard report, context
    /// fields, linked items, recent report timeline, and how its project
    /// stands on the host it runs on.
    pub async fn supervisor_session_status(
        &self,
        supervisor_id: u64,
        session_id: u64,
    ) -> Result<serde_json::Value, DaemonError> {
        let authorized = self.supervised_in_bucket(supervisor_id, session_id)?;
        let project_host = self
            .project_host_state(authorized.project_id, authorized.worker_id, None)
            .await?;
        let _guard = self.state_lock.lock().unwrap();
        let session = self.supervised_in_bucket(supervisor_id, session_id)?;
        let mut sessions = [session];
        self.overlay_awaiting_worker_sessions(&mut sessions);
        let session = &sessions[0];
        let mut status = session_json(session);
        let project = self.storage.get_project(session.project_id)?;
        status["generation"] = serde_json::json!(self.session_generation(session_id));
        status["cursor"] = serde_json::json!(self.bucket_event_cursor(project.bucket_id));
        let context = self.storage.session_context(session_id)?;
        status["glance"] = context_fields_json(&context.glance);
        status["context"] = context_fields_json(&context.detail);
        status["items"] = serde_json::Value::Array(
            self.storage
                .item_refs_for_session(session_id)?
                .into_iter()
                .map(|item| serde_json::json!({"bucket_id": item.bucket_id, "id": item.item_id}))
                .collect(),
        );
        let reports: Vec<serde_json::Value> = self
            .activity_reports(session_id)?
            .into_iter()
            .rev()
            .take(SUPERVISOR_STATUS_REPORTS)
            .map(|r| {
                serde_json::json!({
                    "ts_unix_ms": r.ts_unix_ms,
                    "kind": r.kind,
                    "payload": serde_json::from_str::<serde_json::Value>(&r.payload)
                        .unwrap_or(serde_json::Value::String(r.payload)),
                })
            })
            .collect();
        status["recent_reports"] = serde_json::json!(reports);
        status["host"] = self.host_json(session.worker_id);
        status["project_host"] = self.project_host_json(&project, session.worker_id, &project_host);
        Ok(status)
    }

    /// The tail of a supervised session's agent terminal, decoded
    /// lossily. The caller strips escape sequences for presentation.
    pub async fn supervisor_read_terminal(
        &self,
        supervisor_id: u64,
        session_id: u64,
        max_bytes: usize,
    ) -> Result<bytes::Bytes, DaemonError> {
        self.supervised_in_bucket(supervisor_id, session_id)?;
        let (replay, _rx, _guard) = self.attach(session_id).await?;
        let start = replay.len().saturating_sub(max_bytes);
        Ok(replay.slice(start..))
    }

    /// Writes to a supervised session's agent terminal, optionally
    /// normalizing its trailing line ending to one Enter press.
    /// Puts a blocked worker's question to the supervisor that spawned
    /// it, as a message that can be answered.
    ///
    /// Returns the message the supervisor can answer, or `None` when
    /// there is no live supervisor to ask — a session steered by a human
    /// keeps today's behaviour, where the state change is the whole
    /// signal.
    pub async fn ask_supervisor(&self, session_id: u64, question: &str) -> Option<u64> {
        let child = self.storage.get_session(session_id).ok()?;
        let supervisor_id = child.spawned_by_session_id?;
        let supervisor = self.storage.get_session(supervisor_id).ok()?;
        if !supervisor.state.is_live() {
            return None;
        }
        let text =
            format!("Session {session_id} is blocked and needs an answer from you:\n\n{question}");
        let outcome = self
            .send_session_input(session_id, supervisor_id, &text, true, true)
            .await
            .ok()?;
        let message_id = outcome.message_id?;
        // The block also moved the child into NeedsInput, which the
        // reconciliation pass would announce on its own. One event, one
        // notice: the question is the better of the two, because it can
        // be answered.
        self.suppress_wake_for_current_state(session_id);
        info!(
            session = session_id,
            supervisor = supervisor_id,
            message = message_id,
            "put a blocked session's question to its supervisor"
        );
        Some(message_id)
    }

    /// Hands text to a session over the agent's own inbound channel,
    /// running the delivery on whichever host holds the agent.
    ///
    /// `None` means the terminal is the only way in, either because the
    /// agent has no channel or because the one it has did not take the
    /// message. Both are the caller's cue to fall back rather than to
    /// fail: nothing has been delivered.
    pub(crate) async fn deliver_via_inbox(
        &self,
        session_id: u64,
        text: &str,
        mode: pm_adapters::DeliveryMode,
    ) -> Option<crate::inbox::Delivered> {
        let session = self.storage.get_session(session_id).ok()?;
        if !session.state.is_live() {
            return None;
        }
        if session.worker_id != pm_protocol::domain::LOCAL_WORKER_ID {
            return self.deliver_via_remote_inbox(&session, text, mode).await;
        }
        let channel = crate::inbox::local_inbound_channel(
            &self.registry,
            &self.mux,
            session_id,
            session.agent,
            session.agent_session_id.clone(),
            self.storage.agent_port(session_id).ok().flatten(),
        )?;
        match crate::inbox::deliver(&channel, text, mode).await {
            Ok(delivered) => Some(delivered),
            Err(e) => {
                warn!(
                    session = session_id,
                    transport = channel.transport(),
                    error = %e,
                    "agent inbox did not take the message; falling back to the terminal"
                );
                None
            }
        }
    }

    /// Asks the worker holding a session to hand the message to its
    /// agent, because the address that reaches the agent only resolves
    /// on that host.
    ///
    /// `None` for every reason the local path returns it, plus a worker
    /// too old to be asked: all of them mean nothing was delivered and
    /// the terminal is still owed the message.
    async fn deliver_via_remote_inbox(
        &self,
        session: &pm_protocol::domain::Session,
        text: &str,
        mode: pm_adapters::DeliveryMode,
    ) -> Option<crate::inbox::Delivered> {
        let session_id = session.id;
        let link = self.worker_link(session.worker_id).ok()?;
        if link.protocol_version() < pm_protocol::WORKER_PROTOCOL_AGENT_INBOX {
            return None;
        }
        // The worker's mux is keyed by terminal, the same ids the
        // controller spawns against, so the agent terminal is what names
        // the process over there too.
        let agent_terminal = self.storage.agent_terminal(session_id).ok()?;
        let rx = link
            .request_agent_inbox(crate::workers::AgentInboxRequest {
                session_id,
                agent_terminal_id: agent_terminal.id,
                agent: session.agent,
                agent_session_id: session.agent_session_id.clone().unwrap_or_default(),
                agent_port: self.storage.agent_port(session_id).ok().flatten(),
                text: text.to_string(),
                mode: crate::inbox::inbox_mode(mode),
            })
            .ok()?;
        let result = match tokio::time::timeout(AGENT_INBOX_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            _ => {
                warn!(
                    session = session_id,
                    worker = session.worker_id,
                    "the worker did not report what it did with an agent inbox message in time"
                );
                return None;
            }
        };
        match result.outcome {
            pm_protocol::domain::AgentInboxOutcome::Delivered => Some(crate::inbox::Delivered {
                transport: crate::inbox::transport_name(&result.transport),
                mode: crate::inbox::delivery_mode(result.mode),
            }),
            outcome => {
                warn!(
                    session = session_id,
                    worker = session.worker_id,
                    outcome = outcome.as_str(),
                    detail = %result.detail,
                    "a worker did not hand the message to the agent; falling back to the terminal"
                );
                None
            }
        }
    }

    /// Whether an inbox delivery will be attempted for this session, so
    /// a caller can tell in advance that it is not about to type at a
    /// terminal.
    ///
    /// Optimistic for a remote session: only the worker knows whether
    /// its agent is addressable yet. A caller that acts on this must
    /// still handle `deliver_via_inbox` returning `None`, which every
    /// one of them already does by falling back under the guard the
    /// inbox did not need.
    pub(crate) fn inbox_delivery_possible(&self, session_id: u64) -> bool {
        let Ok(session) = self.storage.get_session(session_id) else {
            return false;
        };
        if !session.state.is_live() {
            return false;
        }
        if session.worker_id == pm_protocol::domain::LOCAL_WORKER_ID {
            return self.inbound_channel(session_id).is_some();
        }
        self.worker_link(session.worker_id)
            .is_ok_and(|link| link.protocol_version() >= pm_protocol::WORKER_PROTOCOL_AGENT_INBOX)
    }

    /// One recorded message, for a caller that needs the capability it
    /// carried or the answer it collected.
    pub fn agent_message(&self, id: u64) -> Result<crate::storage::AgentMessage, DaemonError> {
        Ok(self.storage.agent_message(id)?)
    }

    /// Records an answer to a message and hands it back to the sender.
    ///
    /// The capability is spent in the same statement that stores the
    /// answer, so a second attempt is refused rather than overwriting
    /// the first, and a spent token never means a lost reply.
    pub async fn reply_to_agent_message(
        &self,
        caller_session_id: u64,
        message_id: u64,
        token: &str,
        body: &str,
    ) -> Result<crate::storage::AgentMessage, DaemonError> {
        if body.trim().is_empty() {
            return Err(DaemonError::Rejected("reply body is empty".into()));
        }
        let message = self.storage.agent_message(message_id)?;
        // The session is who the daemon believes is calling; the token
        // is what the message granted. Both have to agree, so a token
        // that leaks into a transcript is still not usable from another
        // session.
        if message.to_session_id != caller_session_id {
            return Err(DaemonError::Rejected(format!(
                "message {message_id} was not addressed to this session"
            )));
        }
        if message.reply_token.as_deref() != Some(token) {
            return Err(DaemonError::Rejected(
                "the reply token does not match this message".into(),
            ));
        }
        if !message.awaits_reply(now_unix_ms()) {
            return Err(DaemonError::Rejected(
                "this message has already been answered or its reply window has closed".into(),
            ));
        }
        if !self.storage.record_agent_reply(message_id, body)? {
            return Err(DaemonError::Rejected(
                "this message has already been answered".into(),
            ));
        }
        // The answer goes back the way the message came. A reply carries
        // no capability of its own, so an exchange is one round trip and
        // two sessions cannot talk each other in circles.
        let note = format!("Reply to your message {message_id}:\n{body}");
        if let Err(e) = self
            .send_session_input(
                caller_session_id,
                message.from_session_id,
                &note,
                true,
                false,
            )
            .await
        {
            // The answer is recorded either way, so a sender that polls
            // still gets it even when its own session could not be
            // reached.
            warn!(
                message = message_id,
                session = message.from_session_id,
                error = %e,
                "recorded a reply the sender could not be handed"
            );
        }
        Ok(self.storage.agent_message(message_id)?)
    }

    /// Waits for the answer to a message this session sent.
    ///
    /// Reading never consumes: the answer stays where it is, so a poll
    /// whose response is lost in transit costs a round trip rather than
    /// the reply.
    pub async fn await_agent_reply(
        &self,
        caller_session_id: u64,
        message_id: u64,
        wait: std::time::Duration,
    ) -> Result<crate::storage::AgentMessage, DaemonError> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            let message = self.storage.agent_message(message_id)?;
            if message.from_session_id != caller_session_id {
                return Err(DaemonError::Rejected(format!(
                    "message {message_id} was not sent by this session"
                )));
            }
            if message.reply_body.is_some() {
                return Ok(message);
            }
            if message.reply_token.is_none() {
                return Err(DaemonError::Rejected(format!(
                    "message {message_id} did not ask for a reply"
                )));
            }
            if !message.awaits_reply(now_unix_ms()) {
                return Ok(message);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(message);
            }
            tokio::time::sleep(REPLY_POLL_INTERVAL).await;
        }
    }

    /// The agent's own inbound channel for a session running on this
    /// host, when it has one and the daemon knows enough to address it.
    ///
    /// Always `None` for a remote session, whose channel is named and
    /// used on its worker. A caller that only needs to know whether a
    /// message will bypass the terminal wants `inbox_delivery_possible`
    /// instead.
    pub fn inbound_channel(&self, session_id: u64) -> Option<pm_adapters::InboundChannel> {
        let session = self.storage.get_session(session_id).ok()?;
        if !session.state.is_live() || session.worker_id != pm_protocol::domain::LOCAL_WORKER_ID {
            // A remote session's agent runs on the worker's host, where
            // none of these addresses resolve from here.
            return None;
        }
        // The mux is keyed by terminal, so the agent's own terminal is
        // what names its process. A session id looks up an unrelated
        // terminal, and the pid it finds addresses another session's
        // agent.
        let agent_terminal = self.storage.agent_terminal(session_id).ok()?;
        crate::inbox::local_inbound_channel(
            &self.registry,
            &self.mux,
            agent_terminal.id,
            session.agent,
            session.agent_session_id.clone(),
            self.storage.agent_port(session_id).ok().flatten(),
        )
    }

    pub async fn supervisor_send_input(
        &self,
        supervisor_id: u64,
        session_id: u64,
        text: &str,
        submit: bool,
        expect_reply: bool,
    ) -> Result<SupervisorInputOutcome, DaemonError> {
        self.supervised_in_bucket(supervisor_id, session_id)?;
        self.send_session_input(supervisor_id, session_id, text, submit, expect_reply)
            .await
    }

    /// Delivers one message to a session, over the agent's own inbound
    /// channel where it has one and its terminal otherwise.
    ///
    /// Authorization belongs to the caller: a supervisor's own check
    /// happens before this, and a reply is authorized by the capability
    /// the message carried. Splitting them is what lets a reply travel
    /// back the same way the message came.
    pub(crate) async fn send_session_input(
        &self,
        from_session_id: u64,
        session_id: u64,
        text: &str,
        submit: bool,
        expect_reply: bool,
    ) -> Result<SupervisorInputOutcome, DaemonError> {
        self.send_session_input_inner(
            from_session_id,
            session_id,
            text,
            submit,
            expect_reply,
            true,
        )
        .await
    }

    pub(crate) async fn send_plan_input(
        &self,
        session_id: u64,
        text: &str,
    ) -> Result<SupervisorInputOutcome, DaemonError> {
        self.send_session_input_inner(session_id, session_id, text, true, false, false)
            .await
    }

    pub(crate) async fn send_connection_input(
        &self,
        session_id: u64,
        text: &str,
    ) -> Result<SupervisorInputOutcome, DaemonError> {
        self.send_session_input_inner(session_id, session_id, text, true, false, false)
            .await
    }

    pub(crate) async fn send_review_input(
        &self,
        session_id: u64,
        text: &str,
    ) -> Result<SupervisorInputOutcome, DaemonError> {
        self.send_session_input_inner(session_id, session_id, text, true, false, false)
            .await
    }

    async fn send_session_input_inner(
        &self,
        from_session_id: u64,
        session_id: u64,
        text: &str,
        submit: bool,
        expect_reply: bool,
        record_item_note: bool,
    ) -> Result<SupervisorInputOutcome, DaemonError> {
        let session = self.storage.get_session(session_id)?;
        if !session.state.is_live() {
            return Ok(SupervisorInputOutcome::not_delivered(
                session_id,
                submit,
                "session_ended",
                false,
                format!("session {session_id} already ended; input was not delivered"),
            ));
        }
        let paste_to_submit = self.adapter_submits_after_bracketed_paste(session.agent);
        if submit
            && paste_to_submit
            && text
                .as_bytes()
                .windows(BRACKETED_PASTE_END.len())
                .any(|window| window == BRACKETED_PASTE_END)
        {
            return Ok(SupervisorInputOutcome::not_delivered(
                session_id,
                submit,
                "invalid_input",
                false,
                "input contains the terminal's bracketed-paste end marker; it was not queued"
                    .into(),
            ));
        }
        // The capability is minted before delivery so the instructions
        // that travel with the message can name it, and recorded in the
        // same place a reply is later looked up, which is what lets an
        // answer outlive a daemon restart or a worker reconnect.
        let reply = expect_reply.then(|| {
            (
                mint_reply_token(),
                now_unix_ms() + REPLY_CAPABILITY_LIFETIME_MS,
            )
        });
        let (body, message_id) = match &reply {
            None => (text.to_string(), None),
            Some((token, expires)) => {
                let id = self.storage.record_agent_message(
                    from_session_id,
                    session_id,
                    text,
                    "",
                    Some((token.as_str(), *expires)),
                )?;
                (reply_instructions(text, id, token), Some(id))
            }
        };
        let text = body.as_str();
        let plan = self.agent_message_plan(session_id, text, submit)?;
        if plan.transport.is_empty() {
            return Ok(SupervisorInputOutcome::not_delivered(
                session_id,
                submit,
                "invalid_input",
                false,
                "input is empty; set submit to true to press Enter".into(),
            ));
        }
        if plan.logical_bytes > SUPERVISOR_INPUT_MAX {
            return Ok(SupervisorInputOutcome::not_delivered(
                session_id,
                submit,
                "input_too_large",
                false,
                format!(
                    "normalized input exceeds {SUPERVISOR_INPUT_MAX} bytes; send it in smaller pieces"
                ),
            ));
        }
        let input_state = session.state;
        // Subscribing before the write closes the race where the Working
        // transition lands between delivery and the confirmation wait.
        let confirm_events = self.wait_events_tx.subscribe();
        let confirm_baseline = self.session_lifecycle_baseline(session_id);
        let delivery = match self
            .deliver_agent_message(session_id, text, submit, plan, true, true)
            .await
        {
            Ok(delivery) => delivery,
            Err(AgentMessageFailure::InboxOnly) => unreachable!("the terminal is allowed"),
            Err(AgentMessageFailure::Terminal(failure)) => {
                let (reason, retryable, detail) = supervisor_input_failure_details(failure);
                return Ok(SupervisorInputOutcome::not_delivered(
                    session_id,
                    submit,
                    reason,
                    retryable,
                    format!("session {session_id} {detail}; input was not delivered"),
                ));
            }
        };
        let AgentMessageDelivery {
            inbox,
            plan,
            submit_failure,
            mut viewer,
        } = delivery;

        if let Some(delivered) = inbox {
            self.observe_user_interaction(session_id, true);
            let mut outcome = SupervisorInputOutcome::delivered_to_inbox(
                session_id,
                plan.logical_bytes,
                delivered.transport,
                delivered.mode == pm_adapters::DeliveryMode::Steer,
            );
            outcome.message_id = message_id;
            return Ok(outcome);
        }
        let terminal = self.storage.agent_terminal(session_id)?;
        if let Some(failure) = submit_failure {
            self.observe_user_interaction(session_id, false);
            let (reason, retryable, _) = supervisor_input_failure_details(failure);
            return Ok(SupervisorInputOutcome::submit_undelivered(
                session_id,
                reason,
                retryable,
                plan.logical_bytes,
            ));
        }
        self.observe_user_interaction(session_id, plan.submission_requested);
        self.acknowledge_user_input(&terminal, plan.submission_requested, input_state);
        let outcome = if !submit {
            SupervisorInputOutcome::queued(
                session_id,
                submit,
                plan.submission_requested,
                plan.logical_bytes,
            )
        } else if input_state == SessionState::Working {
            SupervisorInputOutcome::queued_behind_turn(session_id, plan.logical_bytes)
        } else {
            let mut confirmed = self
                .await_submission_confirmation(session_id, confirm_baseline, confirm_events)
                .await;
            if !confirmed {
                // The known recovery for an Enter the TUI folded into the
                // paste: a lone Enter on the settled composer submits it, and
                // is a no-op if the first Enter already landed.
                warn!(
                    session = session_id,
                    "no turn began after a submitted input; re-sending Enter"
                );
                // The original baseline is kept so a transition that landed
                // between the first wait's timeout and this retry still counts.
                let confirm_events = self.wait_events_tx.subscribe();
                if self
                    .deliver_supervisor_input(
                        &terminal,
                        bytes::Bytes::from_static(b"\r"),
                        &mut viewer,
                    )
                    .await
                    .is_ok()
                {
                    confirmed = self
                        .await_submission_confirmation(session_id, confirm_baseline, confirm_events)
                        .await;
                }
            }
            if confirmed {
                SupervisorInputOutcome::submitted(session_id, plan.logical_bytes)
            } else {
                SupervisorInputOutcome::submission_unconfirmed(session_id, plan.logical_bytes)
            }
        };
        drop(viewer);
        let mut outcome = outcome;
        outcome.message_id = message_id;
        if record_item_note {
            let preview: String = text.chars().take(SUPERVISOR_NOTE_PREVIEW).collect();
            self.supervised_item_note(
                from_session_id,
                session_id,
                &format!(
                    "supervisor sent input to session {session_id} ({state}): {preview:?}",
                    state = outcome.input_state
                ),
            );
        }
        Ok(outcome)
    }

    /// One PTY write for a supervisor-driven input, attaching a viewer
    /// stream on the first transport failure the way `attach_terminal`'s
    /// comment in `supervisor_send_input` describes. The guard is kept in
    /// `viewer` so later writes of the same call reuse the stream.
    async fn deliver_supervisor_input(
        &self,
        terminal: &pm_protocol::domain::Terminal,
        data: bytes::Bytes,
        viewer: &mut Option<crate::workers::ViewerGuard>,
    ) -> Result<(), TerminalInputFailure> {
        match self.try_deliver_terminal_input(terminal, data.clone()) {
            Err(TerminalInputFailure::Transport) if viewer.is_none() => {
                let Ok((_, _, guard)) = self.attach_terminal(terminal.id).await else {
                    return Err(TerminalInputFailure::Transport);
                };
                *viewer = guard;
                self.try_deliver_terminal_input(terminal, data)
            }
            result => result,
        }
    }

    /// Puts `text` into a session's agent as one message.
    ///
    /// The agent's own inbox takes it when there is a reachable one.
    /// Otherwise it goes to the PTY wrapped as a paste, with the Enter
    /// written separately afterwards, because a TUI reads a burst of
    /// characters followed immediately by Enter as a paste and turns
    /// that Enter into a newline.
    ///
    /// This is the only way message text reaches an agent. Writing it
    /// through the keystroke path instead delivers the burst.
    pub(crate) async fn deliver_agent_message(
        &self,
        session_id: u64,
        text: &str,
        submit: bool,
        plan: SupervisorInputPlan,
        may_steer: bool,
        terminal_fallback: bool,
    ) -> Result<AgentMessageDelivery, AgentMessageFailure> {
        // Typing without submitting is a composer edit, which only the
        // terminal can express.
        if submit {
            let working = self
                .storage
                .get_session(session_id)
                .is_ok_and(|session| session.state == SessionState::Working);
            let steer = may_steer
                && working
                && self
                    .inbound_channel(session_id)
                    .is_some_and(|channel| channel.supports_steering());
            let mode = if steer {
                pm_adapters::DeliveryMode::Steer
            } else {
                pm_adapters::DeliveryMode::Queue
            };
            if let Some(delivered) = self.deliver_via_inbox(session_id, text, mode).await {
                return Ok(AgentMessageDelivery {
                    inbox: Some(delivered),
                    plan,
                    submit_failure: None,
                    viewer: None,
                });
            }
        }
        if !terminal_fallback {
            return Err(AgentMessageFailure::InboxOnly);
        }
        let terminal = self
            .storage
            .agent_terminal(session_id)
            .map_err(|_| AgentMessageFailure::Terminal(TerminalInputFailure::MissingPty))?;
        // Nothing streams a terminal until a viewer attaches, so a
        // session nobody has opened has no upstream and the first write
        // lands nowhere. Attach the way a viewer would and hold it.
        let mut viewer = None;
        self.deliver_supervisor_input(&terminal, plan.transport.clone(), &mut viewer)
            .await
            .map_err(AgentMessageFailure::Terminal)?;
        let mut submit_failure = None;
        if let Some(enter) = plan.deferred_submit.clone() {
            tokio::time::sleep(SUPERVISOR_PASTE_SETTLE).await;
            if let Err(failure) = self
                .deliver_supervisor_input(&terminal, enter, &mut viewer)
                .await
            {
                submit_failure = Some(failure);
            }
        }
        Ok(AgentMessageDelivery {
            inbox: None,
            plan,
            submit_failure,
            viewer,
        })
    }

    /// The wait-journal position for the bucket a session lives in, taken
    /// before an input is delivered so a later confirmation check only
    /// counts transitions the input could have caused.
    fn session_lifecycle_baseline(&self, session_id: u64) -> Option<(u64, u64)> {
        let session = self.storage.get_session(session_id).ok()?;
        let project = self.storage.get_project(session.project_id).ok()?;
        let journal = self.session_wait_journal.lock().unwrap();
        let cursor = journal
            .buckets
            .get(&project.bucket_id)
            .map(|bucket| bucket.latest_cursor)
            .unwrap_or(0);
        Some((project.bucket_id, cursor))
    }

    /// Waits up to `SUBMIT_CONFIRM_WAIT` for the session to record a Working
    /// transition after `baseline`, which is the daemon's evidence that the
    /// agent TUI accepted the submitted input as a turn. For agents with
    /// lifecycle hooks that evidence is the prompt-submitted hook; for
    /// hookless agents it is the daemon's own optimistic acknowledgement.
    async fn await_submission_confirmation(
        &self,
        session_id: u64,
        baseline: Option<(u64, u64)>,
        mut events: broadcast::Receiver<()>,
    ) -> bool {
        let Some((bucket_id, after_cursor)) = baseline else {
            return false;
        };
        let deadline = tokio::time::Instant::now() + SUBMIT_CONFIRM_WAIT;
        loop {
            {
                let journal = self.session_wait_journal.lock().unwrap();
                if let Some(bucket) = journal.buckets.get(&bucket_id) {
                    if bucket.events.iter().any(|event| {
                        event.cursor > after_cursor
                            && event.session_id == session_id
                            && event.to == SessionState::Working
                    }) {
                        return true;
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return false,
                received = events.recv() => {
                    if received.is_err() {
                        // Lagged or closed; re-check the journal on a tick.
                        tokio::time::sleep(WAIT_JOURNAL_RECHECK).await;
                    }
                }
            }
        }
    }

    pub fn supervisor_interrupt(
        &self,
        supervisor_id: u64,
        session_id: u64,
    ) -> Result<(), DaemonError> {
        self.supervised(supervisor_id, session_id)?;
        self.interrupt_session(session_id)?;
        self.supervised_item_note(
            supervisor_id,
            session_id,
            &format!("supervisor interrupted session {session_id}"),
        );
        Ok(())
    }

    pub fn supervisor_resume(
        &self,
        supervisor_id: u64,
        session_id: u64,
    ) -> Result<u64, DaemonError> {
        self.supervised(supervisor_id, session_id)?;
        let resumed_id = self.resume_session(session_id)?;
        self.supervised_item_note(
            supervisor_id,
            session_id,
            &format!("supervisor resumed session {session_id}"),
        );
        Ok(resumed_id)
    }

    pub fn supervisor_kill(&self, supervisor_id: u64, session_id: u64) -> Result<(), DaemonError> {
        self.supervised(supervisor_id, session_id)?;
        self.kill_session(session_id)?;
        self.supervised_item_note(
            supervisor_id,
            session_id,
            &format!("supervisor killed session {session_id}"),
        );
        Ok(())
    }

    /// Every session in the bucket, newest first, so a supervisor can
    /// reconcile against what actually exists.
    pub fn supervisor_list_sessions(&self, bucket_id: u64) -> Result<Vec<Session>, DaemonError> {
        let mut sessions = self.storage.sessions_in_bucket(bucket_id)?;
        self.overlay_awaiting_worker_sessions(&mut sessions);
        Ok(sessions)
    }

    pub fn get_session_exact(&self, session_id: u64) -> Result<Session, DaemonError> {
        let mut session = self.storage.get_session(session_id)?;
        self.overlay_awaiting_worker_sessions(std::slice::from_mut(&mut session));
        Ok(session)
    }

    pub fn list_ended_sessions(
        &self,
        cursor: &str,
        limit: u32,
    ) -> Result<crate::storage::SessionPage, DaemonError> {
        let mut page = self.storage.list_ended_sessions(cursor, limit)?;
        self.overlay_awaiting_worker_sessions(&mut page.sessions);
        Ok(page)
    }

    pub fn search_sessions(
        &self,
        query: &str,
        cursor: &str,
        limit: u32,
    ) -> Result<crate::storage::SessionPage, DaemonError> {
        let mut page = self.storage.search_sessions(query, cursor, limit)?;
        self.overlay_awaiting_worker_sessions(&mut page.sessions);
        Ok(page)
    }

    /// Atomically pairs a bucket snapshot with its lifecycle cursor so a
    /// follow-up wait cannot skip a transition observed by neither side.
    pub fn supervisor_list_sessions_snapshot(
        &self,
        bucket_id: u64,
    ) -> Result<(u64, Vec<Session>), DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        let mut sessions = self.storage.sessions_in_bucket(bucket_id)?;
        self.overlay_awaiting_worker_sessions(&mut sessions);
        Ok((self.bucket_event_cursor(bucket_id), sessions))
    }

    pub fn session_generation(&self, session_id: u64) -> u64 {
        self.storage
            .agent_terminal(session_id)
            .map(|terminal| terminal.generation)
            .unwrap_or_default()
    }

    pub fn bucket_event_cursor(&self, bucket_id: u64) -> u64 {
        self.session_wait_journal
            .lock()
            .unwrap()
            .buckets
            .get(&bucket_id)
            .map(|bucket| bucket.latest_cursor)
            .unwrap_or_default()
    }

    fn supervisor_wait_scope(
        &self,
        supervisor_id: u64,
        bucket_id: u64,
        session_ids: &[u64],
    ) -> Result<SupervisorWaitScope, DaemonError> {
        let latest_cursor = self.bucket_event_cursor(bucket_id);
        let supervisor = match self.storage.get_session(supervisor_id) {
            Ok(session) => session,
            Err(_) => {
                return Ok((
                    latest_cursor,
                    Vec::new(),
                    Some(("unauthorized", supervisor_id)),
                ))
            }
        };
        let supervisor_bucket = self
            .storage
            .get_project(supervisor.project_id)
            .map(|project| project.bucket_id)
            .unwrap_or_default();
        if !supervisor.state.is_live()
            || supervisor.role != SessionRole::Supervisor
            || !supervisor.supervisor_api
            || supervisor_bucket != bucket_id
        {
            return Ok((
                latest_cursor,
                Vec::new(),
                Some(("unauthorized", supervisor_id)),
            ));
        }

        let mut snapshots = Vec::with_capacity(session_ids.len());
        for &session_id in session_ids {
            let session = match self.storage.get_session(session_id) {
                Ok(session) => session,
                Err(StorageError::NotFound("session", _)) => {
                    return Ok((latest_cursor, snapshots, Some(("missing", session_id))))
                }
                Err(error) => return Err(error.into()),
            };
            // The bucket is the boundary: a supervisor waits on any
            // session it can already see, not only the ones it spawned.
            let child_bucket = self.storage.get_project(session.project_id)?.bucket_id;
            if child_bucket != bucket_id {
                return Ok((latest_cursor, snapshots, Some(("unauthorized", session_id))));
            }
            snapshots.push(serde_json::json!({
                "session": session.id,
                "generation": self.session_generation(session.id),
                "state": session.state.as_str(),
                "headline": session.headline,
            }));
        }
        Ok((latest_cursor, snapshots, None))
    }

    fn wait_response(
        reason: &str,
        cursor: u64,
        changes: Vec<serde_json::Value>,
        snapshots: Vec<serde_json::Value>,
        baseline: bool,
        target_session: Option<u64>,
    ) -> serde_json::Value {
        let mut response = serde_json::json!({
            "reason": reason,
            "cursor": cursor,
            "changes": changes,
            "sessions": snapshots,
        });
        if baseline {
            response["baseline"] = serde_json::Value::Bool(true);
        }
        if let Some(session_id) = target_session {
            response["session"] = serde_json::json!(session_id);
        }
        response
    }

    fn ready_wait_response(
        &self,
        supervisor_id: u64,
        bucket_id: u64,
        session_ids: &[u64],
        after_cursor: Option<u64>,
        states: &HashSet<SessionState>,
    ) -> Result<Option<serde_json::Value>, DaemonError> {
        // State mutation + lifecycle publication use this same lock, making
        // the snapshots and cursor one atomic observation.
        let _guard = self.state_lock.lock().unwrap();
        let (latest_cursor, snapshots, unavailable) =
            self.supervisor_wait_scope(supervisor_id, bucket_id, session_ids)?;
        if let Some((reason, session_id)) = unavailable {
            return Ok(Some(Self::wait_response(
                reason,
                latest_cursor,
                Vec::new(),
                snapshots,
                false,
                Some(session_id),
            )));
        }
        let Some(after_cursor) = after_cursor else {
            return Ok(Some(Self::wait_response(
                "changed",
                latest_cursor,
                Vec::new(),
                snapshots,
                true,
                None,
            )));
        };
        if after_cursor > latest_cursor {
            return Err(DaemonError::Rejected(format!(
                "after_cursor {after_cursor} is newer than this bucket's latest cursor {latest_cursor}"
            )));
        }
        let watched: HashSet<u64> = session_ids.iter().copied().collect();
        let journal = self.session_wait_journal.lock().unwrap();
        if let Some(oldest) = journal
            .buckets
            .get(&bucket_id)
            .and_then(|bucket| bucket.events.front())
        {
            if after_cursor.saturating_add(1) < oldest.cursor {
                return Err(DaemonError::Rejected(format!(
                    "after_cursor {after_cursor} has expired; call wait_sessions without it for a new baseline"
                )));
            }
        }
        let changes = journal
            .buckets
            .get(&bucket_id)
            .into_iter()
            .flat_map(|bucket| bucket.events.iter())
            .filter(|event| {
                event.cursor > after_cursor
                    && watched.contains(&event.session_id)
                    && (states.is_empty() || states.contains(&event.to))
            })
            .map(|event| {
                serde_json::json!({
                    "cursor": event.cursor,
                    "session": event.session_id,
                    "generation": event.generation,
                    "from": event.from.as_str(),
                    "to": event.to.as_str(),
                    "timestamp_unix_ms": event.timestamp_unix_ms,
                    "headline": event.headline,
                })
            })
            .collect::<Vec<_>>();
        drop(journal);
        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self::wait_response(
            "changed",
            latest_cursor,
            changes,
            snapshots,
            false,
            None,
        )))
    }

    fn current_wait_response(
        &self,
        reason: &str,
        supervisor_id: u64,
        bucket_id: u64,
        session_ids: &[u64],
    ) -> Result<serde_json::Value, DaemonError> {
        let _guard = self.state_lock.lock().unwrap();
        let (cursor, snapshots, unavailable) =
            self.supervisor_wait_scope(supervisor_id, bucket_id, session_ids)?;
        if let Some((reason, session_id)) = unavailable {
            return Ok(Self::wait_response(
                reason,
                cursor,
                Vec::new(),
                snapshots,
                false,
                Some(session_id),
            ));
        }
        Ok(Self::wait_response(
            reason,
            cursor,
            Vec::new(),
            snapshots,
            false,
            None,
        ))
    }

    /// Waits for lifecycle transitions in any supervised child without
    /// reading terminal content or manufacturing session activity.
    pub async fn supervisor_wait_sessions(
        &self,
        supervisor_id: u64,
        bucket_id: u64,
        session_ids: Vec<u64>,
        after_cursor: Option<u64>,
        timeout: std::time::Duration,
        states: HashSet<SessionState>,
    ) -> Result<serde_json::Value, DaemonError> {
        let mut events = self.wait_events_tx.subscribe();
        let mut cancellations = self.wait_cancel_tx.subscribe();
        // A live wait already carries transitions to this supervisor, so
        // the daemon must not also nudge its terminal.
        let _waiting = self.supervisor_wait_guard(supervisor_id);
        if let Some(response) = self.ready_wait_response(
            supervisor_id,
            bucket_id,
            &session_ids,
            after_cursor,
            &states,
        )? {
            return Ok(response);
        }

        let deadline = tokio::time::Instant::now() + timeout;
        let mut authorization_tick = tokio::time::interval(std::time::Duration::from_millis(100));
        authorization_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    if let Some(response) = self.ready_wait_response(
                        supervisor_id,
                        bucket_id,
                        &session_ids,
                        after_cursor,
                        &states,
                    )? {
                        return Ok(response);
                    }
                    return self.current_wait_response(
                        "timeout",
                        supervisor_id,
                        bucket_id,
                        &session_ids,
                    );
                }
                cancellation = cancellations.recv() => {
                    if matches!(cancellation, Ok(session_id) if session_id == supervisor_id) {
                        return self.current_wait_response(
                            "cancelled",
                            supervisor_id,
                            bucket_id,
                            &session_ids,
                        );
                    }
                }
                _ = events.recv() => {}
                _ = authorization_tick.tick() => {}
            }
            if let Some(response) = self.ready_wait_response(
                supervisor_id,
                bucket_id,
                &session_ids,
                after_cursor,
                &states,
            )? {
                return Ok(response);
            }
        }
    }

    /// Resolves a project reference (name or id) inside a bucket. The
    /// error names the available projects so an agent self-corrects.
    pub fn resolve_project_in_bucket(
        &self,
        bucket_id: u64,
        reference: &serde_json::Value,
    ) -> Result<u64, DaemonError> {
        if let Some(id) = reference.as_u64() {
            let project = self.storage.get_project(id)?;
            if project.bucket_id != bucket_id {
                return Err(DaemonError::Rejected(format!(
                    "project {id} is not in this bucket"
                )));
            }
            return Ok(id);
        }
        let name = reference.as_str().unwrap_or_default();
        if let Some(project) = self.storage.project_by_name(bucket_id, name)? {
            return Ok(project.id);
        }
        let names = self.storage.project_names(bucket_id)?;
        Err(DaemonError::Rejected(format!(
            "project {name:?} not found in this bucket; projects here: {}",
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        )))
    }

    /// Resolves one blocked_by reference: an item id or an external key
    /// of an item already in the bucket.
    pub fn resolve_item_ref(
        &self,
        bucket_id: u64,
        reference: &serde_json::Value,
    ) -> Result<u64, DaemonError> {
        if bucket_id == 0 {
            return Err(DaemonError::Rejected(
                "a bucket is required; unqualified legacy item ids are unsupported".into(),
            ));
        }
        if let Some(id) = reference.as_u64() {
            self.storage.get_item(bucket_id, id).map_err(|_| {
                DaemonError::Rejected(format!("item {id} does not exist in this session's bucket"))
            })?;
            return Ok(id);
        }
        let key = reference.as_str().unwrap_or_default();
        if let Some(path) = key.strip_prefix("pm:item/") {
            let parts = path.split('/').collect::<Vec<_>>();
            match parts.as_slice() {
                [legacy] if legacy.parse::<u64>().is_ok() => {
                    return Err(DaemonError::Rejected(format!(
                        "legacy item reference {key} is unqualified and unsupported; use \
                         pm:item/<bucket-id>/<item-number>"
                    )));
                }
                [reference_bucket, item_number]
                    if reference_bucket.parse::<u64>().is_ok()
                        && item_number.parse::<u64>().is_ok() =>
                {
                    let reference_bucket = reference_bucket.parse::<u64>().unwrap_or_default();
                    let item_number = item_number.parse::<u64>().unwrap_or_default();
                    if reference_bucket != bucket_id {
                        return Err(DaemonError::Rejected(format!(
                            "item reference {key} is outside this session's bucket"
                        )));
                    }
                    self.storage.get_item(bucket_id, item_number).map_err(|_| {
                        DaemonError::Rejected(format!(
                            "item {item_number} does not exist in this session's bucket"
                        ))
                    })?;
                    return Ok(item_number);
                }
                _ => {
                    return Err(DaemonError::Rejected(format!(
                        "malformed item reference {key:?}; use \
                         pm:item/<bucket-id>/<item-number>"
                    )));
                }
            }
        }
        self.storage
            .item_id_by_external_key(bucket_id, key)?
            .ok_or_else(|| {
                DaemonError::Rejected(format!(
                    "item reference {key:?} does not match any item in this bucket"
                ))
            })
    }

    pub(crate) fn resolve_live_session(&self, session_token: &str) -> Result<u64, DaemonError> {
        if session_token.is_empty() {
            return Err(DaemonError::Rejected("empty session token".into()));
        }
        let session_id = self
            .storage
            .get_session_id_by_token(session_token)?
            .ok_or_else(|| {
                // A token is minted per terminal generation, so this is the
                // signature of an agent relaunched by hand inside its pane
                // or of a hook arriving from a superseded generation.
                warn!("rejected a session token that matches no live generation");
                DaemonError::Rejected("unknown session token".into())
            })?;
        let session = self.storage.get_session(session_id)?;
        if !session.state.is_live() {
            warn!(
                session = session_id,
                state = session.state.as_str(),
                "rejected a signal for a session that already ended"
            );
            return Err(DaemonError::Rejected(format!(
                "session {session_id} already ended"
            )));
        }
        Ok(session_id)
    }

    pub fn kill_session(&self, id: u64) -> Result<(), DaemonError> {
        let session = self.storage.get_session(id)?;
        let terminal = self.storage.agent_terminal(id)?;
        self.storage
            .set_agent_desired_running(id, terminal.id, false)?;
        if session.worker_id == LOCAL_WORKER_ID && !self.local_worker_enabled {
            self.release_published_names(id);
            return Ok(());
        }
        let result = (|| {
            if session.worker_id == LOCAL_WORKER_ID {
                self.mux.kill(terminal.id)?;
            } else {
                self.worker_link(session.worker_id)?
                    .send(ControllerMsg::TerminalKill {
                        terminal_id: terminal.id,
                        generation: terminal.generation,
                    })?;
            }
            Ok(())
        })();
        // A kill that did not take leaves the session wanted, so its names
        // stay reserved with it.
        if result.is_err() {
            let _ = self
                .storage
                .set_agent_desired_running(id, terminal.id, true);
        } else {
            self.release_published_names(id);
        }
        result
    }

    pub fn interrupt_session(&self, id: u64) -> Result<(), DaemonError> {
        let session = self.storage.get_session(id)?;
        self.ensure_local_worker_enabled(session.worker_id)?;
        let terminal = self.storage.agent_terminal(id)?;
        if session.worker_id == LOCAL_WORKER_ID {
            self.mux.interrupt(terminal.id)?;
        } else {
            self.worker_link(session.worker_id)?
                .send(ControllerMsg::TerminalInterrupt {
                    terminal_id: terminal.id,
                    generation: terminal.generation,
                })?;
        }
        self.observe_user_interaction(id, true);
        self.disable_terminal_fallback(&terminal);
        let _guard = self.state_lock.lock().unwrap();
        let current = self.storage.get_session(id)?;
        if matches!(
            current.state,
            SessionState::Starting | SessionState::Working
        ) {
            let updated =
                self.storage
                    .update_session_state(id, SessionState::Idle, INTERRUPTED_DETAIL)?;
            self.publish(Event::SessionChanged(updated));
        }
        Ok(())
    }

    /// The worker a session runs on, from the in-memory table, falling
    /// back to storage on a miss (then caching it). Keeps the per-input
    /// path off the database.
    fn route_worker(&self, terminal_id: u64) -> Option<u64> {
        if let Some(&worker_id) = self.terminal_workers.lock().unwrap().get(&terminal_id) {
            return Some(worker_id);
        }
        let terminal = self.storage.get_terminal(terminal_id).ok()?;
        let worker_id = self
            .storage
            .get_session(terminal.session_id)
            .ok()?
            .worker_id;
        self.terminal_workers
            .lock()
            .unwrap()
            .insert(terminal_id, worker_id);
        Some(worker_id)
    }

    fn remember_terminal_worker(&self, terminal_id: u64, worker_id: u64) {
        self.terminal_workers
            .lock()
            .unwrap()
            .insert(terminal_id, worker_id);
    }

    fn forget_terminal_worker(&self, terminal_id: u64) {
        self.terminal_workers.lock().unwrap().remove(&terminal_id);
    }

    /// Scrollback replay plus a live receiver for a session. Local and remote
    /// terminals share this viewer lifecycle; only stream startup transport
    /// differs.
    pub async fn attach(
        &self,
        session_id: u64,
    ) -> Result<
        (
            bytes::Bytes,
            broadcast::Receiver<bytes::Bytes>,
            Option<crate::workers::ViewerGuard>,
        ),
        DaemonError,
    > {
        let terminal = self.storage.agent_terminal(session_id)?;
        self.attach_terminal(terminal.id).await
    }

    pub async fn attach_terminal(
        &self,
        id: u64,
    ) -> Result<
        (
            bytes::Bytes,
            broadcast::Receiver<bytes::Bytes>,
            Option<crate::workers::ViewerGuard>,
        ),
        DaemonError,
    > {
        let terminal = self.storage.get_terminal(id)?;
        let link = self.terminal_link(id)?;
        let attach = link.viewer_attach(id);
        let guard = crate::workers::ViewerGuard::new(link.clone(), id, terminal.generation);
        match attach {
            crate::workers::ViewerAttach::Joined { rx, replay } => Ok((replay, rx, Some(guard))),
            crate::workers::ViewerAttach::Waiting { rx, replay } => {
                let replay = self.await_terminal_replay(&link, id, replay).await?;
                Ok((replay, rx, Some(guard)))
            }
            crate::workers::ViewerAttach::First { rx, replay } => {
                self.start_terminal_stream(&link, &terminal, 0)?;
                let replay = self.await_terminal_replay(&link, id, replay).await?;
                Ok((replay, rx, Some(guard)))
            }
        }
    }

    /// Attaches a web viewer, first sizing the PTY to `size` when the viewer
    /// sent one, so the snapshot it receives is already laid out for it.
    pub fn attach_web_terminal(
        &self,
        id: u64,
        replay_bytes: usize,
        size: Option<(u16, u16)>,
    ) -> Result<WebTerminalAttach, DaemonError> {
        self.attach_web_terminal_for_viewer(id, replay_bytes, size, 0, 0)
    }

    pub(crate) fn attach_web_terminal_for_viewer(
        &self,
        id: u64,
        replay_bytes: usize,
        size: Option<(u16, u16)>,
        viewer: u64,
        request: u64,
    ) -> Result<WebTerminalAttach, DaemonError> {
        let terminal = self.storage.get_terminal(id)?;
        let link = self.terminal_link(id)?;
        let previous_size = link.viewer_size_feed(id).0;
        let size = size.filter(|&(cols, rows)| {
            self.viewer_resize(id, terminal.generation, viewer, request, cols, rows)
        });
        let resized = size.is_some_and(|size| size != previous_size);
        let attach = link.streaming_viewer_attach(id);
        if let (crate::workers::StreamingViewerAttach::First { .. }, Some((cols, rows))) =
            (&attach, size)
        {
            // No stream carried the resize above to a remote worker, so the
            // attach itself asks for this size.
            if !link.is_local() {
                link.mirror_terminal_resize(id, cols, rows);
            }
        }
        let guard = crate::workers::ViewerGuard::new(link.clone(), id, terminal.generation);
        let (pty_size, size_rx) = link.viewer_size_feed(id);
        let rewrite_rx = link.viewer_rewrite_feed(id);
        let progress = link.viewer_progress(id);
        let assemble = |replay, output| WebTerminalAttach {
            replay,
            output,
            pty_size,
            size_rx,
            rewrite_rx,
            resized,
            guard: Some(guard),
            progress,
        };
        match attach {
            crate::workers::StreamingViewerAttach::Joined { rx, replay } => {
                Ok(assemble(WebTerminalReplay::Complete { bytes: replay }, rx))
            }
            crate::workers::StreamingViewerAttach::Waiting { rx, replay } => {
                Ok(assemble(WebTerminalReplay::Streaming(replay), rx))
            }
            crate::workers::StreamingViewerAttach::First { rx, replay } => {
                self.start_terminal_stream(&link, &terminal, replay_bytes)?;
                Ok(assemble(WebTerminalReplay::Streaming(replay), rx))
            }
        }
    }

    async fn await_terminal_replay(
        &self,
        link: &std::sync::Arc<crate::workers::WorkerLink>,
        terminal_id: u64,
        replay: tokio::sync::oneshot::Receiver<bytes::Bytes>,
    ) -> Result<bytes::Bytes, DaemonError> {
        match tokio::time::timeout(ATTACH_TIMEOUT, replay).await {
            Ok(Ok(replay)) => Ok(replay),
            _ => {
                link.remove_session(terminal_id);
                Err(DaemonError::Rejected(
                    "worker terminal stream did not become ready".into(),
                ))
            }
        }
    }

    fn terminal_link(
        &self,
        terminal_id: u64,
    ) -> Result<std::sync::Arc<crate::workers::WorkerLink>, DaemonError> {
        match self.route_worker(terminal_id) {
            Some(LOCAL_WORKER_ID) => {
                self.ensure_local_worker_enabled(LOCAL_WORKER_ID)?;
                Ok(self.local_worker.clone())
            }
            Some(worker_id) => self.worker_link(worker_id),
            None => Err(DaemonError::Storage(StorageError::NotFound(
                "terminal",
                terminal_id,
            ))),
        }
    }

    fn start_terminal_stream(
        &self,
        link: &std::sync::Arc<crate::workers::WorkerLink>,
        terminal: &pm_protocol::domain::Terminal,
        replay_bytes: usize,
    ) -> Result<(), DaemonError> {
        if link.is_local() {
            link.start_local_terminal_stream(terminal.id, terminal.generation)?;
            Ok(())
        } else {
            self.start_remote_terminal_stream(link, terminal, replay_bytes)
        }
    }

    fn start_remote_terminal_stream(
        &self,
        link: &std::sync::Arc<crate::workers::WorkerLink>,
        terminal: &pm_protocol::domain::Terminal,
        replay_bytes: usize,
    ) -> Result<(), DaemonError> {
        let token = crate::auth::generate_token();
        let token_hash = crate::auth::hash_token(&token);
        if !self
            .terminal_streams
            .issue(token_hash.clone(), link, terminal.id, terminal.generation)
        {
            return Err(DaemonError::Rejected(
                "worker terminal stream limit reached".into(),
            ));
        }
        link.send(ControllerMsg::TerminalAttach {
            terminal_id: terminal.id,
            generation: terminal.generation,
            token,
            replay_bytes: replay_bytes as u64,
            size: link.relay_size(terminal.id),
        })
        .inspect_err(|_| self.terminal_streams.revoke(&token_hash))?;
        Ok(())
    }

    pub fn pty_input(&self, id: u64, data: bytes::Bytes) {
        if let Ok(t) = self.storage.agent_terminal(id) {
            let submitted = data.iter().any(|byte| matches!(byte, b'\r' | b'\n'));
            let input_state = self.storage.get_session(t.session_id).map(|s| s.state).ok();
            if !data.is_empty() && self.deliver_terminal_input(&t, data) {
                self.observe_user_interaction(t.session_id, submitted);
                if let Some(input_state) = input_state {
                    self.acknowledge_user_input(&t, submitted, input_state);
                }
            }
        }
    }

    pub fn terminal_input(&self, id: u64, data: bytes::Bytes) {
        let submitted = data.iter().any(|byte| matches!(byte, b'\r' | b'\n'));
        self.terminal_input_with_submission(id, data, submitted);
    }

    pub fn terminal_input_with_submission(&self, id: u64, data: bytes::Bytes, submitted: bool) {
        if let Ok(t) = self.storage.get_terminal(id) {
            let input_state = self.storage.get_session(t.session_id).map(|s| s.state).ok();
            if !data.is_empty()
                && self.deliver_terminal_input(&t, data)
                && t.kind == pm_protocol::domain::TerminalKind::Agent
            {
                self.observe_user_interaction(t.session_id, submitted);
                if let Some(input_state) = input_state {
                    self.acknowledge_user_input(&t, submitted, input_state);
                }
            }
        }
    }

    /// Input for a terminal the caller already resolved: no database read
    /// per keystroke, and the agent-input acknowledgement runs off the loop.
    pub fn terminal_input_for(
        self: &std::sync::Arc<Self>,
        t: &pm_protocol::domain::Terminal,
        data: bytes::Bytes,
        submitted: bool,
    ) {
        if data.is_empty() || !self.deliver_terminal_input(t, data) {
            return;
        }
        if t.kind != pm_protocol::domain::TerminalKind::Agent {
            return;
        }
        let daemon = std::sync::Arc::clone(self);
        let t = t.clone();
        tokio::task::spawn_blocking(move || {
            let input_state = daemon
                .storage
                .get_session(t.session_id)
                .map(|s| s.state)
                .ok();
            daemon.observe_user_interaction(t.session_id, submitted);
            if let Some(input_state) = input_state {
                daemon.acknowledge_user_input(&t, submitted, input_state);
            }
        });
    }

    fn deliver_terminal_input(
        &self,
        terminal: &pm_protocol::domain::Terminal,
        data: bytes::Bytes,
    ) -> bool {
        self.try_deliver_terminal_input(terminal, data).is_ok()
    }

    pub(crate) fn try_deliver_terminal_input(
        &self,
        terminal: &pm_protocol::domain::Terminal,
        data: bytes::Bytes,
    ) -> Result<(), TerminalInputFailure> {
        let link = self
            .terminal_link(terminal.id)
            .map_err(|_| TerminalInputFailure::MissingPty)?;
        link.terminal_input(terminal.id, terminal.generation, data)
            .map_err(|error| match error {
                crate::workers::WorkerError::Busy => TerminalInputFailure::Busy,
                crate::workers::WorkerError::Closed | crate::workers::WorkerError::Offline(_) => {
                    TerminalInputFailure::Transport
                }
            })
    }

    fn acknowledge_user_input(
        &self,
        terminal: &pm_protocol::domain::Terminal,
        submitted: bool,
        input_state: SessionState,
    ) {
        // An agent reporting through Program Status says when its turn
        // starts, and a guess from input would only be overwritten.
        if self.program_status_decides(terminal.session_id) {
            return;
        }
        let Ok(session) = self.storage.get_session(terminal.session_id) else {
            return;
        };
        if !user_input_acknowledges_state(
            input_state,
            session.state,
            submitted,
            self.adapter_has_lifecycle_hooks(session.agent),
        ) {
            return;
        }
        self.disable_terminal_fallback(terminal);
        let _guard = self.state_lock.lock().unwrap();
        let Ok(session) = self.storage.get_session(terminal.session_id) else {
            return;
        };
        if !user_input_acknowledges_state(
            input_state,
            session.state,
            submitted,
            self.adapter_has_lifecycle_hooks(session.agent),
        ) {
            return;
        }
        if let Ok(updated) =
            self.storage
                .update_session_state(terminal.session_id, SessionState::Working, "")
        {
            self.publish(Event::SessionChanged(updated));
        }
    }

    pub fn pty_resize(&self, id: u64, cols: u16, rows: u16) {
        if let Ok(t) = self.storage.agent_terminal(id) {
            self.terminal_resize(t.id, cols, rows);
        }
    }

    /// A viewer's in-band snapshot request, answered from the relay model.
    pub fn web_terminal_resnapshot(
        &self,
        id: u64,
        progress: &crate::workers::ViewerProgress,
    ) -> Option<(
        bytes::Bytes,
        tokio::sync::broadcast::Receiver<bytes::Bytes>,
        (u16, u16),
    )> {
        self.terminal_link(id).ok()?.resnapshot_viewer(id, progress)
    }

    pub(crate) fn viewer_resize(
        &self,
        id: u64,
        generation: u64,
        viewer: u64,
        request: u64,
        cols: u16,
        rows: u16,
    ) -> bool {
        if cols < crate::mux::MIN_COLS || rows < crate::mux::MIN_ROWS {
            return false;
        }
        let Ok(terminal) = self.storage.get_terminal(id) else {
            return false;
        };
        if terminal.generation != generation {
            return false;
        }
        let mut applied = false;
        self.viewer_owners
            .claim(id, generation, viewer, request, (cols, rows), || {
                applied = true;
                if let Ok(link) = self.terminal_link(id) {
                    let _ = link.terminal_resize(id, generation, cols, rows);
                    link.mirror_terminal_resize(id, cols, rows);
                }
            });
        applied
    }

    pub fn terminal_resize(&self, id: u64, cols: u16, rows: u16) {
        if let Ok(terminal) = self.storage.get_terminal(id) {
            self.viewer_resize(id, terminal.generation, 0, 0, cols, rows);
        }
    }

    pub(crate) fn worker_link(
        &self,
        worker_id: u64,
    ) -> Result<std::sync::Arc<crate::workers::WorkerLink>, DaemonError> {
        self.workers.get(worker_id).ok_or(DaemonError::Worker(
            crate::workers::WorkerError::Offline(worker_id),
        ))
    }

    pub fn scrollback_path(&self, session_id: u64) -> PathBuf {
        self.storage
            .agent_terminal(session_id)
            .map(|t| self.terminal_scrollback_path(t.id, t.generation))
            .unwrap_or_else(|_| {
                self.scrollback_dir
                    .join(format!("session-{session_id}.bin"))
            })
    }

    pub fn terminal_scrollback_path(&self, terminal_id: u64, generation: u64) -> PathBuf {
        self.scrollback_dir.join(format!(
            "terminal-{terminal_id}-generation-{generation}.bin"
        ))
    }

    pub fn live_session_count(&self) -> Result<usize, DaemonError> {
        Ok(self.storage.live_session_count()?)
    }

    pub fn begin_shutdown(&self) -> usize {
        self.shutdown_terminals
            .lock()
            .unwrap()
            .extend(self.mux.live_terminal_ids());
        self.mux.terminate_all()
    }

    pub fn recover_local_terminals(&self) {
        if !self.local_worker_enabled {
            return;
        }
        let sessions = match self.storage.auto_resume_sessions_on_worker(LOCAL_WORKER_ID) {
            Ok(sessions) => sessions,
            Err(e) => {
                error!(error = %e, "failed to load sessions for startup recovery");
                return;
            }
        };
        for session in sessions {
            let Ok(agent_terminal) = self.storage.agent_terminal(session.id) else {
                continue;
            };
            if self.mux.is_running(agent_terminal.id) {
                continue;
            }
            self.refresh_local_agent_resumability(session.id);
            if let Err(e) = self.resume_session_with_state(
                session.id,
                SessionState::Idle,
                RECOVERED_STATE_DETAIL,
            ) {
                let _ = self.storage.set_session_desired_running(session.id, false);
                let _ = self
                    .storage
                    .set_terminal_desired_running(agent_terminal.id, false);
                let detail = format!("automatic resume failed: {e}");
                if let Ok(updated) = self.storage.set_session_ended(
                    session.id,
                    SessionState::Failed,
                    &detail,
                    None,
                    now_unix_ms(),
                ) {
                    self.publish(Event::SessionChanged(updated));
                }
                warn!(session = session.id, error = %e, "automatic resume failed");
            }
        }

        let terminals = match self.storage.desired_terminals_on_worker(LOCAL_WORKER_ID) {
            Ok(terminals) => terminals,
            Err(e) => {
                error!(error = %e, "failed to load shells for startup recovery");
                return;
            }
        };
        for terminal in terminals {
            if terminal.kind != pm_protocol::domain::TerminalKind::Shell
                || self.mux.is_running(terminal.id)
            {
                continue;
            }
            if let Err(e) = self.recover_shell(&terminal, LOCAL_WORKER_ID) {
                let _ = self
                    .storage
                    .set_terminal_desired_running(terminal.id, false);
                warn!(terminal = terminal.id, error = %e, "automatic shell restart failed");
            }
        }
    }

    pub fn agent_terminal(
        &self,
        session_id: u64,
    ) -> Result<pm_protocol::domain::Terminal, DaemonError> {
        Ok(self.storage.agent_terminal(session_id)?)
    }

    pub fn terminal(&self, terminal_id: u64) -> Result<pm_protocol::domain::Terminal, DaemonError> {
        Ok(self.storage.get_terminal(terminal_id)?)
    }

    pub fn create_shell(&self, session_id: u64, title: &str) -> Result<u64, DaemonError> {
        let session = self.storage.get_session(session_id)?;
        self.ensure_local_worker_enabled(session.worker_id)?;
        if session.worker_id != LOCAL_WORKER_ID {
            self.worker_link(session.worker_id)?;
        }
        let title = if title.trim().is_empty() {
            "Shell"
        } else {
            title
        };
        let terminal = self
            .storage
            .create_shell(session_id, title, now_unix_ms())?;
        self.remember_terminal_worker(terminal.id, session.worker_id);
        if let Err(error) = self.spawn_shell_run(&terminal, session.worker_id) {
            let _ = self.storage.delete_terminal(terminal.id);
            self.forget_terminal_worker(terminal.id);
            return Err(error);
        }
        Ok(terminal.id)
    }

    fn spawn_shell_run(
        &self,
        terminal: &pm_protocol::domain::Terminal,
        worker_id: u64,
    ) -> Result<(), DaemonError> {
        let initial_size = self
            .storage
            .agent_terminal(terminal.session_id)
            .ok()
            .and_then(|agent_term| {
                if worker_id == LOCAL_WORKER_ID {
                    self.mux.current_size(agent_term.id).ok()
                } else {
                    self.worker_link(worker_id)
                        .ok()
                        .map(|l| l.relay_size(agent_term.id))
                }
            });
        if worker_id == LOCAL_WORKER_ID {
            self.ensure_local_worker_enabled(worker_id)?;
            let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            let spec = pm_adapters::CommandSpec {
                program,
                args: Vec::new(),
                env: Vec::new(),
                cwd: PathBuf::from(&terminal.cwd),
            };
            self.mux.spawn(
                terminal.id,
                terminal.generation,
                terminal.session_id,
                &spec,
                false,
                true,
                self.spawn_truecolor(),
                initial_size,
                false,
            )?;
        } else {
            let link = self.worker_link(worker_id)?;
            let (initial_cols, initial_rows) =
                if link.protocol_version() >= pm_protocol::WORKER_PROTOCOL_SPAWN_INITIAL_SIZE {
                    (initial_size.map(|(c, _)| c), initial_size.map(|(_, r)| r))
                } else {
                    (None, None)
                };
            link.send(ControllerMsg::SpawnShell {
                terminal_id: terminal.id,
                generation: terminal.generation,
                cwd: terminal.cwd.clone(),
                truecolor: self.spawn_truecolor(),
                initial_cols,
                initial_rows,
            })?;
        }
        let updated = self.storage.update_terminal_run(
            terminal.id,
            terminal.generation,
            pm_protocol::domain::TerminalRunState::Running,
            None,
            false,
            now_unix_ms(),
        )?;
        self.publish(Event::TerminalChanged(updated));
        Ok(())
    }

    fn recover_shell(
        &self,
        terminal: &pm_protocol::domain::Terminal,
        worker_id: u64,
    ) -> Result<(), DaemonError> {
        if terminal.state.is_live() {
            self.storage.update_terminal_run(
                terminal.id,
                terminal.generation,
                pm_protocol::domain::TerminalRunState::Exited,
                None,
                false,
                now_unix_ms(),
            )?;
        }
        let run = self.storage.restart_terminal(terminal.id, now_unix_ms())?;
        if let Err(e) = self.spawn_shell_run(&run, worker_id) {
            let _ = self.storage.update_terminal_run(
                run.id,
                run.generation,
                pm_protocol::domain::TerminalRunState::Failed,
                None,
                false,
                now_unix_ms(),
            );
            return Err(e);
        }
        Ok(())
    }

    pub fn restart_terminal(&self, terminal_id: u64) -> Result<(), DaemonError> {
        let old = self.storage.get_terminal(terminal_id)?;
        if old.kind == pm_protocol::domain::TerminalKind::Agent {
            return Err(DaemonError::Rejected(
                "resume the session to restart its agent terminal".into(),
            ));
        }
        if old.state.is_live() {
            return Err(DaemonError::Rejected("terminal is still running".into()));
        }
        let worker_id = self.storage.get_session(old.session_id)?.worker_id;
        self.ensure_local_worker_enabled(worker_id)?;
        let terminal = self.storage.restart_terminal(terminal_id, now_unix_ms())?;
        self.spawn_shell_run(&terminal, worker_id)
    }

    pub fn close_terminal(&self, terminal_id: u64) -> Result<(), DaemonError> {
        let terminal = match self.storage.get_terminal(terminal_id) {
            Ok(terminal) => terminal,
            Err(StorageError::NotFound("terminal", id)) if id == terminal_id => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if terminal.kind == pm_protocol::domain::TerminalKind::Agent {
            return Err(DaemonError::Rejected(
                "the agent terminal cannot be closed".into(),
            ));
        }
        self.storage
            .set_terminal_desired_running(terminal_id, false)?;
        if terminal.state.is_live() {
            let worker_id = self.storage.get_session(terminal.session_id)?.worker_id;
            if worker_id == LOCAL_WORKER_ID {
                let _ = self.mux.kill(terminal_id);
            } else {
                self.worker_link(worker_id)?
                    .send(ControllerMsg::TerminalKill {
                        terminal_id,
                        generation: terminal.generation,
                    })?;
            }
        }
        self.remove_shell_terminal(terminal_id)?;
        Ok(())
    }

    fn remove_shell_terminal(&self, terminal_id: u64) -> Result<(), DaemonError> {
        match self.storage.delete_terminal(terminal_id) {
            Ok(()) => {}
            Err(StorageError::NotFound("terminal", id)) if id == terminal_id => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        self.forget_terminal_worker(terminal_id);
        self.publish(Event::TerminalRemoved(terminal_id));
        Ok(())
    }

    /// Consumes mux exit notifications: persists the transcript, marks
    /// the session ended, and publishes the change.
    pub fn handle_session_exit(&self, exit: SessionExit) {
        self.report_freshness.forget(exit.semantic_session_id);
        let terminal = match self.storage.get_terminal(exit.terminal_id) {
            Ok(t) => t,
            Err(StorageError::NotFound("terminal", id)) if id == exit.terminal_id => {
                self.shutdown_terminals.lock().unwrap().remove(&id);
                self.mux.remove(id);
                self.forget_terminal_worker(id);
                return;
            }
            Err(e) => {
                error!(terminal = exit.terminal_id, error = %e, "failed to resolve terminal exit");
                return;
            }
        };
        if terminal.generation != exit.generation {
            debug!(
                terminal = exit.terminal_id,
                stale_generation = exit.generation,
                current_generation = terminal.generation,
                "ignored stale terminal exit"
            );
            return;
        }
        // Match remote worker lifecycle cleanup before this terminal id can
        // be reused by a resumed generation.
        self.local_worker.remove_session(exit.terminal_id);
        let path = self.terminal_scrollback_path(exit.terminal_id, exit.generation);
        if let Err(e) = std::fs::write(&path, &exit.scrollback) {
            error!(terminal = exit.terminal_id, error = %e, "failed to write transcript");
        }
        let _ = self.storage.update_terminal_run(
            exit.terminal_id,
            exit.generation,
            pm_protocol::domain::TerminalRunState::Exited,
            exit.exit_code,
            true,
            now_unix_ms(),
        );
        let paused_for_shutdown = self
            .shutdown_terminals
            .lock()
            .unwrap()
            .remove(&exit.terminal_id);
        if !paused_for_shutdown {
            if terminal.kind == pm_protocol::domain::TerminalKind::Agent {
                let _ = self.storage.set_agent_desired_running(
                    terminal.session_id,
                    exit.terminal_id,
                    false,
                );
            } else {
                let _ = self
                    .storage
                    .set_terminal_desired_running(exit.terminal_id, false);
            }
        }
        if terminal.kind != pm_protocol::domain::TerminalKind::Agent {
            self.mux.remove(exit.terminal_id);
            if !paused_for_shutdown && exit.exit_code == Some(0) {
                if let Err(error) = self.remove_shell_terminal(exit.terminal_id) {
                    error!(terminal = exit.terminal_id, error = %error, "failed to remove exited shell");
                }
                return;
            }
            self.publish(Event::TerminalChanged(
                self.storage
                    .get_terminal(exit.terminal_id)
                    .unwrap_or(terminal),
            ));
            return;
        }
        self.refresh_local_agent_resumability(terminal.session_id);
        self.program_status_exited(terminal.session_id);
        let _guard = self.state_lock.lock().unwrap();
        match self.storage.set_session_ended(
            terminal.session_id,
            SessionState::Exited,
            "",
            exit.exit_code,
            now_unix_ms(),
        ) {
            Ok(updated) => {
                info!(
                    session = terminal.session_id,
                    code = ?exit.exit_code,
                    "session exited"
                );
                self.publish(Event::SessionChanged(updated));
            }
            Err(e) => error!(session = terminal.session_id, error = %e, "failed to persist exit"),
        }
        self.mux.remove(exit.terminal_id);
        self.forget_terminal_worker(exit.terminal_id);
    }

    fn refresh_local_agent_resumability(&self, session_id: u64) {
        let Ok(session) = self.storage.get_session(session_id) else {
            return;
        };
        if session.worker_id != LOCAL_WORKER_ID {
            return;
        }
        let resumable = session.agent_session_id.is_some()
            && self
                .storage
                .get_transcript_path(session_id)
                .ok()
                .flatten()
                .filter(|path| !path.is_empty())
                .is_none_or(|path| std::path::Path::new(&path).exists());
        let _ = self.storage.set_agent_resumable(session_id, resumable);
    }

    /// Validates a connecting worker (enrollment on first join, else the key
    /// the handshake proved), marks it online, and returns its link plus the
    /// control-message receiver the connection drains and the credential
    /// to hand back on a fresh enrollment.
    pub fn register_worker_connection(
        self: &std::sync::Arc<Self>,
        hello: WorkerHello<'_>,
    ) -> Result<WorkerRegistration, DaemonError> {
        let WorkerHello {
            enrollment_token,
            credential,
            peer_key_hash,
            hostname,
            platform,
            pm_version,
            runtime,
            container,
            default_project_root,
            live_sessions,
            live_terminals,
            protocol_version,
        } = hello;
        if peer_key_hash.is_empty() {
            return Err(DaemonError::Rejected(
                "worker registration needs an authenticated peer key".into(),
            ));
        }
        let now = now_unix_ms();
        let (worker_id, issued_credential) = if !enrollment_token.is_empty() {
            let hash = crate::auth::hash_token(enrollment_token);
            let enrollment = self.storage.consume_worker_enrollment(&hash, now)?;
            let cred = crate::auth::generate_token();
            let cred_hash = crate::auth::hash_token(&cred);
            let name = if enrollment.label.trim().is_empty() {
                hostname
            } else {
                enrollment.label.trim()
            };
            // A host's pinned key is its identity. An enrollment minted
            // against a specific host names the row to rotate; otherwise a
            // key the controller already knows is the same machine
            // enrolling again, and rotating that row in place is what keeps
            // its per-worker project paths, bucket defaults, and session
            // history attached. Registering would strand all of it on an id
            // nothing connects as any more.
            let known = self.storage.worker_by_key_hash(peer_key_hash)?;
            if let (Some(target), Some(known)) = (enrollment.worker_id, known) {
                if target != known {
                    let name = self
                        .storage
                        .get_worker(known)
                        .map(|worker| worker.name)
                        .unwrap_or_else(|_| known.to_string());
                    return Err(DaemonError::Rejected(format!(
                        "this machine is already enrolled as host \"{name}\" (id {known}); \
                         re-enroll that host instead of adding a second one, or its \
                         per-host project paths and session history stay on the old entry"
                    )));
                }
            }
            // The redeeming key is not the one this host is pinned to, so the
            // machine holding the old key stops being able to connect. A host
            // added through the UI has no key pinned yet, so its first
            // registration is a machine arriving rather than one displaced.
            let displaces_the_pinned_key = match enrollment.worker_id {
                Some(target) => {
                    known != Some(target)
                        && self.storage.worker_has_pinned_key(target).unwrap_or(false)
                }
                None => false,
            };
            let worker = match enrollment.worker_id.or(known) {
                Some(id) => self.storage.rebind_worker(
                    id,
                    name,
                    hostname,
                    platform,
                    pm_version,
                    default_project_root,
                    &cred_hash,
                    peer_key_hash,
                    now,
                )?,
                None => self.storage.register_worker(
                    name,
                    hostname,
                    platform,
                    pm_version,
                    default_project_root,
                    &cred_hash,
                    peer_key_hash,
                    now,
                )?,
            };
            self.storage.set_worker_connection(
                worker.id,
                enrollment.connect_mode,
                &enrollment.endpoint,
            )?;
            if displaces_the_pinned_key {
                warn!(
                    worker = worker.id,
                    host = worker.name.as_str(),
                    key = peer_key_hash,
                    hostname,
                    mode = enrollment.connect_mode.as_str(),
                    endpoint = enrollment.endpoint.as_str(),
                    "a re-enrollment replaced this host's key: the machine that held the \
                     old one can no longer connect, and work dispatched to this host now \
                     runs here"
                );
                self.publish_security_notice(
                    pm_protocol::domain::SecurityNoticeKind::HostKeyReplaced,
                    &worker.name,
                    "this host is now a different machine, keeping its project paths and \
                     history. Work dispatched to it runs there. Remove it if you did not \
                     re-enroll it.",
                );
            } else {
                self.publish_security_notice(
                    pm_protocol::domain::SecurityNoticeKind::HostEnrolled,
                    &worker.name,
                    "this machine joined as a host and can be dispatched sessions, which \
                     run as its user on it.",
                );
            }
            (worker.id, cred)
        } else {
            let id = self
                .storage
                .worker_by_key_hash(peer_key_hash)?
                .ok_or_else(|| DaemonError::Rejected("unknown host key".into()))?;
            // The handshake already proved the key. Checking the credential
            // too means a stolen host key alone does not impersonate the host.
            let expected = self.storage.get_worker_credential_hash(id)?;
            if expected.is_none_or(|stored| stored != crate::auth::hash_token(credential)) {
                return Err(DaemonError::Rejected("unknown worker credential".into()));
            }
            self.storage.touch_worker(id, now)?;
            // Refresh unconditionally: a reconnect after an upgrade is
            // exactly when the stored build goes stale, and a build
            // without version reporting must read as unknown again.
            self.storage
                .set_worker_report(id, pm_version, runtime, container)?;
            (id, String::new())
        };

        let (link, rx) = self.workers.connect(worker_id);
        link.set_protocol_version(protocol_version);
        // A host the controller dials cannot dial back, so its streams are
        // opened from this end too.
        if let Ok(worker) = self.storage.get_worker(worker_id) {
            if worker.connect_mode == ConnectMode::Accept && !worker.endpoint.is_empty() {
                if let Some(host_key) = pm_tls::KeyHash::from_hex(peer_key_hash) {
                    link.dials_streams(std::sync::Arc::new(crate::workers::StreamDial {
                        endpoint: worker.endpoint.clone(),
                        host_key,
                        daemon: std::sync::Arc::downgrade(self),
                    }));
                }
            }
        }
        let epoch = {
            let mut epochs = self.worker_epochs.lock().unwrap();
            let e = epochs.entry(worker_id).or_insert(0);
            *e += 1;
            *e
        };
        let mut announced = live_terminals.to_vec();
        if announced.is_empty() {
            for session_id in live_sessions {
                if let Ok(terminal) = self.storage.agent_terminal(*session_id) {
                    announced.push(WorkerTerminal {
                        terminal_id: terminal.id,
                        generation: terminal.generation,
                        kind: pm_protocol::domain::TerminalKind::Agent,
                        state: pm_protocol::domain::TerminalRunState::Running,
                        agent_resumable: false,
                        transcript_available: false,
                    });
                }
            }
        }
        self.reconcile_worker_terminals(worker_id, &announced);
        {
            let _guard = self.state_lock.lock().unwrap();
            if let Ok(worker) = self.worker_snapshot(worker_id) {
                self.publish(Event::WorkerChanged(worker));
            }
        }
        info!(worker = worker_id, hostname, "worker connected");
        Ok(WorkerRegistration {
            worker_id,
            credential: issued_credential,
            link,
            rx,
            epoch,
        })
    }

    fn reconcile_worker_terminals(&self, worker_id: u64, announced: &[WorkerTerminal]) {
        for inventory in announced {
            let Ok(terminal) = self.storage.get_terminal(inventory.terminal_id) else {
                continue;
            };
            let Ok(session) = self.storage.get_session(terminal.session_id) else {
                continue;
            };
            if session.worker_id != worker_id
                || terminal.generation != inventory.generation
                || terminal.kind != inventory.kind
                || !inventory.state.is_live()
            {
                let _ = self.worker_link(worker_id).and_then(|link| {
                    link.send(ControllerMsg::TerminalKill {
                        terminal_id: inventory.terminal_id,
                        generation: inventory.generation,
                    })
                    .map_err(DaemonError::from)
                });
                continue;
            }
            let desired = self
                .storage
                .terminal_desired_running(terminal.id)
                .unwrap_or(false);
            if !desired {
                let _ = self.worker_link(worker_id).and_then(|link| {
                    link.send(ControllerMsg::TerminalKill {
                        terminal_id: terminal.id,
                        generation: terminal.generation,
                    })
                    .map_err(DaemonError::from)
                });
                continue;
            }
            self.remember_terminal_worker(terminal.id, worker_id);
            if let Ok(updated) = self.storage.update_terminal_run(
                terminal.id,
                terminal.generation,
                pm_protocol::domain::TerminalRunState::Running,
                None,
                inventory.transcript_available,
                now_unix_ms(),
            ) {
                self.publish(Event::TerminalChanged(updated));
            }
            if terminal.kind == pm_protocol::domain::TerminalKind::Agent {
                if inventory.agent_resumable {
                    let _ = self.storage.set_agent_resumable(session.id, true);
                }
                let restored = if session.state == SessionState::Starting {
                    self.storage.update_session_state(
                        session.id,
                        SessionState::Idle,
                        RECOVERED_STATE_DETAIL,
                    )
                } else if session.state.is_live() {
                    self.storage.get_session(session.id)
                } else {
                    self.storage
                        .reactivate_session(session.id, SessionState::Idle)
                };
                if let Ok(updated) = restored {
                    self.publish(Event::SessionChanged(updated));
                }
            }
        }

        self.end_orphaned_worker_sessions(worker_id, announced);

        let desired = match self.storage.desired_terminals_on_worker(worker_id) {
            Ok(terminals) => terminals,
            Err(_) => return,
        };
        for terminal in desired {
            if announced.iter().any(|item| {
                item.terminal_id == terminal.id && item.generation == terminal.generation
            }) {
                continue;
            }
            self.forget_terminal_worker(terminal.id);
            if terminal.kind == pm_protocol::domain::TerminalKind::Agent {
                let Ok(session) = self.storage.get_session(terminal.session_id) else {
                    continue;
                };
                if let Err(e) = self.resume_session_with_state(
                    session.id,
                    SessionState::Idle,
                    RECOVERED_STATE_DETAIL,
                ) {
                    let _ = self.storage.set_session_desired_running(session.id, false);
                    let _ = self
                        .storage
                        .set_terminal_desired_running(terminal.id, false);
                    let detail = format!("automatic resume failed: {e}");
                    if let Ok(updated) = self.storage.set_session_ended(
                        session.id,
                        SessionState::Failed,
                        &detail,
                        None,
                        now_unix_ms(),
                    ) {
                        self.publish(Event::SessionChanged(updated));
                    }
                    warn!(session = session.id, worker = worker_id, error = %e, "automatic remote resume failed");
                }
            } else if let Err(e) = self.recover_shell(&terminal, worker_id) {
                let _ = self
                    .storage
                    .set_terminal_desired_running(terminal.id, false);
                warn!(terminal = terminal.id, worker = worker_id, error = %e, "automatic remote shell restart failed");
            }
        }
    }

    /// Ends live sessions whose agent terminal the reconnecting worker no
    /// longer has and nothing wants resumed. No later message from the
    /// worker would ever close them.
    fn end_orphaned_worker_sessions(&self, worker_id: u64, announced: &[WorkerTerminal]) {
        let orphaned = match self
            .storage
            .undesired_live_agent_terminals_on_worker(worker_id)
        {
            Ok(terminals) => terminals,
            Err(e) => {
                warn!(worker = worker_id, error = %e, "failed to list orphaned sessions");
                return;
            }
        };
        let now = now_unix_ms();
        for terminal in orphaned {
            if announced.iter().any(|item| {
                item.terminal_id == terminal.id && item.generation == terminal.generation
            }) {
                continue;
            }
            self.forget_terminal_worker(terminal.id);
            let _guard = self.state_lock.lock().unwrap();
            if let Ok(updated) = self.storage.set_session_ended(
                terminal.session_id,
                SessionState::Failed,
                ORPHANED_SESSION_DETAIL,
                None,
                now,
            ) {
                info!(
                    session = terminal.session_id,
                    worker = worker_id,
                    "ended session the worker reconnected without"
                );
                self.publish(Event::SessionChanged(updated));
                if let Ok(ended) = self.storage.get_terminal(terminal.id) {
                    self.publish(Event::TerminalChanged(ended));
                }
            }
        }
    }

    /// Marks a worker offline without failing its sessions; they are kept
    /// alive for the reconnect grace period. Returns the worker id.
    pub fn disconnect_worker(&self, link: &std::sync::Arc<crate::workers::WorkerLink>) -> u64 {
        let worker_id = link.worker_id;
        self.workers.disconnect(link);
        let _guard = self.state_lock.lock().unwrap();
        if let Ok(worker) = self.worker_snapshot(worker_id) {
            self.publish(Event::WorkerChanged(worker));
        }
        if let Ok(mut sessions) = self.storage.auto_resume_sessions_on_worker(worker_id) {
            self.overlay_awaiting_worker_sessions(&mut sessions);
            for session in sessions {
                self.publish(Event::SessionChanged(session));
            }
        }
        info!(worker = worker_id, "worker disconnected");
        worker_id
    }

    /// Fails a worker's still-live sessions once its reconnect grace has
    /// elapsed with no newer connection (epoch unchanged) and it is still
    /// offline. A reconnect bumps the epoch and cancels this.
    ///
    /// A session the auto-resume authority still owns is exempt: an offline
    /// worker is not evidence that it ended, and the reconnect path resumes
    /// it. Ending one would stamp it as finished, which drops it from the
    /// snapshot's recent-session window and hides work that is coming back.
    pub fn fail_worker_if_still_gone(&self, worker_id: u64, epoch: u64) {
        if self.worker_is_online(worker_id) {
            return;
        }
        if self.worker_epochs.lock().unwrap().get(&worker_id) != Some(&epoch) {
            return;
        }
        let _guard = self.state_lock.lock().unwrap();
        let awaiting = self
            .storage
            .auto_resume_sessions_on_worker(worker_id)
            .unwrap_or_default()
            .into_iter()
            .map(|session| session.id)
            .collect::<std::collections::HashSet<_>>();
        if let Ok(ids) = self.storage.live_sessions_on_worker(worker_id) {
            for id in ids {
                if awaiting.contains(&id) {
                    continue;
                }
                if let Ok(updated) = self.storage.set_session_ended(
                    id,
                    SessionState::Failed,
                    "worker did not reconnect",
                    None,
                    now_unix_ms(),
                ) {
                    if let Ok(t) = self.storage.agent_terminal(id) {
                        self.forget_terminal_worker(t.id);
                    }
                    self.publish(Event::SessionChanged(updated));
                }
            }
        }
    }

    /// Whether a worker has standing to speak about a session, which it has
    /// only for the sessions dispatched to it. Session ids are small
    /// sequential integers, so one compromised host could otherwise walk
    /// `1..N` and rewrite the state of every session on the controller,
    /// including local ones and other hosts'.
    fn speaks_for_session(&self, worker_id: u64, session_id: u64, msg: &str) -> bool {
        let owner = self
            .storage
            .get_session(session_id)
            .map(|session| session.worker_id)
            .ok();
        if owner == Some(worker_id) {
            return true;
        }
        warn!(
            worker = worker_id,
            session = session_id,
            owner = ?owner,
            msg,
            "ignoring a host's message about a session that is not on it"
        );
        false
    }

    /// The same for a message addressed to a terminal, resolved through the
    /// session that owns it.
    fn speaks_for_terminal(&self, worker_id: u64, terminal_id: u64, msg: &str) -> bool {
        match self.storage.get_terminal(terminal_id) {
            Ok(terminal) => self.speaks_for_session(worker_id, terminal.session_id, msg),
            Err(_) => false,
        }
    }

    /// Applies a control message relayed up from a remote worker.
    pub fn apply_worker_message(self: &std::sync::Arc<Self>, worker_id: u64, msg: WorkerMsg) {
        match msg {
            // Registration is only meaningful as the first frame.
            WorkerMsg::Register { .. } => {}
            WorkerMsg::Heartbeat => {
                let _ = self.storage.touch_worker(worker_id, now_unix_ms());
            }
            WorkerMsg::SessionState {
                session_id,
                state,
                detail,
            } => {
                if !self.speaks_for_session(worker_id, session_id, "SessionState") {
                    return;
                }
                let _guard = self.state_lock.lock().unwrap();
                if let Ok(updated) = self
                    .storage
                    .update_session_state(session_id, state, &detail)
                {
                    self.publish(Event::SessionChanged(updated));
                }
            }
            WorkerMsg::NeedsInput { session_id } => {
                if self.speaks_for_session(worker_id, session_id, "NeedsInput") {
                    self.handle_pty_needs_input(session_id);
                }
            }
            WorkerMsg::TerminalActivity {
                terminal_id,
                generation,
            } => {
                if self.speaks_for_terminal(worker_id, terminal_id, "TerminalActivity") {
                    self.handle_terminal_activity(terminal_id, generation);
                }
            }
            WorkerMsg::ProgramStatus {
                terminal_id,
                generation,
                reset,
                records,
                removed,
            } => {
                if self.speaks_for_terminal(worker_id, terminal_id, "ProgramStatus") {
                    self.handle_program_status(
                        terminal_id,
                        generation,
                        &crate::program_status::Changes {
                            reset,
                            records,
                            removed,
                        },
                    );
                }
            }
            WorkerMsg::SessionExit {
                session_id,
                exit_code,
            } => self.handle_remote_exit(worker_id, session_id, exit_code),
            WorkerMsg::TerminalExit {
                terminal_id,
                generation,
                exit_code,
                state,
                transcript_available,
                transcript_size,
                detail,
            } => {
                self.handle_remote_terminal_exit(
                    worker_id,
                    terminal_id,
                    generation,
                    exit_code,
                    state,
                    &detail,
                    false,
                );
                if transcript_available {
                    if let Some(link) = self.workers.get(worker_id) {
                        self.enqueue_worker_transcripts(
                            &link,
                            vec![pm_protocol::domain::WorkerTranscript {
                                terminal_id,
                                generation,
                                size: transcript_size,
                            }],
                        );
                    }
                }
            }
            WorkerMsg::HookReport {
                session_token,
                kind,
                detail,
                agent_session_id,
                transcript_path,
                req_id,
                background_work,
            } => {
                let nudge = match self.handle_hook_event(
                    &session_token,
                    kind,
                    &detail,
                    &agent_session_id,
                    &transcript_path,
                    background_work,
                ) {
                    Ok(nudge) => nudge.unwrap_or_default(),
                    Err(error) => {
                        warn!(
                            worker = worker_id,
                            kind = kind.as_str(),
                            %error,
                            "rejected a hook relayed from a worker"
                        );
                        String::new()
                    }
                };
                // A zero req_id is a worker that predates the reply and is
                // not waiting for one.
                if req_id != 0 {
                    if let Some(link) = self.workers.get(worker_id) {
                        let _ = link.send(ControllerMsg::HookResult { req_id, nudge });
                    }
                }
                if !agent_session_id.is_empty() {
                    if let Ok(Some(session_id)) =
                        self.storage.get_session_id_by_token(&session_token)
                    {
                        let _ = self.storage.set_agent_resumable(session_id, true);
                        if let Ok(session) = self.storage.get_session(session_id) {
                            self.publish(Event::SessionChanged(session));
                        }
                    }
                }
            }
            WorkerMsg::RepoResponse {
                req_id,
                ok,
                error,
                answer,
            } => {
                if let Some(link) = self.workers.get(worker_id) {
                    link.resolve_repo(
                        req_id,
                        crate::workers::RepoResponseResult { ok, error, answer },
                    );
                }
            }
            WorkerMsg::HarnessStatus { req_id, status } => {
                if let Ok(link) = self.worker_link(worker_id) {
                    link.resolve_harness(req_id, status);
                }
            }
            WorkerMsg::PathChecked {
                req_id,
                status,
                detail,
            } => {
                if let Some(link) = self.workers.get(worker_id) {
                    link.resolve_path_check(
                        req_id,
                        crate::workers::PathCheckResult { status, detail },
                    );
                }
            }
            WorkerMsg::AgentInboxResult {
                req_id,
                outcome,
                transport,
                mode,
                detail,
            } => {
                if let Some(link) = self.workers.get(worker_id) {
                    link.resolve_agent_inbox(
                        req_id,
                        crate::workers::AgentInboxResult {
                            outcome,
                            transport,
                            mode,
                            detail,
                        },
                    );
                }
            }
            WorkerMsg::FsListing {
                req_id,
                ok,
                error,
                dir,
                parent,
                entries,
            } => {
                if let Some(link) = self.workers.get(worker_id) {
                    link.resolve_fs(
                        req_id,
                        crate::workers::FsListingResult {
                            ok,
                            error,
                            dir,
                            parent,
                            entries,
                        },
                    );
                }
            }
            // A host that cannot reach the browser plane relays its agents'
            // reports here instead. The answer goes back down the same link.
            WorkerMsg::McpRequest {
                req_id,
                bearer,
                body,
            } => {
                let Some(link) = self.workers.get(worker_id) else {
                    return;
                };
                let Ok(request) = serde_json::from_str::<serde_json::Value>(&body) else {
                    let _ = link.send(ControllerMsg::McpResponse {
                        req_id,
                        status: 400,
                        body: String::new(),
                    });
                    return;
                };
                let daemon = self.clone();
                tokio::spawn(async move {
                    let outcome = crate::mcp::dispatch(&daemon, &bearer, request).await;
                    let _ = link.send(ControllerMsg::McpResponse {
                        req_id,
                        status: u32::from(outcome.status),
                        body: outcome
                            .body
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                    });
                });
            }
            WorkerMsg::FileRead {
                req_id,
                ok,
                error,
                content,
                filename,
            } => {
                if let Some(link) = self.workers.get(worker_id) {
                    link.resolve_file(
                        req_id,
                        crate::workers::FileReadResult {
                            ok,
                            error,
                            content,
                            filename,
                        },
                    );
                }
            }
            WorkerMsg::ForwardOpened { req_id, ok, error } => {
                if let Some(link) = self.workers.get(worker_id) {
                    link.resolve_forward_open(
                        req_id,
                        crate::workers::ForwardOpenResult { ok, error },
                    );
                }
            }
            WorkerMsg::DirShareBound {
                share_id,
                ok,
                error,
                port,
            } => {
                if let Some(link) = self.workers.get(worker_id) {
                    link.resolve_dir_share_bound(
                        share_id,
                        crate::workers::DirShareBoundResult { ok, error, port },
                    );
                }
            }
        }
    }

    pub async fn harness_status(
        &self,
        project_id: u64,
        worker_id: u64,
        agent: Option<AgentKind>,
        install: bool,
    ) -> Result<(AgentKind, pm_protocol::domain::HarnessStatus), DaemonError> {
        let project = self.storage.get_project(project_id)?;
        self.resolve_spawn_host(project_id, &worker_id.to_string())?;
        let (agent, _) = self.resolve_agent(&project, agent)?;
        if worker_id == LOCAL_WORKER_ID {
            if !self.local_worker_enabled() {
                return Err(DaemonError::Rejected(
                    "the local worker is disabled on this daemon".into(),
                ));
            }
            return Ok((agent, crate::harness_install::request(agent, install)));
        }
        let link = self.worker_link(worker_id)?;
        if link.protocol_version() < pm_protocol::WORKER_PROTOCOL_HARNESS_INSTALL {
            if install {
                return Err(DaemonError::Rejected(
                    "update this worker to install a harness from the dashboard".into(),
                ));
            }
            return Ok((
                agent,
                pm_protocol::domain::HarnessStatus {
                    state: "unsupported".into(),
                    command: String::new(),
                    output: String::new(),
                    error: String::new(),
                },
            ));
        }
        let rx = link.request_harness(agent, install)?;
        match tokio::time::timeout(FS_LIST_TIMEOUT, rx).await {
            Ok(Ok(status)) => Ok((agent, status)),
            _ => Err(DaemonError::Rejected(
                "worker did not answer the harness installation check".into(),
            )),
        }
    }

    pub async fn repo_op(
        &self,
        worker_id: u64,
        op: &crate::review_repo::RepoOp,
    ) -> Result<crate::review_repo::RepoAnswer, DaemonError> {
        let encoded = op.encode();
        let answer = if worker_id == pm_protocol::domain::LOCAL_WORKER_ID {
            if !self.local_worker_enabled() {
                return Err(DaemonError::Rejected(
                    "the local worker is disabled on this daemon".into(),
                ));
            }
            crate::review_repo::handle_encoded(&encoded).map_err(DaemonError::Rejected)?
        } else {
            let link = self.worker_link(worker_id)?;
            // A worker that predates the reader would never understand
            // the message, so say so rather than wait out the timeout.
            if link.protocol_version() < pm_protocol::WORKER_PROTOCOL_REVIEW_REPO {
                return Err(DaemonError::Rejected(format!(
                    "this worker runs an older Puppet Master that cannot serve reviews; \
                     update it to a build speaking worker protocol {} or newer",
                    pm_protocol::WORKER_PROTOCOL_REVIEW_REPO
                )));
            }
            let rx = link.request_repo(encoded)?;
            match tokio::time::timeout(REPO_OP_TIMEOUT, rx).await {
                Ok(Ok(result)) if result.ok => result.answer,
                Ok(Ok(result)) => return Err(DaemonError::Rejected(result.error)),
                _ => {
                    return Err(DaemonError::Rejected(
                        "worker did not answer the repository read".into(),
                    ))
                }
            }
        };
        crate::review_repo::RepoAnswer::decode(&answer).map_err(DaemonError::Rejected)
    }

    pub async fn worker_fs_list(
        &self,
        worker_id: u64,
        path: String,
    ) -> Result<crate::workers::FsListingResult, DaemonError> {
        let rx = self.worker_link(worker_id)?.request_fs(path)?;
        match tokio::time::timeout(FS_LIST_TIMEOUT, rx).await {
            Ok(Ok(result)) => Ok(result),
            _ => Err(DaemonError::Rejected(
                "worker did not answer the directory listing".into(),
            )),
        }
    }

    pub async fn worker_file_read(
        &self,
        worker_id: u64,
        root: String,
        path: String,
        max_bytes: u64,
    ) -> Result<crate::workers::FileReadResult, DaemonError> {
        let rx = self
            .worker_link(worker_id)?
            .request_file(root, path, max_bytes)?;
        match tokio::time::timeout(FS_LIST_TIMEOUT, rx).await {
            Ok(Ok(result)) => Ok(result),
            _ => Err(DaemonError::Rejected(
                "worker did not answer the attachment read".into(),
            )),
        }
    }

    fn handle_remote_exit(&self, worker_id: u64, session_id: u64, exit_code: Option<i32>) {
        let Ok(terminal) = self.storage.agent_terminal(session_id) else {
            return;
        };
        self.handle_remote_terminal_exit(
            worker_id,
            terminal.id,
            terminal.generation,
            exit_code,
            pm_protocol::domain::TerminalRunState::Exited,
            "",
            false,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_remote_terminal_exit(
        &self,
        worker_id: u64,
        terminal_id: u64,
        generation: u64,
        exit_code: Option<i32>,
        state: pm_protocol::domain::TerminalRunState,
        detail: &str,
        transcript_available: bool,
    ) {
        let Ok(terminal) = self.storage.get_terminal(terminal_id) else {
            return;
        };
        if self
            .storage
            .get_session(terminal.session_id)
            .map(|s| s.worker_id)
            .ok()
            != Some(worker_id)
            || terminal.generation != generation
        {
            return;
        }
        // A worker answers a kill for a terminal it no longer has with an
        // exit, which can trail the real one and must not overwrite it.
        if !terminal.state.is_live() {
            return;
        }
        if terminal.kind == pm_protocol::domain::TerminalKind::Agent {
            self.program_status_exited(terminal.session_id);
        }
        let updated_terminal = match self.storage.update_terminal_run(
            terminal_id,
            generation,
            state,
            exit_code,
            transcript_available,
            now_unix_ms(),
        ) {
            Ok(t) => t,
            Err(e) => {
                error!(terminal = terminal_id, error = %e, "failed to persist terminal exit");
                return;
            }
        };
        let _ = self
            .storage
            .set_terminal_desired_running(terminal_id, false);
        if let Some(link) = self.workers.get(worker_id) {
            link.remove_session(terminal_id);
        }
        if terminal.kind != pm_protocol::domain::TerminalKind::Agent {
            if state == pm_protocol::domain::TerminalRunState::Exited && exit_code == Some(0) {
                if let Err(error) = self.remove_shell_terminal(terminal_id) {
                    error!(terminal = terminal_id, error = %error, "failed to remove exited remote shell");
                }
            } else {
                self.publish(Event::TerminalChanged(updated_terminal));
                self.forget_terminal_worker(terminal_id);
            }
            return;
        }
        self.publish(Event::TerminalChanged(updated_terminal));
        self.forget_terminal_worker(terminal_id);
        let _ = self
            .storage
            .set_session_desired_running(terminal.session_id, false);
        let _guard = self.state_lock.lock().unwrap();
        let failed = state == pm_protocol::domain::TerminalRunState::Failed;
        match self.storage.set_session_ended(
            terminal.session_id,
            if failed {
                SessionState::Failed
            } else {
                SessionState::Exited
            },
            remote_exit_detail(failed, detail),
            exit_code,
            now_unix_ms(),
        ) {
            Ok(updated) => {
                info!(session = terminal.session_id, code = ?exit_code, "remote session exited");
                self.publish(Event::SessionChanged(updated));
            }
            Err(e) => {
                error!(session = terminal.session_id, error = %e, "failed to persist remote exit")
            }
        }
    }

    /// Scope filter for outgoing events, resolved against current
    /// storage so bucket/project scopes follow membership.
    pub fn event_in_scope(&self, event: &Event, scope: Scope) -> bool {
        match scope {
            Scope::All => true,
            Scope::Session(sid) => match event {
                Event::SessionChanged(s) => s.id == sid,
                Event::SessionRemoved(id) => *id == sid,
                // Worker presence is global, delivered to every scope.
                Event::WorkerChanged(_) | Event::WorkerRemoved(_) => true,
                Event::TerminalChanged(t) => t.session_id == sid,
                Event::TerminalRemoved(_) => true,
                Event::ContextChanged(c) => c.session_id == sid,
                Event::ModelProfileChanged(_) | Event::ModelProfileRemoved(_) => true,
                Event::PlanChanged(plan) => plan.owning_session_id == sid,
                Event::PlanRemoved(_) => true,
                Event::SessionAlert(a) => a.session_id == sid,
                // Not about a session, and the point of it is that the user is
                // told, so a narrow scope must not be what hides it.
                Event::SecurityNotice(_) => true,
                _ => false,
            },
            Scope::Project(pid) => match event {
                Event::SessionChanged(s) => s.project_id == pid,
                Event::ProjectChanged(p) => p.id == pid,
                Event::ProjectRemoved(id) => *id == pid,
                Event::WorkerChanged(_) | Event::WorkerRemoved(_) => true,
                Event::TerminalChanged(t) => self
                    .storage
                    .get_session(t.session_id)
                    .map(|s| s.project_id == pid)
                    .unwrap_or(false),
                Event::TerminalRemoved(_) => true,
                Event::ContextChanged(c) => self
                    .storage
                    .get_session(c.session_id)
                    .map(|s| s.project_id == pid)
                    .unwrap_or(false),
                Event::ForwardChanged(f) => self
                    .storage
                    .get_session(f.session_id)
                    .map(|s| s.project_id == pid)
                    .unwrap_or(false),
                Event::ForwardRemoved(_) => true,
                Event::ItemChanged(i) => i.project_id == Some(pid),
                Event::ItemRemoved(_) => true,
                Event::InstructionLayerChanged(layer) => layer.project_id == Some(pid),
                Event::ModelProfileChanged(_) | Event::ModelProfileRemoved(_) => true,
                Event::ReviewChanged(r) => r.project_id == pid,
                Event::ReviewRemoved(_) => true,
                Event::PlanChanged(plan) => plan.project_id == pid,
                Event::PlanRemoved(_) => true,
                Event::SessionAlert(a) => self
                    .storage
                    .get_session(a.session_id)
                    .map(|s| s.project_id == pid)
                    .unwrap_or(false),
                Event::SecurityNotice(_) => true,
                _ => false,
            },
            Scope::Bucket(bid) => match event {
                Event::ReviewChanged(r) => self
                    .storage
                    .get_project(r.project_id)
                    .map(|p| p.bucket_id == bid)
                    .unwrap_or(false),
                Event::ReviewRemoved(_) => true,
                Event::BucketChanged(b) => b.id == bid,
                Event::BucketRemoved(id) => *id == bid,
                Event::ProjectChanged(p) => p.bucket_id == bid,
                Event::ProjectRemoved(_) => true,
                Event::SessionChanged(s) => self
                    .storage
                    .get_project(s.project_id)
                    .map(|p| p.bucket_id == bid)
                    .unwrap_or(false),
                Event::SessionRemoved(_) => true,
                Event::WorkerChanged(_) | Event::WorkerRemoved(_) => true,
                Event::TerminalChanged(t) => self
                    .storage
                    .get_session(t.session_id)
                    .map(|s| {
                        self.storage
                            .get_project(s.project_id)
                            .map(|p| p.bucket_id == bid)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false),
                Event::TerminalRemoved(_) => true,
                Event::ContextChanged(c) => self
                    .storage
                    .get_session(c.session_id)
                    .map(|s| {
                        self.storage
                            .get_project(s.project_id)
                            .map(|p| p.bucket_id == bid)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false),
                Event::ForwardChanged(f) => self
                    .storage
                    .get_session(f.session_id)
                    .map(|s| {
                        self.storage
                            .get_project(s.project_id)
                            .map(|p| p.bucket_id == bid)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false),
                Event::ForwardRemoved(_) => true,
                Event::ItemChanged(i) => i.bucket_id == bid,
                Event::ItemRemoved(_) => true,
                Event::BriefingChanged(b) => b.bucket_id == bid,
                Event::UserSettingChanged(_) => false,
                Event::InstructionLayerChanged(layer) => layer.bucket_id == bid,
                // Profiles are controller-global, so every scope sees them.
                Event::ModelProfileChanged(_) | Event::ModelProfileRemoved(_) => true,
                Event::PlanChanged(plan) => plan.bucket_id == bid,
                Event::PlanRemoved(_) => true,
                Event::SessionAlert(a) => self
                    .storage
                    .get_session(a.session_id)
                    .and_then(|s| self.storage.get_project(s.project_id))
                    .map(|p| p.bucket_id == bid)
                    .unwrap_or(false),
                // About the controller rather than any one bucket, and the
                // point of it is that the user is told, so no scope filters it
                // out.
                Event::SecurityNotice(_) => true,
            },
        }
    }
}

/// Accepts connections for one published forward. Holds only a weak
/// daemon handle so an unbound listener cannot keep the daemon alive.
async fn run_forward_listener(
    weak: std::sync::Weak<Daemon>,
    listener: tokio::net::TcpListener,
    forward_id: u64,
    worker_id: u64,
    worker_port: u16,
    scheme: String,
) {
    let http_forward = crate::forward_proxy::is_http_scheme(&scheme);
    if !http_forward {
        let bound = listener.local_addr().ok();
        let off_host = bound.is_some_and(|addr| !addr.ip().is_loopback());
        warn!(
            forward = forward_id,
            scheme = %scheme,
            addr = ?bound,
            "non-HTTP forward is unauthenticated — anyone who can reach the \
             listener reaches the agent's server"
        );
        if off_host {
            warn!(
                forward = forward_id,
                addr = ?bound,
                "and --forward-bind put that listener on an address other than \
                 loopback, so it is reachable from off this host. The agent chose \
                 the port it forwards to and does not have to own it"
            );
        }
    }
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            break;
        };
        let Some(daemon) = weak.upgrade() else {
            break;
        };
        tokio::spawn(handle_forward_stream(
            daemon,
            forward_id,
            worker_id,
            worker_port,
            tcp,
            http_forward,
        ));
    }
}

/// Authenticates and connects one accepted connection to the
/// worker-side target port: directly for the local worker, via a
/// token-authenticated dial-back stream for a remote one.
///
/// For HTTP-family forwards the first request's headers are read and
/// the caller is checked against the dashboard session cookie, a
/// mobile bearer token, or a scoped forward token. Non-HTTP forwards
/// skip auth (with a warning logged at listener start).
async fn handle_forward_stream(
    daemon: std::sync::Arc<Daemon>,
    forward_id: u64,
    worker_id: u64,
    worker_port: u16,
    mut tcp: tokio::net::TcpStream,
    http_forward: bool,
) {
    use crate::forward_proxy::{authenticate_forward, ForwardAuthResult, ForwardStream};

    let fwd = if http_forward {
        match authenticate_forward(&mut tcp, &daemon, forward_id).await {
            ForwardAuthResult::Authenticated {
                header_bytes,
                username: _,
            } => ForwardStream::new(header_bytes, tcp),
            ForwardAuthResult::Rejected | ForwardAuthResult::Handled => return,
            ForwardAuthResult::Error(e) => {
                debug!(forward = forward_id, error = %e, "forward auth read error");
                return;
            }
        }
    } else {
        ForwardStream::plain(tcp)
    };

    if worker_id == LOCAL_WORKER_ID {
        match tokio::net::TcpStream::connect(("127.0.0.1", worker_port)).await {
            Ok(mut target) => {
                daemon.record_forward_target_reachable(forward_id, true);
                let mut fwd = fwd;
                let _ = tokio::io::copy_bidirectional(&mut fwd, &mut target).await;
            }
            Err(e) => {
                debug!(forward = forward_id, error = %e, "local forward target refused");
                daemon.record_forward_target_reachable(forward_id, false);
            }
        }
        return;
    }

    let Ok(link) = daemon.worker_link(worker_id) else {
        debug!(
            forward = forward_id,
            worker = worker_id,
            "forward connection while worker offline"
        );
        return;
    };
    let token = crate::auth::generate_token();
    let token_hash = crate::auth::hash_token(&token);
    // Park before asking, so the worker's dial-back cannot race the
    // registration.
    daemon.forwards.park_stream(token_hash.clone(), fwd);
    let rx = match link.request_forward_open(worker_port, token) {
        Ok(rx) => rx,
        Err(_) => {
            daemon.forwards.claim_stream(&token_hash);
            return;
        }
    };
    match tokio::time::timeout(crate::forward::STREAM_DIAL_TIMEOUT, rx).await {
        Ok(Ok(result)) if result.ok => daemon.record_forward_target_reachable(forward_id, true),
        Ok(Ok(result)) => {
            debug!(forward = forward_id, error = %result.error, "worker forward dial failed");
            daemon.record_forward_target_reachable(forward_id, false);
            daemon.forwards.claim_stream(&token_hash);
            return;
        }
        _ => {
            daemon.forwards.claim_stream(&token_hash);
            return;
        }
    }
    // The worker reported a successful local dial; if it never dials
    // the stream endpoint, sweep the parked connection once the window
    // passes (a served stream was already claimed, so this is a no-op).
    tokio::time::sleep(crate::forward::STREAM_DIAL_TIMEOUT).await;
    daemon.forwards.claim_stream(&token_hash);
}

#[cfg(test)]
mod web_terminal_stream_tests {
    use super::*;

    #[test]
    fn input_only_acknowledges_the_state_observed_before_delivery() {
        assert!(user_input_acknowledges_state(
            SessionState::NeedsInput,
            SessionState::NeedsInput,
            true,
            true,
        ));
        assert!(!user_input_acknowledges_state(
            SessionState::Working,
            SessionState::NeedsInput,
            true,
            true,
        ));
        assert!(user_input_acknowledges_state(
            SessionState::Idle,
            SessionState::Idle,
            true,
            false,
        ));
        assert!(!user_input_acknowledges_state(
            SessionState::Idle,
            SessionState::Idle,
            true,
            true,
        ));
    }

    #[test]
    fn supervisor_submit_normalizes_only_trailing_line_endings() {
        for text in ["command", "command\r", "command\n", "command\r\n\r"] {
            assert_eq!(
                supervisor_input_plan(false, text, true).transport,
                bytes::Bytes::from_static(b"command\r")
            );
        }
        assert_eq!(
            supervisor_input_plan(false, "", true).transport,
            bytes::Bytes::from_static(b"\r")
        );
        assert_eq!(
            supervisor_input_plan(false, "echo first\necho second\r\n", true).transport,
            bytes::Bytes::from_static(b"echo first\necho second\r")
        );
        assert_eq!(
            supervisor_input_plan(false, "héllo", true).transport,
            bytes::Bytes::copy_from_slice("héllo\r".as_bytes())
        );
    }

    #[test]
    fn supervisor_legacy_input_is_byte_for_byte() {
        for text in ["", "command", "command\r", "command\n", "a\r\nb", "héllo"] {
            assert_eq!(
                supervisor_input_plan(false, text, false).transport,
                bytes::Bytes::copy_from_slice(text.as_bytes())
            );
        }
    }

    #[test]
    fn agent_submit_defers_enter_until_after_the_paste_boundary() {
        let plan = supervisor_input_plan(true, "long\nmessage\r\n", true);
        assert_eq!(
            plan.transport,
            bytes::Bytes::from_static(b"\x1b[200~long\nmessage\x1b[201~")
        );
        assert_eq!(plan.deferred_submit, Some(bytes::Bytes::from_static(b"\r")));
        assert_eq!(plan.logical_bytes, 13);
        assert!(plan.submission_requested);
    }

    /// Every managed agent CLI runs a TUI that enables bracketed paste and
    /// folds a following Enter back into the paste, so all of them need the
    /// deferred submit; only the scripted test agent takes the plain path.
    #[test]
    fn every_real_agent_submits_through_a_paste_boundary() {
        let registry = pm_adapters::AdapterRegistry::standard();
        for kind in registry.kinds() {
            assert!(
                registry.get(kind).unwrap().submits_after_bracketed_paste(),
                "{} must submit through a paste boundary",
                kind.as_str()
            );
        }
        let test_agent: Box<dyn pm_adapters::AgentAdapter> =
            Box::new(pm_adapters::TestAgentAdapter {
                program: std::path::PathBuf::from("/bin/true"),
            });
        assert!(!test_agent.submits_after_bracketed_paste());
    }

    #[test]
    fn plans_without_a_paste_carry_no_deferred_enter() {
        assert!(supervisor_input_plan(false, "command", true)
            .deferred_submit
            .is_none());
        assert!(supervisor_input_plan(true, "", true)
            .deferred_submit
            .is_none());
        assert!(supervisor_input_plan(true, "raw\nlines\n", false)
            .deferred_submit
            .is_none());
        assert_eq!(
            supervisor_input_plan(true, "", true).transport,
            bytes::Bytes::from_static(b"\r")
        );
    }

    #[test]
    fn browser_terminal_streams_are_capped_per_user_and_released() {
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward: Default::default(),
            db_path: None,
            socket_path: temp.path().join("unused.sock"),
            http_addr: None,
            http_tls: None,
            worker_addr: None,
            public_url: None,
            scrollback_dir: temp.path().join("scrollback"),
            registry: AdapterRegistry::empty(),
            local_worker_enabled: true,
            release_channel: None,
        };
        let daemon = Daemon::new(config).unwrap().0;
        for _ in 0..MAX_WEB_TERMINAL_STREAMS_PER_USER {
            assert!(daemon.acquire_web_terminal_stream(7));
        }
        assert!(!daemon.acquire_web_terminal_stream(7));
        daemon.release_web_terminal_stream(7);
        assert!(daemon.acquire_web_terminal_stream(7));
        assert!(daemon.acquire_web_terminal_stream(8));
    }

    fn native_theme(name: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "kind": "puppet-master-terminal-theme", "version": 1, "name": name,
            "colors": {
                "foreground": "#c9ceda", "background": "#0b0e14",
                "cursor": "#ffb224", "cursorAccent": "#0b0e14",
                "selectionForeground": "#eef1f6", "selectionBackground": "#2b3548",
                "black": "#2e3436", "red": "#cc0000", "green": "#4e9a06",
                "yellow": "#c4a000", "blue": "#3465a4", "magenta": "#75507b",
                "cyan": "#06989a", "white": "#d3d7cf", "brightBlack": "#555753",
                "brightRed": "#ef2929", "brightGreen": "#8ae234",
                "brightYellow": "#fce94f", "brightBlue": "#729fcf",
                "brightMagenta": "#ad7fa8", "brightCyan": "#34e2e2",
                "brightWhite": "#eeeeec"
            }
        }))
        .unwrap()
    }

    #[test]
    fn user_theme_streams_are_scoped_and_snapshots_are_atomic() {
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward: Default::default(),
            db_path: None,
            socket_path: temp.path().join("unused.sock"),
            http_addr: None,
            http_tls: None,
            worker_addr: None,
            public_url: None,
            scrollback_dir: temp.path().join("scrollback"),
            registry: AdapterRegistry::empty(),
            local_worker_enabled: true,
            release_channel: None,
        };
        let daemon = Daemon::new(config).unwrap().0;
        let first = daemon.storage.create_user("first", "h", 1).unwrap();
        let second = daemon.storage.create_user("second", "h", 1).unwrap();
        let (snapshot, _events, mut first_updates) = daemon.subscribe_for_user(first);
        let (other_snapshot, _events, mut second_updates) = daemon.subscribe_for_user(second);
        assert!(snapshot.user_settings.is_empty());
        assert!(other_snapshot.user_settings.is_empty());

        daemon
            .set_user_terminal_theme(first, Some(&native_theme("First")))
            .unwrap();
        assert!(first_updates.try_recv().unwrap().value_json.is_some());
        assert!(matches!(
            second_updates.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(
            daemon.subscribe_for_user(first).0.user_settings[0].key,
            crate::terminal_theme::USER_TERMINAL_THEME_KEY
        );
        assert!(daemon.subscribe_for_user(second).0.user_settings.is_empty());
    }

    #[test]
    fn corrupt_stored_theme_falls_back_without_being_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward: Default::default(),
            db_path: None,
            socket_path: temp.path().join("unused.sock"),
            http_addr: None,
            http_tls: None,
            worker_addr: None,
            public_url: None,
            scrollback_dir: temp.path().join("scrollback"),
            registry: AdapterRegistry::empty(),
            local_worker_enabled: true,
            release_channel: None,
        };
        let daemon = Daemon::new(config).unwrap().0;
        let user = daemon.storage.create_user("first", "h", 1).unwrap();
        daemon
            .storage
            .set_user_setting(
                user,
                crate::terminal_theme::USER_TERMINAL_THEME_KEY,
                Some("broken"),
            )
            .unwrap();
        assert!(daemon.subscribe_for_user(user).0.user_settings.is_empty());
        assert_eq!(
            daemon
                .storage
                .get_user_setting(user, crate::terminal_theme::USER_TERMINAL_THEME_KEY)
                .unwrap()
                .as_deref(),
            Some("broken")
        );
    }
}

#[cfg(test)]
mod agent_resolution_tests {
    use super::*;

    fn daemon() -> (Daemon, tempfile::TempDir, u64, u64) {
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward: Default::default(),
            db_path: None,
            socket_path: temp.path().join("unused.sock"),
            http_addr: None,
            http_tls: None,
            worker_addr: None,
            public_url: None,
            scrollback_dir: temp.path().join("scrollback"),
            registry: AdapterRegistry::standard(),
            local_worker_enabled: true,
            release_channel: None,
        };
        let daemon = Daemon::new(config).unwrap().0;
        let bucket = daemon.create_bucket("work").unwrap();
        let project = daemon
            .create_project(bucket, "api", temp.path().to_str().unwrap())
            .unwrap();
        (daemon, temp, bucket, project)
    }

    #[test]
    fn agent_precedence_is_explicit_project_bucket_then_fallback() {
        let (daemon, _temp, bucket_id, project_id) = daemon();
        daemon
            .storage
            .set_bucket_default_agent(bucket_id, Some(AgentKind::Codex))
            .unwrap();
        daemon
            .set_project_default_agent(project_id, Some(AgentKind::ClaudeCode))
            .unwrap();

        let project = daemon.storage.get_project(project_id).unwrap();
        assert_eq!(
            daemon
                .resolve_agent(&project, Some(AgentKind::Codex))
                .unwrap(),
            (AgentKind::Codex, AgentSelectionSource::Explicit)
        );
        assert_eq!(
            daemon.resolve_agent(&project, None).unwrap(),
            (AgentKind::ClaudeCode, AgentSelectionSource::Project)
        );

        daemon.set_project_default_agent(project_id, None).unwrap();
        let project = daemon.storage.get_project(project_id).unwrap();
        assert_eq!(
            daemon.resolve_agent(&project, None).unwrap(),
            (AgentKind::Codex, AgentSelectionSource::Bucket)
        );

        daemon.set_bucket_default_agent(bucket_id, None).unwrap();
        assert_eq!(
            daemon.resolve_agent(&project, None).unwrap(),
            (AgentKind::ClaudeCode, AgentSelectionSource::Fallback)
        );
    }

    fn profile_with(daemon: &Daemon, name: &str, entries: &[(ModelDialect, &str, &str)]) -> u64 {
        let id = daemon
            .create_model_profile(name, Some("sk-secret-value"))
            .unwrap();
        for (dialect, model, base_url) in entries {
            daemon
                .set_model_profile_endpoint(id, *dialect, model, base_url, "gw/small")
                .unwrap();
        }
        id
    }

    #[test]
    fn model_profile_precedence_is_explicit_project_then_bucket() {
        let (daemon, _temp, bucket_id, project_id) = daemon();
        let bucket_profile = profile_with(
            &daemon,
            "bucket gateway",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/big",
                "https://gw.example/v1",
            )],
        );
        let project_profile = profile_with(
            &daemon,
            "project gateway",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/other",
                "https://gw.example/v1",
            )],
        );

        let project = daemon.storage.get_project(project_id).unwrap();
        assert_eq!(daemon.resolve_model_profile(&project, None).unwrap(), None);

        daemon
            .set_bucket_model_profile(bucket_id, Some(bucket_profile))
            .unwrap();
        let project = daemon.storage.get_project(project_id).unwrap();
        assert_eq!(
            daemon.resolve_model_profile(&project, None).unwrap(),
            Some((bucket_profile, ModelProfileSource::Bucket))
        );

        daemon
            .set_project_model_profile(project_id, Some(project_profile))
            .unwrap();
        let project = daemon.storage.get_project(project_id).unwrap();
        assert_eq!(
            daemon.resolve_model_profile(&project, None).unwrap(),
            Some((project_profile, ModelProfileSource::Project))
        );
        assert_eq!(
            daemon
                .resolve_model_profile(&project, Some(bucket_profile))
                .unwrap(),
            Some((bucket_profile, ModelProfileSource::Explicit))
        );
    }

    /// Switching the resolved agent must keep a two-dialect profile
    /// working, selecting the other entry rather than rejecting.
    #[test]
    fn switching_agents_selects_the_other_entry() {
        let (daemon, _temp, _bucket_id, _project_id) = daemon();
        let id = profile_with(
            &daemon,
            "both",
            &[
                (
                    ModelDialect::AnthropicMessages,
                    "gw/claude",
                    "https://gw.example/v1",
                ),
                (
                    ModelDialect::OpenaiResponses,
                    "gw/codex",
                    "https://gw.example/openai",
                ),
            ],
        );
        let claude = daemon
            .resolve_model_endpoint(Some(id), AgentKind::ClaudeCode)
            .unwrap()
            .unwrap();
        assert_eq!(claude.model, "gw/claude");
        assert_eq!(claude.provider_name, "both");
        assert_eq!(claude.api_key, "sk-secret-value");
        let codex = daemon
            .resolve_model_endpoint(Some(id), AgentKind::Codex)
            .unwrap()
            .unwrap();
        assert_eq!(codex.model, "gw/codex");
    }

    /// An inherited profile must not strand an agent whose CLI can
    /// apply no endpoint at all: no entry could ever serve it, so the
    /// spawn runs on the agent's own account instead of being rejected.
    #[test]
    fn an_agent_that_applies_no_endpoint_ignores_an_inherited_profile() {
        let (daemon, _temp, _bucket_id, _project_id) = daemon();
        let id = profile_with(
            &daemon,
            "claude only",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/claude",
                "https://gw.example/v1",
            )],
        );
        assert!(daemon
            .resolve_model_endpoint(Some(id), AgentKind::Antigravity)
            .unwrap()
            .is_none());
    }

    #[test]
    fn an_agent_with_no_matching_entry_is_rejected_naming_the_coverage() {
        let (daemon, _temp, _bucket_id, _project_id) = daemon();
        let id = profile_with(
            &daemon,
            "claude only",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/claude",
                "https://gw.example/v1",
            )],
        );
        let err = daemon
            .resolve_model_endpoint(Some(id), AgentKind::Codex)
            .unwrap_err()
            .to_string();
        assert!(err.contains("claude only"), "{err}");
        assert!(err.contains("codex"), "{err}");
        assert!(err.contains("openai-responses"), "{err}");
        assert!(err.contains("anthropic-messages"), "{err}");
    }

    #[test]
    fn attaching_a_profile_that_cannot_serve_the_resolved_agent_is_rejected() {
        let (daemon, _temp, bucket_id, project_id) = daemon();
        let id = profile_with(
            &daemon,
            "claude only",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/claude",
                "https://gw.example/v1",
            )],
        );
        daemon
            .storage
            .set_bucket_default_agent(bucket_id, Some(AgentKind::Codex))
            .unwrap();
        assert!(daemon
            .set_bucket_model_profile(bucket_id, Some(id))
            .is_err());
        assert!(daemon
            .set_project_model_profile(project_id, Some(id))
            .is_err());

        daemon.set_bucket_default_agent(bucket_id, None).unwrap();
        daemon
            .set_bucket_model_profile(bucket_id, Some(id))
            .unwrap();
    }

    #[test]
    fn a_referenced_profile_cannot_be_deleted_and_the_error_names_referents() {
        let (daemon, _temp, bucket_id, project_id) = daemon();
        let id = profile_with(
            &daemon,
            "gateway",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/big",
                "https://gw.example/v1",
            )],
        );
        daemon
            .set_bucket_model_profile(bucket_id, Some(id))
            .unwrap();
        let err = daemon.delete_model_profile(id).unwrap_err().to_string();
        assert!(err.contains("bucket \"work\""), "{err}");

        daemon.set_bucket_model_profile(bucket_id, None).unwrap();
        daemon
            .set_project_model_profile(project_id, Some(id))
            .unwrap();
        let err = daemon.delete_model_profile(id).unwrap_err().to_string();
        assert!(err.contains("project \"api\""), "{err}");

        daemon.set_project_model_profile(project_id, None).unwrap();
        daemon.delete_model_profile(id).unwrap();
    }

    #[test]
    fn an_entry_a_resumable_session_would_reselect_cannot_be_deleted() {
        let (daemon, _temp, _bucket_id, project_id) = daemon();
        let id = profile_with(
            &daemon,
            "gateway",
            &[
                (
                    ModelDialect::AnthropicMessages,
                    "gw/claude",
                    "https://gw.example/v1",
                ),
                (
                    ModelDialect::OpenaiResponses,
                    "gw/codex",
                    "https://gw.example/openai",
                ),
            ],
        );
        let session = daemon
            .storage
            .create_session_with_model_profile(
                project_id,
                AgentKind::ClaudeCode,
                AgentSelectionSource::Explicit,
                "t",
                "p",
                PermissionMode::Default,
                LOCAL_WORKER_ID,
                true,
                false,
                None,
                Some((id, ModelProfileSource::Bucket)),
                1,
            )
            .unwrap();

        let err = daemon
            .delete_model_profile_endpoint(id, ModelDialect::AnthropicMessages)
            .unwrap_err()
            .to_string();
        assert!(err.contains(&format!("session {}", session.id)), "{err}");
        assert!(err.contains("anthropic-messages"), "{err}");
        // The entry that session would not reselect stays deletable.
        daemon
            .delete_model_profile_endpoint(id, ModelDialect::OpenaiResponses)
            .unwrap();

        let err = daemon.delete_model_profile(id).unwrap_err().to_string();
        assert!(err.contains(&format!("session {}", session.id)), "{err}");
    }

    /// Only a session that could still resume through the profile holds
    /// it; finished history must not pin a profile forever.
    #[test]
    fn an_ended_session_does_not_block_deleting_its_profile() {
        let (daemon, _temp, _bucket_id, project_id) = daemon();
        let id = profile_with(
            &daemon,
            "gateway",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/big",
                "https://gw.example/v1",
            )],
        );
        let session = daemon
            .storage
            .create_session_with_model_profile(
                project_id,
                AgentKind::ClaudeCode,
                AgentSelectionSource::Explicit,
                "t",
                "p",
                PermissionMode::Default,
                LOCAL_WORKER_ID,
                true,
                false,
                None,
                Some((id, ModelProfileSource::Bucket)),
                1,
            )
            .unwrap();
        daemon
            .storage
            .set_session_ended(session.id, SessionState::Exited, "", Some(0), 2)
            .unwrap();
        daemon
            .storage
            .set_agent_resumable(session.id, false)
            .unwrap();

        daemon.delete_model_profile(id).unwrap();
        assert_eq!(
            daemon
                .storage
                .get_session(session.id)
                .unwrap()
                .model_profile_id,
            None
        );
    }

    /// The key is write-only: nothing returns it, and the row holds
    /// ciphertext rather than the value.
    #[test]
    fn a_stored_key_is_never_returned_and_never_stored_in_the_clear() {
        let (daemon, _temp, _bucket_id, _project_id) = daemon();
        let id = profile_with(
            &daemon,
            "gateway",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/big",
                "https://gw.example/v1",
            )],
        );
        let profile = daemon.storage.get_model_profile(id).unwrap();
        assert!(profile.key_set);
        let rendered = format!("{profile:?}");
        assert!(!rendered.contains("sk-secret-value"), "{rendered}");
        for listed in daemon.storage.list_model_profiles().unwrap() {
            assert!(!format!("{listed:?}").contains("sk-secret-value"));
        }
        let ciphertext = daemon
            .storage
            .model_profile_key_ciphertext(id)
            .unwrap()
            .unwrap();
        assert!(!ciphertext.contains("sk-secret-value"), "{ciphertext}");
        assert!(ciphertext.starts_with("v1:"), "{ciphertext}");
    }

    /// Editing a profile leaves the credential alone; replacing it is
    /// an explicit action.
    #[test]
    fn editing_a_profile_keeps_its_key_until_explicitly_changed() {
        let (daemon, _temp, _bucket_id, _project_id) = daemon();
        let id = profile_with(
            &daemon,
            "gateway",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/big",
                "https://gw.example/v1",
            )],
        );
        daemon
            .update_model_profile(id, Some("renamed"), None, false)
            .unwrap();
        daemon
            .set_model_profile_endpoint(
                id,
                ModelDialect::AnthropicMessages,
                "gw/bigger",
                "https://gw.example/v1",
                "",
            )
            .unwrap();
        let profile = daemon.storage.get_model_profile(id).unwrap();
        assert_eq!(profile.name, "renamed");
        assert!(profile.key_set);
        assert_eq!(
            daemon
                .resolve_model_endpoint(Some(id), AgentKind::ClaudeCode)
                .unwrap()
                .unwrap()
                .api_key,
            "sk-secret-value"
        );

        daemon
            .update_model_profile(id, None, Some("sk-rotated"), false)
            .unwrap();
        assert_eq!(
            daemon
                .resolve_model_endpoint(Some(id), AgentKind::ClaudeCode)
                .unwrap()
                .unwrap()
                .api_key,
            "sk-rotated"
        );

        daemon.update_model_profile(id, None, None, true).unwrap();
        assert!(!daemon.storage.get_model_profile(id).unwrap().key_set);
    }

    /// A session stores the profile id, so a resume reselects the entry
    /// and picks up edits made since it spawned.
    #[test]
    fn resume_reselects_the_entry_and_sees_edits() {
        let (daemon, _temp, _bucket_id, project_id) = daemon();
        let id = profile_with(
            &daemon,
            "gateway",
            &[(
                ModelDialect::AnthropicMessages,
                "gw/old",
                "https://old.example/v1",
            )],
        );
        let session = daemon
            .storage
            .create_session_with_model_profile(
                project_id,
                AgentKind::ClaudeCode,
                AgentSelectionSource::Explicit,
                "t",
                "p",
                PermissionMode::Default,
                LOCAL_WORKER_ID,
                true,
                false,
                None,
                Some((id, ModelProfileSource::Bucket)),
                1,
            )
            .unwrap();
        assert_eq!(session.model_profile_id, Some(id));
        assert_eq!(
            session.model_profile_source,
            Some(ModelProfileSource::Bucket)
        );

        daemon
            .set_model_profile_endpoint(
                id,
                ModelDialect::AnthropicMessages,
                "gw/new",
                "https://new.example/v1",
                "",
            )
            .unwrap();
        let stored = daemon.storage.get_session(session.id).unwrap();
        let endpoint = daemon
            .resolve_model_endpoint(stored.model_profile_id, stored.agent)
            .unwrap()
            .unwrap();
        assert_eq!(endpoint.model, "gw/new");
        assert_eq!(endpoint.base_url, "https://new.example/v1");
    }

    /// With no profile configured the spawn context is exactly what it
    /// was before profiles existed.
    #[test]
    fn no_profile_leaves_the_spawn_endpoint_unset() {
        let (daemon, _temp, _bucket_id, project_id) = daemon();
        let project = daemon.storage.get_project(project_id).unwrap();
        assert_eq!(daemon.resolve_model_profile(&project, None).unwrap(), None);
        assert_eq!(
            daemon
                .resolve_model_endpoint(None, AgentKind::ClaudeCode)
                .unwrap(),
            None
        );
    }

    #[test]
    fn published_dialects_come_from_the_registered_adapters() {
        let (daemon, _temp, _bucket_id, _project_id) = daemon();
        let published = daemon.agent_dialects();
        assert_eq!(
            published,
            vec![
                // Antigravity's endpoint override also needs a setting
                // in the user's own global file, so no profile entry
                // reaches it.
                AgentDialects {
                    agent: AgentKind::Antigravity,
                    dialects: vec![],
                    supports_background_model: false,
                },
                AgentDialects {
                    agent: AgentKind::ClaudeCode,
                    dialects: vec![ModelDialect::AnthropicMessages],
                    supports_background_model: true,
                },
                AgentDialects {
                    agent: AgentKind::Codex,
                    dialects: vec![ModelDialect::OpenaiResponses],
                    supports_background_model: false,
                },
                AgentDialects {
                    agent: AgentKind::Gemini,
                    dialects: vec![ModelDialect::GoogleGenai],
                    supports_background_model: false,
                },
                // OpenCode is bring-your-own-key across providers, so it
                // is the one agent a profile can reach through either of
                // the other two endpoint shapes.
                AgentDialects {
                    agent: AgentKind::OpenCode,
                    dialects: vec![
                        ModelDialect::AnthropicMessages,
                        ModelDialect::OpenaiResponses,
                    ],
                    supports_background_model: false,
                },
            ]
        );
    }

    #[test]
    fn unavailable_resolution_reports_agent_and_source() {
        let (mut daemon, _temp, bucket_id, project_id) = daemon();
        daemon.registry = AdapterRegistry::empty();
        daemon
            .storage
            .set_bucket_default_agent(bucket_id, Some(AgentKind::Codex))
            .unwrap();
        let project = daemon.storage.get_project(project_id).unwrap();
        assert!(matches!(
            daemon.resolve_agent(&project, None),
            Err(DaemonError::AgentUnavailable {
                agent: "codex",
                selection_source: "bucket"
            })
        ));
    }
}
