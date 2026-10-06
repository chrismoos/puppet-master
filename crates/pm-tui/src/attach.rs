//! Raw passthrough between the local terminal and a session's PTY,
//! entered after the dashboard leaves its alternate screen. State
//! events keep flowing into the reducer while attached.

use std::io::Write;
use std::time::Duration;

use bytes::Bytes;
use crossterm::terminal;
use pm_client::Client;
use pm_protocol::domain::{ClientMsg, ServerMsg, TerminalKind};
use tokio::sync::mpsc;

use crate::app::{App, AttachTarget};

/// ASCII FS, what Ctrl-\ produces. Chosen over Ctrl-C so interrupts
/// pass through to the agent.
const DETACH_BYTE: u8 = 0x1c;

const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// BEL, rung when a session flips to needs-input.
pub(crate) const BELL: &[u8] = b"\x07";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachEnd {
    Detached,
    TerminalEnded,
    ConnectionLost,
}

pub(crate) async fn passthrough(
    client: &mut Client,
    app: &mut App,
    target: AttachTarget,
    input_rx: &mut mpsc::UnboundedReceiver<Bytes>,
) -> std::io::Result<AttachEnd> {
    let mut stdout = std::io::stdout();
    write!(
        stdout,
        "attached to terminal {}, detach with Ctrl-\\\r\n",
        target.terminal_id
    )?;
    stdout.flush()?;

    let (cols, rows) = terminal::size()?;
    let mut last_size = (cols, rows);
    if client
        .send(ClientMsg::TerminalResize {
            terminal_id: target.terminal_id,
            cols,
            rows,
        })
        .is_err()
    {
        return Ok(AttachEnd::ConnectionLost);
    }
    let mut resize_poll = tokio::time::interval(RESIZE_POLL_INTERVAL);

    loop {
        tokio::select! {
            msg = client.next_msg() => match msg {
                Some(ServerMsg::PtyOutput { terminal_id, data, .. }) => {
                    if terminal_id == target.terminal_id {
                        stdout.write_all(&data)?;
                        stdout.flush()?;
                    }
                }
                Some(ServerMsg::Snapshot(snapshot)) => {
                    app.apply_snapshot(snapshot);
                    if let Some(end) = check_terminal_over(app, target, &mut stdout)? {
                        return Ok(end);
                    }
                }
                Some(ServerMsg::Event(event)) => {
                    app.apply_event(event);
                    if app.take_bell() {
                        stdout.write_all(BELL)?;
                        stdout.flush()?;
                    }
                    if let Some(end) = check_terminal_over(app, target, &mut stdout)? {
                        return Ok(end);
                    }
                }
                Some(ServerMsg::CommandResult { .. }) => {}
                None => return Ok(AttachEnd::ConnectionLost),
            },
            data = input_rx.recv() => match data {
                Some(data) => {
                    if let Some(pos) = data.iter().position(|&b| b == DETACH_BYTE) {
                        if pos > 0
                            && client
                                .send(ClientMsg::TerminalInput { terminal_id: target.terminal_id, data: data.slice(..pos) })
                                .is_err()
                        {
                            return Ok(AttachEnd::ConnectionLost);
                        }
                        return Ok(AttachEnd::Detached);
                    }
                    if client.send(ClientMsg::TerminalInput { terminal_id: target.terminal_id, data }).is_err() {
                        return Ok(AttachEnd::ConnectionLost);
                    }
                }
                None => return Ok(AttachEnd::Detached),
            },
            _ = resize_poll.tick() => {
                let size = terminal::size()?;
                if size != last_size {
                    last_size = size;
                    let _ = client.send(ClientMsg::TerminalResize {
                        terminal_id: target.terminal_id,
                        cols: size.0,
                        rows: size.1,
                    });
                }
            }
        }
    }
}

fn check_terminal_over(
    app: &App,
    target: AttachTarget,
    stdout: &mut impl Write,
) -> std::io::Result<Option<AttachEnd>> {
    let message = match app.world.terminal(target.terminal_id) {
        None => format!("terminal {} removed", target.terminal_id),
        Some(terminal) if !terminal.state.is_live() => {
            let code = terminal
                .exit_code
                .map(|c| format!(" (exit {c})"))
                .unwrap_or_default();
            format!(
                "terminal {} {}{}",
                target.terminal_id,
                terminal.state.as_str(),
                code
            )
        }
        Some(terminal) if terminal.kind == TerminalKind::Agent => {
            match app.world.session(target.session_id) {
                None => format!("session {} removed", target.session_id),
                Some(session) if !session.state.is_live() => {
                    let code = session
                        .exit_code
                        .map(|c| format!(" (exit {c})"))
                        .unwrap_or_default();
                    format!(
                        "session {} {}{}",
                        target.session_id,
                        session.state.as_str(),
                        code
                    )
                }
                Some(_) => return Ok(None),
            }
        }
        Some(_) => return Ok(None),
    };
    write!(stdout, "\r\n{message}\r\n")?;
    stdout.flush()?;
    Ok(Some(AttachEnd::TerminalEnded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{bucket, project, session, shell_terminal, snapshot};
    use pm_protocol::domain::{Event, SessionState};

    #[test]
    fn shell_attach_ends_from_terminal_state_without_ending_the_session() {
        let mut app = App::new();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api")],
            vec![session(1, 1, SessionState::Working)],
        ));
        app.apply_event(Event::TerminalChanged(shell_terminal(50, 1, false)));
        let mut output = Vec::new();
        assert_eq!(
            check_terminal_over(
                &app,
                AttachTarget {
                    session_id: 1,
                    terminal_id: 50,
                },
                &mut output,
            )
            .unwrap(),
            Some(AttachEnd::TerminalEnded)
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("terminal 50 exited"));
        assert!(app.world.session(1).unwrap().state.is_live());
    }

    #[test]
    fn agent_attach_ends_when_the_session_exit_arrives_first() {
        let mut app = App::new();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api")],
            vec![session(1, 1, SessionState::Working)],
        ));
        app.apply_event(Event::SessionChanged(session(1, 1, SessionState::Exited)));
        let mut output = Vec::new();
        assert_eq!(
            check_terminal_over(
                &app,
                AttachTarget {
                    session_id: 1,
                    terminal_id: 1_001,
                },
                &mut output,
            )
            .unwrap(),
            Some(AttachEnd::TerminalEnded)
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("session 1 exited"));
    }
}
