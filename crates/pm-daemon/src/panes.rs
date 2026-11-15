//! Panes an agent arranges beside itself in the session view: terminals
//! running programs or shells, and sandboxed HTML pages.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pm_protocol::domain::{Event, PermissionMode, SessionLayout as LayoutRecord};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::Daemon;

pub const MAX_PANES: usize = 6;
pub const AGENT_PANE: &str = "agent";
/// The root leaf of a shell tab's own layout: the tab's shell.
pub const TAB_PANE: &str = "tab";
const EXIT_MARKER: &str = "[pane exited with status ";
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
const WAIT_POLL: Duration = Duration::from_millis(250);
const WAIT_MAX: Duration = Duration::from_secs(25);
const PAGE_FILE_MAX_BYTES: usize = 8 * 1024 * 1024;
const READ_DEFAULT_LINES: usize = 80;
const READ_MAX_BYTES: usize = 256 * 1024;
const PAGE_TOKEN_BYTES: usize = 16;
const DEFAULT_NEW_PANE_SIZE: u64 = 40;

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Layout {
    pub rev: u64,
    pub tree: Value,
    pub panes: BTreeMap<String, Pane>,
    pub unplaced: Vec<String>,
    /// Shell tabs split into panes, by the tab's shell terminal id. The user
    /// arranges these, and the agent's own layout never holds their panes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tabs: BTreeMap<String, Value>,
}

impl Layout {
    /// The shell tab holding a pane, if it is in one.
    fn tab_of(&self, id: &str) -> Option<String> {
        let mut leaves = Vec::new();
        self.tabs.iter().find_map(|(tab, tree)| {
            leaves.clear();
            collect_leaves(tree, &mut leaves).ok()?;
            leaves.iter().any(|leaf| leaf == id).then(|| tab.clone())
        })
    }

    fn tab_panes(&self) -> std::collections::HashSet<String> {
        let mut leaves = Vec::new();
        for tree in self.tabs.values() {
            let _ = collect_leaves(tree, &mut leaves);
        }
        leaves.into_iter().filter(|id| id != TAB_PANE).collect()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pane {
    pub id: String,
    /// "program", "shell" or "page".
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The line typed into a shell on a worker too old to run the command
    /// itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_forward: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_stream: Option<String>,
    pub version: u64,
    pub user_input: bool,
    /// Shrunk to its header by the user; the program keeps running.
    #[serde(default)]
    pub collapsed: bool,
    /// The title was given explicitly. An untitled terminal pane shows its
    /// running program's name instead.
    #[serde(default = "titled_by_default")]
    pub titled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Opened by the user for their own use, so the agent is not told what happens to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_opened: bool,
}

/// A pane stored before titles were tracked keeps showing the title it had.
fn titled_by_default() -> bool {
    true
}

/// One row per session that has arranged panes. A row goes with its session.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS session_layouts (
 session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 rev INTEGER NOT NULL,
 layout TEXT NOT NULL,
 updated_at INTEGER NOT NULL
);";

impl crate::storage::Storage {
    fn session_layouts(&self) -> rusqlite::Result<Vec<(u64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare("SELECT session_id, layout FROM session_layouts")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })?
            .collect();
        rows
    }

    /// Writes revision `rev` only over revision `rev - 1`, so a write based
    /// on a layout someone else has since changed is refused by the database.
    fn save_session_layout(&self, session_id: u64, rev: u64, layout: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap();
        let changed = conn
            .execute(
                "INSERT INTO session_layouts (session_id, rev, layout, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(session_id) DO UPDATE SET rev = excluded.rev, layout = excluded.layout, updated_at = excluded.updated_at
                 WHERE session_layouts.rev = excluded.rev - 1",
                rusqlite::params![session_id as i64, rev as i64, layout, crate::storage::now_unix_ms()],
            )
            .map_err(|error| format!("could not save the layout: {error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err("conflict: the saved layout moved on".into())
        }
    }
}

#[derive(Default)]
pub struct Panes {
    layouts: Mutex<HashMap<u64, Layout>>,
}

impl Panes {
    /// Loads the layouts every session saved.
    pub fn load(storage: &crate::storage::Storage) -> Self {
        let layouts = storage
            .session_layouts()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(session_id, json)| {
                serde_json::from_str::<Layout>(&json)
                    .ok()
                    .map(|layout| (session_id, layout))
            })
            .collect();
        Self {
            layouts: Mutex::new(layouts),
        }
    }

    /// Whether a pane shows this terminal, so its exit keeps the terminal.
    pub fn owns_terminal(&self, terminal_id: u64) -> bool {
        self.layouts.lock().unwrap().values().any(|layout| {
            layout
                .panes
                .values()
                .any(|pane| pane.terminal_id == Some(terminal_id))
        })
    }

    fn pane_ids(&self) -> Vec<(u64, String)> {
        self.layouts
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(session_id, layout)| {
                layout.panes.keys().map(move |id| (*session_id, id.clone()))
            })
            .collect()
    }

    pub fn records(&self) -> Vec<LayoutRecord> {
        self.layouts
            .lock()
            .unwrap()
            .iter()
            .map(|(session_id, layout)| record(*session_id, layout))
            .collect()
    }

    fn get(&self, session_id: u64) -> Layout {
        self.layouts
            .lock()
            .unwrap()
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| Layout {
                rev: 0,
                tree: json!({ "pane": AGENT_PANE }),
                panes: BTreeMap::new(),
                unplaced: Vec::new(),
                tabs: BTreeMap::new(),
            })
    }

    /// Applies `change` to the session's layout, bumps its revision and
    /// returns the new layout, or the change's refusal.
    fn update<T>(
        &self,
        storage: &crate::storage::Storage,
        session_id: u64,
        change: impl FnOnce(&mut Layout) -> Result<T, String>,
    ) -> Result<(Layout, T), String> {
        let mut layouts = self.layouts.lock().unwrap();
        let mut layout = layouts.get(&session_id).cloned().unwrap_or_else(|| Layout {
            rev: 0,
            tree: json!({ "pane": AGENT_PANE }),
            panes: BTreeMap::new(),
            unplaced: Vec::new(),
            tabs: BTreeMap::new(),
        });
        let value = change(&mut layout)?;
        layout.rev += 1;
        let json = serde_json::to_string(&layout).map_err(|error| error.to_string())?;
        storage.save_session_layout(session_id, layout.rev, &json)?;
        layouts.insert(session_id, layout.clone());
        Ok((layout, value))
    }

    fn page(&self, token: &str) -> Option<PageTarget> {
        let layouts = self.layouts.lock().unwrap();
        layouts.iter().find_map(|(session_id, layout)| {
            layout.panes.values().find_map(|pane| {
                if pane.page_token.as_deref() == Some(token) && pane.kind == "browser" {
                    return Some(PageTarget::Browser {
                        session_id: *session_id,
                        port: pane.page_port?,
                        stream: pane.browser_stream.clone()?,
                    });
                }
                (pane.page_token.as_deref() == Some(token)).then(|| match pane.page_forward {
                    Some(forward) => PageTarget::Forward(forward),
                    None => PageTarget::Dir(*session_id, pane.page_dir.clone().unwrap_or_default()),
                })
            })
        })
    }
}

/// What a page pane's token serves: files from a session directory, or the
/// session's dev server through its forward.
pub enum PageTarget {
    Dir(u64, String),
    Forward(u64),
    Browser {
        session_id: u64,
        port: u16,
        stream: String,
    },
}

/// The layout as clients see it, without the launch lines, which can carry
/// environment values the agent passed.
fn record(session_id: u64, layout: &Layout) -> LayoutRecord {
    let mut value = serde_json::to_value(layout).unwrap_or_default();
    if let Some(panes) = value.get_mut("panes").and_then(Value::as_object_mut) {
        for pane in panes.values_mut() {
            if let Some(pane) = pane.as_object_mut() {
                pane.remove("launch");
                pane.remove("env");
                pane.remove("page_forward");
                pane.remove("browser_stream");
            }
        }
    }
    LayoutRecord {
        session_id,
        json: value.to_string(),
    }
}

/// Restarts the watchers for panes loaded from disk.
pub fn resume(daemon: &Arc<Daemon>) {
    for (session_id, id) in daemon.panes.pane_ids() {
        spawn_watcher(daemon.clone(), session_id, id);
    }
}

fn publish(daemon: &Daemon, session_id: u64, layout: &Layout) {
    daemon.publish(Event::SessionLayoutChanged(record(session_id, layout)));
}

// ---- tree helpers ----

fn leaf_id(node: &Value) -> Option<&str> {
    node.get("pane").and_then(Value::as_str)
}

fn children(node: &Value) -> Option<(&'static str, &Vec<Value>)> {
    if let Some(items) = node.get("row").and_then(Value::as_array) {
        return Some(("row", items));
    }
    node.get("column")
        .and_then(Value::as_array)
        .map(|items| ("column", items))
}

fn collect_leaves(node: &Value, out: &mut Vec<String>) -> Result<(), String> {
    if let Some(id) = leaf_id(node) {
        out.push(id.to_string());
        return Ok(());
    }
    match children(node) {
        Some((_, items)) if !items.is_empty() => {
            for item in items {
                collect_leaves(item, out)?;
            }
            Ok(())
        }
        _ => Err(format!(
            "every node must be {{\"pane\": id}}, {{\"row\": [...]}} or {{\"column\": [...]}} with at least one child, got {node}"
        )),
    }
}

fn validate(tree: &Value, panes: &BTreeMap<String, Pane>) -> Result<Vec<String>, String> {
    validate_rooted(tree, panes, AGENT_PANE)
}

fn validate_rooted(
    tree: &Value,
    panes: &BTreeMap<String, Pane>,
    root: &str,
) -> Result<Vec<String>, String> {
    let mut leaves = Vec::new();
    collect_leaves(tree, &mut leaves)?;
    if leaves.iter().filter(|id| *id == root).count() != 1 {
        return Err(format!(
            "the layout must contain the {root:?} pane exactly once"
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for id in &leaves {
        if !seen.insert(id.clone()) {
            return Err(format!("pane {id:?} appears more than once"));
        }
        if id != root && !panes.contains_key(id) {
            return Err(format!("no pane named {id:?}; open it first"));
        }
    }
    if leaves.len() - 1 > MAX_PANES {
        return Err(format!("at most {MAX_PANES} panes besides the agent"));
    }
    Ok(leaves)
}

fn place(tree: &Value, id: &str) -> Value {
    place_rooted(tree, id, AGENT_PANE)
}

fn place_rooted(tree: &Value, id: &str, root: &str) -> Value {
    let leaf = json!({ "pane": id });
    if leaf_id(tree) == Some(root) {
        return json!({ "row": [
            { "pane": root, "size": 100 - DEFAULT_NEW_PANE_SIZE },
            { "pane": id, "size": DEFAULT_NEW_PANE_SIZE }
        ]});
    }
    if let Some(items) = tree.get("row").and_then(Value::as_array) {
        let mut items = items.clone();
        let last = items.pop().unwrap_or(Value::Null);
        if let Some(column) = last.get("column").and_then(Value::as_array) {
            let mut column = column.clone();
            column.push(leaf);
            let mut node = last.clone();
            node["column"] = Value::Array(column);
            items.push(node);
        } else if leaf_id(&last).is_some_and(|last_id| last_id != root) {
            let mut node = json!({ "column": [ { "pane": leaf_id(&last).unwrap() }, leaf ] });
            if let Some(size) = last.get("size") {
                node["size"] = size.clone();
            }
            items.push(node);
        } else {
            items.push(last);
            items.push(leaf);
        }
        let mut node = tree.clone();
        node["row"] = Value::Array(items);
        return node;
    }
    json!({ "row": [ tree, { "pane": id, "size": DEFAULT_NEW_PANE_SIZE } ] })
}

fn remove(tree: &Value, id: &str) -> Option<Value> {
    if let Some(leaf) = leaf_id(tree) {
        return (leaf != id).then(|| tree.clone());
    }
    let (axis, items) = children(tree)?;
    let kept: Vec<Value> = items.iter().filter_map(|item| remove(item, id)).collect();
    match kept.len() {
        0 => None,
        1 => {
            let mut only = kept.into_iter().next().unwrap();
            if let (Some(size), Some(object)) = (tree.get("size"), only.as_object_mut()) {
                object.insert("size".into(), size.clone());
            }
            Some(only)
        }
        _ => {
            let mut node = tree.clone();
            node[axis] = Value::Array(kept);
            Some(node)
        }
    }
}

// ---- helpers ----

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn launch_line(command: &str, cwd: Option<&str>, env: &BTreeMap<String, String>) -> String {
    let mut setup = String::new();
    if let Some(cwd) = cwd {
        setup.push_str(&format!("cd {} && ", shell_quote(cwd)));
    }
    let exports: Vec<String> = env
        .iter()
        .map(|(key, value)| format!("export {key}={};", shell_quote(value)))
        .collect();
    format!(
        " clear; {setup}( {} {command} ); printf '\\n{EXIT_MARKER}%s]\\n' \"$?\"\r",
        exports.join(" ")
    )
}

fn new_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; PAGE_TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A terminal's recent output: from the live stream while it runs, and from
/// the transcript saved at exit once it has ended.
async fn terminal_bytes(daemon: &Daemon, terminal_id: u64) -> Result<Vec<u8>, String> {
    if terminal_exit(daemon, terminal_id).is_none() {
        let (bytes, _rx, _guard) = daemon
            .attach_terminal(terminal_id)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(bytes.to_vec());
    }
    let terminal = daemon
        .storage()
        .get_terminal(terminal_id)
        .map_err(|error| error.to_string())?;
    let saved = std::fs::read(daemon.terminal_scrollback_path(terminal_id, terminal.generation))
        .map_err(|_| "this pane's program has ended and its output was not saved".to_string())?;
    if saved.starts_with(pm_protocol::terminal_frame::GHOSTTY_SNAPSHOT_MAGIC) {
        let model =
            crate::term_model::TerminalModel::restore(&saved).map_err(|error| error.to_string())?;
        return model.try_snapshot_vt().map_err(|error| error.to_string());
    }
    Ok(saved)
}

async fn terminal_text(daemon: &Daemon, terminal_id: u64) -> Result<String, String> {
    let bytes = terminal_bytes(daemon, terminal_id).await?;
    let start = bytes.len().saturating_sub(READ_MAX_BYTES);
    Ok(crate::mcp::strip_ansi(&String::from_utf8_lossy(
        &bytes[start..],
    )))
}

/// The exit status of a terminal whose process has ended.
fn terminal_exit(daemon: &Daemon, terminal_id: u64) -> Option<i32> {
    match daemon.storage().get_terminal(terminal_id) {
        Ok(terminal) => matches!(
            terminal.state,
            pm_protocol::domain::TerminalRunState::Exited
                | pm_protocol::domain::TerminalRunState::Failed
        )
        .then(|| terminal.exit_code.unwrap_or(-1)),
        Err(_) => Some(-1),
    }
}

fn exit_code_in(text: &str) -> Option<i32> {
    let at = text.rfind(EXIT_MARKER)?;
    let rest = &text[at + EXIT_MARKER.len()..];
    rest.split(']').next()?.trim().parse().ok()
}

async fn notify(daemon: &Daemon, session_id: u64, text: String) {
    let text = format!("[Puppet Master] {text}");
    if let Ok(plan) = daemon.agent_message_plan(session_id, &text, true) {
        let _ = daemon
            .deliver_agent_message(session_id, &text, true, plan, false, true)
            .await;
    }
}

fn pane_summary(pane: &Pane, placed: bool) -> Value {
    let state = match (pane.kind.as_str(), pane.exit_code) {
        ("page" | "browser", _) => "live".to_string(),
        (_, Some(code)) => format!("exited {code}"),
        (_, None) if pane.terminal_id.is_some() => "running".into(),
        _ => "stopped".into(),
    };
    json!({
        "id": pane.id,
        "kind": pane.kind,
        "title": pane.title,
        "command": pane.command,
        "state": state,
        "placed": placed,
        "user_input": pane.user_input,
        "collapsed": pane.collapsed,
        "version": pane.version,
    })
}

fn layout_view(layout: &Layout) -> Value {
    let placed: Vec<String> = {
        let mut leaves = Vec::new();
        let _ = collect_leaves(&layout.tree, &mut leaves);
        leaves
    };
    json!({
        "rev": layout.rev,
        "layout": layout.tree,
        "panes": layout.panes.values().map(|pane| pane_summary(pane, placed.contains(&pane.id))).collect::<Vec<_>>(),
        "unplaced": layout.unplaced,
    })
}

// ---- agent tools ----

pub fn tool_definitions() -> Vec<Value> {
    let id = json!({ "type": "string", "description": "The pane's id" });
    vec![
        json!({
            "name": "open_terminal",
            "description": "Open a terminal pane in the user's session view running `command` (or the user's shell when command is omitted). Use it only to show the user something: when they ask to see a program, its output or a live view, or when watching a long-running process such as a dev server or test watcher clearly helps them follow your work. Do not use it for commands you only need yourself, for background jobs, or to keep a process out of sight; run those with your own tools. The pane opens visible beside you; the user can resize, hide or stop it. Requires bypass permissions.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string", "description": "Your name for the pane, unique in this session, used by every other pane call" },
                "command": { "type": "string", "description": "Shell command line to run; omit for a plain interactive shell" },
                "cwd": { "type": "string", "description": "Directory relative to the session working directory" },
                "env": { "type": "object", "description": "Extra environment variables" },
                "title": { "type": "string", "description": "Short header label you choose for the user, e.g. \"auth tests\" or \"dev server\"; not the command" }
            }, "required": ["id", "title"] }
        }),
        json!({
            "name": "open_page",
            "description": "Open a page pane in a sandboxed frame beside you, to show the user a dashboard, chart, report or the app you are building. Give either dir, an HTML directory in your working directory with index.html at its root, or port, a dev server you run on this host. A dir page reloads by itself when index.html changes; for live data have it fetch a relative file such as data.json on an interval, or call reload_pane. A port page is served under the base_path this call returns: configure the dev server's base or public path to it (for Vite, --base <base_path>) so its assets and HMR resolve. Pages run scripts but have no access to Puppet Master or the user's login.",
            "inputSchema": { "type": "object", "properties": {
                "id": { "type": "string", "description": "Your name for the pane, unique in this session" },
                "dir": { "type": "string", "description": "Directory relative to the session working directory" },
                "port": { "type": "integer", "description": "Port of a dev server on this host, instead of dir" },
                "title": { "type": "string", "description": "Short header label you choose for the user" }
            }, "required": ["id", "title"] }
        }),
        json!({
            "name": "get_layout",
            "description": "The current pane layout of your session view: its revision, the split tree, every pane with its state, and panes the user hid (unplaced). The user can change the layout at any time; call this before set_layout.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "set_layout",
            "description": "Arrange your session view. `layout` is a tree: {\"pane\": id, \"size\": percent} leaves inside {\"row\": [...]} (side by side) or {\"column\": [...]} (stacked) splits, which may also carry a size. It must contain the \"agent\" pane exactly once. Pass the base_rev you read from get_layout: if the user changed anything since, the call is refused with the current layout so you can adjust and retry instead of overwriting their changes.",
            "inputSchema": { "type": "object", "properties": {
                "base_rev": { "type": "integer" },
                "layout": { "type": "object" }
            }, "required": ["base_rev", "layout"] }
        }),
        json!({
            "name": "list_panes",
            "description": "Your panes with their kind, state (running, exited N, stopped, live), whether they are placed or hidden by the user, and whether the user enabled typing in them.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "read_pane",
            "description": "The recent output of a terminal pane as plain text.",
            "inputSchema": { "type": "object", "properties": {
                "id": id,
                "lines": { "type": "integer", "description": "How many trailing lines, default 80" },
                "raw": { "type": "boolean", "description": "Keep escape sequences" }
            }, "required": ["id"] }
        }),
        json!({
            "name": "wait_pane",
            "description": "Wait until a terminal pane's program exits, its output matches a regex, or it goes quiet, without polling. Returns what happened and the recent output.",
            "inputSchema": { "type": "object", "properties": {
                "id": id,
                "until": { "type": "string", "enum": ["exit", "match", "quiet"] },
                "pattern": { "type": "string", "description": "Regex for until=match, tested against output that arrives after the call" },
                "quiet_ms": { "type": "integer", "description": "For until=quiet, default 2000" },
                "timeout_ms": { "type": "integer", "description": "At most 25000" }
            }, "required": ["id", "until"] }
        }),
        json!({
            "name": "send_to_pane",
            "description": "Type into a terminal pane: text, then Enter when submit is true, and named keys such as ctrl-c, ctrl-d, enter, esc, tab, up, down, left, right or single characters. Requires bypass permissions.",
            "inputSchema": { "type": "object", "properties": {
                "id": id,
                "text": { "type": "string" },
                "submit": { "type": "boolean" },
                "keys": { "type": "array", "items": { "type": "string" } }
            }, "required": ["id"] }
        }),
        json!({
            "name": "restart_pane",
            "description": "Run a terminal pane's command again from the start in the same pane. Requires bypass permissions.",
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] }
        }),
        json!({
            "name": "set_pane_title",
            "description": "Rename one of your panes, for example to reflect what it now shows.",
            "inputSchema": { "type": "object", "properties": {
                "id": id,
                "title": { "type": "string" }
            }, "required": ["id", "title"] }
        }),
        json!({
            "name": "reload_pane",
            "description": "Reload a page pane.",
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] }
        }),
        json!({
            "name": "close_pane",
            "description": "Remove one of your panes. A terminal pane's program is stopped.",
            "inputSchema": { "type": "object", "properties": { "id": id }, "required": ["id"] }
        }),
    ]
}

pub fn is_tool(name: &str) -> bool {
    tool_definitions().iter().any(|tool| tool["name"] == name)
}

pub async fn call_tool(
    daemon: &Arc<Daemon>,
    token: &str,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let session_id = daemon
        .resolve_live_session(token)
        .map_err(|error| error.to_string())?;
    let session = daemon
        .storage()
        .get_session(session_id)
        .map_err(|error| error.to_string())?;
    let bypass = session.permission_mode == PermissionMode::Bypass;
    let needs_bypass = || {
        if bypass {
            Ok(())
        } else {
            Err(format!(
                "{name} needs bypass permissions, and this session runs in {} mode. Ask the user to run it themselves or to switch this bucket to bypass.",
                session.permission_mode.as_str()
            ))
        }
    };
    let id_arg = || {
        args["id"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| "id is required".to_string())
    };
    let panes = &daemon.panes;
    match name {
        "open_terminal" => {
            needs_bypass()?;
            let id = id_arg()?;
            if id == AGENT_PANE {
                return Err("\"agent\" is your own pane".into());
            }
            let command = args["command"]
                .as_str()
                .map(str::to_string)
                .filter(|c| !c.trim().is_empty());
            let cwd = args["cwd"].as_str().filter(|c| !c.is_empty());
            if let Some(cwd) = cwd {
                if cwd.starts_with('/') || cwd.split('/').any(|part| part == "..") {
                    return Err("cwd must be relative to the session working directory".into());
                }
            }
            let env: BTreeMap<String, String> = args["env"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(key, _)| {
                    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect();
            let title = args["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| id.clone());
            {
                let current = panes.get(session_id);
                if current.panes.contains_key(&id) {
                    return Err(format!(
                        "a pane named {id:?} already exists; close it or pick another id"
                    ));
                }
                if current.panes.len() >= MAX_PANES {
                    return Err(format!("at most {MAX_PANES} panes; close one first"));
                }
            }
            let titled = args["title"].as_str().is_some_and(|t| !t.trim().is_empty());
            let (terminal_id, launch) =
                start_terminal(daemon, session_id, &title, command.as_deref(), cwd, &env)?;
            let pane = Pane {
                id: id.clone(),
                kind: if command.is_some() {
                    "program"
                } else {
                    "shell"
                }
                .into(),
                title,
                terminal_id: Some(terminal_id),
                command: command.clone(),
                launch,
                cwd: cwd.map(str::to_string),
                env: env.clone(),
                page_token: None,
                page_dir: None,
                page_port: None,
                page_forward: None,
                browser_stream: None,
                version: 0,
                user_input: command.is_none(),
                collapsed: false,
                titled,
                exit_code: None,
                user_opened: false,
            };
            let (layout, ()) = panes.update(daemon.storage(), session_id, |layout| {
                layout.tree = place(&layout.tree, &id);
                layout.panes.insert(id.clone(), pane);
                Ok(())
            })?;
            publish(daemon, session_id, &layout);
            spawn_watcher(daemon.clone(), session_id, id.clone());
            Ok(json!({ "ok": true, "id": id, "state": "running", "rev": layout.rev }))
        }
        "open_page" => {
            let id = id_arg()?;
            if id == AGENT_PANE {
                return Err("\"agent\" is your own pane".into());
            }
            let port = args["port"]
                .as_u64()
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p != 0);
            let dir = args["dir"]
                .as_str()
                .unwrap_or_default()
                .trim_matches('/')
                .to_string();
            if port.is_none() && (dir.is_empty() || dir.split('/').any(|part| part == "..")) {
                return Err(
                    "give dir, a directory relative to the session working directory, or port"
                        .into(),
                );
            }
            {
                let current = panes.get(session_id);
                if current.panes.contains_key(&id) {
                    return Err(format!("a pane named {id:?} already exists"));
                }
                if current.panes.len() >= MAX_PANES {
                    return Err(format!("at most {MAX_PANES} panes; close one first"));
                }
            }
            let forward = match port {
                Some(port) => {
                    let slug = format!(
                        "pane-{session_id}-{}",
                        id.to_ascii_lowercase()
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric())
                            .collect::<String>()
                    );
                    let forward = daemon
                        .publish_port(token, port, &slug, &format!("pane {id}"), "http")
                        .await
                        .map_err(|error| format!("could not reach port {port}: {error}"))?;
                    Some(forward.id)
                }
                None => {
                    daemon
                        .session_file_bytes(
                            &session,
                            &format!("{dir}/index.html"),
                            "dir",
                            PAGE_FILE_MAX_BYTES,
                        )
                        .await
                        .map_err(|error| format!("could not read {dir}/index.html: {error}"))?;
                    None
                }
            };
            let title = args["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| id.clone());
            let token_value = new_token();
            let (layout, ()) = panes.update(daemon.storage(), session_id, |layout| {
                layout.tree = place(&layout.tree, &id);
                layout.panes.insert(
                    id.clone(),
                    Pane {
                        id: id.clone(),
                        kind: "page".into(),
                        title,
                        terminal_id: None,
                        command: None,
                        launch: None,
                        cwd: None,
                        env: BTreeMap::new(),
                        page_token: Some(token_value.clone()),
                        page_dir: (!dir.is_empty() && port.is_none()).then(|| dir.clone()),
                        page_port: port,
                        page_forward: forward,
                        browser_stream: None,
                        version: 0,
                        user_input: true,
                        collapsed: false,
                        titled: true,
                        exit_code: None,
                        user_opened: false,
                    },
                );
                Ok(())
            })?;
            publish(daemon, session_id, &layout);
            if port.is_none() {
                spawn_watcher(daemon.clone(), session_id, id.clone());
            }
            Ok(
                json!({ "ok": true, "id": id, "rev": layout.rev, "base_path": format!("/pane-pages/{token_value}/") }),
            )
        }
        "get_layout" => Ok(layout_view(&panes.get(session_id))),
        "list_panes" => Ok(layout_view(&panes.get(session_id))["panes"].clone()),
        "set_layout" => {
            let base_rev = args["base_rev"]
                .as_u64()
                .ok_or("base_rev is required; read it with get_layout")?;
            let tree = args["layout"].clone();
            let result = panes.update(daemon.storage(), session_id, |layout| {
                if layout.rev != base_rev {
                    return Err(format!(
                        "conflict: the layout changed since revision {base_rev} (the user may have resized or hidden panes). Current layout: {}",
                        layout_view(layout)
                    ));
                }
                let leaves = validate(&tree, &layout.panes)?;
                let in_tabs = layout.tab_panes();
                if let Some(id) = leaves.iter().find(|id| in_tabs.contains(*id)) {
                    return Err(format!("pane {id:?} is in one of the user's shell tabs"));
                }
                layout.unplaced.retain(|id| !leaves.contains(id));
                for id in layout.panes.keys() {
                    if !leaves.contains(id) && !layout.unplaced.contains(id) && !in_tabs.contains(id) {
                        layout.unplaced.push(id.clone());
                    }
                }
                layout.tree = tree.clone();
                Ok(())
            });
            let (layout, ()) = result?;
            publish(daemon, session_id, &layout);
            Ok(json!({ "ok": true, "rev": layout.rev }))
        }
        "read_pane" => {
            let id = id_arg()?;
            let pane = panes
                .get(session_id)
                .panes
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("no pane named {id:?}"))?;
            let terminal_id = pane
                .terminal_id
                .ok_or("that pane has no running terminal")?;
            let text = if args["raw"].as_bool() == Some(true) {
                let bytes = terminal_bytes(daemon, terminal_id).await?;
                String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(READ_MAX_BYTES)..])
                    .to_string()
            } else {
                terminal_text(daemon, terminal_id).await?
            };
            let lines = args["lines"]
                .as_u64()
                .map_or(READ_DEFAULT_LINES, |n| n as usize);
            let all: Vec<&str> = text.lines().collect();
            let tail = all[all.len().saturating_sub(lines)..].join("\n");
            Ok(json!({ "id": id, "state": pane_summary(&pane, true)["state"], "text": tail }))
        }
        "wait_pane" => {
            let id = id_arg()?;
            let pane = panes
                .get(session_id)
                .panes
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("no pane named {id:?}"))?;
            let terminal_id = pane
                .terminal_id
                .ok_or("that pane has no running terminal")?;
            let until = args["until"].as_str().unwrap_or("exit").to_string();
            let pattern = match args["pattern"].as_str() {
                Some(pattern) => {
                    Some(regex::Regex::new(pattern).map_err(|error| error.to_string())?)
                }
                None if until == "match" => {
                    return Err("pattern is required for until=match".into())
                }
                None => None,
            };
            let quiet = Duration::from_millis(args["quiet_ms"].as_u64().unwrap_or(2000));
            let timeout = Duration::from_millis(
                args["timeout_ms"]
                    .as_u64()
                    .unwrap_or(WAIT_MAX.as_millis() as u64),
            )
            .min(WAIT_MAX);
            if until == "exit" {
                if let Some(code) = terminal_exit(daemon, terminal_id) {
                    let text = terminal_text(daemon, terminal_id).await.unwrap_or_default();
                    let all: Vec<&str> = text.lines().collect();
                    return Ok(
                        json!({ "done": true, "what": format!("exited with status {code}"), "text": all[all.len().saturating_sub(40)..].join("\n") }),
                    );
                }
            }
            let start = tokio::time::Instant::now();
            let baseline = terminal_text(daemon, terminal_id).await?;
            let mut last = baseline.clone();
            let mut last_change = start;
            loop {
                tokio::time::sleep(WAIT_POLL).await;
                let text = terminal_text(daemon, terminal_id).await?;
                if text != last {
                    last_change = tokio::time::Instant::now();
                    last = text.clone();
                }
                let fresh = text
                    .strip_prefix(baseline.as_str())
                    .unwrap_or(text.as_str());
                let hit = match until.as_str() {
                    "exit" => terminal_exit(daemon, terminal_id)
                        .or_else(|| exit_code_in(fresh))
                        .map(|code| format!("exited with status {code}")),
                    "match" => pattern
                        .as_ref()
                        .and_then(|re| re.find(fresh))
                        .map(|m| format!("matched {:?}", m.as_str())),
                    _ => (last_change.elapsed() >= quiet)
                        .then(|| format!("quiet for {} ms", quiet.as_millis())),
                };
                let tail = || {
                    let all: Vec<&str> = text.lines().collect();
                    all[all.len().saturating_sub(40)..].join("\n")
                };
                if let Some(what) = hit {
                    return Ok(json!({ "done": true, "what": what, "text": tail() }));
                }
                if start.elapsed() >= timeout {
                    return Ok(
                        json!({ "done": false, "what": "timed out; call again to keep waiting", "text": tail() }),
                    );
                }
            }
        }
        "send_to_pane" => {
            needs_bypass()?;
            let id = id_arg()?;
            let pane = panes
                .get(session_id)
                .panes
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("no pane named {id:?}"))?;
            let terminal_id = pane
                .terminal_id
                .ok_or("that pane has no running terminal")?;
            let mut data = args["text"].as_str().unwrap_or_default().to_string();
            if args["submit"].as_bool() == Some(true) {
                data.push('\r');
            }
            for key in args["keys"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                data.push_str(match key.to_ascii_lowercase().as_str() {
                    "ctrl-c" => "\u{3}",
                    "ctrl-d" => "\u{4}",
                    "ctrl-z" => "\u{1a}",
                    "ctrl-l" => "\u{c}",
                    "enter" => "\r",
                    "esc" | "escape" => "\u{1b}",
                    "tab" => "\t",
                    "backspace" => "\u{7f}",
                    "up" => "\u{1b}[A",
                    "down" => "\u{1b}[B",
                    "right" => "\u{1b}[C",
                    "left" => "\u{1b}[D",
                    other if other.chars().count() == 1 => key,
                    other => return Err(format!("unknown key {other:?}")),
                });
            }
            daemon.terminal_input(terminal_id, bytes::Bytes::from(data));
            Ok(json!({ "ok": true, "user_input_enabled": pane.user_input }))
        }
        "restart_pane" => {
            needs_bypass()?;
            let id = id_arg()?;
            let pane = panes
                .get(session_id)
                .panes
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("no pane named {id:?}"))?;
            if pane.kind == "page" || pane.kind == "browser" {
                return Err("use reload_pane for a page".into());
            }
            let old = pane.terminal_id;
            let (terminal_id, launch) = start_terminal(
                daemon,
                session_id,
                &pane.title,
                pane.command.as_deref(),
                pane.cwd.as_deref(),
                &pane.env,
            )?;
            let (layout, ()) = panes.update(daemon.storage(), session_id, |layout| {
                let entry = layout.panes.get_mut(&id).ok_or("pane is gone")?;
                entry.terminal_id = Some(terminal_id);
                entry.launch = launch.clone();
                entry.exit_code = None;
                entry.version += 1;
                Ok(())
            })?;
            if let Some(old) = old {
                let _ = daemon.close_terminal(old);
            }
            publish(daemon, session_id, &layout);
            spawn_watcher(daemon.clone(), session_id, id.clone());
            Ok(json!({ "ok": true, "rev": layout.rev }))
        }
        "set_pane_title" => {
            let id = id_arg()?;
            let title = args["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or("title is required")?
                .to_string();
            let (layout, ()) = panes.update(daemon.storage(), session_id, |layout| {
                let entry = layout
                    .panes
                    .get_mut(&id)
                    .ok_or_else(|| format!("no pane named {id:?}"))?;
                entry.title = title.clone();
                entry.titled = true;
                Ok(())
            })?;
            publish(daemon, session_id, &layout);
            Ok(json!({ "ok": true }))
        }
        "reload_pane" => {
            let id = id_arg()?;
            let (layout, ()) = panes.update(daemon.storage(), session_id, |layout| {
                let entry = layout
                    .panes
                    .get_mut(&id)
                    .ok_or_else(|| format!("no pane named {id:?}"))?;
                entry.version += 1;
                Ok(())
            })?;
            publish(daemon, session_id, &layout);
            Ok(json!({ "ok": true }))
        }
        "close_pane" => {
            let id = id_arg()?;
            let (layout, pane) = remove_pane(daemon, session_id, &id)?;
            release(daemon, &pane);
            publish(daemon, session_id, &layout);
            Ok(json!({ "ok": true, "rev": layout.rev }))
        }
        _ => Err(format!("unknown pane tool {name}")),
    }
}

/// Stops what a removed pane held: its terminal's process, or the forward
/// behind a dev-server page.
fn release(daemon: &Daemon, pane: &Pane) {
    if let Some(terminal_id) = pane.terminal_id {
        let _ = daemon.close_terminal(terminal_id);
    }
    if let Some(forward) = pane.page_forward {
        let _ = daemon.close_forward(forward);
    }
}

/// Starts a pane's terminal. Returns its id and, when the worker could only
/// start an interactive shell, the line typed into it to run the command.
fn start_terminal(
    daemon: &Daemon,
    session_id: u64,
    title: &str,
    command: Option<&str>,
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<(u64, Option<String>), String> {
    let launch = command.map(|command| crate::daemon::ShellLaunch {
        command: command.to_string(),
        env: env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        cwd: cwd.map(str::to_string),
    });
    let (terminal_id, direct) = daemon
        .create_pane_terminal(session_id, title, launch.as_ref())
        .map_err(|error| error.to_string())?;
    let typed = match command {
        Some(command) if !direct => {
            let line = launch_line(command, cwd, env);
            daemon.terminal_input(terminal_id, bytes::Bytes::from(line.clone()));
            Some(line)
        }
        _ => None,
    };
    Ok((terminal_id, typed))
}

fn remove_pane(daemon: &Daemon, session_id: u64, id: &str) -> Result<(Layout, Pane), String> {
    daemon.panes.update(daemon.storage(), session_id, |layout| {
        let pane = layout
            .panes
            .remove(id)
            .ok_or_else(|| format!("no pane named {id:?}"))?;
        layout.tree = remove(&layout.tree, id).unwrap_or_else(|| json!({ "pane": AGENT_PANE }));
        layout.unplaced.retain(|other| other != id);
        layout.tabs = std::mem::take(&mut layout.tabs)
            .into_iter()
            .filter_map(|(tab, tree)| {
                let kept = remove(&tree, id)?;
                (leaf_id(&kept) != Some(TAB_PANE)).then_some((tab, kept))
            })
            .collect();
        Ok(pane)
    })
}

fn spawn_watcher(daemon: Arc<Daemon>, session_id: u64, id: String) {
    tokio::spawn(async move {
        let mut page_hash: Option<u64> = None;
        let mut watched_terminal: Option<u64> = None;
        loop {
            tokio::time::sleep(WATCH_INTERVAL).await;
            let Some(pane) = daemon.panes.get(session_id).panes.get(&id).cloned() else {
                return;
            };
            if pane.kind == "page" || pane.kind == "browser" {
                let Ok(session) = daemon.storage().get_session(session_id) else {
                    return;
                };
                let path = format!("{}/index.html", pane.page_dir.clone().unwrap_or_default());
                if let Ok(bytes) = daemon
                    .session_file_bytes(&session, &path, "dir", PAGE_FILE_MAX_BYTES)
                    .await
                {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    bytes.hash(&mut hasher);
                    let hash = hasher.finish();
                    if page_hash.is_some_and(|previous| previous != hash) {
                        if let Ok((layout, ())) =
                            daemon.panes.update(daemon.storage(), session_id, |layout| {
                                let entry = layout.panes.get_mut(&id).ok_or("gone")?;
                                entry.version += 1;
                                Ok(())
                            })
                        {
                            publish(&daemon, session_id, &layout);
                        }
                    }
                    page_hash = Some(hash);
                }
                continue;
            }
            let Some(terminal_id) = pane.terminal_id else {
                return;
            };
            if watched_terminal.is_some_and(|watched| watched != terminal_id) {
                return;
            }
            watched_terminal = Some(terminal_id);
            if pane.exit_code.is_some() && pane.kind != "shell" {
                continue;
            }
            let ended = terminal_exit(&daemon, terminal_id).or(pane.exit_code);
            let typed_exit = if ended.is_none() && pane.launch.is_some() {
                terminal_text(&daemon, terminal_id)
                    .await
                    .ok()
                    .and_then(|text| exit_code_in(&text))
            } else {
                None
            };
            let layout_now = daemon.panes.get(session_id);
            let tab_ended = layout_now
                .tab_of(&id)
                .and_then(|tab| tab.parse::<u64>().ok())
                .is_some_and(|tab| terminal_exit(&daemon, tab).is_some());
            if tab_ended {
                if let Ok((layout, pane)) = remove_pane(&daemon, session_id, &id) {
                    release(&daemon, &pane);
                    publish(&daemon, session_id, &layout);
                }
                return;
            }
            if pane.kind == "shell" && ended.is_some() {
                if let Ok((layout, pane)) = remove_pane(&daemon, session_id, &id) {
                    release(&daemon, &pane);
                    publish(&daemon, session_id, &layout);
                    if let Some(notice) = shell_exit_notice(&pane) {
                        notify(&daemon, session_id, notice).await;
                    }
                }
                return;
            }
            if let Some(code) = ended.or(typed_exit) {
                if let Ok((layout, ())) =
                    daemon.panes.update(daemon.storage(), session_id, |layout| {
                        let entry = layout.panes.get_mut(&id).ok_or("gone")?;
                        entry.exit_code = Some(code);
                        Ok(())
                    })
                {
                    publish(&daemon, session_id, &layout);
                }
                notify(&daemon, session_id, format!("Pane \"{id}\" exited with status {code}. read_pane shows its output; restart_pane runs it again.")).await;
            }
        }
    });
}

// ---- user actions from the web ----

/// Lays out the agent's view, or with `tab` that shell tab's own view.
pub async fn user_set_layout(
    daemon: &Arc<Daemon>,
    session_id: u64,
    base_rev: u64,
    tree: Value,
    tab: Option<&str>,
) -> Result<Value, String> {
    let (layout, ()) = daemon
        .panes
        .update(daemon.storage(), session_id, |layout| {
            if layout.rev != base_rev {
                return Err("conflict".into());
            }
            let root = if tab.is_some() { TAB_PANE } else { AGENT_PANE };
            let leaves = validate_rooted(&tree, &layout.panes, root)?;
            if leaves.iter().any(|id| layout.unplaced.contains(id)) {
                return Err("hidden panes cannot be placed by a resize".into());
            }
            if leaves
                .iter()
                .any(|id| id != root && layout.tab_of(id).as_deref() != tab)
            {
                return Err("a pane can only be arranged in the view it is in".into());
            }
            match tab {
                Some(tab) => {
                    layout.tabs.insert(tab.to_string(), tree.clone());
                }
                None => layout.tree = tree.clone(),
            }
            Ok(())
        })?;
    publish(daemon, session_id, &layout);
    Ok(json!({ "rev": layout.rev }))
}

/// Opens a shell pane the user asked for, in the agent's view or with `tab`
/// in that shell tab's view. The layout places it, unless the layout changed
/// since `base_rev`, in which case it is placed the way an agent's new pane is.
pub async fn user_open_shell(
    daemon: &Arc<Daemon>,
    session_id: u64,
    id: &str,
    base_rev: u64,
    tree: Value,
    tab: Option<&str>,
) -> Result<Value, String> {
    if let Some(tab) = tab {
        let owned = tab
            .parse::<u64>()
            .ok()
            .and_then(|terminal_id| daemon.storage().get_terminal(terminal_id).ok())
            .is_some_and(|terminal| {
                terminal.session_id == session_id && terminal_exit(daemon, terminal.id).is_none()
            });
        if !owned {
            return Err("that shell tab is not open in this session".into());
        }
    }
    let id = id.trim();
    if id.is_empty() || id == AGENT_PANE {
        return Err("a shell pane needs an id other than \"agent\"".into());
    }
    {
        let current = daemon.panes.get(session_id);
        if current.panes.contains_key(id) {
            return Err(format!("a pane named {id:?} already exists"));
        }
        if current.panes.len() >= MAX_PANES {
            return Err(format!("at most {MAX_PANES} panes; close one first"));
        }
    }
    let title = format!("shell {}", id.trim_start_matches("shell-"));
    let (terminal_id, _) =
        start_terminal(daemon, session_id, &title, None, None, &BTreeMap::new())?;
    let pane = Pane {
        id: id.to_string(),
        kind: "shell".into(),
        title,
        terminal_id: Some(terminal_id),
        command: None,
        launch: None,
        cwd: None,
        env: BTreeMap::new(),
        page_token: None,
        page_dir: None,
        page_port: None,
        page_forward: None,
        browser_stream: None,
        version: 0,
        user_input: true,
        collapsed: false,
        titled: false,
        exit_code: None,
        user_opened: true,
    };
    let placed = daemon.panes.update(daemon.storage(), session_id, |layout| {
        let mut panes = layout.panes.clone();
        panes.insert(id.to_string(), pane.clone());
        let root = if tab.is_some() { TAB_PANE } else { AGENT_PANE };
        let current = match tab {
            Some(tab) => layout
                .tabs
                .get(tab)
                .cloned()
                .unwrap_or_else(|| json!({ "pane": TAB_PANE })),
            None => layout.tree.clone(),
        };
        let fits = layout.rev == base_rev
            && validate_rooted(&tree, &panes, root).is_ok_and(|leaves| {
                leaves.iter().any(|leaf| leaf == id)
                    && leaves.iter().all(|leaf| {
                        leaf == root || leaf == id || layout.tab_of(leaf).as_deref() == tab
                    })
            });
        let next = if fits {
            tree.clone()
        } else {
            place_rooted(&current, id, root)
        };
        match tab {
            Some(tab) => {
                layout.tabs.insert(tab.to_string(), next);
            }
            None => layout.tree = next,
        }
        layout.panes = panes;
        Ok(())
    });
    let (layout, ()) = match placed {
        Ok(placed) => placed,
        Err(error) => {
            let _ = daemon.close_terminal(terminal_id);
            return Err(error);
        }
    };
    publish(daemon, session_id, &layout);
    spawn_watcher(daemon.clone(), session_id, id.to_string());
    Ok(json!({ "ok": true, "id": id, "rev": layout.rev }))
}

pub async fn user_pane_action(
    daemon: &Arc<Daemon>,
    session_id: u64,
    id: &str,
    action: &str,
) -> Result<Value, String> {
    let panes = &daemon.panes;
    let notice = match action {
        "hide" => {
            let (layout, ()) = panes.update(daemon.storage(), session_id, |layout| {
                if !layout.panes.contains_key(id) {
                    return Err(format!("no pane named {id:?}"));
                }
                if layout.tab_of(id).is_some() {
                    return Err("a pane in a shell tab can be stopped but not hidden".into());
                }
                layout.tree =
                    remove(&layout.tree, id).unwrap_or_else(|| json!({ "pane": AGENT_PANE }));
                if !layout.unplaced.iter().any(|other| other == id) {
                    layout.unplaced.push(id.to_string());
                }
                Ok(())
            })?;
            publish(daemon, session_id, &layout);
            None
        }
        "restore" => {
            let (layout, ()) = panes.update(daemon.storage(), session_id, |layout| {
                if !layout.unplaced.iter().any(|other| other == id) {
                    return Err(format!("pane {id:?} is not hidden"));
                }
                layout.unplaced.retain(|other| other != id);
                layout.tree = place(&layout.tree, id);
                Ok(())
            })?;
            publish(daemon, session_id, &layout);
            None
        }
        "stop" => {
            let (layout, pane) = remove_pane(daemon, session_id, id)?;
            release(daemon, &pane);
            publish(daemon, session_id, &layout);
            stop_notice(&pane)
        }
        "collapse" | "expand" => {
            let collapsed = action == "collapse";
            let (layout, ()) = panes.update(daemon.storage(), session_id, |layout| {
                let entry = layout
                    .panes
                    .get_mut(id)
                    .ok_or_else(|| format!("no pane named {id:?}"))?;
                entry.collapsed = collapsed;
                Ok(())
            })?;
            publish(daemon, session_id, &layout);
            return Ok(json!({ "ok": true }));
        }
        "input-on" | "input-off" => {
            let enabled = action == "input-on";
            let (layout, pane) = panes.update(daemon.storage(), session_id, |layout| {
                let entry = layout
                    .panes
                    .get_mut(id)
                    .ok_or_else(|| format!("no pane named {id:?}"))?;
                entry.user_input = enabled;
                Ok(entry.clone())
            })?;
            publish(daemon, session_id, &layout);
            if enabled {
                typing_notice(&pane)
            } else {
                None
            }
        }
        _ => return Err(format!("unknown action {action}")),
    };
    if let Some(notice) = notice {
        let daemon = daemon.clone();
        tokio::spawn(async move { notify(&daemon, session_id, notice).await });
    }
    Ok(json!({ "ok": true }))
}

fn shell_exit_notice(pane: &Pane) -> Option<String> {
    (!pane.user_opened).then(|| format!("Shell pane \"{}\" exited and was closed.", pane.id))
}

fn stop_notice(pane: &Pane) -> Option<String> {
    (!pane.user_opened).then(|| {
        format!(
            "The user stopped pane \"{}\" and its program. Do not start it again unless the user asks.",
            pane.id
        )
    })
}

fn typing_notice(pane: &Pane) -> Option<String> {
    (!pane.user_opened).then(|| {
        format!(
            "The user enabled typing in pane \"{}\"; avoid sending keys there while they work in it.",
            pane.id
        )
    })
}

/// What an ended pane program printed, for the pane to show once there is no
/// live terminal to attach to.
pub async fn pane_output(
    daemon: &Arc<Daemon>,
    session_id: u64,
    id: &str,
) -> Result<String, String> {
    let pane = daemon
        .panes
        .get(session_id)
        .panes
        .get(id)
        .cloned()
        .ok_or_else(|| format!("no pane named {id:?}"))?;
    let terminal_id = pane.terminal_id.ok_or("that pane has no terminal")?;
    terminal_text(daemon, terminal_id).await
}

/// The session and file a page pane's capability token serves, if any.
pub fn page_target(daemon: &Daemon, token: &str) -> Option<PageTarget> {
    daemon.panes.page(token)
}

pub const PAGE_MAX_BYTES: usize = PAGE_FILE_MAX_BYTES;

pub async fn show_browser(
    daemon: &Arc<Daemon>,
    session_id: u64,
    context: &str,
    port: u16,
    stream: &str,
    title: Option<&str>,
) -> Result<Value, String> {
    let id = format!("browser-{context}");
    let current = daemon.panes.get(session_id);
    if !current.panes.contains_key(&id) && current.panes.len() >= MAX_PANES {
        return Err(format!("at most {MAX_PANES} panes; close one first"));
    }
    if current
        .panes
        .get(&id)
        .is_some_and(|pane| pane.kind != "browser")
    {
        return Err(format!("pane {id:?} is already used by another program"));
    }
    let (layout, ()) = daemon
        .panes
        .update(daemon.storage(), session_id, |layout| {
            if !layout.panes.contains_key(&id) && layout.panes.len() >= MAX_PANES {
                return Err(format!("at most {MAX_PANES} panes; close one first"));
            }
            let old = layout.panes.get(&id).cloned();
            if old.as_ref().is_some_and(|pane| pane.kind != "browser") {
                return Err(format!("pane {id:?} is already in use"));
            }
            let same_stream = old.as_ref().filter(|pane| {
                pane.page_port == Some(port) && pane.browser_stream.as_deref() == Some(stream)
            });
            let pane = Pane {
                id: id.clone(),
                kind: "browser".into(),
                title: title
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or("Browser")
                    .to_string(),
                terminal_id: None,
                command: None,
                launch: None,
                cwd: None,
                env: BTreeMap::new(),
                page_token: Some(
                    same_stream
                        .and_then(|pane| pane.page_token.clone())
                        .unwrap_or_else(new_token),
                ),
                page_dir: None,
                page_port: Some(port),
                page_forward: None,
                browser_stream: Some(stream.to_string()),
                version: same_stream
                    .map(|pane| pane.version)
                    .unwrap_or_else(|| old.as_ref().map_or(0, |pane| pane.version + 1)),
                user_input: false,
                collapsed: false,
                titled: true,
                exit_code: None,
                user_opened: false,
            };
            layout.unplaced.retain(|other| other != &id);
            let mut leaves = Vec::new();
            collect_leaves(&layout.tree, &mut leaves)?;
            if !leaves.contains(&id) {
                layout.tree = place(&layout.tree, &id);
            }
            layout.panes.insert(id.clone(), pane);
            Ok(())
        })?;
    publish(daemon, session_id, &layout);
    Ok(json!({"ok":true,"id":id,"browser_context":context,"rev":layout.rev}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_protocol::domain::{AgentKind, PermissionMode};

    #[test]
    fn a_shell_the_user_opened_tells_the_agent_nothing() {
        let mut own = pane("shell-1");
        own.kind = "shell".into();
        own.user_opened = true;
        assert_eq!(shell_exit_notice(&own), None);
        assert_eq!(stop_notice(&own), None);
        assert_eq!(typing_notice(&own), None);
    }

    #[test]
    fn a_pane_the_agent_opened_still_reports_exits_stops_and_typing() {
        let mut agents = pane("build");
        agents.kind = "shell".into();
        assert!(shell_exit_notice(&agents).is_some_and(|n| n.contains("\"build\" exited")));
        assert!(stop_notice(&agents).is_some_and(|n| n.contains("stopped pane \"build\"")));
        assert!(typing_notice(&agents).is_some_and(|n| n.contains("enabled typing")));
    }

    #[test]
    fn a_stored_pane_without_the_flag_reads_as_opened_by_the_agent() {
        let stored = r#"{"id":"log","kind":"shell","title":"log","version":0,"user_input":true}"#;
        let pane: Pane = serde_json::from_str(stored).unwrap();
        assert!(!pane.user_opened);
        let mut own = pane.clone();
        own.user_opened = true;
        let round: Pane = serde_json::from_str(&serde_json::to_string(&own).unwrap()).unwrap();
        assert!(round.user_opened);
        assert!(!serde_json::to_string(&pane)
            .unwrap()
            .contains("user_opened"));
    }

    fn pane(id: &str) -> Pane {
        Pane {
            id: id.into(),
            kind: "program".into(),
            title: id.into(),
            terminal_id: Some(7),
            command: Some("make test".into()),
            launch: Some("typed line".into()),
            cwd: None,
            env: BTreeMap::from([("TOKEN".to_string(), "secret".to_string())]),
            page_token: None,
            page_dir: None,
            page_port: None,
            page_forward: Some(3),
            browser_stream: None,
            version: 0,
            user_input: false,
            collapsed: false,
            titled: true,
            exit_code: None,
            user_opened: false,
        }
    }

    fn panes(ids: &[&str]) -> BTreeMap<String, Pane> {
        ids.iter().map(|id| (id.to_string(), pane(id))).collect()
    }

    #[test]
    fn a_shell_tab_layout_is_rooted_at_its_tab_and_new_panes_join_it() {
        let known = panes(&["a"]);
        assert!(validate_rooted(
            &json!({ "row": [{ "pane": "tab" }, { "pane": "a" }] }),
            &known,
            TAB_PANE
        )
        .is_ok());
        assert!(validate_rooted(
            &json!({ "row": [{ "pane": "agent" }, { "pane": "a" }] }),
            &known,
            TAB_PANE
        )
        .unwrap_err()
        .contains("\"tab\""));
        assert_eq!(
            place_rooted(&json!({ "pane": "tab" }), "a", TAB_PANE),
            json!({ "row": [
                { "pane": "tab", "size": 100 - DEFAULT_NEW_PANE_SIZE },
                { "pane": "a", "size": DEFAULT_NEW_PANE_SIZE }
            ]})
        );
    }

    #[test]
    fn a_pane_is_found_in_the_tab_that_holds_it() {
        let mut layout = Layout {
            rev: 1,
            tree: json!({ "row": [{ "pane": "agent" }, { "pane": "a" }] }),
            panes: panes(&["a", "b", "c"]),
            unplaced: vec![],
            tabs: BTreeMap::new(),
        };
        layout.tabs.insert(
            "27".into(),
            json!({ "column": [{ "pane": "tab" }, { "pane": "b" }, { "pane": "c" }] }),
        );
        assert_eq!(layout.tab_of("b").as_deref(), Some("27"));
        assert_eq!(layout.tab_of("a"), None);
        assert_eq!(
            layout.tab_panes(),
            ["b".to_string(), "c".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn records_stored_before_tabs_and_titles_load_with_their_titles_kept() {
        let stored = json!({
            "rev": 3,
            "tree": { "row": [{ "pane": "agent" }, { "pane": "log" }] },
            "panes": { "log": { "id": "log", "kind": "shell", "title": "Build log", "version": 0, "user_input": true } },
            "unplaced": []
        });
        let layout: Layout = serde_json::from_value(stored).unwrap();
        assert!(layout.tabs.is_empty());
        assert!(
            layout.panes["log"].titled,
            "a stored pane keeps showing the title it had"
        );
    }

    #[test]
    fn a_layout_must_hold_the_agent_once_and_only_known_panes() {
        let known = panes(&["tests"]);
        assert!(validate(
            &json!({ "row": [{ "pane": "agent" }, { "pane": "tests" }] }),
            &known
        )
        .is_ok());
        assert!(validate(&json!({ "pane": "tests" }), &known)
            .unwrap_err()
            .contains("\"agent\""));
        assert!(validate(
            &json!({ "row": [{ "pane": "agent" }, { "pane": "agent" }] }),
            &known
        )
        .is_err());
        assert!(validate(
            &json!({ "row": [{ "pane": "agent" }, { "pane": "ghost" }] }),
            &known
        )
        .unwrap_err()
        .contains("open it first"));
        assert!(validate(
            &json!({ "row": [{ "pane": "agent" }, { "pane": "tests" }, { "pane": "tests" }] }),
            &known
        )
        .unwrap_err()
        .contains("more than once"));
        assert!(validate(&json!({ "row": [] }), &known).is_err());
    }

    #[test]
    fn a_layout_holds_at_most_the_pane_cap_besides_the_agent() {
        let ids: Vec<String> = (0..=MAX_PANES).map(|n| format!("p{n}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut leaves = vec![json!({ "pane": "agent" })];
        leaves.extend(ids.iter().map(|id| json!({ "pane": id })));
        assert!(validate(&json!({ "row": leaves }), &panes(&refs))
            .unwrap_err()
            .contains("at most"));
    }

    #[test]
    fn a_new_pane_splits_beside_the_agent_then_stacks_in_the_right_column() {
        let first = place(&json!({ "pane": "agent" }), "a");
        assert_eq!(
            first,
            json!({ "row": [
                { "pane": "agent", "size": 100 - DEFAULT_NEW_PANE_SIZE },
                { "pane": "a", "size": DEFAULT_NEW_PANE_SIZE }
            ]})
        );
        let second = place(&first, "b");
        assert_eq!(
            second["row"][1],
            json!({ "column": [{ "pane": "a" }, { "pane": "b" }], "size": DEFAULT_NEW_PANE_SIZE })
        );
        let third = place(&second, "c");
        assert_eq!(third["row"][1]["column"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn removing_a_pane_collapses_a_split_left_with_one_child_into_it() {
        let tree = json!({ "row": [
            { "pane": "agent", "size": 60 },
            { "column": [{ "pane": "a" }, { "pane": "b" }], "size": 40 }
        ]});
        let without_b = remove(&tree, "b").unwrap();
        assert_eq!(
            without_b,
            json!({ "row": [{ "pane": "agent", "size": 60 }, { "pane": "a", "size": 40 }] })
        );
        assert_eq!(
            remove(&without_b, "a").unwrap(),
            json!({ "pane": "agent", "size": 60 })
        );
    }

    #[test]
    fn clients_never_see_launch_lines_environment_or_forward_ids() {
        let layout = Layout {
            rev: 1,
            tree: json!({ "pane": "agent" }),
            panes: panes(&["a"]),
            unplaced: vec![],
            tabs: BTreeMap::new(),
        };
        let json = record(9, &layout).json;
        assert!(
            !json.contains("secret")
                && !json.contains("typed line")
                && !json.contains("page_forward")
        );
        assert!(json.contains("make test"));
    }

    #[test]
    fn the_typed_fallback_quotes_the_directory_and_reports_the_exit_status() {
        let env = BTreeMap::from([("NAME".to_string(), "it's".to_string())]);
        let line = launch_line("cargo test", Some("my dir"), &env);
        assert!(line.contains("cd 'my dir' &&"));
        assert!(line.contains("export NAME='it'\\''s';"));
        assert_eq!(exit_code_in(&format!("output\n{EXIT_MARKER}3]\n")), Some(3));
        assert_eq!(exit_code_in("no marker"), None);
    }

    fn storage_with_session() -> (crate::storage::Storage, u64) {
        let storage = crate::storage::Storage::open_in_memory().unwrap();
        let bucket = storage.create_bucket("panes").unwrap();
        let project = storage
            .create_project(bucket.id, "project", "/tmp/panes-test")
            .unwrap();
        let session = storage
            .create_session(
                project.id,
                AgentKind::ClaudeCode,
                "task",
                "prompt",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                crate::storage::now_unix_ms(),
            )
            .unwrap();
        (storage, session.id)
    }

    #[test]
    fn the_database_refuses_a_layout_write_based_on_an_old_revision() {
        let (storage, session) = storage_with_session();
        storage.save_session_layout(session, 1, "{}").unwrap();
        storage.save_session_layout(session, 2, "{}").unwrap();
        assert!(storage
            .save_session_layout(session, 2, "{}")
            .unwrap_err()
            .contains("conflict"));
        assert!(storage.save_session_layout(session, 5, "{}").is_err());
    }

    #[test]
    fn layouts_survive_a_reload_and_a_stale_change_leaves_them_untouched() {
        let (storage, session) = storage_with_session();
        let panes = Panes::default();
        let (layout, ()) = panes
            .update(&storage, session, |layout| {
                layout.panes.insert("a".into(), pane("a"));
                layout.tree = place(&layout.tree, "a");
                Ok(())
            })
            .unwrap();
        assert_eq!(layout.rev, 1);
        let reloaded = Panes::load(&storage).get(session);
        assert_eq!(reloaded.rev, 1);
        assert!(reloaded.panes.contains_key("a"));
        storage.save_session_layout(session, 2, "{}").unwrap();
        assert!(panes.update(&storage, session, |_| Ok(())).is_err());
        assert_eq!(panes.get(session).rev, 1);
    }

    #[test]
    fn a_pane_owns_its_terminal_until_it_is_removed() {
        let (storage, session) = storage_with_session();
        let panes = Panes::default();
        panes
            .update(&storage, session, |layout| {
                layout.panes.insert("a".into(), pane("a"));
                Ok(())
            })
            .unwrap();
        assert!(panes.owns_terminal(7));
        assert!(!panes.owns_terminal(8));
    }
}
