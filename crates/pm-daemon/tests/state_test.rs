//! State engine: hook-driven transitions and their rejection paths.

mod support;

use pm_adapters::{
    AdapterError, AdapterRegistry, AgentAdapter, CommandSpec, SpawnCtx, SpawnPlan,
    ENV_SESSION_TOKEN, ENV_SOCKET,
};
use pm_protocol::domain::{
    AgentKind, HookKind, ItemStatus, ItemWrite, PermissionMode, RespondTarget, SessionState,
};
use support::*;

struct HookHarnessAdapter(AgentKind);

impl AgentAdapter for HookHarnessAdapter {
    fn kind(&self) -> AgentKind {
        self.0
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        self.plan(ctx, &ctx.task_prompt)
    }

    fn resume_command(
        &self,
        ctx: &SpawnCtx,
        agent_session_id: &str,
    ) -> Result<SpawnPlan, AdapterError> {
        self.plan(ctx, &format!("resumed:{agent_session_id}"))
    }
}

impl HookHarnessAdapter {
    fn plan(&self, ctx: &SpawnCtx, prompt: &str) -> Result<SpawnPlan, AdapterError> {
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: testagent_bin().display().to_string(),
                args: vec![prompt.to_string()],
                env: vec![
                    (
                        ENV_SOCKET.into(),
                        ctx.integration.socket_path.display().to_string(),
                    ),
                    (
                        ENV_SESSION_TOKEN.into(),
                        ctx.integration.session_token.clone(),
                    ),
                ],
                cwd: ctx.cwd.clone(),
            },
            agent_session_id: Some(format!("hook-session-{}", ctx.integration.session_id)),
            detect_osc9_needs_input: self.0 == AgentKind::Codex,
        })
    }
}

#[test]
fn disabled_local_worker_is_hidden_and_rejects_default_spawns() {
    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: false,
        release_channel: None,
        forward: Default::default(),
    };
    let (daemon, _channels) = pm_daemon::Daemon::new(config).unwrap();
    let bucket = daemon.create_bucket("bucket").unwrap();
    let project = daemon
        .create_project(bucket, "project", tmp.path().to_str().unwrap())
        .unwrap();

    assert!(daemon.subscribe().0.workers.is_empty());
    assert!(daemon.list_workers().unwrap().is_empty());
    let error = daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "task",
            "prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "local worker is disabled for this daemon; select a remote worker"
    );
    assert!(daemon.subscribe().0.sessions.is_empty());
}

#[test]
fn local_worker_storage_returns_when_reenabled() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = |local_worker_enabled| pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        db_path: Some(db_path.clone()),
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled,
        release_channel: None,
        forward: Default::default(),
    };

    let (disabled, _channels) = pm_daemon::Daemon::new(config(false)).unwrap();
    let disabled_snapshot = disabled.subscribe().0;
    assert!(disabled_snapshot.workers.is_empty());
    assert_eq!(disabled_snapshot.buckets.len(), 1);
    assert_eq!(disabled_snapshot.buckets[0].name, "Default");
    assert_eq!(disabled_snapshot.buckets[0].default_worker_id, 0);
    assert_eq!(disabled_snapshot.buckets[0].allowed_worker_ids, vec![0]);
    drop(disabled);

    let (enabled, _channels) = pm_daemon::Daemon::new(config(true)).unwrap();
    let workers = enabled.subscribe().0.workers;
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].id, pm_protocol::domain::LOCAL_WORKER_ID);
    assert!(workers[0].online);
}

#[tokio::test]
async fn disabled_local_worker_does_not_recover_desired_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = |local_worker_enabled| pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        db_path: Some(db_path.clone()),
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled,
        release_channel: None,
        forward: Default::default(),
    };
    let (enabled, mut channels) = pm_daemon::Daemon::new(config(true)).unwrap();
    let bucket = enabled.create_bucket("bucket").unwrap();
    let project = enabled
        .create_project(bucket, "project", tmp.path().to_str().unwrap())
        .unwrap();
    let session = enabled
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "task",
            "prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    assert_eq!(enabled.begin_shutdown(), 1);
    let exit = tokio::time::timeout(TEST_TIMEOUT, channels.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    enabled.handle_session_exit(exit);
    let generation = enabled.agent_terminal(session).unwrap().generation;
    drop(enabled);

    let (disabled, _channels) = pm_daemon::Daemon::new(config(false)).unwrap();
    disabled.recover_local_terminals();
    let terminal = disabled.agent_terminal(session).unwrap();
    assert_eq!(terminal.generation, generation);
    assert!(!disabled.mux.is_running(terminal.id));
}

fn session_of(daemon: &pm_daemon::Daemon, id: u64) -> pm_protocol::domain::Session {
    daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|session| session.id == id)
        .unwrap()
}

fn state_of(env: &TestEnv, id: u64) -> (SessionState, String) {
    let session = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    (session.state, session.state_detail)
}

fn bucket_of(env: &TestEnv) -> u64 {
    env.daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|project| project.id == env.project_id)
        .unwrap()
        .bucket_id
}

fn spawn_supervisor(env: &TestEnv, project_id: u64) -> u64 {
    env.daemon
        .spawn_session(
            project_id,
            AgentKind::Test,
            "supervisor",
            "supervise",
            None,
            PermissionMode::Inherit,
            None,
            true,
            true,
            None,
        )
        .unwrap()
}

fn seed_blocked_question(env: &TestEnv) -> u64 {
    let bucket_id = bucket_of(env);
    env.daemon
        .upsert_item(
            bucket_id,
            &ItemWrite {
                bucket_id,
                title: Some("choose the rollout".into()),
                body: Some("Choose a staged rollout plan for the next release.".into()),
                question: Some("Ship to everyone?".into()),
                status: Some(ItemStatus::Blocked),
                note: Some("The canary environment is already available.".into()),
                ..ItemWrite::default()
            },
            None,
        )
        .unwrap()
        .0
        .id
}

#[tokio::test]
async fn hooks_drive_the_session_state_machine() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_hook_event(
            &token,
            HookKind::NeedsInput,
            "which db should I use?",
            "",
            "",
            false,
        )
        .unwrap();
    assert_eq!(
        state_of(&env, id),
        (SessionState::NeedsInput, "which db should I use?".into())
    );

    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "", "", false)
        .unwrap();
    assert_eq!(state_of(&env, id).0, SessionState::Working);

    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert_eq!(state_of(&env, id).0, SessionState::Idle);

    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnFailed, "overloaded", "", "", false)
        .unwrap();
    assert_eq!(
        state_of(&env, id),
        (SessionState::Idle, "overloaded".into())
    );
}

#[tokio::test]
async fn an_unanswered_prompt_notice_leaves_the_session_idle() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert_eq!(state_of(&env, id).0, SessionState::Idle);

    // Claude sends this once the user has gone quiet, which says nothing about
    // the session and must not read as a session that wants something.
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::NeedsInput,
            "Claude is waiting for your input",
            "",
            "",
            false,
        )
        .unwrap();
    assert_eq!(
        state_of(&env, id).0,
        SessionState::Idle,
        "an unanswered prompt is not a session that needs input"
    );

    // An approval prompt is a real block and still asks for attention.
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::NeedsInput,
            "Claude needs your permission to use Bash",
            "",
            "",
            false,
        )
        .unwrap();
    assert_eq!(
        state_of(&env, id),
        (
            SessionState::NeedsInput,
            "Claude needs your permission to use Bash".into()
        )
    );
}

#[tokio::test]
async fn an_unanswered_prompt_notice_never_latches_across_a_turn() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::NeedsInput,
            "Claude is waiting for your input",
            "",
            "",
            false,
        )
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();

    // A turn ending deliberately preserves needs-input so a real question
    // survives it, which is exactly what would make a wrongly set one stick.
    assert_eq!(state_of(&env, id).0, SessionState::Idle);
}

#[tokio::test]
async fn turn_end_nudges_to_report_until_a_headline_is_set() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    let nudge = env
        .daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert!(
        nudge.is_some_and(|r| r.contains("report")),
        "a turn ending with no headline should nudge the agent to report"
    );

    env.daemon
        .handle_agent_report(
            &token,
            pm_daemon::daemon::AgentReport::Report {
                goal: String::new(),
                headline: "building the thing".into(),
                summary: None,
                note: String::new(),
                glance: None,
                context: None,
                clear: Vec::new(),
                git: Default::default(),
            },
        )
        .unwrap();

    let after = env
        .daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert!(
        after.is_none(),
        "once a headline is set the stop hook must not nudge"
    );
}

#[tokio::test]
async fn needs_input_survives_turn_end_until_user_input_acknowledges_it() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "pick one", "", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnFailed, "rate_limit", "", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert_eq!(
        state_of(&env, id),
        (SessionState::NeedsInput, "pick one".into())
    );

    let terminal = env.daemon.agent_terminal(id).unwrap();
    env.daemon
        .terminal_input(terminal.id, bytes::Bytes::from_static(b"choice\r"));
    assert_eq!(state_of(&env, id), (SessionState::Working, String::new()));
}

#[tokio::test]
async fn hook_authority_keeps_stop_idle_despite_later_terminal_activity() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let terminal = env.daemon.agent_terminal(id).unwrap();

    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    env.daemon
        .handle_terminal_activity(terminal.id, terminal.generation);

    assert_eq!(state_of(&env, id), (SessionState::Idle, String::new()));
    assert!(session_of(&env.daemon, id).last_agent_activity_at_unix_ms > 0);
}

#[tokio::test]
async fn claude_and_codex_pty_redraws_never_replace_lifecycle_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut registry = AdapterRegistry::empty();
    registry.register(Box::new(HookHarnessAdapter(AgentKind::ClaudeCode)));
    registry.register(Box::new(HookHarnessAdapter(AgentKind::Codex)));
    let (daemon, _channels) = pm_daemon::Daemon::new(pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry,
        local_worker_enabled: true,
        release_channel: None,
        forward: Default::default(),
    })
    .unwrap();
    let bucket = daemon.create_bucket("bucket").unwrap();
    let project = daemon
        .create_project(bucket, "project", tmp.path().to_str().unwrap())
        .unwrap();

    for agent in [AgentKind::ClaudeCode, AgentKind::Codex] {
        let id = daemon
            .spawn_session(
                project,
                agent,
                "task",
                "prompt",
                None,
                PermissionMode::Inherit,
                None,
                true,
                false,
                None,
            )
            .unwrap();
        let token = daemon.session_token(id).unwrap().unwrap();
        let terminal = daemon.agent_terminal(id).unwrap();

        daemon
            .handle_hook_event(&token, HookKind::Started, "", "", "", false)
            .unwrap();
        daemon
            .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
            .unwrap();
        daemon.terminal_input(terminal.id, bytes::Bytes::from_static(b"x"));
        daemon.handle_terminal_activity(terminal.id, terminal.generation);
        assert_eq!(
            session_of(&daemon, id).state,
            SessionState::Idle,
            "{agent:?} typing and redraw output are recency only"
        );

        daemon.terminal_input(terminal.id, bytes::Bytes::from_static(b"\r"));
        assert_eq!(session_of(&daemon, id).state, SessionState::Idle);
        daemon
            .handle_hook_event(&token, HookKind::PromptSubmitted, "", "", "", false)
            .unwrap();
        assert_eq!(session_of(&daemon, id).state, SessionState::Working);
    }
}

#[tokio::test]
async fn needs_input_attention_is_seen_separately_and_only_submission_acknowledges() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    env.daemon.handle_pty_needs_input(id);
    assert_eq!(state_of(&env, id).0, SessionState::NeedsInput);
    assert!(session_of(&env.daemon, id).needs_input_unseen);

    env.daemon.mark_session_seen(id).unwrap();
    let seen = session_of(&env.daemon, id);
    assert_eq!(seen.state, SessionState::NeedsInput);
    assert!(!seen.needs_input_unseen);

    let terminal = env.daemon.agent_terminal(id).unwrap();
    env.daemon
        .terminal_input(terminal.id, bytes::Bytes::from_static(b"n"));
    env.daemon.terminal_input(
        terminal.id,
        bytes::Bytes::from_static(b"\x1b[<0;12;8M\x1b[<0;12;8m\x1b[A"),
    );
    env.daemon.terminal_input_with_submission(
        terminal.id,
        bytes::Bytes::from_static(b"pasted\ntext"),
        false,
    );

    assert_eq!(state_of(&env, id).0, SessionState::NeedsInput);

    env.daemon
        .terminal_input_with_submission(terminal.id, bytes::Bytes::from_static(b"\r"), true);
    assert_eq!(state_of(&env, id), (SessionState::Working, String::new()));

    env.daemon.handle_pty_needs_input(id);
    assert!(session_of(&env.daemon, id).needs_input_unseen);
    env.daemon.pty_input(id, bytes::Bytes::from_static(b"\r"));

    assert_eq!(state_of(&env, id), (SessionState::Working, String::new()));
    let session = session_of(&env.daemon, id);
    assert!(session.last_user_interaction_at_unix_ms > 0);
    assert!(session.last_activity_at_unix_ms >= session.last_user_interaction_at_unix_ms);
    env.daemon.checkpoint_session_activity();
}

#[tokio::test]
async fn a_turn_finishing_working_reads_as_unseen_idle_until_viewed() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    assert_eq!(state_of(&env, id).0, SessionState::Working);
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    let fresh = session_of(&env.daemon, id);
    assert_eq!(fresh.state, SessionState::Idle);
    assert!(fresh.idle_unseen);

    env.daemon.mark_session_seen(id).unwrap();
    let seen = session_of(&env.daemon, id);
    assert_eq!(seen.state, SessionState::Idle);
    assert!(!seen.idle_unseen);

    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert!(session_of(&env.daemon, id).idle_unseen);
}

#[tokio::test]
async fn prompt_submission_clears_unseen_idle_without_viewing() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert!(session_of(&env.daemon, id).idle_unseen);

    env.daemon
        .handle_hook_event(&token, HookKind::PromptSubmitted, "", "", "", false)
        .unwrap();
    assert_eq!(state_of(&env, id).0, SessionState::Working);
    assert!(!session_of(&env.daemon, id).idle_unseen);

    // A turn that fails re-enters idle without having finished anything new.
    env.daemon
        .handle_hook_event(&token, HookKind::TurnFailed, "overloaded", "", "", false)
        .unwrap();
    assert_eq!(state_of(&env, id).0, SessionState::Idle);
    assert!(!session_of(&env.daemon, id).idle_unseen);
}

#[tokio::test]
async fn an_empty_interactive_launch_starts_idle_but_not_unseen() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_hook_event(&token, HookKind::Started, "", "real-agent-id", "", false)
        .unwrap();
    let session = session_of(&env.daemon, id);
    assert_eq!(session.state, SessionState::Idle);
    assert!(!session.idle_unseen);

    // A turn ending while already idle finished nothing the user missed.
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert!(!session_of(&env.daemon, id).idle_unseen);
}

#[tokio::test]
async fn preserved_needs_input_never_gains_unseen_idle() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "pick a db", "", "", false)
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    let session = session_of(&env.daemon, id);
    assert_eq!(session.state, SessionState::NeedsInput);
    assert!(session.needs_input_unseen);
    assert!(!session.idle_unseen);
}

#[tokio::test]
async fn responding_to_item_routes_to_live_supervisor_and_updates_item() {
    let env = daemon_env();
    let item_id = seed_blocked_question(&env);
    let supervisor_id = spawn_supervisor(&env, env.project_id);
    let terminal = env.daemon.agent_terminal(supervisor_id).unwrap();
    let (replay, mut output) = env.daemon.mux.attach(terminal.id).unwrap();

    let routed = env
        .daemon
        .respond_to_item(
            bucket_of(&env),
            item_id,
            "Roll out to ten percent first.",
            RespondTarget::Session(supervisor_id),
        )
        .await
        .unwrap();
    assert_eq!(routed, Some(supervisor_id));

    let item = env.daemon.get_item(bucket_of(&env), item_id).unwrap();
    assert!(item.question.is_empty());
    assert_eq!(item.status, ItemStatus::InProgress);
    let notes = env.daemon.item_notes(bucket_of(&env), item_id).unwrap();
    assert!(notes.iter().any(|note| {
        note.kind == "user_reply" && note.text == "Roll out to ten percent first."
    }));
    await_output(
        &mut output,
        replay.to_vec(),
        format!("User replied on item {item_id}. Read the item and continue.").as_bytes(),
    )
    .await;
}

#[tokio::test]
async fn responding_to_item_rejects_invalid_session_targets() {
    let mut env = daemon_env();
    let item_id = seed_blocked_question(&env);

    let regular_session = spawn_test_session(&env, "regular");
    let error = env
        .daemon
        .respond_to_item(
            bucket_of(&env),
            item_id,
            "answer",
            RespondTarget::Session(regular_session),
        )
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not offer the supervisor API"));

    let dead_session = spawn_supervisor(&env, env.project_id);
    env.daemon.kill_session(dead_session).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);
    let error = env
        .daemon
        .respond_to_item(
            bucket_of(&env),
            item_id,
            "answer",
            RespondTarget::Session(dead_session),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already ended"));

    let other_bucket = env.daemon.create_bucket("other-bucket").unwrap();
    let other_project = env
        .daemon
        .create_project(other_bucket, "other-project", "/tmp")
        .unwrap();
    let cross_bucket_session = spawn_supervisor(&env, other_project);
    let error = env
        .daemon
        .respond_to_item(
            bucket_of(&env),
            item_id,
            "answer",
            RespondTarget::Session(cross_bucket_session),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("is not in item"));

    assert_eq!(
        env.daemon
            .get_item(bucket_of(&env), item_id)
            .unwrap()
            .question,
        "Ship to everyone?"
    );
}

#[tokio::test]
async fn responding_to_item_reply_only_records_without_routing() {
    let env = daemon_env();
    let item_id = seed_blocked_question(&env);
    let routed = env
        .daemon
        .respond_to_item(
            bucket_of(&env),
            item_id,
            "Keep it blocked.",
            RespondTarget::ReplyOnly,
        )
        .await
        .unwrap();
    assert_eq!(routed, None);
    let item = env.daemon.get_item(bucket_of(&env), item_id).unwrap();
    assert!(item.question.is_empty());
    assert_eq!(item.status, ItemStatus::Blocked);
}

#[tokio::test]
async fn responding_to_item_spawns_and_links_a_new_supervisor() {
    let env = daemon_env();
    let item_id = seed_blocked_question(&env);
    let bucket_id = bucket_of(&env);
    env.daemon
        .set_bucket_permission_mode(bucket_id, PermissionMode::Auto)
        .unwrap();

    let session_id = env
        .daemon
        .respond_to_item(
            bucket_id,
            item_id,
            "Roll out to ten percent first.",
            RespondTarget::NewSupervisor {
                project_id: env.project_id,
            },
        )
        .await
        .unwrap()
        .unwrap();

    let snapshot = env.daemon.subscribe().0;
    let project = snapshot
        .projects
        .iter()
        .find(|project| project.id == env.project_id)
        .unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(session.project_id, env.project_id);
    assert_eq!(session.agent, AgentKind::Test);
    assert_eq!(session.cwd, project.path);
    assert_eq!(session.worker_id, pm_protocol::domain::LOCAL_WORKER_ID);
    assert_eq!(session.permission_mode, PermissionMode::Auto);
    assert!(session.items_api);
    assert!(session.supervisor_api);
    assert_eq!(session.spawned_by_session_id, None);
    assert!(session.task_prompt.contains(&format!(
        "You are the supervisor for item {item_id}. The user was asked: Ship to everyone?\nThey replied: Roll out to ten percent first.\nRead the item and continue."
    )));
    for context in [
        "choose the rollout",
        "Choose a staged rollout plan for the next release.",
        "The canary environment is already available.",
    ] {
        assert!(
            session.task_prompt.contains(context),
            "spawn prompt omitted {context:?}: {}",
            session.task_prompt
        );
    }

    let item = env.daemon.get_item(bucket_id, item_id).unwrap();
    assert!(item.question.is_empty());
    assert_eq!(item.status, ItemStatus::InProgress);
    assert!(item.session_ids.contains(&session_id));
    assert!(env
        .daemon
        .item_notes(bucket_id, item_id)
        .unwrap()
        .iter()
        .any(|note| {
            note.kind == "user_reply" && note.text == "Roll out to ten percent first."
        }));
}

#[tokio::test]
async fn responding_to_item_rejects_a_new_supervisor_outside_the_item_bucket() {
    let env = daemon_env();
    let item_id = seed_blocked_question(&env);
    let other_bucket = env.daemon.create_bucket("other-bucket").unwrap();
    let other_project = env
        .daemon
        .create_project(other_bucket, "other-project", "/tmp")
        .unwrap();

    let error = env
        .daemon
        .respond_to_item(
            bucket_of(&env),
            item_id,
            "This must not be recorded.",
            RespondTarget::NewSupervisor {
                project_id: other_project,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("project {other_project} is not in item {item_id}'s bucket")
    );
    let missing = env
        .daemon
        .respond_to_item(
            bucket_of(&env),
            item_id,
            "This must not be recorded either.",
            RespondTarget::NewSupervisor {
                project_id: u64::MAX,
            },
        )
        .await
        .unwrap_err();
    assert!(missing.to_string().contains("project"));

    let item = env.daemon.get_item(bucket_of(&env), item_id).unwrap();
    assert_eq!(item.question, "Ship to everyone?");
    assert_eq!(item.status, ItemStatus::Blocked);
    assert!(!env
        .daemon
        .item_notes(bucket_of(&env), item_id)
        .unwrap()
        .iter()
        .any(|note| note.kind == "user_reply"));
}

#[tokio::test]
async fn responding_to_item_keeps_the_reply_when_new_supervisor_spawn_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: false,
        release_channel: None,
        forward: Default::default(),
    };
    let (daemon, _channels) = pm_daemon::Daemon::new(config).unwrap();
    let bucket_id = daemon.create_bucket("bucket").unwrap();
    let project_id = daemon
        .create_project(bucket_id, "project", tmp.path().to_str().unwrap())
        .unwrap();
    let item_id = daemon
        .upsert_item(
            bucket_id,
            &ItemWrite {
                bucket_id,
                title: Some("blocked item".into()),
                question: Some("Which path?".into()),
                status: Some(ItemStatus::Blocked),
                ..ItemWrite::default()
            },
            None,
        )
        .unwrap()
        .0
        .id;

    let error = daemon
        .respond_to_item(
            bucket_id,
            item_id,
            "Take the safe path.",
            RespondTarget::NewSupervisor { project_id },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("local worker is disabled"));
    let item = daemon.get_item(bucket_id, item_id).unwrap();
    assert!(item.question.is_empty());
    assert_eq!(item.status, ItemStatus::InProgress);
    assert!(daemon
        .item_notes(bucket_id, item_id)
        .unwrap()
        .iter()
        .any(|note| { note.kind == "user_reply" && note.text == "Take the safe path." }));
}

#[tokio::test]
async fn agent_terminal_output_activity_is_checkpointed_but_stale_generations_are_ignored() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let terminal = env.daemon.agent_terminal(id).unwrap();

    env.daemon
        .handle_terminal_activity(terminal.id, terminal.generation + 1);
    env.daemon.checkpoint_session_activity();
    assert_eq!(
        session_of(&env.daemon, id).last_agent_activity_at_unix_ms,
        0
    );

    env.daemon
        .handle_terminal_activity(terminal.id, terminal.generation);
    let session = session_of(&env.daemon, id);
    assert!(session.last_agent_activity_at_unix_ms > 0);
    assert_eq!(
        session.last_activity_at_unix_ms, session.created_at_unix_ms,
        "output feeds the internal clock, not the one the list shows"
    );
    env.daemon.checkpoint_session_activity();
    assert_eq!(
        session_of(&env.daemon, id).last_activity_at_unix_ms,
        session.created_at_unix_ms
    );
}

#[tokio::test]
async fn stale_local_terminal_exit_cannot_end_the_current_generation() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let terminal = env.daemon.agent_terminal(id).unwrap();

    env.daemon.handle_session_exit(pm_daemon::mux::SessionExit {
        terminal_id: terminal.id,
        generation: terminal.generation + 1,
        semantic_session_id: id,
        exit_code: Some(1),
        scrollback: bytes::Bytes::from_static(b"stale"),
    });

    assert_eq!(session_of(&env.daemon, id).state, SessionState::Working);
    assert!(env.daemon.mux.is_running(terminal.id));
}

#[tokio::test]
async fn unknown_and_empty_tokens_are_rejected() {
    let env = daemon_env();
    spawn_test_session(&env, "p");

    let err = env
        .daemon
        .handle_hook_event("bogus", HookKind::NeedsInput, "", "", "", false)
        .unwrap_err();
    assert!(err.to_string().contains("unknown session token"), "{err}");

    let err = env
        .daemon
        .handle_hook_event("", HookKind::NeedsInput, "", "", "", false)
        .unwrap_err();
    assert!(err.to_string().contains("empty session token"), "{err}");
}

#[tokio::test]
async fn hooks_for_ended_sessions_are_rejected() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    let err = env
        .daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap_err();
    assert!(err.to_string().contains("already ended"), "{err}");
    assert_eq!(state_of(&env, id).0, SessionState::Exited);
}

#[tokio::test]
async fn ended_sessions_resume_in_place() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "original-task");
    let old_generation_token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);
    let title = env.daemon.subscribe().0.sessions[0].task_title.clone();

    let before = env.daemon.subscribe().0.sessions.len();
    let resumed_id = env.daemon.resume_session(id).unwrap();
    assert_eq!(resumed_id, id, "resume reuses the session's own entry");
    let current_generation_token = env.daemon.session_token(id).unwrap().unwrap();
    assert_ne!(old_generation_token, current_generation_token);
    assert!(env
        .daemon
        .handle_hook_event(
            &old_generation_token,
            HookKind::TurnEnded,
            "",
            "",
            "",
            false
        )
        .is_err());
    assert_eq!(state_of(&env, id).0, SessionState::Starting);

    let snapshot = env.daemon.subscribe().0;
    assert_eq!(
        snapshot.sessions.len(),
        before,
        "resume must not add a new session row"
    );
    let s = snapshot.sessions.iter().find(|s| s.id == id).unwrap();
    assert_eq!(s.state, SessionState::Starting);
    assert_eq!(s.task_title, title, "resume preserves the task title");
    assert_eq!(
        s.agent_session_id.as_deref(),
        Some(format!("testsess-{id}").as_str())
    );
    assert_eq!(s.ended_at_unix_ms, None, "resume clears the end timestamp");
    assert_eq!(s.exit_code, None, "resume clears the exit code");

    let (replay, mut rx) = env.daemon.mux.attach(id).unwrap();
    let expected = format!("READY resumed:testsess-{id}");
    await_output(&mut rx, replay.to_vec(), expected.as_bytes()).await;
}

#[tokio::test]
async fn resume_keeps_the_sessions_original_permission_mode() {
    use pm_protocol::domain::{AgentKind, PermissionMode};

    let mut env = daemon_env();
    let id = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "test task",
            "original-task",
            None,
            PermissionMode::Bypass,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    assert_eq!(
        env.daemon
            .subscribe()
            .0
            .sessions
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .permission_mode,
        PermissionMode::Bypass
    );

    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == env.project_id)
        .unwrap();
    env.daemon
        .set_bucket_permission_mode(project.bucket_id, PermissionMode::Default)
        .unwrap();
    env.daemon
        .set_project_permission_mode(project.id, PermissionMode::Auto)
        .unwrap();

    let resumed_id = env.daemon.resume_session(id).unwrap();
    assert_eq!(resumed_id, id);
    let resumed = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert_eq!(resumed.permission_mode, PermissionMode::Bypass);
}

#[tokio::test]
async fn empty_interactive_session_with_no_transcript_restarts_in_place() {
    use pm_protocol::domain::{AgentKind, HookKind, PermissionMode};

    let mut env = daemon_env();
    let id = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "interactive",
            "",
            None,
            PermissionMode::Bypass,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let missing_transcript = env
        .daemon
        .scrollback_path(id)
        .with_file_name("missing.jsonl");
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::Started,
            "",
            "agent-id-with-no-file",
            missing_transcript.to_str().unwrap(),
            false,
        )
        .unwrap();

    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    let resumed_id = env.daemon.resume_session(id).unwrap();
    assert_eq!(resumed_id, id);
    let resumed = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert_eq!(resumed.permission_mode, PermissionMode::Bypass);

    let (replay, mut rx) = env.daemon.mux.attach(id).unwrap();
    let output = await_output(&mut rx, replay.to_vec(), b"READY ").await;
    assert!(
        !String::from_utf8_lossy(&output).contains("resumed:agent-id-with-no-file"),
        "missing transcript should restart an empty interactive session, not agent-native resume"
    );
}

#[tokio::test]
async fn live_and_never_identified_sessions_refuse_resume() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");

    let err = env.daemon.resume_session(id).unwrap_err();
    assert!(err.to_string().contains("still live"), "{err}");

    let missing = env.daemon.resume_session(999).unwrap_err();
    assert!(missing.to_string().contains("not found"), "{missing}");

    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);
}

#[tokio::test]
async fn hooks_capture_agent_identity_and_resume_guards_on_transcript() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    // A live transcript file the hook reports.
    let transcript = env
        .daemon
        .scrollback_path(id)
        .with_file_name(format!("agent-{id}.jsonl"));
    std::fs::write(&transcript, b"conversation").unwrap();
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::TurnEnded,
            "",
            "claude-real-id",
            transcript.to_str().unwrap(),
            false,
        )
        .unwrap();

    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    // With the transcript present, resume proceeds using the captured id.
    let resumed_id = env.daemon.resume_session(id).unwrap();
    assert_eq!(resumed_id, id);
    let resumed = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert_eq!(resumed.agent_session_id.as_deref(), Some("claude-real-id"));

    // End it again, then a resume with the transcript removed refuses.
    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);
    std::fs::remove_file(&transcript).unwrap();
    let err = env.daemon.resume_session(id).unwrap_err();
    // Naming the cause matters: a missing file, a worker that reports the
    // session unresumable, and a session that never recorded an agent id
    // all refuse here and are fixed in different ways.
    assert!(
        err.to_string()
            .contains("recorded transcript file is missing"),
        "{err}"
    );
}

#[tokio::test]
async fn each_session_gets_a_distinct_token() {
    let env = daemon_env();
    let a = spawn_test_session(&env, "p");
    let b = spawn_test_session(&env, "p");
    let ta = env.daemon.session_token(a).unwrap().unwrap();
    let tb = env.daemon.session_token(b).unwrap().unwrap();
    assert_ne!(ta, tb);

    env.daemon
        .handle_hook_event(&tb, HookKind::NeedsInput, "q", "", "", false)
        .unwrap();
    assert_eq!(state_of(&env, a).0, SessionState::Working);
    assert_eq!(state_of(&env, b).0, SessionState::NeedsInput);
}

#[tokio::test]
async fn setting_permission_modes_updates_bucket_and_project() {
    use pm_protocol::domain::PermissionMode;
    let env = daemon_env();
    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .next()
        .unwrap();

    env.daemon
        .set_bucket_permission_mode(project.bucket_id, PermissionMode::Bypass)
        .unwrap();
    env.daemon
        .set_project_permission_mode(project.id, PermissionMode::Auto)
        .unwrap();

    let snap = env.daemon.subscribe().0;
    let b = snap
        .buckets
        .iter()
        .find(|b| b.id == project.bucket_id)
        .unwrap();
    let p = snap.projects.iter().find(|p| p.id == project.id).unwrap();
    assert_eq!(b.permission_mode, PermissionMode::Bypass);
    assert_eq!(p.permission_mode, PermissionMode::Auto);

    // Clearing the project override stores Inherit.
    env.daemon
        .set_project_permission_mode(project.id, PermissionMode::Inherit)
        .unwrap();
    let p = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == project.id)
        .unwrap();
    assert_eq!(p.permission_mode, PermissionMode::Inherit);
}

#[tokio::test]
async fn updating_a_project_publishes_the_changed_project() {
    use pm_protocol::domain::{Event, PermissionMode};

    let env = daemon_env();
    let (_, mut events) = env.daemon.subscribe();
    env.daemon
        .update_project(
            env.project_id,
            Some("/tmp/updated-project"),
            Some(PermissionMode::Auto),
            Some(Some(pm_protocol::domain::LOCAL_WORKER_ID)),
        )
        .unwrap();

    let event = tokio::time::timeout(TEST_TIMEOUT, events.recv())
        .await
        .unwrap()
        .unwrap();
    let Event::ProjectChanged(project) = event else {
        panic!("expected ProjectChanged event");
    };
    assert_eq!(project.id, env.project_id);
    assert_eq!(project.path, "/tmp/updated-project");
    assert_eq!(project.permission_mode, PermissionMode::Auto);
    assert_eq!(
        project.worker_id,
        Some(pm_protocol::domain::LOCAL_WORKER_ID)
    );
}

#[test]
fn clearing_a_project_path_falls_back_to_the_worker_home() {
    let env = daemon_env();

    // Whitespace is not a path. It clears the field, which is how a project
    // goes back to running wherever the worker's own home is.
    env.daemon
        .update_project(env.project_id, Some("  "), None, None)
        .unwrap();

    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|project| project.id == env.project_id)
        .unwrap();
    assert_eq!(project.path, "");
}

#[tokio::test]
async fn session_start_hook_captures_identity_without_changing_state() {
    use pm_protocol::domain::PermissionMode;
    let _ = PermissionMode::Inherit;
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let transcript = tmp.path().join("agent.jsonl");
    std::fs::write(&transcript, b"x").unwrap();

    // Session is Working after spawn; a Started hook must not change that.
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::Started,
            "",
            "real-agent-id",
            transcript.to_str().unwrap(),
            false,
        )
        .unwrap();

    let session = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert_eq!(session.state, SessionState::Working);
    assert_eq!(session.agent_session_id.as_deref(), Some("real-agent-id"));
    assert!(session.resumable);
}

#[tokio::test]
async fn session_start_marks_an_empty_interactive_launch_idle() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    assert_eq!(
        state_of(&env, id).0,
        SessionState::Starting,
        "a launch with no prompt has nothing working yet"
    );

    env.daemon
        .handle_hook_event(&token, HookKind::Started, "", "real-agent-id", "", false)
        .unwrap();

    assert_eq!(state_of(&env, id), (SessionState::Idle, String::new()));
}

#[tokio::test]
async fn a_resumed_session_settles_idle_without_passing_through_working() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let transcript = env.daemon.scrollback_path(id).with_extension("agent.jsonl");
    std::fs::write(&transcript, b"saved conversation").unwrap();
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::Started,
            "",
            "real-agent-id",
            transcript.to_str().unwrap(),
            false,
        )
        .unwrap();
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "real-agent-id", "", false)
        .unwrap();
    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    env.daemon.resume_session(id).unwrap();
    // A resume submits no prompt, so reading it as a working turn would
    // let the start hook report a finish the agent never performed.
    assert_eq!(state_of(&env, id).0, SessionState::Starting);

    let resumed_token = env.daemon.session_token(id).unwrap().unwrap();
    env.daemon
        .handle_hook_event(
            &resumed_token,
            HookKind::Started,
            "",
            "real-agent-id",
            "",
            false,
        )
        .unwrap();

    assert_eq!(state_of(&env, id), (SessionState::Idle, String::new()));
}

#[tokio::test]
async fn session_start_leaves_a_working_turn_alone() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    env.daemon
        .handle_hook_event(
            &token,
            HookKind::PromptSubmitted,
            "",
            "real-agent-id",
            "",
            false,
        )
        .unwrap();

    // Claude fires this hook again on `/clear` and `/compact` mid-turn.
    env.daemon
        .handle_hook_event(&token, HookKind::Started, "", "real-agent-id", "", false)
        .unwrap();

    assert_eq!(state_of(&env, id).0, SessionState::Working);
}

#[tokio::test]
async fn exit_refreshes_a_transcript_created_after_the_start_hook() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let transcript = env.daemon.scrollback_path(id).with_extension("agent.jsonl");

    env.daemon
        .handle_hook_event(
            &token,
            HookKind::Started,
            "",
            "real-agent-id",
            transcript.to_str().unwrap(),
            false,
        )
        .unwrap();
    assert!(!env.daemon.subscribe().0.sessions[0].resumable);

    std::fs::write(&transcript, b"saved conversation").unwrap();
    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    let session = env
        .daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|session| session.id == id)
        .unwrap();
    assert!(session.resumable);
}

#[tokio::test]
async fn startup_repairs_resumability_when_a_transcript_appeared_after_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = || pm_daemon::DaemonConfig {
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
    let (daemon, mut channels) = pm_daemon::Daemon::new(config()).unwrap();
    let bucket = daemon.create_bucket("bucket").unwrap();
    let project = daemon
        .create_project(bucket, "project", tmp.path().to_str().unwrap())
        .unwrap();
    let id = daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "task",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let token = daemon.session_token(id).unwrap().unwrap();
    let transcript = tmp.path().join("late-agent.jsonl");
    daemon
        .handle_hook_event(
            &token,
            HookKind::Started,
            "",
            "real-agent-id",
            transcript.to_str().unwrap(),
            false,
        )
        .unwrap();
    daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, channels.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    daemon.handle_session_exit(exit);
    assert!(!daemon.subscribe().0.sessions[0].resumable);
    drop(daemon);

    std::fs::write(&transcript, b"saved conversation").unwrap();
    let (reopened, _) = pm_daemon::Daemon::new(config()).unwrap();
    let session = reopened
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|session| session.id == id)
        .unwrap();
    assert!(session.resumable);
}

#[tokio::test]
async fn explicit_kill_does_not_auto_resume_but_manual_resume_still_works() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = || pm_daemon::DaemonConfig {
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
    let (daemon, mut channels) = pm_daemon::Daemon::new(config()).unwrap();
    let bucket = daemon.create_bucket("bucket").unwrap();
    let project = daemon
        .create_project(bucket, "project", tmp.path().to_str().unwrap())
        .unwrap();
    let id = daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "task",
            "prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, channels.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    daemon.handle_session_exit(exit);
    drop(daemon);

    let (reopened, _channels) = pm_daemon::Daemon::new(config()).unwrap();
    reopened.recover_local_terminals();
    assert_eq!(session_of(&reopened, id).state, SessionState::Exited);
    assert!(!reopened
        .mux
        .is_running(reopened.agent_terminal(id).unwrap().id));

    reopened.resume_session(id).unwrap();
    assert_eq!(session_of(&reopened, id).state, SessionState::Starting);
}

#[tokio::test]
async fn crash_recovers_a_desired_session_idle_until_agent_output() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = || pm_daemon::DaemonConfig {
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
    let (daemon, mut channels) = pm_daemon::Daemon::new(config()).unwrap();
    let bucket = daemon.create_bucket("bucket").unwrap();
    let project = daemon
        .create_project(bucket, "project", tmp.path().to_str().unwrap())
        .unwrap();
    let id = daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "task",
            "prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let original_terminal = daemon.agent_terminal(id).unwrap();
    daemon.mux.kill(original_terminal.id).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, channels.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session_of(&daemon, id).state, SessionState::Working);
    drop(daemon);

    let (reopened, _channels) = pm_daemon::Daemon::new(config()).unwrap();
    reopened.recover_local_terminals();
    let recovered = reopened.agent_terminal(id).unwrap();
    assert_eq!(recovered.id, original_terminal.id);
    assert_eq!(recovered.generation, original_terminal.generation + 1);
    let recovered_session = session_of(&reopened, id);
    assert_eq!(recovered_session.state, SessionState::Idle);
    assert_eq!(
        recovered_session.state_detail,
        "recovered after daemon restart"
    );
    assert!(reopened.mux.is_running(recovered.id));

    reopened.handle_terminal_activity(recovered.id, recovered.generation);
    let active_session = session_of(&reopened, id);
    assert_eq!(active_session.state, SessionState::Working);
    assert!(active_session.state_detail.is_empty());
    assert_eq!(
        active_session.last_activity_at_unix_ms, recovered_session.last_activity_at_unix_ms,
        "the recovery repaint is output, not activity"
    );

    let token = reopened.session_token(id).unwrap().unwrap();
    reopened
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    reopened.handle_terminal_activity(recovered.id, recovered.generation);
    let settled_session = session_of(&reopened, id);
    assert_eq!(
        settled_session.state,
        SessionState::Idle,
        "the recovered PTY fallback is one-shot and hooks permanently supersede it"
    );
    assert!(
        settled_session.last_activity_at_unix_ms > recovered_session.last_activity_at_unix_ms,
        "a turn boundary is activity"
    );

    reopened.terminal_input(recovered.id, bytes::Bytes::from_static(b"next\r"));
    assert_eq!(
        session_of(&reopened, id).state,
        SessionState::Working,
        "a submitted prompt is the explicit hookless fallback"
    );
}

#[tokio::test]
async fn graceful_shutdown_restores_agent_and_shell_generations() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = || pm_daemon::DaemonConfig {
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
    let (daemon, mut channels) = pm_daemon::Daemon::new(config()).unwrap();
    let bucket = daemon.create_bucket("bucket").unwrap();
    let project = daemon
        .create_project(bucket, "project", tmp.path().to_str().unwrap())
        .unwrap();
    let session_id = daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "task",
            "prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let agent = daemon.agent_terminal(session_id).unwrap();
    let shell_id = daemon.create_shell(session_id, "build").unwrap();
    let shell = daemon.terminal(shell_id).unwrap();

    assert_eq!(daemon.begin_shutdown(), 2);
    for _ in 0..2 {
        let exit = tokio::time::timeout(TEST_TIMEOUT, channels.exit_rx.recv())
            .await
            .unwrap()
            .unwrap();
        daemon.handle_session_exit(exit);
    }
    assert_eq!(session_of(&daemon, session_id).state, SessionState::Exited);
    drop(daemon);

    let (reopened, _channels) = pm_daemon::Daemon::new(config()).unwrap();
    reopened.recover_local_terminals();
    let recovered_agent = reopened.agent_terminal(session_id).unwrap();
    let recovered_shell = reopened.terminal(shell_id).unwrap();
    assert_eq!(recovered_agent.generation, agent.generation + 1);
    assert_eq!(recovered_shell.generation, shell.generation + 1);
    assert!(reopened.mux.is_running(recovered_agent.id));
    assert!(reopened.mux.is_running(recovered_shell.id));
}

#[tokio::test]
async fn explicit_shell_close_cleans_up_its_late_exit() {
    let mut env = daemon_env();
    let session_id = spawn_test_session(&env, "prompt");
    let shell_id = env.daemon.create_shell(session_id, "build").unwrap();

    env.daemon.close_terminal(shell_id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.terminal_id, shell_id);
    env.daemon.handle_session_exit(exit);

    assert!(env.daemon.terminal(shell_id).is_err());
    assert!(env.daemon.mux.attach(shell_id).is_err());
    assert_eq!(
        session_of(&env.daemon, session_id).state,
        SessionState::Working
    );
}

#[tokio::test]
async fn clean_shell_exit_removes_terminal_without_ending_agent_session() {
    use pm_protocol::domain::Event;

    let mut env = daemon_env();
    let session_id = spawn_test_session(&env, "prompt");
    let shell_id = env.daemon.create_shell(session_id, "build").unwrap();
    let (_, mut events) = env.daemon.subscribe();

    env.daemon
        .terminal_input(shell_id, bytes::Bytes::from_static(b"exit\n"));
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.exit_code, Some(0));
    env.daemon.handle_session_exit(exit);

    assert!(env.daemon.terminal(shell_id).is_err());
    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, events.recv())
            .await
            .unwrap()
            .unwrap(),
        Event::TerminalRemoved(shell_id),
    );
    assert_eq!(
        session_of(&env.daemon, session_id).state,
        SessionState::Working
    );
    env.daemon.close_terminal(shell_id).unwrap();
}

#[tokio::test]
async fn nonzero_shell_exit_remains_visible_for_diagnostics() {
    let mut env = daemon_env();
    let session_id = spawn_test_session(&env, "prompt");
    let shell_id = env.daemon.create_shell(session_id, "build").unwrap();

    env.daemon
        .terminal_input(shell_id, bytes::Bytes::from_static(b"exit 7\n"));
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    let shell = env.daemon.terminal(shell_id).unwrap();
    assert_eq!(shell.state, pm_protocol::domain::TerminalRunState::Exited);
    assert_eq!(shell.exit_code, Some(7));
    assert_eq!(
        session_of(&env.daemon, session_id).state,
        SessionState::Working
    );
}

#[tokio::test]
async fn agent_exit_keeps_its_terminal_identity() {
    let mut env = daemon_env();
    let session_id = spawn_test_session(&env, "prompt");
    let agent_id = env.daemon.agent_terminal(session_id).unwrap().id;

    env.daemon.kill_session(session_id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    assert_eq!(env.daemon.agent_terminal(session_id).unwrap().id, agent_id);
    assert_eq!(
        session_of(&env.daemon, session_id).state,
        SessionState::Exited
    );
}

#[tokio::test]
async fn spawn_publishes_the_agent_terminal_to_subscribers() {
    use pm_protocol::domain::{Event, TerminalKind};

    let env = daemon_env();
    let (_, mut events) = env.daemon.subscribe();
    let id = spawn_test_session(&env, "p");

    // A client subscribed before the spawn opens the new session by
    // terminal identity, so the terminal must arrive as an event, not
    // only in later snapshots.
    let mut saw_agent_terminal = false;
    while let Ok(event) = events.try_recv() {
        if let Event::TerminalChanged(terminal) = event {
            if terminal.session_id == id && terminal.kind == TerminalKind::Agent {
                saw_agent_terminal = true;
            }
        }
    }
    assert!(
        saw_agent_terminal,
        "a fresh spawn must publish its agent terminal"
    );
}

#[tokio::test]
/// A turn that ends while the agent still has a backgrounded subagent
/// running has not left the session idle. Reporting it idle invites a
/// supervisor to hand it more work while it is still busy.
async fn a_turn_ending_with_background_work_stays_working() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "background work");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", true)
        .unwrap();
    let session = env.daemon.get_session_exact(id).unwrap();
    assert_eq!(
        session.state,
        SessionState::Working,
        "a backgrounded subagent means the session is still working"
    );

    // The agent hooks again when the last of it lands, and that one is
    // the real end of the turn.
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert_eq!(
        env.daemon.get_session_exact(id).unwrap().state,
        SessionState::Idle
    );
}

#[tokio::test]
async fn settings_validate_persist_and_reset() {
    use pm_daemon::daemon::SETTING_SPAWN_TRUECOLOR;

    let env = daemon_env();

    // Defaults are reported without a stored row.
    let settings = env.daemon.settings().unwrap();
    let truecolor = settings
        .iter()
        .find(|s| s["key"] == SETTING_SPAWN_TRUECOLOR)
        .unwrap();
    assert_eq!(truecolor["value"], "true");
    assert_eq!(truecolor["set"], false);

    // Unknown keys and non-boolean values are refused loudly.
    let err = env
        .daemon
        .set_setting("spawn.truecolour", Some("false"))
        .unwrap_err();
    assert!(err.to_string().contains(SETTING_SPAWN_TRUECOLOR));
    let err = env
        .daemon
        .set_setting(SETTING_SPAWN_TRUECOLOR, Some("yes"))
        .unwrap_err();
    assert!(err.to_string().contains("true or false"));

    env.daemon
        .set_setting(SETTING_SPAWN_TRUECOLOR, Some("false"))
        .unwrap();
    let settings = env.daemon.settings().unwrap();
    let truecolor = settings
        .iter()
        .find(|s| s["key"] == SETTING_SPAWN_TRUECOLOR)
        .unwrap();
    assert_eq!(truecolor["value"], "false");
    assert_eq!(truecolor["set"], true);

    env.daemon
        .set_setting(SETTING_SPAWN_TRUECOLOR, None)
        .unwrap();
    let settings = env.daemon.settings().unwrap();
    assert_eq!(
        settings
            .iter()
            .find(|s| s["key"] == SETTING_SPAWN_TRUECOLOR)
            .unwrap()["value"],
        "true"
    );
}

/// The clock the session list shows moves on what a person or the agent
/// actually did: a submitted line, a turn boundary, a report. Output and
/// typing keep their own clocks for quiet detection.
#[tokio::test]
async fn the_shown_activity_clock_follows_submits_and_turns_not_output_or_typing() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let terminal = env.daemon.agent_terminal(id).unwrap();
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let created = session_of(&env.daemon, id);
    assert_eq!(created.last_activity_at_unix_ms, created.created_at_unix_ms);

    std::thread::sleep(std::time::Duration::from_millis(5));
    env.daemon
        .handle_terminal_activity(terminal.id, terminal.generation);
    env.daemon
        .terminal_input(terminal.id, bytes::Bytes::from_static(b"still typ"));
    let quiet = session_of(&env.daemon, id);
    assert!(quiet.last_agent_activity_at_unix_ms > created.created_at_unix_ms);
    assert!(quiet.last_user_interaction_at_unix_ms > created.created_at_unix_ms);
    assert_eq!(quiet.last_activity_at_unix_ms, created.created_at_unix_ms);
    env.daemon.checkpoint_session_activity();
    assert_eq!(
        session_of(&env.daemon, id).last_activity_at_unix_ms,
        created.created_at_unix_ms
    );

    std::thread::sleep(std::time::Duration::from_millis(5));
    env.daemon
        .terminal_input(terminal.id, bytes::Bytes::from_static(b"ing\r"));
    let submitted = session_of(&env.daemon, id);
    assert!(submitted.last_activity_at_unix_ms > created.created_at_unix_ms);
    env.daemon.checkpoint_session_activity();
    assert_eq!(
        session_of(&env.daemon, id).last_activity_at_unix_ms,
        submitted.last_activity_at_unix_ms,
        "the submit survives the checkpoint"
    );

    std::thread::sleep(std::time::Duration::from_millis(5));
    env.daemon
        .handle_hook_event(&token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    let ended = session_of(&env.daemon, id);
    assert!(ended.last_activity_at_unix_ms > submitted.last_activity_at_unix_ms);

    std::thread::sleep(std::time::Duration::from_millis(5));
    env.daemon
        .handle_hook_event(&token, HookKind::NeedsInput, "pick one", "", "", false)
        .unwrap();
    let asked = session_of(&env.daemon, id);
    assert!(asked.last_activity_at_unix_ms > ended.last_activity_at_unix_ms);

    std::thread::sleep(std::time::Duration::from_millis(5));
    env.daemon
        .update_session_apis(id, Some(true), None)
        .unwrap();
    assert_eq!(
        session_of(&env.daemon, id).last_activity_at_unix_ms,
        asked.last_activity_at_unix_ms,
        "a user toggling session APIs is not the session doing anything"
    );
}

/// A reply to an item is a message, not a burst of keystrokes. An agent
/// TUI folds an Enter that arrives with the characters back into the
/// paste, so a reply written as one raw write never submits and sits in
/// the composer instead.
#[tokio::test]
async fn responding_to_item_frames_its_notice_as_a_paste_and_a_separate_enter() {
    let env = hooked_agent_daemon_env(AgentKind::Codex);
    let item_id = seed_blocked_question(&env);
    let supervisor_id = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Codex,
            "supervisor",
            "supervise",
            None,
            PermissionMode::Inherit,
            None,
            true,
            true,
            None,
        )
        .unwrap();
    let terminal = env.daemon.agent_terminal(supervisor_id).unwrap();
    let (replay, mut output) = env.daemon.mux.attach(terminal.id).unwrap();

    let routed = env
        .daemon
        .respond_to_item(
            bucket_of(&env),
            item_id,
            "Ship it.",
            RespondTarget::Session(supervisor_id),
        )
        .await
        .unwrap();
    assert_eq!(routed, Some(supervisor_id));

    // The scripted composer echoes a submitted line, and only a paste
    // whose Enter arrived separately submits.
    await_output(
        &mut output,
        replay.to_vec(),
        format!("User replied on item {item_id}. Read the item and continue.").as_bytes(),
    )
    .await;
}
