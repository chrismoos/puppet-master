//! Raw-mode terminal attach: bridges the local terminal to a session's
//! PTY through the daemon. Detach with Ctrl-\.

use std::io::{Read, Write};
use std::time::Duration;

use bytes::Bytes;
use crossterm::terminal;
use pm_client::{Client, Target};
use pm_protocol::domain::{ClientMsg, Event, Scope, ServerMsg, Session, SessionState};

/// ASCII FS, what Ctrl-\ produces. Chosen over Ctrl-C so interrupts
/// pass through to the agent.
const DETACH_BYTE: u8 = 0x1c;

const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Rung when the attached session flips to needs-input, so the
/// terminal (or its multiplexer) can signal for attention. tmux's
/// monitor-bell turns this into a flagged window.
const BELL: &[u8] = b"\x07";

/// Terminal-state marker prefixed to the emitted title.
fn state_glyph(state: SessionState) -> &'static str {
    match state {
        SessionState::Starting => "…",
        SessionState::Working => "●",
        SessionState::NeedsInput => "▲",
        SessionState::Idle => "○",
        SessionState::Exited => "·",
        SessionState::Failed => "✗",
        SessionState::AwaitingWorker => "◇",
    }
}

/// OSC 2 window/pane title, which tmux exposes as #{pane_title} and can
/// use as the window name.
fn title_sequence(session: &Session) -> String {
    let name = crate::tmux::display_name(session);
    format!("\x1b]2;{} {}\x07", state_glyph(session.state), name)
}

/// The bell rings only on the transition into needs-input, so headline
/// updates while already blocked do not re-ring.
fn should_ring(previous: Option<SessionState>, next: SessionState) -> bool {
    next == SessionState::NeedsInput && previous != Some(SessionState::NeedsInput)
}

/// Private modes a relayed full-screen app may leave enabled on the
/// user's terminal — mouse reporting (1000/1002/1003/1006), focus
/// reporting (1004), bracketed paste (2004) — plus showing the cursor,
/// resetting attributes, and leaving the alternate screen. The app never
/// sees the detach, so it never disables these itself; without this the
/// user's shell is left in e.g. mouse-reporting mode and every click
/// injects escape sequences.
pub const TERMINAL_RESET: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?25h\x1b[m\x1b[?1049l";

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> anyhow::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(RawModeGuard)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Runs on every exit path (detach, session end, connection loss,
        // error) so the user's terminal is never left in a mode the
        // relayed app set.
        let mut out = std::io::stdout();
        let _ = out.write_all(TERMINAL_RESET);
        let _ = out.flush();
        let _ = terminal::disable_raw_mode();
    }
}

pub async fn run(socket: &Target, session_id: u64) -> anyhow::Result<()> {
    run_address(socket, Some(session_id), None).await
}

pub async fn run_terminal(socket: &Target, terminal_id: u64) -> anyhow::Result<()> {
    run_address(socket, None, Some(terminal_id)).await
}

async fn run_address(
    socket: &Target,
    session_id: Option<u64>,
    terminal_id: Option<u64>,
) -> anyhow::Result<()> {
    let mut client = Client::open(socket).await?;
    client
        .request(ClientMsg::Subscribe {
            scope: session_id.map(Scope::Session).unwrap_or(Scope::All),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut attached_session_id = session_id;
    let mut initial_session: Option<Session> = None;
    if let Some(terminal_id) = terminal_id {
        loop {
            match client.next_msg().await {
                Some(ServerMsg::Snapshot(snapshot)) => {
                    attached_session_id = snapshot
                        .terminals
                        .iter()
                        .find(|t| t.id == terminal_id)
                        .map(|t| t.session_id);
                    break;
                }
                Some(_) => continue,
                None => anyhow::bail!("connection closed before terminal snapshot"),
            }
        }
        if attached_session_id.is_none() {
            anyhow::bail!("terminal {terminal_id} not found");
        }
        client
            .request(ClientMsg::Subscribe {
                scope: Scope::Session(attached_session_id.unwrap()),
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        loop {
            match client.next_msg().await {
                Some(ServerMsg::Snapshot(snapshot)) => {
                    initial_session = snapshot
                        .sessions
                        .iter()
                        .find(|s| Some(s.id) == attached_session_id)
                        .cloned();
                    break;
                }
                Some(_) => continue,
                None => anyhow::bail!("connection closed while subscribing to session"),
            }
        }
    }
    client
        .request(match terminal_id {
            Some(terminal_id) => ClientMsg::AttachTerminal { terminal_id },
            None => ClientMsg::AttachPty {
                session_id: session_id.unwrap(),
            },
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let (cols, rows) = terminal::size()?;
    client.send(match terminal_id {
        Some(terminal_id) => ClientMsg::TerminalResize {
            terminal_id,
            cols,
            rows,
        },
        None => ClientMsg::PtyResize {
            session_id: session_id.unwrap(),
            cols,
            rows,
        },
    })?;

    let _raw = RawModeGuard::enable()?;
    let label = terminal_id
        .map(|id| format!("terminal {id}"))
        .unwrap_or_else(|| format!("session {}", session_id.unwrap()));
    eprint!("attached to {label}, detach with Ctrl-\\\r\n");

    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut stdout = std::io::stdout();
    let mut resize_poll = tokio::time::interval(RESIZE_POLL_INTERVAL);
    let mut last_size = (cols, rows);
    let mut last_state: Option<SessionState> = None;
    if let Some(session) = &initial_session {
        stdout.write_all(title_sequence(session).as_bytes())?;
        stdout.flush()?;
        last_state = Some(session.state);
    }

    loop {
        tokio::select! {
            msg = client.next_msg() => {
                match msg {
                    Some(ServerMsg::PtyOutput { terminal_id: output_terminal, data, .. }) if terminal_id.is_none_or(|id| id == output_terminal) => {
                        stdout.write_all(&data)?;
                        stdout.flush()?;
                    }
                    Some(ServerMsg::Snapshot(snapshot)) => {
                        if let Some(s) = snapshot.sessions.iter().find(|s| Some(s.id) == attached_session_id) {
                            stdout.write_all(title_sequence(s).as_bytes())?;
                            stdout.flush()?;
                            last_state.get_or_insert(s.state);
                        }
                    }
                    Some(ServerMsg::Event(Event::SessionChanged(s))) if attached_session_id == Some(s.id) => {
                        stdout.write_all(title_sequence(&s).as_bytes())?;
                        if should_ring(last_state, s.state) {
                            stdout.write_all(BELL)?;
                        }
                        stdout.flush()?;
                        last_state = Some(s.state);
                        if !s.state.is_live() {
                            let code = s.exit_code.map(|c| format!(" (exit {c})")).unwrap_or_default();
                            eprint!("\r\n{label} {}{}\r\n", s.state.as_str(), code);
                            if s.state == SessionState::Failed && !s.state_detail.is_empty() {
                                eprint!("{}\r\n", s.state_detail);
                            }
                            return Ok(());
                        }
                    }
                    Some(ServerMsg::Event(Event::TerminalChanged(t))) if terminal_id == Some(t.id) && !t.state.is_live() => {
                        let code = t.exit_code.map(|c| format!(" (exit {c})")).unwrap_or_default();
                        eprint!("\r\n{label} {}{}\r\n", t.state.as_str(), code);
                        return Ok(());
                    }
                    Some(_) => {}
                    None => {
                        eprint!("\r\nconnection to daemon lost\r\n");
                        return Ok(());
                    }
                }
            }
            data = stdin_rx.recv() => {
                match data {
                    Some(data) => {
                        if let Some(pos) = data.iter().position(|&b| b == DETACH_BYTE) {
                            if pos > 0 {
                                client.send(match terminal_id { Some(terminal_id) => ClientMsg::TerminalInput { terminal_id, data: data.slice(..pos) }, None => ClientMsg::PtyInput { session_id: session_id.unwrap(), data: data.slice(..pos) } })?;
                            }
                            eprint!("\r\ndetached from {label}\r\n");
                            return Ok(());
                        }
                        client.send(match terminal_id { Some(terminal_id) => ClientMsg::TerminalInput { terminal_id, data }, None => ClientMsg::PtyInput { session_id: session_id.unwrap(), data } })?;
                    }
                    None => return Ok(()),
                }
            }
            _ = resize_poll.tick() => {
                let size = terminal::size()?;
                if size != last_size {
                    last_size = size;
                    client.send(match terminal_id { Some(terminal_id) => ClientMsg::TerminalResize { terminal_id, cols: size.0, rows: size.1 }, None => ClientMsg::PtyResize { session_id: session_id.unwrap(), cols: size.0, rows: size.1 } })?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_protocol::domain::{AgentKind, AgentSelectionSource, PermissionMode};

    fn session(state: SessionState, headline: &str, title: &str) -> Session {
        Session {
            git: None,
            id: 7,
            project_id: 1,
            agent: AgentKind::ClaudeCode,
            agent_source: AgentSelectionSource::Explicit,
            state,
            task_title: title.into(),
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
            cwd: String::new(),
            goal: String::new(),
            headline: headline.into(),
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

    #[test]
    fn title_prefers_goal_and_carries_the_state_glyph() {
        let mut s = session(SessionState::Working, "swapping cookies", "task");
        s.goal = "Moving auth to JWTs".into();
        assert_eq!(title_sequence(&s), "\x1b]2;● Moving auth to JWTs\x07");
        let s = session(SessionState::Working, "migrating auth", "");
        assert_eq!(title_sequence(&s), "\x1b]2;● migrating auth\x07");
        let s = session(SessionState::NeedsInput, "", "fix tests");
        assert_eq!(title_sequence(&s), "\x1b]2;▲ fix tests\x07");
        let s = session(SessionState::Failed, "", "");
        assert_eq!(title_sequence(&s), "\x1b]2;✗ session 7\x07");
    }

    #[test]
    fn bell_rings_only_on_the_transition_into_needs_input() {
        assert!(should_ring(
            Some(SessionState::Working),
            SessionState::NeedsInput
        ));
        assert!(should_ring(None, SessionState::NeedsInput));
        assert!(!should_ring(
            Some(SessionState::NeedsInput),
            SessionState::NeedsInput
        ));
        assert!(!should_ring(
            Some(SessionState::NeedsInput),
            SessionState::Working
        ));
    }

    #[test]
    fn terminal_reset_disables_the_modes_a_relayed_app_may_leave_on() {
        let s = String::from_utf8(TERMINAL_RESET.to_vec()).unwrap();
        // Mouse reporting, in every mode/encoding the reported bug hits.
        assert!(s.contains("\x1b[?1000l"));
        assert!(s.contains("\x1b[?1002l"));
        assert!(s.contains("\x1b[?1003l"));
        assert!(s.contains("\x1b[?1006l"));
        // Focus reporting and bracketed paste also leak into the shell.
        assert!(s.contains("\x1b[?1004l"));
        assert!(s.contains("\x1b[?2004l"));
        // And leave the user with a visible cursor, clean attrs, main screen.
        assert!(s.contains("\x1b[?25h"));
        assert!(s.contains("\x1b[?1049l"));
        // Never a full RIS reset, which would wipe scrollback.
        assert!(!s.contains("\x1bc"));
    }
}
