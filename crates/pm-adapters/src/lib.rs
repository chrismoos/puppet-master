//! Agent adapters own everything specific to one agent CLI: how to
//! spawn it, how to wire its lifecycle hooks back to the daemon, and
//! later how to resume and interpret its output. The daemon core
//! stays agent-agnostic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rand::RngCore;

use pm_protocol::domain::{
    AgentKind, ModelDialect, ModelProfileEndpoint, PermissionMode, ResolvedModelEndpoint,
    SessionForward,
};

/// Environment variables injected into every managed session; hook
/// commands inherit them and use them to reach the daemon.
pub const ENV_SOCKET: &str = "PM_SOCKET";
pub const ENV_SESSION_TOKEN: &str = "PM_SESSION_TOKEN";
/// The daemon's MCP endpoint, read by the `pm _mcp` stdio bridge that
/// serves agent CLIs whose MCP configuration is global rather than
/// per-session.
pub const ENV_MCP_URL: &str = "PM_MCP_URL";

/// Claude Code treats this as "the environment already constrains this
/// session", which among other things settles its directory-trust
/// question without a prompt. Undocumented, so it is set only where its
/// effect is known to be nil.
const ENV_CLAUDE_SANDBOXED: &str = "CLAUDE_CODE_SANDBOXED";

/// Set to 1, Claude Code renders into the scrollback instead of taking
/// over the alternate screen, so a session's output stays scrollable in
/// whatever is attached to the PTY.
const ENV_CLAUDE_DISABLE_ALTERNATE_SCREEN: &str = "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN";

/// Claude Code reads its endpoint and credential from these.
const ENV_ANTHROPIC_BASE_URL: &str = "ANTHROPIC_BASE_URL";
const ENV_ANTHROPIC_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
/// The CLI's small/fast model, which is what an entry's background
/// model means. `ANTHROPIC_DEFAULT_HAIKU_MODEL` is deliberately not
/// set: it remaps the haiku tier alias, so it would also change what
/// an explicit haiku selection resolves to.
const ENV_ANTHROPIC_SMALL_FAST_MODEL: &str = "ANTHROPIC_SMALL_FAST_MODEL";

/// The variable Codex is told to read its credential from, so the key
/// reaches the child through the environment rather than a config file
/// or the command line.
const ENV_MODEL_API_KEY: &str = "PM_MODEL_API_KEY";

/// Gemini has no `--settings` flag. It merges four settings layers and
/// this variable points at the System Overrides layer, which merges
/// object-valued settings rather than replacing them, so a per-session
/// file adds our hooks and MCP server without disturbing the user's own
/// settings or their login.
const ENV_GEMINI_SYSTEM_SETTINGS_PATH: &str = "GEMINI_CLI_SYSTEM_SETTINGS_PATH";

/// Gemini's endpoint and credential variables. Vertex AI is a second
/// endpoint shape with its own variables that a single `base_url` field
/// cannot disambiguate, so an entry never selects it.
const ENV_GOOGLE_GEMINI_BASE_URL: &str = "GOOGLE_GEMINI_BASE_URL";
const ENV_GEMINI_API_KEY: &str = "GEMINI_API_KEY";

/// The name the compiled instructions are written under. Gemini
/// discovers context files by filename, so the name is what reaches it.
const GEMINI_INSTRUCTIONS_FILE: &str = "PM-INSTRUCTIONS.md";

/// Gemini's own default context filename, kept alongside ours so naming
/// our file does not stop a project's GEMINI.md from loading.
const GEMINI_DEFAULT_CONTEXT_FILE: &str = "GEMINI.md";

/// The `model_providers` table key Codex's provider override is
/// written under.
const CODEX_PROVIDER_ID: &str = "puppet-master";

/// The only wire format Codex accepts; it rejects any other value at
/// config load.
const CODEX_WIRE_API: &str = "responses";

/// The customization tree the Antigravity CLI reads out of each
/// workspace directory it is given.
const ANTIGRAVITY_CUSTOMIZATION_DIR: &str = ".agents";
/// The CLI reads MCP servers from one file per user, under the home
/// directory, and from nowhere a session can point it at.
const ANTIGRAVITY_MCP_CONFIG: &str = ".gemini/config/mcp_config.json";
const ANTIGRAVITY_HOOKS_FILE: &str = "hooks.json";
const ANTIGRAVITY_RULES_DIR: &str = "rules";
/// The CLI merges the rules directory by filename, and reads only the
/// two names it knows; a file under any other name is left on disk and
/// never reaches the model.
const ANTIGRAVITY_INSTRUCTIONS_FILE: &str = "AGENTS.md";

/// The name our MCP reporting server is registered under, shared by the
/// server config and the permission allow-rule so they cannot drift.
pub const MCP_SERVER_NAME: &str = "puppet-master";

/// OpenCode merges every config layer it finds, so this variable adds a
/// per-session file to the user's own config and provider logins rather
/// than replacing them. Its directory form, `OPENCODE_CONFIG_DIR`, makes
/// OpenCode bootstrap the directory with a `node_modules` of its own as
/// soon as it holds a plugin, which would cost an npm install and 60 MB
/// on every spawn.
const ENV_OPENCODE_CONFIG: &str = "OPENCODE_CONFIG";

/// The `provider` table key OpenCode's endpoint override is written
/// under, and the provider half of the `provider/model` argument.
const OPENCODE_PROVIDER_ID: &str = "puppet-master";

/// The AI SDK package OpenCode loads for each dialect an entry can
/// select. Both take the endpoint and credential from `options`.
const OPENCODE_ANTHROPIC_NPM: &str = "@ai-sdk/anthropic";
const OPENCODE_OPENAI_NPM: &str = "@ai-sdk/openai-compatible";

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("no adapter registered for agent {0:?}")]
    UnknownAgent(&'static str),
    #[error("failed to write session integration file: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} sessions cannot be resumed")]
    ResumeUnsupported(&'static str),
}

pub struct SpawnCtx {
    /// Project working directory the session runs in.
    pub cwd: PathBuf,
    pub task_prompt: String,
    /// The already-resolved effective permission mode.
    pub permission_mode: PermissionMode,
    /// Controller-compiled private Markdown, separate from the task prompt.
    pub compiled_instructions: String,
    /// The endpoint entry the daemon selected for this agent, with the
    /// profile's credential. None runs the agent on its own account.
    pub model_endpoint: Option<ResolvedModelEndpoint>,
    /// Whether the agent may take over the alternate screen. False
    /// keeps its output in the scrollback; only the adapters whose CLI
    /// exposes the choice read it.
    pub fullscreen: bool,
    pub integration: Integration,
}

/// Everything an adapter needs to wire a session back to the daemon.
pub struct Integration {
    pub session_id: u64,
    pub session_token: String,
    pub socket_path: PathBuf,
    /// Absolute path of the pm binary, used as the hook command.
    pub pm_exe: PathBuf,
    /// Directory for per-session files the adapter writes (settings,
    /// MCP config); already exists.
    pub files_dir: PathBuf,
    /// The daemon's MCP reporting endpoint, when the HTTP surface is
    /// up; None means sessions get no self-reporting channel.
    pub mcp_url: Option<String>,
    /// A loopback port reserved for an agent that serves its own
    /// session API. Only the adapters that take a port read it.
    pub agent_port: Option<u16>,
}

/// Transport-neutral description of a process to launch, converted to
/// the PTY layer's command type by the session manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

impl CommandSpec {
    fn with_integration_env(mut self, integration: &Integration) -> Self {
        self.env.push((
            ENV_SOCKET.into(),
            integration.socket_path.display().to_string(),
        ));
        self.env
            .push((ENV_SESSION_TOKEN.into(), integration.session_token.clone()));
        self
    }
}

/// A ready-to-launch session: the command plus the agent's own
/// session identifier when the adapter can predetermine it (used for
/// agent-native resume later).
#[derive(Debug)]
pub struct SpawnPlan {
    pub spec: CommandSpec,
    pub agent_session_id: Option<String>,
    /// The agent emits an OSC9 escape into the PTY when it pauses for
    /// approval; the mux watches for it to mark needs-input.
    pub detect_osc9_needs_input: bool,
}

/// What the daemon knows about a live session, offered to an adapter so
/// it can name the agent's own inbound channel. Every field is optional
/// because a session is spawned before any of them is known.
#[derive(Debug, Clone, Default)]
pub struct InboundFacts {
    /// The PTY child's process id, for the terminal the agent runs in.
    /// Claude Code names its inbox socket after the pid of the process
    /// that binds it, and that pid is also what identifies the peer on
    /// the other end of it.
    pub agent_pid: Option<u32>,
    /// The agent's own conversation identifier, the same one a resume
    /// takes.
    pub agent_session_id: Option<String>,
    /// The loopback port an agent was told to serve its own API on.
    pub agent_port: Option<u16>,
    /// The directory the agent binds its inbox socket in, already
    /// resolved by the daemon. Which directory that is depends on the
    /// platform and on how long the resulting path would be, so it is
    /// found on disk rather than assumed here.
    pub socket_dir: Option<PathBuf>,
    /// The credential the agent minted for its inbox, when the daemon
    /// has it. Optional wherever the agent accepts an unauthenticated
    /// local connection.
    pub inbox_token: Option<String>,
}

/// How a message reaches a running agent without typing at its
/// terminal. Each variant carries everything the delivery needs, so a
/// caller never re-derives an address the adapter already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundChannel {
    /// Claude Code binds a per-session Unix socket and takes
    /// newline-delimited JSON frames on it.
    ClaudeSocket {
        path: PathBuf,
        token: Option<String>,
        /// The process that must be on the other end: the agent this
        /// controller spawned. The socket is named after a pid in a
        /// directory every session of this user shares, so the name is an
        /// address and not an identity, and delivery checks the peer
        /// against this rather than trusting the name.
        expect_pid: u32,
    },
    /// Codex queues a message onto a thread through its own CLI, which
    /// speaks to the app server the TUI already runs on.
    CodexQueue { thread: String },
    /// OpenCode's TUI serves its session API, and the prompt endpoint
    /// takes the message.
    OpenCodeHttp { base_url: String, session: String },
}

impl InboundChannel {
    /// The name this channel reports as, for logs and for telling a
    /// caller which path carried its message.
    pub fn transport(&self) -> &'static str {
        match self {
            Self::ClaudeSocket { .. } => "claude-socket",
            Self::CodexQueue { .. } => "codex-queue",
            Self::OpenCodeHttp { .. } => "opencode-http",
        }
    }

    /// Whether this channel can interrupt a turn in progress rather than
    /// waiting for the agent to reach the end of one.
    pub fn supports_steering(&self) -> bool {
        matches!(self, Self::OpenCodeHttp { .. })
    }
}

/// When a message should reach the model. Not every channel honours
/// both, so a caller asks and the delivery reports what it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    /// Take the agent's next natural break.
    Queue,
    /// Reach the model without waiting for the current turn to end,
    /// where the agent offers that.
    Steer,
}

pub trait AgentAdapter: Send + Sync {
    fn kind(&self) -> AgentKind;
    /// The agent's own inbound channel, or `None` when the terminal is
    /// the only way in. Returning `None` for facts that are not yet
    /// known is normal: the daemon asks again as a session settles.
    fn inbound_channel(&self, _facts: &InboundFacts) -> Option<InboundChannel> {
        None
    }
    /// Whether this agent serves its own API and needs a loopback port
    /// chosen for it before it launches.
    fn wants_agent_port(&self) -> bool {
        false
    }
    /// The API dialects this agent's CLI speaks, most preferred first.
    /// The daemon intersects them with a model profile's entries to
    /// select the one it hands back on `SpawnCtx`.
    fn dialects(&self) -> &'static [ModelDialect] {
        &[]
    }
    /// Whether this agent's CLI has a small/fast model setting. An
    /// adapter that returns false ignores an entry's background model,
    /// and the daemon publishes that so a UI can say so instead of
    /// accepting a value it will drop.
    fn supports_background_model(&self) -> bool {
        false
    }
    /// Whether this adapter installs lifecycle hooks for every managed
    /// generation. The daemon uses this capability to keep PTY output from
    /// standing in for semantic lifecycle events.
    fn has_lifecycle_hooks(&self) -> bool {
        false
    }
    /// Whether this agent's TUI enables bracketed paste and reads a burst
    /// of raw characters as pasted content. Submitting to such a TUI takes
    /// one explicit paste plus a separate Enter written after it settles:
    /// an Enter in the same write becomes a pasted newline and the message
    /// sits unsubmitted in the composer.
    fn submits_after_bracketed_paste(&self) -> bool {
        false
    }
    /// Whether this agent's CLI takes context the daemon supplies at the
    /// start of every session, a resume included. The daemon appends a
    /// resumed session's forward inventory to its compiled instructions
    /// when it does not, so the text arrives either way and never twice.
    fn injects_session_start_context(&self) -> bool {
        false
    }
    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError>;
    /// Spawns a session that resumes an earlier agent conversation.
    fn resume_command(
        &self,
        _ctx: &SpawnCtx,
        _agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        Err(AdapterError::ResumeUnsupported(self.kind().as_str()))
    }
}

pub struct ClaudeCodeAdapter;

/// The command line an agent CLI runs for one lifecycle hook. Agent
/// CLIs execute hook commands through `/bin/sh -c`, so the pm binary
/// path must be shell-quoted or a path with spaces splits into a bogus
/// command and the daemon never sees the lifecycle event.
fn hook_shell_command(pm_exe: &std::path::Path, kind: &str) -> String {
    format!(
        "{} _hook {kind}",
        shell_quote(&pm_exe.display().to_string())
    )
}

/// The `--agent` value Claude Code's hooks carry so `pm _hook` can tell
/// a Claude SessionStart from Gemini's, which uses the same payload
/// shape, and add the Claude-only instructions to its context.
pub const CLAUDE_HOOK_AGENT: &str = "claude";

fn claude_hook_shell_command(pm_exe: &std::path::Path, kind: &str) -> String {
    format!(
        "{} --agent {CLAUDE_HOOK_AGENT}",
        hook_shell_command(pm_exe, kind)
    )
}

/// Single-quotes a value for `/bin/sh -c` unless every byte is
/// shell-neutral, keeping typical install paths readable.
fn shell_quote(value: &str) -> String {
    let neutral = |b: u8| {
        b.is_ascii_alphanumeric()
            || matches!(b, b'_' | b'-' | b'.' | b'/' | b'+' | b':' | b'@' | b'%')
    };
    if !value.is_empty() && value.bytes().all(neutral) {
        return value.into();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Whether embedding this path unquoted in a shell command line would
/// split or reinterpret it. The adapters quote such paths themselves;
/// spawn plumbing uses this to log that the launch environment is
/// hazardous, since older agent CLI versions may still mishandle it.
pub fn path_needs_shell_quoting(path: &std::path::Path) -> bool {
    let value = path.display().to_string();
    shell_quote(&value) != value
}

/// Claude Code lifecycle hooks wired per session: SessionStart establishes
/// generation authority, Notification fires when the agent waits on the user,
/// Stop/StopFailure finish a turn, and UserPromptSubmit starts the next. Each
/// runs `pm _hook`, which forwards the signal over the daemon socket using the
/// PM_SESSION_TOKEN / PM_SOCKET env this process exports. The
/// matcher-less SessionStart hook also re-injects the reporting brief on
/// startup, resume, `/clear`, and `/compact`, so the contract survives a
/// context reset.
///
/// `fullscreen` asks for the alt-screen renderer outright rather than
/// leaving it to whatever the user's own Claude config says, so the
/// daemon setting decides it in both directions.
fn claude_settings_json(pm_exe: &std::path::Path, fullscreen: bool) -> String {
    let hook = |kind: &str| {
        serde_json::json!([{
            "hooks": [{
                "type": "command",
                "command": claude_hook_shell_command(pm_exe, kind),
            }]
        }])
    };
    let mut settings = serde_json::json!({
        // Pre-approve the reporting server's tools so they never raise an
        // approval prompt in default or auto permission mode. They only
        // update the dashboard and create no files or output, so auto-
        // allowing them is safe and keeps the session unattended.
        "permissions": {
            "allow": [format!("mcp__{MCP_SERVER_NAME}")]
        },
        // Without this a session that bypasses permission prompts holds
        // every peer message for a human to approve, which no unattended
        // worker is there to do.
        "crossSessionInbound": "accept",
        "hooks": {
            "SessionStart": hook("session-start"),
            "Notification": hook("needs-input"),
            "Stop": hook("turn-ended"),
            "StopFailure": hook("turn-failed"),
            "UserPromptSubmit": hook("prompt-submitted"),
        }
    });
    if fullscreen {
        settings["tui"] = serde_json::json!("fullscreen");
    }
    settings.to_string()
}

/// The reporting brief injected into every managed session so the agent
/// keeps the dashboard current. It is the durable contract: Claude
/// re-injects it on every SessionStart (surviving `/clear` and
/// `/compact`), and both agents get it appended to the initial prompt.
/// The tools carry their own detailed descriptions; this is the why.
pub const REPORTING_BRIEF: &str = "You are running inside Puppet Master, a dashboard where a \
human watches many agent sessions at once. Keep yours legible from the outside: call the \
`report` tool whenever things meaningfully change — you start a step, finish one, learn \
something, or hit a blocker. Set `goal`: it names what the session is \
about, a short noun phrase such as \"Optimizing femtocell software\", and it is your session's \
name in the list. Goal is required on every report; it may remain the same across turns, but \
must be constantly updated to reflect the current goal. The `headline` is \
required: it is the current step, or the outcome when the turn ends, shown under the goal. \
Never put the step in the goal or the goal in the headline. Keep headlines plain text (`&`, not `&amp;`). Optionally include `summary`, a timeline `note`, `glance` \
(the 1-3 most important chips for the row), and `context` (fuller key/value facts like \
preview_url, tests). Keep `glance` and `context` small, current, and high-signal. Treat 20 \
context fields as a ceiling, not a target, and use `clear` to drop stale keys. When you are \
working in a git repository, report where you are in the `git` field — branch, worktree, and \
whatever else you know — rather than as a glance chip or a context key, and send it again \
after any checkout or worktree move so the dashboard does not show a branch you have left. \
Call \
`flag_blocked` the moment you need the user. Before ending a turn that cannot continue without \
a decision, clarification, permission, credential, or external action, call `flag_blocked` with \
one concrete question. Never merely print a question and then stop or finish. Idle means the \
turn is complete and no input is currently required; never use Idle as an implicit waiting \
state. These tools only update the dashboard a human is watching — they create no files or \
output. When you share HTML, images, reports, or similar \
artifacts for review, call `publish_dir` with the directory holding them and a slug naming \
it, and give the user the returned URL instead of only filesystem paths. Point it at the \
narrowest directory that contains only the intended artifacts. Do not start an HTTP server \
of your own for files: the host running this session serves the directory, so there is no \
port to pick, nothing to keep alive, and nothing to restart after a resume. `list_dirs` \
shows what you publish and `unpublish_dir` closes one. Use `publish_port` instead for a \
server you are genuinely running, such as a dev server or an API, and give the user the URL \
it returns. The \
slug is a DNS label, 3 to 40 lowercase characters of a-z, 0-9 and hyphen, unique across the \
controller's live forwards, and it becomes the hostname where forwards are served under a \
share domain. Republishing a port under a different slug is refused, so close the forward \
and publish again to rename it. Bind a server you start to loopback \
when practical and do not expose unrelated or sensitive files. Keep it available while the \
review is useful. After a resume you are told which ports you published, so restart each \
server on its own port rather than publishing it again, and when a server is no longer needed, \
stop it and call `unpublish_port`. Published directories are not in that list because the \
host keeps serving them for you. Forwarded HTTP previews live under \
`/forwards/{id}/`; the proxy strips that prefix before reaching your server. Use relative \
URLs for assets, links, forms, fetch calls, and WebSockets, or configure the app's base path \
to the returned URL's path. Do not use root-relative URLs such as `/assets/app.js`, \
hardcoded localhost URLs, or `../` paths that escape the mount. Keep directory links \
trailing-slashed, and configure dev-server WebSocket/HMR paths under the same prefix.";

/// Instructions only Claude Code needs, because only its harness offers
/// them: it can publish pages as claude.ai artifacts, which live outside
/// the dashboard the user is watching and need a separate login there.
/// The reporting brief already says to publish a directory instead, and
/// this names the alternative it must not reach for.
pub const CLAUDE_HARNESS_BRIEF: &str = "Do not publish a claude.ai artifact, and do not \
create a Claude Docs document, unless the user explicitly asks for one in this session. \
That includes mockups, reports, plans, screenshots and any other page you want the user to \
look at: the user reads this session through Puppet Master, so write the page into a \
directory and call `publish_dir` as described above, then give them the URL it returns. A \
request to show something, to write it up, or to share it with a team is a request for a \
published directory, not an artifact. If the user does ask for an artifact by name, make it, and still \
offer nothing else through claude.ai on your own initiative.";

/// The heading the Claude-only brief sits under in the instructions
/// file, so it reads as a peer of the compiled sections above it.
const CLAUDE_HARNESS_HEADING: &str = "## Claude Code harness";

/// The Claude-only brief appended under the compiled instructions, or
/// under the reporting brief when the daemon compiled none. Both the
/// system-prompt file at spawn and the SessionStart hook's context use
/// this so the two copies cannot drift apart.
pub fn with_claude_harness_brief(instructions: &str) -> String {
    let base = if instructions.trim().is_empty() {
        REPORTING_BRIEF
    } else {
        instructions
    };
    format!("{base}\n\n{CLAUDE_HARNESS_HEADING}\n\n{CLAUDE_HARNESS_BRIEF}")
}

/// What a session that restarted is told about the forwards it
/// published, so it can bring each server back rather than leaving a URL
/// the user already holds pointing at nothing. It reads as a statement
/// about the restart because it sits in the session's instructions and
/// is re-read for the rest of the session.
const FORWARD_INVENTORY_PREAMBLE: &str =
    "Port forwards this session published before its agent last restarted:";

/// The standing instruction under the list. Republishing would mint a
/// second forward for a port the session already owns, and a new slug
/// would move the URL out from under whoever was given the first one.
/// It disclaims being the session's work because an agent that finds it
/// in its instructions on resume has nothing else in front of it.
const FORWARD_INVENTORY_INSTRUCTION: &str =
    "Those servers did not survive the restart. Before serving or repeating any of these URLs, \
confirm each server is still listening and restart any that is not, on the same port. Never \
publish the port again or change its slug. This is housekeeping for work already under way, \
not a task of its own: keep the goal the session already had rather than reporting this as \
one, and only repeat a URL to the user if they ask for it.";

/// What a legacy forward is called in the list: publishing required no
/// slug when its row was written, so it has none to name.
const UNNAMED_FORWARD_SLUG: &str = "unnamed";

/// The inventory a resumed session is given, or empty when it published
/// nothing. Both paths compose it here so the Claude hook's context and
/// the instructions other agents are started with cannot drift apart.
pub fn forward_inventory(forwards: &[SessionForward]) -> String {
    if forwards.is_empty() {
        return String::new();
    }
    let mut text = String::from(FORWARD_INVENTORY_PREAMBLE);
    for forward in forwards {
        let slug = if forward.slug.is_empty() {
            UNNAMED_FORWARD_SLUG
        } else {
            forward.slug.as_str()
        };
        let address = if forward.url.is_empty() {
            "no public URL".to_string()
        } else {
            format!("at {}", forward.url)
        };
        text.push_str(&format!(
            "\n- {slug}, local port {}, {address}, {}",
            forward.worker_port,
            probe_result_phrase(forward.target_reachable),
        ));
    }
    text.push_str("\n\n");
    text.push_str(FORWARD_INVENTORY_INSTRUCTION);
    text
}

/// How the daemon's last probe of a forwarded port reads to the agent.
/// A probe happens when a client opens the forward, so an untried
/// forward says so rather than claiming either result.
fn probe_result_phrase(target_reachable: Option<bool>) -> &'static str {
    match target_reachable {
        Some(true) => "and the last probe of that port answered",
        Some(false) => "and the last probe of that port was not answered",
        None => "and that port has not been probed yet",
    }
}

/// The initial prompt with the reporting brief appended when MCP is on,
/// or None when there is no prompt (start the agent interactively).
fn prompt_with_reporting(ctx: &SpawnCtx) -> Option<String> {
    (!ctx.task_prompt.is_empty()).then(|| ctx.task_prompt.clone())
}

fn write_instruction_file(ctx: &SpawnCtx) -> Result<Option<PathBuf>, AdapterError> {
    if ctx.compiled_instructions.is_empty() {
        return Ok(None);
    }
    let path = ctx.integration.files_dir.join(format!(
        "session-{}-instructions.md",
        ctx.integration.session_id
    ));
    write_session_file(&path, &ctx.compiled_instructions)?;
    Ok(Some(path))
}

/// Mode for a per-session file, matching what the daemon gives its own state.
/// These files carry the session's MCP bearer token and its compiled
/// instruction overlays, so they are owner-only in their own right rather than
/// relying on the directory above them.
#[cfg(unix)]
const SESSION_FILE_MODE: u32 = 0o600;

/// Mode for a per-session directory an agent is pointed at.
#[cfg(unix)]
const SESSION_DIR_MODE: u32 = 0o700;

fn write_session_file(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    restrict_session_path(path, SESSION_FILE_MODE)
}

/// Creates a per-session directory owner-only, including any parent it has to
/// make on the way. Narrowing the leaf alone would leave a readable directory
/// holding it, because `create_dir_all` gives what it creates the default mode.
fn create_session_dir(path: &Path) -> std::io::Result<()> {
    let mut created = Vec::new();
    let mut cursor = Some(path);
    while let Some(dir) = cursor.filter(|dir| !dir.exists()) {
        created.push(dir);
        cursor = dir.parent();
    }
    std::fs::create_dir_all(path)?;
    for dir in created {
        restrict_session_path(dir, SESSION_DIR_MODE)?;
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_session_path(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn restrict_session_path(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

fn claude_mcp_config_json(mcp_url: &str, token: &str) -> String {
    let mut servers = serde_json::Map::new();
    servers.insert(
        MCP_SERVER_NAME.to_string(),
        serde_json::json!({
            "type": "http",
            "url": mcp_url,
            "headers": { "Authorization": format!("Bearer {token}") }
        }),
    );
    serde_json::json!({ "mcpServers": servers }).to_string()
}

/// Claude Code's model flags and endpoint environment for a selected
/// entry. The credential rides the environment, never the settings
/// file or argv, and only when the entry redirects traffic to the
/// provider the credential belongs to.
fn claude_model_args(endpoint: Option<&ResolvedModelEndpoint>) -> Vec<String> {
    let Some(endpoint) = endpoint else {
        return vec![];
    };
    if endpoint.model.is_empty() {
        return vec![];
    }
    vec!["--model".into(), endpoint.model.clone()]
}

fn claude_model_env(endpoint: Option<&ResolvedModelEndpoint>) -> Vec<(String, String)> {
    let Some(endpoint) = endpoint else {
        return vec![];
    };
    let mut env = Vec::new();
    if endpoint.redirects_traffic() {
        env.push((ENV_ANTHROPIC_BASE_URL.into(), endpoint.base_url.clone()));
        if !endpoint.api_key.is_empty() {
            env.push((ENV_ANTHROPIC_AUTH_TOKEN.into(), endpoint.api_key.clone()));
        }
    }
    if !endpoint.background_model.is_empty() {
        env.push((
            ENV_ANTHROPIC_SMALL_FAST_MODEL.into(),
            endpoint.background_model.clone(),
        ));
    }
    env
}

/// The host OpenCode's own API is pinned to. Loopback only: the port
/// takes a session prompt, so it must not be reachable off the box.
const OPENCODE_API_HOST: &str = "127.0.0.1";

/// Claude Code names the socket after the pid of the process that binds
/// it, so the daemon's own child pid resolves the address with nothing
/// to look up.
fn claude_inbox_path(socket_dir: &std::path::Path, pid: u32) -> PathBuf {
    socket_dir.join(format!("{pid}.sock"))
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn submits_after_bracketed_paste(&self) -> bool {
        true
    }

    fn dialects(&self) -> &'static [ModelDialect] {
        &[ModelDialect::AnthropicMessages]
    }

    fn supports_background_model(&self) -> bool {
        true
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    /// Claude reads its SessionStart hook's stdout and injects the
    /// `additionalContext` it prints, on startup and on every resume.
    fn injects_session_start_context(&self) -> bool {
        true
    }

    fn inbound_channel(&self, facts: &InboundFacts) -> Option<InboundChannel> {
        let socket_dir = facts.socket_dir.as_deref()?;
        let pid = facts.agent_pid?;
        Some(InboundChannel::ClaudeSocket {
            path: claude_inbox_path(socket_dir, pid),
            token: facts.inbox_token.clone(),
            expect_pid: pid,
        })
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        // Claude assigns its own session id; we learn it from the hook
        // payload rather than predetermining it, because an interactive
        // session started with a fabricated --session-id is not reliably
        // resumable.
        let mut args = self.integration_args(ctx)?;
        args.extend(claude_permission_args(ctx.permission_mode));
        args.extend(claude_model_args(ctx.model_endpoint.as_ref()));
        // An empty prompt starts Claude interactively with no initial
        // message; only pass a positional prompt when there is one. --
        // ends option parsing so the variadic --mcp-config cannot
        // swallow it.
        if let Some(prompt) = self.prompt(ctx) {
            args.push("--".into());
            args.push(prompt);
        }
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: "claude".into(),
                args,
                env: claude_env(ctx),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: None,
            detect_osc9_needs_input: false,
        })
    }

    fn resume_command(
        &self,
        ctx: &SpawnCtx,
        agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        let mut args = self.integration_args(ctx)?;
        args.extend(claude_permission_args(ctx.permission_mode));
        args.extend(claude_model_args(ctx.model_endpoint.as_ref()));
        args.push("--resume".into());
        args.push(agent_session_id.to_string());
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: "claude".into(),
                args,
                env: claude_env(ctx),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: Some(agent_session_id.to_string()),
            detect_osc9_needs_input: false,
        })
    }
}

/// Claude permission-mode flags for our agent-agnostic modes.
/// Claude Code asks whether it trusts a directory before it starts, and
/// a spawned session has nobody to answer. The gate withholds exactly
/// one thing: permission rules and additional directories a repository
/// declares for itself, so tool calls those rules would have
/// pre-approved ask instead. A session already launched with
/// `--dangerously-skip-permissions` asks for nothing, so for that mode
/// the gate is protecting a decision that has already been made and
/// answering it ahead of time grants nothing further.
///
/// Any other mode keeps the gate, where withholding a repository's own
/// grants is the whole point.
/// Everything Claude Code is launched with in its environment.
fn claude_env(ctx: &SpawnCtx) -> Vec<(String, String)> {
    let mut env = claude_model_env(ctx.model_endpoint.as_ref());
    env.extend(claude_trust_env(ctx.permission_mode));
    if !ctx.fullscreen {
        env.push((ENV_CLAUDE_DISABLE_ALTERNATE_SCREEN.into(), "1".into()));
    }
    env
}

fn claude_trust_env(mode: PermissionMode) -> Vec<(String, String)> {
    match mode {
        PermissionMode::Bypass => vec![(ENV_CLAUDE_SANDBOXED.into(), "1".into())],
        PermissionMode::Auto | PermissionMode::Default | PermissionMode::Inherit => vec![],
    }
}

fn claude_permission_args(mode: PermissionMode) -> Vec<String> {
    match mode {
        PermissionMode::Auto => vec!["--permission-mode".into(), "acceptEdits".into()],
        PermissionMode::Bypass => vec!["--dangerously-skip-permissions".into()],
        PermissionMode::Default | PermissionMode::Inherit => vec![],
    }
}

/// Codex sandbox/approval flags for our agent-agnostic modes.
fn codex_permission_args(mode: PermissionMode) -> Vec<String> {
    match mode {
        PermissionMode::Auto => vec![
            "--sandbox".into(),
            "workspace-write".into(),
            "-a".into(),
            "on-request".into(),
        ],
        PermissionMode::Bypass => vec!["--dangerously-bypass-approvals-and-sandbox".into()],
        PermissionMode::Default | PermissionMode::Inherit => vec![],
    }
}

impl ClaudeCodeAdapter {
    /// The --settings and --mcp-config flags shared by spawn and
    /// resume; writes the per-session files.
    fn integration_args(&self, ctx: &SpawnCtx) -> Result<Vec<String>, AdapterError> {
        let settings_path = ctx.integration.files_dir.join(format!(
            "session-{}-claude-settings.json",
            ctx.integration.session_id
        ));
        write_session_file(
            &settings_path,
            claude_settings_json(&ctx.integration.pm_exe, ctx.fullscreen),
        )?;
        let mut args = vec!["--settings".into(), settings_path.display().to_string()];
        let instructions_path = ctx.integration.files_dir.join(format!(
            "session-{}-instructions.md",
            ctx.integration.session_id
        ));
        write_session_file(
            &instructions_path,
            with_claude_harness_brief(&ctx.compiled_instructions),
        )?;
        args.push("--append-system-prompt-file".into());
        args.push(instructions_path.display().to_string());
        if let Some(mcp_url) = &ctx.integration.mcp_url {
            let mcp_path = ctx.integration.files_dir.join(format!(
                "session-{}-claude-mcp.json",
                ctx.integration.session_id
            ));
            write_session_file(
                &mcp_path,
                claude_mcp_config_json(mcp_url, &ctx.integration.session_token),
            )?;
            args.push("--mcp-config".into());
            args.push(mcp_path.display().to_string());
        }
        Ok(args)
    }

    /// The initial prompt, or None to start Claude with no prompt. The
    /// reporting hint only rides along when there is a real prompt.
    fn prompt(&self, ctx: &SpawnCtx) -> Option<String> {
        prompt_with_reporting(ctx)
    }
}

pub struct CodexAdapter;

const CODEX_INTERRUPT_HOOK_TIMEOUT_SECONDS: u64 = 3;

/// Codex's lifecycle hooks share Claude's schema and payload shape
/// (session_id, transcript_path, hook_event_name), so the same
/// `pm _hook` receiver serves both. Hooks are injected as inline TOML
/// config overrides rather than a settings file.
impl CodexAdapter {
    fn hook_override(pm_exe: &std::path::Path, event: &str, kind: &str) -> String {
        let timeout = if event == "Interrupt" {
            format!(",timeout={CODEX_INTERRUPT_HOOK_TIMEOUT_SECONDS}")
        } else {
            String::new()
        };
        format!(
            "hooks.{event}=[{{hooks=[{{type=\"command\",command={}{timeout}}}]}}]",
            toml_string(&hook_shell_command(pm_exe, kind))
        )
    }

    fn config_args(&self, ctx: &SpawnCtx) -> Result<Vec<String>, AdapterError> {
        let pm_exe = &ctx.integration.pm_exe;
        let mut config = Vec::new();
        trust_project_dir(&ctx.cwd)?;
        if let Some(mcp_url) = &ctx.integration.mcp_url {
            config.push(format!(
                "mcp_servers.{MCP_SERVER_NAME}.url={}",
                toml_string(mcp_url)
            ));
            config.push(format!(
                "mcp_servers.{MCP_SERVER_NAME}.bearer_token_env_var=\"PM_SESSION_TOKEN\""
            ));
            // Auto-approve the reporting server's tools so they never
            // raise an approval prompt outside bypass mode, matching the
            // Claude allow-rule. They only update the dashboard.
            config.push(format!(
                "mcp_servers.{MCP_SERVER_NAME}.default_tools_approval_mode=\"approve\""
            ));
            // Codex hooks cannot inject context, so carry the reporting
            // brief as a developer instruction — it adds to the session's
            // instructions every turn rather than sitting once at the tail
            // of the prompt, which the model attends to far less reliably.
        }
        if let Some(path) = write_instruction_file(ctx)? {
            let markdown = std::fs::read_to_string(path)?;
            config.push(format!("developer_instructions={}", toml_string(&markdown)));
        }
        config.push(Self::hook_override(pm_exe, "SessionStart", "session-start"));
        config.push(Self::hook_override(pm_exe, "Stop", "turn-ended"));
        config.push(Self::hook_override(pm_exe, "Interrupt", "turn-failed"));
        config.push(Self::hook_override(pm_exe, "Notification", "needs-input"));
        config.push(Self::hook_override(
            pm_exe,
            "UserPromptSubmit",
            "prompt-submitted",
        ));
        config.extend(codex_model_config(ctx.model_endpoint.as_ref()));

        let mut args = Vec::new();
        for c in config {
            args.push("-c".into());
            args.push(c);
        }
        // Emit an OSC9 desktop-notification escape into the PTY only
        // when Codex pauses for approval, so the mux can detect
        // needs-input; scoped to that one event and forced on
        // regardless of terminal focus.
        for c in [
            "tui.notifications=[\"approval-requested\"]",
            "tui.notification_method=\"osc9\"",
            "tui.notification_condition=\"always\"",
        ] {
            args.push("-c".into());
            args.push(c.into());
        }
        // Our own pm _hook commands are trusted by construction; without
        // this Codex silently skips untrusted hooks.
        args.push("--dangerously-bypass-hook-trust".into());
        args.extend(codex_permission_args(ctx.permission_mode));
        Ok(args)
    }

    /// The initial prompt verbatim, or None for an interactive start.
    /// Codex carries the reporting brief as a developer instruction, so
    /// it is not appended here.
    fn prompt(&self, ctx: &SpawnCtx) -> Option<String> {
        (!ctx.task_prompt.is_empty()).then(|| ctx.task_prompt.clone())
    }
}

/// Codex's `-c` overrides for a selected entry. Codex has no
/// small/fast model setting, so `background_model` is ignored. The
/// provider block only exists when the entry redirects traffic;
/// otherwise the entry just pins a model on the CLI's own account.
fn codex_model_config(endpoint: Option<&ResolvedModelEndpoint>) -> Vec<String> {
    let Some(endpoint) = endpoint else {
        return vec![];
    };
    let mut config = Vec::new();
    if !endpoint.model.is_empty() {
        config.push(format!("model={}", toml_string(&endpoint.model)));
    }
    if !endpoint.redirects_traffic() {
        return config;
    }
    let name = if endpoint.provider_name.is_empty() {
        MCP_SERVER_NAME
    } else {
        &endpoint.provider_name
    };
    config.push(format!("model_provider=\"{CODEX_PROVIDER_ID}\""));
    config.push(format!(
        "model_providers.{CODEX_PROVIDER_ID}.name={}",
        toml_string(name)
    ));
    config.push(format!(
        "model_providers.{CODEX_PROVIDER_ID}.base_url={}",
        toml_string(&endpoint.base_url)
    ));
    config.push(format!(
        "model_providers.{CODEX_PROVIDER_ID}.env_key=\"{ENV_MODEL_API_KEY}\""
    ));
    config.push(format!(
        "model_providers.{CODEX_PROVIDER_ID}.wire_api=\"{CODEX_WIRE_API}\""
    ));
    config
}

fn codex_model_env(endpoint: Option<&ResolvedModelEndpoint>) -> Vec<(String, String)> {
    match endpoint {
        Some(endpoint) if endpoint.redirects_traffic() && !endpoint.api_key.is_empty() => {
            vec![(ENV_MODEL_API_KEY.into(), endpoint.api_key.clone())]
        }
        _ => vec![],
    }
}

/// Encodes a value as a TOML basic string. Control characters other
/// than tab are invalid unescaped in TOML, so newlines in instruction
/// markdown and any stray control bytes must use escape sequences.
/// Codex asks the operator whether a directory is trusted before it
/// starts, and withholds project-local config, hooks and exec policies
/// until they answer. Nobody is at the terminal of a spawned session, so
/// that question stalls it indefinitely: no turn begins, no lifecycle
/// hook arrives, and the session never becomes resumable or
/// addressable. Every task worktree is a path the operator has never
/// seen, so a managed session hits this on the paths it is most likely
/// to run in.
///
/// Codex reads the decision from its own config and not from a `-c`
/// override, so recording it there is the only way to answer ahead of
/// time. The write is append-only and skipped when an entry already
/// exists, so it adds the same line the operator's own "yes" would and
/// never rewrites what they have.
fn trust_project_dir(cwd: &std::path::Path) -> Result<(), AdapterError> {
    let Some(home) = codex_home() else {
        return Ok(());
    };
    trust_project_dir_in(&home, cwd)
}

fn trust_project_dir_in(home: &std::path::Path, cwd: &std::path::Path) -> Result<(), AdapterError> {
    let config = home.join("config.toml");
    let existing = std::fs::read_to_string(&config).unwrap_or_default();
    let header = format!("[projects.{}]", toml_string(&cwd.display().to_string()));
    if existing.lines().any(|line| line.trim() == header) {
        return Ok(());
    }
    std::fs::create_dir_all(home)?;
    let mut stanza = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        stanza.push('\n');
    }
    stanza.push_str(&format!("\n{header}\ntrust_level = \"trusted\"\n"));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config)?;
    file.write_all(stanza.as_bytes())?;
    Ok(())
}

/// Where Codex keeps its configuration, honouring the override it reads
/// itself so a caller that redirects Codex is not answered in the wrong
/// file.
fn codex_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".codex"))
}

fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl AgentAdapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn submits_after_bracketed_paste(&self) -> bool {
        true
    }

    fn dialects(&self) -> &'static [ModelDialect] {
        &[ModelDialect::OpenaiResponses]
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    fn inbound_channel(&self, facts: &InboundFacts) -> Option<InboundChannel> {
        // The thread id is the same one a resume takes, and the TUI
        // runs on the app server that owns the thread store, so a
        // queued message reaches a session that is already open.
        let thread = facts.agent_session_id.clone()?;
        Some(InboundChannel::CodexQueue { thread })
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        let mut args = self.config_args(ctx)?;
        // An empty prompt starts Codex interactively with no message.
        if let Some(prompt) = self.prompt(ctx) {
            args.push(prompt);
        }
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: "codex".into(),
                args,
                env: codex_model_env(ctx.model_endpoint.as_ref()),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: None,
            detect_osc9_needs_input: true,
        })
    }

    fn resume_command(
        &self,
        ctx: &SpawnCtx,
        agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        let mut args = self.config_args(ctx)?;
        args.push("resume".into());
        args.push(agent_session_id.to_string());
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: "codex".into(),
                args,
                env: codex_model_env(ctx.model_endpoint.as_ref()),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: Some(agent_session_id.to_string()),
            detect_osc9_needs_input: true,
        })
    }
}

pub struct GeminiAdapter;

/// Gemini's lifecycle hooks use Claude's definition schema and payload
/// field names, so the same `pm _hook` receiver serves it, and its
/// SessionStart reads `hookSpecificOutput.additionalContext`, so the
/// reporting brief is re-injected on every start rather than carried as
/// a one-off instruction.
///
/// Gemini has no StopFailure counterpart, so nothing reports a turn that
/// died before AfterAgent. That turn is left to the daemon's stale-turn
/// watchdog, which this adapter reaches by declaring lifecycle hooks:
/// once the PTY has been silent long enough the session moves to Idle
/// with a detail saying the end was inferred, and one byte of output
/// puts it back to Working.
fn gemini_settings_json(
    pm_exe: &std::path::Path,
    mcp: Option<(&str, &str)>,
    with_instructions: bool,
) -> String {
    let hook = |kind: &str| {
        serde_json::json!([{
            "hooks": [{
                "type": "command",
                "command": hook_shell_command(pm_exe, kind),
            }]
        }])
    };
    let mut settings = serde_json::json!({
        "hooks": {
            "SessionStart": hook("session-start"),
            "Notification": hook("needs-input"),
            "BeforeAgent": hook("prompt-submitted"),
            "AfterAgent": hook("turn-ended"),
        }
    });
    if let Some((mcp_url, token)) = mcp {
        settings["mcpServers"] = serde_json::json!({
            MCP_SERVER_NAME: {
                "httpUrl": mcp_url,
                "headers": { "Authorization": format!("Bearer {token}") },
                // Bypasses this server's approval prompts so the
                // reporting tools never stall an unattended session,
                // matching the allow-rule Claude and Codex are given.
                "trust": true,
            }
        });
    }
    if with_instructions {
        settings["context"] = serde_json::json!({
            "fileName": [GEMINI_DEFAULT_CONTEXT_FILE, GEMINI_INSTRUCTIONS_FILE],
            "loadMemoryFromIncludeDirectories": true,
        });
    }
    settings.to_string()
}

fn gemini_model_args(endpoint: Option<&ResolvedModelEndpoint>) -> Vec<String> {
    let Some(endpoint) = endpoint else {
        return vec![];
    };
    if endpoint.model.is_empty() {
        return vec![];
    }
    vec!["-m".into(), endpoint.model.clone()]
}

/// Gemini has no small/fast model setting, so an entry's background
/// model is ignored. The credential rides the environment, and only when
/// the entry redirects traffic to the provider it belongs to.
fn gemini_model_env(endpoint: Option<&ResolvedModelEndpoint>) -> Vec<(String, String)> {
    match endpoint {
        Some(endpoint) if endpoint.redirects_traffic() => {
            let mut env = vec![(ENV_GOOGLE_GEMINI_BASE_URL.into(), endpoint.base_url.clone())];
            if !endpoint.api_key.is_empty() {
                env.push((ENV_GEMINI_API_KEY.into(), endpoint.api_key.clone()));
            }
            env
        }
        _ => vec![],
    }
}

/// Gemini approval-mode flags for our agent-agnostic modes.
fn gemini_permission_args(mode: PermissionMode) -> Vec<String> {
    match mode {
        PermissionMode::Auto => vec!["--approval-mode".into(), "auto_edit".into()],
        PermissionMode::Bypass => vec!["--approval-mode".into(), "yolo".into()],
        PermissionMode::Default | PermissionMode::Inherit => vec![],
    }
}

/// A random RFC 4122 version 4 UUID, the only session-id form Gemini
/// accepts. It must be fresh per launch: Gemini exits fatally when
/// `--session-id` names a session that already exists on disk.
fn random_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

impl GeminiAdapter {
    fn settings_path(ctx: &SpawnCtx) -> PathBuf {
        ctx.integration.files_dir.join(format!(
            "session-{}-gemini-settings.json",
            ctx.integration.session_id
        ))
    }

    /// The flags and per-session files shared by spawn and resume.
    /// `--skip-trust` is not optional: without it Gemini refuses to start
    /// in a directory it has not been told to trust, and the session dies
    /// at launch.
    fn integration_args(&self, ctx: &SpawnCtx) -> Result<Vec<String>, AdapterError> {
        let instructions_dir = self.write_instructions_context(ctx)?;
        let mcp = ctx
            .integration
            .mcp_url
            .as_deref()
            .map(|url| (url, ctx.integration.session_token.as_str()));
        write_session_file(
            &Self::settings_path(ctx),
            gemini_settings_json(&ctx.integration.pm_exe, mcp, instructions_dir.is_some()),
        )?;
        let mut args = vec!["--skip-trust".into()];
        if let Some(dir) = &instructions_dir {
            args.push("--include-directories".into());
            args.push(dir.display().to_string());
        }
        args.extend(gemini_permission_args(ctx.permission_mode));
        args.extend(gemini_model_args(ctx.model_endpoint.as_ref()));
        Ok(args)
    }

    /// Writes the compiled instructions as a context file in a directory
    /// of its own. Gemini reads context files out of the directories it
    /// is given, and the session's other files carry the session token,
    /// so the instructions cannot share a directory with them.
    fn write_instructions_context(&self, ctx: &SpawnCtx) -> Result<Option<PathBuf>, AdapterError> {
        if ctx.compiled_instructions.is_empty() {
            return Ok(None);
        }
        let dir = ctx.integration.files_dir.join(format!(
            "session-{}-gemini-context",
            ctx.integration.session_id
        ));
        create_session_dir(&dir)?;
        write_session_file(
            &dir.join(GEMINI_INSTRUCTIONS_FILE),
            &ctx.compiled_instructions,
        )?;
        Ok(Some(dir))
    }

    fn env(&self, ctx: &SpawnCtx) -> Vec<(String, String)> {
        let mut env = gemini_model_env(ctx.model_endpoint.as_ref());
        env.push((
            ENV_GEMINI_SYSTEM_SETTINGS_PATH.into(),
            Self::settings_path(ctx).display().to_string(),
        ));
        env
    }
}

impl AgentAdapter for GeminiAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Gemini
    }

    fn submits_after_bracketed_paste(&self) -> bool {
        true
    }

    fn dialects(&self) -> &'static [ModelDialect] {
        &[ModelDialect::GoogleGenai]
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        let mut args = self.integration_args(ctx)?;
        // Gemini honors a session id chosen here and reports it back in
        // every hook payload, so unlike Claude the session is resumable
        // from the moment it launches.
        let agent_session_id = random_uuid_v4();
        args.push("--session-id".into());
        args.push(agent_session_id.clone());
        if let Some(prompt) = prompt_with_reporting(ctx) {
            args.push("--".into());
            args.push(prompt);
        }
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: "gemini".into(),
                args,
                env: self.env(ctx),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: Some(agent_session_id),
            detect_osc9_needs_input: false,
        })
    }

    fn resume_command(
        &self,
        ctx: &SpawnCtx,
        agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        // `--resume` takes "latest", a 1-based index, or a full session
        // id, and the resumed session keeps that id. `--session-id` is
        // not passed alongside it: it means "start a new session with
        // this id" and rejects one that already exists.
        let mut args = self.integration_args(ctx)?;
        args.push("--resume".into());
        args.push(agent_session_id.to_string());
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: "gemini".into(),
                args,
                env: self.env(ctx),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: Some(agent_session_id.to_string()),
            detect_osc9_needs_input: false,
        })
    }
}

pub struct OpenCodeAdapter;

/// OpenCode has no hook configuration. Its plugins receive the whole
/// event bus, so one per-session plugin module translates the events
/// that carry lifecycle meaning into the same `pm _hook` calls the other
/// adapters install, and the existing receiver serves it unchanged.
///
/// A turn starting is `session.status` going busy, not the user
/// `message.updated`: OpenCode emits that one again for the same message
/// after the turn is over, which would put a finished session back to
/// working and leave it there.
///
/// Busy is a level OpenCode re-broadcasts several times within one turn,
/// so only the edge into it submits a prompt. Reporting every busy event
/// put the session to working half a dozen times per turn, which reads
/// in the log as the same event arriving twice. The turn's end clears
/// the edge so the next one is reported again.
///
/// `session.idle` also lands right after `session.error`, and both end a
/// turn, so reporting the idle would replace the failure detail with
/// nothing. The plugin drops the idle that follows a failure it already
/// reported.
///
/// The task tool runs subagents as child sessions with their own
/// events; a child's `session.idle` would end the parent's turn early,
/// so events for a session created with a parent are ignored.
///
/// OpenCode does not wait for one subscriber call before making the
/// next, and the events that end a turn arrive within a millisecond of
/// each other, so the hook calls are chained: two racing `pm _hook`
/// processes let a turn-ended land before the working it followed and
/// leave a finished session reading as busy.
///
/// A resumed session emits no `session.created`, so the plugin reports
/// the session start when it loads and lets `session.created` carry the
/// agent's own id when there is one. Without it a resumed session never
/// settles out of starting.
fn opencode_plugin_js(pm_exe: &std::path::Path) -> String {
    let pm = serde_json::Value::String(pm_exe.display().to_string());
    format!(
        r#"const PM = {pm};

// A permission or question is asked and answered inside a running turn,
// so the answer never reaches the controller as a fresh submission: the
// session below is already busy and the turn-ended hook preserves the
// blocked state. Reporting each reply as a submission is what returns
// the session to working.
const KINDS = {{
  "session.created": "session-start",
  "permission.asked": "needs-input",
  "permission.replied": "prompt-submitted",
  "question.asked": "needs-input",
  "question.replied": "prompt-submitted",
  "question.rejected": "prompt-submitted",
  "session.error": "turn-failed",
  "session.idle": "turn-ended",
}};

export const PuppetMaster = async ({{ $ }}) => {{
  const children = new Set();
  const failed = new Set();
  const busy = new Set();
  let queue = Promise.resolve();

  const send = (kind, session, detail) => {{
    const field = kind === "turn-failed" ? "error" : "message";
    const payload = JSON.stringify({{ session_id: session, [field]: detail }});
    queue = queue
      .then(() => $`${{PM}} _hook ${{kind}} < ${{Buffer.from(payload)}}`.quiet().nothrow())
      .catch(() => {{}});
    return queue;
  }};

  await send("session-start", "", "");

  return {{
    event: async ({{ event }}) => {{
      const data = event.properties ?? event.data ?? {{}};
      const session = data.sessionID ?? "";
      if (event.type === "session.created" && data.info?.parentID) children.add(session);
      if (children.has(session)) return;

      let kind = KINDS[event.type];
      let detail = "";
      if (event.type === "session.status") {{
        if (data.status?.type !== "busy") {{
          busy.delete(session);
          return;
        }}
        if (busy.has(session)) return;
        busy.add(session);
        kind = "prompt-submitted";
      }} else if (event.type === "permission.asked") {{
        const target = data.patterns?.[0];
        detail = target ? `${{data.permission}}: ${{target}}` : (data.permission ?? "");
      }} else if (event.type === "question.asked") {{
        const first = data.questions?.[0] ?? {{}};
        detail = first.header || first.question || "";
      }} else if (event.type === "session.error") {{
        busy.delete(session);
        failed.add(session);
        detail = data.error?.name ?? "";
      }} else if (event.type === "session.idle") {{
        busy.delete(session);
        if (failed.delete(session)) return;
      }}
      if (!kind) return;

      await send(kind, session, detail);
    }},
  }};
}};
"#
    )
}

/// OpenCode's per-session config. Every key here merges over the user's
/// own config rather than replacing it, and the credential is a
/// `{env:...}` reference so it stays in the environment.
fn opencode_config_json(
    plugin_path: &std::path::Path,
    instructions: Option<&std::path::Path>,
    mcp: Option<(&str, &str)>,
    permission: Option<serde_json::Value>,
    endpoint: Option<&ResolvedModelEndpoint>,
) -> String {
    let mut config = serde_json::json!({
        "plugin": [format!("file://{}", plugin_path.display())],
    });
    if let Some(path) = instructions {
        config["instructions"] = serde_json::json!([path.display().to_string()]);
    }
    if let Some((mcp_url, token)) = mcp {
        config["mcp"] = serde_json::json!({
            MCP_SERVER_NAME: {
                "type": "remote",
                "url": mcp_url,
                "enabled": true,
                "headers": { "Authorization": format!("Bearer {token}") },
            }
        });
    }
    if let Some(permission) = permission {
        config["permission"] = permission;
    }
    if let Some(provider) = opencode_provider(endpoint) {
        config["provider"] = serde_json::json!({ OPENCODE_PROVIDER_ID: provider });
    }
    config.to_string()
}

/// The provider block for an entry that redirects traffic. An entry
/// that only pins a model needs none: the model then names a provider
/// OpenCode already has a login for.
fn opencode_provider(endpoint: Option<&ResolvedModelEndpoint>) -> Option<serde_json::Value> {
    let endpoint = endpoint?;
    if !endpoint.redirects_traffic() {
        return None;
    }
    let name = if endpoint.provider_name.is_empty() {
        MCP_SERVER_NAME
    } else {
        &endpoint.provider_name
    };
    let npm = match endpoint.dialect {
        ModelDialect::AnthropicMessages => OPENCODE_ANTHROPIC_NPM,
        _ => OPENCODE_OPENAI_NPM,
    };
    let mut provider = serde_json::json!({
        "npm": npm,
        "name": name,
        "options": {
            "baseURL": endpoint.base_url,
            // OpenCode expands this at config load, so the key reaches
            // the provider from the environment and never sits in a file.
            "apiKey": format!("{{env:{ENV_MODEL_API_KEY}}}"),
        },
    });
    if !endpoint.model.is_empty() {
        provider["models"] = serde_json::json!({
            endpoint.model.clone(): { "name": endpoint.model.clone() }
        });
    }
    Some(provider)
}

/// OpenCode ships its default agent with an allow-everything ruleset, so
/// unlike the other CLIs it asks for nothing on its own. Default mode
/// therefore installs the ask baseline rather than passing no flags, or
/// a default-mode session would run unattended and never reach
/// needs-input.
///
/// The reporting server's tools are allowed by name so they never raise
/// an approval prompt, matching the allow-rule Claude, Codex and Gemini
/// are given. OpenCode's own default would already permit them, but a
/// user config that asks for everything would not.
fn opencode_permission_config(mode: PermissionMode, mcp: bool) -> Option<serde_json::Value> {
    let mut rules = match mode {
        PermissionMode::Auto => serde_json::json!({
            "edit": "allow",
            "bash": "ask",
            "webfetch": "ask",
        }),
        PermissionMode::Default | PermissionMode::Inherit => serde_json::json!({
            "edit": "ask",
            "bash": "ask",
            "webfetch": "ask",
        }),
        PermissionMode::Bypass => return None,
    };
    if mcp {
        rules[format!("{MCP_SERVER_NAME}_*")] = serde_json::json!("allow");
    }
    Some(rules)
}

/// `--auto` approves everything not explicitly denied; the other modes
/// carry their rules in the config instead.
fn opencode_permission_args(mode: PermissionMode) -> Vec<String> {
    match mode {
        PermissionMode::Bypass => vec!["--auto".into()],
        _ => vec![],
    }
}

/// `-m provider/model`. A redirected entry runs on our own provider
/// block; otherwise the entry's model already names one of OpenCode's.
fn opencode_model_args(endpoint: Option<&ResolvedModelEndpoint>) -> Vec<String> {
    let Some(endpoint) = endpoint else {
        return vec![];
    };
    if endpoint.model.is_empty() {
        return vec![];
    }
    let model = if endpoint.redirects_traffic() {
        format!("{OPENCODE_PROVIDER_ID}/{}", endpoint.model)
    } else {
        endpoint.model.clone()
    };
    vec!["-m".into(), model]
}

impl OpenCodeAdapter {
    fn config_path(ctx: &SpawnCtx) -> PathBuf {
        ctx.integration.files_dir.join(format!(
            "session-{}-opencode.json",
            ctx.integration.session_id
        ))
    }

    /// Writes the per-session plugin and config. OpenCode has no flag
    /// for either, so the config reaches it through the environment and
    /// names the plugin by absolute path.
    fn write_session_files(&self, ctx: &SpawnCtx) -> Result<(), AdapterError> {
        let plugin_path = ctx.integration.files_dir.join(format!(
            "session-{}-opencode-plugin.js",
            ctx.integration.session_id
        ));
        write_session_file(&plugin_path, opencode_plugin_js(&ctx.integration.pm_exe))?;
        let instructions = write_instruction_file(ctx)?;
        let mcp = ctx
            .integration
            .mcp_url
            .as_deref()
            .map(|url| (url, ctx.integration.session_token.as_str()));
        write_session_file(
            &Self::config_path(ctx),
            opencode_config_json(
                &plugin_path,
                instructions.as_deref(),
                mcp,
                opencode_permission_config(ctx.permission_mode, mcp.is_some()),
                ctx.model_endpoint.as_ref(),
            ),
        )?;
        Ok(())
    }

    /// The flags shared by spawn and resume; writes the session files.
    /// `--pure` is never passed: it runs OpenCode without external
    /// plugins, which would drop every lifecycle event.
    fn integration_args(&self, ctx: &SpawnCtx) -> Result<Vec<String>, AdapterError> {
        self.write_session_files(ctx)?;
        let mut args = opencode_permission_args(ctx.permission_mode);
        args.extend(opencode_model_args(ctx.model_endpoint.as_ref()));
        Ok(args)
    }

    fn env(&self, ctx: &SpawnCtx) -> Vec<(String, String)> {
        let mut env = vec![(
            ENV_OPENCODE_CONFIG.into(),
            Self::config_path(ctx).display().to_string(),
        )];
        if let Some(endpoint) = &ctx.model_endpoint {
            if endpoint.redirects_traffic() && !endpoint.api_key.is_empty() {
                env.push((ENV_MODEL_API_KEY.into(), endpoint.api_key.clone()));
            }
        }
        env
    }
}

impl AgentAdapter for OpenCodeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn submits_after_bracketed_paste(&self) -> bool {
        true
    }

    /// OpenCode is bring-your-own-key across providers, so one adapter
    /// serves both endpoint shapes the other agents split between them.
    fn dialects(&self) -> &'static [ModelDialect] {
        &[
            ModelDialect::AnthropicMessages,
            ModelDialect::OpenaiResponses,
        ]
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    fn wants_agent_port(&self) -> bool {
        true
    }

    fn inbound_channel(&self, facts: &InboundFacts) -> Option<InboundChannel> {
        let port = facts.agent_port?;
        let session = facts.agent_session_id.clone()?;
        Some(InboundChannel::OpenCodeHttp {
            base_url: format!("http://127.0.0.1:{port}"),
            session,
        })
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        let mut args = self.integration_args(ctx)?;
        // The TUI serves its session API on a port it picks at random
        // unless it is told one. Pinning it is what makes the session
        // addressable afterwards.
        if let Some(port) = ctx.integration.agent_port {
            args.push("--hostname".into());
            args.push(OPENCODE_API_HOST.into());
            args.push("--port".into());
            args.push(port.to_string());
        }
        // The positional argument is the project directory, so an
        // initial message rides on --prompt rather than argv's tail.
        if !ctx.task_prompt.is_empty() {
            args.push("--prompt".into());
            args.push(ctx.task_prompt.clone());
        }
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: "opencode".into(),
                args,
                env: self.env(ctx),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            // OpenCode assigns the session id itself and reports it in
            // the session.created event the plugin forwards.
            agent_session_id: None,
            detect_osc9_needs_input: false,
        })
    }

    fn resume_command(
        &self,
        ctx: &SpawnCtx,
        agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        let mut args = self.integration_args(ctx)?;
        args.push("--session".into());
        args.push(agent_session_id.to_string());
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: "opencode".into(),
                args,
                env: self.env(ctx),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: Some(agent_session_id.to_string()),
            detect_osc9_needs_input: false,
        })
    }
}

pub struct AntigravityAdapter;

/// Antigravity's CLI has no flag for a settings, hook or MCP file, and
/// its global config lives under the home directory next to the login
/// it shares with the IDE. Its customization files are instead read out
/// of every workspace directory it is given beyond the first, so a
/// per-session directory passed with `--add-dir` carries our hooks, our
/// reporting server and the compiled instructions without touching the
/// user's own configuration or the project checkout.
fn antigravity_hooks_json(pm_exe: &std::path::Path) -> String {
    // Handlers for the events that take no matcher are flat. The
    // nested `hooks` array Claude and Gemini use is rejected at parse
    // with "command hook must specify 'command'", which disables every
    // hook in the file rather than just the malformed one.
    let hook = |kind: &str| {
        serde_json::json!([{
            "type": "command",
            "command": hook_shell_command(pm_exe, kind),
        }])
    };
    serde_json::json!({
        MCP_SERVER_NAME: {
            "SessionStart": hook("started"),
            "PreInvocation": hook("prompt-submitted"),
            "Stop": hook("turn-ended"),
        }
    })
    .to_string()
}

/// The reporting server as an Antigravity MCP entry. The CLI reads MCP
/// servers only from the user's own file, so the entry cannot carry a
/// session's endpoint or token: it launches `pm _mcp`, a stdio bridge
/// that reads both out of the environment the session gave the agent.
/// A stdio server inherits that environment; an HTTP entry's headers are
/// literal text, so a token could not reach one.
fn antigravity_mcp_entry(pm_exe: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "command": pm_exe.display().to_string(),
        "args": ["_mcp"],
    })
}

/// Registers the reporting server in the user's own MCP file, adding
/// only our entry and leaving every other server and any unknown field
/// as they were. The write goes through a temporary file in the same
/// directory so a spawn racing another one cannot leave the user with a
/// half-written config, and is skipped entirely when the entry is
/// already what it should be.
fn register_antigravity_mcp_server(pm_exe: &std::path::Path) -> Result<(), AdapterError> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "HOME is unset, so the Antigravity MCP config cannot be found",
        )
    })?;
    let path = PathBuf::from(home).join(ANTIGRAVITY_MCP_CONFIG);
    let mut config = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(|value| value.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    let entry = antigravity_mcp_entry(pm_exe);
    if config["mcpServers"][MCP_SERVER_NAME] == entry {
        return Ok(());
    }
    if !config["mcpServers"].is_object() {
        config["mcpServers"] = serde_json::json!({});
    }
    config["mcpServers"][MCP_SERVER_NAME] = entry;
    let dir = path.parent().expect("the config path has a directory");
    std::fs::create_dir_all(dir)?;
    let staged = dir.join(format!(
        ".{MCP_SERVER_NAME}-mcp-{}.json",
        std::process::id()
    ));
    std::fs::write(&staged, config.to_string())?;
    std::fs::rename(&staged, &path)?;
    Ok(())
}

/// Antigravity's execution-mode and permission flags for our
/// agent-agnostic modes. Default mode passes nothing, leaving the CLI on
/// its configured `toolPermission` policy, which asks.
fn antigravity_permission_args(mode: PermissionMode) -> Vec<String> {
    match mode {
        PermissionMode::Auto => vec!["--mode".into(), "accept-edits".into()],
        PermissionMode::Bypass => vec!["--dangerously-skip-permissions".into()],
        PermissionMode::Default | PermissionMode::Inherit => vec![],
    }
}

impl AntigravityAdapter {
    /// The customization directory handed to `--add-dir`. The files the
    /// CLI reads sit in the `.agents` subdirectory of it.
    fn session_dir(ctx: &SpawnCtx) -> PathBuf {
        ctx.integration.files_dir.join(format!(
            "session-{}-antigravity",
            ctx.integration.session_id
        ))
    }

    /// Writes the per-session customization tree and returns the flags
    /// shared by spawn and resume.
    fn integration_args(&self, ctx: &SpawnCtx) -> Result<Vec<String>, AdapterError> {
        let dir = Self::session_dir(ctx).join(ANTIGRAVITY_CUSTOMIZATION_DIR);
        create_session_dir(&dir)?;
        write_session_file(
            &dir.join(ANTIGRAVITY_HOOKS_FILE),
            antigravity_hooks_json(&ctx.integration.pm_exe),
        )?;
        if ctx.integration.mcp_url.is_some() {
            register_antigravity_mcp_server(&ctx.integration.pm_exe)?;
        }
        if !ctx.compiled_instructions.is_empty() {
            let rules = dir.join(ANTIGRAVITY_RULES_DIR);
            create_session_dir(&rules)?;
            write_session_file(
                &rules.join(ANTIGRAVITY_INSTRUCTIONS_FILE),
                &ctx.compiled_instructions,
            )?;
        }
        let mut args = vec![
            "--add-dir".into(),
            Self::session_dir(ctx).display().to_string(),
        ];
        args.extend(antigravity_permission_args(ctx.permission_mode));
        Ok(args)
    }
}

impl AntigravityAdapter {
    /// The endpoint the stdio bridge posts to; the token it pairs with
    /// is already in every managed session's environment.
    fn env(ctx: &SpawnCtx) -> Vec<(String, String)> {
        ctx.integration
            .mcp_url
            .iter()
            .map(|url| (ENV_MCP_URL.to_string(), url.clone()))
            .collect()
    }
}

impl AgentAdapter for AntigravityAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Antigravity
    }

    fn submits_after_bracketed_paste(&self) -> bool {
        true
    }

    /// A model profile entry cannot reach this CLI: redirecting it to
    /// another endpoint also takes a `modelProvider` setting, and that
    /// lives only in the user's global settings file, which a session
    /// must not rewrite. Sessions run on the agent's own account.
    fn dialects(&self) -> &'static [ModelDialect] {
        &[]
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        let mut args = self.integration_args(ctx)?;
        // `--prompt-interactive` sends the first message and stays in the
        // TUI; `--print` would run one turn and exit.
        if let Some(prompt) = prompt_with_reporting(ctx) {
            args.push("--prompt-interactive".into());
            args.push(prompt);
        }
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: AgentKind::Antigravity.program().into(),
                args,
                env: Self::env(ctx),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            // The CLI assigns the conversation id and has no flag to
            // choose one; every hook payload reports it, so the session
            // becomes resumable once the first hook arrives.
            agent_session_id: None,
            // The TUI writes OSC9 progress updates continuously, so the
            // marker cannot stand for an approval pause here.
            detect_osc9_needs_input: false,
        })
    }

    fn resume_command(
        &self,
        ctx: &SpawnCtx,
        agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        let mut args = self.integration_args(ctx)?;
        args.push("--conversation".into());
        args.push(agent_session_id.to_string());
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: AgentKind::Antigravity.program().into(),
                args,
                env: Self::env(ctx),
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: Some(agent_session_id.to_string()),
            detect_osc9_needs_input: false,
        })
    }
}

/// Runs an explicit binary (the scripted test agent); registered only
/// by the test suite.
pub struct TestAgentAdapter {
    pub program: PathBuf,
}

/// Points the scripted agent's inbound channel at a directory a test
/// binds its socket in, so the delivery path can be exercised without a
/// real agent. The socket inside it is named the way Claude Code names
/// its own, after the pid of the agent's PTY child, so a test exercises
/// the same resolution a real session does.
pub const TEST_INBOX_DIR_ENV: &str = "PM_TEST_INBOX_DIR";

impl AgentAdapter for TestAgentAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Test
    }

    /// The scripted agent has no inbox of its own, so a test names the
    /// directory one is bound in and this resolves the socket within it
    /// exactly as the Claude adapter does.
    fn inbound_channel(&self, facts: &InboundFacts) -> Option<InboundChannel> {
        let dir = std::env::var(TEST_INBOX_DIR_ENV)
            .ok()
            .filter(|p| !p.is_empty())?;
        let pid = facts.agent_pid?;
        Some(InboundChannel::ClaudeSocket {
            path: claude_inbox_path(std::path::Path::new(&dir), pid),
            token: None,
            // A test binds that socket in its own process, so the peer a
            // delivery will find there is this process.
            expect_pid: std::process::id(),
        })
    }

    fn dialects(&self) -> &'static [ModelDialect] {
        &[ModelDialect::AnthropicMessages]
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: self.program.display().to_string(),
                args: vec![ctx.task_prompt.clone()],
                env: vec![],
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: Some(format!("testsess-{}", ctx.integration.session_id)),
            detect_osc9_needs_input: false,
        })
    }

    fn resume_command(
        &self,
        ctx: &SpawnCtx,
        agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: self.program.display().to_string(),
                args: vec![format!("resumed:{agent_session_id}")],
                env: vec![],
                cwd: ctx.cwd.clone(),
            }
            .with_integration_env(&ctx.integration),
            agent_session_id: Some(agent_session_id.to_string()),
            detect_osc9_needs_input: false,
        })
    }
}

/// Picks the entry an adapter can speak to, in the adapter's own
/// preference order. The error carries the dialects the profile does
/// cover so the caller can name them; a spawn never falls back to the
/// agent's default account.
pub fn select_endpoint<'a>(
    adapter: &dyn AgentAdapter,
    entries: &'a [ModelProfileEndpoint],
) -> Result<&'a ModelProfileEndpoint, Vec<ModelDialect>> {
    for dialect in adapter.dialects() {
        if let Some(entry) = entries.iter().find(|entry| entry.dialect == *dialect) {
            return Ok(entry);
        }
    }
    let mut covered: Vec<ModelDialect> = entries.iter().map(|entry| entry.dialect).collect();
    covered.sort();
    Err(covered)
}

pub struct AdapterRegistry {
    adapters: HashMap<AgentKind, Box<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    /// The production set: real agent CLIs only.
    pub fn standard() -> Self {
        let mut r = Self {
            adapters: HashMap::new(),
        };
        r.register(Box::new(ClaudeCodeAdapter));
        r.register(Box::new(CodexAdapter));
        r.register(Box::new(GeminiAdapter));
        r.register(Box::new(OpenCodeAdapter));
        r.register(Box::new(AntigravityAdapter));
        r
    }

    pub fn empty() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    pub fn register(&mut self, adapter: Box<dyn AgentAdapter>) {
        self.adapters.insert(adapter.kind(), adapter);
    }

    /// The kinds with a registered adapter, in a stable order. Callers
    /// that publish the set of spawnable agents read it from here so a
    /// newly registered adapter cannot be missing from what they claim.
    pub fn kinds(&self) -> Vec<AgentKind> {
        let mut kinds: Vec<AgentKind> = self.adapters.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }

    pub fn adapters(&self) -> impl Iterator<Item = &dyn AgentAdapter> {
        self.kinds()
            .into_iter()
            .filter_map(|kind| self.get(kind).ok())
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn get(&self, kind: AgentKind) -> Result<&dyn AgentAdapter, AdapterError> {
        self.adapters
            .get(&kind)
            .map(|a| a.as_ref())
            .ok_or(AdapterError::UnknownAgent(kind.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward(slug: &str, worker_port: u16, url: &str, reachable: Option<bool>) -> SessionForward {
        SessionForward {
            id: 1,
            session_id: 7,
            worker_port,
            listener_port: 41000,
            slug: slug.into(),
            label: String::new(),
            scheme: "http".into(),
            created_at_unix_ms: 0,
            url: url.into(),
            target_reachable: reachable,
            source_path: String::new(),
        }
    }

    #[test]
    fn a_session_with_no_forwards_gets_no_inventory() {
        assert_eq!(forward_inventory(&[]), "");
    }

    #[test]
    fn the_inventory_names_every_forward_and_ends_with_the_instruction() {
        let text = forward_inventory(&[
            forward(
                "docs-preview",
                8080,
                "https://docs-preview.example/",
                Some(true),
            ),
            forward("api", 9000, "https://api.example/", Some(false)),
        ]);
        assert!(text.starts_with(FORWARD_INVENTORY_PREAMBLE), "{text}");
        assert!(text.contains("docs-preview, local port 8080, at https://docs-preview.example/"));
        assert!(text.contains("api, local port 9000, at https://api.example/"));
        assert!(text.ends_with(FORWARD_INVENTORY_INSTRUCTION), "{text}");
    }

    #[test]
    fn each_probe_state_reads_differently() {
        let answered = forward_inventory(&[forward("a", 1, "u", Some(true))]);
        let refused = forward_inventory(&[forward("a", 1, "u", Some(false))]);
        let untried = forward_inventory(&[forward("a", 1, "u", None)]);
        assert!(
            answered.contains("the last probe of that port answered"),
            "{answered}"
        );
        assert!(
            refused.contains("the last probe of that port was not answered"),
            "{refused}"
        );
        assert!(
            untried.contains("that port has not been probed yet"),
            "{untried}"
        );
        assert_ne!(answered, refused);
        assert_ne!(refused, untried);
    }

    #[test]
    fn a_forward_with_no_slug_or_url_is_still_listed() {
        let text = forward_inventory(&[forward("", 8080, "", None)]);
        assert!(
            text.contains("unnamed, local port 8080, no public URL"),
            "{text}"
        );
    }

    #[test]
    fn only_claude_carries_session_start_context_itself() {
        for adapter in AdapterRegistry::standard().adapters() {
            let claude = adapter.kind() == AgentKind::ClaudeCode;
            assert_eq!(
                adapter.injects_session_start_context(),
                claude,
                "{} reports the wrong session-start context capability",
                adapter.kind().as_str()
            );
        }
        assert!(!TestAgentAdapter {
            program: PathBuf::from("/build/testagent")
        }
        .injects_session_start_context());
    }

    fn ctx(files_dir: PathBuf) -> SpawnCtx {
        SpawnCtx {
            cwd: PathBuf::from("/tmp/proj"),
            task_prompt: "fix the tests".into(),
            permission_mode: PermissionMode::Default,
            compiled_instructions: REPORTING_BRIEF.into(),
            model_endpoint: None,
            fullscreen: false,
            integration: Integration {
                session_id: 7,
                session_token: "tok-abc".into(),
                socket_path: PathBuf::from("/run/pm.sock"),
                pm_exe: PathBuf::from("/usr/local/bin/pm"),
                files_dir,
                mcp_url: None,
                agent_port: None,
            },
        }
    }

    /// The trust answer rides only with the mode that has already given
    /// away what the gate protects. Setting it more widely would let a
    /// repository grant itself permissions a prompting session never
    /// agreed to.
    #[test]
    fn claude_answers_the_trust_question_only_when_permissions_are_already_bypassed() {
        let files = tempfile::tempdir().unwrap();
        let mut c = ctx(files.path().to_path_buf());
        c.permission_mode = PermissionMode::Bypass;
        let spec = ClaudeCodeAdapter.spawn_command(&c).unwrap().spec;
        assert!(
            spec.env
                .iter()
                .any(|(k, v)| k == "CLAUDE_CODE_SANDBOXED" && v == "1"),
            "{:?}",
            spec.env
        );
        assert!(spec
            .args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));

        for mode in [
            PermissionMode::Default,
            PermissionMode::Auto,
            PermissionMode::Inherit,
        ] {
            let mut c = ctx(files.path().to_path_buf());
            c.permission_mode = mode;
            let spec = ClaudeCodeAdapter.spawn_command(&c).unwrap().spec;
            assert!(
                !spec.env.iter().any(|(k, _)| k == "CLAUDE_CODE_SANDBOXED"),
                "{mode:?} must keep the gate: {:?}",
                spec.env
            );
        }
    }

    /// A spawned session has nobody to answer Codex's trust question, so
    /// the answer has to be on disk before it asks.
    #[test]
    fn codex_trust_is_recorded_once_and_leaves_the_rest_of_the_config_alone() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        std::fs::write(&config, "model = \"gpt-5\"\n").unwrap();

        let cwd = PathBuf::from("/tmp/a worktree/with \"quotes\"");
        trust_project_dir_in(home.path(), &cwd).unwrap();
        let written = std::fs::read_to_string(&config).unwrap();
        assert!(written.starts_with("model = \"gpt-5\"\n"), "{written}");
        assert!(
            written.contains(&format!(
                "[projects.{}]",
                toml_string(&cwd.display().to_string())
            )),
            "{written}"
        );
        assert!(written.contains("trust_level = \"trusted\""), "{written}");

        // Spawning again in the same directory must not keep appending.
        trust_project_dir_in(home.path(), &cwd).unwrap();
        let again = std::fs::read_to_string(&config).unwrap();
        assert_eq!(written, again);
    }

    /// A first run has no config at all, and the trust answer must not
    /// depend on one already existing.
    #[test]
    fn codex_trust_creates_the_config_when_there_is_none() {
        let home = tempfile::tempdir().unwrap();
        let nested = home.path().join("codex");
        let cwd = PathBuf::from("/tmp/fresh");
        trust_project_dir_in(&nested, &cwd).unwrap();
        let written = std::fs::read_to_string(nested.join("config.toml")).unwrap();
        assert!(written.contains("[projects.\"/tmp/fresh\"]"), "{written}");
        assert!(written.contains("trust_level = \"trusted\""), "{written}");
    }

    fn settings_written_for(spec: &CommandSpec) -> serde_json::Value {
        let at = spec.args.iter().position(|a| a == "--settings").unwrap();
        serde_json::from_str(&std::fs::read_to_string(&spec.args[at + 1]).unwrap()).unwrap()
    }

    /// The default keeps Claude out of the alternate screen so its
    /// output stays in the PTY scrollback. The env answer is absolute;
    /// asking for fullscreen instead states the renderer in the session
    /// settings, so the choice does not depend on the user's own config.
    #[test]
    fn the_renderer_setting_decides_claudes_alternate_screen_either_way() {
        let files = tempfile::tempdir().unwrap();
        let classic = ClaudeCodeAdapter
            .spawn_command(&ctx(files.path().to_path_buf()))
            .unwrap()
            .spec;
        assert_eq!(
            env_of(&classic)["CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN"],
            "1"
        );
        assert!(settings_written_for(&classic).get("tui").is_none());

        let mut c = ctx(files.path().to_path_buf());
        c.fullscreen = true;
        let full = ClaudeCodeAdapter.spawn_command(&c).unwrap().spec;
        assert!(
            !full
                .env
                .iter()
                .any(|(k, _)| k == "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN"),
            "{:?}",
            full.env
        );
        assert_eq!(settings_written_for(&full)["tui"], "fullscreen");
    }

    /// A resumed session renders the same way a fresh one does.
    #[test]
    fn claude_resume_carries_the_alternate_screen_choice() {
        let files = tempfile::tempdir().unwrap();
        let classic = ClaudeCodeAdapter
            .resume_command(&ctx(files.path().to_path_buf()), "sess-1")
            .unwrap()
            .spec;
        assert_eq!(
            env_of(&classic)["CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN"],
            "1"
        );

        let mut c = ctx(files.path().to_path_buf());
        c.fullscreen = true;
        let full = ClaudeCodeAdapter.resume_command(&c, "sess-1").unwrap().spec;
        assert!(!full
            .env
            .iter()
            .any(|(k, _)| k == "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN"));
    }

    fn ctx_empty_prompt(files_dir: PathBuf) -> SpawnCtx {
        let mut c = ctx(files_dir);
        c.task_prompt = String::new();
        c
    }

    #[test]
    fn empty_prompt_starts_agents_with_no_positional() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = ClaudeCodeAdapter
            .spawn_command(&ctx_empty_prompt(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        assert!(!claude.args.iter().any(|a| a == "--"));
        assert!(claude.args.iter().all(|a| a != "fix the tests"));
        // Last arg is the settings path, not a prompt.
        assert!(claude.args.iter().any(|a| a == "--settings"));

        let codex = CodexAdapter
            .spawn_command(&ctx_empty_prompt(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        assert_eq!(
            codex.args.last().unwrap(),
            "--dangerously-bypass-hook-trust"
        );
    }

    fn ctx_with_mcp(files_dir: PathBuf) -> SpawnCtx {
        let mut c = ctx(files_dir);
        c.integration.mcp_url = Some("http://127.0.0.1:7676/mcp".into());
        c
    }

    /// Runs `body` with `HOME` pointed at a scratch directory. The
    /// environment is process-wide, so the tests that need it take one
    /// lock rather than racing each other's home.
    fn with_home<T>(home: &std::path::Path, body: impl FnOnce() -> T) -> T {
        static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = body();
        match previous {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        drop(guard);
        out
    }

    fn env_of(spec: &CommandSpec) -> std::collections::HashMap<&str, &str> {
        spec.env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    #[test]
    fn claude_gets_settings_file_with_all_lifecycle_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = ClaudeCodeAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap();
        let spec = plan.spec;

        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args[0], "--settings");
        assert_eq!(spec.args.last().unwrap(), "fix the tests");
        // The prompt must be positional (after --), never consumed by a
        // preceding variadic flag.
        assert_eq!(spec.args[spec.args.len() - 2], "--");
        assert!(!spec.args.iter().any(|a| a == "--session-id"));
        assert_eq!(plan.agent_session_id, None);

        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&spec.args[1]).unwrap()).unwrap();
        // The reporting server's tools are pre-approved so they never
        // prompt in default/auto permission mode.
        let allow = settings["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|r| r == "mcp__puppet-master"), "{allow:?}");
        for (hook, kind) in [
            ("SessionStart", "session-start"),
            ("Notification", "needs-input"),
            ("Stop", "turn-ended"),
            ("StopFailure", "turn-failed"),
            ("UserPromptSubmit", "prompt-submitted"),
        ] {
            let command = settings["hooks"][hook][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert_eq!(
                command,
                format!("/usr/local/bin/pm _hook {kind} --agent claude")
            );
        }

        let env = env_of(&spec);
        assert_eq!(env[ENV_SESSION_TOKEN], "tok-abc");
        assert_eq!(env[ENV_SOCKET], "/run/pm.sock");
    }

    /// The observed field failure: a pm binary under a macOS shared
    /// folder path with spaces produced `/bin/sh: /Volumes/My: No such
    /// file or directory` from every lifecycle hook, so the session
    /// stayed working forever.
    #[test]
    fn hook_commands_shell_quote_a_pm_path_with_spaces() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path().to_path_buf());
        c.integration.pm_exe = PathBuf::from("/Volumes/My Shared Files/bin/pm");

        let spec = ClaudeCodeAdapter.spawn_command(&c).unwrap().spec;
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&spec.args[1]).unwrap()).unwrap();
        let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(
            command,
            "'/Volumes/My Shared Files/bin/pm' _hook turn-ended --agent claude"
        );

        let codex = CodexAdapter.spawn_command(&c).unwrap().spec;
        let hook = config_value(&codex.args, "hooks.Stop=").unwrap();
        assert!(
            hook.contains("command=\"'/Volumes/My Shared Files/bin/pm' _hook turn-ended\""),
            "{hook}"
        );
    }

    #[test]
    fn shell_quote_passes_neutral_values_and_survives_metacharacters() {
        assert_eq!(shell_quote("/usr/local/bin/pm"), "/usr/local/bin/pm");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(shell_quote(""), "''");
        assert!(path_needs_shell_quoting(std::path::Path::new(
            "/Volumes/My Shared Files/pm"
        )));
        assert!(!path_needs_shell_quoting(std::path::Path::new(
            "/usr/local/bin/pm"
        )));
    }

    #[test]
    fn toml_string_escapes_quotes_backslashes_and_control_characters() {
        assert_eq!(toml_string("plain"), "\"plain\"");
        assert_eq!(toml_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(
            toml_string("line1\nline2\r\tend"),
            "\"line1\\nline2\\r\\tend\""
        );
        assert_eq!(toml_string("bell\x07"), "\"bell\\u0007\"");
    }

    /// Every per-session file an adapter writes carries something worth
    /// reading: the session's MCP bearer token, which is its whole authority
    /// over the controller, or the compiled instruction overlays. The worker's
    /// runtime directory is owner-only, and these are owner-only inside it too,
    /// so a directory later widened, or a copy that preserves modes, does not
    /// hand them out.
    #[cfg(unix)]
    #[test]
    fn every_session_file_an_adapter_writes_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        fn entries_under(dir: &std::path::Path, found: &mut Vec<(String, u32)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
                found.push((entry.path().display().to_string(), mode));
                if entry.file_type().unwrap().is_dir() {
                    entries_under(&entry.path(), found);
                }
            }
        }

        let adapters: [(&str, &dyn AgentAdapter); 4] = [
            ("claude", &ClaudeCodeAdapter),
            ("gemini", &GeminiAdapter),
            ("opencode", &OpenCodeAdapter),
            ("antigravity", &AntigravityAdapter),
        ];
        let home = tempfile::tempdir().unwrap();
        for (label, adapter) in adapters {
            let tmp = tempfile::tempdir().unwrap();
            let built = with_home(home.path(), || {
                adapter.spawn_command(&ctx_with_mcp(tmp.path().to_path_buf()))
            });
            assert!(built.is_ok(), "{label} could not build a spawn");
            let mut found = Vec::new();
            entries_under(tmp.path(), &mut found);
            assert!(
                !found.is_empty(),
                "{label} wrote no session files, so this asserts nothing"
            );
            for (path, mode) in found {
                assert_eq!(
                    mode & 0o077,
                    0,
                    "{label} left {path} at {mode:o}, reachable beyond its owner"
                );
            }
        }
    }

    #[test]
    fn claude_keeps_task_prompt_separate_and_injects_private_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = ClaudeCodeAdapter
            .spawn_command(&ctx_with_mcp(tmp.path().to_path_buf()))
            .unwrap()
            .spec;

        let mcp_flag = spec.args.iter().position(|a| a == "--mcp-config").unwrap();
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&spec.args[mcp_flag + 1]).unwrap())
                .unwrap();
        let server = &config["mcpServers"]["puppet-master"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "http://127.0.0.1:7676/mcp");
        assert_eq!(server["headers"]["Authorization"], "Bearer tok-abc");

        assert_eq!(spec.args.last().unwrap(), "fix the tests");
        let instruction_flag = spec
            .args
            .iter()
            .position(|a| a == "--append-system-prompt-file")
            .unwrap();
        let instructions = std::fs::read_to_string(&spec.args[instruction_flag + 1]).unwrap();
        assert!(instructions.contains("flag_blocked"), "{instructions}");
        assert_eq!(instructions, with_claude_harness_brief(REPORTING_BRIEF));
    }

    #[test]
    fn claude_gets_the_harness_brief_even_without_compiled_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path().to_path_buf());
        c.compiled_instructions = String::new();
        let spec = ClaudeCodeAdapter.spawn_command(&c).unwrap().spec;
        let flag = spec
            .args
            .iter()
            .position(|a| a == "--append-system-prompt-file")
            .unwrap();
        let instructions = std::fs::read_to_string(&spec.args[flag + 1]).unwrap();
        assert!(instructions.starts_with(REPORTING_BRIEF), "{instructions}");
        assert!(
            instructions.ends_with(CLAUDE_HARNESS_BRIEF),
            "{instructions}"
        );
    }

    #[test]
    fn only_claude_carries_the_harness_brief_and_hook_agent_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path().to_path_buf());
        let codex = CodexAdapter.spawn_command(&c).unwrap().spec;
        let hook = config_value(&codex.args, "hooks.Stop=").unwrap();
        assert!(!hook.contains("--agent"), "{hook}");
        let gemini = GeminiAdapter.spawn_command(&c).unwrap().spec;
        let settings = gemini_settings_of(&gemini);
        let command = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(!command.contains("--agent"), "{command}");
        let codex_instructions =
            std::fs::read_to_string(c.integration.files_dir.join("session-7-instructions.md"))
                .unwrap();
        assert_eq!(codex_instructions, REPORTING_BRIEF);
    }

    #[test]
    fn claude_harness_brief_forbids_unrequested_artifacts_and_points_at_forwards() {
        for guidance in [
            "Do not publish a claude.ai artifact",
            "unless the user explicitly asks for one",
            "`publish_dir`",
            "give them the URL it returns",
            "a request for a \
             published directory, not an artifact",
        ] {
            assert!(
                CLAUDE_HARNESS_BRIEF.contains(guidance),
                "harness brief omits: {guidance}"
            );
        }
        let composed = with_claude_harness_brief("## Puppet Master contract\n\nreport often.");
        assert!(
            composed.starts_with("## Puppet Master contract"),
            "{composed}"
        );
        assert!(
            composed.contains("\n\n## Claude Code harness\n\n"),
            "{composed}"
        );
        assert!(composed.ends_with(CLAUDE_HARNESS_BRIEF), "{composed}");
        assert!(with_claude_harness_brief("  ").starts_with(REPORTING_BRIEF));
    }

    #[test]
    fn claude_without_mcp_leaves_prompt_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = ClaudeCodeAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        assert!(!spec.args.iter().any(|a| a == "--mcp-config"));
        assert_eq!(spec.args.last().unwrap(), "fix the tests");
    }

    #[test]
    fn claude_resume_uses_native_resume_with_integration_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = ClaudeCodeAdapter
            .resume_command(
                &ctx_with_mode(tmp.path().to_path_buf(), PermissionMode::Bypass),
                "uuid-123",
            )
            .unwrap();
        let resume_flag = plan.spec.args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(plan.spec.args[resume_flag + 1], "uuid-123");
        assert!(plan.spec.args.iter().any(|a| a == "--settings"));
        assert!(plan
            .spec
            .args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));
        assert!(!plan.spec.args.iter().any(|a| a == "--session-id"));
        assert_eq!(plan.agent_session_id.as_deref(), Some("uuid-123"));
    }

    /// Returns the value following the flag with the given prefix.
    fn config_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
        args.iter()
            .zip(args.iter().skip(1))
            .find(|(flag, val)| *flag == "-c" && val.starts_with(key))
            .map(|(_, val)| val.as_str())
    }

    #[test]
    fn codex_injects_mcp_hooks_and_trust_bypass() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = CodexAdapter
            .spawn_command(&ctx_with_mcp(tmp.path().to_path_buf()))
            .unwrap();
        let spec = plan.spec;
        assert_eq!(spec.program, "codex");
        assert_eq!(plan.agent_session_id, None);

        assert_eq!(
            config_value(&spec.args, "mcp_servers.puppet-master.url="),
            Some("mcp_servers.puppet-master.url=\"http://127.0.0.1:7676/mcp\"")
        );
        assert!(config_value(
            &spec.args,
            "mcp_servers.puppet-master.bearer_token_env_var="
        )
        .is_some());
        assert_eq!(
            config_value(
                &spec.args,
                "mcp_servers.puppet-master.default_tools_approval_mode="
            ),
            Some("mcp_servers.puppet-master.default_tools_approval_mode=\"approve\"")
        );
        for (event, kind) in [
            ("Stop", "turn-ended"),
            ("Interrupt", "turn-failed"),
            ("Notification", "needs-input"),
            ("UserPromptSubmit", "prompt-submitted"),
        ] {
            let hook = config_value(&spec.args, &format!("hooks.{event}=")).unwrap();
            assert!(
                hook.contains(&format!("/usr/local/bin/pm _hook {kind}")),
                "{hook}"
            );
        }
        assert!(spec
            .args
            .iter()
            .any(|a| a == "--dangerously-bypass-hook-trust"));

        // The prompt is the trailing positional, left verbatim; the
        // reporting brief rides in developer_instructions instead.
        let prompt = spec.args.last().unwrap();
        assert_eq!(prompt, "fix the tests");
        let brief = config_value(&spec.args, "developer_instructions=").unwrap();
        assert!(brief.contains("report"), "{brief}");
        assert!(brief.contains("flag_blocked"), "{brief}");
        assert_eq!(env_of(&spec)[ENV_SESSION_TOKEN], "tok-abc");
    }

    #[test]
    fn codex_without_mcp_still_injects_hooks_and_plain_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = CodexAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        assert!(config_value(&spec.args, "mcp_servers").is_none());
        assert!(config_value(&spec.args, "hooks.Stop=").is_some());
        assert!(config_value(&spec.args, "hooks.Interrupt=").is_some());
        assert_eq!(spec.args.last().unwrap(), "fix the tests");
    }

    #[test]
    fn codex_resume_uses_resume_subcommand_with_integration() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = CodexAdapter
            .resume_command(&ctx_with_mcp(tmp.path().to_path_buf()), "uuid-9")
            .unwrap();
        let resume_at = plan.spec.args.iter().position(|a| a == "resume").unwrap();
        assert_eq!(plan.spec.args[resume_at + 1], "uuid-9");
        // Global flags precede the subcommand.
        assert!(plan.spec.args[..resume_at].iter().any(|a| a == "-c"));
        assert!(plan.spec.args[..resume_at]
            .iter()
            .any(|a| a == "--dangerously-bypass-hook-trust"));
        let hook = config_value(&plan.spec.args, "hooks.Interrupt=").unwrap();
        assert!(hook.contains("_hook turn-failed"));
        assert!(hook.contains(&format!("timeout={CODEX_INTERRUPT_HOOK_TIMEOUT_SECONDS}")));
        assert_eq!(plan.agent_session_id.as_deref(), Some("uuid-9"));
    }

    fn gemini_settings_of(spec: &CommandSpec) -> serde_json::Value {
        let path = env_of(spec)[ENV_GEMINI_SYSTEM_SETTINGS_PATH].to_string();
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn gemini_gets_a_system_settings_file_with_all_lifecycle_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = GeminiAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap();
        let spec = plan.spec;

        assert_eq!(spec.program, "gemini");
        // Without --skip-trust Gemini refuses to start in a directory it
        // has not been told to trust, so the session dies at launch.
        assert_eq!(spec.args[0], "--skip-trust");
        assert_eq!(spec.args.last().unwrap(), "fix the tests");
        let dashdash = spec.args.iter().position(|a| a == "--").unwrap();
        assert_eq!(dashdash, spec.args.len() - 2);

        // There is no --settings flag; the file reaches Gemini as the
        // System Overrides settings layer.
        let settings = gemini_settings_of(&spec);
        for (hook, kind) in [
            ("SessionStart", "session-start"),
            ("Notification", "needs-input"),
            ("BeforeAgent", "prompt-submitted"),
            ("AfterAgent", "turn-ended"),
        ] {
            let command = settings["hooks"][hook][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert_eq!(command, format!("/usr/local/bin/pm _hook {kind}"));
        }
        // Gemini has no StopFailure counterpart, so no turn-failed hook
        // is installed and the stale-turn watchdog ends a failed turn.
        let installed: Vec<&String> = settings["hooks"].as_object().unwrap().keys().collect();
        assert_eq!(installed.len(), 4, "{installed:?}");

        let env = env_of(&spec);
        assert_eq!(env[ENV_SESSION_TOKEN], "tok-abc");
        assert_eq!(env[ENV_SOCKET], "/run/pm.sock");
    }

    #[test]
    fn gemini_registers_the_reporting_server_over_streamable_http() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = GeminiAdapter
            .spawn_command(&ctx_with_mcp(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let server = gemini_settings_of(&spec)["mcpServers"][MCP_SERVER_NAME].clone();
        // httpUrl is the streamable-HTTP key; url means SSE.
        assert_eq!(server["httpUrl"], "http://127.0.0.1:7676/mcp");
        assert!(server["url"].is_null());
        assert_eq!(server["headers"]["Authorization"], "Bearer tok-abc");
        assert_eq!(server["trust"], true);
    }

    #[test]
    fn gemini_without_mcp_still_installs_hooks_and_registers_no_server() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = GeminiAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let settings = gemini_settings_of(&spec);
        assert!(settings["mcpServers"].is_null());
        assert!(settings["hooks"]["SessionStart"].is_array());
    }

    /// Gemini reads context files by name out of the directories it is
    /// given, so the instructions get a directory of their own: the
    /// session's other files carry the session token.
    #[test]
    fn gemini_carries_the_compiled_instructions_as_a_context_file() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = GeminiAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let index = spec
            .args
            .iter()
            .position(|a| a == "--include-directories")
            .unwrap();
        let dir = std::path::Path::new(&spec.args[index + 1]);
        assert_eq!(
            std::fs::read_to_string(dir.join(GEMINI_INSTRUCTIONS_FILE)).unwrap(),
            REPORTING_BRIEF
        );
        assert_eq!(
            std::fs::read_dir(dir).unwrap().count(),
            1,
            "the context directory must hold nothing but the instructions"
        );
        let names = gemini_settings_of(&spec)["context"]["fileName"].clone();
        // Naming our file must not stop a project's own GEMINI.md.
        assert_eq!(
            names,
            serde_json::json!(["GEMINI.md", "PM-INSTRUCTIONS.md"])
        );
    }

    #[test]
    fn gemini_offers_no_context_directory_without_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path().to_path_buf());
        c.compiled_instructions = String::new();
        let spec = GeminiAdapter.spawn_command(&c).unwrap().spec;
        // A missing include directory is fatal to Gemini, so the flag is
        // only passed when a directory was actually written.
        assert!(!spec.args.iter().any(|a| a == "--include-directories"));
        assert!(gemini_settings_of(&spec)["context"].is_null());
    }

    /// Gemini honors a session id chosen at spawn and hands it back in
    /// every hook payload, so the session is resumable from launch.
    #[test]
    fn gemini_predetermines_a_fresh_session_id_and_resumes_by_it() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = GeminiAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap();
        let id = plan.agent_session_id.clone().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
        assert_eq!(id.as_bytes()[14], b'4', "version 4: {id}");
        assert!(
            matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "{id}"
        );
        let index = plan
            .spec
            .args
            .iter()
            .position(|a| a == "--session-id")
            .unwrap();
        assert_eq!(plan.spec.args[index + 1], id);

        // Gemini rejects a --session-id that already exists, so a second
        // launch must not reuse one.
        let other = GeminiAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .agent_session_id
            .unwrap();
        assert_ne!(other, id);

        let resumed = GeminiAdapter
            .resume_command(&ctx(tmp.path().to_path_buf()), &id)
            .unwrap();
        // --resume takes the session id itself and keeps it; --session-id
        // means "start a new session with this id" and must not ride along.
        let index = resumed
            .spec
            .args
            .iter()
            .position(|a| a == "--resume")
            .unwrap();
        assert_eq!(resumed.spec.args[index + 1], id);
        assert!(!resumed.spec.args.iter().any(|a| a == "--session-id"));
        assert_eq!(resumed.agent_session_id.as_deref(), Some(id.as_str()));
        assert!(resumed.spec.args.iter().any(|a| a == "--skip-trust"));
    }

    #[test]
    fn gemini_permission_flags_map_per_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let flags = |mode| {
            GeminiAdapter
                .spawn_command(&ctx_with_mode(tmp.path().to_path_buf(), mode))
                .unwrap()
                .spec
                .args
        };
        let auto = flags(PermissionMode::Auto);
        let index = auto.iter().position(|a| a == "--approval-mode").unwrap();
        assert_eq!(auto[index + 1], "auto_edit");

        let bypass = flags(PermissionMode::Bypass);
        let index = bypass.iter().position(|a| a == "--approval-mode").unwrap();
        assert_eq!(bypass[index + 1], "yolo");

        for mode in [PermissionMode::Default, PermissionMode::Inherit] {
            assert!(!flags(mode).iter().any(|a| a == "--approval-mode"));
        }
    }

    fn opencode_config_of(spec: &CommandSpec) -> serde_json::Value {
        let path = env_of(spec)[ENV_OPENCODE_CONFIG].to_string();
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn opencode_plugin_of(spec: &CommandSpec) -> String {
        let entry = opencode_config_of(spec)["plugin"][0]
            .as_str()
            .unwrap()
            .to_string();
        let path = entry
            .strip_prefix("file://")
            .expect("plugin is named by URL");
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn opencode_spawn_and_resume_leave_the_alternate_screen_unset() {
        let files = tempfile::tempdir().unwrap();
        let mut context = ctx(files.path().to_path_buf());
        for fullscreen in [false, true] {
            context.fullscreen = fullscreen;
            let spawned = OpenCodeAdapter.spawn_command(&context).unwrap();
            let resumed = OpenCodeAdapter.resume_command(&context, "sess-1").unwrap();
            for plan in [spawned, resumed] {
                assert!(
                    !plan
                        .spec
                        .env
                        .iter()
                        .any(|(k, _)| k == "OTUI_USE_ALTERNATE_SCREEN"),
                    "{:?}",
                    plan.spec.env
                );
            }
        }
    }

    #[test]
    fn opencode_gets_a_config_file_naming_a_per_session_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = OpenCodeAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap();
        let spec = plan.spec;

        assert_eq!(spec.program, "opencode");
        // The positional argument is the project directory, so the
        // initial message can only reach OpenCode through --prompt.
        let prompt = spec.args.iter().position(|a| a == "--prompt").unwrap();
        assert_eq!(spec.args[prompt + 1], "fix the tests");
        // --pure would run OpenCode without external plugins, which
        // drops every lifecycle event.
        assert!(!spec.args.iter().any(|a| a == "--pure"));
        // OpenCode assigns the session id and reports it through the
        // plugin, so nothing is predetermined here.
        assert!(plan.agent_session_id.is_none());
        assert!(!plan.detect_osc9_needs_input);

        let env = env_of(&spec);
        assert_eq!(env[ENV_SESSION_TOKEN], "tok-abc");
        assert_eq!(env[ENV_SOCKET], "/run/pm.sock");
        // There is no config flag; the file reaches OpenCode as one more
        // merged layer over the user's own config.
        assert!(env[ENV_OPENCODE_CONFIG].ends_with("session-7-opencode.json"));

        let plugin = opencode_plugin_of(&spec);
        for kind in [
            "session-start",
            "needs-input",
            "turn-failed",
            "turn-ended",
            "prompt-submitted",
        ] {
            assert!(plugin.contains(kind), "plugin omits {kind}");
        }
        assert!(plugin.contains(r#"const PM = "/usr/local/bin/pm""#));
    }

    /// OpenCode has no hook configuration, so every lifecycle signal
    /// comes from one plugin subscribed to the event bus.
    #[test]
    fn opencode_plugin_maps_each_bus_event_to_its_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = OpenCodeAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let plugin = opencode_plugin_of(&spec);
        for (event, kind) in [
            ("session.created", "session-start"),
            ("permission.asked", "needs-input"),
            ("permission.replied", "prompt-submitted"),
            ("question.asked", "needs-input"),
            ("question.replied", "prompt-submitted"),
            ("question.rejected", "prompt-submitted"),
            ("session.error", "turn-failed"),
            ("session.idle", "turn-ended"),
        ] {
            assert!(
                plugin.contains(&format!(r#""{event}": "{kind}""#)),
                "plugin does not map {event}"
            );
        }
        // A turn starts when the session goes busy. The user
        // message.updated fires again after the turn is over, so it
        // cannot stand in for the submission.
        assert!(plugin.contains(r#"data.status?.type !== "busy""#));
        assert!(!plugin.contains("message.updated"));
        // session.idle lands right after session.error and both end a
        // turn, so the second one must not replace the failure detail.
        assert!(plugin.contains("failed.delete(session)"));
        // A subagent runs as a child session; its idle would end the
        // parent's turn early.
        assert!(plugin.contains("children.add(session)"));
        // The events ending a turn arrive together, so the hook calls
        // are chained rather than raced.
        assert!(plugin.contains("queue = queue"));
        // A resumed session emits no session.created, so the start is
        // reported when the plugin loads.
        assert!(plugin.contains(r#"await send("session-start", "", "")"#));
    }

    /// Drives the generated plugin under node over `events` and returns
    /// the hook kinds it sent, in order. Only OpenCode's shell binding is
    /// stubbed, so the plugin itself runs exactly as it ships.
    ///
    /// Returns None where node is unavailable; every lane that runs this
    /// crate's tests also builds the web bundle, so that is a developer
    /// running the Rust suite alone rather than a lane losing coverage.
    fn hook_kinds_from_plugin(events: &[serde_json::Value]) -> Option<Vec<String>> {
        Some(
            hook_calls_from_plugin(events)?
                .into_iter()
                .map(|(kind, _)| kind)
                .collect(),
        )
    }

    /// The same drive, keeping each hook's payload so a test can assert
    /// the detail the controller records as the blocked reason.
    fn hook_calls_from_plugin(
        events: &[serde_json::Value],
    ) -> Option<Vec<(String, serde_json::Value)>> {
        let tmp = tempfile::tempdir().unwrap();
        let recorder = tmp.path().join("record.mjs");
        let plugin = tmp.path().join("plugin.mjs");
        std::fs::write(&plugin, opencode_plugin_js(&recorder)).unwrap();

        let driver = format!(
            r#"import {{ PuppetMaster }} from {plugin};
const sent = [];
const $ = (strings, ...values) => {{
  // The plugin builds `${{PM}} _hook ${{kind}} < ${{payload}}`, so the kind is
  // the second interpolated value and the payload the third.
  sent.push([String(values[1]), JSON.parse(String(values[2]))]);
  const done = Promise.resolve();
  done.quiet = () => done;
  done.nothrow = () => done;
  return done;
}};
const plugin = await PuppetMaster({{ $ }});
for (const event of {events}) await plugin.event({{ event }});
console.log(JSON.stringify(sent));
"#,
            plugin = serde_json::Value::String(plugin.display().to_string()),
            events = serde_json::to_string(events).unwrap(),
        );
        let driver_path = tmp.path().join("driver.mjs");
        std::fs::write(&driver_path, driver).unwrap();

        let out = std::process::Command::new("node")
            .arg(&driver_path)
            .output()
            .ok()?;
        assert!(
            out.status.success(),
            "driving the plugin failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = String::from_utf8(out.stdout).unwrap();
        Some(serde_json::from_str(line.trim()).unwrap())
    }

    fn status(session: &str, kind: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "session.status",
            "properties": { "sessionID": session, "status": { "type": kind } },
        })
    }

    fn bus_event(ty: &str, session: &str) -> serde_json::Value {
        serde_json::json!({ "type": ty, "properties": { "sessionID": session } })
    }

    fn permission_asked(session: &str, permission: &str, pattern: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "permission.asked",
            "properties": {
                "sessionID": session,
                "permission": permission,
                "patterns": [pattern],
            },
        })
    }

    /// OpenCode asks a batch, each entry carrying both the full question
    /// and the short header it labels the prompt with.
    fn question_asked(session: &str, header: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "question.asked",
            "properties": {
                "sessionID": session,
                "questions": [{
                    "question": format!("Which one, given {header}?"),
                    "header": header,
                    "options": [],
                }],
            },
        })
    }

    /// OpenCode re-broadcasts `busy` several times inside one turn. Each
    /// one used to submit a prompt, which put the session to working
    /// repeatedly and read in the controller log as a duplicated event.
    #[test]
    fn opencode_reports_one_turn_start_per_busy_run() {
        let Some(sent) = hook_kinds_from_plugin(&[
            status("s1", "busy"),
            status("s1", "busy"),
            status("s1", "busy"),
            bus_event("session.idle", "s1"),
        ]) else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        assert_eq!(sent, ["session-start", "prompt-submitted", "turn-ended"]);
    }

    /// The next turn has to be reported, so the end of one clears the
    /// edge rather than latching the session busy forever.
    #[test]
    fn opencode_reports_the_turn_after_an_idle_and_after_a_failure() {
        let Some(sent) = hook_kinds_from_plugin(&[
            status("s1", "busy"),
            bus_event("session.idle", "s1"),
            status("s1", "busy"),
            bus_event("session.error", "s1"),
            // The idle trailing a failure is still dropped, so the
            // failure detail survives.
            bus_event("session.idle", "s1"),
            status("s1", "busy"),
            bus_event("session.idle", "s1"),
        ]) else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        assert_eq!(
            sent,
            [
                "session-start",
                "prompt-submitted",
                "turn-ended",
                "prompt-submitted",
                "turn-failed",
                "prompt-submitted",
                "turn-ended",
            ]
        );
    }

    /// A permission is asked and answered inside a turn that never
    /// stopped, so the busy edge that normally reports a submission has
    /// already been spent. Without the reply the session stays blocked
    /// for the rest of its life: the controller preserves needs-input
    /// across the turn end, and only a submission or a human typing into
    /// the terminal clears it.
    #[test]
    fn opencode_reports_an_answered_permission_as_a_submission() {
        let Some(sent) = hook_kinds_from_plugin(&[
            status("s1", "busy"),
            permission_asked("s1", "external_directory", "/tmp/cache/*"),
            bus_event("permission.replied", "s1"),
            bus_event("session.idle", "s1"),
        ]) else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        assert_eq!(
            sent,
            [
                "session-start",
                "prompt-submitted",
                "needs-input",
                "prompt-submitted",
                "turn-ended",
            ]
        );
    }

    /// A question blocks the same way a permission does, and is answered
    /// or dismissed without ending the turn.
    #[test]
    fn opencode_reports_an_asked_question_and_both_ways_it_ends() {
        let Some(sent) = hook_kinds_from_plugin(&[
            status("s1", "busy"),
            question_asked("s1", "Database choice"),
            bus_event("question.replied", "s1"),
            question_asked("s1", "Retry policy"),
            bus_event("question.rejected", "s1"),
            bus_event("session.idle", "s1"),
        ]) else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        assert_eq!(
            sent,
            [
                "session-start",
                "prompt-submitted",
                "needs-input",
                "prompt-submitted",
                "needs-input",
                "prompt-submitted",
                "turn-ended",
            ]
        );
    }

    /// The blocked reason the controller records, for each of the two
    /// things OpenCode can block on.
    #[test]
    fn opencode_names_what_it_is_blocked_on() {
        let Some(sent) = hook_calls_from_plugin(&[
            permission_asked("s1", "external_directory", "/tmp/cache/*"),
            question_asked("s1", "Database choice"),
        ]) else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        let details = sent
            .iter()
            .filter(|(kind, _)| kind == "needs-input")
            .map(|(_, payload)| payload["message"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            details,
            ["external_directory: /tmp/cache/*", "Database choice"]
        );
    }

    /// A status that leaves busy ends the run without reporting
    /// anything, so the turn after it is a new one.
    #[test]
    fn opencode_treats_a_status_leaving_busy_as_the_end_of_the_run() {
        let Some(sent) = hook_kinds_from_plugin(&[
            status("s1", "busy"),
            status("s1", "idle"),
            status("s1", "busy"),
        ]) else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        assert_eq!(
            sent,
            ["session-start", "prompt-submitted", "prompt-submitted"]
        );
    }

    /// Each session keeps its own edge, so a second session going busy
    /// is its own turn rather than a repeat of the first.
    #[test]
    fn opencode_tracks_the_busy_edge_per_session() {
        let Some(sent) = hook_kinds_from_plugin(&[
            status("s1", "busy"),
            status("s2", "busy"),
            status("s1", "busy"),
        ]) else {
            eprintln!("skipped: node is not on PATH");
            return;
        };
        assert_eq!(
            sent,
            ["session-start", "prompt-submitted", "prompt-submitted"]
        );
    }

    #[test]
    fn opencode_registers_the_reporting_server_as_a_remote_mcp() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = OpenCodeAdapter
            .spawn_command(&ctx_with_mcp(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let config = opencode_config_of(&spec);
        let server = config["mcp"][MCP_SERVER_NAME].clone();
        assert_eq!(server["type"], "remote");
        assert_eq!(server["url"], "http://127.0.0.1:7676/mcp");
        assert_eq!(server["headers"]["Authorization"], "Bearer tok-abc");
        assert_eq!(server["enabled"], true);
        // Its tools are allowed by name, so a user config that asks for
        // everything still cannot stall the session on a report.
        assert_eq!(
            config["permission"][format!("{MCP_SERVER_NAME}_*")],
            "allow"
        );
    }

    #[test]
    fn opencode_without_mcp_still_loads_the_plugin_and_registers_no_server() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = OpenCodeAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let config = opencode_config_of(&spec);
        assert!(config["mcp"].is_null());
        assert!(config["plugin"][0].as_str().unwrap().ends_with(".js"));
    }

    /// OpenCode adds every `instructions` file to the system prompt on
    /// every turn, so the compiled instructions survive a context reset
    /// without a hook that can inject context.
    #[test]
    fn opencode_carries_the_compiled_instructions_as_a_config_instruction() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = OpenCodeAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let path = opencode_config_of(&spec)["instructions"][0]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(std::fs::read_to_string(path).unwrap(), REPORTING_BRIEF);
        // The brief rides the instructions, so it is not repeated on the
        // prompt the way Codex would need.
        let prompt = spec.args.iter().position(|a| a == "--prompt").unwrap();
        assert_eq!(spec.args[prompt + 1], "fix the tests");
    }

    #[test]
    fn opencode_omits_instructions_when_there_are_none() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path().to_path_buf());
        c.compiled_instructions = String::new();
        let spec = OpenCodeAdapter.spawn_command(&c).unwrap().spec;
        assert!(opencode_config_of(&spec)["instructions"].is_null());
    }

    #[test]
    fn opencode_resumes_by_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = OpenCodeAdapter
            .resume_command(&ctx(tmp.path().to_path_buf()), "ses_abc123")
            .unwrap();
        let index = plan
            .spec
            .args
            .iter()
            .position(|a| a == "--session")
            .unwrap();
        assert_eq!(plan.spec.args[index + 1], "ses_abc123");
        assert_eq!(plan.agent_session_id.as_deref(), Some("ses_abc123"));
        // A resumed session takes no new prompt, and it still gets the
        // plugin that reports its lifecycle.
        assert!(!plan.spec.args.iter().any(|a| a == "--prompt"));
        assert!(!opencode_plugin_of(&plan.spec).is_empty());
    }

    /// OpenCode's own default agent allows every tool, so default mode
    /// installs the ask baseline instead of passing no flags; without it
    /// a default-mode session would run unattended.
    #[test]
    fn opencode_permission_rules_map_per_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let spawn = |mode| {
            OpenCodeAdapter
                .spawn_command(&ctx_with_mode(tmp.path().to_path_buf(), mode))
                .unwrap()
                .spec
        };

        for mode in [PermissionMode::Default, PermissionMode::Inherit] {
            let spec = spawn(mode);
            assert!(!spec.args.iter().any(|a| a == "--auto"));
            // No reporting server is registered here, so the rules are
            // only the ask baseline.
            assert_eq!(
                opencode_config_of(&spec)["permission"],
                serde_json::json!({ "edit": "ask", "bash": "ask", "webfetch": "ask" })
            );
        }

        let auto = spawn(PermissionMode::Auto);
        assert!(!auto.args.iter().any(|a| a == "--auto"));
        assert_eq!(opencode_config_of(&auto)["permission"]["edit"], "allow");
        assert_eq!(opencode_config_of(&auto)["permission"]["bash"], "ask");

        // --auto approves everything not explicitly denied, so bypass
        // needs no rules of its own.
        let bypass = spawn(PermissionMode::Bypass);
        assert!(bypass.args.iter().any(|a| a == "--auto"));
        assert!(opencode_config_of(&bypass)["permission"].is_null());
    }

    #[test]
    fn opencode_takes_model_from_argv_and_endpoint_from_a_provider_block() {
        let tmp = tempfile::tempdir().unwrap();
        for dialect in [
            ModelDialect::AnthropicMessages,
            ModelDialect::OpenaiResponses,
        ] {
            let spec = OpenCodeAdapter
                .spawn_command(&ctx_with_endpoint(
                    tmp.path().to_path_buf(),
                    endpoint(dialect),
                ))
                .unwrap()
                .spec;
            let index = spec.args.iter().position(|a| a == "-m").unwrap();
            assert_eq!(spec.args[index + 1], "puppet-master/gateway/big");

            let provider = opencode_config_of(&spec)["provider"][OPENCODE_PROVIDER_ID].clone();
            assert_eq!(provider["name"], "Gateway");
            assert_eq!(provider["options"]["baseURL"], "https://gw.example/v1");
            assert_eq!(provider["models"]["gateway/big"]["name"], "gateway/big");
            let npm = if dialect == ModelDialect::AnthropicMessages {
                OPENCODE_ANTHROPIC_NPM
            } else {
                OPENCODE_OPENAI_NPM
            };
            assert_eq!(provider["npm"], npm);

            // The key is an environment reference, never the value.
            assert_eq!(
                provider["options"]["apiKey"],
                format!("{{env:{ENV_MODEL_API_KEY}}}")
            );
            assert_eq!(env_of(&spec)[ENV_MODEL_API_KEY], "sk-secret-value");
        }

        // OpenCode has no small/fast model setting, so an entry's
        // background model is ignored.
        assert!(!OpenCodeAdapter.supports_background_model());
    }

    /// An entry that only pins a model runs on OpenCode's own provider
    /// logins, so the model keeps whatever provider it already names.
    #[test]
    fn opencode_entry_without_a_base_url_keeps_the_models_own_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let mut endpoint = endpoint(ModelDialect::OpenaiResponses);
        endpoint.base_url = String::new();
        let spec = OpenCodeAdapter
            .spawn_command(&ctx_with_endpoint(tmp.path().to_path_buf(), endpoint))
            .unwrap()
            .spec;
        let index = spec.args.iter().position(|a| a == "-m").unwrap();
        assert_eq!(spec.args[index + 1], "gateway/big");
        assert!(opencode_config_of(&spec)["provider"].is_null());
        assert!(!env_of(&spec).contains_key(ENV_MODEL_API_KEY));
    }

    #[test]
    fn test_agent_runs_the_configured_binary_with_env() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = AdapterRegistry::empty();
        r.register(Box::new(TestAgentAdapter {
            program: PathBuf::from("/build/testagent"),
        }));
        let spec = r
            .get(AgentKind::Test)
            .unwrap()
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        assert_eq!(spec.program, "/build/testagent");
        assert_eq!(env_of(&spec)[ENV_SESSION_TOKEN], "tok-abc");
    }

    #[test]
    fn reporting_brief_names_dashboard_tools_and_artifact_preview_contract() {
        for tool in [
            "report",
            "flag_blocked",
            "publish_dir",
            "list_dirs",
            "unpublish_dir",
            "publish_port",
            "unpublish_port",
        ] {
            assert!(REPORTING_BRIEF.contains(tool), "brief omits {tool}");
        }
        for guidance in [
            "HTML, images, reports, or similar artifacts",
            "narrowest directory",
            "returned URL instead of only filesystem paths",
            "Do not start an HTTP server \
             of your own for files",
            "nothing to restart after a resume",
            "for a \
             server you are genuinely running",
            "Bind a server you start to loopback",
            "After a resume you are told which ports you published",
            "restart each server on its own port rather than publishing it again",
            "Published directories are not in that list",
            "do not expose unrelated or sensitive files",
            "Use relative URLs",
            "WebSockets",
            "proxy strips that prefix",
            "paths that escape the mount",
        ] {
            assert!(
                REPORTING_BRIEF.contains(guidance),
                "brief omits artifact preview guidance: {guidance}"
            );
        }
    }

    #[test]
    fn reporting_brief_requires_explicit_blocking_before_a_waiting_turn_ends() {
        for guidance in [
            "Before ending a turn that cannot continue",
            "call `flag_blocked` with one concrete question",
            "Never merely print a question and then stop or finish",
            "Idle means the turn is complete and no input is currently required",
            "never use Idle as an implicit waiting state",
        ] {
            assert!(
                REPORTING_BRIEF.contains(guidance),
                "brief omits waiting-state guidance: {guidance}"
            );
        }
    }

    #[test]
    fn reporting_brief_keeps_live_status_compact_and_current() {
        for guidance in [
            "Set `goal`",
            "it names what the session is about",
            "Goal is required on every report",
            "constantly updated to reflect the current goal",
            "it is the current step, or the outcome when the turn ends",
            "Never put the step in the goal or the goal in the headline",
            "Keep headlines plain text (`&`, not `&amp;`)",
            "Keep `glance` and `context` small, current, and high-signal",
            "Treat 20 context fields as a ceiling, not a target",
            "use `clear` to drop stale keys",
        ] {
            assert!(
                REPORTING_BRIEF.contains(guidance),
                "brief omits compact reporting guidance: {guidance}"
            );
        }
    }

    fn endpoint(dialect: ModelDialect) -> ResolvedModelEndpoint {
        ResolvedModelEndpoint {
            dialect,
            model: "gateway/big".into(),
            base_url: "https://gw.example/v1".into(),
            background_model: "gateway/small".into(),
            api_key: "sk-secret-value".into(),
            provider_name: "Gateway".into(),
        }
    }

    fn ctx_with_endpoint(files_dir: PathBuf, endpoint: ResolvedModelEndpoint) -> SpawnCtx {
        let mut c = ctx(files_dir);
        c.model_endpoint = Some(endpoint);
        c
    }

    #[test]
    fn claude_takes_model_from_argv_and_endpoint_from_env() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = ClaudeCodeAdapter
            .spawn_command(&ctx_with_endpoint(
                tmp.path().to_path_buf(),
                endpoint(ModelDialect::AnthropicMessages),
            ))
            .unwrap()
            .spec;

        let model = spec.args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(spec.args[model + 1], "gateway/big");

        let env = env_of(&spec);
        assert_eq!(env["ANTHROPIC_BASE_URL"], "https://gw.example/v1");
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "sk-secret-value");
        assert!(ClaudeCodeAdapter.supports_background_model());
        assert_eq!(env["ANTHROPIC_SMALL_FAST_MODEL"], "gateway/small");
        // Remapping the haiku tier alias would also change what an
        // explicit haiku selection resolves to, which is not what an
        // entry's background model asks for.
        assert!(!env.contains_key("ANTHROPIC_DEFAULT_HAIKU_MODEL"));
    }

    #[test]
    fn gemini_takes_model_from_argv_and_endpoint_from_env() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = GeminiAdapter
            .spawn_command(&ctx_with_endpoint(
                tmp.path().to_path_buf(),
                endpoint(ModelDialect::GoogleGenai),
            ))
            .unwrap()
            .spec;

        let model = spec.args.iter().position(|a| a == "-m").unwrap();
        assert_eq!(spec.args[model + 1], "gateway/big");

        let env = env_of(&spec);
        assert_eq!(env["GOOGLE_GEMINI_BASE_URL"], "https://gw.example/v1");
        assert_eq!(env["GEMINI_API_KEY"], "sk-secret-value");
        // Vertex is a second endpoint shape one base_url cannot pick out,
        // so an entry never reaches it.
        assert!(!env.contains_key("GOOGLE_VERTEX_BASE_URL"));
        assert!(!env.contains_key("GOOGLE_GENAI_USE_VERTEXAI"));
    }

    /// Gemini has no small/fast model setting, so an entry's background
    /// model reaches nothing rather than being applied somewhere else.
    #[test]
    fn gemini_emits_nothing_for_the_background_model() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = GeminiAdapter
            .spawn_command(&ctx_with_endpoint(
                tmp.path().to_path_buf(),
                endpoint(ModelDialect::GoogleGenai),
            ))
            .unwrap()
            .spec;
        assert!(!GeminiAdapter.supports_background_model());
        assert!(!spec.args.iter().any(|a| a.contains("gateway/small")));
        assert!(spec.env.iter().all(|(_, v)| v != "gateway/small"));
        assert!(
            !std::fs::read_to_string(env_of(&spec)[ENV_GEMINI_SYSTEM_SETTINGS_PATH])
                .unwrap()
                .contains("gateway/small")
        );
    }

    #[test]
    fn codex_takes_model_and_provider_from_config_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = CodexAdapter
            .spawn_command(&ctx_with_endpoint(
                tmp.path().to_path_buf(),
                endpoint(ModelDialect::OpenaiResponses),
            ))
            .unwrap()
            .spec;

        assert_eq!(
            config_value(&spec.args, "model="),
            Some("model=\"gateway/big\"")
        );
        assert_eq!(
            config_value(&spec.args, "model_provider="),
            Some("model_provider=\"puppet-master\"")
        );
        assert_eq!(
            config_value(&spec.args, "model_providers.puppet-master.name="),
            Some("model_providers.puppet-master.name=\"Gateway\"")
        );
        assert_eq!(
            config_value(&spec.args, "model_providers.puppet-master.base_url="),
            Some("model_providers.puppet-master.base_url=\"https://gw.example/v1\"")
        );
        // The credential is named, not written: Codex reads it from the
        // variable the adapter exports.
        assert_eq!(
            config_value(&spec.args, "model_providers.puppet-master.env_key="),
            Some("model_providers.puppet-master.env_key=\"PM_MODEL_API_KEY\"")
        );
        assert_eq!(
            config_value(&spec.args, "model_providers.puppet-master.wire_api="),
            Some("model_providers.puppet-master.wire_api=\"responses\"")
        );
        assert_eq!(env_of(&spec)["PM_MODEL_API_KEY"], "sk-secret-value");
    }

    /// Codex has no small/fast model setting, so an entry's background
    /// model must reach neither its config overrides nor its
    /// environment rather than being written somewhere inert.
    #[test]
    fn codex_emits_nothing_for_the_background_model() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!CodexAdapter.supports_background_model());
        let ctx = ctx_with_endpoint(
            tmp.path().to_path_buf(),
            endpoint(ModelDialect::OpenaiResponses),
        );
        for spec in [
            CodexAdapter.spawn_command(&ctx).unwrap().spec,
            CodexAdapter.resume_command(&ctx, "uuid-1").unwrap().spec,
        ] {
            assert!(
                !spec.args.iter().any(|a| a.contains("gateway/small")),
                "{:?}",
                spec.args
            );
            assert!(
                !spec.env.iter().any(|(key, value)| key.contains("SMALL")
                    || key.contains("BACKGROUND")
                    || value == "gateway/small"),
                "{:?}",
                spec.env
            );
        }
    }

    /// Every file the adapters write, plus argv, must stay free of the
    /// credential; only the child environment carries it. Walks the whole
    /// registered set so a newly added adapter is covered without being
    /// named here.
    #[test]
    fn no_adapter_puts_the_api_key_in_a_session_file_or_argv() {
        let home = tempfile::tempdir().unwrap();
        // One adapter registers its reporting server in the user's own
        // file, so the walk runs against a scratch home rather than the
        // developer's.
        with_home(home.path(), no_adapter_leaks_the_key);
        let config = home.path().join(ANTIGRAVITY_MCP_CONFIG);
        assert!(!std::fs::read_to_string(config)
            .unwrap_or_default()
            .contains("sk-secret-value"));
    }

    fn no_adapter_leaks_the_key() {
        let registry = AdapterRegistry::standard();
        for dialect in ModelDialect::ALL.iter().copied() {
            let tmp = tempfile::tempdir().unwrap();
            for adapter in registry.adapters() {
                let mut c = ctx_with_endpoint(tmp.path().to_path_buf(), endpoint(dialect));
                c.integration.mcp_url = Some("http://127.0.0.1:7676/mcp".into());
                for spec in [
                    adapter.spawn_command(&c).unwrap().spec,
                    adapter.resume_command(&c, "uuid-1").unwrap().spec,
                ] {
                    for arg in &spec.args {
                        assert!(
                            !arg.contains("sk-secret-value"),
                            "{} argv leaked the key: {arg}",
                            spec.program
                        );
                    }
                    for entry in std::fs::read_dir(tmp.path()).unwrap() {
                        let path = entry.unwrap().path();
                        let contents = std::fs::read_to_string(&path).unwrap_or_default();
                        assert!(
                            !contents.contains("sk-secret-value"),
                            "{} leaked the key into {}",
                            spec.program,
                            path.display()
                        );
                    }
                    // The daemon only hands an adapter an endpoint it
                    // selected for one of that adapter's own dialects,
                    // so an adapter declaring none never has a
                    // credential to carry anywhere.
                    if adapter.dialects().is_empty() {
                        assert!(
                            spec.env.iter().all(|(_, value)| value != "sk-secret-value"),
                            "{} exported a key its CLI cannot apply",
                            spec.program
                        );
                    } else {
                        assert!(spec.env.iter().any(|(_, value)| value == "sk-secret-value"));
                    }
                }
            }
        }
    }

    /// An entry with no base_url pins a model on the agent CLI's own
    /// account, so neither the credential nor a provider override
    /// applies.
    #[test]
    fn an_entry_without_a_base_url_only_pins_the_model() {
        let tmp = tempfile::tempdir().unwrap();
        let mut endpoint = endpoint(ModelDialect::AnthropicMessages);
        endpoint.base_url = String::new();

        let claude = ClaudeCodeAdapter
            .spawn_command(&ctx_with_endpoint(
                tmp.path().to_path_buf(),
                endpoint.clone(),
            ))
            .unwrap()
            .spec;
        let env = env_of(&claude);
        assert!(claude.args.iter().any(|a| a == "--model"));
        assert!(!env.contains_key("ANTHROPIC_BASE_URL"));
        assert!(!env.contains_key("ANTHROPIC_AUTH_TOKEN"));

        endpoint.dialect = ModelDialect::OpenaiResponses;
        let codex = CodexAdapter
            .spawn_command(&ctx_with_endpoint(
                tmp.path().to_path_buf(),
                endpoint.clone(),
            ))
            .unwrap()
            .spec;
        assert!(config_value(&codex.args, "model=").is_some());
        assert!(config_value(&codex.args, "model_provider=").is_none());
        assert!(!env_of(&codex).contains_key("PM_MODEL_API_KEY"));

        endpoint.dialect = ModelDialect::GoogleGenai;
        let gemini = GeminiAdapter
            .spawn_command(&ctx_with_endpoint(tmp.path().to_path_buf(), endpoint))
            .unwrap()
            .spec;
        let env = env_of(&gemini);
        assert!(gemini.args.iter().any(|a| a == "-m"));
        assert!(!env.contains_key("GOOGLE_GEMINI_BASE_URL"));
        assert!(!env.contains_key("GEMINI_API_KEY"));
    }

    fn entry(dialect: ModelDialect) -> ModelProfileEndpoint {
        ModelProfileEndpoint {
            profile_id: 1,
            dialect,
            model: "m".into(),
            base_url: String::new(),
            background_model: String::new(),
        }
    }

    /// Each adapter currently declares one dialect, but selection still
    /// walks the adapter's own order rather than the entry order, so an
    /// agent that speaks several again stays deterministic.
    #[test]
    fn entry_selection_follows_the_adapters_dialect_preference() {
        let entries = [
            entry(ModelDialect::OpenaiResponses),
            entry(ModelDialect::AnthropicMessages),
            entry(ModelDialect::GoogleGenai),
        ];
        assert_eq!(
            select_endpoint(&ClaudeCodeAdapter, &entries)
                .unwrap()
                .dialect,
            ModelDialect::AnthropicMessages
        );
        assert_eq!(
            select_endpoint(&CodexAdapter, &entries).unwrap().dialect,
            ModelDialect::OpenaiResponses
        );
        // Gemini speaks neither of the other two dialects, so without its
        // own entry a profile cannot reach it at all.
        assert_eq!(
            select_endpoint(&GeminiAdapter, &entries).unwrap().dialect,
            ModelDialect::GoogleGenai
        );
        assert_eq!(
            select_endpoint(&GeminiAdapter, &entries[..2]).unwrap_err(),
            vec![
                ModelDialect::AnthropicMessages,
                ModelDialect::OpenaiResponses
            ]
        );

        struct TwoDialects;
        impl AgentAdapter for TwoDialects {
            fn kind(&self) -> AgentKind {
                AgentKind::Test
            }
            fn dialects(&self) -> &'static [ModelDialect] {
                &[
                    ModelDialect::OpenaiResponses,
                    ModelDialect::AnthropicMessages,
                ]
            }
            fn spawn_command(&self, _ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
                unreachable!()
            }
        }
        assert_eq!(
            select_endpoint(&TwoDialects, &entries).unwrap().dialect,
            ModelDialect::OpenaiResponses
        );
    }

    #[test]
    fn an_agent_with_no_matching_entry_is_rejected_with_the_covered_dialects() {
        let entries = [entry(ModelDialect::AnthropicMessages)];
        let covered = select_endpoint(&CodexAdapter, &entries).unwrap_err();
        assert_eq!(covered, vec![ModelDialect::AnthropicMessages]);
        assert!(select_endpoint(&CodexAdapter, &[]).unwrap_err().is_empty());
    }

    #[test]
    fn standard_registry_has_real_agents_but_not_test_agent() {
        let r = AdapterRegistry::standard();
        assert!(r.get(AgentKind::ClaudeCode).is_ok());
        assert!(r.get(AgentKind::Codex).is_ok());
        assert!(r.get(AgentKind::Test).is_err());
    }

    /// The agents a UI offers and the agents the registry can spawn are
    /// two lists that must not drift: an offer the registry rejects is a
    /// spawn that fails, and an adapter no picker lists is unreachable.
    #[test]
    fn selectable_agents_are_exactly_the_standard_registry() {
        assert_eq!(
            AdapterRegistry::standard().kinds(),
            AgentKind::SELECTABLE.to_vec()
        );
    }

    #[test]
    fn every_registered_adapter_reports_its_own_kind() {
        let registry = AdapterRegistry::standard();
        for kind in registry.kinds() {
            assert_eq!(registry.get(kind).unwrap().kind(), kind);
        }
    }

    fn ctx_with_mode(files_dir: PathBuf, mode: PermissionMode) -> SpawnCtx {
        let mut c = ctx(files_dir);
        c.permission_mode = mode;
        c
    }

    #[test]
    fn claude_permission_flags_map_per_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let auto = ClaudeCodeAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Auto,
            ))
            .unwrap()
            .spec;
        let ai = auto
            .args
            .iter()
            .position(|a| a == "--permission-mode")
            .unwrap();
        assert_eq!(auto.args[ai + 1], "acceptEdits");

        let bypass = ClaudeCodeAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Bypass,
            ))
            .unwrap()
            .spec;
        assert!(bypass
            .args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));

        let default = ClaudeCodeAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Default,
            ))
            .unwrap()
            .spec;
        assert!(!default.args.iter().any(|a| a == "--permission-mode"));
        assert!(!default
            .args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));
    }

    #[test]
    fn codex_permission_flags_map_per_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let auto = CodexAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Auto,
            ))
            .unwrap()
            .spec;
        assert!(auto
            .args
            .windows(2)
            .any(|w| w[0] == "--sandbox" && w[1] == "workspace-write"));
        assert!(auto
            .args
            .windows(2)
            .any(|w| w[0] == "-a" && w[1] == "on-request"));

        let bypass = CodexAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Bypass,
            ))
            .unwrap()
            .spec;
        assert!(bypass
            .args
            .iter()
            .any(|a| a == "--dangerously-bypass-approvals-and-sandbox"));

        let default = CodexAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Default,
            ))
            .unwrap()
            .spec;
        assert!(!default.args.iter().any(|a| a == "--sandbox"));
    }

    /// The directory `--add-dir` names, which is the only place the CLI
    /// reads our hooks, reporting server and instructions from.
    fn antigravity_dir_of(spec: &CommandSpec) -> PathBuf {
        let i = spec.args.iter().position(|a| a == "--add-dir").unwrap();
        PathBuf::from(&spec.args[i + 1]).join(".agents")
    }

    fn antigravity_json_at(path: PathBuf) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn antigravity_runs_the_agy_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = AntigravityAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        assert_eq!(spec.program, "agy");
    }

    #[test]
    fn antigravity_installs_the_lifecycle_hooks_it_has() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = AntigravityAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let hooks = antigravity_json_at(antigravity_dir_of(&spec).join("hooks.json"));
        let entry = &hooks[MCP_SERVER_NAME];
        // Flat handlers, not the nested `hooks` array the other agents
        // take: the CLI rejects the whole file on the nested form.
        assert_eq!(
            entry["PreInvocation"][0]["command"],
            serde_json::json!("/usr/local/bin/pm _hook prompt-submitted")
        );
        assert_eq!(
            entry["PreInvocation"][0]["type"],
            serde_json::json!("command")
        );
        assert_eq!(
            entry["Stop"][0]["command"],
            serde_json::json!("/usr/local/bin/pm _hook turn-ended")
        );
        assert_eq!(
            entry["SessionStart"][0]["command"],
            serde_json::json!("/usr/local/bin/pm _hook started")
        );
        assert_eq!(
            entry["SessionStart"][0]["type"],
            serde_json::json!("command")
        );
        assert!(entry["Notification"].is_null());
    }

    /// The CLI reads MCP servers only from the user's own file, so the
    /// registered entry is session-agnostic and the endpoint reaches
    /// the bridge through the environment instead.
    #[test]
    fn antigravity_reaches_the_reporting_server_through_the_stdio_bridge() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let spec = with_home(home.path(), || {
            AntigravityAdapter
                .spawn_command(&ctx_with_mcp(tmp.path().to_path_buf()))
                .unwrap()
                .spec
        });
        let env = env_of(&spec);
        assert_eq!(env.get(ENV_MCP_URL), Some(&"http://127.0.0.1:7676/mcp"));
        assert_eq!(env.get(ENV_SESSION_TOKEN), Some(&"tok-abc"));

        let server = antigravity_json_at(home.path().join(ANTIGRAVITY_MCP_CONFIG))["mcpServers"]
            [MCP_SERVER_NAME]
            .clone();
        assert_eq!(server["command"], "/usr/local/bin/pm");
        assert_eq!(server["args"], serde_json::json!(["_mcp"]));
        // The token is never written to the file every agy session on
        // the host reads.
        assert!(!server.to_string().contains("tok-abc"));
        assert!(!antigravity_dir_of(&spec).join("mcp_config.json").exists());
    }

    /// Registering must not disturb the servers a user configured, nor
    /// the fields of ours they may have edited around.
    #[test]
    fn antigravity_registration_leaves_the_users_own_servers_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(ANTIGRAVITY_MCP_CONFIG);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"theirSetting":true,"mcpServers":{"theirs":{"serverUrl":"https://example.invalid/mcp"}}}"#,
        )
        .unwrap();
        with_home(home.path(), || {
            AntigravityAdapter
                .spawn_command(&ctx_with_mcp(tmp.path().to_path_buf()))
                .unwrap();
        });
        let config = antigravity_json_at(path);
        assert_eq!(config["theirSetting"], serde_json::json!(true));
        assert_eq!(
            config["mcpServers"]["theirs"]["serverUrl"],
            "https://example.invalid/mcp"
        );
        assert_eq!(
            config["mcpServers"][MCP_SERVER_NAME]["args"],
            serde_json::json!(["_mcp"])
        );
    }

    #[test]
    fn antigravity_without_mcp_registers_nothing_and_still_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let spec = with_home(home.path(), || {
            AntigravityAdapter
                .spawn_command(&ctx(tmp.path().to_path_buf()))
                .unwrap()
                .spec
        });
        assert!(!home.path().join(ANTIGRAVITY_MCP_CONFIG).exists());
        assert!(!env_of(&spec).contains_key(ENV_MCP_URL));
        assert!(antigravity_dir_of(&spec).join("hooks.json").exists());
    }

    /// The rules directory is merged by filename and only `AGENTS.md`
    /// and `GEMINI.md` are read, so a file under any other name would
    /// sit on disk and never reach the model.
    #[test]
    fn antigravity_carries_the_compiled_instructions_as_a_rules_file() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = AntigravityAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let rules = antigravity_dir_of(&spec).join("rules").join("AGENTS.md");
        assert_eq!(std::fs::read_to_string(rules).unwrap(), REPORTING_BRIEF);
    }

    #[test]
    fn antigravity_writes_no_rules_file_without_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path().to_path_buf());
        c.compiled_instructions = String::new();
        let spec = AntigravityAdapter.spawn_command(&c).unwrap().spec;
        assert!(!antigravity_dir_of(&spec).join("rules").exists());
    }

    /// The prompt rides `--prompt-interactive`, which sends it and stays
    /// in the TUI; `--print` would answer once and exit.
    #[test]
    fn antigravity_sends_the_first_prompt_without_leaving_the_tui() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = AntigravityAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        let i = spec
            .args
            .iter()
            .position(|a| a == "--prompt-interactive")
            .unwrap();
        assert_eq!(spec.args[i + 1], "fix the tests");
        assert!(spec.args.iter().all(|a| a != "--print"));

        let bare = AntigravityAdapter
            .spawn_command(&ctx_empty_prompt(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        assert!(bare.args.iter().all(|a| a != "--prompt-interactive"));
    }

    #[test]
    fn antigravity_learns_its_conversation_id_and_resumes_by_it() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = AntigravityAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap();
        // No flag chooses the conversation id, so a fresh launch has
        // none until a hook payload reports it.
        assert_eq!(plan.agent_session_id, None);
        assert!(plan.spec.args.iter().all(|a| a != "--conversation"));

        let resumed = AntigravityAdapter
            .resume_command(&ctx(tmp.path().to_path_buf()), "ec33ebf9-0cba-4100")
            .unwrap();
        let i = resumed
            .spec
            .args
            .iter()
            .position(|a| a == "--conversation")
            .unwrap();
        assert_eq!(resumed.spec.args[i + 1], "ec33ebf9-0cba-4100");
        assert_eq!(
            resumed.agent_session_id.as_deref(),
            Some("ec33ebf9-0cba-4100")
        );
        // Resume reinstalls the customization tree, or the resumed
        // session would report nothing.
        assert!(antigravity_dir_of(&resumed.spec)
            .join("hooks.json")
            .exists());
    }

    #[test]
    fn antigravity_permission_flags_map_per_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let auto = AntigravityAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Auto,
            ))
            .unwrap()
            .spec;
        let i = auto.args.iter().position(|a| a == "--mode").unwrap();
        assert_eq!(auto.args[i + 1], "accept-edits");

        let bypass = AntigravityAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Bypass,
            ))
            .unwrap()
            .spec;
        assert!(bypass
            .args
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));

        let default = AntigravityAdapter
            .spawn_command(&ctx_with_mode(
                tmp.path().to_path_buf(),
                PermissionMode::Default,
            ))
            .unwrap()
            .spec;
        assert!(default.args.iter().all(|a| a != "--mode"));
        assert!(default
            .args
            .iter()
            .all(|a| a != "--dangerously-skip-permissions"));
    }

    /// The CLI's endpoint override also needs a setting in the user's
    /// own global file, so no profile entry can select it and a session
    /// always runs on the agent's account.
    #[test]
    fn antigravity_takes_no_model_profile_entry() {
        assert!(AntigravityAdapter.dialects().is_empty());
        let entries: Vec<ModelProfileEndpoint> = ModelDialect::ALL
            .iter()
            .map(|dialect| ModelProfileEndpoint {
                profile_id: 1,
                dialect: *dialect,
                model: "some-model".into(),
                base_url: "https://example.invalid".into(),
                background_model: String::new(),
            })
            .collect();
        assert!(select_endpoint(&AntigravityAdapter, &entries).is_err());
    }

    #[test]
    fn antigravity_session_files_stay_out_of_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = AntigravityAdapter
            .spawn_command(&ctx(tmp.path().to_path_buf()))
            .unwrap()
            .spec;
        assert_eq!(spec.cwd, PathBuf::from("/tmp/proj"));
        assert!(antigravity_dir_of(&spec).starts_with(tmp.path()));
    }
}
