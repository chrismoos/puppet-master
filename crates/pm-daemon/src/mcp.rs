//! Minimal MCP server (streamable HTTP, stateless, JSON responses)
//! exposing the agent self-reporting tools. Authenticated by the
//! per-session bearer token, so the daemon always knows which session
//! is reporting. Only the subset both agent CLIs actually use is
//! implemented: initialize, ping, tools/list, tools/call, and the
//! initialized notification.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde_json::{json, Value};

use pm_protocol::domain::{
    AgentKind, ContextField, ContextKind, ContextSeverity, ItemPriority, ItemQuery, ItemSourceKind,
    ItemStatus, ItemWrite, PlanDecisionMode, PlanState, ITEM_STATUS_CREATE_GUIDANCE,
};

use tracing::{debug, warn};

use crate::daemon::{item_json, item_summary_json, session_json, AgentReport, Daemon, DaemonError};

pub const PROTOCOL_VERSION: &str = "2025-03-26";

const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;

/// How long an MCP client waits for a tool call before it abandons the
/// request and hands the agent a transport error instead of the answer.
/// Measured against a real client: 25s held, 30s did not.
const CLIENT_CALL_BUDGET_SECONDS: u64 = 30;

/// A call this slow is close enough to the budget that the agent may
/// have been given a transport error rather than the answer recorded
/// here, so it is logged whatever level the daemon is running at.
const SLOW_CALL: Duration = Duration::from_secs(CLIENT_CALL_BUDGET_SECONDS - 5);

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub async fn handle(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let Some(token) = bearer_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let outcome = dispatch(&daemon, token, request).await;
    match outcome.body {
        Some(body) => (
            StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::OK),
            Json(body),
        )
            .into_response(),
        None => StatusCode::from_u16(outcome.status)
            .unwrap_or(StatusCode::ACCEPTED)
            .into_response(),
    }
}

/// One handled request, independent of how it arrived. A host that cannot
/// reach this listener relays its agents' requests over the control link and
/// needs the same answers.
pub struct McpOutcome {
    pub status: u16,
    pub body: Option<Value>,
}

pub async fn dispatch(daemon: &Arc<Daemon>, token: &str, request: Value) -> McpOutcome {
    let started = Instant::now();
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let id = request["id"].clone();

    // Notifications carry no id and expect no body.
    if id.is_null() {
        return McpOutcome {
            status: StatusCode::ACCEPTED.as_u16(),
            body: None,
        };
    }

    let result = match method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "puppet-master", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": tool_definitions(
                daemon.session_offers_items_api(token),
                daemon.session_offers_supervisor_api(token),
                &daemon.spawnable_agents(),
            )
        })),
        "tools/call" => call_tool(daemon, token, &request["params"]).await,
        _ => Err((
            JSONRPC_METHOD_NOT_FOUND,
            format!("method {method:?} not supported"),
        )),
    };

    let body = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": code, "message": message }
        }),
    };
    let status = StatusCode::OK.as_u16();
    log_call(
        daemon,
        token,
        call_label(&request),
        started.elapsed(),
        status,
    );
    McpOutcome {
        status,
        body: Some(body),
    }
}

/// What a call is called in the log: the tool for a `tools/call`, the
/// method for everything else. Never the bearer token.
fn call_label(request: &Value) -> &str {
    let method = request["method"].as_str().unwrap_or_default();
    if method == "tools/call" {
        request["params"]["name"].as_str().unwrap_or(method)
    } else {
        method
    }
}

/// Records what was answered and how long it took, so a client that
/// gave up before this can be told apart from a daemon that never
/// answered. The session is resolved only for the slow line, which is
/// rare, rather than on every call.
fn log_call(daemon: &Arc<Daemon>, token: &str, call: &str, elapsed: Duration, status: u16) {
    let elapsed_ms = elapsed.as_millis() as u64;
    if elapsed < SLOW_CALL {
        debug!(call, elapsed_ms, status, "answered an mcp call");
        return;
    }
    let session = daemon.storage.get_session_id_by_token(token).ok().flatten();
    warn!(
        call,
        session = ?session,
        elapsed_ms,
        status,
        budget_ms = CLIENT_CALL_BUDGET_SECONDS * 1000,
        "an mcp call took long enough that the agent may have seen a transport error instead of \
         this answer"
    );
}

/// The shared input schema for one context field, so the agent chooses
/// `kind`/`severity` deliberately.
fn field_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "key": { "type": "string", "description": "Stable identifier, e.g. 'branch'" },
            "label": { "type": "string", "description": "Human-facing name shown by the value" },
            "value": { "type": "string" },
            "kind": {
                "type": "string",
                "enum": ["text", "code", "url", "badge", "metric", "progress", "timestamp"],
                "description": "How it renders: code=monospace, url=link, badge=colored pill, \
                    metric=number, progress=0-100, timestamp=relative time"
            },
            "severity": {
                "type": "string",
                "enum": ["neutral", "info", "good", "warn", "bad"],
                "description": "Colors a badge or metric"
            }
        },
        "required": ["key", "label", "value", "kind"]
    })
}

fn status_values() -> Vec<&'static str> {
    ItemStatus::ALL.iter().map(|s| s.as_str()).collect()
}

fn priority_values() -> Vec<&'static str> {
    ItemPriority::ALL.iter().map(|p| p.as_str()).collect()
}

fn source_kind_values() -> Vec<&'static str> {
    ItemSourceKind::ALL.iter().map(|k| k.as_str()).collect()
}

/// The item tools' shared per-item schema.
fn item_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "external_key": {
                "type": "string",
                "description": "Stable dedup identity, unique per bucket, e.g. \
                    'github:pr:org/repo#123', 'email:<message-id>', 'jira:ABC-42'. Always \
                    set it for items swept from an external source so re-sweeps update \
                    instead of duplicating. Omit only for one-off items (then use `id` to \
                    update later)."
            },
            "id": { "type": "integer", "description": "Item id, to update an item without an external_key" },
            "title": { "type": "string", "description": "Required to create. One terse line, the row on the board" },
            "body": {
                "type": "string",
                "maxLength": crate::storage::ITEM_BODY_MAX,
                "description": format!("Context the user needs to act: who asked, what for, key detail. Maximum {} Unicode scalar values; oversized bodies are rejected without modification.", crate::storage::ITEM_BODY_MAX)
            },
            "question": { "type": "string", "description": "Current question for the user; set to an empty string to clear" },
            "status": {
                "type": "string",
                "enum": status_values(),
                "description": format!("Omit when unchanged. {ITEM_STATUS_CREATE_GUIDANCE} \
                    `in_progress`=being worked; `blocked`=cannot proceed until the user or another \
                    item acts; `blocked_external`=someone outside must act (review requested, reply \
                    sent); `done`; `dropped`=deliberately not doing")
            },
            "priority": { "type": "string", "enum": priority_values() },
            "source_kind": { "type": "string", "enum": source_kind_values(), "description": "The channel this came from" },
            "source_detail": { "type": "string", "description": "Which mailbox/repo/channel/board, e.g. 'acme/api', '#proj-x'" },
            "url": { "type": "string", "description": "Link to the item's external source (the PR, ticket, thread)" },
            "project": { "description": "Project name or id in this bucket, when the item belongs to one" },
            "due": { "description": "Due time as RFC 3339/ISO 8601 with Z or a UTC offset, 'YYYY-MM-DD', or unix milliseconds" },
            "blocked_by": {
                "type": "array",
                "items": {},
                "description": "Items this one waits on: ids or external_keys already upserted. \
                    Order blockers before the items they block."
            },
            "note": { "type": "string", "description": "Timestamped timeline entry; REQUIRED when moving an item out of done/dropped" }
        }
    })
}

fn tool_definitions(items_api: bool, supervisor_api: bool, spawnable_agents: &[&str]) -> Value {
    let mut tools = base_tool_definitions();
    if let Value::Array(tools) = &mut tools {
        tools.extend(crate::connections::tool_definitions());
        if let Value::Array(reviews) = review_tool_definitions() {
            tools.extend(reviews);
        }
        if items_api {
            if let Value::Array(items) = item_tool_definitions() {
                tools.extend(items);
            }
        }
        if supervisor_api {
            if let Value::Array(supervisor) = supervisor_tool_definitions(spawnable_agents) {
                tools.extend(supervisor);
            }
        }
    }
    tools
}

/// The review loop. Deliberately event-driven: an agent takes work
/// when it is ready instead of holding a shell open for the whole
/// review, so the session stays usable and the review outlives it.
/// How long a review wait holds by default.
///
/// Measured against a real client: 25s holds, 30s comes back as a
/// transport error. So the limit is 30s and a default that sits on it
/// fails roughly whenever it is actually used, which reads as the
/// feature being broken rather than as nothing having arrived. This is
/// deliberately well under that, because the agent is told to call
/// straight back — the only thing a longer hold buys is fewer round
/// trips, and it buys them by gambling on the timeout.
pub const DEFAULT_REVIEW_WAIT_SECONDS: u64 = 20;

/// A caller may ask for longer, but not for so long that every client
/// gives up first.
pub const MAX_REVIEW_WAIT_SECONDS: u64 = 120;

/// The default has crept onto the client's limit twice, so this fails
/// the build rather than the review.
const _: () = assert!(
    DEFAULT_REVIEW_WAIT_SECONDS + 5 <= CLIENT_CALL_BUDGET_SECONDS,
    "the default review wait must stay clear of what an MCP client allows"
);
const _: () = assert!(MAX_REVIEW_WAIT_SECONDS <= 120);

/// An empty review hold returns just after its wait and is doing what
/// it is supposed to, so the slow-call line has to sit above it or
/// every idle poll reports one.
const _: () = assert!(
    DEFAULT_REVIEW_WAIT_SECONDS < CLIENT_CALL_BUDGET_SECONDS - 5,
    "an ordinary review hold must not log as a slow call"
);

fn review_tool_definitions() -> Value {
    json!([
        {
            "name": "open_review",
            "description": "Start a PR-style review of your work, or attach to the one already \
                covering it, and give the user the returned URL, then go straight into \
                next_review_event and stay there until the review is finished. State the context \
                outright: \
                Puppet Master does not guess it, because your session's launch directory is \
                fixed at spawn and you may have moved or created a worktree since. \
                Pass `worktree` (the absolute path of the tree you want read) and a `base` \
                that is already a resolved SHA — run `git rev-parse` yourself; a branch name \
                is rejected because a review must keep meaning what it meant after the ref \
                moves. The review reads your working tree, so the edits you make answering a \
                comment show up in it. `files` names an exact scope when you want one. To review \
                a document that is not in a repository at all, set `source_file` instead and \
                leave base and worktree empty. The reviewer comments in the browser; you take the \
                comments with next_review_event.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "worktree": { "type": "string", "description": "Absolute path of the working tree to read. Required for a range review; with source_file it defaults to the directory holding that file." },
                    "base": { "type": "string", "description": "Resolved base SHA, from `git rev-parse`. A ref name is refused." },
                    "pathspec": { "type": "array", "items": { "type": "string" }, "description": "Limit the review to these paths, for splitting a large branch into readable batches. Each filter is a separate review." },
                    "files": { "type": "array", "items": { "type": "string" }, "description": "Exactly these paths are the scope; nothing is enumerated. Use it to include a file git would not list, or to leave generated noise out." },
                    "source_file": { "type": "string", "description": "Absolute path of a document to review against an empty baseline, with no repository involved. Leave base and worktree empty when using this." },
                    "label": { "type": "string", "description": "What to call this review wherever a person sees it, including its tab. Name it for the work, not the range — 'session git field' reads better than '69db9c7..WORKING' when several are open at once. Defaults to the SHAs." },
                    "reset": { "type": "boolean", "description": "Discard existing threads and history for this target before opening." }
                }
            }
        },
        {
            "name": "next_review_event",
            "description": "Wait for the reviewer's next comment. This BLOCKS until one arrives, \
                so call it and stay in it — do not go do something else while a review is open. \
                Returns the thread with its current line and an excerpt of today's code, which you \
                should trust over the line the comment was originally written against. Address it \
                by editing the file in place, call post_review_reply, then call this again. Keep \
                looping until it tells you the review is finished. If it returns saying nothing \
                arrived in time, call it again immediately: that is a client timeout, not the end \
                of the review. Threads on one file are handed out one at a time, so you can work \
                on separate files concurrently without conflicting.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "wait_seconds": { "type": "integer", "description": "How long to hold the call open waiting for a comment. Defaults to 20. Clients commonly time out a request at 30s, so raise this only if you know yours allows longer — a hold that outlives the client comes back as a transport error, not as an answer." }
                }
            }
        },
        {
            "name": "post_review_reply",
            "description": "Reply to one review thread. Keep it short and direct — this is a \
                conversation, not documentation. Set `addressed` false when you decided not to \
                make the change, and say why in the body. Calling this captures the tree your \
                edits produced, so make the edits first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "thread_id": { "type": "integer", "description": "From next_review_event" },
                    "body": { "type": "string", "description": "What you did, or why you did not" },
                    "addressed": { "type": "boolean", "description": "False when you declined the change; defaults to true" }
                },
                "required": ["thread_id", "body"]
            }
        },
        {
            "name": "review_status",
            "description": "Open, answering, and resolved thread counts for this session's \
                reviews, so you can tell whether the reviewer is waiting on you.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

fn item_tool_definitions() -> Value {
    json!([
        {
            "name": "upsert_items",
            "description": "Create or update work items on this bucket's board — the user's \
                cross-channel work list on the dashboard. Use a stable `external_key` per item \
                so re-sweeps are idempotent: upserting an existing key updates only the fields \
                you send. Send `status` only when you know it changed. The server enforces the \
                user's decisions: an item that is done or dropped stays that way unless you \
                include a `note` saying why it reopened, and snoozes are untouchable — do not \
                re-file handled work. Order blockers before the items they block so `blocked_by` \
                references resolve. Returns each item's id; reference items in text as \
                pm:item/<bucket-id>/<id> and sessions as pm:session/<id>.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "maxItems": crate::storage::ITEM_BATCH_MAX,
                        "items": item_schema()
                    }
                },
                "required": ["items"]
            }
        },
        {
            "name": "list_items",
            "description": "List this bucket's work items as summary rows. Call this FIRST in \
                a sweep and reconcile against what the board already holds instead of \
                trusting memory — that is what makes re-sweeps safe. By default done/dropped \
                items are excluded (set include_done to audit them before filing something \
                you suspect was handled) and snoozed items are excluded (the user parked \
                those; do not re-argue them). Each row carries id, ref, title, status, \
                priority, source, timestamps, blocked_by edges, and body_chars, but not the \
                body or question text: read those with get_items for the few rows you need. \
                The response carries matching_total and next_offset, so narrow with search, \
                project, or updated_since rather than paging through a large board.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "statuses": { "type": "array", "items": { "type": "string", "enum": status_values() } },
                    "search": { "type": "string", "description": "Case-insensitive title, body, question, external key, source detail, or URL search" },
                    "project": { "description": "Project name or id to narrow to" },
                    "priorities": { "type": "array", "items": { "type": "string", "enum": priority_values() } },
                    "sources": { "type": "array", "items": { "type": "string", "enum": source_kind_values() } },
                    "updated_since": { "description": "Only items updated at/after this, as RFC 3339/ISO 8601 with Z or a UTC offset, 'YYYY-MM-DD', or unix milliseconds" },
                    "include_done": { "type": "boolean", "description": "Include done and dropped items" },
                    "include_snoozed": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": crate::storage::ITEM_QUERY_LIMIT_MAX, "default": LIST_ITEMS_PAGE_DEFAULT },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 }
                }
            }
        },
        {
            "name": "get_items",
            "description": "Read full items from this bucket, including body and question \
                text, by bucket-local id, canonical pm:item/<bucket>/<id>, or external_key. \
                Use it after list_items for the rows you actually need rather than listing \
                with a large limit. Every reference must resolve inside this bucket or the \
                call fails naming the offender.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": GET_ITEMS_MAX,
                        "items": { "description": "Bucket-local item id, canonical pm:item/<bucket>/<id>, or external_key" }
                    }
                },
                "required": ["items"]
            }
        },
        {
            "name": "attach_item_file",
            "description": "Attach a bounded file from this session's worker filesystem to an item in this bucket. Paths resolve only inside the session working directory; traversal and symlinks are rejected. Remote-worker files are read by that worker and transferred through the bounded worker protocol. Maximum 10 MiB per file and 50 MiB per item.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "item": { "description": "Bucket-local item id, canonical pm:item/<bucket>/<id>, or external_key" },
                    "path": { "type": "string", "description": "Absolute path inside the session cwd, or a path relative to it" },
                    "filename": { "type": "string", "description": "Optional safe display filename" },
                    "media_type": { "type": "string", "description": "Optional MIME display hint; active or invalid types fall back to application/octet-stream" }
                },
                "required": ["item", "path"]
            }
        },
        {
            "name": "list_item_attachments",
            "description": "List attachment metadata for one item in this bucket. File bytes are never embedded in normal item snapshots or this metadata response.",
            "inputSchema": {
                "type": "object",
                "properties": { "item": { "description": "Bucket-local item id, canonical pm:item/<bucket>/<id>, or external_key" } },
                "required": ["item"]
            }
        },
        {
            "name": "get_item_attachment",
            "description": "Retrieve one attachment as a bounded MCP blob resource. Agent retrieval is limited to 1 MiB to keep tool responses bounded; larger files remain available through the authenticated Board download endpoint.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "item": { "description": "Bucket-local item id, canonical pm:item/<bucket>/<id>, or external_key" },
                    "attachment_id": { "type": "integer", "minimum": 1 }
                },
                "required": ["item", "attachment_id"]
            }
        },
        {
            "name": "post_briefing",
            "description": "Post this bucket's briefing: one markdown page the user reads \
                instead of touring their inboxes. Lead with what needs the user right now, \
                then what is in flight, what is blocked and on what, and what completed since \
                the last briefing. Link every item and session you mention with normal \
                markdown links using pm:item/<bucket-id>/<id>, pm:session/<id>, \
                pm:project/<id> — the \
                dashboard resolves them; never use localhost URLs. Becomes the latest \
                briefing shown on the board (history is kept).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "markdown": { "type": "string" }
                },
                "required": ["markdown"]
            }
        }
    ])
}

/// Bytes of terminal tail returned when the caller does not choose.
const READ_TERMINAL_DEFAULT: usize = 16_384;

/// Largest terminal tail one read may return.
const READ_TERMINAL_MAX: usize = 65_536;

/// Sessions returned by list_sessions when the caller does not choose.
const LIST_SESSIONS_DEFAULT: usize = 50;
const WAIT_SESSIONS_MAX: usize = 128;
/// A held call has to come back before the client stops waiting for it,
/// or the caller gets a transport error instead of an answer and cannot
/// tell "nothing happened" from "the wait broke". The default sits a
/// clear margin inside the budget rather than on it: at exactly the
/// budget, every quiet wait races the client's own timer.
const WAIT_TIMEOUT_DEFAULT_MS: u64 = 20_000;
const WAIT_TIMEOUT_MIN_MS: u64 = 1_000;
const WAIT_TIMEOUT_MAX_MS: u64 = (CLIENT_CALL_BUDGET_SECONDS - 5) * 1_000;

const _: () = assert!(
    WAIT_TIMEOUT_MAX_MS < CLIENT_CALL_BUDGET_SECONDS * 1_000,
    "a wait that outlives the client comes back as a transport error"
);
const _: () = assert!(WAIT_TIMEOUT_DEFAULT_MS + 5_000 <= WAIT_TIMEOUT_MAX_MS);

/// The longest any tool holds a call open, in seconds. Same reasoning as
/// the wait above: a caller is told to come straight back rather than
/// being allowed to ask for a hold its own client will abandon.
const MAX_HOLD_SECONDS: u64 = CLIENT_CALL_BUDGET_SECONDS - 5;

fn supervisor_tool_definitions(spawnable_agents: &[&str]) -> Value {
    json!([
        {
            "name": "spawn_session",
            "description": "Spawn an agent session that works a board item. The session runs \
                in the chosen project exactly as the user configured it: the project's own \
                path and permission mode always apply and cannot be chosen here. For a \
                project configured on more than one host, the optional host argument picks \
                which configured host runs the session; omitted, the project's default \
                worker resolution applies. A spawn is refused when the project cannot \
                run on the resolved host, naming the project, the host, and the path: a \
                path that is unset, missing, not a directory, or unreadable is reported \
                as that rather than as the host being offline. The new session is linked \
                to the item and a note lands on the item's timeline. Returns the new \
                session id; watch it with session_status and read_terminal.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "description": "Project name or id in this bucket the session runs in" },
                    "agent": { "type": "string", "enum": spawnable_agents, "description": "Optional explicit agent override; omitted uses the project's effective default" },
                    "title": { "type": "string", "description": "One terse line for the session list; defaults to the prompt's first characters" },
                    "prompt": { "type": "string", "description": "The task prompt the agent starts with" },
                    "item": { "description": "REQUIRED. Board item this session works: a bucket-local id, canonical pm:item/<bucket>/<id>, or external_key" },
                    "host": { "description": "Optional worker to run on, as a worker id or name. Only workers the project allows are accepted; anything else is rejected with the valid choices. Omit to use the project's default." }
                },
                "required": ["project", "prompt", "item"]
            }
        },
        {
            "name": "session_status",
            "description": "A session you spawned: its state, dashboard report (headline, \
                summary, glance, context), linked items, and recent report timeline. Poll \
                this to follow progress without reading the terminal. Also returns host, \
                whether the controller can reach it, and project_host: whether the \
                project can run there and, when it cannot, which configured field is \
                wrong. Returns the bucket lifecycle cursor for a race-free follow-up \
                wait_sessions call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer", "description": "Session id returned by spawn_session" }
                },
                "required": ["session"]
            }
        },
        {
            "name": "snooze_supervision",
            "description": "Delay periodic idle reminders while waiting on progressing children or a user decision, and silence this turn's clean completion notification. Supervisor-only; accepts an active turn or needs-input state after flag_blocked. Snoozing preserves the blocked question and its alert. Defaults to 5 minutes, accepts 2-60. Child transitions still wake you immediately. A subsequent prompt-submitted hook restores completion notifications without canceling the reminder snooze. NeedsInput and Failed still notify. Prefer wait_sessions when actively supervising.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "minutes": { "type": "integer", "minimum": crate::supervisor_wake::MIN_SNOOZE_MINUTES, "maximum": crate::supervisor_wake::MAX_SNOOZE_MINUTES, "default": crate::supervisor_wake::DEFAULT_SNOOZE_MINUTES }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "wait_sessions",
            "description": "Wait for normalized lifecycle changes in any spawned child. Pass \
                the cursor from session_status, list_sessions, or the previous wait so a \
                transition between status and wait is returned immediately. Omitting \
                after_cursor returns an immediate baseline snapshot and cursor. Claude and \
                Codex hooks produce the same states. A submitted user prompt cancels the \
                wait promptly; waiting itself does not count as agent activity.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sessions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": WAIT_SESSIONS_MAX,
                        "items": { "type": "integer" },
                        "description": "One or more child session ids returned by spawn_session"
                    },
                    "after_cursor": { "type": "integer", "minimum": 0, "description": "Opaque cursor returned by a previous status/list/wait call" },
                    "timeout_ms": { "type": "integer", "minimum": WAIT_TIMEOUT_MIN_MS, "maximum": WAIT_TIMEOUT_MAX_MS, "default": WAIT_TIMEOUT_DEFAULT_MS, "description": "Bounded wait in ms. Clamped so the call always returns before a client gives up on it; when it comes back empty, call again" },
                    "states": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["starting", "working", "needs-input", "idle", "exited", "failed"] },
                        "description": "Optional destination-state filter; omitted or empty returns any lifecycle change"
                    }
                },
                "required": ["sessions"]
            }
        },
        {
            "name": "read_terminal",
            "description": "The tail of a spawned session's agent terminal, as plain text \
                with escape sequences stripped (set raw to keep them). Use it to see what \
                the agent is actually doing or why it is stuck.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer", "description": "Session id returned by spawn_session" },
                    "max_bytes": { "type": "integer", "description": "Tail size to return; default 16384, at most 65536" },
                    "raw": { "type": "boolean", "description": "Keep escape sequences instead of stripping them" }
                },
                "required": ["session"]
            }
        },
        {
            "name": "await_reply",
            "description": "Wait for the answer to a message you sent with reply=true. Holds \
                briefly and returns whatever it has, so call it straight back while the reply \
                is still outstanding. Reading never consumes the answer, and the answer also \
                arrives in this session on its own, so a wait that times out has lost nothing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message_id": { "type": "integer", "description": "From the send_input response" },
                    "wait_seconds": { "type": "integer", "description": "How long to hold, default 20. Clamped so the call always returns before a client gives up on it, so call straight back while the reply is outstanding rather than asking for a longer hold" }
                },
                "required": ["message_id"],
                "additionalProperties": false
            }
        },
        {
            "name": "send_input",
            "description": "Send input to a spawned session's agent terminal. Set submit=true \
                for commands or messages: Puppet Master removes trailing CR/LF terminators, \
                sends nonempty Codex/Claude text as one explicit paste, writes exactly one \
                Enter after the paste has settled, then waits briefly for the session's \
                Working transition (re-sending the Enter once if no turn begins). Branch on \
                input_state: 'submitted' means a turn began; 'queued_behind_turn' means the \
                agent was mid-turn and the message is queued in its composer (watch \
                wait_sessions); 'submission_unconfirmed' means no turn was observed — read the \
                terminal, or retry with empty text and submit=true to press Enter again \
                without duplicating the message; 'submit_undelivered' means the text landed \
                but the Enter did not — retry with empty text and submit=true, never resend \
                the text. Omitted submit defaults to false and preserves text byte-for-byte, \
                including legacy embedded CR/LF — so text containing a newline can still \
                submit even with submit=false; probe with text that has no CR/LF. The input \
                is audited on the item timeline.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer", "description": "Session id returned by spawn_session" },
                    "text": { "type": "string", "description": "Text to type; embedded CR/LF retain their legacy behavior" },
                    "submit": { "type": "boolean", "default": false, "description": "Queue exactly one final Enter after normalizing trailing CR/LF; defaults to false" },
                    "reply": { "type": "boolean", "default": false, "description": "Ask for one answer back. The message carries a single-use capability the other session spends with reply_message, the response returns the message_id to wait on, and the answer arrives both in this session and through await_reply. Needs submit=true" }
                },
                "required": ["session", "text"]
            }
        },
        {
            "name": "interrupt_session",
            "description": "Send an interrupt (Ctrl-C) to a spawned session's agent, like \
                pressing Escape on a runaway step. The session stays alive.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer", "description": "Session id returned by spawn_session" }
                },
                "required": ["session"]
            }
        },
        {
            "name": "resume_session",
            "description": "Resume an ended spawned session's agent conversation in place. \
                Live sessions and sessions without an available conversation transcript are \
                refused. Audited on the item timeline. Returns the resumed session id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer", "description": "Ended session id returned by spawn_session" }
                },
                "required": ["session"]
            }
        },
        {
            "name": "kill_session",
            "description": "Kill a spawned session's agent process for good. Use interrupt \
                first when a nudge might do; kill is for sessions that are wedged or no \
                longer needed. Audited on the item timeline.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "integer", "description": "Session id returned by spawn_session" }
                },
                "required": ["session"]
            }
        },
        {
            "name": "list_sessions",
            "description": "Sessions in this bucket, newest first, including ended ones — \
                reconcile against this instead of trusting memory. Sessions you spawned \
                carry your session id in spawned_by_session_id, and every row names the \
                host it runs on and whether the controller can reach it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "Most recent sessions to return; default 50" }
                }
            }
        },
        {
            "name": "list_instructions",
            "description": "List editable bucket instruction layers and, when project is supplied, that project's layers. Includes append-only revision history so durable changes can be audited and reverted. Built-in Puppet Master and role contracts are immutable and are not returned as editable layers.",
            "inputSchema": { "type":"object", "properties": { "project": { "description":"Optional project name or id in this bucket" } } }
        },
        {
            "name": "get_effective_instructions",
            "description": "Preview the deterministic instruction Markdown a new or resumed Worker or Supervisor would receive, after immutable contract, role, bucket, and optional project layers are compiled. This does not affect a running turn.",
            "inputSchema": { "type":"object", "properties": { "role": {"type":"string","enum":["worker","supervisor"]}, "project": {"description":"Optional project name or id in this bucket"} }, "required":["role"] }
        },
        {
            "name": "set_instructions",
            "description": "Create or replace one persistent instruction overlay inside your bucket. Use only when the user explicitly requests a durable rule. scope is bucket or project; project scope requires a project in this bucket. target may be all, worker, or supervisor. expected_revision is 0 to create or the current revision to update; conflicts never overwrite. Changes affect new spawns and resumes, not running turns.",
            "inputSchema": { "type":"object", "properties": { "scope":{"type":"string","enum":["bucket","project"]}, "project":{"description":"Required for project scope: name or id in this bucket"}, "role":{"type":"string","enum":["all","worker","supervisor"]}, "markdown":{"type":"string","maxLength":crate::storage::INSTRUCTION_MARKDOWN_MAX}, "expected_revision":{"type":"integer","minimum":0}, "note":{"type":"string"} }, "required":["scope","role","markdown","expected_revision"] }
        }
    ])
}

/// Reads the `git` object of a report into a partial update. Absent
/// keys stay `None` so they keep their stored values, and blank strings
/// are treated as absent rather than as a request to clear.
fn git_update(value: Option<&Value>) -> crate::storage::SessionGitUpdate {
    let mut update = crate::storage::SessionGitUpdate::default();
    let Some(obj) = value.and_then(Value::as_object) else {
        return update;
    };
    let text = |key: &str| -> Option<String> {
        let raw = obj.get(key)?.as_str()?.trim();
        (!raw.is_empty()).then(|| crate::daemon::truncate_chars(raw, crate::storage::GIT_VALUE_MAX))
    };
    update.branch = text("branch");
    update.worktree = text("worktree");
    update.repo_root = text("repo_root");
    update.commit = text("commit");
    update.upstream = text("upstream");
    update.dirty = obj.get("dirty").and_then(Value::as_bool);
    update
}

fn base_tool_definitions() -> Value {
    json!([
        {
            "name": "report",
            "description": "Update this session's live status on the dashboard a human is \
                watching. Call it whenever things meaningfully change — you start a step, \
                finish one, learn something, hit a blocker. `goal` is REQUIRED: it names what \
                the session is about and is its name in the list: a short noun phrase, e.g. \
                \"Moving auth to JWTs\". Goal may remain the same across turns, but should be \
                constantly updated to reflect the current goal. `headline` is REQUIRED and is the current step, or the outcome when the turn \
                ends: one terse present-tense line (<80 chars), e.g. \"Swapping cookie checks — \
                3/5 files\". Never put the step in the goal or the goal in the headline. Keep \
                headlines plain text (`&`, not `&amp;`). Everything else is optional and only \
                changes what you send: \
                `summary` (a few sentences for the detail view), `note` (a timestamped \
                timeline entry), `glance` (the 1–3 most important chips on the row — \
                replace-all, most important first, extremely terse; max 3), `context` (fuller \
                key/value facts for the detail panel — upserts by key, e.g. branch, \
                preview_url, tests), and `clear` (context keys to drop when stale). Keep \
                `glance` and `context` small, current, and high-signal. Treat 20 context fields \
                as a ceiling, not a target, and use `clear` to drop stale keys. Prefer \
                well-known keys (`status`, `tests`, `preview_url`). \
                `git` is its own structured field — report the branch, worktree, and \
                anything else you know there rather than spending a glance chip or a \
                context key on it, and resend it whenever you check out or move. \
                This creates no files or output — it only updates the dashboard.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "goal": { "type": "string", "description": "REQUIRED. What the session is about, a short noun phrase that names it in the list (e.g. 'Optimizing femtocell software'). Goal may remain the same across turns, but should be constantly updated to reflect the current goal." },
                    "headline": { "type": "string", "description": "REQUIRED. The current step, or the outcome when the turn ends: one terse present-tense plain-text line (<80 chars); use '&', not '&amp;'" },
                    "summary": { "type": "string", "description": "A few sentences for the detail view" },
                    "note": { "type": "string", "description": "A timestamped timeline entry, e.g. 'tests passing'" },
                    "glance": { "type": "array", "description": "The 1–3 highest-signal current chips on the row (replace-all, max 3)", "items": field_schema() },
                    "context": { "type": "array", "description": "Small set of current, high-signal detail facts, upserted by key (20 is a ceiling, not a target)", "items": field_schema() },
                    "clear": { "type": "array", "description": "Stale context keys to remove", "items": { "type": "string" } },
                    "git": {
                        "type": "object",
                        "description": "Where this session sits in git. Send what you know; an omitted field keeps its stored value, so a branch change needs only `branch`. Resend after any checkout, worktree move, or commit.",
                        "properties": {
                            "branch": { "type": "string", "description": "Current branch, or a short SHA when HEAD is detached" },
                            "worktree": { "type": "string", "description": "Working tree this session operates in; differs from repo_root in a linked worktree" },
                            "repo_root": { "type": "string", "description": "Root of the main checkout the worktree belongs to" },
                            "commit": { "type": "string", "description": "Short SHA of HEAD" },
                            "upstream": { "type": "string", "description": "Tracking ref, e.g. 'origin/master'" },
                            "dirty": { "type": "boolean", "description": "Whether the tree has uncommitted changes" }
                        }
                    }
                },
                "required": ["goal", "headline"]
            }
        },
        {
            "name": "flag_blocked",
            "description": "Signal that you are blocked waiting on the user. Call this the \
                moment you need a decision, clarification, or permission to continue.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "What you need from the user" }
                },
                "required": ["question"]
            }
        },
        {
            "name": "reply_message",
            "description": "Answer a message that asked you for a reply. The message \
                carries the message_id and reply_token to pass back, and the capability works \
                exactly once, so put your whole answer in one call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message_id": { "type": "integer", "description": "From the message you are answering" },
                    "reply_token": { "type": "string", "description": "The single-use token that came with it" },
                    "body": { "type": "string", "description": "Your answer" }
                },
                "required": ["message_id", "reply_token", "body"],
                "additionalProperties": false
            }
        },
        {
            "name": "publish_port",
            "description": "You started a server (dev server, API, preview) the user may want \
                to open. localhost URLs are unreachable for the user — call this with the \
                local port and a slug naming it, and give the user the returned URL instead. \
                Never show the user a localhost URL. The slug names the forward, and where \
                the controller serves forwards under a share domain it becomes the \
                hostname: slug 'docs-preview' is served at https://docs-preview.<domain>/. \
                Choose one that says what the server is. Idempotent: republishing a port \
                with the same slug returns its existing URL. Republishing it under a \
                different slug is refused, because the first URL is already in the user's \
                hands — close the forward and publish again to rename it. A slug another \
                forward already holds is refused too, and the error names it. \
                Published ports survive restarts of this session, and a session that \
                resumes is told which forwards it published: restart each server on the \
                port named there and the same public URL works again, without publishing \
                it a second time. If this \
                fails because the controller has no public URL configured, that is an \
                operator setting, not something retrying fixes — tell the user what the \
                error says and move on.\n\
                \n\
                The returned URL requires the user to be signed in to the dashboard. \
                Signed-in visitors return automatically through the dashboard handoff. \
                Signed-out visitors log in and then return to the forward. A login prompt is NOT a \
                failure — do not republish or retry when you see one. Show the URL \
                exactly as returned; do not reconstruct, shorten, or strip query \
                parameters from it. The URL is not a shareable public link — it is \
                tied to the user's dashboard session. HTTP previews are mounted under \
                /forwards/{id}/ and that prefix is stripped upstream. Use relative URLs \
                for assets, navigation, forms, fetch and WebSockets, or configure the \
                app base path to the returned URL path. Root-relative /assets or /api \
                URLs and parent paths that escape the mount will not work. Configure \
                HMR under the same prefix. Serve loopback HTTP; public HTTPS is handled \
                by the normal controller entry point.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": { "type": "integer", "description": "The local port your server listens on" },
                    "slug": { "type": "string", "description": "Names this forward, and is the hostname under a share domain. A DNS label: 3 to 40 characters of lowercase a-z, 0-9 and hyphen, starting and ending alphanumeric, with no consecutive hyphens. Uppercase is rejected rather than lowercased. 'www', 'api', 'app', 'admin', 'share', 'pm' and 'mail' are reserved, as is the letter f followed by digits. Must be unique across every live forward on this controller. Example: 'docs-preview'." },
                    "label": { "type": "string", "description": "Optional longer name shown in the dashboard, e.g. 'vite dev server'. Defaults to the slug." },
                    "scheme": { "type": "string", "description": "Target protocol, default http. HTTP/ws use the normal public URL. Raw TCP retains a separate listener. Serve loopback HTTP for HTTPS public URLs." }
                },
                "required": ["port", "slug"]
            }
        },
        {
            "name": "publish_dir",
            "description": "Share a directory of files with the user — a report, screenshots, a \
                built site, anything you have written to disk. Give the path and a slug: the \
                host running this session serves the directory over HTTP and returns a URL, so \
                you never start a server of your own, never pick a port, and never restart \
                anything. The share stays up while the session does, including across restarts \
                of this session and of the host, and the URL keeps working throughout. Prefer \
                this over publish_port for anything that is just files. If the directory holds \
                an index.html it is served at the URL root, otherwise the URL lists the files. \
                The directory stays live: files you write afterwards appear without republishing. \
                \n\
                The path must be inside the session working directory. Dot-files and anything \
                reached through a symlink are never served, a file whose name or first bytes \
                read as key material is refused so a secret is not published by accident, and \
                a directory that is itself a git repository root is refused — point at the \
                subdirectory holding the artifacts instead. Show the user the returned URL exactly as given; it \
                requires them to be signed in to the dashboard, and a login prompt is normal \
                rather than a failure. Close it with unpublish_dir, which never touches the \
                files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The directory to share, relative to the session working directory, or an absolute path inside it" },
                    "slug": { "type": "string", "description": "Names this share, and is the hostname under a share domain. A DNS label: 3 to 40 characters of lowercase a-z, 0-9 and hyphen, starting and ending alphanumeric, with no consecutive hyphens. 'www', 'api', 'app', 'admin', 'share', 'pm' and 'mail' are reserved, as is the letter f followed by digits. Must be unique across every live forward and share on this controller. Example: 'render-report'." },
                    "label": { "type": "string", "description": "Optional longer name shown in the dashboard. Defaults to the slug." }
                },
                "required": ["path", "slug"]
            }
        },
        {
            "name": "list_dirs",
            "description": "List the directories this session publishes, with each one's slug, \
                path and public URL.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "unpublish_dir",
            "description": "Stop sharing a published directory. The URL stops working and the \
                dashboard entry disappears. The files themselves are never touched.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "The slug the directory was published under" }
                },
                "required": ["slug"]
            }
        },
        {
            "name": "unpublish_port",
            "description": "Close a previously published port when its server is gone for \
                good. The public URL stops working and the dashboard entry disappears.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": { "type": "integer", "description": "The local port to unpublish" }
                },
                "required": ["port"]
            }
        },
        {
            "name": "upsert_plan",
            "description": "Create or update this session's first-class planning workspace. Write the canonical Markdown to a durable file in the repository or project directory early, keep it current, and pass its path here. Omit plan to create; otherwise only supplied fields change.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "plan": { "type": "integer", "description": "Existing plan id; omit to create" },
                    "name": { "type": "string" },
                    "summary": { "type": "string" },
                    "markdown_path": { "type": "string", "description": "Durable Markdown path inside this session's working directory" },
                    "state": { "type": "string", "enum": ["active", "accepted", "archived"] },
                    "linked_items": { "type": "array", "items": { "type": "integer" }, "description": "Bucket-local board item numbers; replaces links when supplied" }
                }
            }
        },
        {
            "name": "sync_plan",
            "description": "Read the durable Markdown file through this session's local or remote worker, store a new current snapshot, and refresh the planning UI. Call after materially changing the plan document.",
            "inputSchema": { "type": "object", "properties": { "plan": {"type":"integer"}, "markdown_path": {"type":"string"} }, "required": ["plan"] }
        },
        {
            "name": "present_plan_decision",
            "description": "Create or revise the one decision the user should focus on now. This makes the planning workspace the session's NeedsInput affordance. Option details are flexible Markdown; keep the option list labels compact. After calling this tool successfully, end your turn immediately and wait for the user's response. When the response arrives, resolve it and present the next decision before starting deeper work.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "plan": {"type":"integer"}, "key": {"type":"string"}, "title": {"type":"string"},
                    "prompt_markdown": {"type":"string"}, "detail_markdown": {"type":"string"},
                    "mode": {"type":"string","enum":["single","multiple","dialogue"]},
                    "allow_custom": {"type":"boolean"},
                    "require_selection": {"type":"boolean","description":"When false the user may submit with no selection; default true"},
                    "recommended_key": {"type":"string","description":"Optional key of the recommended option (for single-mode decisions)"},
                    "options": {"type":"array","items":{"type":"object","properties":{"key":{"type":"string"},"label":{"type":"string"},"detail_markdown":{"type":"string"},"recommended":{"type":"boolean","description":"When true and mode is single, hints this is the agent's recommended option"}},"required":["key","label"]}}
                },
                "required": ["plan","key","title","mode"]
            }
        },
        {
            "name": "present_plan_decision_batch",
            "description": "Present 2 to 8 independent decisions that the user can answer in one pass and submit atomically. Use a stable batch key and only group questions whose answers do not depend on one another. After calling this tool successfully, end your turn immediately. When the batch response arrives, resolve every answer and present the next decision or independent batch before starting deeper work.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "plan": {"type":"integer"},
                    "batch_key": {"type":"string"},
                    "decisions": {
                        "type":"array", "minItems":2, "maxItems":8,
                        "items": {
                            "type":"object",
                            "properties": {
                                "key":{"type":"string"}, "title":{"type":"string"},
                                "prompt_markdown":{"type":"string"}, "detail_markdown":{"type":"string"},
                                "mode":{"type":"string","enum":["single","multiple"]},
                                "allow_custom":{"type":"boolean"},
                                "require_selection":{"type":"boolean","description":"When false the user may submit with no selection; default true"},
                                "recommended_key":{"type":"string","description":"Optional key of the recommended option (for single-mode decisions)"},
                                "options":{"type":"array","items":{"type":"object","properties":{"key":{"type":"string"},"label":{"type":"string"},"detail_markdown":{"type":"string"},"recommended":{"type":"boolean","description":"When true and mode is single, hints this is the agent's recommended option"}},"required":["key","label"]}}
                            },
                            "required":["key","title","mode"]
                        }
                    }
                },
                "required":["plan","batch_key","decisions"]
            }
        },
        {
            "name": "resolve_plan_decision",
            "description": "Resolve the named decision after processing the user's response. The response remains in the plan history; present another decision to advance the center pane automatically.",
            "inputSchema": {"type":"object","properties":{"plan":{"type":"integer"},"key":{"type":"string"},"resolution_markdown":{"type":"string"}},"required":["plan","key"]}
        },
        {
            "name": "post_plan_message",
            "description": "Post an agent message to the plan-level or active-decision dialogue without changing the canonical Markdown.",
            "inputSchema": {"type":"object","properties":{"plan":{"type":"integer"},"decision":{"type":"integer"},"body":{"type":"string"}},"required":["plan","body"]}
        },
        {
            "name": "get_plan",
            "description": "Read one plan with its current Markdown snapshot, decisions, responses, and dialogue.",
            "inputSchema": {"type":"object","properties":{"plan":{"type":"integer"}},"required":["plan"]}
        },
        {
            "name": "list_plans",
            "description": "List plans in this session's project. Archived plans are omitted unless requested.",
            "inputSchema": {"type":"object","properties":{"include_archived":{"type":"boolean"}}}
        }
    ])
}

/// Parses the `fields` argument array into domain context fields,
/// falling unknown kinds/severities back to text/neutral.
fn parse_fields(value: &Value) -> Vec<ContextField> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|f| ContextField {
                    key: f["key"].as_str().unwrap_or_default().to_string(),
                    label: crate::text::unescape_html_entities(
                        f["label"].as_str().unwrap_or_default(),
                    ),
                    value: crate::text::unescape_html_entities(
                        f["value"].as_str().unwrap_or_default(),
                    ),
                    kind: ContextKind::parse_or_text(f["kind"].as_str().unwrap_or_default()),
                    severity: ContextSeverity::parse_or_neutral(
                        f["severity"].as_str().unwrap_or_default(),
                    ),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Wraps a tool outcome as MCP call content.
/// Appended to a publish result when the preview will be served on the
/// dashboard's own origin.
///
/// The agent is told to hand this URL to the user, and under the default mount
/// opening it gives the page the signed-in user's authority over the
/// controller. The daemon says so at startup, but this is the moment the URL
/// changes hands, so it says so here too. Only when it is true: under a share
/// domain it is not.
fn same_origin_caveat(daemon: &Daemon) -> &'static str {
    match daemon
        .forward_mount_mode()
        .origin_sharing(daemon.public_url().unwrap_or_default())
    {
        crate::forward_mount::OriginSharing::SameOrigin => {
            " This controller serves previews on the dashboard's own origin, so tell the \
             user that opening it runs the page with their access to this controller, and \
             only publish work they would run themselves."
        }
        _ => "",
    }
}

fn tool_text(is_error: bool, text: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

fn attachment_json(attachment: &pm_protocol::domain::ItemAttachment) -> Value {
    json!({
        "id": attachment.id,
        "bucket_id": attachment.bucket_id,
        "item_id": attachment.item_id,
        "filename": attachment.filename,
        "media_type": attachment.media_type,
        "byte_length": attachment.byte_length,
        "sha256": attachment.sha256,
        "created_at_unix_ms": attachment.created_at_unix_ms,
        "created_by_session_id": attachment.created_by_session_id,
    })
}

fn attachment_blob_result(attachment: crate::storage::ItemAttachmentContent) -> Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(&attachment.content);
    let uri = format!("pm://item-attachments/{}", attachment.metadata.id);
    json!({
        "content": [{
            "type": "resource",
            "resource": {
                "uri": uri,
                "mimeType": attachment.metadata.media_type,
                "blob": encoded
            }
        }],
        "structuredContent": attachment_json(&attachment.metadata),
        "isError": false
    })
}

fn input_tool_result(outcome: crate::daemon::SupervisorInputOutcome) -> Value {
    let is_error = matches!(outcome.delivery, "not_delivered" | "partial");
    json!({
        "content": [{ "type": "text", "text": outcome.message }],
        "structuredContent": outcome,
        "isError": is_error
    })
}

fn parse_item_status(v: &Value) -> Result<Option<ItemStatus>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => ItemStatus::parse(s).map(Some).ok_or_else(|| {
            format!(
                "unknown status {s:?}; valid: {}",
                status_values().join(", ")
            )
        }),
        _ => Err("status must be a string".into()),
    }
}

fn parse_item_priority(v: &Value) -> Result<Option<ItemPriority>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => ItemPriority::parse(s).map(Some).ok_or_else(|| {
            format!(
                "unknown priority {s:?}; valid: {}",
                priority_values().join(", ")
            )
        }),
        _ => Err("priority must be a string".into()),
    }
}

fn parse_item_source_kind(v: &Value) -> Result<Option<ItemSourceKind>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => ItemSourceKind::parse(s).map(Some).ok_or_else(|| {
            format!(
                "unknown source_kind {s:?}; valid: {}",
                source_kind_values().join(", ")
            )
        }),
        _ => Err("source_kind must be a string".into()),
    }
}

fn parse_filter_list<T>(
    args: &Value,
    name: &str,
    parse: fn(&Value) -> Result<Option<T>, String>,
) -> Result<Vec<T>, String> {
    match &args[name] {
        Value::Null => Ok(Vec::new()),
        Value::Array(values) => values
            .iter()
            .map(|value| parse(value)?.ok_or_else(|| format!("{name} entries cannot be null")))
            .collect(),
        _ => Err(format!("{name} must be an array")),
    }
}

/// Days since 1970-01-01 for a proleptic-Gregorian civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const MS_PER_DAY: i64 = 86_400_000;

fn parse_civil_date(s: &str) -> Option<i64> {
    let mut parts = s.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * MS_PER_DAY)
}

const TIME_FORMATS: &str =
    "RFC 3339/ISO 8601 with Z or a UTC offset, 'YYYY-MM-DD', or unix milliseconds";

/// A time argument: RFC 3339, a 'YYYY-MM-DD' date, or unix milliseconds.
fn parse_time(field: &str, v: &Value) -> Result<Option<i64>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{field} must be {TIME_FORMATS}")),
        Value::String(s) => parse_civil_date(s)
            .or_else(|| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|timestamp| timestamp.timestamp_millis())
            })
            .map(Some)
            .ok_or_else(|| format!("unparseable {field} {s:?}; use {TIME_FORMATS}")),
        _ => Err(format!("{field} must be {TIME_FORMATS}")),
    }
}

/// One batch entry after the parse phase: the write minus its
/// blocked_by references, which resolve at apply time so earlier batch
/// entries are visible.
struct ParsedItem {
    write: ItemWrite,
    blocked_refs: Option<Vec<Value>>,
    label: String,
}

fn parse_item(daemon: &Daemon, bucket_id: u64, v: &Value) -> Result<ParsedItem, String> {
    if !v.is_object() {
        return Err("each item must be an object".into());
    }
    let text = |key: &str| -> Result<Option<String>, String> {
        match &v[key] {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(s.clone())),
            _ => Err(format!("{key} must be a string")),
        }
    };
    let project_id = match &v["project"] {
        Value::Null => None,
        reference => Some(
            daemon
                .resolve_project_in_bucket(bucket_id, reference)
                .map_err(|e| e.to_string())?,
        ),
    };
    let blocked_refs = match &v["blocked_by"] {
        Value::Null => None,
        Value::Array(refs) => {
            for r in refs {
                if !r.is_string() && !r.is_u64() {
                    return Err("blocked_by entries must be item ids or external_keys".into());
                }
            }
            Some(refs.clone())
        }
        _ => return Err("blocked_by must be an array".into()),
    };
    let external_key = text("external_key")?;
    let id = v["id"].as_u64();
    let title = text("title")?;
    let label = external_key
        .clone()
        .or_else(|| id.map(|i| format!("item {i}")))
        .or_else(|| title.clone())
        .unwrap_or_default();
    Ok(ParsedItem {
        write: ItemWrite {
            bucket_id,
            id,
            external_key,
            title,
            body: text("body")?,
            question: text("question")?,
            status: parse_item_status(&v["status"])?,
            priority: parse_item_priority(&v["priority"])?,
            source_kind: parse_item_source_kind(&v["source_kind"])?,
            source_detail: text("source_detail")?,
            url: text("url")?,
            project_id,
            clear_project: false,
            due_at_unix_ms: parse_time("due", &v["due"])?,
            clear_due: false,
            blocked_by: None,
            note: text("note")?,
            link_session_id: None,
        },
        blocked_refs,
        label,
    })
}

fn upsert_items(daemon: &Daemon, session_id: u64, bucket_id: u64, args: &Value) -> Value {
    let Some(items) = args["items"].as_array() else {
        return tool_text(true, "items must be an array".into());
    };
    if items.is_empty() {
        return tool_text(true, "items must not be empty".into());
    }
    if items.len() > crate::storage::ITEM_BATCH_MAX {
        return tool_text(
            true,
            format!(
                "at most {} items per call; split the batch",
                crate::storage::ITEM_BATCH_MAX
            ),
        );
    }

    let body_errors: Vec<Value> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let body = item.get("body")?.as_str()?;
            let actual = body.chars().count();
            (actual > crate::storage::ITEM_BODY_MAX).then(|| {
                json!({
                    "index": index,
                    "field": "body",
                    "code": "max_length",
                    "limit": crate::storage::ITEM_BODY_MAX,
                    "actual": actual,
                    "unit": "Unicode scalar values",
                    "message": format!(
                        "body is {actual} Unicode scalar values, limit is {}",
                        crate::storage::ITEM_BODY_MAX
                    )
                })
            })
        })
        .collect();
    if !body_errors.is_empty() {
        return tool_text(
            true,
            json!({
                "error": "validation_failed",
                "message": "invalid item bodies; nothing was written",
                "details": body_errors,
            })
            .to_string(),
        );
    }

    // Parse phase: malformed input rejects the whole batch so a typo
    // never half-applies a sweep.
    let mut parsed = Vec::new();
    let mut problems = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        match parse_item(daemon, bucket_id, item) {
            Ok(p) => parsed.push(p),
            Err(e) => problems.push(format!("items[{idx}]: {e}")),
        }
    }
    if !problems.is_empty() {
        return tool_text(
            true,
            format!(
                "invalid items — nothing was written:\n{}",
                problems.join("\n")
            ),
        );
    }

    // Apply phase: per-item outcomes, so one sticky-rule refusal does
    // not discard the rest of the sweep.
    let mut results = Vec::new();
    let mut notes = Vec::new();
    let mut failures = 0usize;
    for (idx, mut p) in parsed.into_iter().enumerate() {
        if let Some(refs) = p.blocked_refs.take() {
            let ids: Result<Vec<u64>, _> = refs
                .iter()
                .map(|r| daemon.resolve_item_ref(bucket_id, r))
                .collect();
            match ids {
                Ok(ids) => p.write.blocked_by = Some(ids),
                Err(e) => {
                    failures += 1;
                    results.push(json!({ "index": idx, "key": p.label, "error": e.to_string() }));
                    continue;
                }
            }
        }
        match daemon.upsert_item(bucket_id, &p.write, Some(session_id)) {
            Ok((item, outcome, truncated)) => {
                if !truncated.is_empty() {
                    notes.push(format!(
                        "items[{idx}]: truncated {} to fit",
                        truncated.join(", ")
                    ));
                }
                results.push(json!({
                    "index": idx, "key": p.label, "id": item.id, "outcome": outcome.as_str(),
                }));
            }
            Err(e) => {
                failures += 1;
                results.push(json!({ "index": idx, "key": p.label, "error": e.to_string() }));
            }
        }
    }
    let all_failed = failures == items.len();
    let mut body = json!({ "results": results });
    if !notes.is_empty() {
        body["notes"] = json!(notes);
    }
    tool_text(all_failed, body.to_string())
}

fn list_items(daemon: &Daemon, bucket_id: u64, args: &Value) -> Value {
    let mut statuses = Vec::new();
    match &args["statuses"] {
        Value::Null => {}
        Value::Array(values) => {
            for value in values {
                match parse_item_status(value) {
                    Ok(Some(s)) => statuses.push(s),
                    Ok(None) => {}
                    Err(e) => return tool_text(true, e),
                }
            }
        }
        _ => return tool_text(true, "statuses must be an array".into()),
    }
    let project_id = match &args["project"] {
        Value::Null => None,
        reference => match daemon.resolve_project_in_bucket(bucket_id, reference) {
            Ok(id) => Some(id),
            Err(e) => return tool_text(true, e.to_string()),
        },
    };
    let updated_since_unix_ms = match parse_time("updated_since", &args["updated_since"]) {
        Ok(v) => v,
        Err(e) => return tool_text(true, e),
    };
    let priorities = match parse_filter_list(args, "priorities", parse_item_priority) {
        Ok(v) => v,
        Err(e) => return tool_text(true, e),
    };
    let source_kinds = match parse_filter_list(args, "sources", parse_item_source_kind) {
        Ok(v) => v,
        Err(e) => return tool_text(true, e),
    };
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(LIST_ITEMS_PAGE_DEFAULT as u64)
        .clamp(1, crate::storage::ITEM_QUERY_LIMIT_MAX as u64) as u32;
    let offset = args["offset"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
    let query = ItemQuery {
        bucket_id,
        statuses,
        search: args["search"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        project_id,
        priorities,
        source_kinds,
        updated_since_unix_ms,
        include_closed: args["include_done"].as_bool().unwrap_or(false),
        include_snoozed: args["include_snoozed"].as_bool().unwrap_or(false),
        limit: Some(limit),
        offset,
        summary_filter: None,
    };
    match daemon.list_items_with_counts(&query) {
        Ok((items, counts)) => {
            let items: Vec<Value> = items.iter().map(item_summary_json).collect();
            let next_offset = (items.len() == limit as usize
                && (offset as u64).saturating_add(limit as u64) < counts.matching_total)
                .then_some(offset.saturating_add(limit));
            tool_text(
                false,
                json!({
                    "items": items,
                    "matching_total": counts.matching_total,
                    "next_offset": next_offset,
                })
                .to_string(),
            )
        }
        Err(e) => tool_text(true, e.to_string()),
    }
}

/// Default page for `list_items`. Small enough that an agent reads the page
/// instead of skimming it, and `matching_total` says when to narrow.
pub const LIST_ITEMS_PAGE_DEFAULT: u32 = 20;
/// Full items carry bodies up to `ITEM_BODY_MAX`, so one `get_items` call
/// stays bounded by count as well.
pub const GET_ITEMS_MAX: usize = 20;

fn get_items(daemon: &Daemon, bucket_id: u64, args: &Value) -> Value {
    let Some(references) = args["items"].as_array() else {
        return tool_text(true, "items must be an array of item references".into());
    };
    if references.is_empty() {
        return tool_text(true, "items must not be empty".into());
    }
    if references.len() > GET_ITEMS_MAX {
        return tool_text(
            true,
            format!(
                "items holds {} references; at most {GET_ITEMS_MAX} per call",
                references.len()
            ),
        );
    }
    let mut items = Vec::with_capacity(references.len());
    for reference in references {
        let id = match daemon.resolve_item_ref(bucket_id, reference) {
            Ok(id) => id,
            Err(e) => return tool_text(true, e.to_string()),
        };
        match daemon.get_item(bucket_id, id) {
            Ok(item) => items.push(item_json(&item)),
            Err(e) => return tool_text(true, e.to_string()),
        }
    }
    tool_text(false, json!({ "items": items }).to_string())
}

/// Characters of the prompt used as the session title when the
/// supervisor does not pass one, matching the CLI's default.
const SPAWN_TITLE_FROM_PROMPT: usize = 60;

async fn spawn_session_tool(
    daemon: &Daemon,
    supervisor_id: u64,
    bucket_id: u64,
    args: &Value,
) -> Value {
    let spawnable = daemon.spawnable_agents();
    let agent = match args.get("agent").and_then(Value::as_str) {
        Some(agent_str) => {
            match AgentKind::parse(agent_str).filter(|agent| spawnable.contains(&agent.as_str())) {
                Some(agent) => Some(agent),
                None => {
                    return tool_text(
                        true,
                        format!(
                            "unknown agent {agent_str:?}; valid: {}",
                            spawnable.join(", ")
                        ),
                    )
                }
            }
        }
        None => None,
    };
    let prompt = args["prompt"].as_str().unwrap_or_default();
    if prompt.trim().is_empty() {
        return tool_text(true, "prompt must not be empty".into());
    }
    if args["project"].is_null() {
        return tool_text(
            true,
            "project is required: a project name or id in this bucket".into(),
        );
    }
    if args["item"].is_null() {
        return tool_text(
            true,
            "item is required: the board item id or external_key this session works".into(),
        );
    }
    let title_owned;
    let title = match args["title"].as_str() {
        Some(t) if !t.trim().is_empty() => t,
        _ => {
            title_owned = prompt
                .chars()
                .take(SPAWN_TITLE_FROM_PROMPT)
                .collect::<String>();
            &title_owned
        }
    };
    match daemon
        .supervisor_spawn(
            supervisor_id,
            bucket_id,
            &args["project"],
            agent,
            title,
            prompt,
            &args["item"],
            &args["host"],
        )
        .await
    {
        Ok((session_id, item_id)) => match daemon.get_session_exact(session_id) {
            Ok(session) => tool_text(
                false,
                json!({ "session_id": session_id, "bucket_id": bucket_id, "item_id": item_id,
                    "item_ref": format!("pm:item/{bucket_id}/{item_id}"),
                    "agent": session.agent.as_str(), "agent_source": session.agent_source.as_str(),
                    "worker_id": session.worker_id,
                    "host": daemon.worker_name(session.worker_id).unwrap_or_default() })
                .to_string(),
            ),
            Err(e) => tool_text(true, e.to_string()),
        },
        Err(e) => tool_text(true, e.to_string()),
    }
}

async fn read_terminal_tool(
    daemon: &Daemon,
    supervisor_id: u64,
    session_id: u64,
    args: &Value,
) -> Value {
    let max_bytes = args["max_bytes"]
        .as_u64()
        .map(|v| v as usize)
        .unwrap_or(READ_TERMINAL_DEFAULT)
        .min(READ_TERMINAL_MAX);
    match daemon
        .supervisor_read_terminal(supervisor_id, session_id, max_bytes)
        .await
    {
        Ok(replay) => {
            let text = String::from_utf8_lossy(&replay);
            let text = if args["raw"].as_bool().unwrap_or(false) {
                text.into_owned()
            } else {
                strip_ansi(&text)
            };
            tool_text(false, text)
        }
        Err(e) => tool_text(true, e.to_string()),
    }
}

fn list_sessions_tool(daemon: &Daemon, bucket_id: u64, args: &Value) -> Value {
    let limit = args["limit"]
        .as_u64()
        .map(|v| v as usize)
        .unwrap_or(LIST_SESSIONS_DEFAULT);
    match daemon.supervisor_list_sessions_snapshot(bucket_id) {
        Ok((cursor, sessions)) => {
            let sessions: Vec<Value> = sessions
                .iter()
                .take(limit)
                .map(|session| {
                    let mut value = session_json(session);
                    value["generation"] = json!(daemon.session_generation(session.id));
                    value["host"] = daemon.host_json(session.worker_id);
                    value
                })
                .collect();
            tool_text(
                false,
                json!({
                    "cursor": cursor,
                    "sessions": sessions,
                })
                .to_string(),
            )
        }
        Err(e) => tool_text(true, e.to_string()),
    }
}

async fn wait_sessions_tool(
    daemon: &Daemon,
    supervisor_id: u64,
    bucket_id: u64,
    args: &Value,
) -> Result<Value, (i64, String)> {
    let values = args["sessions"].as_array().ok_or((
        JSONRPC_INVALID_PARAMS,
        "sessions must be a non-empty array of session ids".to_string(),
    ))?;
    if values.is_empty() || values.len() > WAIT_SESSIONS_MAX {
        return Err((
            JSONRPC_INVALID_PARAMS,
            format!("sessions must contain 1-{WAIT_SESSIONS_MAX} session ids"),
        ));
    }
    let mut session_ids = Vec::with_capacity(values.len());
    let mut seen = HashSet::new();
    for value in values {
        let Some(session_id) = value.as_u64() else {
            return Err((
                JSONRPC_INVALID_PARAMS,
                "sessions must contain only integer session ids".into(),
            ));
        };
        if seen.insert(session_id) {
            session_ids.push(session_id);
        }
    }
    let after_cursor = match args.get("after_cursor") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or((
            JSONRPC_INVALID_PARAMS,
            "after_cursor must be a non-negative integer".into(),
        ))?),
    };
    let timeout_ms = match args.get("timeout_ms") {
        None | Some(Value::Null) => WAIT_TIMEOUT_DEFAULT_MS,
        Some(value) => value.as_u64().ok_or((
            JSONRPC_INVALID_PARAMS,
            "timeout_ms must be a positive integer".into(),
        ))?,
    }
    .clamp(WAIT_TIMEOUT_MIN_MS, WAIT_TIMEOUT_MAX_MS);
    let mut states = HashSet::new();
    if let Some(values) = args.get("states").filter(|value| !value.is_null()) {
        let values = values.as_array().ok_or((
            JSONRPC_INVALID_PARAMS,
            "states must be an array of lifecycle state names".into(),
        ))?;
        for value in values {
            let Some(state) = value
                .as_str()
                .and_then(pm_protocol::domain::SessionState::parse)
            else {
                return Err((
                    JSONRPC_INVALID_PARAMS,
                    "states may contain starting, working, needs-input, idle, exited, or failed"
                        .into(),
                ));
            };
            states.insert(state);
        }
    }
    Ok(
        match daemon
            .supervisor_wait_sessions(
                supervisor_id,
                bucket_id,
                session_ids,
                after_cursor,
                std::time::Duration::from_millis(timeout_ms),
                states,
            )
            .await
        {
            Ok(response) => tool_text(false, response.to_string()),
            Err(error) => tool_text(true, error.to_string()),
        },
    )
}

/// Removes escape sequences and non-printing control bytes so a
/// terminal tail reads as plain text.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameter and intermediate bytes, then one
                // final byte in @..~.
                Some('[') => {
                    for f in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&f) {
                            break;
                        }
                    }
                }
                // OSC: terminated by BEL or ESC \.
                Some(']') => {
                    let mut prev = '\0';
                    for f in chars.by_ref() {
                        if f == '\u{7}' || (prev == '\u{1b}' && f == '\\') {
                            break;
                        }
                        prev = f;
                    }
                }
                // Charset designation carries one more byte.
                Some('(') | Some(')') => {
                    chars.next();
                }
                _ => {}
            },
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn post_briefing(daemon: &Daemon, session_id: u64, bucket_id: u64, args: &Value) -> Value {
    let markdown = args["markdown"].as_str().unwrap_or_default();
    if markdown.trim().is_empty() {
        return tool_text(true, "markdown must not be empty".into());
    }
    match daemon.post_briefing(bucket_id, Some(session_id), markdown) {
        Ok((_, truncated)) => {
            let mut note = "briefing posted; it is now the latest for this bucket".to_string();
            if truncated {
                note.push_str(&format!(
                    "; markdown was truncated to {} characters",
                    crate::storage::BRIEFING_MAX
                ));
            }
            tool_text(false, note)
        }
        Err(e) => tool_text(true, e.to_string()),
    }
}

const REPORT_FIELDS: &[&str] = &[
    "goal", "headline", "summary", "note", "glance", "context", "clear", "git",
];
const FLAG_BLOCKED_FIELDS: &[&str] = &["question"];

/// A tool call whose arguments carry keys the schema does not define is a
/// guessed payload, and applying the known subset would report success for
/// an update that never happened.
fn check_known_keys(args: &Value, known: &[&str]) -> Result<(), String> {
    let Some(object) = args.as_object() else {
        return if args.is_null() {
            Ok(())
        } else {
            Err("arguments must be a JSON object".into())
        };
    };
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !known.contains(key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(format!(
        "unknown field{} {}; this tool takes {}. Load the tool's schema instead of guessing \
         its shape",
        if unknown.len() == 1 { "" } else { "s" },
        unknown
            .iter()
            .map(|key| format!("{key:?}"))
            .collect::<Vec<_>>()
            .join(", "),
        known.join(", "),
    ))
}

/// A report with a blank headline may still carry a note or a chip, but one
/// with nothing to apply would answer "recorded" while changing nothing.
fn check_report_arguments(args: &Value) -> Result<(), String> {
    check_known_keys(args, REPORT_FIELDS)?;
    let headline_present = args["headline"]
        .as_str()
        .is_some_and(|headline| !headline.trim().is_empty());
    if headline_present {
        return Ok(());
    }
    let has_other = REPORT_FIELDS
        .iter()
        .filter(|key| **key != "headline")
        .any(|key| match &args[*key] {
            Value::Null => false,
            Value::String(text) => !text.trim().is_empty(),
            Value::Array(items) => !items.is_empty(),
            Value::Object(fields) => !fields.is_empty(),
            _ => true,
        });
    if has_other {
        return Ok(());
    }
    Err(
        "report needs a headline: one terse present-tense line saying what you are doing now"
            .into(),
    )
}

async fn call_tool(
    daemon: &Arc<Daemon>,
    token: &str,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = &params["arguments"];
    if crate::connections::tool_definitions()
        .iter()
        .any(|tool| tool["name"] == name)
    {
        let result = crate::connections::dispatch(daemon, token, name, args).await;
        return Ok(match result {
            Ok(value) => json!({"content":[{"type":"text","text":value.to_string()}]}),
            Err(error) => {
                json!({"isError":true,"content":[{"type":"text","text":error.to_string()}]})
            }
        });
    }
    let str_arg = |key: &str| args[key].as_str().unwrap_or_default().to_string();

    let port_arg = || {
        args["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .ok_or((
                JSONRPC_INVALID_PARAMS,
                "port must be an integer in 1-65535".to_string(),
            ))
    };
    match name {
        "spawn_session"
        | "session_status"
        | "wait_sessions"
        | "snooze_supervision"
        | "read_terminal"
        | "send_input"
        | "await_reply"
        | "interrupt_session"
        | "resume_session"
        | "kill_session"
        | "list_sessions"
        | "list_instructions"
        | "get_effective_instructions"
        | "set_instructions" => {
            let (supervisor_id, bucket_id) = match daemon.supervisor_session(token) {
                Ok(scope) => scope,
                Err(e) => return Ok(tool_text(true, e.to_string())),
            };
            // Only deliberate actions on a child reset the reminder
            // ladder. Read-only calls like session_status, list_sessions,
            // read_terminal, and spawn_session are routine: a supervisor
            // that checks status to build a report, or spawns a new child,
            // is not attending to an existing stuck one. Resetting on those
            // allowed the supervisor's own reporting to suppress escalation
            // indefinitely.
            if matches!(
                name,
                "wait_sessions"
                    | "send_input"
                    | "interrupt_session"
                    | "resume_session"
                    | "kill_session"
            ) {
                daemon.note_supervision_activity(supervisor_id);
            }
            let session_arg = || {
                args["session"].as_u64().ok_or((
                    JSONRPC_INVALID_PARAMS,
                    "session must be an integer session id".to_string(),
                ))
            };
            return Ok(match name {
                "list_instructions" => {
                    let project_id = if args.get("project").is_some_and(|v| !v.is_null()) {
                        match daemon.resolve_instruction_project(bucket_id, &args["project"]) {
                            Ok(id) => Some(id),
                            Err(e) => return Ok(tool_text(true, e.to_string())),
                        }
                    } else {
                        None
                    };
                    match daemon.list_instructions(bucket_id, project_id) {
                        Ok(layers) => {
                            let values=layers.into_iter().map(|l| { let history=daemon.instruction_history(l.id).unwrap_or_default().into_iter().map(|r|json!({"revision":r.revision,"markdown":r.markdown,"note":r.note,"updated_at_unix_ms":r.updated_at_unix_ms,"updated_by_session_id":r.updated_by_session_id})).collect::<Vec<_>>(); json!({"id":l.id,"scope":if l.project_id.is_some(){"project"}else{"bucket"},"bucket_id":l.bucket_id,"project_id":l.project_id,"role":l.target.as_str(),"markdown":l.markdown,"revision":l.revision,"history":history}) }).collect::<Vec<_>>();
                            tool_text(
                                false,
                                serde_json::to_string(&values).unwrap_or_else(|_| "[]".into()),
                            )
                        }
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                "get_effective_instructions" => {
                    let Some(role) = pm_protocol::domain::SessionRole::parse(
                        args["role"].as_str().unwrap_or_default(),
                    ) else {
                        return Err((
                            JSONRPC_INVALID_PARAMS,
                            "role must be worker or supervisor".into(),
                        ));
                    };
                    let project_id = if args.get("project").is_some_and(|v| !v.is_null()) {
                        Some(
                            daemon
                                .resolve_instruction_project(bucket_id, &args["project"])
                                .map_err(|e| (JSONRPC_INVALID_PARAMS, e.to_string()))?,
                        )
                    } else {
                        None
                    };
                    match daemon.effective_instructions(bucket_id, project_id, role) {
                        Ok(v) => tool_text(false, v),
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                "set_instructions" => {
                    let Some(target) = pm_protocol::domain::InstructionTarget::parse(
                        args["role"].as_str().unwrap_or_default(),
                    ) else {
                        return Err((
                            JSONRPC_INVALID_PARAMS,
                            "role must be all, worker, or supervisor".into(),
                        ));
                    };
                    let scope = args["scope"].as_str().unwrap_or_default();
                    let project_id = match scope {
                        "bucket" => None,
                        "project" => Some(
                            daemon
                                .resolve_instruction_project(bucket_id, &args["project"])
                                .map_err(|e| (JSONRPC_INVALID_PARAMS, e.to_string()))?,
                        ),
                        _ => {
                            return Err((
                                JSONRPC_INVALID_PARAMS,
                                "scope must be bucket or project".into(),
                            ))
                        }
                    };
                    let markdown = args["markdown"].as_str().unwrap_or_default();
                    let expected = args["expected_revision"].as_u64().ok_or((
                        JSONRPC_INVALID_PARAMS,
                        "expected_revision must be a non-negative integer".into(),
                    ))?;
                    match daemon.set_instructions(
                        bucket_id,
                        project_id,
                        target,
                        markdown,
                        expected,
                        args["note"].as_str().unwrap_or_default(),
                        Some(supervisor_id),
                    ) {
                        Ok(l) => tool_text(
                            false,
                            format!("instruction layer {} is now revision {}", l.id, l.revision),
                        ),
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                "spawn_session" => spawn_session_tool(daemon, supervisor_id, bucket_id, args).await,
                "session_status" => {
                    match daemon
                        .supervisor_session_status(supervisor_id, session_arg()?)
                        .await
                    {
                        Ok(status) => tool_text(false, status.to_string()),
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                "snooze_supervision" => {
                    let minutes = match args.get("minutes") {
                        None => crate::supervisor_wake::DEFAULT_SNOOZE_MINUTES,
                        Some(value) => match value.as_u64() {
                            Some(minutes) => minutes,
                            None => {
                                return Ok(tool_text(
                                    true,
                                    "minutes must be an integer in 2-60".into(),
                                ))
                            }
                        },
                    };
                    match daemon.snooze_supervision(supervisor_id, minutes) {
                        Ok(until) => tool_text(false, json!({"snoozed_until_unix_ms": until, "minutes": minutes, "current_completion_silent": true}).to_string()),
                        Err(error) => tool_text(true, error.to_string()),
                    }
                }
                "wait_sessions" => {
                    wait_sessions_tool(daemon, supervisor_id, bucket_id, args).await?
                }
                "read_terminal" => {
                    read_terminal_tool(daemon, supervisor_id, session_arg()?, args).await
                }
                "await_reply" => {
                    let message_id = args["message_id"].as_u64().unwrap_or_default();
                    let seconds = args["wait_seconds"]
                        .as_u64()
                        .unwrap_or(DEFAULT_REVIEW_WAIT_SECONDS)
                        .clamp(1, MAX_HOLD_SECONDS);
                    match daemon
                        .await_agent_reply(
                            supervisor_id,
                            message_id,
                            std::time::Duration::from_secs(seconds),
                        )
                        .await
                    {
                        Ok(message) => match message.reply_body {
                            Some(body) => tool_text(
                                false,
                                format!(
                                    "session {} answered message {}:\n{body}",
                                    message.to_session_id, message.id
                                ),
                            ),
                            None if message.awaits_reply(crate::daemon::now_unix_ms()) => {
                                tool_text(
                                    false,
                                    format!(
                                    "no answer to message {message_id} yet in {seconds}s. It is \
                                     still outstanding, so call await_reply again right now to \
                                     keep waiting."
                                ),
                                )
                            }
                            None => tool_text(
                                false,
                                format!(
                                    "message {message_id} will not be answered: its reply \
                                     window has closed."
                                ),
                            ),
                        },
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                "send_input" => {
                    let session_id = session_arg()?;
                    let text = args["text"].as_str().unwrap_or_default();
                    let submit = args["submit"].as_bool().unwrap_or(false);
                    let reply = args["reply"].as_bool().unwrap_or(false);
                    match daemon
                        .supervisor_send_input(supervisor_id, session_id, text, submit, reply)
                        .await
                    {
                        Ok(outcome) => input_tool_result(outcome),
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                "interrupt_session" => {
                    let session_id = session_arg()?;
                    match daemon.supervisor_interrupt(supervisor_id, session_id) {
                        Ok(()) => tool_text(false, format!("session {session_id} interrupted")),
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                "resume_session" => {
                    let session_id = session_arg()?;
                    match daemon.supervisor_resume(supervisor_id, session_id) {
                        Ok(resumed_id) => tool_text(false, format!("session {resumed_id} resumed")),
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                "kill_session" => {
                    let session_id = session_arg()?;
                    match daemon.supervisor_kill(supervisor_id, session_id) {
                        Ok(()) => tool_text(false, format!("session {session_id} killed")),
                        Err(e) => tool_text(true, e.to_string()),
                    }
                }
                _ => list_sessions_tool(daemon, bucket_id, args),
            });
        }
        "upsert_items"
        | "list_items"
        | "get_items"
        | "attach_item_file"
        | "list_item_attachments"
        | "get_item_attachment"
        | "post_briefing" => {
            let (session_id, bucket_id) = match daemon.items_session(token) {
                Ok(scope) => scope,
                Err(e) => return Ok(tool_text(true, e.to_string())),
            };
            return Ok(match name {
                "upsert_items" => upsert_items(daemon, session_id, bucket_id, args),
                "list_items" => list_items(daemon, bucket_id, args),
                "get_items" => get_items(daemon, bucket_id, args),
                "attach_item_file" => {
                    let item_id = match daemon.resolve_item_ref(bucket_id, &args["item"]) {
                        Ok(id) => id,
                        Err(error) => return Ok(tool_text(true, error.to_string())),
                    };
                    let path = args["path"].as_str().unwrap_or_default();
                    if path.trim().is_empty() {
                        tool_text(true, "path must not be empty".into())
                    } else {
                        match daemon
                            .attach_item_file(
                                session_id,
                                bucket_id,
                                item_id,
                                path,
                                args["filename"].as_str(),
                                args["media_type"].as_str(),
                            )
                            .await
                        {
                            Ok(attachment) => {
                                tool_text(false, attachment_json(&attachment).to_string())
                            }
                            Err(error) => tool_text(true, error.to_string()),
                        }
                    }
                }
                "list_item_attachments" => {
                    let item_id = match daemon.resolve_item_ref(bucket_id, &args["item"]) {
                        Ok(id) => id,
                        Err(error) => return Ok(tool_text(true, error.to_string())),
                    };
                    match daemon.list_item_attachments(bucket_id, item_id) {
                        Ok(attachments) => tool_text(
                            false,
                            json!({
                                "attachments": attachments.iter().map(attachment_json).collect::<Vec<_>>()
                            })
                            .to_string(),
                        ),
                        Err(error) => tool_text(true, error.to_string()),
                    }
                }
                "get_item_attachment" => {
                    let item_id = match daemon.resolve_item_ref(bucket_id, &args["item"]) {
                        Ok(id) => id,
                        Err(error) => return Ok(tool_text(true, error.to_string())),
                    };
                    let Some(attachment_id) = args["attachment_id"].as_u64() else {
                        return Ok(tool_text(
                            true,
                            "attachment_id must be a positive integer".into(),
                        ));
                    };
                    match daemon.get_item_attachment(bucket_id, item_id, attachment_id) {
                        Ok(attachment)
                            if attachment.content.len()
                                <= crate::attachments::MCP_ATTACHMENT_FETCH_MAX =>
                        {
                            attachment_blob_result(attachment)
                        }
                        Ok(_) => tool_text(
                            true,
                            format!(
                                "attachment exceeds the {}-byte agent retrieval limit; use the authenticated Board download",
                                crate::attachments::MCP_ATTACHMENT_FETCH_MAX
                            ),
                        ),
                        Err(error) => tool_text(true, error.to_string()),
                    }
                }
                _ => post_briefing(daemon, session_id, bucket_id, args),
            });
        }
        "publish_port" => {
            let port = port_arg()?;
            return Ok(
                match daemon
                    .publish_port(
                        token,
                        port,
                        &str_arg("slug"),
                        &str_arg("label"),
                        &str_arg("scheme"),
                    )
                    .await
                {
                    Ok(forward) => tool_text(
                        false,
                        format!(
                            "Published. The user-reachable URL is {} — show the user this \
                             URL exactly as given, never a localhost one. The URL requires \
                             the user to be signed in; a login prompt is normal for \
                             unauthenticated visitors, who return to the forward after login, and does not mean the forward is \
                             broken. Use relative asset, API and WebSocket URLs or configure your app base path to this URL path.{}",
                            forward.url,
                            same_origin_caveat(daemon)
                        ),
                    ),
                    Err(e) => tool_text(true, e.to_string()),
                },
            );
        }
        "publish_dir" => {
            return Ok(
                match daemon
                    .publish_dir(token, &str_arg("path"), &str_arg("slug"), &str_arg("label"))
                    .await
                {
                    Ok(forward) => tool_text(
                        false,
                        format!(
                            "Published. The user-reachable URL is {} — show the user this URL \
                             exactly as given, never a localhost one or a filesystem path. The \
                             directory stays served while this session lives, across restarts \
                             of the session and of its host, so do not publish it again. The \
                             URL requires the user to be signed in, and a login prompt is \
                             normal rather than a failure.{}",
                            forward.url,
                            same_origin_caveat(daemon)
                        ),
                    ),
                    Err(e) => tool_text(true, e.to_string()),
                },
            );
        }
        "list_dirs" => {
            return Ok(match daemon.list_dir_shares(token) {
                Ok(shares) if shares.is_empty() => {
                    tool_text(false, "this session publishes no directories".into())
                }
                Ok(shares) => tool_text(
                    false,
                    serde_json::Value::Array(
                        shares
                            .into_iter()
                            .map(|(share, forward)| {
                                serde_json::json!({
                                    "slug": share.slug,
                                    "path": share.path,
                                    "label": share.label,
                                    "url": forward.url,
                                    "serving": !forward.url.is_empty(),
                                })
                            })
                            .collect(),
                    )
                    .to_string(),
                ),
                Err(e) => tool_text(true, e.to_string()),
            });
        }
        "unpublish_dir" => {
            let slug = str_arg("slug");
            return Ok(match daemon.unpublish_dir(token, &slug) {
                Ok(()) => tool_text(
                    false,
                    format!("{slug} is no longer published. Its files were not touched."),
                ),
                Err(e) => tool_text(true, e.to_string()),
            });
        }
        "unpublish_port" => {
            let port = port_arg()?;
            return Ok(match daemon.unpublish_port(token, port) {
                Ok(()) => tool_text(false, format!("port {port} unpublished")),
                Err(e) => tool_text(true, e.to_string()),
            });
        }
        _ => {}
    }

    // Presence distinguishes "leave untouched" (absent) from "clear"
    // (present but empty), so only send bags the agent actually included.
    let opt_fields = |key: &str| args.get(key).map(parse_fields);
    let str_array = |key: &str| {
        args[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|k| k.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };

    if matches!(
        name,
        "open_review" | "next_review_event" | "post_review_reply" | "review_status"
    ) {
        return Ok(review_tool_call(daemon, token, name, args).await);
    }

    if matches!(
        name,
        "upsert_plan"
            | "sync_plan"
            | "present_plan_decision"
            | "present_plan_decision_batch"
            | "resolve_plan_decision"
            | "post_plan_message"
            | "get_plan"
            | "list_plans"
    ) {
        return Ok(plan_tool_call(daemon, token, name, args).await);
    }

    if name == "reply_message" {
        let session_id = match daemon.resolve_live_session(token) {
            Ok(id) => id,
            Err(e) => return Ok(tool_text(true, e.to_string())),
        };
        let message_id = args["message_id"].as_u64().unwrap_or_default();
        let reply_token = args["reply_token"].as_str().unwrap_or_default();
        let body = args["body"].as_str().unwrap_or_default();
        return Ok(
            match daemon
                .reply_to_agent_message(session_id, message_id, reply_token, body)
                .await
            {
                Ok(message) => tool_text(
                    false,
                    format!(
                        "replied to message {} from session {}. The capability is now spent.",
                        message.id, message.from_session_id
                    ),
                ),
                Err(e) => tool_text(true, e.to_string()),
            },
        );
    }

    let report = match name {
        "report" => {
            if let Err(message) = check_report_arguments(args) {
                return Ok(tool_text(true, message));
            }
            AgentReport::Report {
                goal: crate::text::unescape_html_entities(&str_arg("goal")),
                headline: crate::text::unescape_html_entities(&str_arg("headline")),
                summary: args
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .map(crate::text::unescape_html_entities),
                note: crate::text::unescape_html_entities(&str_arg("note")),
                glance: opt_fields("glance"),
                context: opt_fields("context"),
                clear: str_array("clear"),
                git: Box::new(git_update(args.get("git"))),
            }
        }
        "flag_blocked" => {
            if let Err(message) = check_known_keys(args, FLAG_BLOCKED_FIELDS) {
                return Ok(tool_text(true, message));
            }
            let question = crate::text::unescape_html_entities(&str_arg("question"));
            if question.trim().is_empty() {
                return Ok(tool_text(
                    true,
                    "flag_blocked needs a question: say what you need from the user".into(),
                ));
            }
            AgentReport::Blocked { question }
        }
        other => return Err((JSONRPC_INVALID_PARAMS, format!("unknown tool {other:?}"))),
    };

    let blocked_question = match &report {
        AgentReport::Blocked { question } => Some(question.clone()),
        _ => None,
    };
    match daemon.handle_agent_report(token, report) {
        Ok(note) => {
            // A block is a question, and a supervised session has
            // somebody to ask. Putting it to them directly is what turns
            // a state change they have to notice into one they can
            // answer.
            if let Some(question) = blocked_question {
                if let Ok(session_id) = daemon.resolve_live_session(token) {
                    if let Some(message_id) = daemon.ask_supervisor(session_id, &question).await {
                        return Ok(tool_text(
                            false,
                            format!(
                                "recorded, and put to your supervisor as message {message_id}. \
                                 Their answer will reach you here."
                            ),
                        ));
                    }
                }
            }
            Ok(tool_text(false, note.unwrap_or_else(|| "recorded".into())))
        }
        Err(e) => Ok(tool_text(true, e.to_string())),
    }
}

async fn plan_tool_call(daemon: &Arc<Daemon>, token: &str, name: &str, args: &Value) -> Value {
    let session_id = match daemon.resolve_live_session(token) {
        Ok(id) => id,
        Err(error) => return tool_text(true, error.to_string()),
    };
    let plan_id = || args["plan"].as_u64();
    let strings = |key: &str| {
        args[key]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_u64())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let outcome = match name {
        "upsert_plan" => {
            let linked = args.get("linked_items").map(|_| strings("linked_items"));
            let state = match args.get("state").and_then(Value::as_str) {
                Some(value) => match PlanState::parse(value) {
                    Some(state) => Some(state),
                    None => {
                        return tool_text(
                            true,
                            "state must be active, accepted, or archived".into(),
                        )
                    }
                },
                None => None,
            };
            let result = if let Some(plan_id) = plan_id() {
                daemon.update_plan(
                    session_id,
                    plan_id,
                    crate::plan::PlanUpdate {
                        name: args.get("name").and_then(Value::as_str),
                        summary: args.get("summary").and_then(Value::as_str),
                        state,
                        markdown_path: args.get("markdown_path").and_then(Value::as_str),
                        linked_item_ids: linked.as_deref(),
                    },
                )
            } else {
                daemon.create_plan(
                    session_id,
                    args["name"].as_str().unwrap_or_default(),
                    args["summary"].as_str().unwrap_or_default(),
                    args["markdown_path"].as_str().unwrap_or_default(),
                    linked.as_deref().unwrap_or_default(),
                )
            };
            result.map(|plan| crate::plan::plan_json(&plan))
        }
        "sync_plan" => match plan_id() {
            Some(plan_id) => daemon
                .sync_plan(
                    session_id,
                    plan_id,
                    args.get("markdown_path").and_then(Value::as_str),
                )
                .await
                .map(|plan| crate::plan::plan_json(&plan)),
            None => Err(DaemonError::Rejected("plan must be an integer".into())),
        },
        "present_plan_decision" => {
            let Some(plan_id) = plan_id() else {
                return tool_text(true, "plan must be an integer".into());
            };
            let Some(mode) = PlanDecisionMode::parse(args["mode"].as_str().unwrap_or_default())
            else {
                return tool_text(true, "mode must be single, multiple, or dialogue".into());
            };
            let recommended_key = args["recommended_key"].as_str();
            let options = args["options"]
                .as_array()
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            let key = option["key"].as_str().unwrap_or_default().to_string();
                            let is_rec = option["recommended"]
                                .as_bool()
                                .or_else(|| option["is_recommended"].as_bool())
                                .unwrap_or_else(|| recommended_key == Some(key.as_str()));
                            crate::plan_store::PlanOptionDraft {
                                key,
                                label: option["label"].as_str().unwrap_or_default().to_string(),
                                detail_markdown: option["detail_markdown"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                recommended: is_rec,
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            daemon
                .present_plan_decision(
                    session_id,
                    plan_id,
                    args["key"].as_str().unwrap_or_default(),
                    args["title"].as_str().unwrap_or_default(),
                    args["prompt_markdown"].as_str().unwrap_or_default(),
                    args["detail_markdown"].as_str().unwrap_or_default(),
                    mode,
                    args["allow_custom"].as_bool().unwrap_or(false),
                    args["require_selection"].as_bool().unwrap_or(true),
                    &options,
                )
                .map(|(plan, decision)| serde_json::json!({"plan": crate::plan::plan_json(&plan), "decision": decision}))
        }
        "present_plan_decision_batch" => {
            let Some(plan_id) = plan_id() else {
                return tool_text(true, "plan must be an integer".into());
            };
            let mut decisions = Vec::new();
            for decision in args["decisions"].as_array().cloned().unwrap_or_default() {
                let Some(mode) =
                    PlanDecisionMode::parse(decision["mode"].as_str().unwrap_or_default())
                else {
                    return tool_text(true, "mode must be single or multiple".into());
                };
                if mode == PlanDecisionMode::Dialogue {
                    return tool_text(true, "batched decisions cannot use dialogue mode".into());
                }
                let recommended_key = decision["recommended_key"].as_str();
                let options = decision["options"]
                    .as_array()
                    .map(|options| {
                        options
                            .iter()
                            .map(|option| {
                                let key = option["key"].as_str().unwrap_or_default().to_string();
                                let is_rec = option["recommended"]
                                    .as_bool()
                                    .or_else(|| option["is_recommended"].as_bool())
                                    .unwrap_or_else(|| recommended_key == Some(key.as_str()));
                                crate::plan_store::PlanOptionDraft {
                                    key,
                                    label: option["label"].as_str().unwrap_or_default().to_string(),
                                    detail_markdown: option["detail_markdown"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_string(),
                                    recommended: is_rec,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                decisions.push(crate::plan::PlanDecisionPresentation {
                    key: decision["key"].as_str().unwrap_or_default().to_string(),
                    title: decision["title"].as_str().unwrap_or_default().to_string(),
                    prompt_markdown: decision["prompt_markdown"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    detail_markdown: decision["detail_markdown"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    mode,
                    allow_custom: decision["allow_custom"].as_bool().unwrap_or(false),
                    require_selection: decision["require_selection"].as_bool().unwrap_or(true),
                    options,
                });
            }
            daemon
                .present_plan_decision_batch(
                    session_id,
                    plan_id,
                    args["batch_key"].as_str().unwrap_or_default(),
                    &decisions,
                )
                .map(|(plan, decisions)| serde_json::json!({"plan": crate::plan::plan_json(&plan), "decisions": decisions}))
        }
        "resolve_plan_decision" => match plan_id() {
            Some(plan_id) => daemon
                .resolve_plan_decision(
                    session_id,
                    plan_id,
                    args["key"].as_str().unwrap_or_default(),
                    args["resolution_markdown"].as_str().unwrap_or_default(),
                )
                .map(|plan| crate::plan::plan_json(&plan)),
            None => Err(DaemonError::Rejected("plan must be an integer".into())),
        },
        "post_plan_message" => match plan_id() {
            Some(plan_id) => daemon
                .post_plan_agent_message(
                    session_id,
                    plan_id,
                    args["decision"].as_u64(),
                    args["body"].as_str().unwrap_or_default(),
                )
                .map(|message| serde_json::json!(message)),
            None => Err(DaemonError::Rejected("plan must be an integer".into())),
        },
        "get_plan" => match plan_id() {
            Some(plan_id) => daemon
                .owned_plan(session_id, plan_id)
                .and_then(|_| daemon.plan_detail(plan_id))
                .map(|detail| serde_json::json!(detail)),
            None => Err(DaemonError::Rejected("plan must be an integer".into())),
        },
        "list_plans" => {
            let project_id = match daemon.storage().get_session(session_id) {
                Ok(session) => session.project_id,
                Err(error) => return tool_text(true, error.to_string()),
            };
            daemon
                .storage()
                .list_plans(Some(project_id), args["include_archived"].as_bool().unwrap_or(false))
                .map(|plans| serde_json::json!({"plans": plans.iter().map(crate::plan::plan_json).collect::<Vec<_>>() }))
                .map_err(Into::into)
        }
        _ => unreachable!(),
    };
    match outcome {
        Ok(value) => tool_text(false, value.to_string()),
        Err(error) => tool_text(true, error.to_string()),
    }
}

/// The review tools all resolve the calling session first, so a token
/// that is no longer live gets one clear error rather than four.
async fn review_tool_call(daemon: &Arc<Daemon>, token: &str, name: &str, args: &Value) -> Value {
    let session_id = match daemon.resolve_live_session(token) {
        Ok(id) => id,
        Err(e) => return tool_text(true, e.to_string()),
    };
    let strings = |key: &str| -> Vec<String> {
        args[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    match name {
        "open_review" => {
            let files = strings("files");
            let ctx = crate::review::ReviewContext {
                worktree: args["worktree"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                base: args["base"].as_str().unwrap_or_default().trim().to_string(),
                head: args["head"].as_str().unwrap_or_default().trim().to_string(),
                pathspec: strings("pathspec"),
                files: (!files.is_empty()).then_some(files),
                source_file: args["source_file"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                label: args["label"].as_str().unwrap_or_default().to_string(),
            };
            // Catching a ref here, rather than resolving it, is the
            // point: a review anchored to a moving name would quietly
            // change what it covers.
            if let Some(problem) = frozen_head(&ctx).or_else(|| unresolved_ref(&ctx)) {
                return tool_text(true, problem);
            }
            let reset = args["reset"].as_bool().unwrap_or(false);
            match daemon.open_review(session_id, &ctx, reset).await {
                Ok(review) => tool_text(
                    false,
                    format!(
                        "review {} open on {}\nGive the user this URL: {}\nTake comments with \
                         next_review_event.",
                        review.id,
                        review.label,
                        daemon.review_url(review.id)
                    ),
                ),
                Err(e) => tool_text(true, e.to_string()),
            }
        }
        "next_review_event" => {
            let seconds = args["wait_seconds"]
                .as_u64()
                .unwrap_or(DEFAULT_REVIEW_WAIT_SECONDS)
                .clamp(1, MAX_REVIEW_WAIT_SECONDS);
            let wait = std::time::Duration::from_secs(seconds);
            match daemon.await_review_event(session_id, wait).await {
                Ok(crate::review::ReviewWait::Event(event)) => {
                    tool_text(false, render_review_event(&event))
                }
                Ok(crate::review::ReviewWait::Finished) => tool_text(
                    false,
                    "the review is finished. Nothing more is coming; carry on with your work."
                        .into(),
                ),
                Ok(crate::review::ReviewWait::TimedOut) => tool_text(
                    false,
                    format!(
                        "nothing arrived in {seconds}s. The review is still open, so call \
                         next_review_event again right now to keep waiting."
                    ),
                ),
                Err(e) => tool_text(true, e.to_string()),
            }
        }
        "post_review_reply" => {
            let thread_id = args["thread_id"].as_u64().unwrap_or_default();
            let body = args["body"].as_str().unwrap_or_default();
            let addressed = args["addressed"].as_bool().unwrap_or(true);
            match daemon
                .post_review_reply(session_id, thread_id, body, addressed)
                .await
            {
                Ok(review) => tool_text(
                    false,
                    format!(
                        "replied. {} open, {} answered, {} resolved.",
                        review.open_count, review.answered_count, review.resolved_count
                    ),
                ),
                Err(e) => tool_text(true, e.to_string()),
            }
        }
        "review_status" => match daemon.review_status(session_id) {
            Ok(reviews) if reviews.is_empty() => {
                tool_text(false, "no open reviews for this session.".into())
            }
            Ok(reviews) => {
                let lines: Vec<String> = reviews
                    .iter()
                    .map(|r| {
                        format!(
                            "review {} on {} — {} open, {} answering, {} answered, {} resolved",
                            r.id,
                            r.label,
                            r.open_count,
                            r.draft_count,
                            r.answered_count,
                            r.resolved_count
                        )
                    })
                    .collect();
                tool_text(false, lines.join("\n"))
            }
            Err(e) => tool_text(true, e.to_string()),
        },
        other => tool_text(true, format!("unknown tool {other:?}")),
    }
}

/// A base that is plainly a ref rather than a SHA, with the command
/// that turns it into one.
fn unresolved_ref(ctx: &crate::review::ReviewContext) -> Option<String> {
    let value = &ctx.base;
    let looks_like_sha = value.len() >= 7 && value.chars().all(|c| c.is_ascii_hexdigit());
    if value.is_empty() || looks_like_sha {
        return None;
    }
    Some(format!(
        "`base` must be a resolved SHA, not {value:?}. Run `git -C {} rev-parse {value}` and \
         pass the result, so the review keeps meaning what it means now even after the ref \
         moves.",
        ctx.worktree
    ))
}

/// A head SHA freezes what the review reads, and this surface exists
/// for an agent to answer comments by editing the tree. Those edits
/// would land outside the review, every reply would capture the same
/// frozen bytes, and no revision could ever be recorded.
fn frozen_head(ctx: &crate::review::ReviewContext) -> Option<String> {
    if ctx.head.is_empty() {
        return None;
    }
    Some(format!(
        "`head` must be empty here. This review reads the working tree so the edits you make \
         answering a comment appear in it; pinning head to {:?} freezes what the review reads \
         and your edits would never show up. A person opens a read-only review of an already \
         landed range with `pm review open --head`.",
        ctx.head
    ))
}

/// What the agent reads for one comment. The current line and excerpt
/// come first because they describe today's code; the original hunk is
/// context for what the reviewer was looking at.
/// The reviewer answered a marked option list. Written out as named
/// fields so the agent reads the decision off the event instead of
/// recovering it from a sentence, which is the whole reason the answer
/// travels as data.
fn render_choice_answer(c: &pm_protocol::domain::ReviewChoiceAnswer) -> String {
    let mut out = format!("answer to choice \"{}\"\n", c.choice_id);
    out.push_str(match c.select {
        pm_protocol::domain::ReviewChoiceSelect::Many => "  select: many\n",
        pm_protocol::domain::ReviewChoiceSelect::One => "  select: one\n",
    });
    if c.option_ids.is_empty() {
        out.push_str("  chose: nothing\n");
    }
    for (n, id) in c.option_ids.iter().enumerate() {
        let label = c.option_labels.get(n).map(String::as_str).unwrap_or("");
        out.push_str(&format!("  chose: {id} ({label})\n"));
    }
    if !c.other_text.is_empty() {
        out.push_str(&format!("  other: {}\n", c.other_text));
    }
    if !c.notes.is_empty() {
        out.push_str(&format!("  notes: {}\n", c.notes));
    }
    out
}

fn render_review_event(event: &crate::review::ReviewEvent) -> String {
    let t = &event.thread;
    let mut out = String::new();
    out.push_str(&format!(
        "thread {} on {}:{}\n",
        t.id, t.path, t.current_line
    ));
    match t.anchor_status {
        pm_protocol::domain::ReviewAnchorStatus::Moved => out.push_str(&format!(
            "the commented line moved: written against line {}, now at {}\n",
            t.line, t.current_line
        )),
        pm_protocol::domain::ReviewAnchorStatus::Changed => out.push_str(&format!(
            "warning: the commented line itself changed since the comment was written              (was line {})\n",
            t.line
        )),
        _ => {}
    }
    for m in &t.messages {
        let who = match m.author {
            pm_protocol::domain::ReviewAuthor::Session => "you",
            _ => "reviewer",
        };
        out.push_str(&format!("\n{who}: {}\n", m.body));
        if let Some(c) = &m.choice {
            out.push_str(&render_choice_answer(c));
        }
    }
    if !t.current_excerpt.is_empty() {
        out.push_str("\ncurrent code:\n");
        out.push_str(&t.current_excerpt);
        out.push('\n');
    }
    out.push_str(&format!(
        "\nEdit {} in {} to address this, then call post_review_reply with thread_id {}.",
        t.path, event.review.worktree, t.id
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::{call_label, parse_time, render_choice_answer, strip_ansi};
    use pm_protocol::domain::{ReviewChoiceAnswer, ReviewChoiceSelect};
    use serde_json::json;

    // The agent has to read the decision off the event. Every field the
    // reader filled in has to be there, named, and none of it recovered
    // from a sentence.
    #[test]
    fn a_choice_answer_renders_as_named_fields() {
        let out = render_choice_answer(&ReviewChoiceAnswer {
            choice_id: "auth-approach".into(),
            select: ReviewChoiceSelect::One,
            option_ids: vec!["server-side-sessions".into()],
            option_labels: vec!["Server-side sessions".into()],
            other_text: String::new(),
            notes: "sticky routing is fine".into(),
        });

        assert_eq!(
            out,
            "answer to choice \"auth-approach\"\n  select: one\n  \
             chose: server-side-sessions (Server-side sessions)\n  \
             notes: sticky routing is fine\n"
        );
    }

    #[test]
    fn several_choices_and_free_text_all_reach_the_agent() {
        let out = render_choice_answer(&ReviewChoiceAnswer {
            choice_id: "transport".into(),
            select: ReviewChoiceSelect::Many,
            option_ids: vec!["grpc".into(), "other".into()],
            option_labels: vec!["gRPC".into(), "Other".into()],
            other_text: "raw GRE".into(),
            notes: String::new(),
        });

        assert!(out.contains("  select: many\n"));
        assert!(out.contains("  chose: grpc (gRPC)\n"));
        assert!(out.contains("  chose: other (Other)\n"));
        assert!(out.contains("  other: raw GRE\n"));
        assert!(!out.contains("notes:"));
    }

    #[test]
    fn parse_time_preserves_rfc3339_instants() {
        let utc = parse_time("due", &json!("2026-07-18T21:59:00Z")).unwrap();
        let offset = parse_time("due", &json!("2026-07-18T23:59:00+02:00")).unwrap();

        assert_eq!(utc, Some(1_784_411_940_000));
        assert_eq!(offset, utc);
    }

    #[test]
    fn parse_time_rejects_a_timestamp_without_timezone() {
        let error = parse_time("due", &json!("2026-07-18T23:59:00")).unwrap_err();

        assert!(error.contains("Z or a UTC offset"), "{error}");
    }

    #[test]
    fn strip_ansi_removes_csi_osc_and_charset_sequences() {
        let input = "\u{1b}[1;32mgreen\u{1b}[0m \u{1b}]0;title\u{7}plain \u{1b}(Btext\r\n";
        assert_eq!(strip_ansi(input), "green plain text\n");
    }

    #[test]
    fn strip_ansi_keeps_tabs_and_newlines() {
        assert_eq!(strip_ansi("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn strip_ansi_survives_a_truncated_escape() {
        assert_eq!(strip_ansi("done\u{1b}["), "done");
        assert_eq!(strip_ansi("done\u{1b}"), "done");
    }

    #[test]
    fn strip_ansi_handles_osc_with_st_terminator() {
        assert_eq!(
            strip_ansi("\u{1b}]8;;x\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
    }

    // A slow call is only worth reporting if the line names the tool
    // that was slow. "tools/call" is every tool at once.
    #[test]
    fn a_tool_call_is_logged_under_its_tool_name() {
        assert_eq!(
            call_label(&json!({
                "method": "tools/call",
                "params": { "name": "next_review_event", "arguments": {} }
            })),
            "next_review_event"
        );
    }

    #[test]
    fn other_methods_are_logged_under_the_method() {
        assert_eq!(call_label(&json!({ "method": "tools/list" })), "tools/list");
        assert_eq!(call_label(&json!({ "method": "initialize" })), "initialize");
    }

    #[test]
    fn a_call_naming_no_tool_still_has_a_label() {
        assert_eq!(call_label(&json!({ "method": "tools/call" })), "tools/call");
        assert_eq!(call_label(&json!({})), "");
    }
}
