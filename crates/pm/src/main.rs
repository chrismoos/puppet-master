mod attach;
mod channel;
mod fdlimit;
mod logging;
mod paths;
mod remotes;
mod sandbox;
mod tmux;
mod worker;
mod worker_cmd;
mod worker_link;
mod worker_listener;
mod worker_mcp;
mod worker_systemd;
mod worker_transcripts;
mod worker_update;

use std::path::PathBuf;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use pm_client::{Client, Target};
use pm_protocol::domain::{
    AgentKind, ClientMsg, RespondTarget, Scope, ServerMsg, Snapshot, ITEM_STATUS_CREATE_GUIDANCE,
};

#[derive(Parser)]
#[command(
    name = "pm",
    version,
    about = "Puppet Master: manage agent sessions across projects"
)]
struct Cli {
    /// Daemon unix socket path.
    #[arg(long, global = true, env = "PM_SOCKET")]
    socket: Option<PathBuf>,
    /// Log pm's own crates in more detail: once for debug, twice for
    /// trace, three times for trace everywhere including the transport
    /// libraries. Applies to `pm daemon`, `pm worker` and `pm pushgw`,
    /// which are the commands that log. `RUST_LOG` overrides it.
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Run the daemon in the foreground.
    Daemon {
        /// SQLite database path.
        #[arg(long)]
        db: Option<PathBuf>,
        /// Directory for ended-session transcripts.
        #[arg(long)]
        scrollback_dir: Option<PathBuf>,
        /// Bind address for the web UI and API.
        #[arg(long, default_value = "127.0.0.1:7676")]
        http: std::net::SocketAddr,
        /// Disable the HTTP surface entirely.
        #[arg(long)]
        no_http: bool,
        /// PEM certificate chain that serves the web UI and API over
        /// HTTPS. Browsers only allow notifications on HTTPS or
        /// localhost, so a dashboard reached at any other address needs
        /// this. Off by default, which keeps plain HTTP.
        #[arg(long, requires = "http_tls_key", value_name = "PEM")]
        http_tls_cert: Option<PathBuf>,
        /// PEM private key for --http-tls-cert.
        #[arg(long, requires = "http_tls_cert", value_name = "PEM")]
        http_tls_key: Option<PathBuf>,
        /// Bind address for the mutually authenticated worker plane.
        /// Workers reach the controller here and nowhere else. Every
        /// interface by default, because a worker on another machine
        /// and a container on this one both need an address they can
        /// route to, and the plane admits nobody without a pinned key
        /// or an unspent enrollment token. Narrow it to 127.0.0.1:7677
        /// for a controller that will only ever run its own sessions,
        /// or turn it off with --no-worker-listen.
        #[arg(long, default_value = "0.0.0.0:7677")]
        worker_listen: std::net::SocketAddr,
        /// Disable the worker plane, refusing every remote worker.
        #[arg(long)]
        no_worker_listen: bool,
        /// Disable running sessions on the controller host.
        #[arg(long)]
        no_local_worker: bool,
        /// Deprecated: use --public-url. Accepted as a hostname and
        /// turned into a public URL against the web UI's port.
        #[arg(long)]
        public_host: Option<String>,
        /// Base URL users, remote workers and published forwards reach
        /// this controller at, when that differs from what the daemon
        /// binds, such as behind a reverse proxy or on a tailnet name.
        /// The dashboard's WebSocket handshakes and its state-changing
        /// requests are allowed from this origin as well as from the
        /// address they were sent to, so a proxy that rewrites Host
        /// needs it set.
        #[arg(long)]
        public_url: Option<String>,
        /// Interface forward listeners bind. HTTP forwards use the web
        /// server unless --share-port-range gives each one a listener.
        ///
        /// Raw TCP forwards bind loopback whatever the web UI binds, because
        /// they are unauthenticated and the agent names the port it forwards
        /// to without having to own it. Setting this is what exposes them,
        /// and it exposes every one an agent publishes.
        #[arg(long)]
        forward_bind: Option<std::net::IpAddr>,
        /// Port range for raw TCP forward listeners as lo-hi (e.g. 41000-41999);
        /// defaults to OS-assigned ephemeral ports.
        #[arg(long, value_parser = parse_port_range)]
        forward_ports: Option<(u16, u16)>,
        /// Wildcard domain that mounts each HTTP forward on its own
        /// subdomain, `f<id>.<domain>`, at the root of that origin, so a
        /// preview that emits root-relative URLs works. Needs a wildcard
        /// DNS record for *.<domain> and a certificate covering it, here
        /// or on the reverse proxy in front. Takes precedence over
        /// --share-port-range.
        ///
        /// Prefer a different registrable domain from the dashboard's own
        /// host. A subdomain of it gives a preview its own origin but
        /// leaves it on the same site, so the browser still attaches the
        /// dashboard's session cookie to requests the preview makes to it,
        /// as the per-forward-port mount does. That is allowed and the
        /// daemon names it at startup.
        #[arg(long, env = "PM_SHARE_DOMAIN", value_name = "DOMAIN")]
        share_domain: Option<String>,
        /// Port range, lo-hi (e.g. 40000-40100), that mounts each HTTP
        /// forward on a listener of its own at the root of the public
        /// host. Read only when --share-domain is unset. The reverse
        /// proxy and firewall in front must pass the range.
        ///
        /// A port makes a separate origin but not a separate site, so a
        /// preview reaches no dashboard response or storage while still
        /// sharing the cookie jar. Only --share-domain, on a registrable
        /// domain of its own, makes a preview a separate site.
        #[arg(long, env = "PM_SHARE_PORT_RANGE", value_parser = parse_port_range)]
        share_port_range: Option<(u16, u16)>,
        /// Terminal silence, in milliseconds, after which an agent that
        /// installs lifecycle hooks is treated as having finished a turn
        /// it never reported. Defaults to two minutes.
        #[arg(long)]
        stale_turn_quiet_ms: Option<i64>,
        /// How long, in milliseconds, a newly launched agent may run
        /// without producing a single lifecycle hook before its hooks are
        /// treated as not working. Defaults to one minute.
        #[arg(long)]
        hook_silence_grace_ms: Option<i64>,
    },
    /// Run the push gateway relay.
    ///
    /// The relay holds no per-device or per-controller state and
    /// authenticates nothing: controllers send the device token with
    /// each push, and rate limits bound abuse.
    Pushgw(Box<PushgwArgs>),
    /// Join this machine to a controller as a worker and run the sessions
    /// it dispatches. First run needs --controller and --token; later runs
    /// reconnect from the saved config.
    Worker {
        /// Inspect or change what is configured, instead of running a
        /// worker.
        #[command(subcommand)]
        action: Option<WorkerCmd>,
        /// Controller URL, e.g. wss://host:7676. Remembered after the
        /// first run.
        #[arg(long)]
        controller: Option<String>,
        /// Accept a connection from the controller instead of dialing it,
        /// for a host the controller can route to but which cannot open
        /// outbound connections back. Binds a mutually authenticated
        /// listener at this address.
        #[arg(long)]
        listen: Option<std::net::SocketAddr>,
        /// Allow binding a wildcard address. The worker plane accepts process
        /// spawns, so binding every interface is deliberate, not a default.
        #[arg(long, requires = "listen")]
        listen_any: bool,
        /// Only accept the controller from these addresses or CIDR ranges.
        /// Repeatable; omitted accepts any source that gets through the
        /// mutual handshake.
        #[arg(long, requires = "listen", value_delimiter = ',')]
        allow_from: Vec<String>,
        /// One-time enrollment token minted by the controller.
        #[arg(long)]
        token: Option<String>,
        /// Complete registration, save the durable credential, and exit.
        #[arg(long, hide = true)]
        enroll_only: bool,
        /// Launch the worker inside a container instead of this
        /// process. The container runs a Linux pm binary as the ordinary
        /// `pm worker`.
        #[arg(long)]
        sandbox: bool,
        /// Bind-mount a host directory into the sandbox. A bare path
        /// mounts at the identical absolute path, which project spawns
        /// require. Repeatable.
        #[arg(long = "dir", value_name = "HOST[:CONTAINER][:ro|rw]")]
        dir: Vec<String>,
        /// What the runtime does when the container stops. Defaults to
        /// unless-stopped, which brings the worker back from a crash or
        /// a reboot while respecting a deliberate stop.
        #[arg(long, value_enum)]
        restart: Option<sandbox::RestartPolicy>,
        /// Stay attached to the container's output instead of returning
        /// once it is up. The runtime supervises the worker, so a
        /// launch detaches by default and prints how to reach it.
        #[arg(long)]
        foreground: bool,
        /// Identifies this worker on this machine. Each name enrols
        /// separately, with its own key, so one machine can join a
        /// controller as several hosts; a bare `pm worker --name <name>`
        /// resumes that one. Also labels the container under --sandbox.
        #[arg(long)]
        name: Option<String>,
        /// Image to run. Defaults to the stock Ubuntu system container
        /// on Incus, which is provisioned at launch, and on docker and
        /// podman to the published worker image for this version, pulled
        /// on first use. Pass the tag `make sandbox-image` builds to run
        /// a working tree instead.
        #[arg(long)]
        image: Option<String>,
        /// Environment for the containerized worker, KEY=VALUE or KEY
        /// to pass the launcher's value through. Repeatable.
        #[arg(long = "env", value_name = "KEY[=VALUE]")]
        env: Vec<String>,
        /// Container network. A network mode on docker and podman, the
        /// name of an Incus network on incus, which has no host mode.
        #[arg(long)]
        network: Option<String>,
        /// Container runtime; autodetected when omitted, preferring
        /// incus on Linux and docker elsewhere.
        #[arg(long, value_enum)]
        runtime: Option<sandbox::RuntimeKind>,
        /// Enable ID shifting for the Incus home volume. Requires idmapped mounts.
        #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
        incus_shifted_home: Option<bool>,
        /// Replace a running sandbox container even when it has live
        /// sessions.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        limits: LimitArgs,
        /// Print a systemd unit that runs this exact invocation, instead
        /// of running the worker. The enrollment token is dropped from
        /// the unit, so the command that enrolled this host can be
        /// rerun with this flag appended.
        #[arg(long)]
        systemd: bool,
        /// Write the generated unit, reload systemd, and enable it now.
        #[arg(long, requires = "systemd")]
        install: bool,
        /// Generate a system unit under /etc/systemd/system rather than
        /// a user unit under ~/.config/systemd/user.
        #[arg(long = "system", requires = "systemd")]
        system_scope: bool,
    },
    /// Manage buckets.
    Bucket {
        #[command(subcommand)]
        command: BucketCmd,
    },
    /// Manage projects.
    Project {
        #[command(subcommand)]
        command: ProjectCmd,
    },
    /// Manage model profiles: named provider accounts with one endpoint
    /// per API dialect, attachable to a bucket, project, or spawn.
    Models {
        #[command(subcommand)]
        command: ModelsCmd,
    },
    /// Manage terminals subordinate to a session.
    Terminal {
        #[command(subcommand)]
        command: TerminalCmd,
    },
    /// Spawn an agent session in a project.
    Spawn {
        #[arg(long, short)]
        project: u64,
        /// Agent to run: claude, codex, gemini, opencode, or antigravity.
        #[arg(long, default_value = "claude")]
        agent: String,
        /// Short label shown in listings; defaults to the prompt.
        #[arg(long)]
        title: Option<String>,
        /// Working directory override; defaults to the project's path.
        #[arg(long)]
        cwd: Option<String>,
        /// Permission mode: default, auto, or bypass. Omit to inherit
        /// the project (then bucket) default.
        #[arg(long)]
        permission: Option<String>,
        /// Disable the Items API for this session: its MCP server will
        /// not offer the work-item tools.
        #[arg(long)]
        no_items_api: bool,
        /// Grant the supervisor tools: the session's MCP server can
        /// spawn sessions for board items in its bucket, watch them,
        /// and type into their terminals. Spawns through those tools
        /// always use the chosen project's own path and permission
        /// mode, and may pick among the project's configured hosts.
        #[arg(long)]
        supervisor: bool,
        /// Session role: worker or supervisor. --supervisor remains a
        /// compatibility alias.
        #[arg(long)]
        role: Option<String>,
        /// Model profile id for this session; omit to inherit the
        /// project (then bucket) profile. The spawn is rejected when
        /// the profile has no endpoint the chosen agent can use.
        #[arg(long)]
        model_profile: Option<u64>,
        /// Worker to run on, as a worker id or name. Only workers the
        /// project allows are accepted; anything else is rejected with
        /// the valid choices. Omit to use the project's default worker
        /// resolution.
        #[arg(long)]
        worker: Option<String>,
        /// Initial prompt; omit to start the agent with no prompt and
        /// type in its terminal.
        prompt: Option<String>,
    },
    /// List buckets, projects, and live sessions, most recently active
    /// first.
    Ls {
        /// Include ended sessions.
        #[arg(long, short)]
        all: bool,
    },
    /// Published port forwards.
    Forwards {
        #[command(subcommand)]
        command: ForwardsCmd,
    },
    /// PR-style reviews of local work.
    Review {
        #[command(subcommand)]
        command: ReviewCmd,
    },
    /// Work items on a bucket's board.
    Items {
        #[command(subcommand)]
        command: ItemsCmd,
    },
    /// Daemon settings stored in its database.
    Config {
        #[command(subcommand)]
        command: ConfigCmd,
    },
    /// Manage versioned bucket/project instruction overlays.
    Instructions {
        #[command(subcommand)]
        command: InstructionCmd,
    },
    /// Print a bucket's latest briefing.
    Briefing {
        /// Bucket id; optional when only one bucket exists.
        #[arg(long, short)]
        bucket: Option<u64>,
    },
    /// Report the newest published build, and install it over this
    /// binary unless --check. A running daemon keeps serving the build it
    /// started with, so restart it yourself once this has replaced it.
    Update {
        /// Report what is available and change nothing.
        #[arg(long)]
        check: bool,
        /// Install this exact version instead of the newest.
        #[arg(long)]
        version: Option<String>,
        /// Follow this release channel from now on: `stable`, or a channel
        /// name such as `dev`. Installs what the channel points at, even
        /// when that is an older build than this one, and is remembered
        /// for later runs of `pm update` and for the daemon's own check.
        #[arg(long, value_name = "NAME")]
        channel: Option<String>,
    },
    /// Attach the terminal to a session (detach with Ctrl-\).
    Attach { session: u64 },
    /// Send an interrupt (Ctrl-C) to a session.
    Interrupt { session: u64 },
    /// Kill a session's agent process.
    Kill { session: u64 },
    /// Enable or disable a session's MCP APIs at runtime. Revocations
    /// apply on the session's next tool call; a newly granted tool set
    /// may need the agent's MCP client to reconnect (or the session to
    /// be resumed) before the agent notices it.
    Apis {
        /// Session id.
        session: u64,
        /// Enable or disable the work-item tools: true or false.
        #[arg(long)]
        items: Option<bool>,
        /// Enable or disable the supervisor tools: true or false.
        #[arg(long)]
        supervisor: Option<bool>,
        /// Change first-class role: worker or supervisor.
        #[arg(long)]
        role: Option<String>,
    },
    /// Full-screen dashboard (quit with q).
    Tui,
    /// Sign in to a remote controller so commands and the dashboard can
    /// reach it. Asks for the username and password of its web UI and
    /// saves the login under --name, which later commands select it by.
    Login {
        /// The controller's web address, e.g. https://pm.example.com.
        url: String,
        /// Username; asked for when omitted.
        #[arg(long)]
        username: Option<String>,
        /// Read the password from standard input instead of asking.
        #[arg(long)]
        password_stdin: bool,
        /// SHA-256 of the public key the controller's certificate must
        /// carry, in hex. For a certificate no system root vouches for;
        /// without it such a certificate is shown and confirmed
        /// interactively.
        #[arg(long, value_name = "SHA256")]
        pin: Option<String>,
        /// What the controller's device list shows this login as.
        #[arg(long)]
        device_name: Option<String>,
    },
    /// Sign out of a remote controller and forget its saved login.
    Logout,
    /// List saved logins to remote controllers.
    Remotes,
    /// Resume an ended session's agent conversation in place.
    Resume { session: u64 },
    /// Print an ended session's terminal transcript.
    Transcript {
        session: u64,
        /// Transcript directory override (defaults to the data dir).
        #[arg(long)]
        scrollback_dir: Option<PathBuf>,
    },
    /// tmux glue: fleet status, opening sessions as windows, and
    /// materializing saved workspaces as native splits.
    Tmux {
        #[command(subcommand)]
        command: TmuxCmd,
    },
    /// Internal: invoked by injected agent hooks to report lifecycle
    /// signals; reads the hook payload from stdin.
    #[command(name = "_hook", hide = true)]
    InternalHook {
        kind: String,
        /// The agent CLI running the hook, when its context differs from
        /// the shared payload shape (only Claude Code sets it).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Internal: the stdio MCP server an agent CLI launches when it
    /// cannot carry a per-session HTTP endpoint of its own. Bridges to
    /// the endpoint `PM_MCP_URL` names, as the session `PM_SESSION_TOKEN`
    /// identifies.
    #[command(name = "_mcp", hide = true)]
    InternalMcp,
}

#[derive(Subcommand)]
enum TmuxCmd {
    /// One-line fleet summary in tmux status-line markup, for
    /// status-right polling.
    Status,
    /// Open sessions as tmux windows running `pm attach`.
    Open {
        /// Session ids to open.
        sessions: Vec<u64>,
        /// Open every session currently waiting on input instead.
        #[arg(long)]
        needs_input: bool,
    },
    /// Pick a live session from a tmux menu and open it in a window.
    Pick,
    /// List saved workspaces or recreate one's split layout in tmux.
    Workspace {
        /// Workspace id or name; omit to list saved workspaces.
        workspace: Option<String>,
    },
}

/// Resource caps for a sandboxed worker. Declared once and flattened
/// into both the run and the modify command, which take the same six.
#[derive(clap::Args, Clone, Default)]
struct LimitArgs {
    /// CPU cores the sandbox may use. Unset means every core.
    #[arg(long, value_name = "N")]
    cpu: Option<String>,
    /// Memory the sandbox may use, e.g. 4GiB. Unset means unlimited.
    #[arg(long, value_name = "SIZE")]
    memory: Option<String>,
    /// Whether the sandbox may swap. Unset means it may.
    #[arg(long, value_name = "BOOL")]
    memory_swap: Option<bool>,
    /// Share of its cores the sandbox may use, e.g. 50%. Incus only.
    #[arg(long, value_name = "PCT")]
    cpu_allowance: Option<String>,
    /// Whether the memory cap is enforced hard, killing the sandbox,
    /// or soft, reclaiming only under host pressure. Incus only.
    #[arg(long, value_enum)]
    memory_enforce: Option<sandbox::MemoryEnforce>,
    /// Size of the sandbox's root disk, e.g. 40GiB. Incus only.
    #[arg(long, value_name = "SIZE")]
    disk: Option<String>,
}

impl From<LimitArgs> for sandbox::SandboxLimits {
    fn from(args: LimitArgs) -> Self {
        Self {
            cpu: args.cpu,
            memory: args.memory,
            memory_swap: args.memory_swap,
            cpu_allowance: args.cpu_allowance,
            memory_enforce: args.memory_enforce,
            disk: args.disk,
        }
    }
}

/// Managing the workers configured on this machine. Each is named, and
/// its name is both how these commands address it and what its
/// container is called.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum WorkerCmd {
    /// List the workers configured on this machine.
    #[command(visible_alias = "ls")]
    List,
    /// Change a worker's stored settings. Nothing is applied until the
    /// worker is restarted, which this prints.
    Modify {
        /// Which worker. Omitted means the only one configured.
        #[arg(long)]
        name: Option<String>,
        /// Replace the directories mounted into the container.
        #[arg(long = "dir", value_name = "HOST[:CONTAINER][:ro|rw]")]
        dir: Vec<String>,
        /// Add one directory, keeping the rest.
        #[arg(long = "add-dir", value_name = "HOST[:CONTAINER][:ro|rw]")]
        add_dir: Vec<String>,
        /// Stop mounting this host path.
        #[arg(long = "rm-dir", value_name = "HOST")]
        rm_dir: Vec<String>,
        /// Replace the environment passed in.
        #[arg(long = "env", value_name = "KEY[=VALUE]")]
        env: Vec<String>,
        #[arg(long, value_enum)]
        restart: Option<sandbox::RestartPolicy>,
        #[arg(long)]
        image: Option<String>,
        #[arg(long)]
        network: Option<String>,
        #[arg(long, value_enum)]
        runtime: Option<sandbox::RuntimeKind>,
        /// Enable ID shifting for the Incus home volume. Requires idmapped mounts.
        #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
        incus_shifted_home: Option<bool>,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Rebuild a worker's container on its current settings. Its agents
    /// are killed and resumed afterwards with their history.
    Restart {
        #[arg(long)]
        name: Option<String>,
        /// Rebuild even when the container has live sessions.
        #[arg(long)]
        force: bool,
    },
    /// Stop a worker's container, leaving it configured.
    Stop {
        #[arg(long)]
        name: Option<String>,
    },
    /// Start a worker's container again on the settings it was built
    /// with.
    Start {
        #[arg(long)]
        name: Option<String>,
    },
    /// Read a worker's container log.
    Logs {
        #[arg(long)]
        name: Option<String>,
        /// Follow the log instead of printing what there is and exiting.
        #[arg(long, short)]
        follow: bool,
    },
    /// Delete a worker's local configuration and container resources after confirmation.
    Delete {
        /// Which worker. Omitted means the only one configured.
        #[arg(long)]
        name: Option<String>,
        /// Keep the container and its persistent agent state.
        #[arg(long)]
        no_delete_resources: bool,
        /// Accept deletion without an interactive confirmation.
        #[arg(long)]
        accept_delete: bool,
    },
}

#[derive(Subcommand)]
enum BucketCmd {
    Add { name: String },
    Ls,
    Rm { id: u64 },
}

#[derive(Subcommand)]
enum ModelsCmd {
    /// List profiles with their endpoints and which agents they cover.
    /// API keys are never shown, only whether one is set.
    Ls,
    /// Show one profile in the same redacted form.
    Show { id: u64 },
    /// Create a profile. The key is stored sealed and write-only.
    Add {
        name: String,
        /// Provider API key. Required before an endpoint can set a base
        /// url, since that redirects the agent's traffic to it.
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Rename a profile or replace its key. Omitted fields are left
    /// alone, so an edit never drops the stored key.
    Edit {
        id: u64,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
        /// Drop the stored key.
        #[arg(long)]
        clear_api_key: bool,
    },
    /// Delete a profile. Refused while a bucket, project, or resumable
    /// session still references it.
    Rm { id: u64 },
    /// Create or replace the profile's endpoint for one dialect.
    Endpoint {
        profile: u64,
        /// anthropic-messages, openai-responses, or google-genai.
        dialect: String,
        /// Model the agent's main reasoning loop uses.
        #[arg(long)]
        model: String,
        /// Gateway base url; omit to pin the model on the agent CLI's
        /// own account.
        #[arg(long, default_value = "")]
        base_url: String,
        /// Cheaper model for auxiliary work off the main loop. Agents
        /// whose CLI has no such setting ignore it.
        #[arg(long, default_value = "")]
        background_model: String,
    },
    /// Delete one dialect's endpoint. Refused while a resumable session
    /// would reselect it on resume.
    RmEndpoint { profile: u64, dialect: String },
    /// Attach a profile to a bucket, or clear it with --none.
    SetBucket {
        bucket: u64,
        #[arg(long)]
        profile: Option<u64>,
        #[arg(long)]
        none: bool,
    },
    /// Override a project's profile, or clear the override with --none
    /// so it inherits its bucket.
    SetProject {
        project: u64,
        #[arg(long)]
        profile: Option<u64>,
        #[arg(long)]
        none: bool,
    },
}

#[derive(Subcommand)]
enum ForwardsCmd {
    /// List published forwards across all sessions.
    Ls,
    /// Close a forward by id (see `pm forwards ls`).
    Close { forward: u64 },
}

#[derive(Subcommand)]
enum ReviewCmd {
    /// Open a review, or attach to the one already covering the target.
    ///
    /// The worktree is stated, never inferred: a session's launch
    /// directory is fixed when it spawns and the agent may have moved
    /// or created a worktree since.
    Open {
        /// The session that will answer the review's comments.
        session: u64,
        /// Absolute path of the working tree to read.
        #[arg(long)]
        worktree: String,
        /// Base ref or SHA. Resolved against the worktree, wherever it
        /// lives, before the review is opened.
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Head ref or SHA. Omit to review the working tree.
        #[arg(long)]
        head: Option<String>,
        /// Limit the review to matching paths; each filter is its own review.
        #[arg(long = "path")]
        pathspec: Vec<String>,
        /// Review exactly these paths, skipping enumeration.
        #[arg(long = "file")]
        files: Vec<String>,
        /// What to call this review in the UI.
        #[arg(long)]
        label: Option<String>,
        /// Discard existing threads and history for this target first.
        #[arg(long)]
        reset: bool,
    },
    /// List reviews.
    Ls {
        /// Only this session's reviews.
        #[arg(long)]
        session: Option<u64>,
        /// Include finished reviews.
        #[arg(long, short)]
        all: bool,
    },
    /// Thread counts for a session's open reviews.
    Status { session: u64 },
    /// Close a review.
    Finish { review: u64 },
}

#[derive(Args)]
struct PushgwArgs {
    /// Listen address.
    #[arg(long, default_value = "127.0.0.1:8400", env = "PUSHGW_LISTEN")]
    listen: std::net::SocketAddr,
    /// Path to the APNs .p8 signing key file.
    #[arg(long, env = "PUSHGW_APNS_KEY_P8")]
    apns_key_p8: PathBuf,
    /// APNs key id (10-char identifier from the Apple developer portal).
    #[arg(long, env = "PUSHGW_APNS_KEY_ID")]
    apns_key_id: String,
    /// Path to a second .p8 signing key used only for sandbox sends.
    /// Needed when the primary key is scoped to production alone.
    /// Omitted, the primary key signs both environments.
    #[arg(
        long,
        env = "PUSHGW_APNS_SANDBOX_KEY_P8",
        requires = "apns_sandbox_key_id"
    )]
    apns_sandbox_key_p8: Option<PathBuf>,
    /// Key id of the sandbox signing key.
    #[arg(
        long,
        env = "PUSHGW_APNS_SANDBOX_KEY_ID",
        requires = "apns_sandbox_key_p8"
    )]
    apns_sandbox_key_id: Option<String>,
    /// APNs team id.
    #[arg(long, env = "PUSHGW_APNS_TEAM_ID")]
    apns_team_id: String,
    /// APNs topic (bundle id of the iOS app).
    #[arg(long, env = "PUSHGW_APNS_TOPIC")]
    apns_topic: String,
    /// Override the APNs endpoint URL (for testing).
    #[arg(long, env = "PUSHGW_APNS_ENDPOINT")]
    apns_endpoint: Option<String>,
    /// Address of a reverse proxy in front of the relay. A request from
    /// it is rate limited by the client address in its X-Forwarded-For
    /// header instead of the proxy's own. Repeat the flag, or separate
    /// addresses with commas, for more than one.
    #[arg(
        long = "trusted-proxy",
        env = "PUSHGW_TRUSTED_PROXIES",
        value_delimiter = ','
    )]
    trusted_proxies: Vec<std::net::IpAddr>,
    /// Pushes one source may send per rate window.
    #[arg(
        long,
        env = "PUSHGW_RATE_PER_SOURCE",
        default_value_t = pm_pushgw::limits::DEFAULT_PUSHES_PER_SOURCE,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    rate_per_source: u32,
    /// Pushes one device token may receive per rate window.
    #[arg(
        long,
        env = "PUSHGW_RATE_PER_TOKEN",
        default_value_t = pm_pushgw::limits::DEFAULT_PUSHES_PER_TOKEN,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    rate_per_token: u32,
    /// Length in seconds of the window both rate limits count over.
    #[arg(
        long,
        env = "PUSHGW_RATE_WINDOW_SECS",
        default_value_t = pm_pushgw::limits::DEFAULT_RATE_WINDOW_SECS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    rate_window_secs: u64,
    /// Device tokens APNs may call invalid, per source and bad-token
    /// window, before the source is blocked. 0 turns the block off.
    #[arg(
        long,
        env = "PUSHGW_BAD_TOKEN_LIMIT",
        default_value_t = pm_pushgw::limits::DEFAULT_BAD_TOKENS_PER_SOURCE
    )]
    bad_token_limit: u32,
    /// Length in seconds of the window invalid tokens are counted over.
    #[arg(
        long,
        env = "PUSHGW_BAD_TOKEN_WINDOW_SECS",
        default_value_t = pm_pushgw::limits::DEFAULT_BAD_TOKEN_WINDOW_SECS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    bad_token_window_secs: u64,
    /// How long in seconds a source stays blocked.
    #[arg(
        long,
        env = "PUSHGW_BAD_TOKEN_BLOCK_SECS",
        default_value_t = pm_pushgw::limits::DEFAULT_BLOCK_SECS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    bad_token_block_secs: u64,
}

impl PushgwArgs {
    fn limits(&self) -> pm_pushgw::limits::LimitPolicy {
        use std::time::Duration;
        pm_pushgw::limits::LimitPolicy {
            pushes_per_source: self.rate_per_source,
            pushes_per_token: self.rate_per_token,
            window: Duration::from_secs(self.rate_window_secs),
            bad_tokens_per_source: self.bad_token_limit,
            bad_token_window: Duration::from_secs(self.bad_token_window_secs),
            block_duration: Duration::from_secs(self.bad_token_block_secs),
        }
    }
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// List every setting with its current and default values.
    Ls,
    /// Set a setting; applies to sessions spawned afterwards.
    Set {
        key: String,
        /// The value. Omit it when reading from --stdin or --file.
        value: Option<String>,
        /// Read the value from standard input.
        #[arg(long, conflicts_with = "file")]
        stdin: bool,
        /// Read the value from a file.
        #[arg(long, conflicts_with = "stdin")]
        file: Option<PathBuf>,
    },
    /// Clear a setting so the built-in default applies again.
    Unset { key: String },
}

#[derive(Subcommand)]
enum InstructionCmd {
    Ls {
        #[arg(long)]
        bucket: u64,
        #[arg(long)]
        project: Option<u64>,
    },
    Effective {
        #[arg(long)]
        bucket: u64,
        #[arg(long)]
        project: Option<u64>,
        #[arg(long)]
        role: String,
    },
    Set {
        #[arg(long)]
        bucket: u64,
        #[arg(long)]
        project: Option<u64>,
        #[arg(long)]
        role: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long, default_value = "")]
        note: String,
        markdown: String,
    },
    Revert {
        layer: u64,
        revision: u64,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long, default_value = "")]
        note: String,
    },
}

#[derive(Subcommand)]
enum ItemsCmd {
    /// List a bucket's items. Done/dropped are hidden unless --all,
    /// snoozed unless --snoozed.
    Ls {
        /// Bucket id; optional when only one bucket exists.
        #[arg(long, short)]
        bucket: Option<u64>,
        /// Filter to these statuses (repeatable), e.g. --status inbox.
        #[arg(long = "status")]
        statuses: Vec<String>,
        /// Filter to one project id.
        #[arg(long)]
        project: Option<u64>,
        /// Include done and dropped items.
        #[arg(long, short)]
        all: bool,
        /// Include snoozed items.
        #[arg(long)]
        snoozed: bool,
    },
    /// Print one item with its body, links, and timeline.
    Show {
        id: u64,
        #[arg(long, short)]
        bucket: Option<u64>,
    },
    /// Reply to an item and optionally route it to a supervisor.
    Respond {
        id: u64,
        text: String,
        #[arg(long, short)]
        bucket: Option<u64>,
        /// Route to an existing supervisor session.
        #[arg(
            long,
            conflicts_with_all = ["new_supervisor_project", "reply_only"]
        )]
        session: Option<u64>,
        /// Spawn a new supervisor in this project and route to it.
        #[arg(long, conflicts_with_all = ["session", "reply_only"])]
        new_supervisor_project: Option<u64>,
        /// Record the reply without routing it to a session.
        #[arg(
            long,
            conflicts_with_all = ["session", "new_supervisor_project"]
        )]
        reply_only: bool,
    },
    /// Create an item.
    Add {
        /// Bucket id; optional when only one bucket exists.
        #[arg(long, short)]
        bucket: Option<u64>,
        #[arg(long)]
        project: Option<u64>,
        /// urgent, high, normal, or low.
        #[arg(long)]
        priority: Option<String>,
        /// Initial status; raw creates default to inbox when omitted.
        #[arg(long, long_help = ITEM_STATUS_CREATE_GUIDANCE)]
        status: Option<String>,
        /// Due date as YYYY-MM-DD.
        #[arg(long)]
        due: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        body: Option<String>,
        title: String,
    },
    /// Mark an item done.
    Done {
        id: u64,
        #[arg(long, short)]
        bucket: Option<u64>,
    },
    /// Drop an item (deliberately not doing it; survives re-sweeps).
    Drop {
        id: u64,
        #[arg(long, short)]
        bucket: Option<u64>,
    },
    /// Park an item until a date (YYYY-MM-DD), or clear with --clear.
    Snooze {
        id: u64,
        #[arg(long, short)]
        bucket: Option<u64>,
        until: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Delete an item outright. A swept item may return on the next
    /// sweep; prefer drop for those.
    Rm {
        id: u64,
        #[arg(long, short)]
        bucket: Option<u64>,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    Add {
        #[arg(long, short)]
        bucket: u64,
        name: String,
        path: PathBuf,
    },
    Edit {
        id: u64,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, conflicts_with = "clear_worker")]
        worker: Option<u64>,
        #[arg(long)]
        clear_worker: bool,
        #[arg(long)]
        permission: Option<String>,
    },
    Ls,
    Rm {
        id: u64,
    },
    /// Per-worker launch paths for a project.
    Path {
        #[command(subcommand)]
        command: ProjectPathCmd,
    },
}

#[derive(Subcommand)]
enum ProjectPathCmd {
    /// Set the project's launch path on one worker.
    Set {
        project: u64,
        /// Worker id or name; must be one of the project's allowed
        /// workers.
        worker: String,
        /// Absolute path on that worker; not checked against the
        /// local filesystem.
        path: String,
    },
    /// Clear the project's path on one worker so spawns there fall
    /// back to the project's configured path.
    Clear {
        project: u64,
        /// Worker id or name.
        worker: String,
    },
}

#[derive(Subcommand)]
enum TerminalCmd {
    New {
        session: u64,
        #[arg(long)]
        title: Option<String>,
    },
    Attach {
        terminal: u64,
    },
    Restart {
        terminal: u64,
    },
    Close {
        terminal: u64,
    },
    Transcript {
        terminal: u64,
        #[arg(long)]
        generation: Option<u64>,
        #[arg(long)]
        scrollback_dir: Option<PathBuf>,
    },
}

impl Command {
    /// Whether the command decides for itself what it talks to, rather
    /// than reaching whichever daemon `--name` or `--socket` selects. These
    /// run a daemon, act on this machine alone, or manage the saved logins
    /// that selection reads.
    fn chooses_its_own_daemon(&self) -> bool {
        matches!(
            self,
            Command::Daemon { .. }
                | Command::Pushgw(_)
                | Command::Worker { .. }
                | Command::Update { .. }
                | Command::Login { .. }
                | Command::Logout
                | Command::Remotes
                | Command::InternalHook { .. }
                | Command::InternalMcp
        )
    }
}

/// A parsed command line, with the two choices of daemon that clap's
/// derive does not carry: `--name`, which is added to every subcommand,
/// and whether the socket path was passed or inherited.
struct Invocation {
    cli: Cli,
    name: Option<String>,
    socket: remotes::SocketChoice,
}

impl Invocation {
    fn from_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let socket = match matches.value_source("socket") {
            Some(clap::parser::ValueSource::CommandLine) => remotes::SocketChoice::Flag,
            Some(clap::parser::ValueSource::EnvVariable) => remotes::SocketChoice::Environment,
            _ => remotes::SocketChoice::Default,
        };
        Ok(Self {
            cli: Cli::from_arg_matches(matches)?,
            name: remotes::name_flag(matches),
            socket,
        })
    }
}

/// Refuses a command that reads the controller's own files when it was
/// pointed at a remote login.
fn require_local(socket: &Target, what: &str) -> anyhow::Result<()> {
    match socket {
        Target::Unix(_) => Ok(()),
        Target::Remote(remote) => anyhow::bail!(
            "{what} are read from the controller's disk, so this command only works on the \
             machine running the daemon, not through the login {:?}",
            remote.name
        ),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let invocation =
        Invocation::from_matches(&remotes::with_name_flag(Cli::command()).get_matches())
            .unwrap_or_else(|e| e.exit());
    let cli = invocation.cli;
    let socket_path = cli.socket.unwrap_or_else(paths::default_socket_path);
    let verbosity = cli.verbose;
    let name = invocation.name;
    let socket = if cli.command.chooses_its_own_daemon() {
        Target::Unix(socket_path.clone())
    } else {
        remotes::target(name.as_deref(), invocation.socket, socket_path.clone())?
    };

    match cli.command {
        Command::Daemon {
            db,
            scrollback_dir,
            http,
            no_http,
            http_tls_cert,
            http_tls_key,
            worker_listen,
            no_worker_listen,
            public_url,
            no_local_worker,
            public_host,
            forward_bind,
            forward_ports,
            share_domain,
            share_port_range,
            stale_turn_quiet_ms,
            hook_silence_grace_ms,
        } => {
            let forward = pm_daemon::forward::ForwardConfig {
                bind: forward_bind,
                port_range: forward_ports,
                share_domain,
                share_port_range,
                response_timeout: None,
            };
            let http = (!no_http).then_some(http);
            let http_tls = http_tls_cert
                .zip(http_tls_key)
                .map(|(cert_chain, key)| pm_daemon::HttpTls { cert_chain, key });
            run_daemon(
                verbosity,
                socket_path,
                db,
                scrollback_dir,
                DaemonReach {
                    http,
                    http_tls,
                    worker: (!no_worker_listen).then_some(worker_listen),
                    public_url: migrate_public_host(public_url, public_host, http),
                },
                !no_local_worker,
                forward,
                TurnWatchdog {
                    stale_turn_quiet_ms,
                    hook_silence_grace_ms,
                },
            )
            .await
        }
        Command::Pushgw(args) => {
            logging::init(verbosity);
            let limits = args.limits();
            pm_pushgw::serve(pm_pushgw::ServeConfig {
                listen: args.listen,
                apns_key_p8_path: args.apns_key_p8,
                apns_key_id: args.apns_key_id,
                apns_sandbox_key_p8_path: args.apns_sandbox_key_p8,
                apns_sandbox_key_id: args.apns_sandbox_key_id,
                apns_team_id: args.apns_team_id,
                apns_topic: args.apns_topic,
                apns_endpoint: args.apns_endpoint,
                trusted_proxies: args.trusted_proxies,
                limits,
            })
            .await
            .map_err(anyhow::Error::msg)
        }
        Command::Worker {
            action,
            controller,
            token,
            enroll_only,
            listen,
            listen_any,
            allow_from,
            sandbox,
            dir,
            restart,
            foreground,
            name,
            image,
            env,
            network,
            runtime,
            incus_shifted_home,
            force,
            limits,
            systemd,
            install,
            system_scope,
        } => {
            if let Some(action) = action {
                return worker_cmd::run(action, &socket_path).await;
            }
            let limits = sandbox::SandboxLimits::from(limits);
            // Nesting is refused before anything else: inside a
            // container the answer is the same whatever is configured,
            // and a profile-shaped complaint would send the reader after
            // the wrong thing entirely.
            if sandbox {
                sandbox::refuse_re_sandbox(std::env::var_os(sandbox::INNER_MARKER).is_some())?;
            }
            // The worker is named locally and its profile describes how
            // it runs, so the profile decides, not this command line.
            let profile = worker::select(name.as_deref())?;
            let configured = worker::exists(&profile);
            let stored = worker::sandbox_of(&profile);
            let flags = worker_cmd::ConfigFlags {
                dirs: &dir,
                env: &env,
                network: network.as_deref(),
                restart,
                image: image.as_deref(),
                runtime,
                incus_shifted_home,
                limits: &limits,
                sandbox,
                controller: controller.as_deref(),
                listen: listen.is_some(),
            };
            let stored_controller = worker::stored_controller(&profile);
            let held = worker_cmd::Stored {
                sandbox: stored.as_ref(),
                controller: stored_controller.as_deref(),
            };
            worker_cmd::refuse_config_flags(
                &profile,
                configured,
                stored.is_some(),
                token.is_some(),
                &flags,
                &held,
            )?;
            // A worker whose profile records a container runs in one
            // however it is started, which is what makes a bare
            // `pm worker` bring back what was there.
            let sandboxed = sandbox || stored.is_some();
            if !sandboxed {
                worker_cmd::refuse_container_flags_without_a_container(&profile, &flags, &held)?;
            }
            if systemd {
                if token.is_some() {
                    let allow_from = allow_from
                        .iter()
                        .map(|text| worker_listener::Cidr::parse(text))
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    worker::run(
                        verbosity,
                        controller.clone(),
                        token,
                        listen,
                        listen_any,
                        allow_from,
                        name.clone(),
                        true,
                    )
                    .await?;
                }
                return worker_systemd::run(worker_systemd::Request {
                    scope: if system_scope {
                        worker_systemd::Scope::System
                    } else {
                        worker_systemd::Scope::User
                    },
                    install,
                    sandbox: sandboxed,
                    invocation: worker_systemd::Invocation { name, verbosity },
                });
            }
            if sandboxed {
                let settings = worker_cmd::resolve_sandbox(stored, &flags);
                sandbox::validate_incus_shifted_home(
                    settings.runtime,
                    settings.incus_shifted_home,
                )?;
                // Recorded before the launch, so a container that comes
                // up is described by what is on disk even if the launch
                // itself then fails.
                worker::set_sandbox(&profile, Some(settings.clone()))?;
                worker::remember_launch_connection(
                    &profile,
                    controller.as_deref(),
                    listen,
                    listen_any,
                    &allow_from,
                )?;
                let mut args = sandbox::args_from_profile(&profile, settings, force, false);
                args.controller = controller;
                args.token = token;
                args.listen = listen;
                args.listen_any = listen_any;
                args.allow_from = allow_from;
                args.foreground = foreground;
                sandbox::launch(args, &socket_path).await
            } else {
                let allow_from = allow_from
                    .iter()
                    .map(|text| worker_listener::Cidr::parse(text))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                worker::run(
                    verbosity,
                    controller,
                    token,
                    listen,
                    listen_any,
                    allow_from,
                    name,
                    enroll_only,
                )
                .await
            }
        }
        Command::Bucket { command } => match command {
            BucketCmd::Add { name } => {
                let id = request(
                    &socket,
                    ClientMsg::CreateBucket {
                        name,
                        allowed_worker_ids: vec![pm_protocol::domain::LOCAL_WORKER_ID],
                        default_worker_id: pm_protocol::domain::LOCAL_WORKER_ID,
                        is_default: false,
                    },
                )
                .await?;
                println!("bucket {} created", id.unwrap_or_default());
                Ok(())
            }
            BucketCmd::Ls => print_listing(&socket, ListingKind::Buckets).await,
            BucketCmd::Rm { id } => {
                request(&socket, ClientMsg::DeleteBucket { id }).await?;
                println!("bucket {id} deleted");
                Ok(())
            }
        },
        Command::Project { command } => match command {
            ProjectCmd::Add { bucket, name, path } => {
                let path = std::fs::canonicalize(&path)
                    .map_err(|e| anyhow::anyhow!("project path {}: {e}", path.display()))?;
                let id = request(
                    &socket,
                    ClientMsg::CreateProject {
                        bucket_id: bucket,
                        name,
                        path: path.display().to_string(),
                        worker_id: None,
                        allowed_worker_ids: Vec::new(),
                    },
                )
                .await?;
                println!("project {} created", id.unwrap_or_default());
                Ok(())
            }
            ProjectCmd::Edit {
                id,
                path,
                worker,
                clear_worker,
                permission,
            } => {
                if path.is_none() && worker.is_none() && !clear_worker && permission.is_none() {
                    anyhow::bail!(
                        "pass at least one of --path, --worker, --clear-worker, or --permission"
                    );
                }
                let permission_mode = permission
                    .as_deref()
                    .map(|mode| {
                        pm_protocol::domain::PermissionMode::parse(mode).ok_or_else(|| {
                            anyhow::anyhow!(
                                "unknown permission mode {mode:?}, expected inherit, default, auto, or bypass"
                            )
                        })
                    })
                    .transpose()?;
                let worker_id = if clear_worker {
                    Some(None)
                } else {
                    worker.map(Some)
                };
                request(
                    &socket,
                    ClientMsg::UpdateProject {
                        project_id: id,
                        path: path.map(|path| path.display().to_string()),
                        permission_mode,
                        worker_id,
                    },
                )
                .await?;
                println!("project {id} updated");
                Ok(())
            }
            ProjectCmd::Ls => print_listing(&socket, ListingKind::Projects).await,
            ProjectCmd::Rm { id } => {
                request(&socket, ClientMsg::DeleteProject { id }).await?;
                println!("project {id} deleted");
                Ok(())
            }
            ProjectCmd::Path { command } => match command {
                ProjectPathCmd::Set {
                    project,
                    worker,
                    path,
                } => {
                    request(
                        &socket,
                        ClientMsg::SetProjectWorkerPath {
                            project_id: project,
                            worker: worker.clone(),
                            path: Some(path),
                        },
                    )
                    .await?;
                    println!("project {project} path on worker {worker} set");
                    Ok(())
                }
                ProjectPathCmd::Clear { project, worker } => {
                    request(
                        &socket,
                        ClientMsg::SetProjectWorkerPath {
                            project_id: project,
                            worker: worker.clone(),
                            path: None,
                        },
                    )
                    .await?;
                    println!("project {project} path on worker {worker} cleared");
                    Ok(())
                }
            },
        },
        Command::Models { command } => match command {
            ModelsCmd::Ls => {
                let snapshot = fetch_snapshot(&socket).await?;
                print_model_profiles(&snapshot, None);
                Ok(())
            }
            ModelsCmd::Show { id } => {
                let snapshot = fetch_snapshot(&socket).await?;
                if !snapshot.model_profiles.iter().any(|p| p.id == id) {
                    anyhow::bail!("no model profile {id}");
                }
                print_model_profiles(&snapshot, Some(id));
                Ok(())
            }
            ModelsCmd::Add { name, api_key } => {
                let id = request(&socket, ClientMsg::CreateModelProfile { name, api_key }).await?;
                println!("model profile {} created", id.unwrap_or_default());
                Ok(())
            }
            ModelsCmd::Edit {
                id,
                name,
                api_key,
                clear_api_key,
            } => {
                if name.is_none() && api_key.is_none() && !clear_api_key {
                    anyhow::bail!("pass at least one of --name, --api-key, or --clear-api-key");
                }
                request(
                    &socket,
                    ClientMsg::UpdateModelProfile {
                        id,
                        name,
                        api_key,
                        clear_api_key,
                    },
                )
                .await?;
                println!("model profile {id} updated");
                Ok(())
            }
            ModelsCmd::Rm { id } => {
                request(&socket, ClientMsg::DeleteModelProfile { id }).await?;
                println!("model profile {id} deleted");
                Ok(())
            }
            ModelsCmd::Endpoint {
                profile,
                dialect,
                model,
                base_url,
                background_model,
            } => {
                let dialect = parse_dialect(&dialect)?;
                request(
                    &socket,
                    ClientMsg::SetModelProfileEndpoint {
                        profile_id: profile,
                        dialect,
                        model,
                        base_url,
                        background_model,
                    },
                )
                .await?;
                println!("model profile {profile} endpoint {} set", dialect.as_str());
                Ok(())
            }
            ModelsCmd::RmEndpoint { profile, dialect } => {
                let dialect = parse_dialect(&dialect)?;
                request(
                    &socket,
                    ClientMsg::DeleteModelProfileEndpoint {
                        profile_id: profile,
                        dialect,
                    },
                )
                .await?;
                println!(
                    "model profile {profile} endpoint {} deleted",
                    dialect.as_str()
                );
                Ok(())
            }
            ModelsCmd::SetBucket {
                bucket,
                profile,
                none,
            } => {
                let model_profile_id = model_profile_choice(profile, none)?;
                request(
                    &socket,
                    ClientMsg::SetBucketModelProfile {
                        bucket_id: bucket,
                        model_profile_id,
                    },
                )
                .await?;
                println!("bucket {bucket} model profile updated");
                Ok(())
            }
            ModelsCmd::SetProject {
                project,
                profile,
                none,
            } => {
                let model_profile_id = model_profile_choice(profile, none)?;
                request(
                    &socket,
                    ClientMsg::SetProjectModelProfile {
                        project_id: project,
                        model_profile_id,
                    },
                )
                .await?;
                println!("project {project} model profile updated");
                Ok(())
            }
        },
        Command::Terminal { command } => match command {
            TerminalCmd::New { session, title } => {
                require_session(&socket, session).await?;
                let id = request(
                    &socket,
                    ClientMsg::CreateShell {
                        session_id: session,
                        title: title.unwrap_or_default(),
                    },
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("daemon did not return the created terminal id"))?;
                println!("terminal {id} created");
                Ok(())
            }
            TerminalCmd::Attach { terminal } => attach::run_terminal(&socket, terminal).await,
            TerminalCmd::Restart { terminal } => {
                request(
                    &socket,
                    ClientMsg::RestartTerminal {
                        terminal_id: terminal,
                    },
                )
                .await?;
                println!("terminal {terminal} restarted");
                Ok(())
            }
            TerminalCmd::Close { terminal } => {
                request(
                    &socket,
                    ClientMsg::CloseTerminal {
                        terminal_id: terminal,
                    },
                )
                .await?;
                println!("terminal {terminal} closed");
                Ok(())
            }
            TerminalCmd::Transcript {
                terminal,
                generation,
                scrollback_dir,
            } => {
                require_local(&socket, "transcripts")?;
                let (terminal_row, session) = terminal_snapshot(&socket, terminal).await?;
                if session.worker_id != pm_protocol::domain::LOCAL_WORKER_ID {
                    anyhow::bail!(
                        "terminal transcripts on remote worker {} are unavailable through this CLI",
                        session.worker_id
                    );
                }
                let generation = generation.unwrap_or(terminal_row.generation);
                let dir = scrollback_dir.unwrap_or_else(paths::default_scrollback_dir);
                let path = dir.join(format!("terminal-{terminal}-generation-{generation}.bin"));
                let data = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("no transcript at {}: {e}", path.display()))?;
                use std::io::Write;
                std::io::stdout().write_all(&data)?;
                Ok(())
            }
        },
        Command::Spawn {
            project,
            agent,
            title,
            cwd,
            permission,
            no_items_api,
            supervisor,
            role,
            model_profile,
            worker,
            prompt,
        } => {
            let agent = AgentKind::parse(&agent)
                .filter(|kind| AgentKind::SELECTABLE.contains(kind))
                .ok_or_else(|| {
                    let valid: Vec<&str> =
                        AgentKind::SELECTABLE.iter().map(|k| k.as_str()).collect();
                    anyhow::anyhow!(
                        "unknown agent {agent:?}, expected one of {}",
                        valid.join(", ")
                    )
                })?;
            let permission = match permission {
                None => pm_protocol::domain::PermissionMode::Inherit,
                Some(m) => pm_protocol::domain::PermissionMode::parse(&m).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown permission mode {m:?}, expected default, auto, or bypass"
                    )
                })?,
            };
            let prompt = prompt.unwrap_or_default();
            let role = match role.as_deref() {
                Some(v) => pm_protocol::domain::SessionRole::parse(v).ok_or_else(|| {
                    anyhow::anyhow!("unknown role {v:?}, expected worker or supervisor")
                })?,
                None if supervisor => pm_protocol::domain::SessionRole::Supervisor,
                None => pm_protocol::domain::SessionRole::Worker,
            };
            let title = title.unwrap_or_else(|| prompt.chars().take(60).collect());
            let (initial_cols, initial_rows) = crossterm::terminal::size()
                .map(|(c, r)| (Some(c), Some(r)))
                .unwrap_or((None, None));
            let id = request(
                &socket,
                ClientMsg::SpawnSession {
                    project_id: project,
                    agent: Some(agent),
                    task_title: title,
                    task_prompt: prompt,
                    cwd: cwd.unwrap_or_default(),
                    permission_mode: permission,
                    worker_id: None,
                    items_api: !no_items_api,
                    supervisor_api: supervisor
                        || role == pm_protocol::domain::SessionRole::Supervisor,
                    model_profile_id: model_profile,
                    host: worker.unwrap_or_default(),
                    initial_cols,
                    initial_rows,
                },
            )
            .await?;
            let id = id.unwrap_or_default();
            println!("session {id} spawned, attach with: pm attach {id}");
            Ok(())
        }
        Command::Ls { all } => print_listing(&socket, ListingKind::Everything { all }).await,
        Command::Items { command } => {
            match command {
                ItemsCmd::Ls {
                    bucket,
                    statuses,
                    project,
                    all,
                    snoozed,
                } => {
                    let bucket_id = resolve_bucket(&socket, bucket).await?;
                    let statuses = statuses
                        .iter()
                        .map(|s| parse_status_arg(s))
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    let data = request_data(
                        &socket,
                        ClientMsg::ListItems(pm_protocol::domain::ItemQuery {
                            bucket_id,
                            statuses,
                            project_id: project,
                            updated_since_unix_ms: None,
                            include_closed: all,
                            include_snoozed: snoozed,
                            ..Default::default()
                        }),
                    )
                    .await?;
                    let items: Vec<serde_json::Value> = serde_json::from_slice(&data)?;
                    if items.is_empty() {
                        println!("no items on bucket {bucket_id}'s board");
                    } else {
                        print_items_table(&items);
                    }
                    Ok(())
                }
                ItemsCmd::Show { id, bucket } => {
                    let bucket_id = resolve_bucket(&socket, bucket).await?;
                    let data = request_data(
                        &socket,
                        ClientMsg::ItemNotes {
                            bucket_id,
                            item_id: id,
                        },
                    )
                    .await?;
                    let detail: serde_json::Value = serde_json::from_slice(&data)?;
                    print_item_detail(&detail);
                    Ok(())
                }
                ItemsCmd::Respond {
                    id,
                    text,
                    bucket,
                    session,
                    new_supervisor_project,
                    reply_only,
                } => {
                    let bucket_id = resolve_bucket(&socket, bucket).await?;
                    let target = if let Some(session_id) = session {
                        RespondTarget::Session(session_id)
                    } else if let Some(project_id) = new_supervisor_project {
                        RespondTarget::NewSupervisor { project_id }
                    } else if reply_only {
                        RespondTarget::ReplyOnly
                    } else {
                        default_respond_target(&fetch_snapshot(&socket).await?, bucket_id, id)?
                    };
                    let routed_session = request(
                        &socket,
                        ClientMsg::RespondToItem {
                            bucket_id,
                            item_id: id,
                            text,
                            target,
                        },
                    )
                    .await?;
                    match routed_session {
                        Some(session_id) => {
                            println!("replied to pm:item/{bucket_id}/{id}, routed to session {session_id}")
                        }
                        None => println!("replied to pm:item/{bucket_id}/{id} (no session)"),
                    }
                    Ok(())
                }
                ItemsCmd::Add {
                    bucket,
                    project,
                    priority,
                    status,
                    due,
                    url,
                    body,
                    title,
                } => {
                    let bucket_id = resolve_bucket(&socket, bucket).await?;
                    let write = pm_protocol::domain::ItemWrite {
                        bucket_id,
                        title: Some(title),
                        body,
                        status: status.as_deref().map(parse_status_arg).transpose()?,
                        priority: priority.as_deref().map(parse_priority_arg).transpose()?,
                        url,
                        project_id: project,
                        due_at_unix_ms: due.as_deref().map(parse_date_arg).transpose()?,
                        ..Default::default()
                    };
                    let id = request(&socket, ClientMsg::UpsertItem(Box::new(write)))
                        .await?
                        .unwrap_or_default();
                    println!("pm:item/{bucket_id}/{id} created");
                    Ok(())
                }
                ItemsCmd::Done { id, bucket } => {
                    let bucket_id = resolve_bucket(&socket, bucket).await?;
                    set_item_status(
                        &socket,
                        bucket_id,
                        id,
                        pm_protocol::domain::ItemStatus::Done,
                    )
                    .await?;
                    println!("pm:item/{bucket_id}/{id} done");
                    Ok(())
                }
                ItemsCmd::Drop { id, bucket } => {
                    let bucket_id = resolve_bucket(&socket, bucket).await?;
                    set_item_status(
                        &socket,
                        bucket_id,
                        id,
                        pm_protocol::domain::ItemStatus::Dropped,
                    )
                    .await?;
                    println!("pm:item/{bucket_id}/{id} dropped");
                    Ok(())
                }
                ItemsCmd::Snooze {
                    id,
                    bucket,
                    until,
                    clear,
                } => {
                    let bucket_id = resolve_bucket(&socket, bucket).await?;
                    let until_unix_ms = match (until, clear) {
                        (Some(date), false) => Some(parse_date_arg(&date)?),
                        (None, true) => None,
                        _ => anyhow::bail!("pass a date (YYYY-MM-DD) or --clear"),
                    };
                    request(
                        &socket,
                        ClientMsg::SnoozeItem {
                            bucket_id,
                            id,
                            until_unix_ms,
                        },
                    )
                    .await?;
                    match until_unix_ms {
                        Some(_) => println!("pm:item/{bucket_id}/{id} snoozed"),
                        None => println!("pm:item/{bucket_id}/{id} unsnoozed"),
                    }
                    Ok(())
                }
                ItemsCmd::Rm { id, bucket } => {
                    let bucket_id = resolve_bucket(&socket, bucket).await?;
                    request(&socket, ClientMsg::DeleteItem { bucket_id, id }).await?;
                    println!("pm:item/{bucket_id}/{id} deleted");
                    Ok(())
                }
            }
        }
        Command::Config { command } => match command {
            ConfigCmd::Ls => {
                let data = request_data(&socket, ClientMsg::ListSettings).await?;
                let settings: Vec<serde_json::Value> = serde_json::from_slice(&data)?;
                for setting in settings {
                    let set = setting["set"].as_bool().unwrap_or(false);
                    println!(
                        "{} = {}{}",
                        setting["key"].as_str().unwrap_or_default(),
                        setting["value"].as_str().unwrap_or_default(),
                        if set { "" } else { " (default)" },
                    );
                    println!(
                        "    {}",
                        setting["description"].as_str().unwrap_or_default()
                    );
                }
                Ok(())
            }
            ConfigCmd::Set {
                key,
                value,
                stdin,
                file,
            } => {
                let value = resolve_setting_value(value, stdin, file)?;
                request(
                    &socket,
                    ClientMsg::SetSetting {
                        key: key.clone(),
                        value: Some(value.clone()),
                    },
                )
                .await?;
                if pm_daemon::push::SECRET_SETTINGS.contains(&key.as_str()) {
                    println!("{key} set (applies to sessions spawned from now on)");
                } else {
                    println!("{key} = {value} (applies to sessions spawned from now on)");
                }
                Ok(())
            }
            ConfigCmd::Unset { key } => {
                request(
                    &socket,
                    ClientMsg::SetSetting {
                        key: key.clone(),
                        value: None,
                    },
                )
                .await?;
                println!("{key} reset to its default");
                Ok(())
            }
        },
        Command::Instructions { command } => {
            use pm_protocol::domain::{InstructionTarget, SessionRole};
            match command {
                InstructionCmd::Ls { bucket, project } => {
                    let data = request_data(
                        &socket,
                        ClientMsg::ListInstructions {
                            bucket_id: bucket,
                            project_id: project,
                        },
                    )
                    .await?;
                    println!("{}", String::from_utf8_lossy(&data));
                }
                InstructionCmd::Effective {
                    bucket,
                    project,
                    role,
                } => {
                    let role = SessionRole::parse(&role)
                        .ok_or_else(|| anyhow::anyhow!("role must be worker or supervisor"))?;
                    let data = request_data(
                        &socket,
                        ClientMsg::GetEffectiveInstructions {
                            bucket_id: bucket,
                            project_id: project,
                            role,
                        },
                    )
                    .await?;
                    print!("{}", String::from_utf8_lossy(&data));
                }
                InstructionCmd::Set {
                    bucket,
                    project,
                    role,
                    expected_revision,
                    note,
                    markdown,
                } => {
                    let target = InstructionTarget::parse(&role).ok_or_else(|| {
                        anyhow::anyhow!("role must be all, worker, or supervisor")
                    })?;
                    request(
                        &socket,
                        ClientMsg::SetInstructions {
                            bucket_id: bucket,
                            project_id: project,
                            target,
                            markdown,
                            expected_revision,
                            note,
                        },
                    )
                    .await?;
                    println!("instructions updated");
                }
                InstructionCmd::Revert {
                    layer,
                    revision,
                    expected_revision,
                    note,
                } => {
                    request(
                        &socket,
                        ClientMsg::RevertInstructions {
                            layer_id: layer,
                            revision,
                            expected_revision,
                            note,
                        },
                    )
                    .await?;
                    println!("instruction layer reverted");
                }
            }
            Ok(())
        }
        Command::Briefing { bucket } => {
            let snapshot = fetch_snapshot(&socket).await?;
            let bucket_id = bucket_from_snapshot(&snapshot, bucket)?;
            match snapshot.briefings.iter().find(|b| b.bucket_id == bucket_id) {
                Some(b) => {
                    println!(
                        "briefing for bucket {bucket_id}, as of {}",
                        fmt_ts(b.ts_unix_ms)
                    );
                    println!();
                    println!("{}", b.markdown);
                }
                None => println!("no briefing posted for bucket {bucket_id} yet"),
            }
            Ok(())
        }
        Command::Review { command } => match command {
            ReviewCmd::Open {
                session,
                worktree,
                base,
                head,
                pathspec,
                files,
                label,
                reset,
            } => {
                require_session(&socket, session).await?;
                // The CLI keeps the ref convenience the MCP contract
                // drops, by resolving against the worktree first —
                // which it cannot do itself when that tree is on
                // another host.
                let base = resolve_rev(&socket, session, &worktree, &base).await?;
                let head = match head {
                    Some(head) => resolve_rev(&socket, session, &worktree, &head).await?,
                    None => String::new(),
                };
                let id = request(
                    &socket,
                    ClientMsg::OpenReview {
                        session_id: session,
                        worktree,
                        base,
                        head,
                        pathspec,
                        files,
                        source_file: String::new(),
                        label: label.unwrap_or_default(),
                        reset,
                    },
                )
                .await?;
                match id {
                    Some(id) => println!("review {id} open"),
                    None => println!("review open"),
                }
                Ok(())
            }
            ReviewCmd::Ls { session, all } => {
                let data = request_data(
                    &socket,
                    ClientMsg::ListReviews {
                        session_id: session.unwrap_or(0),
                        include_finished: all,
                    },
                )
                .await?;
                print_reviews(&data)
            }
            ReviewCmd::Status { session } => {
                let data = request_data(
                    &socket,
                    ClientMsg::ListReviews {
                        session_id: session,
                        include_finished: false,
                    },
                )
                .await?;
                print_reviews(&data)
            }
            ReviewCmd::Finish { review } => {
                request(&socket, ClientMsg::FinishReview { review_id: review }).await?;
                println!("review {review} finished");
                Ok(())
            }
        },
        Command::Forwards { command } => match command {
            ForwardsCmd::Ls => print_listing(&socket, ListingKind::Forwards).await,
            ForwardsCmd::Close { forward } => {
                request(
                    &socket,
                    ClientMsg::CloseForward {
                        forward_id: forward,
                    },
                )
                .await?;
                println!("forward {forward} closed");
                Ok(())
            }
        },
        Command::Update {
            check,
            version,
            channel,
        } => channel::run_update(check, version.as_deref(), channel.as_deref()).await,
        Command::Attach { session } => {
            require_session(&socket, session).await?;
            attach::run(&socket, session).await
        }
        Command::Interrupt { session } => {
            require_session(&socket, session).await?;
            request(
                &socket,
                ClientMsg::InterruptSession {
                    session_id: session,
                },
            )
            .await?;
            println!("interrupt sent to session {session}");
            Ok(())
        }
        Command::Kill { session } => {
            require_session(&socket, session).await?;
            request(
                &socket,
                ClientMsg::KillSession {
                    session_id: session,
                },
            )
            .await?;
            println!("kill sent to session {session}");
            Ok(())
        }
        Command::Apis {
            session,
            items,
            supervisor,
            role,
        } => {
            if items.is_none() && supervisor.is_none() && role.is_none() {
                anyhow::bail!("pass --role, --items, and/or legacy --supervisor");
            }
            let role = role
                .as_deref()
                .map(|v| {
                    pm_protocol::domain::SessionRole::parse(v).ok_or_else(|| {
                        anyhow::anyhow!("unknown role {v:?}, expected worker or supervisor")
                    })
                })
                .transpose()?;
            require_session(&socket, session).await?;
            request(
                &socket,
                ClientMsg::UpdateSessionApis {
                    session_id: session,
                    items_api: items,
                    supervisor_api: supervisor,
                    role,
                },
            )
            .await?;
            println!("session {session} APIs updated");
            Ok(())
        }
        Command::Tui => Ok(pm_tui::run(socket).await?),
        Command::Login {
            url,
            username,
            password_stdin,
            pin,
            device_name,
        } => {
            remotes::login(remotes::LoginArgs {
                url,
                name,
                username,
                password_stdin,
                pin,
                device_name,
            })
            .await
        }
        Command::Logout => remotes::logout(name).await,
        Command::Remotes => remotes::list(),
        Command::Resume { session } => {
            require_session(&socket, session).await?;
            let id = request(
                &socket,
                ClientMsg::ResumeSession {
                    session_id: session,
                },
            )
            .await?;
            let id = id.unwrap_or(session);
            println!("session {id} resumed, attach with: pm attach {id}");
            Ok(())
        }
        Command::Transcript {
            session,
            scrollback_dir,
        } => {
            require_local(&socket, "transcripts")?;
            let snapshot = require_session(&socket, session).await?;
            let terminal = snapshot
                .terminals
                .into_iter()
                .find(|t| {
                    t.session_id == session && t.kind == pm_protocol::domain::TerminalKind::Agent
                })
                .ok_or_else(|| anyhow::anyhow!("agent terminal for session {session} not found"))?;
            let dir = scrollback_dir.unwrap_or_else(paths::default_scrollback_dir);
            let path = dir.join(format!(
                "terminal-{}-generation-{}.bin",
                terminal.id, terminal.generation
            ));
            let data = std::fs::read(&path)
                .map_err(|e| anyhow::anyhow!("no transcript at {}: {e}", path.display()))?;
            use std::io::Write;
            std::io::stdout().write_all(&data)?;
            Ok(())
        }
        Command::Tmux { command } => match command {
            TmuxCmd::Status => tmux::status(&socket).await,
            TmuxCmd::Open {
                sessions,
                needs_input,
            } => tmux::open(&socket, sessions, needs_input).await,
            TmuxCmd::Pick => tmux::pick(&socket).await,
            TmuxCmd::Workspace { workspace } => {
                tmux::workspace(&socket, workspace.as_deref()).await
            }
        },
        Command::InternalHook { kind, agent } => run_hook(&socket, &kind, agent.as_deref()).await,
        Command::InternalMcp => run_mcp_bridge().await,
    }
}

async fn terminal_snapshot(
    socket: &Target,
    terminal_id: u64,
) -> anyhow::Result<(pm_protocol::domain::Terminal, pm_protocol::domain::Session)> {
    let mut client = Client::open(socket).await?;
    client
        .request(ClientMsg::Subscribe { scope: Scope::All })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    loop {
        match client.next_msg().await {
            Some(ServerMsg::Snapshot(snapshot)) => {
                let terminal = snapshot
                    .terminals
                    .into_iter()
                    .find(|t| t.id == terminal_id)
                    .ok_or_else(|| anyhow::anyhow!("terminal {terminal_id} not found"))?;
                let session = snapshot
                    .sessions
                    .into_iter()
                    .find(|s| s.id == terminal.session_id)
                    .ok_or_else(|| anyhow::anyhow!("session {} not found", terminal.session_id))?;
                return Ok((terminal, session));
            }
            Some(_) => continue,
            None => anyhow::bail!("connection closed before terminal snapshot"),
        }
    }
}

/// Overall budget for delivering a hook signal; the agent harness
/// must never hang on a wedged daemon.
const HOOK_DELIVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The payload keys carrying the agent's own conversation id and
/// transcript path. Claude, Codex and Gemini share one snake_case
/// schema; Antigravity names the same two fields in camelCase.
const HOOK_SESSION_ID_KEYS: &[&str] = &["session_id", "conversationId"];

/// The payload key listing work the agent still has in flight after the
/// hook: a backgrounded subagent, shell command, monitor or workflow.
/// Only Claude Code reports it; a payload without it is a turn that has
/// really finished.
const HOOK_BACKGROUND_TASKS_KEY: &str = "background_tasks";
const HOOK_TRANSCRIPT_PATH_KEYS: &[&str] = &["transcript_path", "transcriptPath"];

/// Whether this payload continues a turn already under way rather than
/// starting one. Only Antigravity's per-invocation counter says so; a
/// payload without it is always a fresh submission.
fn is_hook_continuation(json: &serde_json::Value) -> bool {
    json.get("invocationNum")
        .and_then(|value| value.as_u64())
        .is_some_and(|number| number > 0)
}

fn is_antigravity_payload(json: &serde_json::Value) -> bool {
    json.get("conversationId").is_some()
        || json.get("invocationNum").is_some()
        || json.get("artifactDirectoryPath").is_some()
}

/// Whether the agent is still working despite this hook.
///
/// The key is documented as the in-flight background work registered in
/// the session, so whether it is empty is the whole question and the
/// shapes of the tasks inside it do not matter.
fn has_background_work(json: &serde_json::Value) -> bool {
    json.get(HOOK_BACKGROUND_TASKS_KEY)
        .and_then(|value| value.as_array())
        .is_some_and(|tasks| !tasks.is_empty())
}

/// The first of `keys` the payload carries as a string, empty when it
/// carries none of them.
fn payload_field(json: &serde_json::Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| json.get(*key).and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// The stdio MCP bridge an agent CLI whose per-server configuration is
/// global launches instead of posting to the daemon itself. The server
/// entry naming it is shared by every session on the host, so the
/// session it serves comes from the environment the agent inherited:
/// `PM_MCP_URL` is the endpoint and `PM_SESSION_TOKEN` the identity.
///
/// A session outside Puppet Master has neither, and the same global
/// entry still launches this. It answers there as a server with no
/// tools rather than exiting, so the agent reports a healthy connection
/// instead of a failed one.
async fn run_mcp_bridge() -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let managed = std::env::var(pm_adapters::ENV_MCP_URL)
        .ok()
        .zip(std::env::var(pm_adapters::ENV_SESSION_TOKEN).ok())
        .filter(|(url, token)| !url.is_empty() && !token.is_empty());
    let client = reqwest::Client::new();
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // A notification carries no id and expects no answer.
        let Some(id) = request.get("id").filter(|id| !id.is_null()).cloned() else {
            continue;
        };
        let response = match &managed {
            Some((url, token)) => mcp_bridge_relay(&client, url, token, &request, &id).await,
            None => mcp_bridge_unmanaged(&request, &id),
        };
        let Some(response) = response else { continue };
        out.write_all(format!("{response}\n").as_bytes()).await?;
        out.flush().await?;
    }
    Ok(())
}

/// Posts one request to the daemon's MCP endpoint and returns the line
/// to write back. A transport failure becomes a JSON-RPC error rather
/// than a dropped answer, which would hang the agent's tool call.
async fn mcp_bridge_relay(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    request: &serde_json::Value,
    id: &serde_json::Value,
) -> Option<String> {
    let sent = client
        .post(url)
        .bearer_auth(token)
        .json(request)
        .send()
        .await;
    let body = match sent {
        Ok(response) if response.status().is_success() => response.text().await.ok()?,
        Ok(response) => {
            return Some(mcp_bridge_error(
                id,
                &format!("status {}", response.status()),
            ))
        }
        Err(error) => return Some(mcp_bridge_error(id, &error.to_string())),
    };
    (!body.trim().is_empty()).then_some(body)
}

/// The answers for a session Puppet Master did not spawn: a server that
/// connects and offers nothing.
fn mcp_bridge_unmanaged(request: &serde_json::Value, id: &serde_json::Value) -> Option<String> {
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => serde_json::json!({
            "protocolVersion": pm_daemon::mcp::PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": pm_adapters::MCP_SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        }),
        "tools/list" => serde_json::json!({ "tools": [] }),
        _ => {
            return Some(mcp_bridge_error(
                id,
                "this session was not spawned by Puppet Master, so it has no reporting tools",
            ))
        }
    };
    Some(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
}

fn mcp_bridge_error(id: &serde_json::Value, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32603, "message": message },
    })
    .to_string()
}

async fn run_hook(socket: &Target, kind: &str, agent: Option<&str>) -> anyhow::Result<()> {
    use std::io::Read;

    let kind = pm_protocol::domain::HookKind::parse(kind)
        .ok_or_else(|| anyhow::anyhow!("unknown hook kind {kind:?}"))?;
    let session_token = std::env::var("PM_SESSION_TOKEN")
        .map_err(|_| anyhow::anyhow!("PM_SESSION_TOKEN not set"))?;

    let mut payload = String::new();
    let _ = std::io::stdin().read_to_string(&mut payload);
    let json =
        serde_json::from_str::<serde_json::Value>(&payload).unwrap_or(serde_json::Value::Null);
    let field = |keys: &[&str]| payload_field(&json, keys);
    let stop_hook_active = json
        .get("stop_hook_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Antigravity has no submit event: its PreInvocation hook fires
    // before every model call, several times in one turn. Only the
    // first of them is a prompt being submitted, and the counter it
    // carries restarts at zero for each new turn. Reporting the rest
    // would cancel a Supervisor's blocking wait and clear the pending
    // input flag on every step of a turn already under way.
    if kind == pm_protocol::domain::HookKind::PromptSubmitted && is_hook_continuation(&json) {
        return Ok(());
    }

    // Claude reads a SessionStart hook's stdout and injects
    // `additionalContext` into the model, re-firing on startup, resume,
    // `/clear`, and `/compact`. Emitting the reporting brief here makes
    // the contract durable across a context reset. Codex ignores the
    // stdout, so it is harmless there. Antigravity strictly parses hook
    // results as protojson and fails on unrecognized fields.
    if kind == pm_protocol::domain::HookKind::Started && !is_antigravity_payload(&json) {
        let forwards = session_forward_inventory(socket, &session_token).await;
        let claude = agent == Some(pm_adapters::CLAUDE_HOOK_AGENT);
        println!("{}", session_start_additional_context(&forwards, claude));
    }

    let detail = if kind == pm_protocol::domain::HookKind::TurnFailed {
        field(&["error"])
    } else {
        field(&["message"])
    };
    let nudge = tokio::time::timeout(
        HOOK_DELIVERY_TIMEOUT,
        request_data(
            socket,
            ClientMsg::HookEvent {
                session_token,
                kind,
                detail,
                agent_session_id: field(HOOK_SESSION_ID_KEYS),
                transcript_path: field(HOOK_TRANSCRIPT_PATH_KEYS),
                background_work: has_background_work(&json),
            },
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out delivering hook signal"))??;

    // A Stop hook whose stdout is `{"decision":"block","reason":...}` makes
    // Claude take one more turn with the reason injected. The daemon returns
    // a reason when the turn ended with no dashboard headline set.
    // Antigravity reads the same field but only re-enters its loop on
    // "continue", so the nudge reaches it as an ignored value rather
    // than a forced turn.
    if let Some(output) = stop_hook_block(kind, stop_hook_active, &nudge) {
        println!("{output}");
    }
    Ok(())
}

/// The Stop-hook block JSON to print, or None when the stop should proceed.
/// `stop_hook_active` guards against a forced-continuation loop.
fn stop_hook_block(
    kind: pm_protocol::domain::HookKind,
    stop_hook_active: bool,
    nudge: &[u8],
) -> Option<String> {
    if kind != pm_protocol::domain::HookKind::TurnEnded || stop_hook_active || nudge.is_empty() {
        return None;
    }
    let reason = String::from_utf8_lossy(nudge);
    Some(serde_json::json!({ "decision": "block", "reason": reason }).to_string())
}

/// The SessionStart hook stdout that injects the reporting brief as
/// `additionalContext`, the Claude-only brief under it when Claude Code
/// is the caller, and the session's forward inventory last when it
/// published any.
fn session_start_additional_context(forward_inventory: &str, claude: bool) -> String {
    let brief = if claude {
        pm_adapters::with_claude_harness_brief("")
    } else {
        pm_adapters::REPORTING_BRIEF.to_string()
    };
    let context = if forward_inventory.is_empty() {
        brief
    } else {
        format!("{brief}\n\n{forward_inventory}")
    };
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context,
        }
    })
    .to_string()
}

/// The calling session's forward inventory, or empty when the daemon does
/// not answer in time. This hook runs before the agent's first turn, so a
/// daemon that is slow or gone costs the inventory rather than the brief.
async fn session_forward_inventory(socket: &Target, session_token: &str) -> String {
    let asked = tokio::time::timeout(
        HOOK_DELIVERY_TIMEOUT,
        request_data(
            socket,
            ClientMsg::SessionForwardInventory {
                session_token: session_token.to_string(),
            },
        ),
    )
    .await;
    match asked {
        Ok(Ok(inventory)) => String::from_utf8_lossy(&inventory).into_owned(),
        Ok(Err(e)) => {
            eprintln!("pm: could not read this session's forwards: {e}");
            String::new()
        }
        Err(_) => {
            eprintln!("pm: timed out reading this session's forwards");
            String::new()
        }
    }
}

/// Parses a lo-hi listener port range.
fn parse_port_range(s: &str) -> Result<(u16, u16), String> {
    let (lo, hi) = s
        .split_once('-')
        .ok_or_else(|| "expected lo-hi, e.g. 41000-41999".to_string())?;
    let lo: u16 = lo.trim().parse().map_err(|_| format!("bad port {lo:?}"))?;
    let hi: u16 = hi.trim().parse().map_err(|_| format!("bad port {hi:?}"))?;
    if lo > hi {
        return Err(format!("empty range {lo}-{hi}"));
    }
    Ok((lo, hi))
}

/// Folds a legacy `--public-host` into `--public-url`, which replaced
/// it: one setting answered by two flags is one that can disagree with
/// itself. The old flag keeps working so an existing command line still
/// starts, and says on stderr what to put in the new one.
fn migrate_public_host(
    public_url: Option<String>,
    public_host: Option<String>,
    http: Option<std::net::SocketAddr>,
) -> Option<String> {
    let Some(host) = public_host else {
        return public_url;
    };
    if public_url.is_some() {
        eprintln!("warning: --public-host is deprecated and ignored because --public-url is set");
        return public_url;
    }
    let migrated = match http {
        Some(addr) => format!("http://{host}:{}", addr.port()),
        None => format!("http://{host}"),
    };
    eprintln!("warning: --public-host is deprecated, use --public-url {migrated}");
    Some(migrated)
}

/// Where the daemon listens, and the name it is reached by when that
/// differs from what it binds.
struct DaemonReach {
    http: Option<std::net::SocketAddr>,
    http_tls: Option<pm_daemon::HttpTls>,
    worker: Option<std::net::SocketAddr>,
    public_url: Option<String>,
}

/// Overrides for the turn-end watchdog's thresholds. None keeps the
/// daemon's own default.
struct TurnWatchdog {
    stale_turn_quiet_ms: Option<i64>,
    hook_silence_grace_ms: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon(
    verbosity: u8,
    socket: PathBuf,
    db: Option<PathBuf>,
    scrollback_dir: Option<PathBuf>,
    reach: DaemonReach,
    local_worker_enabled: bool,
    forward: pm_daemon::forward::ForwardConfig,
    watchdog: TurnWatchdog,
) -> anyhow::Result<()> {
    logging::init(verbosity);
    fdlimit::raise_open_files();

    let db_path = db.unwrap_or_else(paths::default_db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: watchdog.stale_turn_quiet_ms,
        hook_silence_grace_ms: watchdog.hook_silence_grace_ms,
        forward,
        db_path: Some(db_path),
        socket_path: socket,
        http_addr: reach.http,
        http_tls: reach.http_tls,
        worker_addr: reach.worker,
        public_url: reach.public_url,
        scrollback_dir: scrollback_dir.unwrap_or_else(paths::default_scrollback_dir),
        registry: pm_adapters::AdapterRegistry::standard(),
        local_worker_enabled,
        release_channel: channel::saved(&paths::update_config_path())?,
    };
    let (daemon, handle) = pm_daemon::start(config).await?;
    println!(
        "pm daemon running, socket at {}",
        handle.socket_path.display()
    );
    if let Some(addr) = handle.http_addr {
        println!("web ui at {}://{addr}/", daemon.http_scheme());
        println!("forwards mounted by {}", describe_mount(&daemon));
    }
    tokio::signal::ctrl_c().await?;
    // Give agents a chance to flush and persist their conversations
    // (so they stay resumable) before the daemon and its children go
    // away.
    let signalled = daemon.begin_shutdown();
    if signalled > 0 {
        println!("shutting down: waiting for {signalled} session(s) to exit cleanly");
        wait_for_session_shutdown(&daemon).await;
    } else {
        println!("shutting down");
    }
    handle.shutdown().await;
    Ok(())
}

/// The active forward mount in one line, so a start-up log says which
/// mode took effect rather than leaving it to be inferred from a URL.
fn describe_mount(daemon: &pm_daemon::Daemon) -> String {
    use pm_daemon::forward_mount::MountMode;
    match daemon.forward_mount_mode() {
        MountMode::ShareDomain(domain) => format!("share domain, each at <slug>.{domain}"),
        MountMode::PerForwardPort { lo, hi } => {
            format!("a listener each from ports {lo}-{hi}")
        }
        MountMode::PathPrefix => "path prefix, each at /forwards/<id>/".to_string(),
    }
}

/// How long the daemon waits after signalling its agent sessions on
/// shutdown, so they can flush their conversations before exit.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

async fn wait_for_session_shutdown(daemon: &pm_daemon::Daemon) {
    let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
    loop {
        match daemon.live_session_count() {
            Ok(0) => break,
            Ok(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Ok(n) => {
                eprintln!("shutdown: {n} session(s) did not exit before timeout");
                break;
            }
            Err(e) => {
                eprintln!("shutdown: could not check live sessions: {e}");
                break;
            }
        }
    }
}

async fn request(socket: &Target, msg: ClientMsg) -> anyhow::Result<Option<u64>> {
    let client = Client::open(socket).await?;
    client
        .request(msg)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Turns a ref into a SHA on the host that holds the tree, so
/// `--base master` works even for a worktree on a remote worker.
async fn resolve_rev(
    socket: &Target,
    session: u64,
    worktree: &str,
    rev: &str,
) -> anyhow::Result<String> {
    let data = request_data(
        socket,
        ClientMsg::ResolveRev {
            session_id: session,
            worktree: worktree.to_string(),
            rev: rev.to_string(),
        },
    )
    .await?;
    let sha = String::from_utf8_lossy(&data).trim().to_string();
    if sha.is_empty() {
        anyhow::bail!("{rev} does not resolve in {worktree}");
    }
    Ok(sha)
}

/// One line per review: what it points at and what it is waiting on.
fn print_reviews(data: &[u8]) -> anyhow::Result<()> {
    let reviews: Vec<serde_json::Value> = serde_json::from_slice(data).unwrap_or_default();
    if reviews.is_empty() {
        println!("no reviews");
        return Ok(());
    }
    for r in reviews {
        let get = |k: &str| r.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        println!(
            "{:>4}  {:<40} {:<8} {} draft, {} open, {} answered, {} resolved",
            get("id"),
            r.get("label").and_then(|v| v.as_str()).unwrap_or(""),
            r.get("state").and_then(|v| v.as_str()).unwrap_or(""),
            get("draft_count"),
            get("open_count"),
            get("answered_count"),
            get("resolved_count"),
        );
    }
    Ok(())
}

async fn request_data(socket: &Target, msg: ClientMsg) -> anyhow::Result<Vec<u8>> {
    let client = Client::open(socket).await?;
    client
        .request_data(msg)
        .await
        .map(|b| b.to_vec())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

enum ListingKind {
    Everything { all: bool },
    Buckets,
    Projects,
    Forwards,
}

async fn fetch_snapshot(socket: &Target) -> anyhow::Result<Snapshot> {
    let mut client = Client::open(socket).await?;
    client
        .request(ClientMsg::Subscribe { scope: Scope::All })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    loop {
        match client.next_msg().await {
            Some(ServerMsg::Snapshot(s)) => return Ok(s),
            Some(_) => continue,
            None => anyhow::bail!("connection closed before snapshot"),
        }
    }
}

async fn require_session(socket: &Target, session_id: u64) -> anyhow::Result<Snapshot> {
    let snapshot = fetch_snapshot(socket).await?;
    if snapshot.sessions.iter().any(|s| s.id == session_id) {
        Ok(snapshot)
    } else {
        let suggestions = format_session_suggestions(&snapshot.sessions);
        if suggestions.is_empty() {
            anyhow::bail!("session {session_id} not found");
        }
        anyhow::bail!("session {session_id} not found\n\n{suggestions}");
    }
}

fn bucket_from_snapshot(snapshot: &Snapshot, requested: Option<u64>) -> anyhow::Result<u64> {
    if let Some(id) = requested {
        return Ok(id);
    }
    match snapshot.buckets.as_slice() {
        [only] => Ok(only.id),
        [] => anyhow::bail!("no buckets yet, create one with: pm bucket add <name>"),
        many => anyhow::bail!(
            "--bucket is required; buckets: {}",
            many.iter()
                .map(|b| format!("{} ({})", b.id, b.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Resolves an optional bucket argument, defaulting to the only bucket.
async fn resolve_bucket(socket: &Target, requested: Option<u64>) -> anyhow::Result<u64> {
    if let Some(id) = requested {
        return Ok(id);
    }
    bucket_from_snapshot(&fetch_snapshot(socket).await?, None)
}

async fn set_item_status(
    socket: &Target,
    bucket_id: u64,
    id: u64,
    status: pm_protocol::domain::ItemStatus,
) -> anyhow::Result<()> {
    let write = pm_protocol::domain::ItemWrite {
        bucket_id,
        id: Some(id),
        status: Some(status),
        ..Default::default()
    };
    request(socket, ClientMsg::UpsertItem(Box::new(write))).await?;
    Ok(())
}

fn default_respond_target(
    snapshot: &Snapshot,
    bucket_id: u64,
    item_id: u64,
) -> anyhow::Result<RespondTarget> {
    let item = snapshot
        .items
        .iter()
        .find(|item| item.bucket_id == bucket_id && item.id == item_id)
        .ok_or_else(|| anyhow::anyhow!("item {item_id} not found"))?;
    if let Some(session_id) = default_respond_session(snapshot, item) {
        return Ok(RespondTarget::Session(session_id));
    }

    let mut projects = snapshot
        .projects
        .iter()
        .filter(|project| project.bucket_id == item.bucket_id)
        .collect::<Vec<_>>();
    projects.sort_by_key(|project| project.id);
    let projects = if projects.is_empty() {
        "(none)".to_string()
    } else {
        projects
            .iter()
            .map(|project| format!("{} ({})", project.id, project.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    anyhow::bail!(
        "no live supervisor session for item {item_id}\nprojects in bucket {}: {projects}\npass --new-supervisor-project <P> or --reply-only",
        item.bucket_id
    )
}

fn default_respond_session(snapshot: &Snapshot, item: &pm_protocol::domain::Item) -> Option<u64> {
    let eligible = |session: &&pm_protocol::domain::Session| {
        session.supervisor_api
            && session.state.is_live()
            && snapshot.projects.iter().any(|project| {
                project.id == session.project_id && project.bucket_id == item.bucket_id
            })
    };
    let newest = |linked_only: bool| {
        snapshot
            .sessions
            .iter()
            .filter(eligible)
            .filter(|session| !linked_only || item.session_ids.contains(&session.id))
            .max_by_key(|session| (session.last_activity_at_unix_ms, session.id))
            .map(|session| session.id)
    };
    newest(true).or_else(|| newest(false))
}

fn parse_status_arg(s: &str) -> anyhow::Result<pm_protocol::domain::ItemStatus> {
    pm_protocol::domain::ItemStatus::parse(s).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown status {s:?}, expected one of: {}",
            pm_protocol::domain::ItemStatus::ALL
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn parse_priority_arg(s: &str) -> anyhow::Result<pm_protocol::domain::ItemPriority> {
    pm_protocol::domain::ItemPriority::parse(s).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown priority {s:?}, expected one of: {}",
            pm_protocol::domain::ItemPriority::ALL
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

const MS_PER_DAY: i64 = 86_400_000;

/// Days since 1970-01-01 for a proleptic-Gregorian civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_date_arg(s: &str) -> anyhow::Result<i64> {
    let parts: Vec<&str> = s.split('-').collect();
    let parsed: Option<(i64, i64, i64)> = match parts.as_slice() {
        [y, m, d] => match (y.parse(), m.parse(), d.parse()) {
            (Ok(y), Ok(m @ 1..=12), Ok(d @ 1..=31)) => Some((y, m, d)),
            _ => None,
        },
        _ => None,
    };
    let (y, m, d) =
        parsed.ok_or_else(|| anyhow::anyhow!("unparseable date {s:?}, expected YYYY-MM-DD"))?;
    Ok(days_from_civil(y, m, d) * MS_PER_DAY)
}

fn fmt_date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

fn fmt_ts(ms: i64) -> String {
    let rem = ms.rem_euclid(MS_PER_DAY);
    format!(
        "{} {:02}:{:02} UTC",
        fmt_date(ms),
        rem / 3_600_000,
        rem % 3_600_000 / 60_000
    )
}

/// Board group/sort rank of a status string, following ItemStatus::ALL.
fn status_rank(status: &str) -> usize {
    pm_protocol::domain::ItemStatus::ALL
        .iter()
        .position(|s| s.as_str() == status)
        .unwrap_or(usize::MAX)
}

fn priority_rank(priority: &str) -> usize {
    pm_protocol::domain::ItemPriority::ALL
        .iter()
        .position(|p| p.as_str() == priority)
        .unwrap_or(usize::MAX)
}

fn item_source(item: &serde_json::Value) -> String {
    let kind = item["source_kind"].as_str().unwrap_or_default();
    let detail = item["source_detail"].as_str().unwrap_or_default();
    if detail.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}:{detail}")
    }
}

fn print_items_table(items: &[serde_json::Value]) {
    let mut items: Vec<&serde_json::Value> = items.iter().collect();
    items.sort_by_key(|i| {
        (
            status_rank(i["status"].as_str().unwrap_or_default()),
            priority_rank(i["priority"].as_str().unwrap_or_default()),
            i["id"].as_u64().unwrap_or_default(),
        )
    });
    println!(
        "{:<6} {:<16} {:<8} {:<11} {:<20} TITLE",
        "ID", "STATUS", "PRI", "DUE", "SOURCE"
    );
    for item in items {
        let due = item["due_at_unix_ms"]
            .as_i64()
            .map(fmt_date)
            .unwrap_or_else(|| "-".into());
        let mut title = item["title"].as_str().unwrap_or_default().to_string();
        let blocked_by = item["blocked_by"].as_array().map(Vec::len).unwrap_or(0);
        if blocked_by > 0 {
            title.push_str(&format!(" [blocked by {blocked_by}]"));
        }
        if item["snoozed_until_unix_ms"].is_number() {
            title.push_str(" [snoozed]");
        }
        println!(
            "{:<6} {:<16} {:<8} {:<11} {:<20} {}",
            item["id"].as_u64().unwrap_or_default(),
            item["status"].as_str().unwrap_or_default(),
            item["priority"].as_str().unwrap_or_default(),
            due,
            item_source(item),
            title,
        );
    }
}

fn print_item_detail(detail: &serde_json::Value) {
    let item = &detail["item"];
    println!(
        "item {} [{} / {}] {}",
        item["id"].as_u64().unwrap_or_default(),
        item["status"].as_str().unwrap_or_default(),
        item["priority"].as_str().unwrap_or_default(),
        item["title"].as_str().unwrap_or_default(),
    );
    if let Some(key) = item["external_key"].as_str() {
        println!("  key      {key}");
    }
    println!("  source   {}", item_source(item));
    if let Some(url) = item["url"].as_str().filter(|u| !u.is_empty()) {
        println!("  url      {url}");
    }
    if let Some(due) = item["due_at_unix_ms"].as_i64() {
        println!("  due      {}", fmt_date(due));
    }
    if let Some(until) = item["snoozed_until_unix_ms"].as_i64() {
        println!("  snoozed  until {}", fmt_date(until));
    }
    if let Some(deps) = item["blocked_by"].as_array().filter(|d| !d.is_empty()) {
        let ids: Vec<String> = deps.iter().map(|d| d.to_string()).collect();
        println!("  blocked  by item(s) {}", ids.join(", "));
    }
    if let Some(sessions) = item["session_ids"].as_array().filter(|s| !s.is_empty()) {
        let ids: Vec<String> = sessions.iter().map(|s| s.to_string()).collect();
        println!("  sessions {}", ids.join(", "));
    }
    if let Some(body) = item["body"].as_str().filter(|b| !b.is_empty()) {
        println!();
        println!("{body}");
    }
    if let Some(notes) = detail["notes"].as_array().filter(|n| !n.is_empty()) {
        println!();
        for note in notes {
            let who = note["session_id"]
                .as_u64()
                .map(|s| format!("session {s}"))
                .unwrap_or_else(|| "you".into());
            println!(
                "  {} [{}] {} — {}",
                fmt_ts(note["ts_unix_ms"].as_i64().unwrap_or_default()),
                note["kind"].as_str().unwrap_or_default(),
                who,
                note["text"].as_str().unwrap_or_default(),
            );
        }
    }
}

/// A project's sessions for the listing: live only unless `all`, most
/// recently active first.
fn listed_sessions(
    sessions: &[pm_protocol::domain::Session],
    project_id: u64,
    all: bool,
) -> Vec<&pm_protocol::domain::Session> {
    let mut listed: Vec<_> = sessions
        .iter()
        .filter(|s| s.project_id == project_id && (all || s.state.is_live()))
        .collect();
    listed.sort_by_key(|s| std::cmp::Reverse(s.last_activity_at_unix_ms));
    listed
}

fn session_name(session: &pm_protocol::domain::Session) -> &str {
    [&session.goal, &session.task_title, &session.headline]
        .into_iter()
        .find(|name| !name.is_empty())
        .map_or("(unnamed)", String::as_str)
}

fn suggested_sessions(
    sessions: &[pm_protocol::domain::Session],
) -> Vec<&pm_protocol::domain::Session> {
    let mut suggested: Vec<_> = sessions.iter().collect();
    suggested.sort_by_key(|s| std::cmp::Reverse((s.last_activity_at_unix_ms, s.id)));
    suggested.truncate(5);
    suggested
}

fn format_session_suggestions(sessions: &[pm_protocol::domain::Session]) -> String {
    let rows = suggested_sessions(sessions)
        .into_iter()
        .map(|s| format!("  {} [{}] {}", s.id, s.state.as_str(), session_name(s)))
        .collect::<Vec<_>>();
    if rows.is_empty() {
        String::new()
    } else {
        format!("Did you mean:\n{}", rows.join("\n"))
    }
}

fn parse_dialect(value: &str) -> anyhow::Result<pm_protocol::domain::ModelDialect> {
    pm_protocol::domain::ModelDialect::parse(value).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown dialect {value:?}, expected anthropic-messages, openai-responses, or google-genai"
        )
    })
}

fn model_profile_choice(profile: Option<u64>, none: bool) -> anyhow::Result<Option<u64>> {
    match (profile, none) {
        (Some(_), true) => anyhow::bail!("pass either --profile or --none, not both"),
        (None, false) => anyhow::bail!("pass --profile <id> or --none"),
        (profile, _) => Ok(profile),
    }
}

/// Prints profiles with their endpoints and derived agent coverage. The
/// key is reported only as set or unset, never by value.
fn print_model_profiles(snapshot: &Snapshot, only: Option<u64>) {
    println!(
        "{:<6} {:<24} {:<8} {:<24} AGENTS",
        "ID", "NAME", "KEY", "DIALECTS"
    );
    for profile in snapshot
        .model_profiles
        .iter()
        .filter(|profile| only.is_none_or(|id| profile.id == id))
    {
        let dialects = profile
            .endpoints
            .iter()
            .map(|entry| entry.dialect.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let agents = snapshot
            .agent_dialects
            .iter()
            .filter(|published| {
                published.dialects.iter().any(|dialect| {
                    profile
                        .endpoints
                        .iter()
                        .any(|entry| entry.dialect == *dialect)
                })
            })
            .map(|published| published.agent.as_str())
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{:<6} {:<24} {:<8} {:<24} {}",
            profile.id,
            profile.name,
            if profile.key_set { "set" } else { "unset" },
            if dialects.is_empty() { "-" } else { &dialects },
            if agents.is_empty() { "none" } else { &agents },
        );
        if only.is_none() {
            continue;
        }
        for entry in &profile.endpoints {
            println!(
                "  {:<22} model={} base_url={} background_model={}",
                entry.dialect.as_str(),
                entry.model,
                if entry.base_url.is_empty() {
                    "(agent default)"
                } else {
                    &entry.base_url
                },
                if entry.background_model.is_empty() {
                    "(agent default)"
                } else {
                    &entry.background_model
                },
            );
        }
    }
}

async fn print_listing(socket: &Target, kind: ListingKind) -> anyhow::Result<()> {
    let mut client = Client::open(socket).await?;
    client
        .request(ClientMsg::Subscribe { scope: Scope::All })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let snapshot = loop {
        match client.next_msg().await {
            Some(ServerMsg::Snapshot(s)) => break s,
            Some(_) => continue,
            None => anyhow::bail!("connection closed before snapshot"),
        }
    };
    print_snapshot(&snapshot, kind);
    Ok(())
}

fn print_snapshot(snap: &Snapshot, kind: ListingKind) {
    match kind {
        ListingKind::Buckets => {
            println!("{:<6} {:<20}", "ID", "NAME");
            for b in &snap.buckets {
                println!("{:<6} {:<20}", b.id, b.name);
            }
        }
        ListingKind::Projects => {
            println!("{:<6} {:<8} {:<20} PATH", "ID", "BUCKET", "NAME");
            for p in &snap.projects {
                println!("{:<6} {:<8} {:<20} {}", p.id, p.bucket_id, p.name, p.path);
            }
        }
        ListingKind::Forwards => {
            println!(
                "{:<6} {:<8} {:<6} {:<20} {:<32} {:<8} LABEL",
                "ID", "SESSION", "PORT", "SLUG", "URL", "TARGET"
            );
            for f in &snap.forwards {
                let target = match f.target_reachable {
                    Some(true) => "ok",
                    Some(false) => "refused",
                    None => "-",
                };
                let url = if f.url.is_empty() { "(no url)" } else { &f.url };
                let slug = if f.slug.is_empty() { "-" } else { &f.slug };
                println!(
                    "{:<6} {:<8} {:<6} {:<20} {:<32} {:<8} {}",
                    f.id, f.session_id, f.worker_port, slug, url, target, f.label
                );
            }
        }
        ListingKind::Everything { all } => {
            for b in &snap.buckets {
                println!("bucket {} {}", b.id, b.name);
                for p in snap.projects.iter().filter(|p| p.bucket_id == b.id) {
                    println!("  project {} {} ({})", p.id, p.name, p.path);
                    for s in listed_sessions(&snap.sessions, p.id, all) {
                        let code = s
                            .exit_code
                            .map(|c| format!(" exit={c}"))
                            .unwrap_or_default();
                        println!(
                            "    session {} [{}]{} {} — {}",
                            s.id,
                            s.state.as_str(),
                            code,
                            s.agent.as_str(),
                            session_name(s)
                        );
                        let chips = snap
                            .contexts
                            .iter()
                            .find(|c| c.session_id == s.id)
                            .map(|c| {
                                c.glance
                                    .iter()
                                    .map(|f| f.value.clone())
                                    .collect::<Vec<_>>()
                                    .join("  ")
                            })
                            .unwrap_or_default();
                        if !chips.is_empty() {
                            println!("      {chips}");
                        }
                    }
                }
            }
            if snap.buckets.is_empty() {
                println!("no buckets yet, create one with: pm bucket add <name>");
            }
        }
    }
}

/// Reports the newest published build and, unless only checking,
/// replaces this binary with it. A daemon already running keeps serving
/// the build it started with until it is restarted.
/// Where a setting's value comes from. A value passed positionally is
/// visible in the process argument vector and in shell history, so
/// credentials should arrive by stdin or from a file instead.
#[derive(Debug, PartialEq, Eq)]
enum ValueSource {
    Literal(String),
    Stdin,
    File(PathBuf),
}

fn select_value_source(
    value: Option<String>,
    stdin: bool,
    file: Option<PathBuf>,
) -> anyhow::Result<ValueSource> {
    let sources = u8::from(value.is_some()) + u8::from(stdin) + u8::from(file.is_some());
    if sources == 0 {
        anyhow::bail!("no value: pass one positionally, or use --stdin or --file");
    }
    if sources > 1 {
        anyhow::bail!("pass the value exactly one way: positionally, --stdin, or --file");
    }
    Ok(match (value, stdin, file) {
        (Some(value), _, _) => ValueSource::Literal(value),
        (_, true, _) => ValueSource::Stdin,
        (_, _, Some(path)) => ValueSource::File(path),
        _ => unreachable!("one source is set"),
    })
}

/// A file or heredoc almost always ends in a newline that is not part of
/// the credential, and a PEM key is rejected with one appended.
fn trim_setting_value(raw: &str) -> String {
    raw.trim_end_matches(['\n', '\r']).to_string()
}

fn resolve_setting_value(
    value: Option<String>,
    stdin: bool,
    file: Option<PathBuf>,
) -> anyhow::Result<String> {
    let raw = match select_value_source(value, stdin, file)? {
        ValueSource::Literal(value) => value,
        ValueSource::Stdin => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        ValueSource::File(path) => std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?,
    };
    Ok(trim_setting_value(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape Claude Code sends when a turn ends with a
    /// backgrounded subagent still running, captured from a live
    /// session rather than written from the schema.
    #[test]
    fn a_turn_that_ends_with_a_background_task_still_counts_as_work() {
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "stop_hook_active": false,
            "last_assistant_message": "Launched. The subagent is running in the background",
            "background_tasks": [{
                "id": "a258a9a0b9fce18ab",
                "type": "subagent",
                "status": "running",
                "description": "Sleep 45 then reply",
                "agent_type": "general-purpose"
            }]
        });
        assert!(has_background_work(&payload));
    }

    /// The same hook once the last of it lands, which is the one that
    /// really means idle.
    #[test]
    fn an_empty_background_list_is_a_finished_turn() {
        assert!(!has_background_work(&serde_json::json!({
            "hook_event_name": "Stop",
            "background_tasks": []
        })));
    }

    /// Every other agent omits the key entirely, and must not be read as
    /// permanently busy.
    #[test]
    fn a_payload_without_the_key_is_a_finished_turn() {
        assert!(!has_background_work(&serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "abc"
        })));
        assert!(!has_background_work(&serde_json::json!({
            "background_tasks": "not-an-array"
        })));
    }

    #[test]
    fn a_positional_value_is_used_as_given() {
        let got = select_value_source(Some("loud".into()), false, None).unwrap();
        assert_eq!(got, ValueSource::Literal("loud".into()));
    }

    #[test]
    fn stdin_and_file_each_select_their_own_source() {
        assert_eq!(
            select_value_source(None, true, None).unwrap(),
            ValueSource::Stdin
        );
        assert_eq!(
            select_value_source(None, false, Some(PathBuf::from("/k.p8"))).unwrap(),
            ValueSource::File(PathBuf::from("/k.p8"))
        );
    }

    #[test]
    fn a_value_given_no_way_is_refused() {
        assert!(select_value_source(None, false, None).is_err());
    }

    #[test]
    fn a_value_given_two_ways_is_refused_rather_than_ranked() {
        // Silently preferring one source would let a stale positional
        // value win over the file the operator meant to read.
        assert!(select_value_source(Some("v".into()), true, None).is_err());
        assert!(select_value_source(Some("v".into()), false, Some(PathBuf::from("/k"))).is_err());
    }

    #[test]
    fn a_trailing_newline_is_dropped_but_inner_ones_are_kept() {
        let pem = "-----BEGIN PRIVATE KEY-----\nMIGT\n-----END PRIVATE KEY-----\n";
        assert_eq!(
            trim_setting_value(pem),
            "-----BEGIN PRIVATE KEY-----\nMIGT\n-----END PRIVATE KEY-----"
        );
        assert_eq!(trim_setting_value("v\r\n"), "v");
        assert_eq!(trim_setting_value("  spaced  "), "  spaced  ");
    }

    #[test]
    fn a_file_value_round_trips_without_its_trailing_newline() {
        let dir = std::env::temp_dir().join(format!("pm-229-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("AuthKey.p8");
        std::fs::write(&path, "secret-key-material\n").unwrap();
        let got = resolve_setting_value(None, false, Some(path.clone())).unwrap();
        assert_eq!(got, "secret-key-material");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_names_the_path_it_could_not_read() {
        let err = resolve_setting_value(None, false, Some(PathBuf::from("/nope/AuthKey.p8")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("/nope/AuthKey.p8"), "unhelpful error: {err}");
    }

    #[test]
    fn every_secret_setting_is_one_the_cli_will_not_echo() {
        // The Set handler decides whether to print the value by asking
        // this list, so a new secret must be added here to stay unprinted.
        for key in pm_daemon::push::SECRET_SETTINGS {
            assert!(
                key.starts_with("push."),
                "unexpected secret setting {key}, check the echo guard still covers it"
            );
        }
    }
    use clap::CommandFactory;

    #[test]
    fn a_legacy_public_host_becomes_a_public_url_on_the_web_ui_port() {
        assert_eq!(
            migrate_public_host(
                None,
                Some("pm.example".into()),
                Some("0.0.0.0:7676".parse().unwrap())
            ),
            Some("http://pm.example:7676".into())
        );
    }

    #[test]
    fn a_legacy_public_host_without_http_carries_no_port() {
        assert_eq!(
            migrate_public_host(None, Some("pm.example".into()), None),
            Some("http://pm.example".into())
        );
    }

    #[test]
    fn public_url_wins_when_both_flags_are_passed() {
        assert_eq!(
            migrate_public_host(
                Some("https://pm.example".into()),
                Some("other.example".into()),
                Some("0.0.0.0:7676".parse().unwrap())
            ),
            Some("https://pm.example".into())
        );
    }

    #[test]
    fn no_public_flags_leave_the_daemon_without_one() {
        assert_eq!(
            migrate_public_host(None, None, Some("0.0.0.0:7676".parse().unwrap())),
            None
        );
    }

    #[test]
    fn project_edit_arguments_parse() {
        let cli = Cli::try_parse_from([
            "pm",
            "project",
            "edit",
            "7",
            "--path",
            "/tmp/api-v2",
            "--worker",
            "3",
            "--permission",
            "auto",
        ])
        .unwrap();
        let Command::Project {
            command:
                ProjectCmd::Edit {
                    id,
                    path,
                    worker,
                    clear_worker,
                    permission,
                },
        } = cli.command
        else {
            panic!("expected project edit command");
        };
        assert_eq!(id, 7);
        assert_eq!(path, Some(PathBuf::from("/tmp/api-v2")));
        assert_eq!(worker, Some(3));
        assert!(!clear_worker);
        assert_eq!(permission.as_deref(), Some("auto"));
    }

    #[test]
    fn item_add_status_help_uses_canonical_guidance() {
        let command = Cli::command();
        let status = command
            .find_subcommand("items")
            .unwrap()
            .find_subcommand("add")
            .unwrap()
            .get_arguments()
            .find(|arg| arg.get_id() == "status")
            .unwrap();
        assert_eq!(
            status.get_long_help().unwrap().to_string(),
            ITEM_STATUS_CREATE_GUIDANCE
        );
    }

    fn session_start_context(forward_inventory: &str) -> String {
        session_start_context_for(forward_inventory, false)
    }

    fn session_start_context_for(forward_inventory: &str, claude: bool) -> String {
        let json: serde_json::Value =
            serde_json::from_str(&session_start_additional_context(forward_inventory, claude))
                .unwrap();
        assert_eq!(json["hookSpecificOutput"]["hookEventName"], "SessionStart");
        json["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn session_start_hook_emits_reporting_brief_as_additional_context() {
        let ctx = session_start_context("");
        assert!(ctx.contains("report"));
        assert!(ctx.contains("flag_blocked"));
        assert_eq!(ctx, pm_adapters::REPORTING_BRIEF);
    }

    #[test]
    fn claude_session_start_hook_adds_the_harness_brief_under_the_contract() {
        let ctx = session_start_context_for("", true);
        assert_eq!(ctx, pm_adapters::with_claude_harness_brief(""));
        assert!(ctx.starts_with(pm_adapters::REPORTING_BRIEF), "{ctx}");
        assert!(ctx.ends_with(pm_adapters::CLAUDE_HARNESS_BRIEF), "{ctx}");
        assert!(!session_start_context("").contains(pm_adapters::CLAUDE_HARNESS_BRIEF));
    }

    #[test]
    fn claude_session_start_hook_keeps_the_forward_inventory_last() {
        let inventory = pm_adapters::forward_inventory(&[pm_protocol::domain::SessionForward {
            id: 1,
            session_id: 7,
            worker_port: 8080,
            listener_port: 41000,
            slug: "docs-preview".into(),
            label: String::new(),
            scheme: "http".into(),
            created_at_unix_ms: 0,
            url: "https://docs-preview.example/".into(),
            target_reachable: Some(true),
            source_path: String::new(),
        }]);
        let ctx = session_start_context_for(&inventory, true);
        let harness = ctx.find(pm_adapters::CLAUDE_HARNESS_BRIEF).unwrap();
        let forwards = ctx.find(&inventory).unwrap();
        assert!(harness < forwards, "{ctx}");
        assert!(ctx.ends_with(&inventory), "{ctx}");
    }

    #[test]
    fn session_start_hook_appends_the_forward_inventory_under_the_brief() {
        let inventory = pm_adapters::forward_inventory(&[pm_protocol::domain::SessionForward {
            id: 1,
            session_id: 7,
            worker_port: 8080,
            listener_port: 41000,
            slug: "docs-preview".into(),
            label: String::new(),
            scheme: "http".into(),
            created_at_unix_ms: 0,
            url: "https://docs-preview.example/".into(),
            target_reachable: Some(false),
            source_path: String::new(),
        }]);
        let ctx = session_start_context(&inventory);
        assert!(ctx.starts_with(pm_adapters::REPORTING_BRIEF), "{ctx}");
        assert!(ctx.contains("docs-preview, local port 8080"), "{ctx}");
        assert!(ctx.ends_with(&inventory), "{ctx}");
    }

    #[test]
    fn stop_hook_blocks_when_a_turn_ends_with_a_nudge() {
        use pm_protocol::domain::HookKind;
        let out = stop_hook_block(HookKind::TurnEnded, false, b"please report").unwrap();
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["decision"], "block");
        assert_eq!(json["reason"], "please report");
    }

    #[test]
    fn stop_hook_does_not_block_in_a_forced_continuation() {
        use pm_protocol::domain::HookKind;
        assert!(stop_hook_block(HookKind::TurnEnded, true, b"please report").is_none());
    }

    #[test]
    fn stop_hook_does_not_block_without_a_nudge() {
        use pm_protocol::domain::HookKind;
        assert!(stop_hook_block(HookKind::TurnEnded, false, b"").is_none());
    }

    /// Claude, Codex and Gemini name the conversation id and transcript
    /// in snake_case; Antigravity names the same two fields in
    /// camelCase, and one receiver serves both.
    #[test]
    fn hook_payload_identity_reads_either_schema() {
        let claude = serde_json::json!({
            "session_id": "claude-session",
            "transcript_path": "/tmp/claude.jsonl",
        });
        assert_eq!(
            payload_field(&claude, HOOK_SESSION_ID_KEYS),
            "claude-session"
        );
        assert_eq!(
            payload_field(&claude, HOOK_TRANSCRIPT_PATH_KEYS),
            "/tmp/claude.jsonl"
        );
        assert!(!is_antigravity_payload(&claude));

        let antigravity = serde_json::json!({
            "conversationId": "ec33ebf9-0cba-4100",
            "transcriptPath": "/tmp/agy.jsonl",
            "terminationReason": "model_stop",
        });
        assert_eq!(
            payload_field(&antigravity, HOOK_SESSION_ID_KEYS),
            "ec33ebf9-0cba-4100"
        );
        assert_eq!(
            payload_field(&antigravity, HOOK_TRANSCRIPT_PATH_KEYS),
            "/tmp/agy.jsonl"
        );
        assert!(is_antigravity_payload(&antigravity));

        assert_eq!(
            payload_field(&serde_json::json!({}), HOOK_SESSION_ID_KEYS),
            ""
        );
        assert!(!is_antigravity_payload(&serde_json::json!({})));
    }

    /// Antigravity fires PreInvocation before every model call, so only
    /// the first of a turn is a prompt being submitted.
    #[test]
    fn only_the_first_invocation_of_a_turn_submits_a_prompt() {
        assert!(!is_hook_continuation(
            &serde_json::json!({ "invocationNum": 0 })
        ));
        assert!(is_hook_continuation(
            &serde_json::json!({ "invocationNum": 1 })
        ));
        assert!(is_hook_continuation(
            &serde_json::json!({ "invocationNum": 9 })
        ));
        // Every other agent's submit hook fires once and carries no
        // counter, so it is never read as a continuation.
        assert!(!is_hook_continuation(
            &serde_json::json!({ "session_id": "s" })
        ));
    }

    #[test]
    fn stop_hook_only_blocks_on_turn_ended() {
        use pm_protocol::domain::HookKind;
        assert!(stop_hook_block(HookKind::Started, false, b"please report").is_none());
        assert!(stop_hook_block(HookKind::PromptSubmitted, false, b"please report").is_none());
    }

    fn session(
        id: u64,
        state: pm_protocol::domain::SessionState,
        last_activity: i64,
    ) -> pm_protocol::domain::Session {
        pm_protocol::domain::Session {
            git: None,
            id,
            project_id: 1,
            agent: AgentKind::ClaudeCode,
            agent_source: pm_protocol::domain::AgentSelectionSource::Explicit,
            state,
            task_title: String::new(),
            task_prompt: String::new(),
            agent_session_id: None,
            created_at_unix_ms: 0,
            ended_at_unix_ms: None,
            exit_code: None,
            state_detail: String::new(),
            activity: String::new(),
            progress_percent: None,
            resumable: false,
            permission_mode: pm_protocol::domain::PermissionMode::Default,
            worker_id: 0,
            cwd: String::new(),
            goal: String::new(),
            headline: String::new(),
            summary: String::new(),
            items_api: true,
            supervisor_api: false,
            role: pm_protocol::domain::SessionRole::Worker,
            spawned_by_session_id: None,
            last_activity_at_unix_ms: last_activity,
            last_agent_activity_at_unix_ms: last_activity,
            last_user_interaction_at_unix_ms: 0,
            needs_input_unseen: state == pm_protocol::domain::SessionState::NeedsInput,
            idle_unseen: false,
            model_profile_id: None,
            model_profile_source: None,
        }
    }

    #[test]
    fn listing_hides_ended_sessions_and_sorts_by_recency() {
        use pm_protocol::domain::SessionState::*;
        let sessions = vec![
            session(1, Exited, 500),
            session(2, Working, 100),
            session(3, Idle, 300),
        ];
        let listed = listed_sessions(&sessions, 1, false);
        assert_eq!(listed.iter().map(|s| s.id).collect::<Vec<_>>(), [3, 2]);
        let all = listed_sessions(&sessions, 1, true);
        assert_eq!(all.iter().map(|s| s.id).collect::<Vec<_>>(), [1, 3, 2]);
    }

    #[test]
    fn suggestions_select_five_most_recent_sessions_across_states() {
        use pm_protocol::domain::SessionState::*;
        let sessions = vec![
            session(1, Exited, 100),
            session(2, Idle, 200),
            session(3, Working, 300),
            session(4, Failed, 400),
            session(5, NeedsInput, 500),
            session(6, Starting, 600),
        ];

        assert_eq!(
            suggested_sessions(&sessions)
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            [6, 5, 4, 3, 2]
        );
    }

    #[test]
    fn suggestions_format_state_and_preferred_session_name() {
        use pm_protocol::domain::SessionState::*;
        let mut sessions = vec![
            session(1, Working, 400),
            session(2, Idle, 300),
            session(3, Idle, 200),
            session(4, Exited, 100),
        ];
        sessions[0].goal = "Agent goal".into();
        sessions[0].task_title = "Original task".into();
        sessions[0].headline = "Current step".into();
        sessions[1].task_title = "Fallback task".into();
        sessions[1].headline = "Current step".into();
        sessions[2].headline = "Only a headline".into();

        assert_eq!(
            format_session_suggestions(&sessions),
            "Did you mean:\n  1 [working] Agent goal\n  2 [idle] Fallback task\n  3 [idle] Only a headline\n  4 [exited] (unnamed)"
        );
        assert!(format_session_suggestions(&[]).is_empty());
    }

    #[test]
    fn daemon_http_tls_needs_both_the_certificate_and_the_key() {
        assert!(Cli::try_parse_from(["pm", "daemon", "--http-tls-cert", "c.pem"]).is_err());
        assert!(Cli::try_parse_from(["pm", "daemon", "--http-tls-key", "k.pem"]).is_err());
        let cli = Cli::try_parse_from([
            "pm",
            "daemon",
            "--http-tls-cert",
            "c.pem",
            "--http-tls-key",
            "k.pem",
        ])
        .unwrap();
        match cli.command {
            Command::Daemon {
                http_tls_cert,
                http_tls_key,
                ..
            } => {
                assert_eq!(http_tls_cert, Some(PathBuf::from("c.pem")));
                assert_eq!(http_tls_key, Some(PathBuf::from("k.pem")));
            }
            _ => panic!("expected daemon command"),
        }
    }

    #[test]
    fn daemon_accepts_no_local_worker_flag() {
        let cli = Cli::try_parse_from(["pm", "daemon", "--no-local-worker"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Daemon {
                no_local_worker: true,
                ..
            }
        ));
    }

    #[test]
    fn pushgw_takes_the_apns_flags_directly() {
        let cli = Cli::try_parse_from([
            "pm",
            "pushgw",
            "--apns-key-p8",
            "/keys/AuthKey.p8",
            "--apns-key-id",
            "ABCDE12345",
            "--apns-team-id",
            "TEAM123456",
            "--apns-topic",
            "com.example.app",
        ])
        .unwrap();
        match cli.command {
            Command::Pushgw(args) => {
                assert_eq!(args.apns_topic, "com.example.app");
                assert_eq!(args.apns_key_p8, PathBuf::from("/keys/AuthKey.p8"));
                assert_eq!(args.listen.to_string(), "127.0.0.1:8400");
                assert_eq!(args.apns_sandbox_key_p8, None);
                assert_eq!(args.apns_sandbox_key_id, None);
                assert!(args.trusted_proxies.is_empty());
                assert_eq!(args.limits(), pm_pushgw::limits::LimitPolicy::default());
            }
            _ => panic!("expected pushgw command"),
        }
    }

    const PUSHGW_REQUIRED_ARGS: [&str; 10] = [
        "pm",
        "pushgw",
        "--apns-key-p8",
        "/keys/AuthKey.p8",
        "--apns-key-id",
        "ABCDE12345",
        "--apns-team-id",
        "TEAM123456",
        "--apns-topic",
        "com.example.app",
    ];

    #[test]
    fn pushgw_takes_every_limit_as_a_flag() {
        let limit_args = [
            "--rate-per-source",
            "120",
            "--rate-per-token",
            "10",
            "--rate-window-secs",
            "30",
            "--bad-token-limit",
            "3",
            "--bad-token-window-secs",
            "120",
            "--bad-token-block-secs",
            "300",
        ];
        let cli = Cli::try_parse_from(PUSHGW_REQUIRED_ARGS.into_iter().chain(limit_args)).unwrap();
        match cli.command {
            Command::Pushgw(args) => {
                use std::time::Duration;
                assert_eq!(
                    args.limits(),
                    pm_pushgw::limits::LimitPolicy {
                        pushes_per_source: 120,
                        pushes_per_token: 10,
                        window: Duration::from_secs(30),
                        bad_tokens_per_source: 3,
                        bad_token_window: Duration::from_secs(120),
                        block_duration: Duration::from_secs(300),
                    }
                );
            }
            _ => panic!("expected pushgw command"),
        }
    }

    /// A zero limit or window would refuse every push or count nothing,
    /// so only the invalid-token limit takes zero, where it means off.
    #[test]
    fn pushgw_rejects_zero_for_everything_but_the_bad_token_limit() {
        for flag in [
            "--rate-per-source",
            "--rate-per-token",
            "--rate-window-secs",
            "--bad-token-window-secs",
            "--bad-token-block-secs",
        ] {
            let args = PUSHGW_REQUIRED_ARGS.into_iter().chain([flag, "0"]);
            assert!(Cli::try_parse_from(args).is_err(), "{flag} accepted 0");
        }
        let args = PUSHGW_REQUIRED_ARGS
            .into_iter()
            .chain(["--bad-token-limit", "0"]);
        assert!(Cli::try_parse_from(args).is_ok());
    }

    #[test]
    fn pushgw_takes_trusted_proxies_repeated_or_comma_separated() {
        let cli = Cli::try_parse_from([
            "pm",
            "pushgw",
            "--apns-key-p8",
            "/keys/AuthKey.p8",
            "--apns-key-id",
            "ABCDE12345",
            "--apns-team-id",
            "TEAM123456",
            "--apns-topic",
            "com.example.app",
            "--trusted-proxy",
            "127.0.0.1,::1",
            "--trusted-proxy",
            "10.0.0.2",
        ])
        .unwrap();
        match cli.command {
            Command::Pushgw(args) => {
                let proxies: Vec<String> =
                    args.trusted_proxies.iter().map(|p| p.to_string()).collect();
                assert_eq!(proxies, ["127.0.0.1", "::1", "10.0.0.2"]);
            }
            _ => panic!("expected pushgw command"),
        }
    }

    #[test]
    fn pushgw_takes_an_optional_sandbox_signing_key() {
        let cli = Cli::try_parse_from([
            "pm",
            "pushgw",
            "--apns-key-p8",
            "/keys/AuthKey.p8",
            "--apns-key-id",
            "ABCDE12345",
            "--apns-sandbox-key-p8",
            "/keys/AuthKey_Sandbox.p8",
            "--apns-sandbox-key-id",
            "SANDB67890",
            "--apns-team-id",
            "TEAM123456",
            "--apns-topic",
            "com.example.app",
        ])
        .unwrap();
        match cli.command {
            Command::Pushgw(args) => {
                assert_eq!(
                    args.apns_sandbox_key_p8,
                    Some(PathBuf::from("/keys/AuthKey_Sandbox.p8"))
                );
                assert_eq!(args.apns_sandbox_key_id.as_deref(), Some("SANDB67890"));
            }
            _ => panic!("expected pushgw command"),
        }
    }

    /// A sandbox key without its id would silently sign sandbox sends
    /// with the primary key, which is the failure the flag exists to
    /// fix, so the pair is required together.
    #[test]
    fn pushgw_rejects_half_a_sandbox_key_pair() {
        let base = [
            "pm",
            "pushgw",
            "--apns-key-p8",
            "/keys/AuthKey.p8",
            "--apns-key-id",
            "ABCDE12345",
            "--apns-team-id",
            "TEAM123456",
            "--apns-topic",
            "com.example.app",
        ];
        for (flag, value) in [
            ("--apns-sandbox-key-p8", "/keys/AuthKey_Sandbox.p8"),
            ("--apns-sandbox-key-id", "SANDB67890"),
        ] {
            let mut argv = base.to_vec();
            argv.push(flag);
            argv.push(value);
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "{flag} alone should not parse"
            );
        }
    }

    #[test]
    fn pushgw_requires_apns_credentials() {
        assert!(Cli::try_parse_from(["pm", "pushgw"]).is_err());
    }

    #[test]
    fn pushgw_rejects_the_removed_signing_flags() {
        let base = [
            "pm",
            "pushgw",
            "--apns-key-p8",
            "k",
            "--apns-key-id",
            "ABCDE12345",
            "--apns-team-id",
            "TEAM123456",
            "--apns-topic",
            "com.example.app",
        ];
        for flag in ["--allowed-signing-key", "--signing-keys-file"] {
            let mut argv = base.to_vec();
            argv.push(flag);
            argv.push("x");
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "{flag} should no longer parse"
            );
        }
    }

    #[test]
    fn worker_delete_defaults_to_cleanup_and_accepts_headless_preservation() {
        let cli = Cli::try_parse_from(["pm", "worker", "delete", "--name", "build"]).unwrap();
        let Command::Worker {
            action:
                Some(WorkerCmd::Delete {
                    no_delete_resources,
                    accept_delete,
                    ..
                }),
            ..
        } = cli.command
        else {
            panic!("expected delete");
        };
        assert!(!no_delete_resources && !accept_delete);
        let cli = Cli::try_parse_from([
            "pm",
            "worker",
            "delete",
            "--no-delete-resources",
            "--accept-delete",
        ])
        .unwrap();
        let Command::Worker {
            action:
                Some(WorkerCmd::Delete {
                    no_delete_resources,
                    accept_delete,
                    ..
                }),
            ..
        } = cli.command
        else {
            panic!("expected delete");
        };
        assert!(no_delete_resources && accept_delete);
        assert!(Cli::try_parse_from(["pm", "worker", "forget"]).is_err());
        assert!(Cli::try_parse_from(["pm", "worker", "delete", "--purge"]).is_err());
    }

    #[test]
    fn worker_parses_shifted_home_opt_in_and_modify_disable() {
        for (flag, expected) in [
            ("--incus-shifted-home", true),
            ("--incus-shifted-home=false", false),
        ] {
            let cli =
                Cli::try_parse_from(["pm", "worker", "--sandbox", "--runtime", "incus", flag])
                    .unwrap();
            let Command::Worker {
                incus_shifted_home, ..
            } = cli.command
            else {
                panic!("expected worker");
            };
            assert_eq!(incus_shifted_home, Some(expected));
            let cli =
                Cli::try_parse_from(["pm", "worker", "modify", "--name", "repos", flag]).unwrap();
            let Command::Worker {
                action:
                    Some(WorkerCmd::Modify {
                        incus_shifted_home, ..
                    }),
                ..
            } = cli.command
            else {
                panic!("expected modify");
            };
            assert_eq!(incus_shifted_home, Some(expected));
        }
        assert!(Cli::try_parse_from(["pm", "worker", "--incus-shifted-home=invalid"]).is_err());
    }

    #[test]
    fn worker_accepts_sandbox_flags() {
        let cli = Cli::try_parse_from([
            "pm",
            "worker",
            "--sandbox",
            "--listen",
            "10.30.2.35:7677",
            "--allow-from",
            "10.30.4.0/24",
            "--dir",
            "/srv/repos",
            "--dir",
            "/data:/mnt/data:ro",
            "--restart",
            "unless-stopped",
            "--foreground",
            "--name",
            "bench",
            "--image",
            "custom:tag",
            "--env",
            "A=1",
            "--network",
            "bridge",
            "--runtime",
            "podman",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Command::Worker {
                sandbox,
                listen,
                allow_from,
                dir,
                restart,
                foreground,
                name,
                image,
                env,
                network,
                runtime,
                force,
                ..
            } => {
                assert!(sandbox && foreground && force);
                assert_eq!(listen, Some("10.30.2.35:7677".parse().unwrap()));
                assert_eq!(allow_from, vec!["10.30.4.0/24"]);
                assert_eq!(dir, vec!["/srv/repos", "/data:/mnt/data:ro"]);
                assert_eq!(restart, Some(sandbox::RestartPolicy::UnlessStopped));
                assert_eq!(name.as_deref(), Some("bench"));
                assert_eq!(image.as_deref(), Some("custom:tag"));
                assert_eq!(env, vec!["A=1"]);
                assert_eq!(network.as_deref(), Some("bridge"));
                assert_eq!(runtime, Some(sandbox::RuntimeKind::Podman));
            }
            _ => panic!("expected worker command"),
        }
    }

    /// Whether a container setting applies is the worker's profile's
    /// answer, and clap cannot see a profile, so these parse and the run
    /// refuses them. `requires = "sandbox"` used to do it here and would
    /// now reject a worker whose profile records a container.
    #[test]
    fn container_settings_parse_and_are_refused_by_the_run() {
        for flag in [
            vec!["--dir", "/srv"],
            vec!["--restart", "always"],
            vec!["--image", "img:1"],
            vec!["--env", "A=1"],
            vec!["--network", "bridge"],
            vec!["--runtime", "docker"],
            vec!["--cpu", "2"],
            vec!["--memory", "4GiB"],
            vec!["--memory-swap", "false"],
            vec!["--cpu-allowance", "50%"],
            vec!["--memory-enforce", "hard"],
            vec!["--disk", "40GiB"],
            vec!["--foreground"],
        ] {
            let mut argv = vec!["pm", "worker"];
            argv.extend(flag.iter().copied());
            Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{flag:?} should parse: {e}"));
        }
        assert!(Cli::try_parse_from(["pm", "worker"]).is_ok());
    }

    /// The caps reach the run as caps, which is what the refusal there
    /// keys on.
    #[test]
    fn a_resource_cap_reaches_the_run_as_a_cap() {
        for flag in [
            ["--cpu", "2"],
            ["--memory", "4GiB"],
            ["--memory-swap", "false"],
            ["--cpu-allowance", "50%"],
            ["--memory-enforce", "hard"],
            ["--disk", "40GiB"],
        ] {
            let cli = Cli::try_parse_from(["pm", "worker", flag[0], flag[1]])
                .unwrap_or_else(|e| panic!("{flag:?} should parse: {e}"));
            let Command::Worker { limits, .. } = cli.command else {
                panic!("expected worker");
            };
            assert!(
                !sandbox::SandboxLimits::from(limits).is_unset(),
                "{flag:?} reached the run as a cap"
            );
        }
    }

    #[test]
    fn worker_accepts_every_sandbox_limit() {
        let cli = Cli::try_parse_from([
            "pm",
            "worker",
            "--sandbox",
            "--controller",
            "wss://h:7676",
            "--cpu",
            "2",
            "--memory",
            "4GiB",
            "--memory-swap",
            "false",
            "--cpu-allowance",
            "50%",
            "--memory-enforce",
            "hard",
            "--disk",
            "40GiB",
        ])
        .unwrap();
        let Command::Worker { limits, .. } = cli.command else {
            panic!("expected worker");
        };
        let limits = sandbox::SandboxLimits::from(limits);
        assert_eq!(limits.cpu.as_deref(), Some("2"));
        assert_eq!(limits.memory.as_deref(), Some("4GiB"));
        assert_eq!(limits.memory_swap, Some(false));
        assert_eq!(limits.cpu_allowance.as_deref(), Some("50%"));
        assert_eq!(limits.memory_enforce, Some(sandbox::MemoryEnforce::Hard));
        assert_eq!(limits.disk.as_deref(), Some("40GiB"));
    }

    /// The generator is meant to be reachable by appending a flag to the
    /// enrolling command, token and all.
    #[test]
    fn the_generator_accepts_the_enrolling_command_unchanged() {
        let cli = Cli::try_parse_from([
            "pm",
            "worker",
            "--controller",
            "wss://host:7677",
            "--token",
            "enrollment",
            "--name",
            "build",
            "--systemd",
            "--system",
            "--install",
        ])
        .unwrap();
        let Command::Worker {
            systemd,
            install,
            system_scope,
            token,
            name,
            ..
        } = cli.command
        else {
            panic!("expected worker");
        };
        assert!(systemd && install && system_scope);
        assert_eq!(token.as_deref(), Some("enrollment"));
        assert_eq!(name.as_deref(), Some("build"));
    }

    #[test]
    fn generator_only_flags_require_systemd() {
        assert!(Cli::try_parse_from(["pm", "worker", "--install"]).is_err());
        assert!(Cli::try_parse_from(["pm", "worker", "--system"]).is_err());
        assert!(Cli::try_parse_from(["pm", "worker", "--systemd"]).is_ok());
    }

    /// `--sandbox --systemd` has to reach the generator so it can explain
    /// why the container runtime already covers this, rather than dying
    /// on an argument conflict.
    #[test]
    fn the_generator_parses_alongside_sandbox() {
        assert!(Cli::try_parse_from(["pm", "worker", "--sandbox", "--systemd"]).is_ok());
    }

    #[test]
    fn tmux_workspace_accepts_an_optional_handle() {
        let listing = Cli::try_parse_from(["pm", "tmux", "workspace"]).unwrap();
        assert!(matches!(
            listing.command,
            Command::Tmux {
                command: TmuxCmd::Workspace { workspace: None }
            }
        ));

        let materialize = Cli::try_parse_from(["pm", "tmux", "workspace", "42"]).unwrap();
        assert!(matches!(
            materialize.command,
            Command::Tmux {
                command: TmuxCmd::Workspace {
                    workspace: Some(ref handle)
                }
            } if handle == "42"
        ));
    }

    fn invocation(argv: &[&str]) -> Invocation {
        let matches = remotes::with_name_flag(Cli::command())
            .try_get_matches_from(argv)
            .unwrap_or_else(|e| panic!("{argv:?} should parse: {e}"));
        Invocation::from_matches(&matches).unwrap()
    }

    #[test]
    fn the_name_flag_collides_with_no_argument_of_any_subcommand() {
        remotes::with_name_flag(Cli::command()).debug_assert();
    }

    #[test]
    fn a_controller_name_is_accepted_wherever_it_is_written() {
        for argv in [
            &["pm", "--name", "work", "ls"][..],
            &["pm", "ls", "--name", "work"],
            &["pm", "items", "--name", "work", "ls"],
            &["pm", "items", "ls", "--name", "work"],
            &["pm", "--name", "home", "items", "ls", "--name", "work"],
        ] {
            assert_eq!(invocation(argv).name.as_deref(), Some("work"), "{argv:?}");
        }
        assert_eq!(invocation(&["pm", "ls"]).name, None);
    }

    #[test]
    fn a_worker_name_is_not_read_as_a_controller_name() {
        let parsed = invocation(&["pm", "worker", "--name", "builder"]);
        assert_eq!(parsed.name, None);
        let Command::Worker { name, .. } = parsed.cli.command else {
            panic!("expected worker command");
        };
        assert_eq!(name.as_deref(), Some("builder"));
    }

    #[test]
    fn a_passed_socket_is_told_apart_from_an_inherited_one() {
        for argv in [
            &["pm", "--socket", "/tmp/x.sock", "ls"][..],
            &["pm", "ls", "--socket", "/tmp/x.sock"],
        ] {
            assert_eq!(
                invocation(argv).socket,
                remotes::SocketChoice::Flag,
                "{argv:?}"
            );
        }
        let inherited = if std::env::var_os("PM_SOCKET").is_some() {
            remotes::SocketChoice::Environment
        } else {
            remotes::SocketChoice::Default
        };
        assert_eq!(invocation(&["pm", "ls"]).socket, inherited);
    }

    #[test]
    fn login_takes_its_name_from_the_controller_name_flag() {
        let parsed = invocation(&[
            "pm",
            "login",
            "https://pm.example",
            "--name",
            "work",
            "--password-stdin",
        ]);
        assert_eq!(parsed.name.as_deref(), Some("work"));
        assert!(parsed.cli.command.chooses_its_own_daemon());
        let Command::Login {
            url,
            password_stdin,
            ..
        } = parsed.cli.command
        else {
            panic!("expected login command");
        };
        assert_eq!(url, "https://pm.example");
        assert!(password_stdin);
    }

    #[test]
    fn commands_that_run_or_manage_daemons_do_not_resolve_one() {
        for argv in [
            &["pm", "daemon"][..],
            &["pm", "worker"],
            &["pm", "update", "--check"],
            &["pm", "logout"],
            &["pm", "remotes"],
            &["pm", "_hook", "started"],
        ] {
            assert!(
                invocation(argv).cli.command.chooses_its_own_daemon(),
                "{argv:?}"
            );
        }
        for argv in [&["pm", "ls"][..], &["pm", "tui"], &["pm", "attach", "3"]] {
            assert!(
                !invocation(argv).cli.command.chooses_its_own_daemon(),
                "{argv:?}"
            );
        }
    }
}
