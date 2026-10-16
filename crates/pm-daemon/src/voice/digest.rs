//! What the voice agent knows about the fleet, as text it can read.
//!
//! Built from the same snapshot every client subscribes to, so what the
//! user hears and what the dashboard shows cannot drift apart. It is a
//! pure function of that snapshot: no terminal output, no per-session
//! queries, nothing that depends on which bucket anybody is looking at.
//!
//! Terminal output is deliberately absent. An agent's own report is
//! short, current, and written to be understood, and build output read
//! aloud is unusable.

use std::collections::HashMap;
use std::fmt::Write as _;

use pm_protocol::domain::{Item, ItemStatus, Session, SessionState, Snapshot};

/// How long a headline may stand unchanged before the digest says so. An
/// agent that has not reported in this long may be stuck, and saying
/// when it last spoke is more honest than repeating a stale line as
/// though it were current.
const STALE_HEADLINE_MS: i64 = 10 * 60 * 1000;

/// How many live sessions are listed one by one before the rest become a
/// count. A listener cannot hold a long list, and the ones that need
/// them are already listed above.
const RUNNING_LISTED: usize = 6;

/// How much of a report line is carried. Enough to say what is
/// happening, short enough that the digest stays readable.
const LINE_MAX: usize = 160;

/// What writes act on. Reads ignore it: the user can ask about anything
/// anywhere. A write needs it resolved, because a guess acts on the
/// wrong repository.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Focus {
    pub bucket_id: Option<u64>,
    pub project_id: Option<u64>,
    pub session_id: Option<u64>,
}

impl Focus {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

pub(super) fn clamp(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= LINE_MAX {
        return trimmed.to_string();
    }
    let kept: String = trimmed.chars().take(LINE_MAX).collect();
    format!("{kept}...")
}

/// How long ago, in units somebody would say out loud.
fn ago(now_unix_ms: i64, then_unix_ms: i64) -> String {
    let seconds = (now_unix_ms - then_unix_ms).max(0) / 1000;
    match seconds {
        0..=44 => "just now".to_string(),
        // Switches to hours before the minute count gets high enough to
        // sound like a reading rather than a sentence.
        45..=2699 => {
            let minutes = (seconds + 30) / 60;
            match minutes {
                0 | 1 => "a minute ago".to_string(),
                _ => format!("{minutes} minutes ago"),
            }
        }
        2700..=5399 => "an hour ago".to_string(),
        _ => {
            let hours = (seconds + 1800) / 3600;
            match hours {
                0 | 1 => "an hour ago".to_string(),
                2..=47 => format!("{hours} hours ago"),
                _ => format!("{} days ago", (hours + 12) / 24),
            }
        }
    }
}

/// What to call a session out loud. The goal is what the agent says it
/// is doing, which is what a person recognizes it by. The id is carried
/// too, because the agent resolves a spoken name back to one.
pub(super) fn label(session: &Session) -> String {
    let name = [session.goal.as_str(), session.task_title.as_str()]
        .into_iter()
        .map(str::trim)
        .find(|candidate| !candidate.is_empty())
        .unwrap_or("unnamed");
    format!("{:?} (session {})", clamp(name), session.id)
}

fn project_names(snapshot: &Snapshot) -> HashMap<u64, String> {
    snapshot
        .projects
        .iter()
        .map(|project| (project.id, project.name.clone()))
        .collect()
}

/// The question a session is waiting on, which is the thing worth
/// hearing about a blocked session. An agent's own detail line is
/// preferred: it is what it said when it stopped.
fn pending_question(session: &Session, items: &[Item]) -> Option<String> {
    let detail = session.state_detail.trim();
    if !detail.is_empty() {
        return Some(clamp(detail));
    }
    items
        .iter()
        .find(|item| item.session_ids.contains(&session.id) && !item.question.trim().is_empty())
        .map(|item| clamp(&item.question))
}

pub fn digest(snapshot: &Snapshot, now_unix_ms: i64, focus: &Focus) -> String {
    let projects = project_names(snapshot);
    let mut out = String::new();

    let live: Vec<&Session> = snapshot
        .sessions
        .iter()
        .filter(|session| session.ended_at_unix_ms.is_none())
        .collect();
    let in_state = |state: SessionState| -> Vec<&&Session> {
        live.iter().filter(|s| s.state == state).collect()
    };

    let where_it_runs = |session: &Session| -> String {
        projects
            .get(&session.project_id)
            .map(|name| format!(" in {name}"))
            .unwrap_or_default()
    };

    let needs_input = in_state(SessionState::NeedsInput);
    if needs_input.is_empty() {
        out.push_str("Nothing needs you.\n");
    } else {
        let _ = writeln!(out, "Needs you ({}):", needs_input.len());
        for session in &needs_input {
            let _ = write!(out, "- {}{}", label(session), where_it_runs(session));
            match pending_question(session, &snapshot.items) {
                Some(question) => {
                    let _ = writeln!(out, " asks: {question}");
                }
                None => {
                    let _ = writeln!(out, " is waiting on you, and did not say what for");
                }
            }
        }
    }

    let failed = in_state(SessionState::Failed);
    if !failed.is_empty() {
        let _ = writeln!(out, "\nFailed ({}):", failed.len());
        for session in &failed {
            let detail = clamp(&session.state_detail);
            let _ = writeln!(
                out,
                "- {}{}: {}",
                label(session),
                where_it_runs(session),
                if detail.is_empty() {
                    "no reason reported".to_string()
                } else {
                    detail
                }
            );
        }
    }

    // Every live session is named, whatever state it is in: the user
    // refers to one by what the dashboard calls it, and a count cannot
    // be resolved back to anything to read about or act on.
    let listed = |out: &mut String, heading: &str, sessions: &[&&Session], stale_ages: bool| {
        if sessions.is_empty() {
            return;
        }
        let _ = writeln!(out, "\n{heading} ({}):", sessions.len());
        for session in sessions.iter().take(RUNNING_LISTED) {
            let headline = clamp(&session.headline);
            let _ = write!(out, "- {}{}", label(session), where_it_runs(session));
            if headline.is_empty() {
                let _ = write!(out, " has not reported anything yet");
            } else {
                let _ = write!(out, ": {headline}");
            }
            if stale_ages && now_unix_ms - session.last_activity_at_unix_ms > STALE_HEADLINE_MS {
                // Saying the age is the honest version: the line may
                // describe something it finished long ago.
                let _ = write!(
                    out,
                    " (last said anything {})",
                    ago(now_unix_ms, session.last_activity_at_unix_ms)
                );
            }
            out.push('\n');
        }
        if sessions.len() > RUNNING_LISTED {
            let _ = writeln!(
                out,
                "- and {} more {}",
                sessions.len() - RUNNING_LISTED,
                heading.to_lowercase()
            );
        }
    };

    listed(&mut out, "Working", &in_state(SessionState::Working), true);
    listed(&mut out, "Idle", &in_state(SessionState::Idle), false);
    let mut starting = in_state(SessionState::Starting);
    starting.extend(in_state(SessionState::AwaitingWorker));
    listed(&mut out, "Starting", &starting, false);

    let open_items = snapshot
        .items
        .iter()
        .filter(|item| !matches!(item.status, ItemStatus::Done | ItemStatus::Dropped));
    let mut asking = Vec::new();
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for item in open_items {
        let bucket = status_name(item.status);
        *counts.entry(bucket).or_default() += 1;
        if !item.question.trim().is_empty() {
            asking.push(item);
        }
    }
    if !counts.is_empty() {
        let mut ordered: Vec<_> = counts.into_iter().collect();
        ordered.sort_unstable();
        let summary = ordered
            .into_iter()
            .map(|(name, n)| format!("{n} {name}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "\nBoard: {summary}.");
    }
    for item in asking.iter().take(RUNNING_LISTED) {
        // Named by title, never by number: an item id is only unique
        // inside its bucket, so a spoken number is ambiguous.
        let _ = writeln!(
            out,
            "- the item {:?} asks: {}",
            clamp(&item.title),
            clamp(&item.question)
        );
    }

    match focused_line(snapshot, focus, &projects) {
        Some(line) => {
            let _ = writeln!(out, "\n{line}");
        }
        None => {
            out.push_str("\nNo project is in focus, so starting new work needs one chosen first.\n")
        }
    }
    out
}

fn status_name(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Inbox => "waiting on you to triage",
        ItemStatus::Planned => "planned",
        ItemStatus::InProgress => "in progress",
        ItemStatus::Blocked => "blocked",
        ItemStatus::BlockedExternal => "waiting on somebody else",
        ItemStatus::Done => "done",
        ItemStatus::Dropped => "dropped",
    }
}

fn focused_line(
    snapshot: &Snapshot,
    focus: &Focus,
    projects: &HashMap<u64, String>,
) -> Option<String> {
    if let Some(session_id) = focus.session_id {
        let session = snapshot.sessions.iter().find(|s| s.id == session_id)?;
        return Some(format!("Focused on {}.", label(session)));
    }
    if let Some(project_id) = focus.project_id {
        let name = projects.get(&project_id)?;
        return Some(format!("Focused on the project {name}."));
    }
    let bucket_id = focus.bucket_id?;
    let bucket = snapshot.buckets.iter().find(|b| b.id == bucket_id)?;
    Some(format!("Focused on {}.", bucket.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_protocol::domain::{
        AgentKind, AgentSelectionSource, ItemPriority, ItemSourceKind, PermissionMode, Project,
        SessionRole,
    };

    const NOW: i64 = 1_800_000_000_000;

    fn session(id: u64, goal: &str, state: SessionState) -> Session {
        Session {
            id,
            project_id: 1,
            agent: AgentKind::Test,
            agent_source: AgentSelectionSource::Explicit,
            state,
            task_title: String::new(),
            task_prompt: String::new(),
            agent_session_id: None,
            created_at_unix_ms: NOW - 60_000,
            ended_at_unix_ms: None,
            exit_code: None,
            state_detail: String::new(),
            activity: String::new(),
            progress_percent: None,
            resumable: false,
            permission_mode: PermissionMode::Default,
            worker_id: 0,
            cwd: String::new(),
            goal: goal.to_string(),
            headline: String::new(),
            summary: String::new(),
            git: None,
            items_api: true,
            supervisor_api: false,
            spawned_by_session_id: None,
            role: SessionRole::Worker,
            last_activity_at_unix_ms: NOW - 1_000,
            last_agent_activity_at_unix_ms: NOW - 1_000,
            last_user_interaction_at_unix_ms: NOW - 1_000,
            needs_input_unseen: false,
            idle_unseen: false,
            model_profile_id: None,
            model_profile_source: None,
            program_status: Vec::new(),
        }
    }

    fn item(id: u64, title: &str, question: &str, status: ItemStatus) -> Item {
        Item {
            id,
            bucket_id: 1,
            project_id: Some(1),
            external_key: None,
            title: title.to_string(),
            body: String::new(),
            question: question.to_string(),
            status,
            priority: ItemPriority::Normal,
            source_kind: ItemSourceKind::Human,
            source_detail: String::new(),
            url: String::new(),
            due_at_unix_ms: None,
            snoozed_until_unix_ms: None,
            created_by_session_id: None,
            created_at_unix_ms: NOW,
            updated_at_unix_ms: NOW,
            done_at_unix_ms: None,
            blocked_by: Vec::new(),
            session_ids: Vec::new(),
        }
    }

    fn snapshot(sessions: Vec<Session>, items: Vec<Item>) -> Snapshot {
        Snapshot {
            projects: vec![Project {
                id: 1,
                bucket_id: 1,
                name: "puppet-master".into(),
                path: "/srv/pm".into(),
                permission_mode: PermissionMode::Inherit,
                default_agent: None,
                model_profile_id: None,
                worker_id: None,
                allowed_worker_ids: Vec::new(),
                worker_paths: Vec::new(),
            }],
            sessions,
            items,
            ..Snapshot::default()
        }
    }

    /// The thing a listener most needs comes first, because they hear
    /// the digest in order and may stop listening.
    #[test]
    fn a_blocked_session_is_named_before_a_working_one() {
        let mut asking = session(2, "Landing the iOS fix", SessionState::NeedsInput);
        asking.state_detail = "Which branch should I deliver to?".into();
        let working = session(3, "Reclaiming disk space", SessionState::Working);
        let text = digest(
            &snapshot(vec![working, asking], vec![]),
            NOW,
            &Focus::default(),
        );

        let blocked_at = text
            .find("Landing the iOS fix")
            .expect("the blocked session");
        let working_at = text
            .find("Reclaiming disk space")
            .expect("the working session");
        assert!(
            blocked_at < working_at,
            "what needs the user comes first:\n{text}"
        );
        assert!(
            text.contains("Which branch should I deliver to?"),
            "the question itself is the point:\n{text}"
        );
    }

    #[test]
    fn nothing_waiting_says_so_plainly() {
        let text = digest(&snapshot(vec![], vec![]), NOW, &Focus::default());
        assert!(text.contains("Nothing needs you."), "{text}");
    }

    /// A headline that has stood for a long time may describe something
    /// finished long ago. Saying its age is more honest than repeating
    /// it as though it were current.
    #[test]
    fn a_stale_headline_is_given_its_age() {
        let mut stale = session(4, "Non-LLM learning", SessionState::Working);
        stale.headline = "Extending verified acceleration".into();
        stale.last_activity_at_unix_ms = NOW - STALE_HEADLINE_MS - 60_000;
        let text = digest(&snapshot(vec![stale], vec![]), NOW, &Focus::default());
        assert!(
            text.contains("last said anything"),
            "a stale line is dated:\n{text}"
        );
    }

    #[test]
    fn a_fresh_headline_is_not_dated() {
        let mut fresh = session(5, "Terminal scrollback", SessionState::Working);
        fresh.headline = "Running the focused lane".into();
        let text = digest(&snapshot(vec![fresh], vec![]), NOW, &Focus::default());
        assert!(!text.contains("last said anything"), "{text}");
        assert!(text.contains("Running the focused lane"), "{text}");
    }

    /// An ended session is history. Reading it out as though it were
    /// live would make the digest wrong rather than merely long.
    #[test]
    fn an_ended_session_is_left_out() {
        let mut ended = session(6, "Bundling master", SessionState::Exited);
        ended.ended_at_unix_ms = Some(NOW - 10_000);
        let text = digest(&snapshot(vec![ended], vec![]), NOW, &Focus::default());
        assert!(!text.contains("Bundling master"), "{text}");
    }

    /// Idle is where a session rests between turns, and the one a user
    /// is most likely to name. A bare count cannot be resolved back to
    /// anything to read about or stop, so every live session is listed.
    #[test]
    fn an_idle_session_is_named_and_carries_its_id() {
        let mut idle = session(4, "", SessionState::Idle);
        idle.task_title = "browser-e2e-two".to_string();
        idle.headline = "Waiting for the next instruction".to_string();
        let text = digest(&snapshot(vec![idle], vec![]), NOW, &Focus::default());
        assert!(text.contains("browser-e2e-two"), "{text}");
        assert!(text.contains("session 4"), "{text}");
        assert!(text.contains("Waiting for the next instruction"), "{text}");
    }

    /// Stopping a session that should not have begun is exactly what a
    /// user asks for while it is still starting.
    #[test]
    fn a_starting_session_is_named_too() {
        let mut starting = session(5, "", SessionState::AwaitingWorker);
        starting.task_title = "browser-e2e".to_string();
        let text = digest(&snapshot(vec![starting], vec![]), NOW, &Focus::default());
        assert!(text.contains("browser-e2e"), "{text}");
        assert!(text.contains("session 5"), "{text}");
    }

    /// Past a handful the names stop earning their tokens, but the count
    /// still has to be right.
    #[test]
    fn idle_sessions_past_the_listed_few_become_a_count() {
        let sessions = (1..=RUNNING_LISTED as u64 + 2)
            .map(|id| session(id, &format!("Idle work {id}"), SessionState::Idle))
            .collect();
        let text = digest(&snapshot(sessions, vec![]), NOW, &Focus::default());
        assert!(text.contains("Idle (8):"), "{text}");
        assert!(text.contains("- and 2 more idle"), "{text}");
    }

    /// Item numbers are only unique inside a bucket, so one said out
    /// loud is ambiguous. The title is what identifies an item.
    #[test]
    fn an_item_asking_something_is_named_by_title_not_number() {
        let asking = item(
            12,
            "Voice: iOS client",
            "Which voice should it use?",
            ItemStatus::Blocked,
        );
        let text = digest(&snapshot(vec![], vec![asking]), NOW, &Focus::default());
        assert!(text.contains("Voice: iOS client"), "{text}");
        assert!(text.contains("Which voice should it use?"), "{text}");
        assert!(
            !text.contains("item 12") && !text.contains("12:"),
            "no bucket-local number is spoken:\n{text}"
        );
    }

    #[test]
    fn a_closed_item_is_not_counted_as_open_work() {
        let done = item(1, "Shipped already", "", ItemStatus::Done);
        let planned = item(2, "Still to do", "", ItemStatus::Planned);
        let text = digest(
            &snapshot(vec![], vec![done, planned]),
            NOW,
            &Focus::default(),
        );
        assert!(text.contains("1 planned"), "{text}");
        assert!(
            !text.contains("done"),
            "a done item is not open work:\n{text}"
        );
    }

    /// Starting work is the one thing with no target of its own, so the
    /// digest says a project is needed rather than leaving it to be
    /// discovered when the spawn refuses. Anything aimed at a session
    /// carries its number already and needs no focus, so the line does
    /// not claim otherwise.
    #[test]
    fn an_unset_focus_is_stated_for_starting_work() {
        let text = digest(&snapshot(vec![], vec![]), NOW, &Focus::default());
        assert!(text.contains("No project is in focus"), "{text}");
        assert!(
            text.contains("starting new work"),
            "the line is about spawning, not every change:\n{text}"
        );
    }

    #[test]
    fn a_focused_project_is_named() {
        let focus = Focus {
            bucket_id: Some(1),
            project_id: Some(1),
            session_id: None,
        };
        let text = digest(&snapshot(vec![], vec![]), NOW, &focus);
        assert!(
            text.contains("Focused on the project puppet-master"),
            "{text}"
        );
    }

    #[test]
    fn many_working_sessions_become_a_count() {
        let working: Vec<Session> = (1..=RUNNING_LISTED as u64 + 3)
            .map(|id| session(id + 10, &format!("task {id}"), SessionState::Working))
            .collect();
        let text = digest(&snapshot(working, vec![]), NOW, &Focus::default());
        assert!(
            text.contains("3 more working"),
            "a listener cannot hold them all:\n{text}"
        );
    }

    #[test]
    fn how_long_ago_reads_the_way_somebody_would_say_it() {
        assert_eq!(ago(NOW, NOW), "just now");
        assert_eq!(ago(NOW, NOW - 90_000), "2 minutes ago");
        assert_eq!(ago(NOW, NOW - 3_600_000), "an hour ago");
        assert_eq!(ago(NOW, NOW - 3 * 3_600_000), "3 hours ago");
    }
}
