//! Remembers when each session's goal and headline last changed, so a
//! report that repeats a line the dashboard has shown for a long time is
//! answered with a request to update it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

/// How long a headline may stay the same before a report is asked about it.
pub(crate) const DEFAULT_HEADLINE_STALE_MS: i64 = 10 * 60 * 1000;
/// How long a goal may stay the same before a report is asked about it.
pub(crate) const DEFAULT_GOAL_STALE_MS: i64 = 60 * 60 * 1000;

#[derive(Default)]
pub(crate) struct ReportFreshness {
    sessions: Mutex<HashMap<u64, Tracked>>,
    headline_stale_ms: AtomicI64,
    goal_stale_ms: AtomicI64,
}

#[derive(Clone, Copy)]
struct Line {
    changed_at: i64,
    /// When the last nudge went out, so a line is asked about once per
    /// stale window rather than on every report.
    nudged_at: Option<i64>,
}

#[derive(Default)]
struct Tracked {
    goal: Option<Line>,
    headline: Option<Line>,
}

pub(crate) struct Observed<'a> {
    pub goal: &'a str,
    pub goal_changed: bool,
    pub headline: &'a str,
    pub headline_changed: bool,
}

impl ReportFreshness {
    pub(crate) fn set_thresholds(&self, headline_ms: i64, goal_ms: i64) {
        self.headline_stale_ms.store(headline_ms, Ordering::Relaxed);
        self.goal_stale_ms.store(goal_ms, Ordering::Relaxed);
    }

    fn threshold(cell: &AtomicI64, default: i64) -> i64 {
        match cell.load(Ordering::Relaxed) {
            0 => default,
            set => set,
        }
    }

    /// Records this report and returns the lines worth asking about.
    pub(crate) fn observe(&self, session_id: u64, now: i64, seen: Observed<'_>) -> Vec<String> {
        let headline_stale = Self::threshold(&self.headline_stale_ms, DEFAULT_HEADLINE_STALE_MS);
        let goal_stale = Self::threshold(&self.goal_stale_ms, DEFAULT_GOAL_STALE_MS);
        let mut sessions = self.sessions.lock().unwrap();
        let tracked = sessions.entry(session_id).or_default();
        let mut notes = Vec::new();
        if let Some(age) = observe_line(
            &mut tracked.headline,
            now,
            seen.headline_changed,
            headline_stale,
        ) {
            notes.push(format!(
                "headline has been {:?} for {}; if the step moved on, send the new one",
                seen.headline,
                describe_age(age)
            ));
        }
        if let Some(age) = observe_line(&mut tracked.goal, now, seen.goal_changed, goal_stale) {
            notes.push(format!(
                "goal has been {:?} for {}; keep it only if it still names the work",
                seen.goal,
                describe_age(age)
            ));
        }
        notes
    }

    pub(crate) fn forget(&self, session_id: u64) {
        self.sessions.lock().unwrap().remove(&session_id);
    }
}

/// Returns how long the line has gone unchanged when that is long enough
/// to ask about and it has not been asked about within that window.
fn observe_line(line: &mut Option<Line>, now: i64, changed: bool, stale_ms: i64) -> Option<i64> {
    let Some(current) = line.as_mut().filter(|_| !changed) else {
        *line = Some(Line {
            changed_at: now,
            nudged_at: None,
        });
        return None;
    };
    let age = now - current.changed_at;
    if age < stale_ms {
        return None;
    }
    if current.nudged_at.is_some_and(|at| now - at < stale_ms) {
        return None;
    }
    current.nudged_at = Some(now);
    Some(age)
}

fn describe_age(ms: i64) -> String {
    let minutes = ms / 60_000;
    if minutes < 1 {
        return "under a minute".into();
    }
    if minutes < 60 {
        return format!("{minutes} min");
    }
    let hours = minutes / 60;
    match minutes % 60 {
        0 => format!("{hours} h"),
        rest => format!("{hours} h {rest} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen<'a>(
        goal: &'a str,
        goal_changed: bool,
        headline: &'a str,
        headline_changed: bool,
    ) -> Observed<'a> {
        Observed {
            goal,
            goal_changed,
            headline,
            headline_changed,
        }
    }

    #[test]
    fn a_repeated_headline_is_asked_about_once_per_window_and_a_change_resets_it() {
        let freshness = ReportFreshness::default();
        freshness.set_thresholds(100, 1_000);
        assert!(freshness
            .observe(1, 0, seen("g", true, "a", true))
            .is_empty());
        assert!(freshness
            .observe(1, 50, seen("g", false, "a", false))
            .is_empty());
        let asked = freshness.observe(1, 100, seen("g", false, "a", false));
        assert_eq!(asked, vec!["headline has been \"a\" for under a minute; if the step moved on, send the new one"]);
        assert!(freshness
            .observe(1, 150, seen("g", false, "a", false))
            .is_empty());
        assert_eq!(
            freshness
                .observe(1, 200, seen("g", false, "a", false))
                .len(),
            1
        );
        assert!(freshness
            .observe(1, 250, seen("g", false, "b", true))
            .is_empty());
        assert!(freshness
            .observe(1, 300, seen("g", false, "b", false))
            .is_empty());
    }

    #[test]
    fn the_goal_has_its_own_longer_window_and_sessions_are_tracked_apart() {
        let freshness = ReportFreshness::default();
        freshness.set_thresholds(100, 1_000);
        freshness.observe(1, 0, seen("g", true, "a", true));
        freshness.observe(2, 0, seen("g", true, "a", true));
        let asked = freshness.observe(1, 1_000, seen("g", false, "a", false));
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[1].starts_with("goal has been \"g\" for"));
        freshness.forget(1);
        assert!(freshness
            .observe(1, 1_050, seen("g", false, "a", false))
            .is_empty());
        assert_eq!(
            freshness
                .observe(2, 1_050, seen("g", false, "a", false))
                .len(),
            2
        );
    }

    #[test]
    fn ages_read_as_minutes_and_hours() {
        assert_eq!(describe_age(59_000), "under a minute");
        assert_eq!(describe_age(23 * 60_000), "23 min");
        assert_eq!(describe_age(120 * 60_000), "2 h");
        assert_eq!(describe_age(135 * 60_000), "2 h 15 min");
    }
}
