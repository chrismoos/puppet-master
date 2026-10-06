//! Resumes a Supervisor whose own turn has ended while a session it
//! spawned still needs supervision.
//!
//! `wait_sessions` only runs inside a turn, so supervision is otherwise
//! purely pull-based: once a Supervisor stops waiting, nothing reaches
//! it again and its children park indefinitely. This delivers a bounded
//! notice to the Supervisor's own agent terminal instead. It never
//! starts new work, never touches a session the Supervisor did not
//! spawn, and never changes a session's semantic state.

use std::collections::HashMap;
use std::sync::Mutex;

use pm_protocol::domain::{Session, SessionRole, SessionState};
use tracing::{info, warn};

use crate::daemon::{now_unix_ms, Daemon, SUPERVISOR_INPUT_MAX};

pub(crate) const DEFAULT_SNOOZE_MINUTES: u64 = 5;
pub(crate) const MIN_SNOOZE_MINUTES: u64 = 2;
pub(crate) const MAX_SNOOZE_MINUTES: u64 = 60;
pub(crate) const MINUTE_MS: i64 = 60_000;

/// Shortest gap between two wakes of the same Supervisor. Bursts of
/// child transitions coalesce into the next notice instead of starting
/// a turn each; the reconciliation pass re-runs, so nothing is lost.
const WAKE_MIN_INTERVAL_MS: i64 = 5_000;

/// Automated input waits until both sides of the PTY have been quiet. This is
/// in addition to refusing delivery while the user has an unsubmitted line.
const WAKE_TERMINAL_IDLE_MS: i64 = 5_000;

/// Sessions named in one notice before it summarizes the rest.
const WAKE_NAMED_SESSIONS: usize = 8;

/// How long a Supervisor's own terminal stays silent before it is
/// reminded that sessions it spawned are still live. Transition notices
/// only fire when a child changes state, so a Supervisor that stops
/// supervising while its children keep working is otherwise never told.
const IDLE_NUDGE_AFTER_MS: i64 = 2 * 60 * 1000;

/// Gaps between successive reminders to the same Supervisor. Running off
/// the end escalates to the user instead of reminding forever: a
/// Supervisor that ignored these is not going to answer another one.
const IDLE_NUDGE_BACKOFF_MS: [i64; 3] = [2 * 60 * 1000, 5 * 60 * 1000, 15 * 60 * 1000];

/// How recently a human must have typed for a reminder to stand down.
/// Kept minimal: the `user_input_pending` check already prevents
/// injecting into a half-composed line, which is the real safety
/// concern. A longer grace allowed a supervisor's conversational
/// activity to suppress escalation indefinitely.
const IDLE_NUDGE_USER_GRACE_MS: i64 = 0;

/// How long a delivered notice has to start the Supervisor's turn before
/// it is treated as lost. Longer than one reconciliation interval so a
/// notice is never judged before the pass that could observe it ran.
const NOTICE_CONFIRM_MS: i64 = 20 * 1000;

/// Deliveries of the same notice before the transition is written off. A
/// paste that never becomes a turn means the pane is not accepting input,
/// which more attempts will not change; the reminder ladder escalates
/// from there.
const MAX_NOTICE_ATTEMPTS: u32 = 3;

/// A child in one of these states cannot progress without its
/// supervisor: it needs an answer, an integration, a cleanup, or its
/// worker disconnected and it is waiting for reconnection.
fn needs_supervision(state: SessionState) -> bool {
    matches!(
        state,
        SessionState::NeedsInput
            | SessionState::Idle
            | SessionState::Failed
            | SessionState::Exited
            | SessionState::AwaitingWorker
    )
}

/// Whether a supervisor's turn has ended, leaving its children unwatched.
/// Sessions in Idle or NeedsInput have finished their turn, as do sessions
/// that remain in Working solely because background tasks are active.
fn supervisor_turn_ended(state: SessionState, state_detail: &str) -> bool {
    matches!(state, SessionState::Idle | SessionState::NeedsInput)
        || (state == SessionState::Working && state_detail == crate::daemon::BACKGROUND_WORK_DETAIL)
}

/// Identifies the exact transition a notice was delivered for. The
/// state revision is what separates a child re-entering `Idle` from the
/// `Idle` already announced, so a parked child never wakes twice.
/// `blocked` identifies the newest flag_blocked question, and is what
/// makes a question asked by an already-parked child a new fact. Without
/// it the key is unchanged, the notice is treated as already announced,
/// and the supervisor is never told the child asked anything.
fn wake_key(
    generation: u64,
    state: SessionState,
    state_revision: u64,
    blocked: Option<i64>,
) -> String {
    let blocked = blocked.unwrap_or_default();
    format!("{generation}:{}:{state_revision}:{blocked}", state.as_str())
}

/// A notice that was written to a Supervisor's terminal but has not yet
/// been seen to start its turn. Announcing the transitions before that
/// happens would consume them: a paste whose Enter never landed would
/// silence the child forever.
#[derive(Debug, Clone)]
struct PendingNotice {
    delivered_at_unix_ms: i64,
    /// The Supervisor's state revision when the notice went out. A turn
    /// started by the notice advances it, which is monotonic evidence and
    /// does not depend on comparing clocks.
    state_revision: u64,
    attempts: u32,
    /// child session -> the wake key to announce once the notice is
    /// confirmed to have started a turn.
    announce: Vec<(u64, String)>,
}

/// What is outstanding after settling a Supervisor's last notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeState {
    /// Nothing outstanding: confirmed, abandoned, or never sent.
    Settled,
    /// Delivered too recently to judge. Its transitions stay unannounced.
    Fresh,
    /// Delivered, never started a turn, and eligible to go again. Carries
    /// the deliveries already made.
    RetryDue(u32),
}

/// Where a Supervisor sits on the reminder ladder.
#[derive(Debug, Clone, Copy, Default)]
struct IdleNudge {
    /// Reminders delivered since it last supervised anything.
    level: usize,
    last_unix_ms: i64,
    /// Set once the ladder ran out and the user was told, so the
    /// escalation happens once rather than on every pass.
    escalated: bool,
}

/// What one delivered notice covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorWake {
    pub supervisor_id: u64,
    pub sessions: Vec<u64>,
}

/// The reminder for a Supervisor that went quiet with live children.
fn idle_notice(live: &[(u64, SessionState, &str)], quiet_for_ms: i64) -> String {
    let named = live
        .iter()
        .take(WAKE_NAMED_SESSIONS)
        .map(|(id, state, detail)| {
            if detail.is_empty() {
                format!("session {id} ({})", state.as_str())
            } else {
                format!("session {id} ({}, {})", state.as_str(), detail)
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let remaining = live.len().saturating_sub(WAKE_NAMED_SESSIONS);
    let rest = if remaining > 0 {
        format!("; and {remaining} more")
    } else {
        String::new()
    };
    format!(
        "[puppet-master] Supervision notice: you have not supervised anything for {} minutes \
         and sessions you spawned are still live: {named}{rest}. Call wait_sessions on your \
         live children (with your retained cursor, or without one for a fresh baseline), check \
         what they are doing, and continue supervising until the work is genuinely handled. \
         If they are progressing and you intentionally end this turn to wait, call \
         snooze_supervision with minutes (2-60, default 5) to delay idle reminders and \
         silence this turn's clean completion alert. Child transitions still wake you.",
        quiet_for_ms / 60_000
    )
}

/// In-memory wake state: what has been announced, when each supervisor
/// was last woken, which supervisors are already waiting, and the
/// reconciliation wakeup.
pub(crate) struct SupervisorWakeRuntime {
    /// child session -> the transition its supervisor was last told about.
    announced: Mutex<HashMap<u64, String>>,
    last_wake_unix_ms: Mutex<HashMap<u64, i64>>,
    /// Supervisors inside `wait_sessions`, which already delivers
    /// transitions on the cursor channel.
    waiting: Mutex<HashMap<u64, usize>>,
    /// supervisor -> how many reminders it has had without supervising
    /// anything since, and when the last one went out.
    idle_nudges: Mutex<HashMap<u64, IdleNudge>>,
    /// supervisor -> a notice written to its terminal whose turn has not
    /// been observed yet.
    pending_notices: Mutex<HashMap<u64, PendingNotice>>,
    /// supervisor -> why supervision last went quiet for it. Reconciliation
    /// runs on a timer, so a reason is logged when it changes rather than
    /// on every pass.
    quiet_reason: Mutex<HashMap<u64, &'static str>>,
    wakeup: tokio::sync::Notify,
}

impl Default for SupervisorWakeRuntime {
    fn default() -> Self {
        Self {
            announced: Mutex::new(HashMap::new()),
            last_wake_unix_ms: Mutex::new(HashMap::new()),
            waiting: Mutex::new(HashMap::new()),
            idle_nudges: Mutex::new(HashMap::new()),
            pending_notices: Mutex::new(HashMap::new()),
            quiet_reason: Mutex::new(HashMap::new()),
            wakeup: tokio::sync::Notify::new(),
        }
    }
}

impl SupervisorWakeRuntime {
    fn is_waiting(&self, supervisor_id: u64) -> bool {
        self.waiting
            .lock()
            .unwrap()
            .get(&supervisor_id)
            .is_some_and(|waits| *waits > 0)
    }

    fn announced_already(&self, session_id: u64, key: &str) -> bool {
        self.announced
            .lock()
            .unwrap()
            .get(&session_id)
            .is_some_and(|announced| announced == key)
    }

    fn record_announced(&self, session_id: u64, key: String) {
        self.announced.lock().unwrap().insert(session_id, key);
    }

    /// Records why a supervisor is not being woken, returning true the
    /// first time a given reason applies. Without this the reconciliation
    /// timer would repeat the same line every few seconds; with it the log
    /// carries a timeline of when supervision went quiet and why.
    fn note_quiet(&self, supervisor_id: u64, reason: &'static str) -> bool {
        let mut reasons = self.quiet_reason.lock().unwrap();
        if reasons.get(&supervisor_id) == Some(&reason) {
            return false;
        }
        reasons.insert(supervisor_id, reason);
        true
    }

    fn clear_quiet(&self, supervisor_id: u64) {
        self.quiet_reason.lock().unwrap().remove(&supervisor_id);
    }

    pub(crate) fn forget_session(&self, session_id: u64) {
        self.announced.lock().unwrap().remove(&session_id);
        self.quiet_reason.lock().unwrap().remove(&session_id);
        self.last_wake_unix_ms.lock().unwrap().remove(&session_id);
        self.idle_nudges.lock().unwrap().remove(&session_id);
        self.pending_notices.lock().unwrap().remove(&session_id);
    }

    fn throttle_allows(&self, supervisor_id: u64, now: i64) -> bool {
        self.last_wake_unix_ms
            .lock()
            .unwrap()
            .get(&supervisor_id)
            .is_none_or(|last| now.saturating_sub(*last) >= WAKE_MIN_INTERVAL_MS)
    }

    /// Whether a reminder is due, and which rung it would be. Running
    /// past the last rung reports the escalation instead.
    fn idle_nudge_due(&self, supervisor_id: u64, now: i64) -> Option<IdleNudge> {
        let nudges = self.idle_nudges.lock().unwrap();
        let state = nudges.get(&supervisor_id).copied().unwrap_or_default();
        if state.escalated {
            return None;
        }
        let wait = IDLE_NUDGE_BACKOFF_MS
            .get(state.level)
            .copied()
            .unwrap_or(*IDLE_NUDGE_BACKOFF_MS.last().unwrap());
        if state.last_unix_ms != 0 && now.saturating_sub(state.last_unix_ms) < wait {
            return None;
        }
        Some(state)
    }

    fn record_idle_nudge(&self, supervisor_id: u64, now: i64, escalated: bool) {
        let mut nudges = self.idle_nudges.lock().unwrap();
        let state = nudges.entry(supervisor_id).or_default();
        state.level += 1;
        state.last_unix_ms = now;
        state.escalated = escalated;
    }

    /// Clears the ladder because the Supervisor supervised something.
    /// Terminal output alone does not count: a Supervisor that wakes,
    /// says something and parks again has not done what it was reminded
    /// to do, and would otherwise reset itself forever.
    pub(crate) fn note_supervision_activity(&self, supervisor_id: u64) {
        self.idle_nudges.lock().unwrap().remove(&supervisor_id);
    }

    fn record_pending_notice(&self, supervisor_id: u64, notice: PendingNotice) {
        self.pending_notices
            .lock()
            .unwrap()
            .insert(supervisor_id, notice);
    }

    fn take_pending_notice(&self, supervisor_id: u64) -> Option<PendingNotice> {
        self.pending_notices.lock().unwrap().remove(&supervisor_id)
    }

    fn record_wake(&self, supervisor_id: u64, now: i64) {
        self.last_wake_unix_ms
            .lock()
            .unwrap()
            .insert(supervisor_id, now);
    }
}

/// Marks a Supervisor as waiting for as long as its `wait_sessions`
/// call is in flight, including when the call is dropped.
pub(crate) struct SupervisorWaitGuard<'a> {
    runtime: &'a SupervisorWakeRuntime,
    supervisor_id: u64,
}

impl Drop for SupervisorWaitGuard<'_> {
    fn drop(&mut self) {
        let mut waiting = self.runtime.waiting.lock().unwrap();
        if let Some(waits) = waiting.get_mut(&self.supervisor_id) {
            *waits = waits.saturating_sub(1);
            if *waits == 0 {
                waiting.remove(&self.supervisor_id);
            }
        }
    }
}

/// The notice itself. It carries session ids and states only: no
/// terminal output, prompt text, item body, or headline.
fn wake_notice(supervisor_state: SessionState, pending: &[(u64, SessionState)]) -> String {
    let named = pending
        .iter()
        .take(WAKE_NAMED_SESSIONS)
        .map(|(id, state)| format!("session {id} is {}", state.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    let remaining = pending.len().saturating_sub(WAKE_NAMED_SESSIONS);
    let rest = if remaining > 0 {
        format!(", and {remaining} more")
    } else {
        String::new()
    };
    let mut notice = format!(
        "[puppet-master] Supervision notice: {named}{rest}. Your turn ended with these sessions \
         you spawned unattended and no wait_sessions call outstanding, so nothing was watching \
         them. Call wait_sessions on your live children (with your retained cursor, or without \
         one for a fresh baseline), triage each transition, and continue supervising until the \
         work is genuinely handled. If your remaining children are progressing and you \
         intentionally end this turn to wait, call snooze_supervision with minutes \
         (2-60, default 5) to delay idle reminders and silence this turn's clean completion \
         alert. Child transitions still wake you."
    );
    if supervisor_state == SessionState::NeedsInput {
        notice.push_str(
            " You are flagged needs-input to the user, and taking this turn clears that flag the \
             same way any delivered input does. If the user's answer is still outstanding, call \
             flag_blocked again with that question before this turn ends.",
        );
    }
    notice
}

impl Daemon {
    pub(crate) fn supervisor_wake(&self) -> &SupervisorWakeRuntime {
        &self.supervisor_wake_runtime
    }

    pub(crate) fn supervisor_wake_wakeup(&self) -> &tokio::sync::Notify {
        &self.supervisor_wake().wakeup
    }

    /// Requests a reconciliation pass after a lifecycle change.
    /// Clears a Supervisor's reminder ladder because it supervised
    /// something.
    pub fn note_supervision_activity(&self, supervisor_id: u64) {
        self.supervisor_wake()
            .note_supervision_activity(supervisor_id);
    }

    pub(crate) fn note_supervisor_wake_candidate(&self) {
        self.supervisor_wake().wakeup.notify_one();
    }

    pub(crate) fn supervisor_wait_guard(&self, supervisor_id: u64) -> SupervisorWaitGuard<'_> {
        let runtime = self.supervisor_wake();
        *runtime
            .waiting
            .lock()
            .unwrap()
            .entry(supervisor_id)
            .or_insert(0) += 1;
        SupervisorWaitGuard {
            runtime,
            supervisor_id,
        }
    }

    /// Seeds what a restart may treat as already announced, from the mark
    /// persisted when the notice was confirmed. A transition whose mark
    /// matches was already delivered and must not be replayed; a child
    /// that parked with no mark was never announced to anyone, and a
    /// restart is precisely when nobody is watching it, so it stays
    /// eligible. The per-supervisor throttle collapses a crowd of them
    /// into one notice.
    pub(crate) fn seed_supervisor_wake_marks(&self) {
        let Ok(children) = self.storage().spawned_sessions() else {
            return;
        };
        for child in children {
            let Some(key) = self.child_wake_key(&child) else {
                continue;
            };
            let announced = self
                .storage()
                .announced_wake_key(child.id)
                .ok()
                .flatten()
                .is_some_and(|persisted| persisted == key);
            // A child that is not parked has nothing outstanding either
            // way, so it never needs to survive as eligible.
            if announced || !needs_supervision(child.state) {
                self.supervisor_wake().record_announced(child.id, key);
            }
        }
    }

    /// Marks a child transition announced, in memory and on disk, so a
    /// restart can tell it apart from one nobody ever heard about.
    /// Marks a child's current transition as already delivered to its
    /// supervisor, so the reconciliation pass does not announce it a
    /// second time. Used when a child reached its supervisor by another
    /// route, which is what a blocked worker's question is.
    pub(crate) fn suppress_wake_for_current_state(&self, session_id: u64) {
        let Ok(child) = self.storage().get_session(session_id) else {
            return;
        };
        if let Some(key) = self.child_wake_key(&child) {
            self.announce_child(session_id, key);
        }
    }

    fn announce_child(&self, session_id: u64, key: String) {
        if let Err(error) = self.storage().set_announced_wake_key(session_id, &key) {
            warn!(session = session_id, %error, "failed to persist a wake announcement");
        }
        self.supervisor_wake().record_announced(session_id, key);
    }

    fn child_wake_key(&self, child: &Session) -> Option<String> {
        let generation = self
            .storage()
            .agent_terminal(child.id)
            .map(|terminal| terminal.generation)
            .unwrap_or_default();
        let (state_revision, _) = self.storage().session_transition_marks(child.id).ok()?;
        let blocked = self
            .storage()
            .latest_blocked_report(child.id)
            .ok()
            .flatten();
        Some(wake_key(generation, child.state, state_revision, blocked))
    }

    /// Reconciles every Supervisor against the children it spawned and
    /// wakes the ones that stopped supervising. Idempotent: a transition
    /// already announced never wakes anyone again.
    pub async fn process_supervisor_wakes(&self) -> Vec<SupervisorWake> {
        self.process_supervisor_wakes_at(now_unix_ms()).await
    }

    /// Time-parameterized reconciliation keeps the idle boundary exact and
    /// lets integration tests advance it without sleeping for real time.
    #[doc(hidden)]
    pub async fn process_supervisor_wakes_at(&self, now: i64) -> Vec<SupervisorWake> {
        let children = match self.storage().spawned_sessions() {
            Ok(children) => children,
            Err(error) => {
                warn!(%error, "failed to load supervised sessions");
                return Vec::new();
            }
        };
        // Apply the AwaitingWorker overlay so children on disconnected
        // workers are visible as needing supervision. Without this, the
        // database state stays at Working and no notice fires.
        let mut children = children;
        self.overlay_awaiting_worker_sessions(&mut children);
        let mut by_supervisor: HashMap<u64, Vec<Session>> = HashMap::new();
        for child in children {
            if let Some(supervisor_id) = child.spawned_by_session_id {
                by_supervisor.entry(supervisor_id).or_default().push(child);
            }
        }
        // Serialized rather than joined: each supervisor's notice is a
        // delivery with its own bookkeeping, and a burst of them racing
        // to write into the same terminals is exactly what the notice
        // machinery is here to avoid.
        let mut wakes = Vec::new();
        for (supervisor_id, children) in by_supervisor {
            let wake = match self.wake_supervisor(supervisor_id, &children, now).await {
                Some(wake) => Some(wake),
                None => {
                    self.nudge_idle_supervisor(supervisor_id, &children, now)
                        .await
                }
            };
            wakes.extend(wake);
        }
        wakes
    }

    /// Reminds a Supervisor that went quiet while sessions it spawned are
    /// still live. Transition notices only fire when a child changes
    /// state, so a Supervisor that simply stopped supervising is
    /// otherwise never told anything.
    async fn nudge_idle_supervisor(
        &self,
        supervisor_id: u64,
        children: &[Session],
        now: i64,
    ) -> Option<SupervisorWake> {
        let supervisor = self.storage().get_session(supervisor_id).ok()?;
        if supervisor.role != SessionRole::Supervisor || !supervisor.supervisor_api {
            return None;
        }
        // Anything else means it is inside a turn, or has ended.
        if !supervisor_turn_ended(supervisor.state, &supervisor.state_detail) {
            return None;
        }
        let generation = self
            .storage()
            .agent_terminal(supervisor_id)
            .ok()?
            .generation;
        if self
            .storage()
            .supervision_snoozed_until(supervisor_id, generation)
            .ok()?
            > now
        {
            return None;
        }
        // A Supervisor inside wait_sessions is supervising by definition.
        if self.supervisor_wake().is_waiting(supervisor_id) {
            return None;
        }

        // A child that has exited is nothing left to answer for. Every
        // other state is: idle and failed children still need integrating
        // or cleaning up.
        let live: Vec<(u64, SessionState, String)> = children
            .iter()
            .filter(|child| child.state != SessionState::Exited)
            .map(|child| (child.id, child.state, child.state_detail.clone()))
            .collect();
        if live.is_empty() {
            return None;
        }
        let live_ids: Vec<u64> = live.iter().map(|(id, _, _)| *id).collect();

        let (last_agent, last_user, user_input_pending) = self.session_activity_clocks(&supervisor);
        // A Supervisor that has produced nothing yet is not "quiet since
        // the epoch": measure from when it started instead, so a fresh one
        // gets its first period before being reminded of anything.
        let quiet_since = if last_agent != 0 {
            last_agent
        } else {
            supervisor.created_at_unix_ms
        };
        let quiet_for = now.saturating_sub(quiet_since);
        if quiet_for < IDLE_NUDGE_AFTER_MS {
            return None;
        }
        // A human at the keyboard owns the session; an unsubmitted line
        // must never have text appended to it. A reminder that goes to
        // the agent's own inbox appends to nothing, so it is exempt.
        let via_inbox = self.inbox_delivery_possible(supervisor_id);
        if !via_inbox
            && (user_input_pending
                || (last_user != 0 && now.saturating_sub(last_user) < IDLE_NUDGE_USER_GRACE_MS))
        {
            return None;
        }

        let state = self.supervisor_wake().idle_nudge_due(supervisor_id, now)?;
        let exhausted = state.level >= IDLE_NUDGE_BACKOFF_MS.len();
        if exhausted {
            let mut question = format!(
                "Supervisor session {supervisor_id} has not supervised sessions {} for a while \
                 and reminders have not restarted it. They may need you directly.",
                live_ids
                    .iter()
                    .take(WAKE_NAMED_SESSIONS)
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            // Surface each child's actual question so the user can act
            // without having to look up what the child wanted.
            for (id, child_state, detail) in live.iter().take(WAKE_NAMED_SESSIONS) {
                if !detail.is_empty() {
                    question.push_str(&format!(
                        "\n  session {} ({}): {}",
                        id,
                        child_state.as_str(),
                        detail,
                    ));
                }
            }
            if self.apply_blocked(supervisor_id, question).is_ok() {
                self.supervisor_wake()
                    .record_idle_nudge(supervisor_id, now, true);
                warn!(
                    supervisor = supervisor_id,
                    sessions = ?live_ids,
                    "supervisor did not resume after repeated reminders, escalated to the user"
                );
            }
            return None;
        }

        let live_refs: Vec<(u64, SessionState, &str)> = live
            .iter()
            .map(|(id, state, detail)| (*id, *state, detail.as_str()))
            .collect();
        let notice = idle_notice(&live_refs, quiet_for);
        debug_assert!(notice.len() <= SUPERVISOR_INPUT_MAX);
        // A reminder that goes to the agent's own inbox appends to
        // nothing, so only the terminal answers to the typing guard.
        let may_type = !(user_input_pending
            || last_user != 0 && now.saturating_sub(last_user) < IDLE_NUDGE_USER_GRACE_MS);
        let plan = self.agent_message_plan(supervisor_id, &notice, true).ok()?;
        if let Err(failure) = self
            .deliver_agent_message(supervisor_id, &notice, true, plan, false, may_type)
            .await
        {
            // Nothing is recorded, so the next pass retries.
            if let crate::daemon::AgentMessageFailure::Terminal(failure) = failure {
                warn!(
                    supervisor = supervisor_id,
                    failure = failure.as_str(),
                    "supervision reminder was not delivered"
                );
            }
            return None;
        }
        self.supervisor_wake()
            .record_idle_nudge(supervisor_id, now, false);
        info!(
            supervisor = supervisor_id,
            sessions = ?live_ids,
            level = state.level,
            "reminded an idle supervisor that its sessions are still live"
        );
        Some(SupervisorWake {
            supervisor_id,
            sessions: live_ids,
        })
    }

    /// Judges a notice already written to a Supervisor's terminal. The
    /// evidence a notice actually landed is the Supervisor starting a
    /// turn, which `working_since` records; a paste whose Enter was
    /// swallowed never produces one. Transitions are announced only once
    /// that evidence exists, or once the attempts run out.
    fn settle_pending_notice(&self, supervisor_id: u64, now: i64) -> NoticeState {
        let Some(pending) = self.supervisor_wake().take_pending_notice(supervisor_id) else {
            return NoticeState::Settled;
        };
        let revision = self
            .storage()
            .session_transition_marks(supervisor_id)
            .ok()
            .map(|(revision, _)| revision)
            .unwrap_or(pending.state_revision);
        if revision > pending.state_revision {
            for (session_id, key) in pending.announce {
                self.announce_child(session_id, key);
            }
            return NoticeState::Settled;
        }
        if now.saturating_sub(pending.delivered_at_unix_ms) < NOTICE_CONFIRM_MS {
            let attempts = pending.attempts;
            self.supervisor_wake()
                .record_pending_notice(supervisor_id, pending);
            debug_assert!(attempts > 0);
            return NoticeState::Fresh;
        }
        if pending.attempts >= MAX_NOTICE_ATTEMPTS {
            // Announce the transition so the idle reminder ladder can
            // take over. The child is not permanently silenced: the
            // nudge path lists all live children and escalates to the
            // user if the supervisor does not act.
            warn!(
                supervisor = supervisor_id,
                attempts = pending.attempts,
                "supervision notices were written but never started a turn"
            );
            for (session_id, key) in pending.announce {
                self.announce_child(session_id, key);
            }
            return NoticeState::Settled;
        }
        info!(
            supervisor = supervisor_id,
            attempts = pending.attempts,
            "a supervision notice did not start a turn; delivering it again"
        );
        NoticeState::RetryDue(pending.attempts)
    }

    async fn wake_supervisor(
        &self,
        supervisor_id: u64,
        children: &[Session],
        now: i64,
    ) -> Option<SupervisorWake> {
        let supervisor = self.storage().get_session(supervisor_id).ok()?;
        if supervisor.role != SessionRole::Supervisor || !supervisor.supervisor_api {
            return None;
        }
        // An unconfirmed notice holds its transitions unannounced, but
        // does not stop a newly parked child from producing its own.
        let outstanding = self.settle_pending_notice(supervisor_id, now);
        let attempts_so_far = match outstanding {
            NoticeState::RetryDue(attempts) => attempts,
            _ => 0,
        };
        let retrying = matches!(outstanding, NoticeState::RetryDue(_));
        // Any other state means the supervisor is inside a turn, or has
        // ended. Only a completed turn leaves children unwatched.
        if !supervisor_turn_ended(supervisor.state, &supervisor.state_detail) {
            if self
                .supervisor_wake()
                .note_quiet(supervisor_id, "supervisor is inside its own turn")
            {
                info!(
                    supervisor = supervisor_id,
                    state = supervisor.state.as_str(),
                    children = children.len(),
                    "holding supervision notices: supervisor is inside its own turn"
                );
            }
            return None;
        }
        if self.supervisor_wake().is_waiting(supervisor_id) {
            if self
                .supervisor_wake()
                .note_quiet(supervisor_id, "supervisor is inside wait_sessions")
            {
                info!(
                    supervisor = supervisor_id,
                    children = children.len(),
                    "holding supervision notices: supervisor is inside wait_sessions"
                );
            }
            return None;
        }
        // A notice that goes to the agent's own inbox is not typed at
        // anyone, so the quiet window and the half-composed line it
        // protects are not its concern. Only a terminal write waits.
        let via_inbox = self.inbox_delivery_possible(supervisor_id);
        if !via_inbox && !self.terminal_is_quiet_for(&supervisor, now, WAKE_TERMINAL_IDLE_MS) {
            if self
                .supervisor_wake()
                .note_quiet(supervisor_id, "supervisor terminal is active")
            {
                info!(
                    supervisor = supervisor_id,
                    "holding supervision notices: someone is typing into the supervisor"
                );
            }
            return None;
        }

        let mut pending = Vec::new();
        for child in children {
            if !needs_supervision(child.state) {
                continue;
            }
            let Some(key) = self.child_wake_key(child) else {
                continue;
            };
            if self.supervisor_wake().announced_already(child.id, &key) {
                continue;
            }
            pending.push((child.id, child.state, key));
        }
        if pending.is_empty() {
            let parked = children
                .iter()
                .filter(|child| needs_supervision(child.state))
                .count();
            if parked > 0
                && self
                    .supervisor_wake()
                    .note_quiet(supervisor_id, "every transition already announced")
            {
                info!(
                    supervisor = supervisor_id,
                    parked,
                    "no supervision notice: every parked child's transition was already announced"
                );
            }
            return None;
        }
        self.supervisor_wake().clear_quiet(supervisor_id);

        // A retry re-delivers a notice that never arrived, so the throttle
        // that spaces out distinct notices does not apply to it.
        if !retrying && !self.supervisor_wake().throttle_allows(supervisor_id, now) {
            return None;
        }

        let states = pending
            .iter()
            .map(|(id, state, _)| (*id, *state))
            .collect::<Vec<_>>();
        let notice = wake_notice(supervisor.state, &states);
        debug_assert!(notice.len() <= SUPERVISOR_INPUT_MAX);
        let attempts = attempts_so_far
            .max(self.notice_attempts(supervisor_id))
            .saturating_add(1);
        // A notice in the agent's own inbox appends to nothing, so only
        // the terminal answers to the guard: the supervisor may have
        // started typing since.
        let may_type = self.terminal_is_quiet_for(&supervisor, now, WAKE_TERMINAL_IDLE_MS);
        let plan = self.agent_message_plan(supervisor_id, &notice, true).ok()?;
        let delivered_to_inbox = match self
            .deliver_agent_message(supervisor_id, &notice, true, plan, false, may_type)
            .await
        {
            Ok(delivery) => delivery.inbox.is_some(),
            Err(failure) => {
                // Nothing is recorded, so the next pass retries this notice.
                if let crate::daemon::AgentMessageFailure::Terminal(failure) = failure {
                    warn!(
                        supervisor = supervisor_id,
                        failure = failure.as_str(),
                        "supervision notice was not delivered"
                    );
                }
                return None;
            }
        };

        // Recorded at delivery, not at confirmation: the throttle exists to
        // space out what is written to the terminal.
        self.supervisor_wake().record_wake(supervisor_id, now);
        if delivered_to_inbox {
            // The agent's own channel acknowledged the handoff, so there
            // is nothing to confirm and nothing to retry. A terminal
            // write has neither, which is the only reason the pending
            // machinery below exists.
            for (session_id, _, key) in &pending {
                self.announce_child(*session_id, key.clone());
            }
        } else {
            // The transitions stay unannounced until the notice is seen to
            // start a turn, so a paste that lands in a pane which is not
            // accepting input is retried rather than silently consumed.
            self.supervisor_wake().record_pending_notice(
                supervisor_id,
                PendingNotice {
                    delivered_at_unix_ms: now,
                    state_revision: self
                        .storage()
                        .session_transition_marks(supervisor_id)
                        .map(|(revision, _)| revision)
                        .unwrap_or(0),
                    attempts,
                    announce: pending
                        .iter()
                        .map(|(id, _, key)| (*id, key.clone()))
                        .collect(),
                },
            );
        }
        let mut sessions = Vec::with_capacity(pending.len());
        for (session_id, state, _) in pending {
            sessions.push(session_id);
            info!(
                supervisor = supervisor_id,
                session = session_id,
                state = state.as_str(),
                attempt = attempts,
                "woke supervisor for an unattended session"
            );
            self.supervised_item_note(
                supervisor_id,
                session_id,
                &format!(
                    "session {session_id} reached {} unattended; \
                     supervisor session {supervisor_id} was notified to resume supervision",
                    state.as_str()
                ),
            );
        }
        Some(SupervisorWake {
            supervisor_id,
            sessions,
        })
    }

    /// Deliveries already made for the notice currently outstanding.
    fn notice_attempts(&self, supervisor_id: u64) -> u32 {
        self.supervisor_wake()
            .pending_notices
            .lock()
            .unwrap()
            .get(&supervisor_id)
            .map(|pending| pending.attempts)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_reason_is_logged_once_until_it_changes() {
        let runtime = SupervisorWakeRuntime::default();

        // Reconciliation runs on a timer, so repeating the same reason
        // every pass would bury the log rather than explain it.
        assert!(runtime.note_quiet(7, "supervisor is inside its own turn"));
        assert!(!runtime.note_quiet(7, "supervisor is inside its own turn"));

        // A different reason is a different fact and worth a line.
        assert!(runtime.note_quiet(7, "supervisor is inside wait_sessions"));
        assert!(!runtime.note_quiet(7, "supervisor is inside wait_sessions"));
    }

    #[test]
    fn supervisors_do_not_share_a_quiet_reason() {
        let runtime = SupervisorWakeRuntime::default();
        assert!(runtime.note_quiet(7, "supervisor is inside its own turn"));
        assert!(
            runtime.note_quiet(9, "supervisor is inside its own turn"),
            "one supervisor going quiet must not silence the reason for another"
        );
    }

    #[test]
    fn a_supervisor_that_is_woken_explains_itself_again_next_time() {
        let runtime = SupervisorWakeRuntime::default();
        assert!(runtime.note_quiet(7, "supervisor is inside its own turn"));
        runtime.clear_quiet(7);
        assert!(
            runtime.note_quiet(7, "supervisor is inside its own turn"),
            "after a wake the next quiet period is a new fact"
        );
    }

    #[test]
    fn only_states_a_supervisor_must_act_on_need_supervision() {
        for state in [
            SessionState::NeedsInput,
            SessionState::Idle,
            SessionState::Failed,
            SessionState::Exited,
            SessionState::AwaitingWorker,
        ] {
            assert!(needs_supervision(state), "{state:?}");
        }
        for state in [SessionState::Starting, SessionState::Working] {
            assert!(!needs_supervision(state), "{state:?}");
        }
    }

    #[test]
    fn a_supervisor_whose_turn_ended_with_background_work_is_available() {
        assert!(supervisor_turn_ended(
            SessionState::Working,
            crate::daemon::BACKGROUND_WORK_DETAIL
        ));
        assert!(!supervisor_turn_ended(SessionState::Working, ""));
        assert!(!supervisor_turn_ended(
            SessionState::Working,
            "running tests"
        ));
        assert!(supervisor_turn_ended(SessionState::Idle, ""));
        assert!(supervisor_turn_ended(
            SessionState::NeedsInput,
            "which approach?"
        ));
        assert!(!supervisor_turn_ended(SessionState::Starting, ""));
    }

    #[test]
    fn a_re_entered_state_is_a_new_wake_identity() {
        let first = wake_key(1, SessionState::Idle, 7, None);
        assert_eq!(first, wake_key(1, SessionState::Idle, 7, None));
        assert_ne!(first, wake_key(1, SessionState::Idle, 8, None));
        assert_ne!(first, wake_key(2, SessionState::Idle, 7, None));
        assert_ne!(first, wake_key(1, SessionState::NeedsInput, 7, None));

        // A parked child that asks keeps its state and revision, so the
        // question is the only thing that can distinguish the key.
        let parked = wake_key(1, SessionState::NeedsInput, 7, None);
        assert_ne!(parked, wake_key(1, SessionState::NeedsInput, 7, Some(4)));
        assert_ne!(
            wake_key(1, SessionState::NeedsInput, 7, Some(4)),
            wake_key(1, SessionState::NeedsInput, 7, Some(5))
        );
    }

    #[test]
    fn the_notice_names_sessions_and_states_only() {
        let notice = wake_notice(
            SessionState::Idle,
            &[(176, SessionState::NeedsInput), (181, SessionState::Exited)],
        );
        assert!(notice.contains("session 176 is needs-input"), "{notice}");
        assert!(notice.contains("session 181 is exited"), "{notice}");
        assert!(notice.contains("wait_sessions"), "{notice}");
        assert!(
            !notice.contains("flag_blocked"),
            "an idle supervisor has no standing question: {notice}"
        );
        assert!(notice.len() <= SUPERVISOR_INPUT_MAX);
    }

    #[test]
    fn a_blocked_supervisor_is_told_its_question_still_stands() {
        let notice = wake_notice(SessionState::NeedsInput, &[(176, SessionState::Idle)]);
        assert!(notice.contains("flag_blocked"), "{notice}");
        assert!(notice.contains("flagged needs-input"), "{notice}");
    }

    #[test]
    fn a_long_pending_set_is_summarized_within_the_input_cap() {
        let pending = (0..64)
            .map(|id| (id, SessionState::Idle))
            .collect::<Vec<_>>();
        let notice = wake_notice(SessionState::NeedsInput, &pending);
        assert!(notice.contains("and 56 more"), "{notice}");
        assert!(notice.len() <= SUPERVISOR_INPUT_MAX);
    }

    #[test]
    fn a_wait_guard_marks_the_supervisor_only_while_it_is_held() {
        let runtime = SupervisorWakeRuntime::default();
        assert!(!runtime.is_waiting(7));
        {
            *runtime.waiting.lock().unwrap().entry(7).or_insert(0) += 1;
            let _guard = SupervisorWaitGuard {
                runtime: &runtime,
                supervisor_id: 7,
            };
            assert!(runtime.is_waiting(7));
        }
        assert!(!runtime.is_waiting(7));
    }

    #[test]
    fn concurrent_waits_keep_the_supervisor_marked_until_the_last_one_ends() {
        let runtime = SupervisorWakeRuntime::default();
        *runtime.waiting.lock().unwrap().entry(7).or_insert(0) += 1;
        let outer = SupervisorWaitGuard {
            runtime: &runtime,
            supervisor_id: 7,
        };
        *runtime.waiting.lock().unwrap().entry(7).or_insert(0) += 1;
        let inner = SupervisorWaitGuard {
            runtime: &runtime,
            supervisor_id: 7,
        };
        drop(inner);
        assert!(runtime.is_waiting(7));
        drop(outer);
        assert!(!runtime.is_waiting(7));
    }

    #[test]
    fn throttling_spaces_wakes_of_one_supervisor() {
        let runtime = SupervisorWakeRuntime::default();
        assert!(runtime.throttle_allows(7, 1_000));
        runtime.record_wake(7, 1_000);
        assert!(!runtime.throttle_allows(7, 1_000 + WAKE_MIN_INTERVAL_MS - 1));
        assert!(runtime.throttle_allows(7, 1_000 + WAKE_MIN_INTERVAL_MS));
        assert!(runtime.throttle_allows(8, 1_000), "per supervisor");
    }
}
