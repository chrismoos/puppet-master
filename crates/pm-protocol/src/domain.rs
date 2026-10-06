//! Domain types the daemon and clients program against. Wire types
//! (prost-generated) stay at the transport boundary; conversions live
//! in `convert`.

use bytes::Bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    Gemini,
    OpenCode,
    Antigravity,
    /// Scripted agent used by the test suite.
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentSelectionSource {
    Explicit,
    Project,
    Bucket,
    Fallback,
}

impl AgentSelectionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Project => "project",
            Self::Bucket => "bucket",
            Self::Fallback => "fallback",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "explicit" => Some(Self::Explicit),
            "project" => Some(Self::Project),
            "bucket" => Some(Self::Bucket),
            "fallback" => Some(Self::Fallback),
            _ => None,
        }
    }
}

/// The API wire format an endpoint speaks. An adapter declares the
/// dialects its CLI supports, so a model profile serves whichever
/// agent a session happens to run without naming agents itself. Only
/// dialects an adapter actually speaks are modelled: one no agent can
/// select would let a user build an entry that fails at spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModelDialect {
    AnthropicMessages,
    OpenaiResponses,
    GoogleGenai,
}

impl ModelDialect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic-messages",
            Self::OpenaiResponses => "openai-responses",
            Self::GoogleGenai => "google-genai",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic-messages" => Some(Self::AnthropicMessages),
            "openai-responses" => Some(Self::OpenaiResponses),
            "google-genai" => Some(Self::GoogleGenai),
            _ => None,
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::AnthropicMessages,
        Self::OpenaiResponses,
        Self::GoogleGenai,
    ];
}

/// Which configuration layer supplied a session's model profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelProfileSource {
    Explicit,
    Project,
    Bucket,
}

impl ModelProfileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Project => "project",
            Self::Bucket => "bucket",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "explicit" => Some(Self::Explicit),
            "project" => Some(Self::Project),
            "bucket" => Some(Self::Bucket),
            _ => None,
        }
    }
}

/// A provider account. `key_set` is the only thing any API reports
/// about the credential; the value is write-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProfile {
    pub id: u64,
    pub name: String,
    pub key_set: bool,
    pub endpoints: Vec<ModelProfileEndpoint>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProfileEndpoint {
    pub profile_id: u64,
    pub dialect: ModelDialect,
    pub model: String,
    /// Empty uses the agent CLI's own default endpoint and account.
    pub base_url: String,
    /// Cheaper model for auxiliary work off the main loop; adapters
    /// whose CLI has no such setting ignore it.
    pub background_model: String,
}

/// The dialects one adapter speaks, most preferred first, plus the
/// endpoint fields its CLI can actually apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDialects {
    pub agent: AgentKind,
    pub dialects: Vec<ModelDialect>,
    /// False means this agent's CLI has no small/fast model setting and
    /// ignores an entry's `background_model`.
    pub supports_background_model: bool,
}

/// The single entry selected for one spawn, plus the credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelEndpoint {
    pub dialect: ModelDialect,
    pub model: String,
    pub base_url: String,
    pub background_model: String,
    pub api_key: String,
    /// The profile's name, for agent CLIs that display their provider.
    pub provider_name: String,
}

impl ResolvedModelEndpoint {
    /// Whether this entry redirects the CLI away from its own account.
    /// The credential belongs to that endpoint, so it is exported only
    /// when the traffic goes there.
    pub fn redirects_traffic(&self) -> bool {
        !self.base_url.is_empty()
    }
}

impl AgentKind {
    /// Every kind the wire enum models. Enumerating this instead of
    /// naming agents keeps a new kind from silently dropping out of the
    /// surfaces that walk the set.
    pub const ALL: &'static [Self] = &[
        Self::ClaudeCode,
        Self::Codex,
        Self::Gemini,
        Self::OpenCode,
        Self::Antigravity,
        Self::Test,
    ];

    /// The agents a spawn can choose: every kind the standard adapter
    /// registry ships an adapter for. A kind outside this list exists on
    /// the wire but has no adapter yet, so offering it would build a
    /// spawn the daemon rejects.
    pub const SELECTABLE: &'static [Self] = &[
        Self::Antigravity,
        Self::ClaudeCode,
        Self::Codex,
        Self::Gemini,
        Self::OpenCode,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::OpenCode => "opencode",
            AgentKind::Antigravity => "antigravity",
            AgentKind::Test => "test",
        }
    }

    /// The binary a spawn launches. It matches the kind's own name for
    /// every agent but Antigravity, whose CLI installs as `agy`.
    pub fn program(&self) -> &'static str {
        match self {
            AgentKind::Antigravity => "agy",
            other => other.as_str(),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(AgentKind::ClaudeCode),
            "codex" => Some(AgentKind::Codex),
            "gemini" => Some(AgentKind::Gemini),
            "opencode" => Some(AgentKind::OpenCode),
            "antigravity" => Some(AgentKind::Antigravity),
            "test" => Some(AgentKind::Test),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    Starting,
    Working,
    NeedsInput,
    Idle,
    Exited,
    Failed,
    AwaitingWorker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionRole {
    Worker,
    Supervisor,
}

impl SessionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Supervisor => "supervisor",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "worker" => Some(Self::Worker),
            "supervisor" => Some(Self::Supervisor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstructionTarget {
    All,
    Worker,
    Supervisor,
}

impl InstructionTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Worker => "worker",
            Self::Supervisor => "supervisor",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "worker" => Some(Self::Worker),
            "supervisor" => Some(Self::Supervisor),
            _ => None,
        }
    }
    pub fn applies_to(self, role: SessionRole) -> bool {
        self == Self::All
            || matches!(
                (self, role),
                (Self::Worker, SessionRole::Worker) | (Self::Supervisor, SessionRole::Supervisor)
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionLayer {
    pub id: u64,
    pub bucket_id: u64,
    pub project_id: Option<u64>,
    pub target: InstructionTarget,
    pub markdown: String,
    pub revision: u64,
    pub updated_at_unix_ms: i64,
    pub updated_by_session_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionRevision {
    pub revision: u64,
    pub markdown: String,
    pub note: String,
    pub updated_at_unix_ms: i64,
    pub updated_by_session_id: Option<u64>,
}

impl SessionState {
    pub fn is_live(&self) -> bool {
        !matches!(self, SessionState::Exited | SessionState::Failed)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Starting => "starting",
            SessionState::Working => "working",
            SessionState::NeedsInput => "needs-input",
            SessionState::Idle => "idle",
            SessionState::Exited => "exited",
            SessionState::Failed => "failed",
            SessionState::AwaitingWorker => "awaiting-worker",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "starting" => Some(SessionState::Starting),
            "working" => Some(SessionState::Working),
            "needs-input" => Some(SessionState::NeedsInput),
            "idle" => Some(SessionState::Idle),
            "exited" => Some(SessionState::Exited),
            "failed" => Some(SessionState::Failed),
            "awaiting-worker" => Some(SessionState::AwaitingWorker),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Inherit from the level above (project inherits bucket, spawn
    /// inherits project). Buckets never carry this.
    Inherit,
    Default,
    Auto,
    Bypass,
}

impl PermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionMode::Inherit => "inherit",
            PermissionMode::Default => "default",
            PermissionMode::Auto => "auto",
            PermissionMode::Bypass => "bypass",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inherit" => Some(PermissionMode::Inherit),
            "default" => Some(PermissionMode::Default),
            "auto" => Some(PermissionMode::Auto),
            "bypass" => Some(PermissionMode::Bypass),
            _ => None,
        }
    }

    /// Resolves an override against a fallback, treating Inherit as
    /// "use the fallback".
    pub fn or(self, fallback: PermissionMode) -> PermissionMode {
        match self {
            PermissionMode::Inherit => fallback,
            other => other,
        }
    }
}

/// A machine that runs sessions. Worker id 0 is the controller's own
/// embedded local worker.
pub const LOCAL_WORKER_ID: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    pub id: u64,
    pub name: String,
    pub hostname: String,
    pub platform: String,
    pub online: bool,
    pub default_project_root: String,
    pub last_seen_at_unix_ms: Option<i64>,
    /// The pm build reported at the worker's last registration; the
    /// local worker carries the controller's own build. Empty when the
    /// worker has never reported one.
    pub pm_version: String,
    /// The container runtime the worker last reported running under, or
    /// empty for one running on the host itself. The Hosts page shows
    /// the difference, and a worker that predates the report looks like
    /// a host worker, which is what it was indistinguishable from before.
    pub runtime: String,
    /// The runtime's name for the worker's container, which is what an
    /// operator passes to that runtime. Empty off a container.
    pub container: String,
    pub connect_mode: ConnectMode,
    /// host:port the controller dials in [`ConnectMode::Accept`].
    pub endpoint: String,
}

/// Which end of the worker plane opens the socket. The protocol roles do
/// not change with it: the worker still registers and the controller still
/// dispatches work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectMode {
    /// The worker dials the controller. Suits a host behind NAT.
    #[default]
    Dial,
    /// The controller dials the worker. Suits a host that can be routed to
    /// but cannot open outbound connections to the controller.
    Accept,
}

impl ConnectMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectMode::Dial => "dial",
            ConnectMode::Accept => "accept",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dial" => Some(ConnectMode::Dial),
            "accept" => Some(ConnectMode::Accept),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    pub id: u64,
    pub name: String,
    pub position: u32,
    pub permission_mode: PermissionMode,
    /// Preferred spawn agent; None uses the system fallback.
    pub default_agent: Option<AgentKind>,
    /// Model profile for this bucket's sessions; None runs them on the
    /// agent CLI's own account.
    pub model_profile_id: Option<u64>,
    /// The bucket's default worker; 0 is the local worker.
    pub default_worker_id: u64,
    /// Workers sessions in this bucket may run on. Never empty.
    pub allowed_worker_ids: Vec<u64>,
    /// Exactly one bucket is the default when buckets exist.
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: u64,
    pub bucket_id: u64,
    pub name: String,
    pub path: String,
    pub permission_mode: PermissionMode,
    /// Spawn-agent override; None inherits the bucket.
    pub default_agent: Option<AgentKind>,
    /// Model-profile override; None inherits the bucket.
    pub model_profile_id: Option<u64>,
    /// Worker override; None inherits the bucket's default worker.
    pub worker_id: Option<u64>,
    /// Project-specific subset of the parent bucket's allowed workers.
    pub allowed_worker_ids: Vec<u64>,
    /// Persisted effective paths on individual workers. An absent worker
    /// falls back to the project's configured path.
    pub worker_paths: Vec<ProjectPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPath {
    pub worker_id: u64,
    pub path: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlanState {
    #[default]
    Active,
    Accepted,
    Archived,
}

impl PlanState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Accepted => "accepted",
            Self::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "accepted" => Some(Self::Accepted),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlanDecisionMode {
    #[default]
    Single,
    Multiple,
    Dialogue,
}

impl PlanDecisionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multiple => "multiple",
            Self::Dialogue => "dialogue",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "single" => Some(Self::Single),
            "multiple" => Some(Self::Multiple),
            "dialogue" => Some(Self::Dialogue),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlanDecisionState {
    #[default]
    Open,
    Waiting,
    Resolved,
}

impl PlanDecisionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Waiting => "waiting",
            Self::Resolved => "resolved",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "waiting" => Some(Self::Waiting),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub id: u64,
    pub project_id: u64,
    pub bucket_id: u64,
    pub owning_session_id: u64,
    pub creating_session_id: u64,
    pub name: String,
    pub summary: String,
    pub state: PlanState,
    pub markdown_path: String,
    pub revision: u64,
    pub active_decision_id: Option<u64>,
    pub linked_item_ids: Vec<u64>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewMode {
    #[default]
    Range,
    /// One file, diffed against an empty baseline.
    File,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewState {
    #[default]
    Open,
    Finished,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewThreadState {
    /// Written but not handed to the agent yet.
    #[default]
    Draft,
    Sent,
    Answered,
    Resolved,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewSide {
    /// Anchored to the working tree, so it moves as the agent edits.
    #[default]
    Right,
    /// Anchored to the immutable base, so it never moves.
    Left,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewAnchorStatus {
    #[default]
    Same,
    Moved,
    /// The commented line itself was edited.
    Changed,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewChoiceSelect {
    #[default]
    One,
    Many,
}

/// A reader's answer to a marked option list in a rendered markdown
/// document, kept as fields rather than folded into the comment body so
/// the agent reads the decision instead of parsing a sentence back into
/// one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewChoiceAnswer {
    /// The id the document's marker gave the question.
    pub choice_id: String,
    pub select: ReviewChoiceSelect,
    /// The options chosen, by the ids the document gave them.
    pub option_ids: Vec<String>,
    /// Their labels as the reader saw them, so the answer stays
    /// readable after the document is reworded.
    pub option_labels: Vec<String>,
    /// What the reader typed into the free-text option.
    pub other_text: String,
    pub notes: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewAuthor {
    #[default]
    User,
    Session,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewSnapshotKind {
    /// The tree the reviewer handed over at the start of a round.
    #[default]
    Sent,
    /// The tree after the agent finished the round.
    Received,
}

/// A PR-style review of a range or a file, keyed by worktree plus
/// normalized range so it outlives the session that opened it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Review {
    pub id: u64,
    pub session_id: u64,
    pub project_id: u64,
    pub worker_id: u64,
    pub worktree: String,
    pub mode: ReviewMode,
    pub base: String,
    /// Empty means the diff tracks the working tree.
    pub head: String,
    pub pathspec: Vec<String>,
    pub source_file: String,
    pub label: String,
    pub range_key: String,
    pub state: ReviewState,
    pub revision: u32,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub draft_count: u32,
    pub open_count: u32,
    pub answered_count: u32,
    pub resolved_count: u32,
    /// When non-empty, exactly these paths are the review's scope.
    pub explicit_files: Vec<String>,
    /// Newest message on each thread. Says what exists, never who has
    /// read it, so it is safe to broadcast.
    pub thread_latest_message: std::collections::BTreeMap<u64, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewThread {
    pub id: u64,
    pub review_id: u64,
    pub path: String,
    pub line: u32,
    pub side: ReviewSide,
    pub excerpt: String,
    pub anchor_snapshot_id: u64,
    pub current_line: u32,
    pub anchor_status: ReviewAnchorStatus,
    pub current_excerpt: String,
    pub state: ReviewThreadState,
    pub created_rev: u32,
    pub created_at_unix_ms: i64,
    pub messages: Vec<ReviewMessage>,
    /// The anchored line changed in a revision the reader has not
    /// advanced to yet.
    pub changed_ahead: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewMessage {
    pub id: u64,
    pub thread_id: u64,
    pub author: ReviewAuthor,
    pub session_id: u64,
    pub body: String,
    pub addressed: bool,
    pub revision: u32,
    pub created_at_unix_ms: i64,
    /// Revision whose changes this reply produced; 0 when none.
    pub changes_rev: u32,
    pub changed_files: Vec<String>,
    /// Set when this message answers a marked option list.
    pub choice: Option<ReviewChoiceAnswer>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewRevision {
    pub id: u64,
    pub review_id: u64,
    pub rev: u32,
    pub kind: ReviewSnapshotKind,
    pub snapshot_id: u64,
    pub created_at_unix_ms: i64,
    pub files: Vec<String>,
}

/// Per user per review. What makes leaving and returning resume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewViewerState {
    pub review_id: u64,
    pub user_id: u64,
    /// 0 means the live working tree.
    pub pinned_rev: u32,
    pub view: String,
    pub layout: String,
    pub context: u32,
    pub viewed_files: Vec<String>,
    pub preview_off_files: Vec<String>,
    pub last_thread_id: u64,
    /// Keyed by "<view>:<layout>:<context>".
    pub scroll: std::collections::BTreeMap<String, i32>,
    pub file_list_collapsed: bool,
    /// Text typed but not yet submitted, keyed by where it was typed.
    pub drafts: std::collections::BTreeMap<String, String>,
    /// Newest message this reader has had in front of them, per thread.
    pub seen: std::collections::BTreeMap<u64, u64>,
}

/// A partial write of viewer state; `None` keeps the stored value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewViewerStateUpdate {
    pub review_id: u64,
    pub pinned_rev: Option<u32>,
    pub view: Option<String>,
    pub layout: Option<String>,
    pub context: Option<u32>,
    pub viewed_files: Option<Vec<String>>,
    pub preview_off_files: Option<Vec<String>>,
    pub last_thread_id: Option<u64>,
    pub scroll_key: Option<String>,
    pub scroll_top: Option<i32>,
    pub file_list_collapsed: Option<bool>,
    /// Sets one draft slot; an empty body clears it.
    pub draft_key: Option<String>,
    pub draft_body: Option<String>,
    /// Marks one thread read up to a message.
    pub seen_thread: Option<u64>,
    pub seen_message: Option<u64>,
}

/// The agent-reported git location of a session. Each field is
/// independently optional: an agent reports what it knows, and a report
/// that omits a field leaves the stored value alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionGit {
    /// Current branch, or a short SHA when HEAD is detached.
    pub branch: String,
    /// Working tree the session operates in.
    pub worktree: String,
    /// Root of the main checkout the worktree belongs to.
    pub repo_root: String,
    /// Short SHA of HEAD.
    pub commit: String,
    /// Tracking ref, e.g. "origin/master".
    pub upstream: String,
    /// Whether the tree has uncommitted changes. `None` when the agent
    /// did not report it, which is not the same as clean.
    pub dirty: Option<bool>,
}

impl SessionGit {
    /// True when no field carries a value, so storage can drop the row
    /// back to `None` rather than publishing an empty object.
    pub fn is_empty(&self) -> bool {
        self.branch.is_empty()
            && self.worktree.is_empty()
            && self.repo_root.is_empty()
            && self.commit.is_empty()
            && self.upstream.is_empty()
            && self.dirty.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: u64,
    pub project_id: u64,
    pub agent: AgentKind,
    /// The configuration layer that selected `agent` at spawn time.
    pub agent_source: AgentSelectionSource,
    pub state: SessionState,
    pub task_title: String,
    pub task_prompt: String,
    pub agent_session_id: Option<String>,
    pub created_at_unix_ms: i64,
    pub ended_at_unix_ms: Option<i64>,
    pub exit_code: Option<i32>,
    pub state_detail: String,
    pub activity: String,
    pub progress_percent: Option<u32>,
    pub resumable: bool,
    pub permission_mode: PermissionMode,
    pub worker_id: u64,
    pub cwd: String,
    /// What the session is about, as a short noun phrase. Seeded at
    /// spawn from the task, replaced when the agent reports one.
    pub goal: String,
    /// Agent-authored current step, or the outcome once the turn ends.
    pub headline: String,
    /// Agent-authored fuller summary for the detail view.
    pub summary: String,
    /// Where the session sits in git, as the agent reports it. `None`
    /// until an agent reports, and when the cwd is not a repository.
    pub git: Option<SessionGit>,
    /// Whether the session's MCP server offers the item tools; set at
    /// spawn time.
    pub items_api: bool,
    /// Whether the session's MCP server offers the supervisor tools
    /// (spawning and steering sessions for board items); set at spawn
    /// time.
    pub supervisor_api: bool,
    /// The supervisor session this one was spawned by, when it was
    /// spawned through the supervisor tools.
    pub spawned_by_session_id: Option<u64>,
    pub role: SessionRole,
    /// The activity clock the session list shows and sorts by: newest of
    /// created, ended, a submitted user line, an agent turn boundary from a
    /// lifecycle hook, and an agent report. PTY output and unsubmitted
    /// typing never move it. Derived by storage, never written directly.
    pub last_activity_at_unix_ms: i64,
    /// Last observed output or explicit report from the agent terminal.
    /// Internal: it tells quiet-detection whether the agent is streaming
    /// and is not shown as activity.
    pub last_agent_activity_at_unix_ms: i64,
    /// Last successfully delivered user input to the agent terminal,
    /// submitted or not.
    pub last_user_interaction_at_unix_ms: i64,
    /// Whether the current NeedsInput transition still needs user attention.
    /// Persisted through the notifications read model, not lifecycle state.
    pub needs_input_unseen: bool,
    /// Whether the turn that made this session idle has not been viewed.
    /// Persisted through the notifications read model, not lifecycle state.
    pub idle_unseen: bool,
    /// Model profile resolved at spawn. The id is stored rather than a
    /// copy of its values, so a resume reselects the entry and picks up
    /// edits made since.
    pub model_profile_id: Option<u64>,
    pub model_profile_source: Option<ModelProfileSource>,
}

impl Session {
    /// The name every client shows for the session: its goal, then task
    /// title, then headline, so no session is listed without a name.
    pub fn display_name(&self) -> String {
        [&self.goal, &self.task_title, &self.headline]
            .into_iter()
            .find(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("session {}", self.id))
    }
}

/// Canonical ready-versus-triage guidance for item creation surfaces.
pub const ITEM_STATUS_CREATE_GUIDANCE: &str = concat!(
    "For a new item, use `planned` for queued/ready work when the request is sufficiently ",
    "specified and no additional user information, triage, prioritization, or decision is ",
    "needed before work can start. A clear user request to create, add, or file an ",
    "unambiguous task should normally be `planned` unless the user specifies another status. ",
    "Use `inbox` only for captured work that genuinely requires user triage, clarification, ",
    "prioritization, or a decision before it is ready. Raw creates that omit status fall back ",
    "to `inbox`, so set `planned` explicitly for ready work. Dispatching a worker is ",
    "orthogonal to status: `planned` does not mean a worker has been or should be dispatched. ",
    "Examples: \"Create a task to rename the Run button to Dispatch\" -> `planned`; ",
    "\"Maybe improve the dashboard navigation\" -> `inbox`; \"Create a task to rename the ",
    "Run button to Dispatch, but do not dispatch it\" -> `planned`."
);

/// Where a work item stands. `Blocked` and `BlockedExternal` are one
/// concept split by who must act: `Blocked` needs the user (or an item
/// on the board) to move, `BlockedExternal` waits on someone outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemStatus {
    /// Captured work that needs user triage, clarification,
    /// prioritization, or a decision before it is ready.
    Inbox,
    /// Queued/ready work that needs no additional user information,
    /// prioritization, or decision before it can start. This does not
    /// imply worker dispatch.
    Planned,
    /// Actively worked, by the user or a linked session.
    InProgress,
    Blocked,
    BlockedExternal,
    Done,
    /// Deliberately not doing; kept so re-sweeps don't re-file it.
    Dropped,
}

impl ItemStatus {
    pub const ALL: [ItemStatus; 7] = [
        ItemStatus::Inbox,
        ItemStatus::Planned,
        ItemStatus::InProgress,
        ItemStatus::Blocked,
        ItemStatus::BlockedExternal,
        ItemStatus::Done,
        ItemStatus::Dropped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Planned => "planned",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::BlockedExternal => "blocked_external",
            Self::Done => "done",
            Self::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Done and dropped are handled work: agents may not silently move
    /// an item back out of them.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Dropped)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemPriority {
    Urgent,
    High,
    Normal,
    Low,
}

impl ItemPriority {
    pub const ALL: [ItemPriority; 4] = [
        ItemPriority::Urgent,
        ItemPriority::High,
        ItemPriority::Normal,
        ItemPriority::Low,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Urgent => "urgent",
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// The channel an item came from. Drives the source badge and filters;
/// the freeform detail (mailbox, repo, channel) rides `source_detail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemSourceKind {
    Email,
    Slack,
    Github,
    Jira,
    Teams,
    Telegram,
    Human,
    Agent,
    Other,
}

impl ItemSourceKind {
    pub const ALL: [ItemSourceKind; 9] = [
        ItemSourceKind::Email,
        ItemSourceKind::Slack,
        ItemSourceKind::Github,
        ItemSourceKind::Jira,
        ItemSourceKind::Teams,
        ItemSourceKind::Telegram,
        ItemSourceKind::Human,
        ItemSourceKind::Agent,
        ItemSourceKind::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Slack => "slack",
            Self::Github => "github",
            Self::Jira => "jira",
            Self::Teams => "teams",
            Self::Telegram => "telegram",
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// A unit of work the user (or an agent on their behalf) may need to
/// act on, scoped to a bucket and optionally narrowed to a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Public bucket-local number. It is only meaningful with `bucket_id`.
    pub id: u64,
    pub bucket_id: u64,
    pub project_id: Option<u64>,
    /// Stable dedup identity for agent re-sweeps, unique per bucket,
    /// opaque to the daemon. None for human-created items.
    pub external_key: Option<String>,
    pub title: String,
    pub body: String,
    /// The current question awaiting a user response; empty when none.
    pub question: String,
    pub status: ItemStatus,
    pub priority: ItemPriority,
    pub source_kind: ItemSourceKind,
    pub source_detail: String,
    /// Link to the item's external source; empty when there is none.
    pub url: String,
    pub due_at_unix_ms: Option<i64>,
    /// Human-only; agents can neither set nor clear it.
    pub snoozed_until_unix_ms: Option<i64>,
    pub created_by_session_id: Option<u64>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub done_at_unix_ms: Option<i64>,
    /// Items this one waits on ("blocked by").
    pub blocked_by: Vec<u64>,
    /// Sessions that have worked this item.
    pub session_ids: Vec<u64>,
}

/// Unambiguous public identity for an item. Database surrogate ids never use
/// this type and never cross the protocol boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemRef {
    pub bucket_id: u64,
    pub item_id: u64,
}

/// Metadata for file bytes stored separately from normal item snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemAttachment {
    pub id: u64,
    pub bucket_id: u64,
    /// Public bucket-local item number.
    pub item_id: u64,
    pub filename: String,
    pub media_type: String,
    pub byte_length: u64,
    /// Lowercase hexadecimal SHA-256 digest. Content bytes never ride this type.
    pub sha256: String,
    pub created_at_unix_ms: i64,
    pub created_by_session_id: Option<u64>,
}

/// A markdown digest an agent posted for a bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketBriefing {
    pub id: u64,
    pub bucket_id: u64,
    pub session_id: Option<u64>,
    pub ts_unix_ms: i64,
    pub markdown: String,
}

/// How a context field renders; the dashboard needs no schema beyond it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextKind {
    Text,
    Code,
    Url,
    Badge,
    Metric,
    Progress,
    Timestamp,
}

impl ContextKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Code => "code",
            Self::Url => "url",
            Self::Badge => "badge",
            Self::Metric => "metric",
            Self::Progress => "progress",
            Self::Timestamp => "timestamp",
        }
    }

    /// Parses a kind, falling back to `Text` for anything unrecognized so
    /// an agent's typo renders as plain text rather than being rejected.
    pub fn parse_or_text(s: &str) -> Self {
        match s {
            "code" => Self::Code,
            "url" => Self::Url,
            "badge" => Self::Badge,
            "metric" => Self::Metric,
            "progress" => Self::Progress,
            "timestamp" => Self::Timestamp,
            _ => Self::Text,
        }
    }
}

/// Colors a badge or metric; `Neutral` is the uncolored default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextSeverity {
    Neutral,
    Info,
    Good,
    Warn,
    Bad,
}

impl ContextSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Info => "info",
            Self::Good => "good",
            Self::Warn => "warn",
            Self::Bad => "bad",
        }
    }

    pub fn parse_or_neutral(s: &str) -> Self {
        match s {
            "info" => Self::Info,
            "good" => Self::Good,
            "warn" => Self::Warn,
            "bad" => Self::Bad,
            _ => Self::Neutral,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextField {
    pub key: String,
    pub label: String,
    pub value: String,
    pub kind: ContextKind,
    pub severity: ContextSeverity,
}

/// A session's two agent-authored context bags. Carried apart from
/// `Session` so field updates don't resend the whole session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionContext {
    pub session_id: u64,
    pub glance: Vec<ContextField>,
    pub detail: Vec<ContextField>,
}

/// A published TCP forward: a controller-side listener spliced to a
/// port on the session's worker loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionForward {
    pub id: u64,
    pub session_id: u64,
    pub worker_port: u16,
    pub listener_port: u16,
    /// The name the publisher chose, and the subdomain the forward is
    /// hosted at under a share domain. Empty on rows created before
    /// publishing required one.
    pub slug: String,
    pub label: String,
    pub scheme: String,
    pub created_at_unix_ms: i64,
    /// The address a user can open. Empty while the listener is not
    /// bound, and also when no `--public-url` is set and the daemon
    /// binds an address no other device can reach.
    pub url: String,
    /// Whether the worker could reach the forwarded port on its own
    /// loopback, from the most recent stream open: `None` until one
    /// happens, `Some(false)` when that port refused. It says nothing
    /// about whether a client can reach the controller's listener.
    pub target_reachable: Option<bool>,
    /// The shared directory, relative to the session working directory,
    /// when this forward serves one rather than a port the agent bound
    /// itself. Empty for a published port.
    pub source_path: String,
}

/// One directory share a worker currently serves. The port is ephemeral
/// and changes on every rebind, so the share id is the identity and the
/// port is only where it answers right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerDirShare {
    pub share_id: u64,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalKind {
    Agent,
    Shell,
}

impl TerminalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Shell => "shell",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "shell" => Some(Self::Shell),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalRunState {
    Starting,
    Running,
    Exited,
    Failed,
}

impl TerminalRunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Failed => "failed",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "starting" => Some(Self::Starting),
            "running" => Some(Self::Running),
            "exited" => Some(Self::Exited),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
    pub fn is_live(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    pub id: u64,
    pub session_id: u64,
    pub kind: TerminalKind,
    pub title: String,
    pub cwd: String,
    pub created_at_unix_ms: i64,
    pub generation: u64,
    pub state: TerminalRunState,
    pub started_at_unix_ms: Option<i64>,
    pub ended_at_unix_ms: Option<i64>,
    pub exit_code: Option<i32>,
    pub scrollback_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Bucket(u64),
    Project(u64),
    Session(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClientEnvelope {
    pub seq: u64,
    pub msg: ClientMsg,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMsg {
    Subscribe {
        scope: Scope,
    },
    SpawnSession {
        project_id: u64,
        agent: Option<AgentKind>,
        task_title: String,
        task_prompt: String,
        cwd: String,
        permission_mode: PermissionMode,
        worker_id: Option<u64>,
        items_api: bool,
        supervisor_api: bool,
        model_profile_id: Option<u64>,
        /// Explicit host as a worker id or name; empty means none. The
        /// daemon accepts only hosts the project is configured for.
        host: String,
        initial_cols: Option<u16>,
        initial_rows: Option<u16>,
    },
    AttachPty {
        session_id: u64,
    },
    DetachPty {
        session_id: u64,
    },
    PtyInput {
        session_id: u64,
        data: Bytes,
    },
    PtyResize {
        session_id: u64,
        cols: u16,
        rows: u16,
    },
    InterruptSession {
        session_id: u64,
    },
    KillSession {
        session_id: u64,
    },
    CreateBucket {
        name: String,
        allowed_worker_ids: Vec<u64>,
        default_worker_id: u64,
        is_default: bool,
    },
    DeleteBucket {
        id: u64,
    },
    CreateProject {
        bucket_id: u64,
        name: String,
        path: String,
        worker_id: Option<u64>,
        allowed_worker_ids: Vec<u64>,
    },
    UpdateProject {
        project_id: u64,
        path: Option<String>,
        permission_mode: Option<PermissionMode>,
        worker_id: Option<Option<u64>>,
    },
    DeleteProject {
        id: u64,
    },
    HookEvent {
        session_token: String,
        kind: HookKind,
        detail: String,
        agent_session_id: String,
        transcript_path: String,
        /// Whether the agent still has work in flight after this hook.
        /// A turn that ends with a backgrounded subagent, shell command,
        /// monitor or workflow outstanding has not left the session
        /// idle, however finished the turn itself looks.
        background_work: bool,
    },
    ResumeSession {
        session_id: u64,
    },
    SetBucketPermissionMode {
        bucket_id: u64,
        mode: PermissionMode,
    },
    SetProjectPermissionMode {
        project_id: u64,
        mode: PermissionMode,
    },
    SetBucketDefaultAgent {
        bucket_id: u64,
        agent: Option<AgentKind>,
    },
    SetProjectDefaultAgent {
        project_id: u64,
        agent: Option<AgentKind>,
    },
    CreateModelProfile {
        name: String,
        api_key: Option<String>,
    },
    /// `None` fields leave the stored value untouched, so editing a
    /// profile keeps its credential.
    UpdateModelProfile {
        id: u64,
        name: Option<String>,
        api_key: Option<String>,
        clear_api_key: bool,
    },
    DeleteModelProfile {
        id: u64,
    },
    /// Creates or replaces the profile's entry for one dialect.
    SetModelProfileEndpoint {
        profile_id: u64,
        dialect: ModelDialect,
        model: String,
        base_url: String,
        background_model: String,
    },
    DeleteModelProfileEndpoint {
        profile_id: u64,
        dialect: ModelDialect,
    },
    SetBucketModelProfile {
        bucket_id: u64,
        model_profile_id: Option<u64>,
    },
    SetProjectModelProfile {
        project_id: u64,
        model_profile_id: Option<u64>,
    },
    /// Sets or clears the project's launch path on one worker. The
    /// worker is referenced by id or name. A `None` path clears the
    /// entry so spawns on that worker fall back to the project path.
    SetProjectWorkerPath {
        project_id: u64,
        worker: String,
        path: Option<String>,
    },
    /// Starts a review, or attaches to the one already covering this
    /// target. `target` is a ref, a range, or a path to a file.
    OpenReview {
        session_id: u64,
        worktree: String,
        base: String,
        head: String,
        pathspec: Vec<String>,
        files: Vec<String>,
        source_file: String,
        label: String,
        reset: bool,
    },
    AddReviewComment {
        review_id: u64,
        path: String,
        line: u32,
        side: ReviewSide,
        excerpt: String,
        body: String,
        send: bool,
        /// The snapshot the reviewer's page was showing, which is what
        /// the line counts against. `None` from an agent or an older
        /// client.
        anchor_snapshot_id: Option<u64>,
        /// Set when the comment answers a marked option list. Boxed so
        /// one seldom-used payload does not set the size of every
        /// message on the wire.
        choice: Option<Box<ReviewChoiceAnswer>>,
    },
    EditReviewComment {
        message_id: u64,
        body: String,
        /// Replaces the answer on this message, so a reader changing
        /// their mind leaves one answer rather than two.
        choice: Option<Box<ReviewChoiceAnswer>>,
    },
    DeleteReviewThread {
        thread_id: u64,
    },
    /// Sends the named threads, or every draft when none are named.
    SendReviewThreads {
        review_id: u64,
        thread_ids: Vec<u64>,
    },
    ResolveReviewThread {
        thread_id: u64,
        resolved: bool,
    },
    /// The reviewer's own reply inside a thread, rather than a second
    /// thread on the same line.
    ReplyReviewThread {
        thread_id: u64,
        body: String,
    },
    PostReviewReply {
        thread_id: u64,
        body: String,
        addressed: bool,
    },
    /// Moves the reader onto a revision; 0 takes the newest.
    AdvanceReview {
        review_id: u64,
        rev: u32,
    },
    FinishReview {
        review_id: u64,
    },
    SetReviewViewerState(Box<ReviewViewerStateUpdate>),
    ListReviews {
        session_id: u64,
        include_finished: bool,
    },
    /// Resolves a revision on the host holding the tree.
    ResolveRev {
        session_id: u64,
        worktree: String,
        rev: String,
    },
    CreateShell {
        session_id: u64,
        title: String,
    },
    RestartTerminal {
        terminal_id: u64,
    },
    CloseTerminal {
        terminal_id: u64,
    },
    AttachTerminal {
        terminal_id: u64,
    },
    DetachTerminal {
        terminal_id: u64,
    },
    TerminalInput {
        terminal_id: u64,
        data: Bytes,
    },
    TerminalResize {
        terminal_id: u64,
        cols: u16,
        rows: u16,
    },
    TerminalTranscript {
        terminal_id: u64,
        generation: Option<u64>,
    },
    /// Read-only listing of saved workspaces; the reply's JSON rides
    /// `CommandResult.data`. The socket is owner-trusted, so this spans
    /// all users.
    ListWorkspaces,
    /// The calling session's own forward inventory, for the session-start
    /// hook. Token-scoped: a session sees only the forwards it published.
    SessionForwardInventory {
        session_token: String,
    },
    CloseForward {
        forward_id: u64,
    },
    UpsertItem(Box<ItemWrite>),
    DeleteItem {
        bucket_id: u64,
        id: u64,
    },
    /// Parks an item until a time; None clears the snooze. Human-only:
    /// deliberately unreachable over MCP.
    SnoozeItem {
        bucket_id: u64,
        id: u64,
        until_unix_ms: Option<i64>,
    },
    /// Item query beyond what snapshots carry; the reply's JSON rides
    /// `CommandResult.data`.
    ListItems(ItemQuery),
    /// An item's append-only timeline as JSON in `CommandResult.data`.
    ItemNotes {
        bucket_id: u64,
        item_id: u64,
    },
    /// Writes one daemon setting; `None` clears it so the built-in
    /// default applies again.
    SetSetting {
        key: String,
        value: Option<String>,
    },
    /// Every known setting with current and default values, as JSON in
    /// `CommandResult.data`.
    ListSettings,
    /// Authenticated exact lookup used to restore selected/deep-linked history.
    GetSession {
        session_id: u64,
    },
    /// Newest-first ended history. The response is a wire SessionPage in data.
    ListEndedSessions {
        cursor: String,
        limit: u32,
    },
    /// Metadata-only search across all sessions. The response is a wire SessionPage in data.
    SearchSessions {
        query: String,
        cursor: String,
        limit: u32,
    },
    /// Marks attention seen without acknowledging the session lifecycle.
    MarkSessionSeen {
        session_id: u64,
    },
    /// Enables or disables a session's MCP tool grants at runtime;
    /// `None` fields leave the stored value untouched.
    UpdateSessionApis {
        session_id: u64,
        items_api: Option<bool>,
        supervisor_api: Option<bool>,
        role: Option<SessionRole>,
    },
    ListInstructions {
        bucket_id: u64,
        project_id: Option<u64>,
    },
    GetEffectiveInstructions {
        bucket_id: u64,
        project_id: Option<u64>,
        role: SessionRole,
    },
    SetInstructions {
        bucket_id: u64,
        project_id: Option<u64>,
        target: InstructionTarget,
        markdown: String,
        expected_revision: u64,
        note: String,
    },
    RevertInstructions {
        layer_id: u64,
        revision: u64,
        expected_revision: u64,
        note: String,
    },
    RespondToItem {
        bucket_id: u64,
        item_id: u64,
        text: String,
        target: RespondTarget,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RespondTarget {
    Session(u64),
    NewSupervisor { project_id: u64 },
    ReplyOnly,
}

/// A human-side item write. `None` fields leave the stored value
/// untouched; on create they take defaults. Not subject to the agent
/// sticky rules.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemWrite {
    pub bucket_id: u64,
    /// Update by id, else by (bucket, external_key), else create.
    pub id: Option<u64>,
    pub external_key: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    /// `Some` sets the current question; an empty string clears it.
    pub question: Option<String>,
    pub status: Option<ItemStatus>,
    pub priority: Option<ItemPriority>,
    pub source_kind: Option<ItemSourceKind>,
    pub source_detail: Option<String>,
    pub url: Option<String>,
    pub project_id: Option<u64>,
    pub clear_project: bool,
    pub due_at_unix_ms: Option<i64>,
    pub clear_due: bool,
    /// `Some` replaces all "blocked by" edges.
    pub blocked_by: Option<Vec<u64>>,
    /// Appends a timeline note.
    pub note: Option<String>,
    /// Records that a session works this item (spawn-from-item).
    pub link_session_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemQuery {
    pub bucket_id: u64,
    /// Empty means every status except done/dropped unless
    /// `include_closed`.
    pub statuses: Vec<ItemStatus>,
    /// Case-insensitive search across the user-visible and source fields.
    pub search: Option<String>,
    pub project_id: Option<u64>,
    pub priorities: Vec<ItemPriority>,
    pub source_kinds: Vec<ItemSourceKind>,
    pub updated_since_unix_ms: Option<i64>,
    pub include_closed: bool,
    pub include_snoozed: bool,
    /// Bounded page size. `None` preserves the legacy unpaged query.
    pub limit: Option<u32>,
    pub offset: u32,
    pub summary_filter: Option<ItemSummaryFilter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemSummaryFilter {
    NeedsYou,
    InProgress,
    Planned,
    BlockedExternal,
    DoneRecently,
    LiveLinked,
}

impl ItemSummaryFilter {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "needs_you" => Some(Self::NeedsYou),
            "in_progress" => Some(Self::InProgress),
            "planned" => Some(Self::Planned),
            "blocked_external" => Some(Self::BlockedExternal),
            "done_recently" => Some(Self::DoneRecently),
            "live_linked" => Some(Self::LiveLinked),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs_you",
            Self::InProgress => "in_progress",
            Self::Planned => "planned",
            Self::BlockedExternal => "blocked_external",
            Self::DoneRecently => "done_recently",
            Self::LiveLinked => "live_linked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    NeedsInput,
    TurnEnded,
    /// A turn ended because the agent API failed; the process remains usable.
    TurnFailed,
    PromptSubmitted,
    /// Session started; captures identity without changing state.
    Started,
}

impl HookKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            HookKind::NeedsInput => "needs-input",
            HookKind::TurnEnded => "turn-ended",
            HookKind::TurnFailed => "turn-failed",
            HookKind::PromptSubmitted => "prompt-submitted",
            HookKind::Started => "session-start",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "needs-input" => Some(HookKind::NeedsInput),
            "turn-ended" => Some(HookKind::TurnEnded),
            "turn-failed" => Some(HookKind::TurnFailed),
            "prompt-submitted" => Some(HookKind::PromptSubmitted),
            "session-start" => Some(HookKind::Started),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    SessionChanged(Session),
    SessionRemoved(u64),
    BucketChanged(Bucket),
    BucketRemoved(u64),
    ProjectChanged(Project),
    ProjectRemoved(u64),
    WorkerChanged(Worker),
    WorkerRemoved(u64),
    TerminalChanged(Terminal),
    TerminalRemoved(u64),
    ContextChanged(SessionContext),
    ForwardChanged(SessionForward),
    ForwardRemoved(u64),
    ItemChanged(Item),
    ItemRemoved(ItemRef),
    BriefingChanged(BucketBriefing),
    /// Scoped by the authenticated transport before delivery. A missing
    /// value means the preference was reset.
    UserSettingChanged(UserSettingChanged),
    InstructionLayerChanged(InstructionLayer),
    /// An entry edit publishes a change for its parent profile.
    ModelProfileChanged(ModelProfile),
    ModelProfileRemoved(u64),
    /// Counts and lifecycle only; threads and diffs are fetched over
    /// HTTP so a large review never rides the event bus.
    ReviewChanged(Review),
    ReviewRemoved(u64),
    PlanChanged(Plan),
    PlanRemoved(u64),
    /// An alert the daemon judged worth raising, so a connected client
    /// renders what push would have sent instead of deciding for itself.
    /// Not persisted: a client that was away when it was raised is told
    /// by push, not by this.
    SessionAlert(SessionAlert),
    /// Something granted or moved a credential. Not persisted: the point is
    /// that something is said at the time, rather than a row a user has to
    /// think to go and look at.
    SecurityNotice(SecurityNotice),
}

/// What happened, for a notice the user is told about rather than asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityNoticeKind {
    DeviceEnrolled,
    HostEnrolled,
    HostKeyReplaced,
    InstructionsRewritten,
}

impl SecurityNoticeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeviceEnrolled => "device-enrolled",
            Self::HostEnrolled => "host-enrolled",
            Self::HostKeyReplaced => "host-key-replaced",
            Self::InstructionsRewritten => "instructions-rewritten",
        }
    }
}

/// A notice about something that granted or moved a credential.
///
/// The HTTP API never reads a secret back, so anything holding a session mints
/// rather than steals — and what it mints is a key of its own, which outlives
/// the password and the cookie it was minted with. Revocation is the only
/// undo, and revocation needs the user to know it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityNotice {
    pub kind: SecurityNoticeKind,
    /// What it happened to, as the user would recognise it.
    pub subject: String,
    /// One sentence naming what it now allows.
    pub detail: String,
}

/// Why the daemon raised an alert, mirroring the push event classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAlertKind {
    NeedsInput,
    Failed,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionAlert {
    pub session_id: u64,
    pub kind: SessionAlertKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSetting {
    pub key: String,
    pub value_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSettingChanged {
    pub key: String,
    pub value_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub buckets: Vec<Bucket>,
    pub projects: Vec<Project>,
    pub sessions: Vec<Session>,
    pub workers: Vec<Worker>,
    pub terminals: Vec<Terminal>,
    pub contexts: Vec<SessionContext>,
    pub forwards: Vec<SessionForward>,
    /// Open items plus recently closed ones; older history via
    /// `ListItems`.
    pub items: Vec<Item>,
    /// The latest briefing per bucket; history is served over HTTP.
    pub briefings: Vec<BucketBriefing>,
    /// Populated only for an authenticated user-scoped subscription.
    pub user_settings: Vec<UserSetting>,
    pub instruction_layers: Vec<InstructionLayer>,
    pub model_profiles: Vec<ModelProfile>,
    /// Which dialects each registered adapter speaks, so clients derive
    /// per-profile agent coverage from the daemon's answer.
    pub agent_dialects: Vec<AgentDialects>,
    /// Open reviews. Threads and diffs are served over HTTP.
    pub reviews: Vec<Review>,
    /// This subscriber's own place in each open review.
    pub review_viewer_states: Vec<ReviewViewerState>,
    pub plans: Vec<Plan>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMsg {
    Snapshot(Snapshot),
    Event(Event),
    PtyOutput {
        session_id: u64,
        terminal_id: u64,
        generation: u64,
        data: Bytes,
        replay: bool,
    },
    CommandResult {
        seq: u64,
        result: Result<Option<u64>, String>,
        /// Reply payload for queries (e.g. ListWorkspaces JSON); empty
        /// for plain acks.
        data: Bytes,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HarnessStatus {
    pub state: String,
    pub command: String,
    pub output: String,
    pub error: String,
}

/// Worker-plane message from a remote worker up to the controller.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerMsg {
    HarnessStatus {
        req_id: u64,
        status: HarnessStatus,
    },
    Register {
        protocol_version: u32,
        enrollment_token: String,
        credential: String,
        hostname: String,
        platform: String,
        /// The pm build the worker runs; empty from builds that predate
        /// version reporting.
        pm_version: String,
        default_project_root: String,
        live_sessions: Vec<u64>,
        live_terminals: Vec<WorkerTerminal>,
        pending_transcripts: Vec<WorkerTranscript>,
        live_dir_shares: Vec<WorkerDirShare>,
        /// The container runtime this worker runs under, empty on a host
        /// worker and from builds below
        /// [`crate::WORKER_PROTOCOL_WORKER_RUNTIME`].
        runtime: String,
        /// The runtime's name for the container holding this worker.
        container: String,
    },
    Heartbeat,
    SessionState {
        session_id: u64,
        state: SessionState,
        detail: String,
    },
    SessionExit {
        session_id: u64,
        exit_code: Option<i32>,
    },
    TerminalExit {
        terminal_id: u64,
        generation: u64,
        exit_code: Option<i32>,
        state: TerminalRunState,
        transcript_available: bool,
        transcript_size: u64,
        detail: String,
    },
    NeedsInput {
        session_id: u64,
    },
    TerminalActivity {
        terminal_id: u64,
        generation: u64,
    },
    HookReport {
        session_token: String,
        kind: HookKind,
        detail: String,
        agent_session_id: String,
        transcript_path: String,
        /// Correlates the controller's `HookResult`. Zero asks for no
        /// reply, which is what a worker built before the reply existed
        /// sends and what the controller answers with silence.
        req_id: u64,
        /// Whether the agent still has work in flight after this hook.
        /// A worker older than the first protocol that reports it sends
        /// false, which is what its sessions have always meant.
        background_work: bool,
    },
    FsListing {
        req_id: u64,
        ok: bool,
        error: String,
        dir: String,
        parent: Option<String>,
        entries: Vec<FsEntry>,
    },
    PathChecked {
        req_id: u64,
        status: PathCheck,
        /// The operating system's error behind an unreadable path,
        /// empty otherwise.
        detail: String,
    },
    /// What an `AgentInbox` request did. `detail` explains a failure and
    /// never repeats the message.
    AgentInboxResult {
        req_id: u64,
        outcome: AgentInboxOutcome,
        transport: String,
        mode: AgentInboxMode,
        detail: String,
    },
    FileRead {
        req_id: u64,
        ok: bool,
        error: String,
        content: Vec<u8>,
        filename: String,
    },
    /// An agent's report, relayed because this host cannot reach the
    /// controller's browser plane directly.
    /// A repository read's answer. The payload is an encoded
    /// `RepoAnswer`, opaque on the way through.
    RepoResponse {
        req_id: u64,
        ok: bool,
        error: String,
        answer: Vec<u8>,
    },
    McpRequest {
        req_id: u64,
        bearer: String,
        body: String,
    },
    ForwardOpened {
        req_id: u64,
        ok: bool,
        error: String,
    },
    /// Answers a `DirShareServe`. `port` is the loopback port the worker
    /// bound for the share, and is meaningful only when `ok`.
    DirShareBound {
        share_id: u64,
        ok: bool,
        error: String,
        port: u16,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerTerminal {
    pub terminal_id: u64,
    pub generation: u64,
    pub kind: TerminalKind,
    pub state: TerminalRunState,
    pub agent_resumable: bool,
    pub transcript_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTranscript {
    pub terminal_id: u64,
    pub generation: u64,
    pub size: u64,
}

/// Whether a configured project path can be a session's working
/// directory on the host that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathCheck {
    Ok,
    Missing,
    NotADirectory,
    Unreadable,
}

impl PathCheck {
    pub fn as_str(self) -> &'static str {
        match self {
            PathCheck::Ok => "ok",
            PathCheck::Missing => "missing",
            PathCheck::NotADirectory => "not-a-directory",
            PathCheck::Unreadable => "unreadable",
        }
    }
}

/// How urgently a message should reach an agent, where its channel
/// distinguishes the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentInboxMode {
    /// Take the agent's next natural break.
    #[default]
    Queue,
    /// Reach the model without waiting for the current turn to end.
    Steer,
}

/// What a worker did with a message bound for a session's agent inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentInboxOutcome {
    Delivered,
    /// The agent has no inbound channel, or not one the worker can
    /// address yet.
    NoChannel,
    /// The channel exists and declined the message.
    Failed,
}

impl AgentInboxOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentInboxOutcome::Delivered => "delivered",
            AgentInboxOutcome::NoChannel => "no-channel",
            AgentInboxOutcome::Failed => "failed",
        }
    }
}

/// One directory entry in a worker filesystem listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
}

/// Worker-plane message from the controller down to a remote worker.
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerMsg {
    HarnessRequest {
        req_id: u64,
        agent: AgentKind,
        install: bool,
    },
    /// Asks the host holding a tree to run a repository read. The
    /// payload is an encoded `RepoOp`, opaque on the way through.
    RepoRequest {
        req_id: u64,
        op: Vec<u8>,
    },
    Registered {
        worker_id: u64,
        credential: String,
        error: String,
        /// Where a worker that predates the local MCP relay sends its
        /// agents' reports: a configured public base URL, else empty.
        /// Empty for workers that bind their own relay.
        mcp_base_url: String,
        /// The controller's browser-plane port for the same older workers,
        /// paired with the address they dialed. Zero for workers that bind
        /// their own relay.
        http_port: u32,
        pm_version: String,
    },
    /// Drops the idle requirement on a pending self-update.
    UpdateNow,
    /// Answers a `HookReport`. `nudge` carries the Stop-hook block reason
    /// when a turn ended with no dashboard headline, and is empty when
    /// the stop should proceed.
    HookResult {
        req_id: u64,
        nudge: String,
    },
    Spawn {
        session_id: u64,
        agent: AgentKind,
        task_prompt: String,
        permission_mode: PermissionMode,
        cwd: String,
        session_token: String,
        resume_agent_session_id: String,
        terminal_id: u64,
        generation: u64,
        truecolor: bool,
        /// Whether the agent may take over the alternate screen rather
        /// than rendering into the PTY scrollback.
        fullscreen: bool,
        compiled_instructions: String,
        /// The entry the controller selected for this session's agent,
        /// with the profile's credential. A remote worker builds its own
        /// spawn context, so the resolved entry travels with the spawn.
        /// Boxed to keep it from dominating the size of every message.
        model_endpoint: Option<Box<ResolvedModelEndpoint>>,
        initial_cols: Option<u16>,
        initial_rows: Option<u16>,
    },
    SpawnShell {
        terminal_id: u64,
        generation: u64,
        cwd: String,
        truecolor: bool,
        initial_cols: Option<u16>,
        initial_rows: Option<u16>,
    },
    Interrupt {
        session_id: u64,
    },
    Kill {
        session_id: u64,
    },
    FsList {
        req_id: u64,
        path: String,
    },
    PathCheck {
        req_id: u64,
        path: String,
    },
    /// Asks the worker to hand `text` to a session's agent over the
    /// agent's own inbound channel. The worker holds the PTY child and
    /// the socket directory; the controller holds the agent's own
    /// session id and the port it was given, so each side sends what
    /// only it knows.
    AgentInbox {
        req_id: u64,
        session_id: u64,
        /// The session's agent terminal. Its PTY child is the process
        /// the inbox belongs to, and the worker's mux is keyed by
        /// terminal, so a session id finds an unrelated child there.
        agent_terminal_id: u64,
        agent: AgentKind,
        agent_session_id: String,
        agent_port: Option<u16>,
        text: String,
        mode: AgentInboxMode,
    },
    FileRead {
        req_id: u64,
        root: String,
        path: String,
        max_bytes: u64,
    },
    McpResponse {
        req_id: u64,
        status: u32,
        body: String,
    },
    TerminalInterrupt {
        terminal_id: u64,
        generation: u64,
    },
    TerminalKill {
        terminal_id: u64,
        generation: u64,
    },
    TerminalAttach {
        terminal_id: u64,
        generation: u64,
        token: String,
        replay_bytes: u64,
        /// PTY size to apply before snapshotting, `(0, 0)` to keep it.
        size: (u16, u16),
    },
    TerminalDetach {
        terminal_id: u64,
        generation: u64,
    },
    Transcript {
        terminal_id: u64,
        generation: u64,
        token: String,
    },
    TranscriptAck {
        terminal_id: u64,
        generation: u64,
    },
    ForwardOpen {
        req_id: u64,
        port: u16,
        token: String,
    },
    /// Asks the worker to serve `path`, resolved below the session
    /// working directory `root`, from a loopback server it owns.
    DirShareServe {
        share_id: u64,
        root: String,
        path: String,
    },
    DirShareStop {
        share_id: u64,
    },
}

#[cfg(test)]
mod permission_tests {
    use super::PermissionMode::*;

    #[test]
    fn or_resolves_the_cascade() {
        // spawn.or(project).or(bucket)
        assert_eq!(
            Inherit.or(Inherit).or(Bypass),
            Bypass,
            "both inherit -> bucket"
        );
        assert_eq!(
            Inherit.or(Auto).or(Bypass),
            Auto,
            "project override wins over bucket"
        );
        assert_eq!(
            Default.or(Auto).or(Bypass),
            Default,
            "spawn override wins over all"
        );
    }
}
