//! What the voice agent is allowed to do, and the two rules that keep a
//! global surface safe.
//!
//! Reads are global: the user can ask about anything, anywhere, because
//! the dashboard already shows all of it. Writes need a focus resolved
//! first, because across many projects a guess acts on the wrong one.
//!
//! Anything that disturbs a running session needs the user to confirm it
//! out loud. The confirmation is minted by the first call and can only
//! be spent after a later user utterance, so the model cannot talk
//! itself into acting: the rule is enforced here rather than asked for
//! in a prompt.

use pm_protocol::domain::{PermissionMode, RespondTarget, Session, Snapshot};
use serde_json::{json, Value};

use super::digest::Focus;
use super::model::ToolCall;
use crate::daemon::{Daemon, UserMessage};

/// How many sessions one listing returns by default, and the most it
/// will return however large a limit is asked for. A longer answer is
/// unusable read aloud, and the model can narrow the query instead.
const LISTED_DEFAULT: u64 = 6;
const LISTED_MAX: u64 = 12;

/// Names the tools whose effect the user has to confirm by voice.
fn disturbs_a_session(name: &str, input: &Value) -> bool {
    match name {
        "interrupt_session" | "kill_session" => true,
        // Queueing a message waits for the agent's own break. Asking to
        // reach it mid-turn does not.
        "message_session" => input.get("steer").and_then(Value::as_bool).unwrap_or(false),
        _ => false,
    }
}

/// A confirmation the user has been asked for but has not yet given.
#[derive(Debug, Clone)]
pub struct Pending {
    pub token: String,
    /// What the agent was told to say, so a second call can be checked
    /// against the thing the user actually heard.
    pub action: String,
    /// The turn it was minted on. Spending it needs a later one, which
    /// is what makes the user's voice the thing that authorizes it.
    pub minted_on_turn: u64,
}

/// Everything the tools act on for one conversation.
pub struct VoiceState {
    pub focus: Focus,
    pub pending: Option<Pending>,
    pub turn: u64,
    /// Messages whose answers the user asked to hear, drained by the
    /// caller after each round so a reply arriving later can be routed
    /// back to this conversation.
    pub awaiting_replies: Vec<u64>,
    /// What the user said this turn, which is what gets forwarded to an
    /// agent verbatim rather than a rephrasing of it.
    pub said: String,
    next_token: u64,
}

impl Default for VoiceState {
    fn default() -> Self {
        Self {
            focus: Focus::default(),
            pending: None,
            turn: 0,
            awaiting_replies: Vec::new(),
            said: String::new(),
            next_token: 1,
        }
    }
}

impl VoiceState {
    fn mint(&mut self, action: &str) -> String {
        let token = format!("confirm-{}", self.next_token);
        self.next_token += 1;
        self.pending = Some(Pending {
            token: token.clone(),
            action: action.to_string(),
            minted_on_turn: self.turn,
        });
        token
    }

    /// Whether a token authorizes acting now. A token minted on this
    /// same turn does not: the user has not spoken since, so nobody has
    /// agreed to anything.
    fn may_spend(&self, token: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.token == token && p.minted_on_turn < self.turn)
    }
}

/// The catalog handed to the model. Descriptions carry the rules the
/// daemon enforces anyway, so the model asks correctly rather than
/// discovering the refusals.
pub fn catalog() -> Value {
    json!([
        {
            "name": "set_focus",
            "description": "Choose where new work goes. Needed before spawning, which has no \
                target otherwise. Anything aimed at a session you have the number for does not \
                need this. Name a project or a session, not both.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Project name, as the digest shows it" },
                    "session": { "type": "integer", "description": "Session id" }
                }
            }
        },
        {
            "name": "session_detail",
            "description": "What one session reports about itself: its goal, headline, summary \
                and where it sits in git. Never returns terminal output, which is unusable \
                read aloud.",
            "input_schema": {
                "type": "object",
                "properties": { "session": { "type": "integer" } },
                "required": ["session"]
            }
        },
        {
            "name": "list_sessions",
            "description": "Find sessions by what they are doing. The digest lists only a few \
                per state, so use this whenever the user names work the digest does not show, \
                or asks about something across more sessions than it lists. Matches the goal, \
                title, headline, summary and project name.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words the user used for the work, such as a project or feature name. Omit for everything." },
                    "include_ended": { "type": "boolean", "description": "Also return sessions that have finished" },
                    "limit": { "type": "integer", "description": "How many to return, most recently active first" }
                }
            }
        },
        {
            "name": "answer_session",
            "description": "Answer the question a session is waiting on, so its agent carries \
                on. Use this for a session the digest says needs the user. To say anything \
                else to an agent, use message_session instead.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer" },
                    "text": { "type": "string", "description": "The user's answer, in their words" }
                },
                "required": ["session", "text"]
            }
        },
        {
            "name": "spawn_session",
            "description": "Start a session on the focused project. Set the focus first: with \
                several projects configured, guessing starts work in the wrong repository.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "The task the agent starts with" },
                    "title": { "type": "string", "description": "One short line for the session list" }
                },
                "required": ["prompt"]
            }
        },
        {
            "name": "message_session",
            "description": "Say something to a session's agent. The delivery field says what \
                became of it. When it starts straight away there is nothing to report, so say \
                nothing about delivery. Anything else means what the user asked for has not \
                happened yet, so say that much in a few words without explaining the channel. \
                Set reply whenever they want to hear what the agent says back: it is read out \
                on its own when the agent answers, so finish your turn rather than waiting for \
                it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer" },
                    "text": { "type": "string", "description": "Leave this out to send exactly what the user said, which is almost always what they want. Set it only when their words would mean nothing to the agent on their own." },
                    "reply": { "type": "boolean", "description": "Ask for one answer back" },
                    "steer": { "type": "boolean", "description": "Try to reach the agent mid-turn. Needs confirmation" },
                    "confirm_token": { "type": "string", "description": "The token from the refusal, once the user has agreed" }
                },
                "required": ["session"]
            }
        },
        {
            "name": "interrupt_session",
            "description": "Stop what a session is doing now, like pressing escape. The turn \
                is discarded rather than redirected. Needs the user to confirm.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer" },
                    "confirm_token": { "type": "string" }
                },
                "required": ["session"]
            }
        },
        {
            "name": "kill_session",
            "description": "End a session. Needs the user to confirm.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer" },
                    "confirm_token": { "type": "string" }
                },
                "required": ["session"]
            }
        }
    ])
}

fn refuse(detail: impl Into<String>) -> Value {
    json!({ "ok": false, "error": detail.into() })
}

fn done(detail: impl Into<String>) -> Value {
    json!({ "ok": true, "detail": detail.into() })
}

fn session_of(snapshot: &Snapshot, id: u64) -> Option<&Session> {
    snapshot.sessions.iter().find(|session| session.id == id)
}

/// Runs one tool call. Every refusal says what to do instead, because
/// the model's next move is to say it to the user.
pub async fn call(
    daemon: &std::sync::Arc<Daemon>,
    state: &mut VoiceState,
    snapshot: &Snapshot,
    tool: &ToolCall,
) -> Value {
    let input = &tool.input;
    let integer = |key: &str| input.get(key).and_then(Value::as_u64);
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    if disturbs_a_session(&tool.name, input) {
        let token = text("confirm_token");
        if token.is_empty() || !state.may_spend(&token) {
            let Some(session_id) = integer("session") else {
                return refuse("name the session by its id");
            };
            let goal = session_of(snapshot, session_id)
                .map(|session| {
                    let named = session.goal.trim();
                    if named.is_empty() {
                        format!("session {session_id}")
                    } else {
                        named.to_string()
                    }
                })
                .unwrap_or_else(|| format!("session {session_id}"));
            let action = format!("{} on {goal}", tool.name.replace('_', " "));
            let minted = state.mint(&action);
            return json!({
                "ok": false,
                "needs_confirmation": true,
                "confirm_token": minted,
                // Naming the goal rather than the id is what lets the
                // user catch a misheard target before anything happens.
                "say_to_user": format!(
                    "Ask the user to confirm {action}, naming it exactly that way, and call \
                     this again with the token once they agree."
                ),
            });
        }
        state.pending = None;
    }

    match tool.name.as_str() {
        "set_focus" => {
            if let Some(session_id) = integer("session") {
                let Some(session) = session_of(snapshot, session_id) else {
                    return refuse(format!("there is no session {session_id}"));
                };
                state.focus = Focus {
                    bucket_id: None,
                    project_id: Some(session.project_id),
                    session_id: Some(session_id),
                };
                return done(format!("focused on session {session_id}"));
            }
            let wanted = text("project");
            if wanted.trim().is_empty() {
                return refuse("name a project or a session to focus on");
            }
            let needle = wanted.trim().to_lowercase();
            let matches: Vec<_> = snapshot
                .projects
                .iter()
                .filter(|project| project.name.to_lowercase().contains(&needle))
                .collect();
            match matches.as_slice() {
                [project] => {
                    state.focus = Focus {
                        bucket_id: Some(project.bucket_id),
                        project_id: Some(project.id),
                        session_id: None,
                    };
                    done(format!("focused on the project {}", project.name))
                }
                [] => refuse(format!(
                    "no project matches {wanted:?}. The projects are: {}",
                    project_names(snapshot)
                )),
                several => refuse(format!(
                    "{wanted:?} matches {}. Ask the user which one.",
                    several
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        "session_detail" => {
            let Some(id) = integer("session") else {
                return refuse("name the session by its id");
            };
            let Some(session) = session_of(snapshot, id) else {
                return refuse(format!("there is no session {id}"));
            };
            json!({
                "ok": true,
                "session": id,
                "goal": session.goal,
                "headline": session.headline,
                "summary": session.summary,
                "state": format!("{:?}", session.state),
                "state_detail": session.state_detail,
                "git": session.git.as_ref().map(|git| json!({
                    "branch": git.branch,
                    "worktree": git.worktree,
                })),
            })
        }
        "list_sessions" => {
            let wanted = text("query");
            let include_ended = input
                .get("include_ended")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let limit = integer("limit")
                .unwrap_or(LISTED_DEFAULT)
                .clamp(1, LISTED_MAX) as usize;
            let found = select_sessions(snapshot, wanted.trim(), include_ended);
            if found.is_empty() {
                return refuse(format!(
                    "no session matches {:?}. Ask the user which work they mean.",
                    wanted.trim()
                ));
            }
            let lines: Vec<String> = found
                .iter()
                .take(limit)
                .map(|session| describe(snapshot, session))
                .collect();
            json!({ "ok": true, "matched": found.len(), "sessions": lines })
        }
        "answer_session" => {
            let Some(session_id) = integer("session") else {
                return refuse("name the session by its id");
            };
            let answer = text("text");
            if answer.trim().is_empty() {
                return refuse("there is nothing to answer with");
            }
            if session_of(snapshot, session_id).is_none() {
                return refuse(format!("there is no session {session_id}"));
            }
            // The question lives on a board item, and answering it there
            // is what releases the agent that is waiting on it.
            let Some(item) = snapshot.items.iter().find(|item| {
                item.session_ids.contains(&session_id) && !item.question.trim().is_empty()
            }) else {
                return refuse(format!(
                    "session {session_id} is not waiting on a question, so there is nothing to \
                     answer. Use message_session to say something to its agent."
                ));
            };
            match daemon
                .respond_to_item(
                    item.bucket_id,
                    item.id,
                    &answer,
                    RespondTarget::Session(session_id),
                )
                .await
            {
                Ok(_) => done(format!("answered {:?}", clamp_title(&item.title))),
                Err(e) => refuse(e.to_string()),
            }
        }
        "spawn_session" => {
            let Some(project_id) = state.focus.project_id else {
                return refuse(format!(
                    "no project is focused, so there is nowhere to start this. Ask the user \
                     which project they mean. The projects are: {}",
                    project_names(snapshot)
                ));
            };
            let prompt = text("prompt");
            if prompt.trim().is_empty() {
                return refuse("there is no task to start");
            }
            let title = text("title");
            if let Err(e) = daemon.ensure_spawnable(project_id, None, None).await {
                return refuse(e.to_string());
            }
            let daemon = std::sync::Arc::clone(daemon);
            let spawned = tokio::task::spawn_blocking(move || {
                daemon.spawn_session_with_agent_override(
                    project_id,
                    None,
                    &title,
                    &prompt,
                    None,
                    PermissionMode::Inherit,
                    None,
                    true,
                    false,
                    None,
                    None,
                    None,
                )
            })
            .await;
            match spawned {
                Ok(Ok(id)) => json!({ "ok": true, "session": id, "detail": "started" }),
                Ok(Err(e)) => refuse(e.to_string()),
                Err(e) => refuse(format!("the spawn did not complete: {e}")),
            }
        }
        "message_session" => {
            let Some(id) = integer("session") else {
                return refuse("name the session by its id");
            };
            // The user's own words by default: a rephrasing loses what
            // they actually asked, and the answer is read back to them
            // rather than to the model.
            let asked = text("text");
            let body = if asked.trim().is_empty() {
                state.said.clone()
            } else {
                asked
            };
            if body.trim().is_empty() {
                return refuse("there is nothing to say");
            }
            let steer = input.get("steer").and_then(Value::as_bool).unwrap_or(false);
            let wants_reply = input.get("reply").and_then(Value::as_bool).unwrap_or(false);
            let outcome = daemon
                .send_user_message(
                    id,
                    UserMessage {
                        text: &body,
                        reply: wants_reply,
                        mode: if steer {
                            pm_protocol::domain::AgentInboxMode::Steer
                        } else {
                            pm_protocol::domain::AgentInboxMode::Queue
                        },
                        idempotency_key: "",
                        voice: true,
                    },
                )
                .await;
            match outcome {
                Ok(sent) => {
                    // Only a message that was actually recorded can be
                    // answered, so only that one is worth waiting on.
                    if let (true, Some(message)) = (wants_reply, sent.message_id) {
                        state.awaiting_replies.push(message);
                    }
                    json!({
                        "ok": true,
                        "message": sent.message_id,
                        // One field, saying what the user would notice
                        // rather than how the channel carried it. A
                        // queued message to an idle agent starts a turn
                        // at once, which is nothing to report.
                        "delivery": delivery(sent.input_state),
                        "answer_follows": wants_reply,
                    })
                }
                Err(e) => refuse(e.to_string()),
            }
        }
        "interrupt_session" => {
            let Some(id) = integer("session") else {
                return refuse("name the session by its id");
            };
            match daemon.interrupt_session(id) {
                Ok(_) => done(format!("interrupted session {id}")),
                Err(e) => refuse(e.to_string()),
            }
        }
        "kill_session" => {
            let Some(id) = integer("session") else {
                return refuse("name the session by its id");
            };
            match daemon.kill_session(id) {
                Ok(_) => done(format!("ended session {id}")),
                Err(e) => refuse(e.to_string()),
            }
        }
        other => refuse(format!("there is no tool called {other:?}")),
    }
}

/// The sessions matching what the user called the work, most recently
/// active first. An empty query matches everything.
fn select_sessions<'a>(
    snapshot: &'a Snapshot,
    query: &str,
    include_ended: bool,
) -> Vec<&'a Session> {
    let needle = query.to_lowercase();
    let project_of = |id: u64| {
        snapshot
            .projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.name.as_str())
            .unwrap_or("")
    };
    let mut found: Vec<&Session> = snapshot
        .sessions
        .iter()
        .filter(|session| include_ended || session.ended_at_unix_ms.is_none())
        .filter(|session| {
            needle.is_empty()
                || [
                    session.goal.as_str(),
                    session.task_title.as_str(),
                    session.headline.as_str(),
                    session.summary.as_str(),
                    project_of(session.project_id),
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(&needle))
        })
        .collect();
    found.sort_by_key(|session| std::cmp::Reverse(session.last_activity_at_unix_ms));
    found
}

/// One session in a line somebody could listen to.
fn describe(snapshot: &Snapshot, session: &Session) -> String {
    let where_it_runs = snapshot
        .projects
        .iter()
        .find(|project| project.id == session.project_id)
        .map(|project| format!(" in {}", project.name))
        .unwrap_or_default();
    let headline = session.headline.trim();
    format!(
        "{}{} is {}{}",
        super::digest::label(session),
        where_it_runs,
        session.state.as_str(),
        if headline.is_empty() {
            String::new()
        } else {
            format!(": {headline}")
        }
    )
}

/// What the user would notice about a message's delivery, from what the
/// send actually reported. Inferring it from whether the agent looked
/// busy claims success for a submission that was never confirmed.
fn delivery(input_state: &str) -> &'static str {
    match input_state {
        "steered" | "submitted" | "submission_requested" => "starts straight away",
        "queued_behind_turn" | "input_queued" => "waits for the turn in flight to end",
        "already_sent" => "was already sent, so it was not said twice",
        "not_delivered" => "did not reach the agent",
        // Covers submission_unconfirmed and submit_undelivered, where
        // the text landed but no turn was seen to begin.
        _ => "may not have reached the agent",
    }
}

/// Keeps a title short enough to say back.
fn clamp_title(title: &str) -> String {
    super::digest::clamp(title)
}

fn project_names(snapshot: &Snapshot) -> String {
    snapshot
        .projects
        .iter()
        .map(|project| project.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    use pm_protocol::domain::{
        AgentKind, AgentSelectionSource, Project, SessionRole, SessionState,
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

    fn snapshot(sessions: Vec<Session>) -> Snapshot {
        Snapshot {
            projects: vec![Project {
                id: 1,
                bucket_id: 1,
                name: "iden".into(),
                path: "/srv/iden".into(),
                permission_mode: PermissionMode::Inherit,
                default_agent: None,
                model_profile_id: None,
                worker_id: None,
                allowed_worker_ids: Vec::new(),
                worker_paths: Vec::new(),
            }],
            sessions,
            ..Snapshot::default()
        }
    }

    /// The catalog is what the model is allowed to do, so it is pinned.
    /// Nothing addresses a board item by title: a title said out loud is
    /// a worse handle than the session doing the work.
    #[test]
    fn the_catalog_is_exactly_these_tools() {
        let names: Vec<String> = catalog()
            .as_array()
            .expect("the catalog is a list")
            .iter()
            .map(|tool| {
                tool["name"]
                    .as_str()
                    .expect("every tool is named")
                    .to_string()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "set_focus",
                "session_detail",
                "list_sessions",
                "answer_session",
                "spawn_session",
                "message_session",
                "interrupt_session",
                "kill_session",
            ]
        );
    }

    /// The user names work the way they talk about it, which may be the
    /// project, the goal, or whatever the session last reported.
    #[test]
    fn work_is_found_by_the_words_the_user_would_use() {
        let mut headlined = session(2, "", SessionState::Working);
        headlined.headline = "Checking in phone one thousand".into();
        let snap = snapshot(vec![
            session(1, "Porting the iden stack", SessionState::Idle),
            headlined,
            session(3, "Something unrelated", SessionState::Idle),
        ]);

        let by_goal = select_sessions(&snap, "iden", false);
        assert_eq!(by_goal.len(), 3, "the project is called iden, so all three");

        let by_headline = select_sessions(&snap, "phone one thousand", false);
        assert_eq!(
            by_headline.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![2]
        );

        assert!(select_sessions(&snap, "nothing like this", false).is_empty());
    }

    /// Asking what happened to finished work is a fair question, but it
    /// is not what "what is running" means.
    #[test]
    fn a_finished_session_is_left_out_unless_it_is_asked_for() {
        let mut ended = session(9, "Shipped yesterday", SessionState::Exited);
        ended.ended_at_unix_ms = Some(NOW - 10_000);
        let snap = snapshot(vec![ended]);
        assert!(select_sessions(&snap, "shipped", false).is_empty());
        assert_eq!(select_sessions(&snap, "shipped", true).len(), 1);
    }

    /// With several matches the one the user means is almost always the
    /// one that moved last.
    #[test]
    fn the_most_recently_active_match_comes_first() {
        let mut old = session(1, "iden one", SessionState::Idle);
        old.last_activity_at_unix_ms = NOW - 600_000;
        let mut recent = session(2, "iden two", SessionState::Idle);
        recent.last_activity_at_unix_ms = NOW - 1_000;
        let snap = snapshot(vec![old, recent]);
        assert_eq!(
            select_sessions(&snap, "iden", false)
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    /// A line has to carry the id, because that is what the agent acts
    /// on after the user picks one out loud.
    #[test]
    fn a_described_session_carries_its_id_and_what_it_is_doing() {
        let mut one = session(4, "Porting the iden stack", SessionState::Working);
        one.headline = "Checking in phone one thousand".into();
        let snap = snapshot(vec![one]);
        let line = describe(&snap, &snap.sessions[0]);
        assert!(line.contains("session 4"), "{line}");
        assert!(line.contains("Porting the iden stack"), "{line}");
        assert!(line.contains("in iden"), "{line}");
        assert!(line.contains("Checking in phone one thousand"), "{line}");
    }

    /// A queue only matters when something is already running to queue
    /// behind. Saying "it was queued" about an idle agent reports a
    /// mechanism the user did not ask about and cannot act on.
    #[test]
    fn delivery_is_only_worth_reporting_when_something_is_wrong() {
        assert_eq!(delivery("submitted"), "starts straight away");
        assert_eq!(delivery("steered"), "starts straight away");
        assert_eq!(
            delivery("queued_behind_turn"),
            "waits for the turn in flight to end"
        );
    }

    /// A submission nobody saw begin is the case that misled a user: the
    /// text was typed, no turn started, and the agent said it had asked.
    #[test]
    fn an_unconfirmed_submission_is_never_called_delivered() {
        for state in [
            "submission_unconfirmed",
            "submit_undelivered",
            "anything new",
        ] {
            assert_eq!(
                delivery(state),
                "may not have reached the agent",
                "{state} is not a delivery"
            );
        }
        assert_eq!(delivery("not_delivered"), "did not reach the agent");
    }

    /// Answering the question a session is waiting on is what it is
    /// waiting for, so it is not a disturbance.
    #[test]
    fn looking_up_and_answering_need_no_confirming() {
        assert!(!disturbs_a_session("list_sessions", &json!({})));
        assert!(!disturbs_a_session("answer_session", &json!({})));
    }

    #[test]
    fn stopping_a_session_always_needs_confirming() {
        assert!(disturbs_a_session("kill_session", &json!({})));
        assert!(disturbs_a_session("interrupt_session", &json!({})));
    }

    /// Queueing a message waits for the agent's own break, so it
    /// disturbs nothing. Asking to reach it mid-turn does.
    #[test]
    fn only_a_steered_message_needs_confirming() {
        assert!(!disturbs_a_session("message_session", &json!({})));
        assert!(!disturbs_a_session(
            "message_session",
            &json!({ "steer": false })
        ));
        assert!(disturbs_a_session(
            "message_session",
            &json!({ "steer": true })
        ));
    }

    #[test]
    fn reading_never_needs_confirming() {
        assert!(!disturbs_a_session("session_detail", &json!({})));
        assert!(!disturbs_a_session("set_focus", &json!({})));
        assert!(!disturbs_a_session("spawn_session", &json!({})));
    }

    /// The rule that makes the user's voice the thing that authorizes an
    /// action: a token minted while answering one utterance cannot be
    /// spent on that same utterance, because nobody has agreed yet.
    #[test]
    fn a_token_cannot_be_spent_on_the_turn_it_was_minted() {
        let mut state = VoiceState {
            turn: 1,
            ..VoiceState::default()
        };
        let token = state.mint("kill session on Landing the iOS fix");
        assert!(
            !state.may_spend(&token),
            "the model would be confirming to itself"
        );
    }

    #[test]
    fn a_token_is_spendable_once_the_user_has_spoken_again() {
        let mut state = VoiceState {
            turn: 1,
            ..VoiceState::default()
        };
        let token = state.mint("kill session on Landing the iOS fix");
        state.turn = 2;
        assert!(state.may_spend(&token));
    }

    #[test]
    fn a_token_that_was_never_minted_is_refused() {
        let mut state = VoiceState {
            turn: 1,
            ..VoiceState::default()
        };
        state.mint("kill session on Landing the iOS fix");
        state.turn = 2;
        assert!(!state.may_spend("confirm-made-up"));
    }

    /// A new confirmation replaces the old one, so agreeing to the
    /// second thing cannot authorize the first.
    #[test]
    fn minting_again_invalidates_the_previous_token() {
        let mut state = VoiceState {
            turn: 1,
            ..VoiceState::default()
        };
        let first = state.mint("kill session on one thing");
        state.turn = 2;
        let second = state.mint("kill session on another thing");
        state.turn = 3;
        assert!(!state.may_spend(&first));
        assert!(state.may_spend(&second));
    }
}
