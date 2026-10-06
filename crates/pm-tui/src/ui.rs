//! Rendering: a thin map from App and the view-model onto ratatui
//! widgets. No state lives here.

use pm_protocol::domain::{ContextSeverity, Session, SessionState, TerminalKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, Connection, Mode, SpawnFocus, SpawnForm};
use crate::view::{self, Row};

const COLOR_STARTING: Color = Color::Cyan;
const COLOR_WORKING: Color = Color::Green;
const COLOR_NEEDS_INPUT: Color = Color::LightRed;
const COLOR_IDLE: Color = Color::Yellow;
const COLOR_ENDED: Color = Color::DarkGray;
const COLOR_FAILED: Color = Color::Red;
const COLOR_DIM: Color = Color::DarkGray;
const COLOR_FOCUS: Color = Color::Cyan;

const FOOTER_HELP: &str =
    " j/k session  h/l terminal  Enter attach  n new  R restart  x close  u resume  s spawn  i int  K kill  q quit";
const EMPTY_HINT: &str = "no buckets yet — create one with: pm bucket add <name>";

const SPAWN_POPUP_WIDTH: u16 = 62;
const SPAWN_POPUP_HEIGHT: u16 = 13;
const CONFIRM_POPUP_WIDTH: u16 = 46;
const CONFIRM_POPUP_HEIGHT: u16 = 5;
/// Longest prompt tail shown in the spawn form before the head is
/// elided.
const PROMPT_DISPLAY_CHARS: usize = 44;

pub(crate) fn render(frame: &mut Frame, app: &App, now_ms: i64) {
    let [header_area, list_area, terminal_area, status_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(Paragraph::new(header_line(app)), header_area);
    render_list(frame, app, now_ms, list_area);
    frame.render_widget(terminal_paragraph(app), terminal_area);
    frame.render_widget(status_paragraph(app), status_area);
    frame.render_widget(
        Paragraph::new(FOOTER_HELP).style(Style::new().fg(COLOR_DIM)),
        footer_area,
    );

    match &app.mode {
        Mode::List => {}
        Mode::Spawn(form) => render_spawn(frame, form),
        Mode::ConfirmKill(id) => render_confirm(frame, app, *id),
        Mode::ConfirmClose(id) => render_close_confirm(frame, app, *id),
    }
}

fn render_list(frame: &mut Frame, app: &App, now_ms: i64, area: Rect) {
    let rows = view::rows(&app.world);
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(EMPTY_HINT).style(Style::new().fg(COLOR_DIM)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = rows.iter().map(|row| row_item(app, row, now_ms)).collect();
    let mut state = ListState::default();
    state.select(
        rows.iter()
            .position(|row| matches!(row, Row::Session { id, .. } if Some(*id) == app.selected)),
    );
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
}

fn header_line(app: &App) -> Line<'static> {
    let live = app
        .world
        .sessions
        .iter()
        .filter(|s| s.state.is_live())
        .count();
    let needs_input = app
        .world
        .sessions
        .iter()
        .filter(|s| s.state == SessionState::NeedsInput)
        .count();
    let mut spans = vec![
        Span::styled(" puppet master ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" {live} live sessions")),
    ];
    if needs_input > 0 {
        spans.push(Span::styled(
            format!("   {needs_input} need input"),
            Style::new()
                .fg(COLOR_NEEDS_INPUT)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn status_paragraph(app: &App) -> Paragraph<'static> {
    match app.connection {
        Connection::Reconnecting { attempt } => {
            Paragraph::new(format!(" connecting to daemon (attempt {attempt})..."))
                .style(Style::new().fg(Color::Red).add_modifier(Modifier::BOLD))
        }
        Connection::Connected => match &app.status {
            Some(status) => Paragraph::new(format!(" {}", status.text)).style(if status.error {
                Style::new().fg(Color::Red)
            } else {
                Style::new().fg(COLOR_DIM)
            }),
            None => Paragraph::new(""),
        },
    }
}

fn row_item(app: &App, row: &Row, now_ms: i64) -> ListItem<'static> {
    match row {
        Row::NeedsInputHeader => ListItem::new(Line::styled(
            " needs input",
            Style::new()
                .fg(COLOR_NEEDS_INPUT)
                .add_modifier(Modifier::BOLD),
        )),
        Row::Bucket(id) => {
            let name = app
                .world
                .bucket(*id)
                .map(|b| b.name.clone())
                .unwrap_or_default();
            ListItem::new(Line::styled(
                format!(" {name}"),
                Style::new().add_modifier(Modifier::BOLD),
            ))
        }
        Row::Project(id) => {
            let name = app
                .world
                .project(*id)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            ListItem::new(Line::raw(format!("   {name}")))
        }
        Row::Supervisors => ListItem::new(Line::styled(
            "   Supervisors",
            Style::new().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
        )),
        Row::Session { id, pinned } => match app.world.session(*id) {
            Some(s) => ListItem::new(session_line(app, s, *pinned, now_ms)),
            None => ListItem::new(Line::raw("")),
        },
    }
}

fn session_line(app: &App, s: &Session, pinned: bool, now_ms: i64) -> Line<'static> {
    let dim = Style::new().fg(COLOR_DIM);
    let mut spans = Vec::new();
    spans.push(Span::raw(if pinned { " " } else { "   " }.to_string()));
    if s.state == SessionState::NeedsInput {
        spans.push(Span::styled(
            "! ".to_string(),
            Style::new()
                .fg(COLOR_NEEDS_INPUT)
                .add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK),
        ));
    } else {
        spans.push(Span::raw("  ".to_string()));
    }
    spans.push(Span::raw(format!("{:>4}  ", s.id)));
    spans.push(Span::styled(
        format!("{:<12}", s.state.as_str()),
        state_style(s.state),
    ));
    spans.push(Span::raw(format!("{:<8}", s.agent.as_str())));
    spans.push(Span::styled(
        format!("{:<11}", s.role.as_str()),
        if s.role == pm_protocol::domain::SessionRole::Supervisor {
            Style::new().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
        } else {
            dim
        },
    ));
    if s.worker_id != pm_protocol::domain::LOCAL_WORKER_ID {
        let (label, style) = match app.world.worker(s.worker_id) {
            Some(w) if w.online => (format!("@{} ", w.name), dim),
            Some(w) => (
                format!("@{} (offline) ", w.name),
                Style::new().fg(COLOR_NEEDS_INPUT),
            ),
            None => (format!("@worker {} ", s.worker_id), dim),
        };
        spans.push(Span::styled(label, style));
    }
    spans.push(Span::raw(format!("{}  ", s.display_name())));
    if !s.headline.is_empty() && s.headline != s.display_name() {
        spans.push(Span::styled(format!("{}  ", s.headline), dim));
    }
    if s.role == pm_protocol::domain::SessionRole::Supervisor {
        let project = app
            .world
            .project(s.project_id)
            .map(|p| p.name.as_str())
            .unwrap_or("unknown");
        let children = app
            .world
            .sessions
            .iter()
            .filter(|child| child.spawned_by_session_id == Some(s.id))
            .count();
        spans.push(Span::styled(
            format!("launch={project} cwd={} children={children}  ", s.cwd),
            dim,
        ));
    }
    spans.push(Span::styled(view::last_active_ago(s, now_ms), dim));
    if let Some(code) = s.exit_code {
        spans.push(Span::styled(format!("  exit {code}"), dim));
    }
    if matches!(s.state, SessionState::Failed | SessionState::AwaitingWorker)
        && !s.state_detail.is_empty()
    {
        spans.push(Span::styled(format!("  {}", s.state_detail), dim));
    }
    if let Some(context) = app.world.context(s.id) {
        for field in &context.glance {
            spans.push(Span::styled(format!("  {} ", field.label), dim));
            spans.push(Span::styled(
                field.value.clone(),
                severity_style(field.severity),
            ));
        }
    }
    if pinned {
        spans.push(Span::styled(
            format!("  [{}]", view::project_label(&app.world, s.project_id)),
            dim,
        ));
    }
    Line::from(spans)
}

fn terminal_paragraph(app: &App) -> Paragraph<'static> {
    let Some(session_id) = app.selected else {
        return Paragraph::new("");
    };
    let terminals = app.world.terminals_for_session(session_id);
    if terminals.is_empty() {
        return Paragraph::new(Line::styled(" no terminals", Style::new().fg(COLOR_DIM)));
    }
    let mut spans = vec![Span::styled(" terminals  ", Style::new().fg(COLOR_DIM))];
    for terminal in terminals {
        let selected = app.selected_terminal == Some(terminal.id);
        let label = if terminal.kind == TerminalKind::Agent {
            "agent".to_string()
        } else if terminal.title.trim().is_empty() {
            format!("shell {}", terminal.id)
        } else {
            terminal.title.clone()
        };
        let text = format!(
            " {} #{} {} g{} ",
            label,
            terminal.id,
            terminal.state.as_str(),
            terminal.generation
        );
        let style = if selected {
            Style::new()
                .fg(Color::Black)
                .bg(COLOR_FOCUS)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(COLOR_DIM)
        };
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
    }
    let detail = app
        .selected_terminal
        .and_then(|id| app.world.terminal(id))
        .map(|terminal| {
            Line::styled(
                format!(" cwd: {}", terminal.cwd),
                Style::new().fg(COLOR_DIM),
            )
        })
        .unwrap_or_default();
    Paragraph::new(vec![Line::from(spans), detail])
}

fn severity_style(severity: ContextSeverity) -> Style {
    let color = match severity {
        ContextSeverity::Good => COLOR_WORKING,
        ContextSeverity::Warn => COLOR_IDLE,
        ContextSeverity::Bad => COLOR_FAILED,
        ContextSeverity::Info => COLOR_STARTING,
        ContextSeverity::Neutral => COLOR_DIM,
    };
    Style::new().fg(color)
}

fn state_style(state: SessionState) -> Style {
    match state {
        SessionState::Starting => Style::new().fg(COLOR_STARTING),
        SessionState::Working => Style::new().fg(COLOR_WORKING),
        SessionState::NeedsInput => Style::new()
            .fg(COLOR_NEEDS_INPUT)
            .add_modifier(Modifier::BOLD),
        SessionState::Idle => Style::new().fg(COLOR_IDLE),
        SessionState::Exited => Style::new().fg(COLOR_ENDED),
        SessionState::Failed => Style::new().fg(COLOR_FAILED),
        SessionState::AwaitingWorker => Style::new().fg(COLOR_DIM),
    }
}

fn render_spawn(frame: &mut Frame, form: &SpawnForm) {
    let area = centered(SPAWN_POPUP_WIDTH, SPAWN_POPUP_HEIGHT, frame.area());
    frame.render_widget(Clear, area);

    let focus_marker = |focus: SpawnFocus| {
        if form.focus == focus {
            Span::styled(
                "> ",
                Style::new().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("  ")
        }
    };
    let agent_span = |kind: pm_protocol::domain::AgentKind| {
        if form.agent == kind {
            Span::styled(
                format!("[{}]", kind.as_str()),
                Style::new().add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {} ", kind.as_str()), Style::new().fg(COLOR_DIM))
        }
    };
    let project = &form.projects[form.project_idx];
    let worker = form.workers.get(form.worker_idx);
    let cwd_shown = prompt_tail(&form.cwd, PROMPT_DISPLAY_CHARS);
    let title_shown = prompt_tail(&form.title, PROMPT_DISPLAY_CHARS);
    let prompt_shown = prompt_tail(&form.prompt, PROMPT_DISPLAY_CHARS);
    let cursor = |focus| if form.focus == focus { "_" } else { "" };

    let lines = vec![
        Line::from(vec![
            focus_marker(SpawnFocus::Project),
            Span::raw(format!(
                "project: {} ({}/{})",
                project.label,
                form.project_idx + 1,
                form.projects.len()
            )),
        ]),
        Line::from(vec![
            focus_marker(SpawnFocus::Worker),
            Span::raw(format!(
                "worker:  {}",
                worker
                    .map(|worker| worker.label.as_str())
                    .unwrap_or("default")
            )),
        ]),
        Line::from(vec![
            focus_marker(SpawnFocus::Cwd),
            Span::raw(format!(
                "cwd:     {}{}",
                if cwd_shown.is_empty() {
                    "(default)"
                } else {
                    &cwd_shown
                },
                cursor(SpawnFocus::Cwd)
            )),
        ]),
        Line::from({
            let mut spans = vec![focus_marker(SpawnFocus::Agent), Span::raw("agent:   ")];
            for (index, kind) in pm_protocol::domain::AgentKind::SELECTABLE
                .iter()
                .enumerate()
            {
                if index > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(agent_span(*kind));
            }
            spans
        }),
        Line::from(vec![
            focus_marker(SpawnFocus::Role),
            Span::raw(format!("role:    {}", form.role.as_str())),
        ]),
        Line::from(vec![
            focus_marker(SpawnFocus::Permission),
            Span::raw(format!("mode:     {}", form.permission_mode.as_str())),
        ]),
        Line::from(vec![
            focus_marker(SpawnFocus::Title),
            Span::raw(format!(
                "title:   {title_shown}{}",
                cursor(SpawnFocus::Title)
            )),
        ]),
        Line::from(vec![
            focus_marker(SpawnFocus::Prompt),
            Span::raw(format!(
                "prompt:  {prompt_shown}{}",
                cursor(SpawnFocus::Prompt)
            )),
        ]),
        Line::raw(""),
        Line::styled(
            "Tab field   Up/Down project   Left/Right choice   Enter spawn   Esc cancel",
            Style::new().fg(COLOR_DIM),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" spawn session ")),
        area,
    );
}

fn render_confirm(frame: &mut Frame, app: &App, session_id: u64) {
    let area = centered(CONFIRM_POPUP_WIDTH, CONFIRM_POPUP_HEIGHT, frame.area());
    frame.render_widget(Clear, area);
    let title = app
        .world
        .session(session_id)
        .map(|s| s.task_title.clone())
        .unwrap_or_default();
    let lines = vec![
        Line::from(vec![
            Span::raw("kill session "),
            Span::styled(
                format!("{session_id}"),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw("? "),
            Span::styled(format!("({title})"), Style::new().fg(COLOR_DIM)),
        ]),
        Line::raw(""),
        Line::styled("y confirm   n cancel", Style::new().fg(COLOR_DIM)),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" confirm kill ")),
        area,
    );
}

fn render_close_confirm(frame: &mut Frame, app: &App, terminal_id: u64) {
    let area = centered(CONFIRM_POPUP_WIDTH, CONFIRM_POPUP_HEIGHT, frame.area());
    frame.render_widget(Clear, area);
    let title = app
        .world
        .terminal(terminal_id)
        .map(|terminal| terminal.title.clone())
        .unwrap_or_default();
    let lines = vec![
        Line::from(vec![
            Span::raw("close terminal "),
            Span::styled(
                terminal_id.to_string(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw("? "),
            Span::styled(format!("({title})"), Style::new().fg(COLOR_DIM)),
        ]),
        Line::raw(""),
        Line::styled("y confirm   n cancel", Style::new().fg(COLOR_DIM)),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" confirm close ")),
        area,
    );
}

fn prompt_tail(prompt: &str, max_chars: usize) -> String {
    let count = prompt.chars().count();
    if count <= max_chars {
        return prompt.to_string();
    }
    let tail: String = prompt.chars().skip(count - (max_chars - 1)).collect();
    format!("…{tail}")
}

fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{bucket, project, session, shell_terminal, snapshot};
    use pm_protocol::domain::{Event, SessionState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn dashboard_renders_terminal_membership_headline_and_context() {
        use pm_protocol::domain::{ContextField, ContextKind, ContextSeverity, SessionContext};

        let mut app = App::new();
        let mut active = session(1, 1, SessionState::Working);
        active.goal = "Moving auth to JWTs".into();
        active.headline = "migrating auth".into();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api")],
            vec![active],
        ));
        app.apply_event(Event::ContextChanged(SessionContext {
            session_id: 1,
            glance: vec![ContextField {
                key: "tests".into(),
                label: "tests".into(),
                value: "passing".into(),
                kind: ContextKind::Badge,
                severity: ContextSeverity::Good,
            }],
            detail: Vec::new(),
        }));
        app.apply_event(Event::TerminalChanged(shell_terminal(50, 1, true)));
        app.selected_terminal = Some(50);

        let mut terminal = Terminal::new(TestBackend::new(140, 16)).unwrap();
        terminal.draw(|frame| render(frame, &app, 1_000)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            rendered.contains("Moving auth to JWTs"),
            "goal names the row"
        );
        assert!(rendered.contains("migrating auth"), "headline");
        assert!(rendered.contains("tests"), "glance label");
        assert!(rendered.contains("passing"), "glance value");
        assert!(rendered.contains("agent #1001 running g1"));
        assert!(rendered.contains("shell 50 #50 running g1"));
    }

    #[test]
    fn dashboard_names_the_offline_recovery_state_and_worker() {
        let mut app = App::new();
        let mut awaiting = session(7, 1, SessionState::AwaitingWorker);
        awaiting.worker_id = 9;
        awaiting.state_detail = "worker offline, resumes when it reconnects".into();
        let mut initial = snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api")],
            vec![awaiting],
        );
        initial.workers.push(pm_protocol::domain::Worker {
            id: 9,
            name: "build-host".into(),
            hostname: "build.local".into(),
            platform: "linux".into(),
            online: false,
            default_project_root: "/srv".into(),
            last_seen_at_unix_ms: Some(1),
            pm_version: String::new(),
            runtime: String::new(),
            container: String::new(),
            connect_mode: Default::default(),
            endpoint: String::new(),
        });
        app.apply_snapshot(initial);

        let mut terminal = Terminal::new(TestBackend::new(140, 12)).unwrap();
        terminal.draw(|frame| render(frame, &app, 1_000)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("awaiting-worker"));
        assert!(rendered.contains("@build-host (offline)"));
        assert!(rendered.contains("worker offline, resumes when it reconnects"));
    }
}
