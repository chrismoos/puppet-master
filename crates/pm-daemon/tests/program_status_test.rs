//! Program Status (OSC 7501) on a local agent terminal, end to end: the
//! scripted agent probes and reports through a real PTY, and the daemon
//! answers, keeps the records and moves the session state.

mod support;

use std::time::Duration;

use base64::Engine;
use pm_adapters::{AdapterError, AdapterRegistry, AgentAdapter, CommandSpec, SpawnCtx, SpawnPlan};
use pm_daemon::daemon::{now_unix_ms, SETTING_SPAWN_PROGRAM_STATUS};
use pm_daemon::session_program_status::PROGRAM_STATUS_ALERT_DEBOUNCE_MS;
use pm_protocol::domain::{
    AgentKind, Event, HookKind, ProgramStatusKind, ProgramStatusState, Session, SessionAlertKind,
    SessionState,
};
use support::*;

/// The line-mode scripted agent, standing in for a hook-integrated agent so
/// hooks and reports can be mixed on one session.
struct HookedScriptedAgent;

impl AgentAdapter for HookedScriptedAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn has_lifecycle_hooks(&self) -> bool {
        true
    }

    fn spawn_command(&self, ctx: &SpawnCtx) -> Result<SpawnPlan, AdapterError> {
        Ok(SpawnPlan {
            spec: CommandSpec {
                program: testagent_bin().display().to_string(),
                args: vec![ctx.task_prompt.clone()],
                env: vec![],
                cwd: ctx.cwd.clone(),
            },
            agent_session_id: Some(format!("scripted-{}", ctx.integration.session_id)),
            detect_osc9_needs_input: false,
        })
    }
}

/// A daemon with the setting as given, applying Program Status changes as
/// the server would.
fn program_status_env(enabled: bool) -> TestEnv {
    let mut registry = AdapterRegistry::empty();
    registry.register(Box::new(HookedScriptedAgent));
    let mut env = daemon_env_with_registry(registry, None);
    if enabled {
        env.daemon
            .set_setting(SETTING_SPAWN_PROGRAM_STATUS, Some("true"))
            .unwrap();
    }
    let mut updates = env.program_status_rx.take().unwrap();
    let daemon = env.daemon.clone();
    tokio::spawn(async move {
        while let Some(update) = updates.recv().await {
            daemon.handle_program_status(update.terminal_id, update.generation, &update.changes);
        }
    });
    env
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn session(env: &TestEnv, id: u64) -> Session {
    env.daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|session| session.id == id)
        .unwrap()
}

async fn wait_for(env: &TestEnv, id: u64, what: &str, done: impl Fn(&Session) -> bool) -> Session {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    loop {
        let current = session(env, id);
        if done(&current) {
            return current;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}: {:?} {:?} {:?}",
            current.state,
            current.state_detail,
            current.program_status
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn state_becomes(env: &TestEnv, id: u64, state: SessionState, detail: &str) -> Session {
    wait_for(env, id, &format!("{state:?} {detail:?}"), |s| {
        s.state == state && s.state_detail == detail
    })
    .await
}

/// Waits for the root record itself, which a session already in the state
/// it maps to would not show.
async fn root_becomes(env: &TestEnv, id: u64, state: ProgramStatusState) -> Session {
    wait_for(env, id, &format!("a {state:?} root record"), |s| {
        s.program_status
            .first()
            .is_some_and(|root| root.id.is_empty() && root.state == state)
    })
    .await
}

struct Agent {
    terminal_id: u64,
    output: tokio::sync::broadcast::Receiver<bytes::Bytes>,
}

impl Agent {
    async fn start(env: &TestEnv) -> (u64, Agent) {
        let id = spawn_agent_session(env, AgentKind::ClaudeCode, "go");
        let (replay, mut output, guard) = env.daemon.attach(id).await.unwrap();
        std::mem::forget(guard);
        await_output(&mut output, replay.to_vec(), b"READY go").await;
        let terminal_id = env.daemon.agent_terminal(id).unwrap().id;
        (
            id,
            Agent {
                terminal_id,
                output,
            },
        )
    }

    fn send(&self, env: &TestEnv, line: &str) {
        env.daemon
            .terminal_input(self.terminal_id, bytes::Bytes::from(format!("{line}\n")));
    }

    async fn probe(&mut self, env: &TestEnv) -> String {
        self.send(env, "pstatus-probe");
        const ANSWER: &[u8] = b"OUT PSTATUS ";
        let output = await_output(&mut self.output, Vec::new(), ANSWER).await;
        let start = output
            .windows(ANSWER.len())
            .position(|window| window == ANSWER)
            .unwrap();
        let answer = await_output(&mut self.output, output[start..].to_vec(), b"\n").await;
        String::from_utf8_lossy(&answer)
            .lines()
            .next()
            .unwrap()
            .trim()
            .to_string()
    }

    fn report(&self, env: &TestEnv, body: &str) {
        self.send(env, &format!("pstatus {body}"));
    }
}

#[tokio::test]
async fn an_agent_probe_is_answered_and_its_reports_drive_the_session_state() {
    let env = program_status_env(true);
    let (id, mut agent) = Agent::start(&env).await;

    assert_eq!(
        agent.probe(&env).await,
        "OUT PSTATUS SUPPORTED 1b5d373530313b3f1b5c",
        "the child reads back exactly the query reply"
    );

    agent.report(
        &env,
        &format!(
            "state=working:app=claude-code:progress=40:msg={}",
            b64("Running tests")
        ),
    );
    state_becomes(&env, id, SessionState::Working, "Running tests (40%)").await;

    agent.report(
        &env,
        &format!(
            "state=working:id=task/1:title={}:msg={}",
            b64("Explore"),
            b64("Reading files")
        ),
    );
    let current = wait_for(&env, id, "a child record", |s| s.program_status.len() == 2).await;
    let child = &current.program_status[1];
    assert_eq!(child.id, "task/1");
    assert_eq!(child.title, "Explore");
    assert_eq!(child.msg, "Reading files");
    assert_eq!(child.app, "claude-code", "a child takes app from the root");
    assert_eq!(
        current.state,
        SessionState::Working,
        "a child never moves the session"
    );

    agent.report(
        &env,
        &format!(
            "state=blocked:kind=permission:app=claude-code:msg={}",
            b64("Allow Bash(cargo test)?")
        ),
    );
    let blocked = state_becomes(
        &env,
        id,
        SessionState::NeedsInput,
        "permission: Allow Bash(cargo test)?",
    )
    .await;
    assert_eq!(
        blocked.program_status[0].kind,
        Some(ProgramStatusKind::Permission)
    );

    agent.report(
        &env,
        &format!("state=done:app=claude-code:msg={}", b64("Tests pass")),
    );
    let done = state_becomes(&env, id, SessionState::Idle, "done: Tests pass").await;
    assert_eq!(done.program_status[0].state, ProgramStatusState::Done);

    agent.report(
        &env,
        &format!("state=error:app=claude-code:msg={}", b64("API error")),
    );
    state_becomes(&env, id, SessionState::Idle, "error: API error").await;

    agent.report(&env, "state=clear:id=task");
    wait_for(&env, id, "the child cleared", |s| {
        s.program_status.len() == 1
    })
    .await;
}

#[tokio::test]
async fn a_rejected_report_changes_nothing_and_a_bad_id_never_reaches_the_root() {
    let env = program_status_env(true);
    let (id, agent) = Agent::start(&env).await;
    agent.report(&env, "state=working:app=claude-code");
    root_becomes(&env, id, ProgramStatusState::Working).await;

    let control = b64("bad\u{1b}[2J");
    agent.report(&env, &format!("state=blocked:msg={control}"));
    agent.report(&env, "state=blocked:id=a//b");
    agent.report(&env, "state=paused");
    agent.report(&env, "state=done:id=marker");
    let current = wait_for(&env, id, "the marker record", |s| {
        s.program_status.iter().any(|r| r.id == "marker")
    })
    .await;
    assert_eq!(current.state, SessionState::Working);
    assert_eq!(current.program_status.len(), 2);
    assert_eq!(current.program_status[0].state, ProgramStatusState::Working);
}

#[tokio::test]
async fn with_the_setting_off_the_probe_goes_unanswered_and_reports_change_nothing() {
    let env = program_status_env(false);
    let (id, mut agent) = Agent::start(&env).await;
    assert_eq!(agent.probe(&env).await, "OUT PSTATUS UNSUPPORTED");
    let before = session(&env, id);
    agent.report(&env, "state=blocked:kind=question");
    agent.send(&env, "echo after");
    await_output(&mut agent.output, Vec::new(), b"OUT after").await;
    let after = session(&env, id);
    assert_eq!(
        (after.state, &after.state_detail),
        (before.state, &before.state_detail)
    );
    assert!(after.program_status.is_empty());
}

#[tokio::test]
async fn hooks_never_override_a_live_root_record_and_resume_once_it_is_gone() {
    let env = program_status_env(true);
    let (id, agent) = Agent::start(&env).await;
    hook(&env, id, HookKind::TurnEnded, "");
    state_becomes(&env, id, SessionState::Idle, "").await;

    agent.report(
        &env,
        &format!("state=blocked:kind=question:msg={}", b64("Which branch?")),
    );
    state_becomes(
        &env,
        id,
        SessionState::NeedsInput,
        "question: Which branch?",
    )
    .await;

    hook(&env, id, HookKind::PromptSubmitted, "");
    hook(
        &env,
        id,
        HookKind::NeedsInput,
        "Claude needs your permission",
    );
    hook(&env, id, HookKind::TurnEnded, "");
    assert_eq!(
        (session(&env, id).state, session(&env, id).state_detail),
        (
            SessionState::NeedsInput,
            "question: Which branch?".to_string()
        ),
        "a hook that disagrees with the live record is kept aside"
    );

    hook(&env, id, HookKind::PromptSubmitted, "");
    agent.report(&env, "state=clear");
    state_becomes(&env, id, SessionState::Working, "").await;
    assert!(session(&env, id).program_status.is_empty());

    hook(&env, id, HookKind::TurnEnded, "");
    assert_eq!(
        session(&env, id).state,
        SessionState::Idle,
        "hooks apply again"
    );
}

#[tokio::test]
async fn a_hook_turn_end_still_records_the_finished_turn_under_a_done_record() {
    let env = program_status_env(true);
    let (id, agent) = Agent::start(&env).await;
    agent.report(&env, "state=working");
    root_becomes(&env, id, ProgramStatusState::Working).await;
    hook(&env, id, HookKind::PromptSubmitted, "");
    agent.report(&env, &format!("state=done:msg={}", b64("Finished")));
    state_becomes(&env, id, SessionState::Idle, "done: Finished").await;
    hook(&env, id, HookKind::TurnEnded, "");
    assert!(
        session(&env, id).idle_unseen,
        "the hook's turn bookkeeping still marks the finished turn unseen"
    );
}

#[tokio::test]
async fn a_quiet_terminal_is_not_read_as_a_lost_turn_while_the_agent_reports_working() {
    let env = program_status_env(true);
    let (id, agent) = Agent::start(&env).await;
    agent.report(&env, "state=working");
    root_becomes(&env, id, ProgramStatusState::Working).await;
    let a_day_later = unix_ms() + 24 * 60 * 60 * 1000;
    env.daemon.process_stale_turns_at(a_day_later);
    env.daemon.process_stale_turns_at(a_day_later + 60_000);
    assert_eq!(session(&env, id).state, SessionState::Working);
    assert_eq!(session(&env, id).state_detail, "");
}

fn drain_alerts(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    id: u64,
) -> Vec<SessionAlertKind> {
    let mut alerts = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let Event::SessionAlert(alert) = event {
            if alert.session_id == id {
                alerts.push(alert.kind);
            }
        }
    }
    alerts
}

fn settle(env: &TestEnv) {
    env.daemon
        .flush_program_status_alerts_at(now_unix_ms() + PROGRAM_STATUS_ALERT_DEBOUNCE_MS);
}

#[tokio::test]
async fn alerts_wait_for_the_state_to_settle_and_judge_where_it_settled() {
    let env = program_status_env(true);
    let (_, mut events) = env.daemon.subscribe();
    let (id, agent) = Agent::start(&env).await;
    agent.report(&env, "state=working");
    root_becomes(&env, id, ProgramStatusState::Working).await;
    agent.report(&env, &format!("state=done:msg={}", b64("ok")));
    state_becomes(&env, id, SessionState::Idle, "done: ok").await;
    assert_eq!(
        drain_alerts(&mut events, id),
        vec![],
        "held until it settles"
    );
    settle(&env);
    assert_eq!(
        drain_alerts(&mut events, id),
        vec![SessionAlertKind::Completed]
    );

    for _ in 0..3 {
        agent.report(&env, "state=blocked:kind=permission");
        state_becomes(&env, id, SessionState::NeedsInput, "permission").await;
        agent.report(&env, "state=working");
        root_becomes(&env, id, ProgramStatusState::Working).await;
    }
    settle(&env);
    assert_eq!(
        drain_alerts(&mut events, id),
        vec![],
        "flips that settle on working alert for nothing"
    );

    agent.report(&env, "state=blocked:kind=permission");
    state_becomes(&env, id, SessionState::NeedsInput, "permission").await;
    settle(&env);
    assert_eq!(
        drain_alerts(&mut events, id),
        vec![SessionAlertKind::NeedsInput]
    );
}

#[tokio::test]
async fn a_held_alert_is_not_raised_before_the_debounce_elapses() {
    let env = program_status_env(true);
    let (_, mut events) = env.daemon.subscribe();
    let (id, agent) = Agent::start(&env).await;
    agent.report(&env, "state=working");
    root_becomes(&env, id, ProgramStatusState::Working).await;
    agent.report(&env, "state=blocked:kind=question");
    state_becomes(&env, id, SessionState::NeedsInput, "question").await;
    env.daemon.flush_program_status_alerts_at(now_unix_ms());
    assert_eq!(drain_alerts(&mut events, id), vec![]);
    settle(&env);
    assert_eq!(
        drain_alerts(&mut events, id),
        vec![SessionAlertKind::NeedsInput]
    );
}

#[tokio::test]
async fn the_agent_exiting_drops_its_working_record_and_keeps_done_ones() {
    let mut env = program_status_env(true);
    let (id, agent) = Agent::start(&env).await;
    agent.report(&env, "state=working");
    agent.report(&env, "state=done:id=task");
    wait_for(&env, id, "both records", |s| s.program_status.len() == 2).await;
    agent.send(&env, "exit 0");
    let exited = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exited);
    let exit = wait_for(&env, id, "the working root dropped", |s| {
        s.program_status.len() == 1
    })
    .await;
    assert_eq!(exit.program_status[0].id, "task");
    assert!(!exit.state.is_live());
}
