//! tmux integration: fleet status for the tmux status line, opening
//! sessions as tmux windows, and materializing saved web workspaces
//! into native tmux splits. Materialization is one-way — tmux owns the
//! layout afterwards; nothing syncs back.

use pm_client::{Client, Target};
use pm_protocol::domain::{ClientMsg, Scope, ServerMsg, Session, SessionState, Snapshot};
use serde::Deserialize;

const WINDOW_NAME_MAX_CHARS: usize = 24;
const MENU_KEYS: &[u8] = b"123456789abcdefghijklmnopqrstuvwxyz";

/// Detached tmux sessions default to 80x24, too small to hold nested
/// splits; size the throwaway client large enough for any layout.
const DETACHED_SESSION_COLS: &str = "220";
const DETACHED_SESSION_ROWS: &str = "60";

/// The saved workspace layout, as written by the web UI. A node is either a
/// leaf naming a terminal or a split carrying its direction and children, and
/// unknown fields are ignored so the web can extend the shape without
/// breaking us.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Layout {
    Pane {
        #[serde(rename = "terminalId")]
        terminal_id: Option<String>,
    },
    Split {
        axis: Axis,
        ratio: f64,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    Columns,
    Rows,
}

#[derive(Debug, Deserialize)]
struct WorkspaceEntry {
    id: u64,
    name: String,
    layout: serde_json::Value,
    position: u32,
}

/// Executes tmux commands; injected so the split planner is testable
/// without a live server.
pub trait TmuxRunner {
    /// Runs `tmux <args>` and returns trimmed stdout.
    fn run(&mut self, args: &[String]) -> anyhow::Result<String>;
}

struct RealTmux;

impl TmuxRunner for RealTmux {
    fn run(&mut self, args: &[String]) -> anyhow::Result<String> {
        let output = std::process::Command::new("tmux").args(args).output()?;
        if !output.status.success() {
            anyhow::bail!(
                "tmux {} failed: {}",
                args.first().map(String::as_str).unwrap_or(""),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

fn inside_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

async fn snapshot(socket: &Target) -> anyhow::Result<(Client, Snapshot)> {
    let mut client = Client::open(socket).await?;
    client
        .request(ClientMsg::Subscribe { scope: Scope::All })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    loop {
        match client.next_msg().await {
            Some(ServerMsg::Snapshot(s)) => return Ok((client, s)),
            Some(_) => continue,
            None => anyhow::bail!("connection closed before snapshot"),
        }
    }
}

pub fn display_name(session: &Session) -> String {
    session.display_name()
}

fn window_name(session: &Session) -> String {
    let name = display_name(session);
    let chars: Vec<char> = name.chars().collect();
    if chars.len() > WINDOW_NAME_MAX_CHARS {
        format!(
            "{}…",
            chars[..WINDOW_NAME_MAX_CHARS - 1]
                .iter()
                .collect::<String>()
        )
    } else {
        name
    }
}

/// The status-line segment, in tmux format markup: live count in green,
/// needs-input in red when nonzero and dimmed when zero.
pub fn status_line(working: usize, needs_input: usize) -> String {
    let needs = if needs_input > 0 {
        format!("#[fg=red,bold]▲ {needs_input}#[default]")
    } else {
        format!("#[fg=colour240]▲ {needs_input}#[default]")
    };
    format!("#[fg=green]● {working}#[default] {needs}")
}

pub async fn status(socket: &Target) -> anyhow::Result<()> {
    let (_client, snapshot) = snapshot(socket).await?;
    let working = snapshot
        .sessions
        .iter()
        .filter(|s| {
            matches!(
                s.state,
                SessionState::Working | SessionState::Starting | SessionState::Idle
            )
        })
        .count();
    let needs = snapshot
        .sessions
        .iter()
        .filter(|s| s.state == SessionState::NeedsInput)
        .count();
    println!("{}", status_line(working, needs));
    Ok(())
}

/// The shell command a tmux pane or window runs to attach, reaching the
/// daemon this command reached.
fn pm_command(socket: &Target, arguments: &[&str]) -> anyhow::Result<String> {
    let pm = std::env::current_exe()?;
    let mut words = vec![pm.display().to_string()];
    words.extend(crate::remotes::target_args(socket));
    words.extend(arguments.iter().map(|argument| argument.to_string()));
    Ok(words
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" "))
}

fn attach_command(socket: &Target, session_id: u64) -> anyhow::Result<String> {
    pm_command(socket, &["attach", &session_id.to_string()])
}

fn terminal_attach_command(socket: &Target, terminal_id: u64) -> anyhow::Result<String> {
    pm_command(socket, &["terminal", "attach", &terminal_id.to_string()])
}

/// Single-quotes a string for the shell tmux hands commands to.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub async fn open(socket: &Target, sessions: Vec<u64>, needs_input: bool) -> anyhow::Result<()> {
    if !inside_tmux() {
        anyhow::bail!("pm tmux open must run inside tmux");
    }
    let (_client, snapshot) = snapshot(socket).await?;
    let targets: Vec<&Session> = if needs_input {
        snapshot
            .sessions
            .iter()
            .filter(|s| s.state == SessionState::NeedsInput)
            .collect()
    } else {
        sessions
            .iter()
            .filter_map(|id| snapshot.sessions.iter().find(|s| s.id == *id))
            .collect()
    };
    if targets.is_empty() {
        anyhow::bail!(if needs_input {
            "no sessions need input".to_string()
        } else {
            "no matching sessions".to_string()
        });
    }
    let mut tmux = RealTmux;
    for session in targets {
        tmux.run(&[
            "new-window".into(),
            "-n".into(),
            window_name(session),
            attach_command(socket, session.id)?,
        ])?;
    }
    Ok(())
}

pub async fn pick(socket: &Target) -> anyhow::Result<()> {
    if !inside_tmux() {
        anyhow::bail!("pm tmux pick must run inside tmux");
    }
    let (_client, snapshot) = snapshot(socket).await?;
    let live: Vec<&Session> = snapshot
        .sessions
        .iter()
        .filter(|s| s.state.is_live())
        .collect();
    if live.is_empty() {
        anyhow::bail!("no live sessions");
    }
    let mut args: Vec<String> = vec!["display-menu".into(), "-T".into(), "pm sessions".into()];
    for (session, key) in live.iter().zip(MENU_KEYS) {
        let marker = if session.state == SessionState::NeedsInput {
            "▲ "
        } else {
            ""
        };
        args.push(format!("{marker}{}", window_name(session)));
        args.push((*key as char).to_string());
        args.push(format!(
            "new-window -n {} {}",
            shell_quote(&window_name(session)),
            shell_quote(&attach_command(socket, session.id)?)
        ));
    }
    RealTmux.run(&args)?;
    Ok(())
}

/// One materialized pane: its tmux pane id and the terminal it should
/// attach to (None renders the empty-pane placeholder).
#[derive(Debug, PartialEq, Eq)]
pub struct Leaf {
    pub pane: String,
    pub terminal_id: Option<u64>,
}

/// Recreates the layout's split geometry under `pane`, returning the
/// leaves in tree order. Ratio is the first child's share; tmux's -p
/// takes the new (second) pane's percentage.
pub fn split_tree(
    tmux: &mut dyn TmuxRunner,
    layout: &Layout,
    pane: String,
    leaves: &mut Vec<Leaf>,
) -> anyhow::Result<()> {
    match layout {
        Layout::Pane { terminal_id } => {
            leaves.push(Leaf {
                pane,
                terminal_id: terminal_id.as_deref().and_then(|t| t.parse().ok()),
            });
            Ok(())
        }
        Layout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let direction = match axis {
                Axis::Columns => "-h",
                Axis::Rows => "-v",
            };
            let second_percent = ((1.0 - ratio.clamp(0.05, 0.95)) * 100.0).round() as u32;
            let new_pane = tmux.run(&[
                "split-window".into(),
                "-d".into(),
                direction.into(),
                "-p".into(),
                second_percent.to_string(),
                "-t".into(),
                pane.clone(),
                "-P".into(),
                "-F".into(),
                "#{pane_id}".into(),
            ])?;
            split_tree(tmux, first, pane, leaves)?;
            split_tree(tmux, second, new_pane, leaves)
        }
    }
}

fn resolve_workspace<'a>(
    workspaces: &'a [WorkspaceEntry],
    handle: &str,
) -> Option<&'a WorkspaceEntry> {
    handle
        .parse::<u64>()
        .ok()
        .and_then(|id| workspaces.iter().find(|workspace| workspace.id == id))
        .or_else(|| {
            workspaces
                .iter()
                .find(|workspace| workspace.name.eq_ignore_ascii_case(handle))
        })
}

fn workspace_listing(workspaces: &[WorkspaceEntry]) -> String {
    if workspaces.is_empty() {
        return "no saved workspaces".into();
    }
    let mut lines = vec![format!("{:<4} {:<6} NAME", "#", "ID")];
    lines.extend(workspaces.iter().map(|workspace| {
        format!(
            "{:<4} {:<6} {}",
            workspace.position + 1,
            workspace.id,
            workspace.name
        )
    }));
    lines.join("\n")
}

pub async fn workspace(socket: &Target, handle: Option<&str>) -> anyhow::Result<()> {
    let (client, snapshot) = snapshot(socket).await?;
    let data = client
        .request_data(ClientMsg::ListWorkspaces)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let workspaces: Vec<WorkspaceEntry> = serde_json::from_slice(&data)?;
    let Some(handle) = handle else {
        println!("{}", workspace_listing(&workspaces));
        return Ok(());
    };
    let entry = resolve_workspace(&workspaces, handle).ok_or_else(|| {
        anyhow::anyhow!(
            "no workspace matching {handle:?}\n\n{}",
            workspace_listing(&workspaces)
        )
    })?;
    let layout: Layout = serde_json::from_value(entry.layout.clone())
        .map_err(|e| anyhow::anyhow!("workspace layout is not understood: {e}"))?;

    let mut tmux = RealTmux;
    let tmux_session = entry.name.replace([':', '.'], "-");
    let root = if inside_tmux() {
        tmux.run(&[
            "new-window".into(),
            "-n".into(),
            entry.name.clone(),
            "-P".into(),
            "-F".into(),
            "#{pane_id}".into(),
        ])?
    } else {
        tmux.run(&[
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            tmux_session.clone(),
            "-x".into(),
            DETACHED_SESSION_COLS.into(),
            "-y".into(),
            DETACHED_SESSION_ROWS.into(),
            "-P".into(),
            "-F".into(),
            "#{pane_id}".into(),
        ])?
    };

    let mut leaves = Vec::new();
    split_tree(&mut tmux, &layout, root, &mut leaves)?;
    for leaf in &leaves {
        let live = leaf.terminal_id.and_then(|id| {
            snapshot
                .terminals
                .iter()
                .find(|t| t.id == id && t.state.is_live())
        });
        let command = match live {
            Some(terminal) => terminal_attach_command(socket, terminal.id)?,
            None => {
                "sh -c 'echo \"pm: this workspace pane has no live terminal\"; exec ${SHELL:-sh}'"
                    .to_string()
            }
        };
        tmux.run(&[
            "respawn-pane".into(),
            "-k".into(),
            "-t".into(),
            leaf.pane.clone(),
            command,
        ])?;
    }

    if !inside_tmux() {
        // Hand the terminal over to the freshly built tmux session.
        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", &tmux_session])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux attach-session failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records commands and hands out synthetic pane ids for splits.
    struct FakeTmux {
        commands: Vec<Vec<String>>,
        next_pane: u32,
    }

    impl FakeTmux {
        fn new() -> Self {
            FakeTmux {
                commands: Vec::new(),
                next_pane: 1,
            }
        }
    }

    impl TmuxRunner for FakeTmux {
        fn run(&mut self, args: &[String]) -> anyhow::Result<String> {
            self.commands.push(args.to_vec());
            if args[0] == "split-window" {
                self.next_pane += 1;
                return Ok(format!("%{}", self.next_pane));
            }
            Ok(String::new())
        }
    }

    fn parse(json: &str) -> Layout {
        serde_json::from_str(json).unwrap()
    }

    fn entry(id: u64, name: &str, position: u32) -> WorkspaceEntry {
        WorkspaceEntry {
            id,
            name: name.into(),
            layout: serde_json::json!({"kind": "pane", "terminalId": null}),
            position,
        }
    }

    #[test]
    fn workspace_resolution_prefers_a_matching_id_over_a_numeric_name() {
        let workspaces = vec![entry(7, "12", 0), entry(12, "primary", 1)];

        assert_eq!(resolve_workspace(&workspaces, "12").unwrap().id, 12);
    }

    #[test]
    fn workspace_resolution_falls_back_to_case_insensitive_names() {
        let workspaces = vec![entry(7, "12", 0), entry(8, "Release Watch", 1)];

        assert_eq!(resolve_workspace(&workspaces, "12").unwrap().id, 7);
        assert_eq!(
            resolve_workspace(&workspaces, "release watch").unwrap().id,
            8
        );
        assert!(resolve_workspace(&workspaces, "missing").is_none());
    }

    #[test]
    fn workspace_listing_shows_ordinal_id_and_name() {
        let workspaces = vec![entry(41, "primary", 0), entry(57, "release watch", 1)];

        assert_eq!(
            workspace_listing(&workspaces),
            "#    ID     NAME\n1    41     primary\n2    57     release watch"
        );
    }

    #[test]
    fn layout_json_round_trips_the_web_schema() {
        let layout = parse(
            r#"{"kind":"split","splitId":"s1","axis":"columns","ratio":0.6,
                "first":{"kind":"pane","paneId":"p1","terminalId":"7"},
                "second":{"kind":"pane","paneId":"p2","terminalId":null}}"#,
        );
        match layout {
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                assert_eq!(axis, Axis::Columns);
                assert!((ratio - 0.6).abs() < f64::EPSILON);
                assert!(matches!(*first, Layout::Pane { terminal_id: Some(ref t) } if t == "7"));
                assert!(matches!(*second, Layout::Pane { terminal_id: None }));
            }
            other => panic!("expected split, got {other:?}"),
        }
    }

    #[test]
    fn split_tree_recreates_nested_geometry() {
        // columns 0.6 [ pane 7 | rows 0.5 [ pane 8 / empty ] ]
        let layout = parse(
            r#"{"kind":"split","axis":"columns","ratio":0.6,
                "first":{"kind":"pane","terminalId":"7"},
                "second":{"kind":"split","axis":"rows","ratio":0.5,
                    "first":{"kind":"pane","terminalId":"8"},
                    "second":{"kind":"pane","terminalId":null}}}"#,
        );
        let mut tmux = FakeTmux::new();
        let mut leaves = Vec::new();
        split_tree(&mut tmux, &layout, "%1".into(), &mut leaves).unwrap();

        assert_eq!(
            leaves,
            vec![
                Leaf {
                    pane: "%1".into(),
                    terminal_id: Some(7)
                },
                Leaf {
                    pane: "%2".into(),
                    terminal_id: Some(8)
                },
                Leaf {
                    pane: "%3".into(),
                    terminal_id: None
                },
            ]
        );
        // First split: horizontal (columns), second pane gets 40%.
        let first = &tmux.commands[0];
        assert_eq!(first[0], "split-window");
        assert!(first.contains(&"-h".to_string()));
        assert!(first.contains(&"40".to_string()));
        // Second split: vertical within the new pane, 50%.
        let nested = &tmux.commands[1];
        assert!(nested.contains(&"-v".to_string()));
        assert!(nested.contains(&"50".to_string()));
        assert!(nested.contains(&"%2".to_string()));
    }

    #[test]
    fn status_line_colors_needs_input_only_when_present() {
        assert_eq!(
            status_line(4, 0),
            "#[fg=green]● 4#[default] #[fg=colour240]▲ 0#[default]"
        );
        assert!(status_line(2, 1).contains("#[fg=red,bold]▲ 1"));
    }

    #[test]
    fn shell_quote_survives_embedded_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
