//! The event loop: terminal setup, daemon connection with reconnect
//! backoff, input dispatch, and the attach suspend/restore dance.

use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use pm_client::{Client, ClientError, Target};
use pm_protocol::domain::{ClientMsg, Scope, ServerMsg};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::app::{App, AttachTarget, Cmd, Connection};
use crate::attach::{self, AttachEnd, BELL};
use crate::{keys, ui, TuiError};

const RECONNECT_INITIAL_DELAY: Duration = Duration::from_millis(500);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(5);
/// Drives elapsed-time redraws and reconnect scheduling.
const TICK_INTERVAL: Duration = Duration::from_millis(250);
const STDIN_BUF_SIZE: usize = 1024;

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

enum Conn {
    Up(Client),
    Down {
        next_attempt: Instant,
        delay: Duration,
        attempt: u32,
    },
}

impl Conn {
    fn down_now() -> Self {
        Conn::Down {
            next_attempt: Instant::now(),
            delay: RECONNECT_INITIAL_DELAY,
            attempt: 1,
        }
    }

    /// Pends forever while down so it can sit in a select alongside
    /// branches that stay live.
    async fn next_msg(&mut self) -> Option<ServerMsg> {
        match self {
            Conn::Up(client) => client.next_msg().await,
            Conn::Down { .. } => std::future::pending().await,
        }
    }
}

/// Runs the dashboard until the user quits. Takes over the terminal
/// (raw mode + alternate screen) and restores it on exit or panic.
pub async fn run(target: Target) -> Result<(), TuiError> {
    let mut input_rx = spawn_stdin_reader();
    install_panic_hook();
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let mut app = App::new();
    let mut conn = Conn::down_now();
    let mut tick = tokio::time::interval(TICK_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        maybe_reconnect(&mut app, &mut conn, &target).await;
        terminal.draw(|frame| ui::render(frame, &app, now_ms()))?;
        if app.take_bell() {
            ring_bell();
        }

        tokio::select! {
            bytes = input_rx.recv() => {
                let Some(bytes) = bytes else { return Ok(()) };
                for key in keys::decode(&bytes) {
                    match app.on_key(key) {
                        None => {}
                        Some(Cmd::Quit) => return Ok(()),
                        Some(Cmd::Refresh) => {
                            mark_disconnected(&mut app, &mut conn);
                            break;
                        }
                        Some(Cmd::Request(msg)) => {
                            handle_request(&mut app, &mut conn, msg).await;
                        }
                        Some(Cmd::Attach(target)) => {
                            run_attach(&mut app, &mut conn, &mut terminal, &mut input_rx, target)
                                .await?;
                            break;
                        }
                    }
                }
            }
            msg = conn.next_msg() => match msg {
                Some(ServerMsg::Snapshot(snapshot)) => app.apply_snapshot(snapshot),
                Some(ServerMsg::Event(event)) => app.apply_event(event),
                Some(ServerMsg::PtyOutput { .. }) => {}
                Some(ServerMsg::CommandResult { .. }) => {}
                None => mark_disconnected(&mut app, &mut conn),
            },
            _ = tick.tick() => {}
        }
    }
}

async fn maybe_reconnect(app: &mut App, conn: &mut Conn, target: &Target) {
    let due = matches!(conn, Conn::Down { next_attempt, .. } if Instant::now() >= *next_attempt);
    if !due {
        return;
    }
    match connect_and_subscribe(target).await {
        Ok(client) => {
            *conn = Conn::Up(client);
            app.connection = Connection::Connected;
        }
        Err(error) => {
            // A remote login can fail for a reason only signing in again
            // fixes, which retrying would never say.
            if matches!(target, Target::Remote(_)) {
                app.set_status(error, true);
            }
            if let Conn::Down {
                next_attempt,
                delay,
                attempt,
            } = conn
            {
                app.connection = Connection::Reconnecting { attempt: *attempt };
                *attempt += 1;
                *next_attempt = Instant::now() + *delay;
                *delay = delay.saturating_mul(2).min(RECONNECT_MAX_DELAY);
            }
        }
    }
}

async fn connect_and_subscribe(target: &Target) -> Result<Client, String> {
    let client = Client::open(target).await.map_err(|e| e.to_string())?;
    client
        .request(ClientMsg::Subscribe { scope: Scope::All })
        .await
        .map_err(|e| e.to_string())?;
    Ok(client)
}

fn mark_disconnected(app: &mut App, conn: &mut Conn) {
    *conn = Conn::down_now();
    app.connection = Connection::Reconnecting { attempt: 1 };
}

async fn handle_request(app: &mut App, conn: &mut Conn, msg: ClientMsg) {
    let kind = RequestKind::of(&msg);
    let Conn::Up(client) = conn else {
        app.set_status("not connected to daemon", true);
        return;
    };
    match client.request(msg).await {
        Ok(id) => {
            kind.apply_success(app, id);
            app.set_status(kind.done_message(id), false);
        }
        Err(ClientError::Daemon(e) | ClientError::Unauthenticated(e)) => app.set_status(e, true),
        Err(ClientError::Closed) => mark_disconnected(app, conn),
    }
}

enum RequestKind {
    Spawn,
    Interrupt(u64),
    Kill(u64),
    Resume(u64),
    CreateShell(u64),
    RestartTerminal(u64),
    CloseTerminal(u64),
    Other,
}

impl RequestKind {
    fn of(msg: &ClientMsg) -> Self {
        match msg {
            ClientMsg::SpawnSession { .. } => RequestKind::Spawn,
            ClientMsg::InterruptSession { session_id } => RequestKind::Interrupt(*session_id),
            ClientMsg::KillSession { session_id } => RequestKind::Kill(*session_id),
            ClientMsg::ResumeSession { session_id } => RequestKind::Resume(*session_id),
            ClientMsg::CreateShell { session_id, .. } => RequestKind::CreateShell(*session_id),
            ClientMsg::RestartTerminal { terminal_id } => {
                RequestKind::RestartTerminal(*terminal_id)
            }
            ClientMsg::CloseTerminal { terminal_id } => RequestKind::CloseTerminal(*terminal_id),
            _ => RequestKind::Other,
        }
    }

    fn apply_success(&self, app: &mut App, created_id: Option<u64>) {
        match self {
            RequestKind::Spawn => {
                if let Some(id) = created_id {
                    app.selected = Some(id);
                    app.selected_terminal = None;
                }
            }
            RequestKind::CreateShell(_) => {
                if let Some(id) = created_id {
                    app.selected_terminal = Some(id);
                }
            }
            _ => {}
        }
    }

    fn done_message(&self, created_id: Option<u64>) -> String {
        match self {
            RequestKind::Spawn => {
                format!("session {} spawned", created_id.unwrap_or_default())
            }
            RequestKind::Interrupt(id) => format!("interrupt sent to session {id}"),
            RequestKind::Kill(id) => format!("kill sent to session {id}"),
            RequestKind::Resume(id) => format!("session {id} resumed"),
            RequestKind::CreateShell(session_id) => match created_id {
                Some(id) => format!("shell terminal {id} created for session {session_id}"),
                None => format!("shell created for session {session_id}"),
            },
            RequestKind::RestartTerminal(id) => format!("terminal {id} restarted"),
            RequestKind::CloseTerminal(id) => format!("terminal {id} closed"),
            RequestKind::Other => "done".to_string(),
        }
    }
}

async fn run_attach(
    app: &mut App,
    conn: &mut Conn,
    terminal: &mut Term,
    input_rx: &mut mpsc::UnboundedReceiver<Bytes>,
    target: AttachTarget,
) -> Result<(), TuiError> {
    let Conn::Up(client) = conn else {
        app.set_status("not connected to daemon", true);
        return Ok(());
    };
    match client
        .request(ClientMsg::AttachTerminal {
            terminal_id: target.terminal_id,
        })
        .await
    {
        Ok(_) => {}
        Err(ClientError::Daemon(e) | ClientError::Unauthenticated(e)) => {
            app.set_status(e, true);
            return Ok(());
        }
        Err(ClientError::Closed) => {
            mark_disconnected(app, conn);
            return Ok(());
        }
    }

    execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show)?;
    let end = attach::passthrough(client, app, target, input_rx).await;
    // The relayed app may have left mouse/paste/focus reporting on; clear
    // it so the resumed dashboard is not fed stray escape sequences.
    reset_input_modes();
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;

    match end? {
        AttachEnd::Detached => {
            let _ = client
                .request(ClientMsg::DetachTerminal {
                    terminal_id: target.terminal_id,
                })
                .await;
            app.set_status(
                format!("detached from terminal {}", target.terminal_id),
                false,
            );
        }
        AttachEnd::TerminalEnded => {
            let _ = client
                .request(ClientMsg::DetachTerminal {
                    terminal_id: target.terminal_id,
                })
                .await;
            app.set_status(format!("terminal {} ended", target.terminal_id), false);
        }
        AttachEnd::ConnectionLost => mark_disconnected(app, conn),
    }
    Ok(())
}

fn spawn_stdin_reader() -> mpsc::UnboundedReceiver<Bytes> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; STDIN_BUF_SIZE];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn ring_bell() {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(BELL);
    let _ = stdout.flush();
}

/// Input-reporting modes a relayed full-screen app (e.g. Claude Code)
/// may enable on the real terminal — mouse (1000/1002/1003/1006), focus
/// (1004), bracketed paste (2004). crossterm restores the screen and
/// cursor for us, but not these, and a leftover mouse mode injects
/// escape sequences into the resumed dashboard and the user's shell.
const INPUT_MODES_RESET: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l";

fn reset_input_modes() {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(INPUT_MODES_RESET);
    let _ = stdout.flush();
}

fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::disable_raw_mode();
            reset_input_modes();
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
            default_hook(info);
        }));
    });
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(std::io::stdout(), EnterAlternateScreen)?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        reset_input_modes();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_creation_selects_the_created_terminal() {
        let mut app = App::new();
        RequestKind::CreateShell(7).apply_success(&mut app, Some(51));
        assert_eq!(app.selected_terminal, Some(51));
        assert_eq!(
            RequestKind::CreateShell(7).done_message(Some(51)),
            "shell terminal 51 created for session 7"
        );
    }

    #[test]
    fn input_modes_reset_disables_mouse_paste_and_focus() {
        let s = String::from_utf8(INPUT_MODES_RESET.to_vec()).unwrap();
        for mode in ["1000", "1002", "1003", "1006", "1004", "2004"] {
            assert!(s.contains(&format!("\x1b[?{mode}l")), "missing {mode}");
        }
        // crossterm owns the alt screen here, so this must not touch it.
        assert!(!s.contains("1049"));
    }
}
