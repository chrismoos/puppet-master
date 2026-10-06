//! Dashboard state: a reducer over Snapshot + Events plus the
//! interaction state machine (selection, spawn form, kill confirm).
//! Everything here is pure so it can be unit tested without a terminal.

use pm_protocol::domain::{
    AgentKind, Bucket, ClientMsg, Event, PermissionMode, Project, Session, SessionContext,
    SessionState, Snapshot, Terminal, TerminalKind, Worker,
};

use crate::keys::Key;
use crate::view::{self, ProjectChoice, WorkerChoice};

/// Spawn titles default to the prompt truncated to this many
/// characters, matching `pm spawn`.
pub const TITLE_MAX_CHARS: usize = 60;

#[derive(Default)]
pub struct World {
    pub buckets: Vec<Bucket>,
    pub projects: Vec<Project>,
    pub sessions: Vec<Session>,
    pub workers: Vec<Worker>,
    pub terminals: Vec<Terminal>,
    pub contexts: Vec<SessionContext>,
    initialized: bool,
}

impl World {
    pub fn bucket(&self, id: u64) -> Option<&Bucket> {
        self.buckets.iter().find(|b| b.id == id)
    }

    pub fn project(&self, id: u64) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn session(&self, id: u64) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn worker(&self, id: u64) -> Option<&Worker> {
        self.workers.iter().find(|w| w.id == id)
    }

    pub fn terminal(&self, id: u64) -> Option<&Terminal> {
        self.terminals.iter().find(|terminal| terminal.id == id)
    }

    pub fn terminals_for_session(&self, session_id: u64) -> Vec<&Terminal> {
        let mut terminals: Vec<_> = self
            .terminals
            .iter()
            .filter(|terminal| terminal.session_id == session_id)
            .collect();
        terminals.sort_by_key(|terminal| (terminal.kind != TerminalKind::Agent, terminal.id));
        terminals
    }

    pub fn context(&self, session_id: u64) -> Option<&SessionContext> {
        self.contexts.iter().find(|c| c.session_id == session_id)
    }

    fn newly_needs_input(&self, incoming: &Session) -> bool {
        incoming.state == SessionState::NeedsInput
            && self
                .session(incoming.id)
                .is_none_or(|old| old.state != SessionState::NeedsInput)
    }

    /// Replaces all state. Returns true when a session is newly in
    /// needs-input relative to what was previously known; the very
    /// first snapshot never counts, since nothing transitioned while
    /// the dashboard was open.
    fn apply_snapshot(&mut self, snapshot: Snapshot) -> bool {
        let newly = self.initialized && snapshot.sessions.iter().any(|s| self.newly_needs_input(s));
        self.buckets = snapshot.buckets;
        self.buckets.sort_by_key(|b| (b.position, b.id));
        self.projects = snapshot.projects;
        self.projects.sort_by_key(|p| p.id);
        self.sessions = snapshot.sessions;
        self.sessions.sort_by_key(|s| s.id);
        self.workers = snapshot.workers;
        self.workers.sort_by_key(|w| w.id);
        self.terminals = snapshot.terminals;
        self.terminals.sort_by_key(|t| t.id);
        self.contexts = snapshot.contexts;
        self.contexts.sort_by_key(|c| c.session_id);
        self.initialized = true;
        newly
    }

    /// Returns true when the event moves a session into needs-input.
    fn apply_event(&mut self, event: Event) -> bool {
        match event {
            Event::SessionChanged(s) => {
                let newly = self.newly_needs_input(&s);
                upsert(&mut self.sessions, s, |s| s.id);
                self.sessions.sort_by_key(|s| s.id);
                newly
            }
            Event::SessionRemoved(id) => {
                self.sessions.retain(|s| s.id != id);
                false
            }
            // The terminal client offers no model-profile management.
            Event::ModelProfileChanged(_) | Event::ModelProfileRemoved(_) => false,
            // The TUI lists sessions, not reviews; reviews are read in
            // the browser where a diff can actually be rendered.
            Event::ReviewChanged(_) | Event::ReviewRemoved(_) => false,
            Event::PlanChanged(_) | Event::PlanRemoved(_) => false,
            // The TUI still reads needs-input off the session change above,
            // so the daemon's alert would double-count here.
            Event::SessionAlert(_) => false,
            // The TUI has no notice band to raise one in. The daemon logs
            // every notice, which is where a terminal-only operator reads them.
            Event::SecurityNotice(_) => false,
            Event::BucketChanged(b) => {
                upsert(&mut self.buckets, b, |b| b.id);
                self.buckets.sort_by_key(|b| (b.position, b.id));
                false
            }
            Event::BucketRemoved(id) => {
                self.buckets.retain(|b| b.id != id);
                false
            }
            Event::ProjectChanged(p) => {
                upsert(&mut self.projects, p, |p| p.id);
                self.projects.sort_by_key(|p| p.id);
                false
            }
            Event::ProjectRemoved(id) => {
                self.projects.retain(|p| p.id != id);
                false
            }
            Event::WorkerChanged(w) => {
                upsert(&mut self.workers, w, |w| w.id);
                self.workers.sort_by_key(|w| w.id);
                false
            }
            Event::WorkerRemoved(id) => {
                self.workers.retain(|w| w.id != id);
                false
            }
            Event::TerminalChanged(t) => {
                upsert(&mut self.terminals, t, |t| t.id);
                self.terminals.sort_by_key(|t| t.id);
                false
            }
            Event::TerminalRemoved(id) => {
                self.terminals.retain(|t| t.id != id);
                false
            }
            Event::ContextChanged(c) => {
                if c.glance.is_empty() && c.detail.is_empty() {
                    self.contexts.retain(|x| x.session_id != c.session_id);
                } else {
                    upsert(&mut self.contexts, c, |c| c.session_id);
                    self.contexts.sort_by_key(|c| c.session_id);
                }
                false
            }
            // Forwards render in the web UI and `pm forwards`, not here.
            Event::ForwardChanged(_) | Event::ForwardRemoved(_) => false,
            // The item board renders in the web UI and `pm items`; TUI
            // parity is a follow-up.
            Event::ItemChanged(_)
            | Event::ItemRemoved(_)
            | Event::BriefingChanged(_)
            | Event::UserSettingChanged(_)
            | Event::InstructionLayerChanged(_) => false,
        }
    }
}

fn upsert<T>(items: &mut Vec<T>, item: T, id: impl Fn(&T) -> u64) {
    match items.iter_mut().find(|x| id(x) == id(&item)) {
        Some(slot) => *slot = item,
        None => items.push(item),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Quit,
    Attach(AttachTarget),
    Request(ClientMsg),
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachTarget {
    pub session_id: u64,
    pub terminal_id: u64,
}

pub enum Mode {
    List,
    Spawn(SpawnForm),
    ConfirmKill(u64),
    ConfirmClose(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnFocus {
    Project,
    Worker,
    Cwd,
    Agent,
    Role,
    Permission,
    Title,
    Prompt,
}

pub struct SpawnForm {
    pub projects: Vec<ProjectChoice>,
    pub project_idx: usize,
    pub workers: Vec<WorkerChoice>,
    pub worker_idx: usize,
    pub cwd: String,
    pub agent: AgentKind,
    pub role: pm_protocol::domain::SessionRole,
    pub permission_mode: PermissionMode,
    pub title: String,
    pub prompt: String,
    pub focus: SpawnFocus,
}

pub struct Status {
    pub text: String,
    pub error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Connected,
    Reconnecting { attempt: u32 },
}

pub struct App {
    pub world: World,
    pub selected: Option<u64>,
    pub selected_terminal: Option<u64>,
    pub mode: Mode,
    pub status: Option<Status>,
    pub connection: Connection,
    bell_pending: bool,
}

impl App {
    pub fn new() -> Self {
        App {
            world: World::default(),
            selected: None,
            selected_terminal: None,
            mode: Mode::List,
            status: None,
            connection: Connection::Reconnecting { attempt: 1 },
            bell_pending: false,
        }
    }

    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        let prev_index = self.selected_index();
        if self.world.apply_snapshot(snapshot) {
            self.bell_pending = true;
        }
        self.fix_selection(prev_index);
        self.fix_terminal_selection();
    }

    pub fn apply_event(&mut self, event: Event) {
        let prev_index = self.selected_index();
        if self.world.apply_event(event) {
            self.bell_pending = true;
        }
        self.fix_selection(prev_index);
        self.fix_terminal_selection();
    }

    /// True at most once per needs-input transition; the caller rings
    /// the terminal bell.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell_pending)
    }

    pub fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some(Status {
            text: text.into(),
            error,
        });
    }

    pub fn on_key(&mut self, key: Key) -> Option<Cmd> {
        match self.mode {
            Mode::List => self.on_key_list(key),
            Mode::Spawn(_) => self.on_key_spawn(key),
            Mode::ConfirmKill(_) | Mode::ConfirmClose(_) => self.on_key_confirm(key),
        }
    }

    fn on_key_list(&mut self, key: Key) -> Option<Cmd> {
        match key {
            Key::Char('q') | Key::CtrlC => Some(Cmd::Quit),
            Key::Char('j') | Key::Down => {
                self.move_selection(1);
                None
            }
            Key::Char('k') | Key::Up => {
                self.move_selection(-1);
                None
            }
            Key::Char('h') | Key::Left => {
                self.move_terminal_selection(-1);
                None
            }
            Key::Char('l') | Key::Right => {
                self.move_terminal_selection(1);
                None
            }
            Key::Enter => self.attach_selected_terminal(),
            Key::Char('n') => self.selected.map(|session_id| {
                Cmd::Request(ClientMsg::CreateShell {
                    session_id,
                    title: String::new(),
                })
            }),
            Key::Char('R') => self.restart_selected_terminal(),
            Key::Char('x') => {
                if let Some(terminal) = self
                    .selected_terminal()
                    .filter(|terminal| terminal.kind == TerminalKind::Shell)
                {
                    self.mode = Mode::ConfirmClose(terminal.id);
                }
                None
            }
            Key::Char('u') => self.resume_selected_session(),
            Key::Char('i') => self
                .selected
                .map(|session_id| Cmd::Request(ClientMsg::InterruptSession { session_id })),
            Key::Char('K') => {
                if let Some(id) = self.selected {
                    self.mode = Mode::ConfirmKill(id);
                }
                None
            }
            Key::Char('s') => {
                self.open_spawn_form();
                None
            }
            Key::Char('r') => Some(Cmd::Refresh),
            _ => None,
        }
    }

    fn open_spawn_form(&mut self) {
        let projects = view::project_choices(&self.world);
        if projects.is_empty() {
            self.set_status("no projects yet, create one with: pm project add", true);
            return;
        }
        let project_idx = self
            .selected
            .and_then(|id| self.world.session(id))
            .and_then(|s| projects.iter().position(|c| c.id == s.project_id))
            .unwrap_or(0);
        let workers = view::worker_choices(&self.world);
        let worker_idx = workers
            .iter()
            .position(|worker| worker.id == projects[project_idx].worker_id)
            .unwrap_or(0);
        self.mode = Mode::Spawn(SpawnForm {
            projects,
            project_idx,
            workers,
            worker_idx,
            cwd: String::new(),
            agent: AgentKind::ClaudeCode,
            role: pm_protocol::domain::SessionRole::Worker,
            permission_mode: PermissionMode::Inherit,
            title: String::new(),
            prompt: String::new(),
            focus: SpawnFocus::Prompt,
        });
    }

    fn on_key_spawn(&mut self, key: Key) -> Option<Cmd> {
        let Mode::Spawn(form) = &mut self.mode else {
            return None;
        };
        match key {
            Key::Esc | Key::CtrlC => {
                self.mode = Mode::List;
                None
            }
            Key::Tab => {
                form.focus = next_focus(form.focus);
                None
            }
            Key::BackTab => {
                form.focus = prev_focus(form.focus);
                None
            }
            Key::Up | Key::Char('k') if form.focus == SpawnFocus::Project => {
                form.project_idx = form.project_idx.saturating_sub(1);
                sync_spawn_worker(form);
                None
            }
            Key::Down | Key::Char('j') if form.focus == SpawnFocus::Project => {
                form.project_idx = (form.project_idx + 1).min(form.projects.len() - 1);
                sync_spawn_worker(form);
                None
            }
            Key::Left | Key::Char('h') if form.focus == SpawnFocus::Worker => {
                form.worker_idx = form.worker_idx.saturating_sub(1);
                None
            }
            Key::Right | Key::Char('l') | Key::Char(' ') if form.focus == SpawnFocus::Worker => {
                form.worker_idx = (form.worker_idx + 1).min(form.workers.len().saturating_sub(1));
                None
            }
            Key::Left | Key::Right | Key::Char(' ') if form.focus == SpawnFocus::Agent => {
                form.agent = next_agent(form.agent);
                None
            }
            Key::Left | Key::Right | Key::Char(' ') if form.focus == SpawnFocus::Role => {
                form.role = match form.role {
                    pm_protocol::domain::SessionRole::Worker => {
                        pm_protocol::domain::SessionRole::Supervisor
                    }
                    pm_protocol::domain::SessionRole::Supervisor => {
                        pm_protocol::domain::SessionRole::Worker
                    }
                };
                None
            }
            Key::Left | Key::Right | Key::Char(' ') if form.focus == SpawnFocus::Permission => {
                form.permission_mode = next_permission_mode(form.permission_mode);
                None
            }
            Key::Enter => self.submit_spawn(),
            Key::Backspace
                if matches!(
                    form.focus,
                    SpawnFocus::Cwd | SpawnFocus::Title | SpawnFocus::Prompt
                ) =>
            {
                spawn_text_field(form).pop();
                None
            }
            Key::Char(c)
                if matches!(
                    form.focus,
                    SpawnFocus::Cwd | SpawnFocus::Title | SpawnFocus::Prompt
                ) =>
            {
                spawn_text_field(form).push(c);
                None
            }
            _ => None,
        }
    }

    fn submit_spawn(&mut self) -> Option<Cmd> {
        let (project_id, worker_id, cwd, agent, role, permission_mode, title, prompt) = {
            let Mode::Spawn(form) = &self.mode else {
                return None;
            };
            (
                form.projects[form.project_idx].id,
                form.workers.get(form.worker_idx).map(|worker| worker.id),
                form.cwd.trim().to_string(),
                form.agent,
                form.role,
                form.permission_mode,
                form.title.trim().to_string(),
                form.prompt.clone(),
            )
        };
        let task_title: String = if !title.is_empty() {
            title
        } else if prompt.trim().is_empty() {
            "session".into()
        } else {
            prompt.trim().chars().take(TITLE_MAX_CHARS).collect()
        };
        self.mode = Mode::List;
        Some(Cmd::Request(ClientMsg::SpawnSession {
            project_id,
            agent: Some(agent),
            task_title,
            task_prompt: prompt,
            cwd,
            permission_mode,
            worker_id,
            items_api: true,
            supervisor_api: role == pm_protocol::domain::SessionRole::Supervisor,
            model_profile_id: None,
            host: String::new(),
            initial_cols: None,
            initial_rows: None,
        }))
    }

    fn on_key_confirm(&mut self, key: Key) -> Option<Cmd> {
        match key {
            Key::Char('y') => {
                let request = match self.mode {
                    Mode::ConfirmKill(session_id) => ClientMsg::KillSession { session_id },
                    Mode::ConfirmClose(terminal_id) => ClientMsg::CloseTerminal { terminal_id },
                    Mode::List | Mode::Spawn(_) => return None,
                };
                self.mode = Mode::List;
                Some(Cmd::Request(request))
            }
            Key::Char('n') | Key::Esc | Key::CtrlC => {
                self.mode = Mode::List;
                None
            }
            _ => None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let ids = view::session_row_ids(&self.world);
        if ids.is_empty() {
            self.selected = None;
            return;
        }
        let next = match self
            .selected
            .and_then(|id| ids.iter().position(|&x| x == id))
        {
            Some(i) => i.saturating_add_signed(delta).min(ids.len() - 1),
            None => 0,
        };
        self.selected = Some(ids[next]);
        self.selected_terminal = None;
        self.fix_terminal_selection();
    }

    fn selected_index(&self) -> Option<usize> {
        let ids = view::session_row_ids(&self.world);
        self.selected
            .and_then(|id| ids.iter().position(|&x| x == id))
    }

    /// Keeps the selection on the same session across list reshuffles;
    /// when the selected session disappears, falls back to the session
    /// now occupying its old position.
    fn fix_selection(&mut self, prev_index: Option<usize>) {
        let ids = view::session_row_ids(&self.world);
        if ids.is_empty() {
            self.selected = None;
            return;
        }
        if let Some(id) = self.selected {
            if ids.contains(&id) {
                return;
            }
        }
        let idx = prev_index.unwrap_or(0).min(ids.len() - 1);
        self.selected = Some(ids[idx]);
    }

    fn fix_terminal_selection(&mut self) {
        let Some(session_id) = self.selected else {
            self.selected_terminal = None;
            return;
        };
        let terminal_ids: Vec<_> = self
            .world
            .terminals_for_session(session_id)
            .into_iter()
            .map(|terminal| terminal.id)
            .collect();
        if self
            .selected_terminal
            .is_some_and(|id| terminal_ids.contains(&id))
        {
            return;
        }
        self.selected_terminal = terminal_ids.first().copied();
    }

    fn selected_terminal(&self) -> Option<&Terminal> {
        self.selected_terminal
            .and_then(|id| self.world.terminal(id))
    }

    fn move_terminal_selection(&mut self, delta: isize) {
        let Some(session_id) = self.selected else {
            return;
        };
        let ids: Vec<_> = self
            .world
            .terminals_for_session(session_id)
            .into_iter()
            .map(|terminal| terminal.id)
            .collect();
        if ids.is_empty() {
            self.selected_terminal = None;
            return;
        }
        let next = self
            .selected_terminal
            .and_then(|id| ids.iter().position(|candidate| *candidate == id))
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(ids.len() - 1);
        self.selected_terminal = Some(ids[next]);
    }

    fn attach_selected_terminal(&mut self) -> Option<Cmd> {
        let terminal = self.selected_terminal()?.clone();
        if !terminal.state.is_live() {
            self.set_status(
                format!("terminal {} is {}", terminal.id, terminal.state.as_str()),
                true,
            );
            return None;
        }
        Some(Cmd::Attach(AttachTarget {
            session_id: terminal.session_id,
            terminal_id: terminal.id,
        }))
    }

    fn restart_selected_terminal(&mut self) -> Option<Cmd> {
        let terminal = self.selected_terminal()?.clone();
        if terminal.kind != TerminalKind::Shell {
            self.set_status("agent terminals resume with their session", true);
            return None;
        }
        if terminal.state.is_live() {
            self.set_status("shell terminal is still running", true);
            return None;
        }
        Some(Cmd::Request(ClientMsg::RestartTerminal {
            terminal_id: terminal.id,
        }))
    }

    fn resume_selected_session(&mut self) -> Option<Cmd> {
        let session = self.selected.and_then(|id| self.world.session(id))?.clone();
        if session.state.is_live() {
            self.set_status("session is still running", true);
            return None;
        }
        if !session.resumable && !session.task_prompt.is_empty() {
            self.set_status("session has no resumable conversation", true);
            return None;
        }
        Some(Cmd::Request(ClientMsg::ResumeSession {
            session_id: session.id,
        }))
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

fn next_focus(focus: SpawnFocus) -> SpawnFocus {
    match focus {
        SpawnFocus::Project => SpawnFocus::Worker,
        SpawnFocus::Worker => SpawnFocus::Cwd,
        SpawnFocus::Cwd => SpawnFocus::Agent,
        SpawnFocus::Agent => SpawnFocus::Permission,
        SpawnFocus::Permission => SpawnFocus::Title,
        SpawnFocus::Title => SpawnFocus::Prompt,
        SpawnFocus::Prompt => SpawnFocus::Role,
        SpawnFocus::Role => SpawnFocus::Project,
    }
}

fn prev_focus(focus: SpawnFocus) -> SpawnFocus {
    match focus {
        SpawnFocus::Project => SpawnFocus::Role,
        SpawnFocus::Worker => SpawnFocus::Project,
        SpawnFocus::Cwd => SpawnFocus::Worker,
        SpawnFocus::Agent => SpawnFocus::Cwd,
        SpawnFocus::Permission => SpawnFocus::Agent,
        SpawnFocus::Title => SpawnFocus::Permission,
        SpawnFocus::Prompt => SpawnFocus::Title,
        SpawnFocus::Role => SpawnFocus::Prompt,
    }
}

fn spawn_text_field(form: &mut SpawnForm) -> &mut String {
    match form.focus {
        SpawnFocus::Cwd => &mut form.cwd,
        SpawnFocus::Title => &mut form.title,
        SpawnFocus::Prompt => &mut form.prompt,
        SpawnFocus::Project
        | SpawnFocus::Worker
        | SpawnFocus::Agent
        | SpawnFocus::Role
        | SpawnFocus::Permission => {
            unreachable!()
        }
    }
}

fn sync_spawn_worker(form: &mut SpawnForm) {
    let worker_id = form.projects[form.project_idx].worker_id;
    form.worker_idx = form
        .workers
        .iter()
        .position(|worker| worker.id == worker_id)
        .unwrap_or(0);
}

/// Cycles the spawn form through the agents a spawn can choose. An
/// agent the form is somehow holding that is not selectable lands back
/// on the first one.
fn next_agent(agent: AgentKind) -> AgentKind {
    let agents = AgentKind::SELECTABLE;
    let next = agents
        .iter()
        .position(|kind| *kind == agent)
        .map(|index| (index + 1) % agents.len())
        .unwrap_or(0);
    agents[next]
}

fn next_permission_mode(mode: PermissionMode) -> PermissionMode {
    match mode {
        PermissionMode::Inherit => PermissionMode::Default,
        PermissionMode::Default => PermissionMode::Auto,
        PermissionMode::Auto => PermissionMode::Bypass,
        PermissionMode::Bypass => PermissionMode::Inherit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{bucket, project, session, shell_terminal, snapshot};

    fn app_with_three_sessions() -> App {
        let mut app = App::new();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api")],
            vec![
                session(1, 1, SessionState::Working),
                session(2, 1, SessionState::Idle),
                session(3, 1, SessionState::Working),
            ],
        ));
        app
    }

    #[test]
    fn snapshot_populates_world_and_selects_first_session() {
        let app = app_with_three_sessions();
        assert_eq!(app.world.sessions.len(), 3);
        assert_eq!(app.selected, Some(1));
        assert_eq!(app.selected_terminal, Some(1_001));
    }

    fn worker(id: u64, online: bool) -> Worker {
        Worker {
            id,
            name: format!("box{id}"),
            hostname: "h".into(),
            platform: "linux".into(),
            online,
            default_project_root: String::new(),
            pm_version: String::new(),
            runtime: String::new(),
            container: String::new(),
            connect_mode: Default::default(),
            endpoint: String::new(),
            last_seen_at_unix_ms: None,
        }
    }

    fn glance_context(session_id: u64, value: &str) -> SessionContext {
        use pm_protocol::domain::{ContextField, ContextKind, ContextSeverity};
        SessionContext {
            session_id,
            glance: vec![ContextField {
                key: "status".into(),
                label: "status".into(),
                value: value.into(),
                kind: ContextKind::Badge,
                severity: ContextSeverity::Good,
            }],
            detail: Vec::new(),
        }
    }

    #[test]
    fn context_events_upsert_and_clear() {
        let mut app = app_with_three_sessions();
        app.apply_event(Event::ContextChanged(glance_context(1, "building")));
        assert_eq!(app.world.context(1).unwrap().glance[0].value, "building");
        app.apply_event(Event::ContextChanged(glance_context(1, "testing")));
        assert_eq!(app.world.context(1).unwrap().glance[0].value, "testing");
        // Empty bags clear the entry.
        app.apply_event(Event::ContextChanged(SessionContext {
            session_id: 1,
            glance: Vec::new(),
            detail: Vec::new(),
        }));
        assert!(app.world.context(1).is_none());
    }

    #[test]
    fn worker_events_upsert_and_remove() {
        let mut app = App::new();
        app.apply_event(Event::WorkerChanged(worker(1, true)));
        assert_eq!(app.world.worker(1).map(|w| w.online), Some(true));
        app.apply_event(Event::WorkerChanged(worker(1, false)));
        assert_eq!(app.world.worker(1).map(|w| w.online), Some(false));
        app.apply_event(Event::WorkerRemoved(1));
        assert!(app.world.worker(1).is_none());
    }

    #[test]
    fn events_update_and_remove_sessions() {
        let mut app = app_with_three_sessions();
        let mut s = session(2, 1, SessionState::Exited);
        s.exit_code = Some(0);
        app.apply_event(Event::SessionChanged(s));
        assert_eq!(app.world.session(2).unwrap().state, SessionState::Exited);
        app.apply_event(Event::SessionRemoved(2));
        assert!(app.world.session(2).is_none());
        assert_eq!(app.world.sessions.len(), 2);
    }

    #[test]
    fn event_for_unknown_session_inserts_it() {
        let mut app = app_with_three_sessions();
        app.apply_event(Event::SessionChanged(session(9, 1, SessionState::Working)));
        assert!(app.world.session(9).is_some());
    }

    #[test]
    fn bell_rings_on_transition_into_needs_input() {
        let mut app = app_with_three_sessions();
        assert!(!app.take_bell());
        app.apply_event(Event::SessionChanged(session(
            1,
            1,
            SessionState::NeedsInput,
        )));
        assert!(app.take_bell());
        assert!(!app.take_bell());
    }

    #[test]
    fn bell_does_not_ring_when_already_needs_input() {
        let mut app = app_with_three_sessions();
        app.apply_event(Event::SessionChanged(session(
            1,
            1,
            SessionState::NeedsInput,
        )));
        app.take_bell();
        app.apply_event(Event::SessionChanged(session(
            1,
            1,
            SessionState::NeedsInput,
        )));
        assert!(!app.take_bell());
    }

    #[test]
    fn bell_does_not_ring_on_initial_snapshot() {
        let mut app = App::new();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api")],
            vec![session(1, 1, SessionState::NeedsInput)],
        ));
        assert!(!app.take_bell());
    }

    #[test]
    fn bell_rings_when_resnapshot_flips_a_session_to_needs_input() {
        let mut app = app_with_three_sessions();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api")],
            vec![
                session(1, 1, SessionState::NeedsInput),
                session(2, 1, SessionState::Idle),
                session(3, 1, SessionState::Working),
            ],
        ));
        assert!(app.take_bell());
    }

    #[test]
    fn resnapshot_with_unchanged_needs_input_does_not_ring() {
        let mut app = App::new();
        let snap = || {
            snapshot(
                vec![bucket(1, "work")],
                vec![project(1, 1, "api")],
                vec![session(1, 1, SessionState::NeedsInput)],
            )
        };
        app.apply_snapshot(snap());
        app.take_bell();
        app.apply_snapshot(snap());
        assert!(!app.take_bell());
    }

    #[test]
    fn selection_moves_and_clamps() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('k'));
        assert_eq!(app.selected, Some(1));
        app.on_key(Key::Char('j'));
        app.on_key(Key::Char('j'));
        assert_eq!(app.selected, Some(3));
        app.on_key(Key::Down);
        assert_eq!(app.selected, Some(3));
        app.on_key(Key::Up);
        assert_eq!(app.selected, Some(2));
    }

    #[test]
    fn selection_follows_session_when_it_gets_pinned() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('j'));
        assert_eq!(app.selected, Some(2));
        app.apply_event(Event::SessionChanged(session(
            2,
            1,
            SessionState::NeedsInput,
        )));
        assert_eq!(app.selected, Some(2));
        assert_eq!(view::session_row_ids(&app.world), vec![2, 1, 3]);
        app.on_key(Key::Char('k'));
        assert_eq!(app.selected, Some(2));
        app.on_key(Key::Char('j'));
        assert_eq!(app.selected, Some(1));
    }

    #[test]
    fn selection_repairs_when_selected_session_is_removed() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('j'));
        assert_eq!(app.selected, Some(2));
        app.apply_event(Event::SessionRemoved(2));
        assert_eq!(app.selected, Some(3));
        app.apply_event(Event::SessionRemoved(3));
        assert_eq!(app.selected, Some(1));
        app.apply_event(Event::SessionRemoved(1));
        assert_eq!(app.selected, None);
    }

    #[test]
    fn enter_attaches_selected_session() {
        let mut app = app_with_three_sessions();
        assert_eq!(
            app.on_key(Key::Enter),
            Some(Cmd::Attach(AttachTarget {
                session_id: 1,
                terminal_id: 1_001,
            }))
        );
    }

    #[test]
    fn terminal_selection_cycles_and_attaches_shells() {
        let mut app = app_with_three_sessions();
        app.apply_event(Event::TerminalChanged(shell_terminal(50, 1, true)));
        app.on_key(Key::Right);
        assert_eq!(app.selected_terminal, Some(50));
        assert_eq!(
            app.on_key(Key::Enter),
            Some(Cmd::Attach(AttachTarget {
                session_id: 1,
                terminal_id: 50,
            }))
        );
        app.apply_event(Event::TerminalRemoved(50));
        assert_eq!(app.selected_terminal, Some(1_001));
    }

    #[test]
    fn shell_lifecycle_keys_target_the_selected_terminal() {
        let mut app = app_with_three_sessions();
        assert_eq!(
            app.on_key(Key::Char('n')),
            Some(Cmd::Request(ClientMsg::CreateShell {
                session_id: 1,
                title: String::new(),
            }))
        );

        app.apply_event(Event::TerminalChanged(shell_terminal(50, 1, false)));
        app.on_key(Key::Right);
        assert_eq!(
            app.on_key(Key::Char('R')),
            Some(Cmd::Request(ClientMsg::RestartTerminal { terminal_id: 50 }))
        );
        assert_eq!(app.on_key(Key::Char('x')), None);
        assert!(matches!(app.mode, Mode::ConfirmClose(50)));
        assert_eq!(
            app.on_key(Key::Char('y')),
            Some(Cmd::Request(ClientMsg::CloseTerminal { terminal_id: 50 }))
        );
    }

    #[test]
    fn resume_targets_an_ended_resumable_session() {
        let mut app = app_with_three_sessions();
        let mut ended = session(1, 1, SessionState::Exited);
        ended.resumable = true;
        app.apply_event(Event::SessionChanged(ended));
        assert_eq!(
            app.on_key(Key::Char('u')),
            Some(Cmd::Request(ClientMsg::ResumeSession { session_id: 1 }))
        );
    }

    #[test]
    fn interrupt_targets_selected_session() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('j'));
        assert_eq!(
            app.on_key(Key::Char('i')),
            Some(Cmd::Request(ClientMsg::InterruptSession { session_id: 2 }))
        );
    }

    #[test]
    fn kill_requires_confirmation() {
        let mut app = app_with_three_sessions();
        assert_eq!(app.on_key(Key::Char('K')), None);
        assert!(matches!(app.mode, Mode::ConfirmKill(1)));
        assert_eq!(
            app.on_key(Key::Char('y')),
            Some(Cmd::Request(ClientMsg::KillSession { session_id: 1 }))
        );
        assert!(matches!(app.mode, Mode::List));
    }

    #[test]
    fn kill_confirmation_can_be_declined() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('K'));
        assert_eq!(app.on_key(Key::Char('n')), None);
        assert!(matches!(app.mode, Mode::List));
        app.on_key(Key::Char('K'));
        assert_eq!(app.on_key(Key::Esc), None);
        assert!(matches!(app.mode, Mode::List));
    }

    #[test]
    fn quit_and_refresh_keys() {
        let mut app = app_with_three_sessions();
        assert_eq!(app.on_key(Key::Char('q')), Some(Cmd::Quit));
        assert_eq!(app.on_key(Key::CtrlC), Some(Cmd::Quit));
        assert_eq!(app.on_key(Key::Char('r')), Some(Cmd::Refresh));
    }

    #[test]
    fn spawn_form_submits_typed_prompt() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('s'));
        for c in "fix bug".chars() {
            app.on_key(Key::Char(c));
        }
        let cmd = app.on_key(Key::Enter);
        assert_eq!(
            cmd,
            Some(Cmd::Request(ClientMsg::SpawnSession {
                project_id: 1,
                agent: Some(AgentKind::ClaudeCode),
                task_title: "fix bug".into(),
                task_prompt: "fix bug".into(),
                cwd: String::new(),
                permission_mode: pm_protocol::domain::PermissionMode::Inherit,
                worker_id: Some(pm_protocol::domain::LOCAL_WORKER_ID),
                items_api: true,
                supervisor_api: false,
                model_profile_id: None,
                host: String::new(),
                initial_cols: None,
                initial_rows: None,
            }))
        );
        assert!(matches!(app.mode, Mode::List));
    }

    #[test]
    fn spawn_title_truncates_long_prompts() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('s'));
        let prompt = "x".repeat(TITLE_MAX_CHARS + 10);
        for c in prompt.chars() {
            app.on_key(Key::Char(c));
        }
        let Some(Cmd::Request(ClientMsg::SpawnSession {
            task_title,
            task_prompt,
            ..
        })) = app.on_key(Key::Enter)
        else {
            panic!("expected spawn request");
        };
        assert_eq!(task_title.chars().count(), TITLE_MAX_CHARS);
        assert_eq!(task_prompt, prompt);
    }

    /// The picker walks every selectable agent and wraps, so an agent
    /// added to the registry becomes reachable in the form without a
    /// second edit here.
    #[test]
    fn agent_cycling_visits_every_selectable_agent_and_wraps() {
        let mut seen = Vec::new();
        let mut agent = AgentKind::SELECTABLE[0];
        for _ in AgentKind::SELECTABLE {
            seen.push(agent);
            agent = next_agent(agent);
        }
        assert_eq!(seen, AgentKind::SELECTABLE.to_vec());
        assert_eq!(agent, AgentKind::SELECTABLE[0]);
        assert_eq!(next_agent(AgentKind::Test), AgentKind::SELECTABLE[0]);
    }

    #[test]
    fn spawn_form_toggles_agent_and_project() {
        let mut app = App::new();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api"), project(2, 1, "web")],
            vec![session(1, 1, SessionState::Idle)],
        ));
        app.on_key(Key::Char('s'));
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        app.on_key(Key::Right);
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        app.on_key(Key::Down);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        for c in "go".chars() {
            app.on_key(Key::Char(c));
        }
        assert_eq!(
            app.on_key(Key::Enter),
            Some(Cmd::Request(ClientMsg::SpawnSession {
                project_id: 2,
                agent: Some(AgentKind::Codex),
                task_title: "go".into(),
                task_prompt: "go".into(),
                cwd: String::new(),
                permission_mode: pm_protocol::domain::PermissionMode::Inherit,
                worker_id: Some(pm_protocol::domain::LOCAL_WORKER_ID),
                items_api: true,
                supervisor_api: false,
                model_profile_id: None,
                host: String::new(),
                initial_cols: None,
                initial_rows: None,
            }))
        );
    }

    #[test]
    fn spawn_form_selects_worker_and_permission_mode() {
        let mut app = app_with_three_sessions();
        app.apply_event(Event::WorkerChanged(worker(7, true)));
        app.on_key(Key::Char('s'));
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        app.on_key(Key::Right);
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        app.on_key(Key::Right);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        for c in "remote".chars() {
            app.on_key(Key::Char(c));
        }
        assert_eq!(
            app.on_key(Key::Enter),
            Some(Cmd::Request(ClientMsg::SpawnSession {
                project_id: 1,
                agent: Some(AgentKind::ClaudeCode),
                task_title: "remote".into(),
                task_prompt: "remote".into(),
                cwd: String::new(),
                permission_mode: PermissionMode::Default,
                worker_id: Some(7),
                items_api: true,
                supervisor_api: false,
                model_profile_id: None,
                host: String::new(),
                initial_cols: None,
                initial_rows: None,
            }))
        );
    }

    #[test]
    fn spawn_form_accepts_cwd_and_title_overrides() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('s'));
        app.on_key(Key::BackTab);
        for c in "named session".chars() {
            app.on_key(Key::Char(c));
        }
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        app.on_key(Key::BackTab);
        for c in "/tmp/work".chars() {
            app.on_key(Key::Char(c));
        }
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        app.on_key(Key::Tab);
        for c in "do it".chars() {
            app.on_key(Key::Char(c));
        }
        assert_eq!(
            app.on_key(Key::Enter),
            Some(Cmd::Request(ClientMsg::SpawnSession {
                project_id: 1,
                agent: Some(AgentKind::ClaudeCode),
                task_title: "named session".into(),
                task_prompt: "do it".into(),
                cwd: "/tmp/work".into(),
                permission_mode: PermissionMode::Inherit,
                worker_id: Some(pm_protocol::domain::LOCAL_WORKER_ID),
                items_api: true,
                supervisor_api: false,
                model_profile_id: None,
                host: String::new(),
                initial_cols: None,
                initial_rows: None,
            }))
        );
    }

    #[test]
    fn spawn_form_allows_empty_prompt() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('s'));
        let Some(Cmd::Request(ClientMsg::SpawnSession {
            task_prompt,
            task_title,
            ..
        })) = app.on_key(Key::Enter)
        else {
            panic!("empty prompt should still spawn");
        };
        assert_eq!(task_prompt, "");
        assert_eq!(task_title, "session");
        assert!(matches!(app.mode, Mode::List));
    }

    #[test]
    fn spawn_form_cancels_with_esc() {
        let mut app = app_with_three_sessions();
        app.on_key(Key::Char('s'));
        assert_eq!(app.on_key(Key::Esc), None);
        assert!(matches!(app.mode, Mode::List));
    }

    #[test]
    fn spawn_without_projects_sets_error_status() {
        let mut app = App::new();
        app.apply_snapshot(snapshot(vec![bucket(1, "work")], vec![], vec![]));
        app.on_key(Key::Char('s'));
        assert!(matches!(app.mode, Mode::List));
        assert!(app.status.as_ref().is_some_and(|s| s.error));
    }

    #[test]
    fn spawn_form_preselects_selected_sessions_project() {
        let mut app = App::new();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api"), project(2, 1, "web")],
            vec![
                session(1, 1, SessionState::Idle),
                session(2, 2, SessionState::Idle),
            ],
        ));
        app.on_key(Key::Char('j'));
        app.on_key(Key::Char('s'));
        let Mode::Spawn(form) = &app.mode else {
            panic!("expected spawn form");
        };
        assert_eq!(form.projects[form.project_idx].id, 2);
    }
}
