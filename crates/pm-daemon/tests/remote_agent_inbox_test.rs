//! Supervision notices for a session whose agent runs on an enrolled
//! worker. The addresses an agent's inbox is reached on only resolve on
//! the host running it, so the controller asks that host to deliver and
//! writes to the terminal only when it will not or cannot.

mod support;

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use pm_protocol::domain::{
    AgentInboxMode, AgentInboxOutcome, AgentKind, ControllerMsg, HookKind, PathCheck,
    PermissionMode, TerminalKind, WorkerMsg,
};
use pm_protocol::terminal_frame::{self, TerminalFrame};
use pm_protocol::WORKER_PROTOCOL_AGENT_INBOX;
use support::{
    bucket_of, daemon_env, end_supervisor_turn, hook, seed_item, spawn_test_session, unix_ms,
    TestEnv,
};
use tokio::sync::mpsc;

/// One request the controller asked a worker to hand to an agent.
#[derive(Debug, Clone)]
struct InboxRequest {
    session_id: u64,
    agent_terminal_id: u64,
    text: String,
    mode: AgentInboxMode,
}

/// A simulated worker that answers path checks and inbox deliveries and
/// records every inbox request it was sent.
struct FakeWorker {
    worker_id: u64,
    credential: String,
    link: Arc<pm_daemon::workers::WorkerLink>,
    inbox_requests: Arc<Mutex<Vec<InboxRequest>>>,
}

impl FakeWorker {
    fn inbox_requests(&self) -> Vec<InboxRequest> {
        self.inbox_requests.lock().unwrap().clone()
    }
}

/// The supervisor's own PTY, as a worker holding it would see writes to
/// it. Nothing streams a remote terminal until something attaches, so
/// without this a terminal write has nowhere to land.
struct AttachedTerminal {
    frames: mpsc::Receiver<Bytes>,
}

impl AttachedTerminal {
    /// The bytes written to the terminal since the last call, which is
    /// the evidence that a notice took the paste route.
    fn drain_input(&mut self) -> Vec<u8> {
        let mut written = Vec::new();
        while let Ok(frame) = self.frames.try_recv() {
            if let Some(TerminalFrame::Input { data, .. }) = terminal_frame::decode(&frame) {
                written.extend_from_slice(data);
            }
        }
        written
    }
}

fn enroll_worker(env: &TestEnv, protocol_version: u32, outcome: AgentInboxOutcome) -> FakeWorker {
    let (token, _expires) = env.daemon.create_worker_enrollment("remote").unwrap();
    let registration = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-remote",
            hostname: "remote",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version,
        })
        .unwrap();
    let worker = FakeWorker {
        worker_id: registration.worker_id,
        credential: registration.credential,
        link: registration.link,
        inbox_requests: Arc::new(Mutex::new(Vec::new())),
    };
    serve(
        env,
        worker.worker_id,
        registration.rx,
        &worker.inbox_requests,
        outcome,
    );
    worker
}

/// Answers the control messages a test is not about, and records and
/// answers the ones it is.
fn serve(
    env: &TestEnv,
    worker_id: u64,
    mut rx: mpsc::Receiver<ControllerMsg>,
    inbox_requests: &Arc<Mutex<Vec<InboxRequest>>>,
    outcome: AgentInboxOutcome,
) {
    let daemon = env.daemon.clone();
    let recorded = inbox_requests.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                ControllerMsg::PathCheck { req_id, .. } => {
                    daemon.apply_worker_message(
                        worker_id,
                        WorkerMsg::PathChecked {
                            req_id,
                            status: PathCheck::Ok,
                            detail: String::new(),
                        },
                    );
                }
                ControllerMsg::AgentInbox {
                    req_id,
                    session_id,
                    agent_terminal_id,
                    text,
                    mode,
                    ..
                } => {
                    recorded.lock().unwrap().push(InboxRequest {
                        session_id,
                        agent_terminal_id,
                        text,
                        mode,
                    });
                    let transport = match outcome {
                        AgentInboxOutcome::Delivered => "claude-socket".to_string(),
                        _ => String::new(),
                    };
                    daemon.apply_worker_message(
                        worker_id,
                        WorkerMsg::AgentInboxResult {
                            req_id,
                            outcome,
                            transport,
                            mode,
                            detail: String::new(),
                        },
                    );
                }
                _ => {}
            }
        }
    });
}

/// Points the project's bucket and the project itself at the worker, so
/// every session the test spawns lands there.
fn route_to(env: &TestEnv, worker_id: u64) {
    let snapshot = env.daemon.subscribe().0;
    let project = snapshot
        .projects
        .iter()
        .find(|p| p.id == env.project_id)
        .unwrap()
        .clone();
    env.daemon
        .set_bucket_workers(project.bucket_id, &[0, worker_id], worker_id, None)
        .unwrap();
    env.daemon
        .set_project_workers(env.project_id, &[worker_id], Some(worker_id))
        .unwrap();
}

fn spawn_remote_supervisor(env: &TestEnv, worker_id: u64) -> u64 {
    env.daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "supervisor",
            "supervise the board",
            None,
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            true,
            None,
        )
        .unwrap()
}

async fn spawn_remote_child(env: &TestEnv, supervisor: u64, key: &str) -> u64 {
    let item_id = seed_item(env, key);
    let bucket_id = bucket_of(env);
    let (session_id, _) = env
        .daemon
        .supervisor_spawn(
            supervisor,
            bucket_id,
            &serde_json::json!(env.project_id),
            Some(AgentKind::Test),
            "child",
            "do the work",
            &serde_json::json!(item_id),
            &serde_json::Value::Null,
        )
        .await
        .unwrap();
    session_id
}

fn attach_terminal(env: &TestEnv, worker: &FakeWorker, session_id: u64) -> AttachedTerminal {
    let terminal = env
        .daemon
        .subscribe()
        .0
        .terminals
        .into_iter()
        .find(|t| t.session_id == session_id && t.kind == TerminalKind::Agent)
        .expect("the session has an agent terminal");
    let (tx, frames) = mpsc::channel(64);
    worker
        .link
        .connect_terminal_stream(terminal.id, terminal.generation, tx);
    AttachedTerminal { frames }
}

/// Leaves unsubmitted text in the supervisor's composer, which is what
/// a paste whose Enter never landed leaves behind. The typed bytes have
/// to reach the terminal for the daemon to record the line as pending,
/// so the attached stream seeing them is the precondition, not a
/// decoration.
fn leave_unsubmitted_text(env: &TestEnv, supervisor: u64, terminal: &mut AttachedTerminal) {
    terminal.drain_input();
    env.daemon
        .pty_input(supervisor, Bytes::from_static(b"half written"));
    assert_eq!(
        terminal.drain_input(),
        b"half written",
        "the composer was not actually dirtied"
    );
}

/// A supervisor whose composer holds an unsubmitted line is exactly the
/// state a failed paste leaves behind, and it is what refuses every
/// terminal notice from then on. The inbox appends to nothing, so the
/// notice must still arrive.
#[tokio::test]
/// The worker resolves an inbox from the terminal the controller names,
/// and its mux is keyed by terminal there exactly as it is here. Naming
/// the session instead finds whichever terminal carries that number,
/// which on a host running more than one session is another agent.
async fn an_inbox_request_names_the_sessions_own_agent_terminal() {
    let env = daemon_env();
    // A local session with a shell of its own pushes the terminal ids
    // past the session ids, which is the only reason the two are
    // distinguishable here.
    let local = spawn_test_session(&env, "local");
    env.daemon.create_shell(local, "shell").unwrap();

    let worker = enroll_worker(
        &env,
        pm_protocol::WORKER_PROTOCOL_VERSION,
        AgentInboxOutcome::Delivered,
    );
    route_to(&env, worker.worker_id);
    let supervisor = spawn_remote_supervisor(&env, worker.worker_id);
    let agent_terminal = env.daemon.agent_terminal(supervisor).unwrap().id;
    assert_ne!(
        agent_terminal, supervisor,
        "the ids have to diverge or this asserts nothing"
    );

    let child = spawn_remote_child(&env, supervisor, "wake:terminal-id").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");

    let wakes = env.daemon.process_supervisor_wakes_at(unix_ms()).await;
    assert_eq!(wakes.len(), 1, "the supervisor was woken once");
    let requests = worker.inbox_requests();
    assert_eq!(requests.len(), 1, "the worker was asked once");
    assert_eq!(
        requests[0].agent_terminal_id, agent_terminal,
        "the request must name the supervisor's own agent terminal"
    );
}

#[tokio::test]
async fn a_dirty_composer_does_not_stop_a_notice_reaching_a_remote_agents_inbox() {
    let env = daemon_env();
    let worker = enroll_worker(
        &env,
        pm_protocol::WORKER_PROTOCOL_VERSION,
        AgentInboxOutcome::Delivered,
    );
    route_to(&env, worker.worker_id);

    let supervisor = spawn_remote_supervisor(&env, worker.worker_id);
    let mut terminal = attach_terminal(&env, &worker, supervisor);
    let child = spawn_remote_child(&env, supervisor, "wake:dirty-composer").await;
    end_supervisor_turn(&env, supervisor);
    leave_unsubmitted_text(&env, supervisor, &mut terminal);
    hook(&env, child, HookKind::TurnEnded, "");

    let wakes = env.daemon.process_supervisor_wakes_at(unix_ms()).await;
    assert_eq!(
        wakes.len(),
        1,
        "a notice that is not typed must not wait on a half-written line"
    );
    assert!(wakes[0].sessions.contains(&child));

    let requests = worker.inbox_requests();
    assert_eq!(requests.len(), 1, "the worker was asked once");
    assert_eq!(requests[0].session_id, supervisor);
    assert_eq!(
        requests[0].agent_terminal_id,
        env.daemon.agent_terminal(supervisor).unwrap().id,
        "the worker resolves the inbox from the terminal it is given"
    );
    assert_eq!(requests[0].mode, AgentInboxMode::Queue);
    assert!(
        requests[0].text.contains(&child.to_string()),
        "the notice names the child that transitioned"
    );
    assert!(
        terminal.drain_input().is_empty(),
        "nothing was appended to the half-written line"
    );

    // Acknowledged by the agent's own channel, so there is no paste to
    // confirm and nothing to retry.
    assert!(
        env.daemon
            .process_supervisor_wakes_at(unix_ms() + 60_000)
            .await
            .is_empty(),
        "an acknowledged notice must not be delivered twice"
    );
}

/// The idle-nudge ladder is refused by the same flag, and is the half of
/// the deadlock that has no bound: without the inbox a dirty composer
/// silences it until a human submits.
#[tokio::test]
async fn a_dirty_composer_does_not_stop_an_idle_nudge_reaching_a_remote_agents_inbox() {
    let env = daemon_env();
    let worker = enroll_worker(
        &env,
        pm_protocol::WORKER_PROTOCOL_VERSION,
        AgentInboxOutcome::Delivered,
    );
    route_to(&env, worker.worker_id);

    let supervisor = spawn_remote_supervisor(&env, worker.worker_id);
    let mut terminal = attach_terminal(&env, &worker, supervisor);
    let child = spawn_remote_child(&env, supervisor, "nudge:dirty-composer").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");

    // Take the transition notice out of the way, so what is left is the
    // ladder that keeps reminding a supervisor which never came back.
    let now = unix_ms();
    env.daemon.process_supervisor_wakes_at(now).await;
    leave_unsubmitted_text(&env, supervisor, &mut terminal);

    let later = now + 40 * 60 * 1000;
    let wakes = env.daemon.process_supervisor_wakes_at(later).await;
    assert_eq!(
        wakes.len(),
        1,
        "an idle reminder to the agent's own inbox must not be refused by a half-written line"
    );

    let requests = worker.inbox_requests();
    assert_eq!(
        requests.len(),
        2,
        "the transition notice, then the reminder"
    );
    assert_eq!(requests[1].session_id, supervisor);
    assert!(terminal.drain_input().is_empty());
}

/// A worker that does not announce the capability cannot be asked: it
/// would drop a message it cannot decode and answer nothing. Its
/// sessions keep the terminal write they have always had.
#[tokio::test]
async fn a_worker_below_the_capability_version_keeps_the_terminal_write() {
    let env = daemon_env();
    let worker = enroll_worker(
        &env,
        WORKER_PROTOCOL_AGENT_INBOX - 1,
        AgentInboxOutcome::Delivered,
    );
    route_to(&env, worker.worker_id);

    let supervisor = spawn_remote_supervisor(&env, worker.worker_id);
    let mut terminal = attach_terminal(&env, &worker, supervisor);
    let child = spawn_remote_child(&env, supervisor, "wake:old-worker").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");
    terminal.drain_input();

    let wakes = env.daemon.process_supervisor_wakes_at(unix_ms()).await;
    assert_eq!(wakes.len(), 1, "the notice still goes out, by paste");
    assert!(
        worker.inbox_requests().is_empty(),
        "an older worker must never be sent a capability it does not announce"
    );
    let written = String::from_utf8_lossy(&terminal.drain_input()).to_string();
    assert!(
        written.contains(&child.to_string()),
        "the notice was written to the supervisor's terminal instead: {written:?}"
    );
}

/// And the flag that a paste sets still refuses the notice on such a
/// worker, exactly as it does today. Paired with the delivered case
/// above, this is what shows the composer is dirty in both: the same
/// state, refused by an older worker and carried by a current one.
#[tokio::test]
async fn a_worker_below_the_capability_version_still_refuses_on_a_dirty_composer() {
    let env = daemon_env();
    let worker = enroll_worker(
        &env,
        WORKER_PROTOCOL_AGENT_INBOX - 1,
        AgentInboxOutcome::Delivered,
    );
    route_to(&env, worker.worker_id);

    let supervisor = spawn_remote_supervisor(&env, worker.worker_id);
    let mut terminal = attach_terminal(&env, &worker, supervisor);
    let child = spawn_remote_child(&env, supervisor, "wake:old-worker-dirty").await;
    end_supervisor_turn(&env, supervisor);
    leave_unsubmitted_text(&env, supervisor, &mut terminal);
    hook(&env, child, HookKind::TurnEnded, "");

    assert!(
        env.daemon
            .process_supervisor_wakes_at(unix_ms())
            .await
            .is_empty(),
        "an older worker's behaviour is unchanged, including the refusal"
    );
    assert!(worker.inbox_requests().is_empty());
    assert!(
        terminal.drain_input().is_empty(),
        "and nothing was appended to the half-written line"
    );
}

/// A worker that has the capability but cannot address the agent yet
/// answers so, and the controller owes the session a terminal write
/// under the guard the inbox did not need.
#[tokio::test]
async fn a_worker_without_a_channel_falls_back_to_the_terminal() {
    let env = daemon_env();
    let worker = enroll_worker(
        &env,
        pm_protocol::WORKER_PROTOCOL_VERSION,
        AgentInboxOutcome::NoChannel,
    );
    route_to(&env, worker.worker_id);

    let supervisor = spawn_remote_supervisor(&env, worker.worker_id);
    let mut terminal = attach_terminal(&env, &worker, supervisor);
    let child = spawn_remote_child(&env, supervisor, "wake:no-channel").await;
    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");
    terminal.drain_input();

    let wakes = env.daemon.process_supervisor_wakes_at(unix_ms()).await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(worker.inbox_requests().len(), 1, "the worker was asked");
    let written = String::from_utf8_lossy(&terminal.drain_input()).to_string();
    assert!(
        written.contains(&child.to_string()),
        "the terminal carried the notice the worker would not: {written:?}"
    );
}

/// A worker that reconnects announcing an older protocol must stop
/// being sent the capability, not keep what the connection before it
/// announced.
#[tokio::test]
async fn a_worker_that_reconnects_older_stops_being_sent_the_capability() {
    let env = daemon_env();
    let worker = enroll_worker(
        &env,
        pm_protocol::WORKER_PROTOCOL_VERSION,
        AgentInboxOutcome::Delivered,
    );
    route_to(&env, worker.worker_id);
    let supervisor = spawn_remote_supervisor(&env, worker.worker_id);
    let child = spawn_remote_child(&env, supervisor, "wake:downgrade").await;

    let downgraded = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: "",
            credential: &worker.credential,
            peer_key_hash: "key-remote",
            hostname: "remote",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[supervisor, child],
            live_terminals: &[],
            protocol_version: WORKER_PROTOCOL_AGENT_INBOX - 1,
        })
        .unwrap();
    let mut rx = downgraded.rx;

    end_supervisor_turn(&env, supervisor);
    hook(&env, child, HookKind::TurnEnded, "");
    env.daemon.process_supervisor_wakes_at(unix_ms()).await;

    let mut seen = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        seen.push(msg);
    }
    assert!(
        !seen
            .iter()
            .any(|msg| matches!(msg, ControllerMsg::AgentInbox { .. })),
        "a reconnect that announces less must be gated on what it announced"
    );
    assert!(
        worker.inbox_requests().is_empty(),
        "and nothing reached the superseded connection either"
    );
}
