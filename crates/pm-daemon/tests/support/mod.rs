// Shared by several test binaries; each uses only a subset, which
// would otherwise trip dead-code warnings per binary.
#![allow(dead_code)]

pub mod connections;

/// The Host a router test addresses and the origin a browser reaching it
/// states. The daemon refuses a state-changing request that carries the
/// session cookie and states no origin, so a test standing in for the
/// signed-in dashboard sends both, the way a browser does.
pub const TEST_HOST: &str = "dashboard.test";
pub const TEST_ORIGIN: &str = "http://dashboard.test";

/// The `Authorization` value a signed-in dashboard sends.
///
/// The session cookie mints this and authenticates nothing else, so a test
/// standing in for the dashboard holds what the dashboard holds. `session_value`
/// is what a login returned.
pub fn dashboard_bearer(daemon: &Daemon, session_value: &str) -> String {
    let (user_id, _) = daemon
        .auth_verify_user(session_value)
        .expect("a signed-in session");
    let (token, _) = daemon
        .issue_web_access_token(user_id, session_value)
        .expect("a token");
    format!("Bearer {token}")
}

/// Signs a user in and returns the dashboard credential in one step, for the
/// tests whose subject is not authentication.
pub fn signed_in_bearer(daemon: &Daemon) -> String {
    let session = daemon
        .auth_setup("testuser", "longenoughpassword")
        .expect("setup");
    dashboard_bearer(daemon, &session)
}

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pm_adapters::{
    AdapterError, AdapterRegistry, AgentAdapter, CommandSpec, SpawnCtx, SpawnPlan, TestAgentAdapter,
};
use pm_daemon::{Daemon, DaemonConfig};
use pm_protocol::domain::{AgentKind, HookKind, ItemWrite, PermissionMode};
use serde_json::json;
use tokio::sync::mpsc;

pub const TEST_TIMEOUT: Duration = Duration::from_secs(10);

pub fn testagent_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pm-testagent"))
}

pub fn test_registry() -> AdapterRegistry {
    let mut r = AdapterRegistry::empty();
    r.register(Box::new(TestAgentAdapter {
        program: testagent_bin(),
    }));
    r
}

/// Stands in for a real agent CLI as one of the hook-integrated kinds,
/// so coverage that turns on which agent a session runs can pick the
/// kind without launching that agent.
struct TestHookedAdapter {
    kind: AgentKind,
}

impl AgentAdapter for TestHookedAdapter {
    fn kind(&self) -> AgentKind {
        self.kind
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    /// The scripted TUI composer reproduces a real agent TUI's paste
    /// handling, which is the whole point of driving submit tests through
    /// it.
    fn submits_after_bracketed_paste(&self) -> bool {
        true
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: testagent_bin().display().to_string(),
                args: vec!["tui-composer".into()],
                env: vec![],
                cwd: ctx.cwd.clone(),
            },
            agent_session_id: Some(format!(
                "{}-tui-{}",
                self.kind.as_str(),
                ctx.integration.session_id
            )),
            detect_osc9_needs_input: false,
        })
    }
}

/// What the daemon handed an adapter as a session's instructions, in
/// spawn order, so a test can assert on what a resumed agent is started
/// with rather than on a file only a real CLI would write.
#[derive(Clone, Default)]
pub struct RecordedInstructions(Arc<std::sync::Mutex<Vec<(u64, String)>>>);

impl RecordedInstructions {
    /// The instructions the most recent spawn of this session carried.
    pub fn latest(&self, session_id: u64) -> Option<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(id, _)| *id == session_id)
            .map(|(_, text)| text.clone())
    }
}

/// Stands in for an agent CLI that carries the compiled instructions,
/// recording what each spawn and resume was given. `session_start_context`
/// picks which side of the forward-inventory split the kind sits on.
struct TestRecordingAdapter {
    kind: AgentKind,
    session_start_context: bool,
    recorded: RecordedInstructions,
}

impl TestRecordingAdapter {
    fn plan(&self, ctx: &SpawnCtx, agent_session_id: String) -> SpawnPlan {
        self.recorded.0.lock().unwrap().push((
            ctx.integration.session_id,
            ctx.compiled_instructions.clone(),
        ));
        SpawnPlan {
            spec: CommandSpec {
                program: testagent_bin().display().to_string(),
                args: vec![ctx.task_prompt.clone()],
                env: vec![],
                cwd: ctx.cwd.clone(),
            },
            agent_session_id: Some(agent_session_id),
            detect_osc9_needs_input: false,
        }
    }
}

impl AgentAdapter for TestRecordingAdapter {
    fn kind(&self) -> AgentKind {
        self.kind
    }

    fn injects_session_start_context(&self) -> bool {
        self.session_start_context
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        Ok(self.plan(
            ctx,
            format!("{}-{}", self.kind.as_str(), ctx.integration.session_id),
        ))
    }

    fn resume_command(
        &self,
        ctx: &SpawnCtx,
        agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        Ok(self.plan(ctx, agent_session_id.to_string()))
    }
}

pub struct TestEnv {
    pub daemon: Arc<Daemon>,
    pub exit_rx: mpsc::UnboundedReceiver<pm_daemon::mux::SessionExit>,
    /// Program Status changes from local agent terminals. No server runs,
    /// so a test that needs them applied takes this and applies them.
    pub program_status_rx: Option<mpsc::UnboundedReceiver<pm_daemon::mux::ProgramStatusUpdate>>,
    pub project_id: u64,
    _tmp: tempfile::TempDir,
}

impl TestEnv {
    /// The project's directory, which is also every session's cwd.
    /// Reviews need it to make the directory a real repository.
    pub fn project_root(&self) -> PathBuf {
        self._tmp.path().to_path_buf()
    }
}

/// Daemon with in-memory storage, a test-agent registry, and one
/// bucket/project rooted in a temp dir. No socket server; transport
/// tests start their own.
pub fn daemon_env() -> TestEnv {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, channels) = Daemon::new(config).unwrap();
    let daemon = Arc::new(daemon);
    let bucket_id = daemon.create_bucket("test-bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "test-project", tmp.path().to_str().unwrap())
        .unwrap();
    TestEnv {
        daemon,
        exit_rx: channels.exit_rx,
        program_status_rx: Some(channels.program_status_rx),
        project_id,
        _tmp: tmp,
    }
}

/// [`daemon_env`] with a configured public URL, which is what forward
/// publishing needs before it will name a host.
pub fn daemon_env_with_public_url(public_url: &str) -> TestEnv {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: Some(public_url.to_string()),
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, channels) = Daemon::new(config).unwrap();
    let daemon = Arc::new(daemon);
    let bucket_id = daemon.create_bucket("test-bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "test-project", tmp.path().to_str().unwrap())
        .unwrap();
    TestEnv {
        daemon,
        exit_rx: channels.exit_rx,
        program_status_rx: Some(channels.program_status_rx),
        project_id,
        _tmp: tmp,
    }
}

/// [`daemon_env_with_public_url`] with a forward mount configured, for
/// the share-domain and per-forward-port modes.
pub fn daemon_env_with_forward_mount(
    public_url: &str,
    forward: pm_daemon::forward::ForwardConfig,
) -> TestEnv {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward,
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: Some(public_url.to_string()),
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, channels) = Daemon::new(config).unwrap();
    let daemon = Arc::new(daemon);
    let bucket_id = daemon.create_bucket("test-bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "test-project", tmp.path().to_str().unwrap())
        .unwrap();
    TestEnv {
        daemon,
        exit_rx: channels.exit_rx,
        program_status_rx: Some(channels.program_status_rx),
        project_id,
        _tmp: tmp,
    }
}

/// [`daemon_env_with_forward_mount`] over a database that already holds
/// state, which is how a test reaches a row the daemon's own API will
/// not create any more, such as a forward published before slugs were
/// required.
pub fn daemon_env_with_forward_mount_on_db(
    public_url: &str,
    forward: pm_daemon::forward::ForwardConfig,
    db_path: PathBuf,
) -> TestEnv {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward,
        db_path: Some(db_path),
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: Some(public_url.to_string()),
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, channels) = Daemon::new(config).unwrap();
    let daemon = Arc::new(daemon);
    let bucket_id = daemon.create_bucket("opened-again").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "opened-again", tmp.path().to_str().unwrap())
        .unwrap();
    TestEnv {
        daemon,
        exit_rx: channels.exit_rx,
        program_status_rx: Some(channels.program_status_rx),
        project_id,
        _tmp: tmp,
    }
}

/// A daemon whose database is on disk, so a second daemon can open the
/// same state and exercise restart behavior. Returns the database path.
pub fn file_backed_daemon_env() -> (TestEnv, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: Some(db_path.clone()),
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, channels) = Daemon::new(config).unwrap();
    let daemon = Arc::new(daemon);
    let bucket_id = daemon.create_bucket("test-bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "test-project", tmp.path().to_str().unwrap())
        .unwrap();
    (
        TestEnv {
            daemon,
            exit_rx: channels.exit_rx,
            program_status_rx: Some(channels.program_status_rx),
            project_id,
            _tmp: tmp,
        },
        db_path,
    )
}

pub fn codex_tui_daemon_env() -> TestEnv {
    hooked_agent_daemon_env(AgentKind::Codex)
}

/// A daemon whose registry carries the scripted test agent plus a
/// hook-integrated stand-in for one real agent kind.
pub fn hooked_agent_daemon_env(agent: AgentKind) -> TestEnv {
    let mut registry = AdapterRegistry::empty();
    registry.register(Box::new(TestAgentAdapter {
        program: testagent_bin(),
    }));
    registry.register(Box::new(TestHookedAdapter { kind: agent }));
    daemon_env_with_registry(registry, None)
}

/// A daemon whose agents record the instructions they are spawned with,
/// for both sides of the session-start-context split: `AgentKind::Test`
/// carries none of its own, `AgentKind::ClaudeCode` carries its own.
pub fn daemon_env_recording_instructions(public_url: &str) -> (TestEnv, RecordedInstructions) {
    let recorded = RecordedInstructions::default();
    let mut registry = AdapterRegistry::empty();
    registry.register(Box::new(TestRecordingAdapter {
        kind: AgentKind::Test,
        session_start_context: false,
        recorded: recorded.clone(),
    }));
    registry.register(Box::new(TestRecordingAdapter {
        kind: AgentKind::ClaudeCode,
        session_start_context: true,
        recorded: recorded.clone(),
    }));
    (
        daemon_env_with_registry(registry, Some(public_url)),
        recorded,
    )
}

pub fn daemon_env_with_registry(registry: AdapterRegistry, public_url: Option<&str>) -> TestEnv {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: public_url.map(str::to_string),
        scrollback_dir: tmp.path().join("scrollback"),
        registry,
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, channels) = Daemon::new(config).unwrap();
    let daemon = Arc::new(daemon);
    let bucket_id = daemon.create_bucket("test-bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "test-project", tmp.path().to_str().unwrap())
        .unwrap();
    TestEnv {
        daemon,
        exit_rx: channels.exit_rx,
        program_status_rx: Some(channels.program_status_rx),
        project_id,
        _tmp: tmp,
    }
}

/// A hook-integrated daemon whose database lives on disk, with the handle
/// that reopens it. A restart is the interesting case for anything the
/// daemon only knows in memory, since the agents on a remote host keep
/// running across one.
pub struct DaemonRestart {
    db_path: PathBuf,
    scrollback_dir: PathBuf,
    socket_path: PathBuf,
    agent: AgentKind,
}

impl DaemonRestart {
    /// Drops the running daemon and opens a fresh one over the same state,
    /// keeping the temp directory alive through the original env.
    pub fn reopen(&self, previous: &TestEnv) -> TestEnv {
        let mut registry = AdapterRegistry::empty();
        registry.register(Box::new(TestAgentAdapter {
            program: testagent_bin(),
        }));
        registry.register(Box::new(TestHookedAdapter { kind: self.agent }));
        let config = DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward: Default::default(),
            db_path: Some(self.db_path.clone()),
            socket_path: self.socket_path.clone(),
            http_addr: None,
            http_tls: None,
            worker_addr: None,
            public_url: None,
            scrollback_dir: self.scrollback_dir.clone(),
            registry,
            local_worker_enabled: true,
            release_channel: None,
        };
        let (daemon, channels) = Daemon::new(config).unwrap();
        TestEnv {
            daemon: Arc::new(daemon),
            exit_rx: channels.exit_rx,
            program_status_rx: Some(channels.program_status_rx),
            project_id: previous.project_id,
            _tmp: tempfile::tempdir().unwrap(),
        }
    }
}

pub fn restartable_hooked_agent_daemon_env(agent: AgentKind) -> (TestEnv, DaemonRestart) {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let scrollback_dir = tmp.path().join("scrollback");
    let socket_path = tmp.path().join("unused.sock");
    let mut registry = AdapterRegistry::empty();
    registry.register(Box::new(TestAgentAdapter {
        program: testagent_bin(),
    }));
    registry.register(Box::new(TestHookedAdapter { kind: agent }));
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: Some(db_path.clone()),
        socket_path: socket_path.clone(),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: scrollback_dir.clone(),
        registry,
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, channels) = Daemon::new(config).unwrap();
    let daemon = Arc::new(daemon);
    let bucket_id = daemon.create_bucket("test-bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "test-project", tmp.path().to_str().unwrap())
        .unwrap();
    (
        TestEnv {
            daemon,
            exit_rx: channels.exit_rx,
            program_status_rx: Some(channels.program_status_rx),
            project_id,
            _tmp: tmp,
        },
        DaemonRestart {
            db_path,
            scrollback_dir,
            socket_path,
            agent,
        },
    )
}

pub fn spawn_test_session(env: &TestEnv, prompt: &str) -> u64 {
    spawn_agent_session(env, AgentKind::Test, prompt)
}

/// [`spawn_test_session`] for a chosen agent kind, which the registry
/// must carry a stand-in for.
pub fn spawn_agent_session(env: &TestEnv, agent: AgentKind, prompt: &str) -> u64 {
    env.daemon
        .spawn_session(
            env.project_id,
            agent,
            "test task",
            prompt,
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap()
}

/// Reads from a broadcast receiver until the accumulated output
/// contains `needle`, panicking on timeout.
pub async fn await_output(
    rx: &mut tokio::sync::broadcast::Receiver<bytes::Bytes>,
    mut acc: Vec<u8>,
    needle: &[u8],
) -> Vec<u8> {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if acc.windows(needle.len().max(1)).any(|w| w == needle) {
                return acc;
            }
            let chunk = match rx.recv().await {
                Ok(chunk) => chunk,
                // A viewer that falls behind a deliberate flood loses
                // chunks, exactly as a real one does. The needle being
                // waited for still arrives; losing it surfaces as the
                // timeout below rather than as a channel error.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!(
                        "output stream closed before {:?}",
                        String::from_utf8_lossy(needle)
                    )
                }
            };
            acc.extend_from_slice(&chunk);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for {:?}",
            String::from_utf8_lossy(needle)
        )
    })
}

/// Answers the two things the controller asks a host about itself
/// before it can act, so a test reads the rest of the link unchanged
/// rather than also standing in for a host's filesystem and agents.
///
/// A remote spawn asks whether the project path is usable there, which
/// is answered "usable". A message for a session's agent asks the host
/// to hand it to the agent's own inbox, which is answered "no channel":
/// there is no agent process behind a simulated worker, so the
/// controller falls back to the terminal, which is what these tests
/// watch. A test about the inbox itself answers for its own worker.
pub fn answering_host_requests(
    daemon: Arc<Daemon>,
    worker_id: u64,
    mut rx: mpsc::Receiver<pm_protocol::domain::ControllerMsg>,
) -> mpsc::Receiver<pm_protocol::domain::ControllerMsg> {
    let (tx, forwarded) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                pm_protocol::domain::ControllerMsg::PathCheck { req_id, .. } => {
                    daemon.apply_worker_message(
                        worker_id,
                        pm_protocol::domain::WorkerMsg::PathChecked {
                            req_id,
                            status: pm_protocol::domain::PathCheck::Ok,
                            detail: String::new(),
                        },
                    );
                }
                pm_protocol::domain::ControllerMsg::AgentInbox { req_id, mode, .. } => {
                    daemon.apply_worker_message(
                        worker_id,
                        pm_protocol::domain::WorkerMsg::AgentInboxResult {
                            req_id,
                            outcome: pm_protocol::domain::AgentInboxOutcome::NoChannel,
                            transport: String::new(),
                            mode,
                            detail: "no agent runs behind this worker".into(),
                        },
                    );
                }
                other => {
                    if tx.send(other).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    forwarded
}

pub fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn hook(env: &TestEnv, session: u64, kind: HookKind, detail: &str) {
    let token = env.daemon.session_token(session).unwrap().unwrap();
    env.daemon
        .handle_hook_event(&token, kind, detail, "", "", false)
        .unwrap();
}

pub fn hook_with_background_work(
    env: &TestEnv,
    session: u64,
    kind: HookKind,
    detail: &str,
    background_work: bool,
) {
    let token = env.daemon.session_token(session).unwrap().unwrap();
    env.daemon
        .handle_hook_event(&token, kind, detail, "", "", background_work)
        .unwrap();
}

pub fn spawn_supervisor(env: &TestEnv) -> u64 {
    env.daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "supervisor",
            "supervise the board",
            None,
            PermissionMode::Inherit,
            None,
            true,
            true,
            None,
        )
        .unwrap()
}

pub fn seed_item(env: &TestEnv, key: &str) -> u64 {
    let bucket_id = bucket_of(env);
    let write = ItemWrite {
        bucket_id,
        external_key: Some(key.into()),
        title: Some(key.into()),
        ..ItemWrite::default()
    };
    env.daemon
        .upsert_item(bucket_id, &write, None)
        .unwrap()
        .0
        .id
}

/// The bucket the test project lives in. A file-backed database also
/// carries the default bucket, so the project is what identifies it.
pub fn bucket_of(env: &TestEnv) -> u64 {
    env.daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == env.project_id)
        .unwrap()
        .bucket_id
}

/// A child spawned through the supervisor path, so it is linked to an
/// item and the wake's audit note has somewhere to land.
pub async fn spawn_child(env: &TestEnv, supervisor: u64, key: &str) -> (u64, u64) {
    let item_id = seed_item(env, key);
    let (session_id, _) = env
        .daemon
        .supervisor_spawn(
            supervisor,
            bucket_of(env),
            &json!(env.project_id),
            Some(AgentKind::Test),
            "child",
            "do the work",
            &json!(item_id),
            &serde_json::Value::Null,
        )
        .await
        .unwrap();
    (session_id, item_id)
}

/// Ends the supervisor's turn, which is the state the wake exists for.
pub fn end_supervisor_turn(env: &TestEnv, supervisor: u64) {
    hook(env, supervisor, HookKind::TurnEnded, "");
}

/// Dials the controller's host plane the way `pm worker` does: mutual TLS
/// first, then the WebSocket carrying the single-use stream token.
pub async fn dial_worker_plane(
    addr: std::net::SocketAddr,
    identity: &pm_tls::Identity,
    token: &str,
) -> anyhow::Result<
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let config = pm_tls::client_config(identity, &pm_tls::PeerPolicy::Pairing)?;
    let tcp = tokio::net::TcpStream::connect(addr).await?;
    let stream = tokio_rustls::TlsConnector::from(config)
        .connect(pm_tls::peer_server_name(), tcp)
        .await?;
    let url = format!("wss://{addr}/worker/stream");
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        format!("Bearer {token}").parse()?,
    );
    let (ws, _) = tokio_tungstenite::client_async(request, stream).await?;
    Ok(ws)
}

/// Plays the worker's half of every forward stream the controller asks
/// for: answers the dial on the control link, dials the stream endpoint
/// back with the single-use token, and splices it to the target the way
/// `pm worker` does. Counts the requests, which is what says whether
/// the proxy opened a new upstream or reused one it had.
///
/// A `stall` notification freezes every stream open at that moment:
/// the splice stops being polled but keeps both its sockets, which is
/// what a worker whose machine vanished looks like from the controller.
/// Streams opened afterwards relay normally.
pub async fn relay_forward_opens(
    daemon: std::sync::Arc<pm_daemon::Daemon>,
    worker_id: u64,
    mut rx: tokio::sync::mpsc::Receiver<pm_protocol::domain::ControllerMsg>,
    worker_addr: std::net::SocketAddr,
    identity: pm_tls::Identity,
    opens: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stall: Option<std::sync::Arc<tokio::sync::Notify>>,
) {
    use pm_protocol::domain::{ControllerMsg, WorkerMsg};

    while let Some(message) = rx.recv().await {
        let ControllerMsg::ForwardOpen {
            req_id,
            port,
            token,
        } = message
        else {
            continue;
        };
        opens.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        daemon.apply_worker_message(
            worker_id,
            WorkerMsg::ForwardOpened {
                req_id,
                ok: true,
                error: String::new(),
            },
        );
        let ws = dial_worker_plane(worker_addr, &identity, &token)
            .await
            .expect("the stream endpoint accepts the worker's dial-back");
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        match stall.clone() {
            None => {
                tokio::spawn(splice_stream_to_target(ws, tcp));
            }
            Some(stall) => {
                tokio::spawn(async move {
                    let mut splice = std::pin::pin!(splice_stream_to_target(ws, tcp));
                    tokio::select! {
                        _ = &mut splice => {}
                        _ = stall.notified() => std::future::pending::<()>().await,
                    }
                });
            }
        }
    }
}

pub async fn splice_stream_to_target(
    ws: tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    tcp: tokio::net::TcpStream,
) {
    use futures::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::Message;

    let (mut sink, mut stream) = ws.split();
    let (mut target_read, mut target_write) = tokio::io::split(tcp);
    let inbound = async {
        while let Some(Ok(Message::Binary(data))) = stream.next().await {
            if target_write.write_all(&data).await.is_err() {
                break;
            }
        }
        let _ = target_write.shutdown().await;
    };
    let outbound = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match target_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if sink
                        .send(Message::Binary(buf[..n].to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        let _ = sink.close().await;
    };
    tokio::join!(inbound, outbound);
}

/// Where `session`'s own agent would bind its inbox, inside a directory
/// the scripted adapter is pointed at from here.
///
/// A test socket bound anywhere else accepts whichever pid the resolution
/// happened to find, which is how a message reaches the wrong agent while
/// still reporting a delivery. Binding here means a test exercises the
/// resolution a real session goes through.
pub fn agent_inbox_path(env: &TestEnv, dir: &std::path::Path, session: u64) -> PathBuf {
    std::env::set_var(pm_adapters::TEST_INBOX_DIR_ENV, dir);
    let terminal = env.daemon.agent_terminal(session).unwrap();
    let pid = env
        .daemon
        .mux
        .child_pid(terminal.id)
        .expect("a live agent terminal child");
    dir.join(format!("{pid}.sock"))
}

/// Stands in for a session's agent listening on its own inbox, reporting
/// the frames it is sent line by line.
pub struct AgentInbox {
    _dir: tempfile::TempDir,
    pub lines: tokio::sync::mpsc::Receiver<String>,
}

impl Drop for AgentInbox {
    fn drop(&mut self) {
        std::env::remove_var(pm_adapters::TEST_INBOX_DIR_ENV);
    }
}

pub fn agent_inbox(env: &TestEnv, session: u64) -> AgentInbox {
    let dir = tempfile::tempdir().unwrap();
    let listener =
        tokio::net::UnixListener::bind(agent_inbox_path(env, dir.path(), session)).unwrap();
    let (tx, lines) = tokio::sync::mpsc::channel::<String>(8);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut reader = tokio::io::BufReader::new(&mut stream);
            let mut line = String::new();
            while let Ok(n) = tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line).await {
                if n == 0 {
                    break;
                }
                let _ = tx.send(line.clone()).await;
                line.clear();
            }
        }
    });
    AgentInbox { _dir: dir, lines }
}
