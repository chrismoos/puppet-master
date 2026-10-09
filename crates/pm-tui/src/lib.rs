//! Full-screen dashboard over the daemon socket: session and terminal
//! state, lifecycle controls, worker-aware spawn, and raw attach.

mod app;
mod attach;
mod keys;
mod run;
mod ui;
mod view;

pub use run::run;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
pub(crate) mod testutil {
    use pm_protocol::domain::{
        AgentKind, AgentSelectionSource, Bucket, PermissionMode, Project, Session, SessionState,
        Snapshot, Terminal, TerminalKind, TerminalRunState, Worker, LOCAL_WORKER_ID,
    };

    pub fn bucket(id: u64, name: &str) -> Bucket {
        Bucket {
            id,
            name: name.into(),
            position: id as u32,
            permission_mode: PermissionMode::Default,
            default_agent: None,
            model_profile_id: None,
            default_worker_id: 0,
            allowed_worker_ids: vec![0],
            is_default: id == 1,
        }
    }

    pub fn project(id: u64, bucket_id: u64, name: &str) -> Project {
        Project {
            id,
            bucket_id,
            name: name.into(),
            path: format!("/projects/{name}"),
            permission_mode: PermissionMode::Inherit,
            default_agent: None,
            model_profile_id: None,
            worker_id: None,
            allowed_worker_ids: vec![0],
            worker_paths: Vec::new(),
        }
    }

    pub fn session(id: u64, project_id: u64, state: SessionState) -> Session {
        Session {
            id,
            project_id,
            agent: AgentKind::ClaudeCode,
            agent_source: AgentSelectionSource::Explicit,
            state,
            git: None,
            task_title: format!("task {id}"),
            task_prompt: String::new(),
            agent_session_id: None,
            created_at_unix_ms: 0,
            ended_at_unix_ms: None,
            exit_code: None,
            state_detail: String::new(),
            activity: String::new(),
            progress_percent: None,
            resumable: false,
            permission_mode: PermissionMode::Default,
            worker_id: 0,
            cwd: "/projects".into(),
            goal: String::new(),
            headline: String::new(),
            summary: String::new(),
            items_api: true,
            supervisor_api: false,
            role: pm_protocol::domain::SessionRole::Worker,
            spawned_by_session_id: None,
            last_activity_at_unix_ms: 0,
            last_agent_activity_at_unix_ms: 0,
            last_user_interaction_at_unix_ms: 0,
            needs_input_unseen: state == SessionState::NeedsInput,
            idle_unseen: false,
            model_profile_id: None,
            model_profile_source: None,
            program_status: Vec::new(),
        }
    }

    pub fn snapshot(
        buckets: Vec<Bucket>,
        projects: Vec<Project>,
        sessions: Vec<Session>,
    ) -> Snapshot {
        let terminals = sessions
            .iter()
            .map(|session| agent_terminal(session.id, session.state.is_live()))
            .collect();
        Snapshot {
            reviews: Vec::new(),
            review_viewer_states: Vec::new(),
            plans: Vec::new(),
            buckets,
            projects,
            sessions,
            workers: vec![Worker {
                id: LOCAL_WORKER_ID,
                name: "local".into(),
                hostname: "localhost".into(),
                platform: "test".into(),
                online: true,
                default_project_root: "/projects".into(),
                last_seen_at_unix_ms: None,
                pm_version: String::new(),
                runtime: String::new(),
                container: String::new(),
                connect_mode: Default::default(),
                endpoint: String::new(),
            }],
            terminals,
            contexts: Vec::new(),
            forwards: Vec::new(),
            items: Vec::new(),
            briefings: Vec::new(),
            user_settings: Vec::new(),
            model_profiles: Vec::new(),
            agent_dialects: Vec::new(),
            instruction_layers: Vec::new(),
        }
    }

    pub fn agent_terminal(session_id: u64, live: bool) -> Terminal {
        Terminal {
            id: 1_000 + session_id,
            session_id,
            kind: TerminalKind::Agent,
            title: "Agent".into(),
            cwd: "/projects".into(),
            created_at_unix_ms: 0,
            generation: 1,
            state: if live {
                TerminalRunState::Running
            } else {
                TerminalRunState::Exited
            },
            started_at_unix_ms: Some(0),
            ended_at_unix_ms: (!live).then_some(1),
            exit_code: None,
            scrollback_available: !live,
        }
    }

    pub fn shell_terminal(id: u64, session_id: u64, live: bool) -> Terminal {
        let mut terminal = agent_terminal(session_id, live);
        terminal.id = id;
        terminal.kind = TerminalKind::Shell;
        terminal.title = format!("shell {id}");
        terminal
    }
}
