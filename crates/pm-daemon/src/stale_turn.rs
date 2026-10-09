//! Infers a turn end that was never reported.
//!
//! A hook-integrated agent's turn ends when its Stop hook reaches the
//! daemon. Nothing else moves a session out of Working, so a hook lost to
//! a busy daemon, a scrubbed environment, a superseded generation, or an
//! agent relaunched by hand leaves the session mid-turn forever: Working
//! is not a state a supervisor is ever notified about, so the work simply
//! stops being watched.
//!
//! This watches for a hook-capable generation whose PTY has gone
//! completely silent and moves it to Idle with a state detail that says
//! the end was inferred. It never reports a clean completion, and it arms
//! one PTY-output promotion so an agent that was merely quiet returns to
//! Working on its next byte of output.

use std::collections::HashMap;
use std::sync::Mutex;

use pm_protocol::domain::{Session, SessionState};
use tracing::{info, warn};

use crate::daemon::{now_unix_ms, Daemon};

/// PTY silence that makes a hook-capable turn suspect. A working agent
/// writes tool output continuously, so total silence for this long is
/// already pathological rather than merely slow.
pub const DEFAULT_STALE_TURN_QUIET_MS: i64 = 2 * 60 * 1000;

/// Consecutive passes a session must look stale before it is demoted. One
/// pass is a sample; two separated by the reconciliation interval rule out
/// a coincidence with a checkpoint or a burst of catch-up work.
const STALE_PASSES_REQUIRED: u32 = 2;

/// Recorded on a session whose turn end was inferred. Non-empty by
/// design: the push classifier treats an empty detail as evidence of a
/// clean completion, and an inferred stop is not one.
pub const INFERRED_IDLE_DETAIL: &str = "turn end not reported; inferred from an idle terminal";

/// Recorded on a session whose adapter installs lifecycle hooks that never
/// fired, which is a broken launch rather than a quiet agent.
pub const HOOKS_UNOBSERVED_DETAIL: &str = "agent lifecycle hooks have not been observed";

/// How long a hook-integrated generation may run without producing a
/// single hook before its hooks are treated as not working. SessionStart
/// fires at launch, so this only needs to cover process startup.
pub const DEFAULT_HOOK_SILENCE_GRACE_MS: i64 = 60 * 1000;

/// Per-session watchdog bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct StaleTurnRuntime {
    /// session -> consecutive passes it has looked stale, keyed with the
    /// generation so a resume restarts the count.
    suspect: Mutex<HashMap<u64, (u64, u32)>>,
}

impl StaleTurnRuntime {
    /// Counts one stale observation and reports whether the session has
    /// now been stale for long enough to act on.
    fn observe_stale(&self, session_id: u64, generation: u64) -> u32 {
        let mut suspect = self.suspect.lock().unwrap();
        let entry = suspect.entry(session_id).or_insert((generation, 0));
        if entry.0 != generation {
            *entry = (generation, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1
    }

    fn clear(&self, session_id: u64) {
        self.suspect.lock().unwrap().remove(&session_id);
    }

    pub(crate) fn forget_session(&self, session_id: u64) {
        self.clear(session_id);
    }
}

/// What one pass did, for tests and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTurnOutcome {
    pub session_id: u64,
    pub kind: StaleTurnKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleTurnKind {
    /// The turn was moved to Idle because its end was never reported.
    InferredIdle,
    /// The generation's hooks never fired at all.
    HooksUnobserved,
}

impl Daemon {
    pub(crate) fn stale_turn(&self) -> &StaleTurnRuntime {
        &self.stale_turn_runtime
    }

    pub fn process_stale_turns(&self) -> Vec<StaleTurnOutcome> {
        self.process_stale_turns_at(now_unix_ms())
    }

    /// Time-parameterized so tests can cross the quiet boundary without
    /// sleeping for real time.
    #[doc(hidden)]
    pub fn process_stale_turns_at(&self, now: i64) -> Vec<StaleTurnOutcome> {
        let sessions = match self.storage().working_sessions() {
            Ok(sessions) => sessions,
            Err(error) => {
                warn!(%error, "failed to load sessions for the stale-turn pass");
                return Vec::new();
            }
        };
        sessions
            .into_iter()
            .filter_map(|session| self.reconcile_working_session(&session, now))
            .collect()
    }

    fn reconcile_working_session(&self, session: &Session, now: i64) -> Option<StaleTurnOutcome> {
        // A hookless adapter has no turn-end signal to lose, so its
        // Working state is never evidence of anything going wrong.
        if !self.adapter_has_lifecycle_hooks(session.agent) {
            return None;
        }
        // An agent reporting through Program Status states its own turn
        // end, so a quiet terminal is not evidence of a lost one.
        if self.program_status_decides(session.id) {
            return None;
        }
        let terminal = self.storage().agent_terminal(session.id).ok()?;
        // A session whose agent process is gone is an exit, handled by the
        // exit path rather than here.
        if !self.agent_process_is_live(session, &terminal) {
            self.stale_turn().clear(session.id);
            return None;
        }
        let (hook_authoritative, registered_at, silence_reported) =
            self.generation_hook_status(terminal.id, terminal.generation)?;

        if !hook_authoritative {
            let silent_for = now.saturating_sub(registered_at);
            if silent_for < self.hook_silence_grace_ms() || silence_reported {
                return None;
            }
            return self.downgrade_to_hookless(session, &terminal, silent_for);
        }

        let (agent_activity, _, _) = self.session_activity_clocks(session);
        let working_since = self
            .storage()
            .session_transition_marks(session.id)
            .ok()
            .and_then(|(_, working_since)| working_since)
            .unwrap_or(session.created_at_unix_ms);
        // Measure from the later of the two: a turn that just started is
        // not stale merely because the PTY was quiet before it.
        let quiet_since = agent_activity.max(working_since);
        if now.saturating_sub(quiet_since) < self.stale_turn_quiet_ms() {
            self.stale_turn().clear(session.id);
            return None;
        }
        let passes = self
            .stale_turn()
            .observe_stale(session.id, terminal.generation);
        if passes < STALE_PASSES_REQUIRED {
            return None;
        }
        self.stale_turn().clear(session.id);
        self.infer_turn_end(session, &terminal, now.saturating_sub(quiet_since))
    }

    /// Whether the agent behind this session is still running. A remote
    /// worker that went offline is not evidence either way, so its
    /// sessions are left alone.
    fn agent_process_is_live(
        &self,
        session: &Session,
        terminal: &pm_protocol::domain::Terminal,
    ) -> bool {
        if session.worker_id == pm_protocol::domain::LOCAL_WORKER_ID {
            return self.mux_is_running(terminal.id);
        }
        self.worker_is_online(session.worker_id)
    }

    fn infer_turn_end(
        &self,
        session: &Session,
        terminal: &pm_protocol::domain::Terminal,
        quiet_for: i64,
    ) -> Option<StaleTurnOutcome> {
        // The agent may have been quiet rather than finished, so arm the
        // promotion before the state lands: one PTY write then puts it
        // straight back to Working.
        self.arm_terminal_fallback_for(terminal.id, terminal.generation);
        self.commit_session_state(session.id, SessionState::Idle, INFERRED_IDLE_DETAIL)
            .ok()?;
        warn!(
            session = session.id,
            agent = session.agent.as_str(),
            quiet_for_ms = quiet_for,
            "no turn-end hook arrived for a quiet agent; inferring the turn ended"
        );
        Some(StaleTurnOutcome {
            session_id: session.id,
            kind: StaleTurnKind::InferredIdle,
        })
    }

    /// Stops trusting hooks on a generation that has never produced one,
    /// so PTY output can drive its state instead of nothing at all.
    fn downgrade_to_hookless(
        &self,
        session: &Session,
        terminal: &pm_protocol::domain::Terminal,
        silent_for: i64,
    ) -> Option<StaleTurnOutcome> {
        self.mark_hook_silence_reported(terminal.id, terminal.generation);
        self.arm_terminal_fallback_for(terminal.id, terminal.generation);
        self.commit_session_state(session.id, SessionState::Idle, HOOKS_UNOBSERVED_DETAIL)
            .ok()?;
        warn!(
            session = session.id,
            agent = session.agent.as_str(),
            silent_for_ms = silent_for,
            "no lifecycle hook has ever arrived for this generation; \
             treating the agent as hookless"
        );
        info!(
            session = session.id,
            "check that the agent CLI supports the hooks pm configures for it"
        );
        Some(StaleTurnOutcome {
            session_id: session.id,
            kind: StaleTurnKind::HooksUnobserved,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_observation_only_acts_on_the_second_consecutive_pass() {
        let runtime = StaleTurnRuntime::default();
        assert_eq!(runtime.observe_stale(1, 7), 1);
        assert_eq!(runtime.observe_stale(1, 7), STALE_PASSES_REQUIRED);
    }

    #[test]
    fn a_new_generation_restarts_the_count() {
        let runtime = StaleTurnRuntime::default();
        assert_eq!(runtime.observe_stale(1, 7), 1);
        assert_eq!(runtime.observe_stale(1, 7), 2);
        assert_eq!(runtime.observe_stale(1, 8), 1);
    }

    #[test]
    fn activity_clears_the_count() {
        let runtime = StaleTurnRuntime::default();
        assert_eq!(runtime.observe_stale(1, 7), 1);
        runtime.clear(1);
        assert_eq!(runtime.observe_stale(1, 7), 1);
    }

    /// An inferred stop must never look like a clean completion to the
    /// push classifier, which keys that on an empty state detail.
    #[test]
    fn inferred_details_are_never_empty() {
        assert!(!INFERRED_IDLE_DETAIL.is_empty());
        assert!(!HOOKS_UNOBSERVED_DETAIL.is_empty());
    }
}
