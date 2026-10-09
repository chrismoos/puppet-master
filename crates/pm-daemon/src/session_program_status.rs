//! Lets an agent's Program Status root record decide its session state.
//!
//! While the agent terminal holds a root record, the record's state is the
//! session state. Lifecycle hooks still do all their own bookkeeping, but
//! the state they would have set is kept aside here instead of being
//! written. When the root record goes away, that kept state is restored, so
//! the session shows what the hooks last said.

use std::collections::HashMap;
use std::sync::Mutex;

use pm_protocol::domain::{
    Event, ProgramStatusRecord, ProgramStatusState, SessionState, TerminalKind,
};
use tracing::{debug, info};

use crate::daemon::Daemon;
use crate::program_status::{Changes, RecordStore};

/// How long a session driven by Program Status must hold a state before it
/// alerts, so a program flipping states raises one alert for where it
/// settles rather than one per flip.
pub const PROGRAM_STATUS_ALERT_DEBOUNCE_MS: i64 = 2_000;

/// An alert held until the session's state settles. `from` is the state
/// before the first transition in the run, so a run that ends where it
/// started alerts for nothing.
struct PendingAlert {
    bucket_id: u64,
    generation: u64,
    from: SessionState,
    due_at_unix_ms: i64,
}

#[derive(Default)]
pub(crate) struct ProgramStatusSessions {
    sessions: Mutex<HashMap<u64, Tracked>>,
}

struct Tracked {
    generation: u64,
    records: RecordStore,
    /// The state and detail hooks would have shown, held while the root
    /// record decides the session state.
    hook_state: Option<(SessionState, String)>,
    pending_alert: Option<PendingAlert>,
}

impl Tracked {
    fn new(generation: u64) -> Self {
        Tracked {
            generation,
            records: RecordStore::default(),
            hook_state: None,
            pending_alert: None,
        }
    }
}

/// The session state and detail a root record stands for. Done reads as
/// idle with a `done` detail, and error as idle with an `error` detail,
/// which is how a failed turn already looks.
pub fn root_session_state(root: &ProgramStatusRecord) -> (SessionState, String) {
    let with_progress = |text: String| match (text.is_empty(), root.progress) {
        (_, None) => text,
        (true, Some(progress)) => format!("{progress}%"),
        (false, Some(progress)) => format!("{text} ({progress}%)"),
    };
    let labeled = |label: &str| {
        if root.msg.is_empty() {
            label.to_string()
        } else {
            format!("{label}: {}", root.msg)
        }
    };
    match root.state {
        ProgramStatusState::Working => (SessionState::Working, with_progress(root.msg.clone())),
        ProgramStatusState::Blocked => {
            let detail = match root.kind {
                Some(kind) => labeled(kind.as_str()),
                None => root.msg.clone(),
            };
            (SessionState::NeedsInput, with_progress(detail))
        }
        ProgramStatusState::Idle => (SessionState::Idle, String::new()),
        ProgramStatusState::Done => (SessionState::Idle, labeled("done")),
        ProgramStatusState::Error => (SessionState::Idle, labeled("error")),
    }
}

/// States the root record may replace. Anything else belongs to a process
/// or worker lifecycle the record knows nothing about.
fn record_may_decide(state: SessionState) -> bool {
    matches!(
        state,
        SessionState::Starting
            | SessionState::Working
            | SessionState::NeedsInput
            | SessionState::Idle
    )
}

impl Daemon {
    /// Applies a change to an agent terminal's records and moves the
    /// session to what its root record says.
    pub fn handle_program_status(&self, terminal_id: u64, generation: u64, changes: &Changes) {
        let Ok(terminal) = self.storage().get_terminal(terminal_id) else {
            return;
        };
        if terminal.generation != generation || terminal.kind != TerminalKind::Agent {
            debug!(
                terminal = terminal_id,
                generation, "ignored program status for a stale or non-agent terminal"
            );
            return;
        }
        let session_id = terminal.session_id;
        let _guard = self.lock_session_state();
        let Ok(session) = self.storage().get_session(session_id) else {
            return;
        };
        let decided = {
            let mut sessions = self.program_status().sessions.lock().unwrap();
            let tracked = sessions
                .entry(session_id)
                .or_insert_with(|| Tracked::new(generation));
            if tracked.generation != generation {
                *tracked = Tracked::new(generation);
            }
            tracked.records.merge(changes);
            match tracked.records.root() {
                Some(root) if record_may_decide(session.state) => {
                    tracked
                        .hook_state
                        .get_or_insert_with(|| (session.state, session.state_detail.clone()));
                    Some(root_session_state(root))
                }
                Some(_) => None,
                None => tracked
                    .hook_state
                    .take()
                    .filter(|_| record_may_decide(session.state)),
            }
        };
        let updated = match decided {
            Some((state, detail)) if state != session.state || detail != session.state_detail => {
                match self
                    .storage()
                    .update_session_state(session_id, state, &detail)
                {
                    Ok(updated) => {
                        info!(
                            session = session_id,
                            state = state.as_str(),
                            "program status transition"
                        );
                        updated
                    }
                    Err(_) => return,
                }
            }
            _ => session,
        };
        self.publish(Event::SessionChanged(updated));
    }

    /// The session's records with `app` resolved, root first.
    pub(crate) fn program_status_records(&self, session_id: u64) -> Vec<ProgramStatusRecord> {
        self.program_status()
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|tracked| tracked.records.resolved())
            .unwrap_or_default()
    }

    /// The state hooks would show, when a root record decides the session
    /// state instead.
    pub(crate) fn deferred_hook_state(&self, session_id: u64) -> Option<(SessionState, String)> {
        self.program_status()
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .and_then(|tracked| tracked.hook_state.clone())
    }

    /// Keeps a hook-derived state aside when a root record decides the
    /// session state. Returns false, keeping nothing, when none does.
    pub(crate) fn defer_hook_state(
        &self,
        session_id: u64,
        state: SessionState,
        detail: &str,
    ) -> bool {
        let mut sessions = self.program_status().sessions.lock().unwrap();
        let Some(hook_state) = sessions
            .get_mut(&session_id)
            .and_then(|tracked| tracked.hook_state.as_mut())
        else {
            return false;
        };
        *hook_state = (state, detail.to_string());
        true
    }

    /// Whether a root record decides the session state.
    pub(crate) fn program_status_decides(&self, session_id: u64) -> bool {
        self.deferred_hook_state(session_id).is_some()
    }

    /// Ends the root record's hold on the state of a session whose agent
    /// exited. Its records stay to be shown until the agent is respawned.
    pub(crate) fn program_status_exited(&self, session_id: u64) {
        if let Some(tracked) = self
            .program_status()
            .sessions
            .lock()
            .unwrap()
            .get_mut(&session_id)
        {
            tracked.hook_state = None;
        }
    }

    /// Drops everything held for a session, for a respawn or a removal.
    pub(crate) fn forget_program_status(&self, session_id: u64) {
        self.program_status()
            .sessions
            .lock()
            .unwrap()
            .remove(&session_id);
    }

    /// Holds the alert for a transition between states a root record
    /// decides, and returns true. Returns false, dropping any held alert,
    /// for every other transition, which alerts at once.
    pub(crate) fn debounce_program_status_alert(
        &self,
        session_id: u64,
        bucket_id: u64,
        generation: u64,
        from: SessionState,
        to: SessionState,
        now_unix_ms: i64,
    ) -> bool {
        let mut sessions = self.program_status().sessions.lock().unwrap();
        let Some(tracked) = sessions.get_mut(&session_id) else {
            return false;
        };
        if tracked.hook_state.is_none() || !record_may_decide(from) || !record_may_decide(to) {
            tracked.pending_alert = None;
            return false;
        }
        let from = match &tracked.pending_alert {
            Some(pending) if pending.generation == generation => pending.from,
            _ => from,
        };
        tracked.pending_alert = Some(PendingAlert {
            bucket_id,
            generation,
            from,
            due_at_unix_ms: now_unix_ms + PROGRAM_STATUS_ALERT_DEBOUNCE_MS,
        });
        true
    }

    pub fn flush_program_status_alerts(&self) {
        self.flush_program_status_alerts_at(crate::daemon::now_unix_ms());
    }

    /// Raises each held alert whose session has kept its state for the
    /// debounce interval, judged from where the run started to where it
    /// settled.
    pub fn flush_program_status_alerts_at(&self, now_unix_ms: i64) {
        let due: Vec<(u64, PendingAlert)> = {
            let mut sessions = self.program_status().sessions.lock().unwrap();
            sessions
                .iter_mut()
                .filter(|(_, tracked)| {
                    tracked
                        .pending_alert
                        .as_ref()
                        .is_some_and(|pending| pending.due_at_unix_ms <= now_unix_ms)
                })
                .filter_map(|(id, tracked)| {
                    tracked
                        .pending_alert
                        .take()
                        .filter(|pending| pending.generation == tracked.generation)
                        .map(|pending| (*id, pending))
                })
                .collect()
        };
        for (session_id, pending) in due {
            let _guard = self.lock_session_state();
            let Ok(session) = self.storage().get_session(session_id) else {
                continue;
            };
            if session.state == pending.from {
                continue;
            }
            let mut session = session;
            session.program_status = self.program_status_records(session_id);
            if let Some(alert) = self.observe_push_transition(
                &session,
                pending.bucket_id,
                pending.generation,
                pending.from,
                session.state,
            ) {
                self.send_session_alert(alert);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_protocol::domain::ProgramStatusKind;

    fn root(state: ProgramStatusState) -> ProgramStatusRecord {
        ProgramStatusRecord {
            id: String::new(),
            state,
            kind: None,
            progress: None,
            app: "claude-code".into(),
            title: String::new(),
            msg: String::new(),
            updated_at_unix_ms: 0,
        }
    }

    #[test]
    fn working_carries_its_message_and_progress() {
        let mut record = root(ProgramStatusState::Working);
        assert_eq!(
            root_session_state(&record),
            (SessionState::Working, String::new())
        );
        record.progress = Some(40);
        assert_eq!(
            root_session_state(&record),
            (SessionState::Working, "40%".into())
        );
        record.msg = "Running tests".into();
        assert_eq!(
            root_session_state(&record),
            (SessionState::Working, "Running tests (40%)".into())
        );
    }

    #[test]
    fn blocked_is_needs_input_labeled_with_its_kind() {
        let mut record = root(ProgramStatusState::Blocked);
        record.msg = "Allow Bash(rm -rf build)?".into();
        assert_eq!(
            root_session_state(&record),
            (SessionState::NeedsInput, "Allow Bash(rm -rf build)?".into())
        );
        record.kind = Some(ProgramStatusKind::Permission);
        assert_eq!(
            root_session_state(&record),
            (
                SessionState::NeedsInput,
                "permission: Allow Bash(rm -rf build)?".into()
            )
        );
        record.msg.clear();
        record.kind = Some(ProgramStatusKind::Auth);
        assert_eq!(
            root_session_state(&record),
            (SessionState::NeedsInput, "auth".into())
        );
    }

    #[test]
    fn idle_done_and_error_are_idle_and_told_apart_by_their_detail() {
        assert_eq!(
            root_session_state(&root(ProgramStatusState::Idle)),
            (SessionState::Idle, String::new())
        );
        let mut done = root(ProgramStatusState::Done);
        assert_eq!(
            root_session_state(&done),
            (SessionState::Idle, "done".into())
        );
        done.msg = "Fixed 3 tests".into();
        assert_eq!(
            root_session_state(&done),
            (SessionState::Idle, "done: Fixed 3 tests".into())
        );
        let mut error = root(ProgramStatusState::Error);
        error.msg = "API error".into();
        assert_eq!(
            root_session_state(&error),
            (SessionState::Idle, "error: API error".into())
        );
    }
}
