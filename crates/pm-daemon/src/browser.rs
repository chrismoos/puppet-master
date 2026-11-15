use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const CALL_TIMEOUT: Duration = Duration::from_secs(65);
const WORKER_REPLY_GRACE: Duration = Duration::from_secs(5);
const STREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const VIEWER_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const RUNTIME_VERSION: &str = "browser-runtime-0.0.85-1";
const FILES: [(&str, &str); 4] = [
    (
        "package.json",
        include_str!("../browser-runtime/package.json"),
    ),
    (
        "package-lock.json",
        include_str!("../browser-runtime/package-lock.json"),
    ),
    (
        "service.mjs",
        include_str!("../browser-runtime/service.mjs"),
    ),
    ("tools.json", include_str!("../browser-runtime/tools.json")),
];

type ProcessSlot = Arc<tokio::sync::Mutex<Option<BrowserProcess>>>;

#[derive(Default)]
pub struct BrowserRuntime {
    sessions: Mutex<HashMap<u64, ProcessSlot>>,
}

struct BrowserProcess {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

pub fn runtime_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PM_BROWSER_RUNTIME_DIR") {
        return Ok(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("cannot locate the browser runtime directory")?;
    Ok(base.join("puppet-master").join(RUNTIME_VERSION))
}

fn write_runtime() -> Result<PathBuf, String> {
    let dir = runtime_dir()?;
    crate::fsperm::create_private_dir(&dir).map_err(|e| e.to_string())?;
    for (name, contents) in FILES {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(contents) {
            let temporary = dir.join(format!("{name}.{}.tmp", rand::random::<u64>()));
            std::fs::write(&temporary, contents).map_err(|e| e.to_string())?;
            std::fs::rename(temporary, path).map_err(|e| e.to_string())?;
        }
    }
    Ok(dir)
}

pub async fn install() -> Result<(), String> {
    let dir = write_runtime()?;
    let status = Command::new("npm")
        .args(["ci", "--ignore-scripts", "--no-fund", "--no-audit"])
        .current_dir(&dir)
        .status()
        .await
        .map_err(|e| format!("Node.js and npm are required: {e}"))?;
    if !status.success() {
        return Err("browser runtime dependency installation failed".into());
    }
    let status = Command::new("node")
        .args(["node_modules/playwright/cli.js", "install", "chromium"])
        .env("PLAYWRIGHT_SKIP_BROWSER_GC", "1")
        .current_dir(&dir)
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Chromium installation failed".into());
    }
    Ok(())
}

impl BrowserProcess {
    fn spawn() -> Result<Self, String> {
        let dir = write_runtime()?;
        if !dir.join("node_modules/@playwright/mcp/index.js").is_file() {
            return Err("managed browsing is not installed on this host. Run `pm browser install` on the worker first".into());
        }
        let mut child = Command::new("node").arg(dir.join("service.mjs"))
            .current_dir(&dir).stdin(Stdio::piped()).stdout(Stdio::piped())
            .stderr(Stdio::inherit()).kill_on_drop(true).spawn()
            .map_err(|e| format!("could not start the browser runtime. Install Node.js and run `pm browser install`: {e}"))?;
        let input = child.stdin.take().ok_or("browser runtime has no input")?;
        let output = BufReader::new(child.stdout.take().ok_or("browser runtime has no output")?);
        Ok(Self {
            child,
            input,
            output,
        })
    }

    async fn call(&mut self, session_id: u64, tool: &str, args: Value) -> Result<Value, String> {
        let mut request = serde_json::to_vec(
            &json!({ "id": 1, "session_id": session_id, "tool": tool, "args": args }),
        )
        .map_err(|e| e.to_string())?;
        request.push(b'\n');
        self.input
            .write_all(&request)
            .await
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        let size = tokio::io::AsyncReadExt::take(&mut self.output, (MAX_RESPONSE_BYTES + 1) as u64)
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?;
        if size == 0 {
            return Err(
                "browser runtime exited. Run `pm browser install` if Chromium is missing".into(),
            );
        }
        if size > MAX_RESPONSE_BYTES {
            let _ = self.child.kill().await;
            return Err("browser response is too large".into());
        }
        let response: Value =
            serde_json::from_str(&line).map_err(|e| format!("invalid browser response: {e}"))?;
        if let Some(error) = response["error"].as_str() {
            return Err(error.to_string());
        }
        Ok(response["result"].clone())
    }
}

impl BrowserRuntime {
    pub async fn call(&self, session_id: u64, tool: &str, args: Value) -> Result<Value, String> {
        let slot = {
            let mut sessions = self.sessions.lock().unwrap();
            if tool == "__close_session" {
                match sessions.remove(&session_id) {
                    Some(slot) => slot,
                    None => return Ok(json!({})),
                }
            } else {
                sessions.entry(session_id).or_default().clone()
            }
        };
        let mut process = slot.lock().await;
        if tool == "__close_session" && process.is_none() {
            return Ok(json!({}));
        }
        if process
            .as_mut()
            .is_some_and(|p| p.child.try_wait().ok().flatten().is_some())
        {
            *process = None;
        }
        if process.is_none() {
            *process = Some(BrowserProcess::spawn()?);
        }
        let result = tokio::time::timeout(
            CALL_TIMEOUT,
            process.as_mut().unwrap().call(session_id, tool, args),
        )
        .await;
        if tool == "__close_session" {
            if let Some(mut child) = process.take() {
                let _ = child.child.kill().await;
            }
        }
        match result {
            Ok(value) => value,
            Err(_) => {
                if let Some(mut child) = process.take() {
                    let _ = child.child.kill().await;
                }
                Err("browser operation timed out and its runtime was stopped. Its outcome is unknown; inspect the page before retrying a write".into())
            }
        }
    }
}

pub fn tool_definitions() -> Vec<Value> {
    let mut tools: Vec<Value> = serde_json::from_str(include_str!("../browser-runtime/tools.json"))
        .expect("browser tool schemas");
    tools.push(json!({
        "name":"browser_contexts",
        "description":"List, create, or close isolated browser contexts in your session. Reuse default unless separate cookies or a different logged-in user are required. Contexts share one Chromium process, have separate cookies and storage, and are temporary. At most four including default. Creating another requires a reason. Closing default resets its state on next use.",
        "inputSchema":{"type":"object","properties":{
            "action":{"type":"string","enum":["list","create","close"]},
            "browser_context":{"type":"string","description":"Context name, default is default."},
            "reason":{"type":"string","description":"Required for create. Why separate cookies or logins are needed."}
        },"required":["action"]}
    }));
    tools.push(json!({
        "name":"show_browser",
        "description":"Show your managed browser live in a read-only native pane beside you when the user asks to watch browsing or testing. Reuses your browser and follows its active tab. Hide or close the pane with normal pane tools. Hiding stops streaming, not automation. Browser tools use a fixed 1280 by 800 viewport by default. Use browser_resize to test other layouts. Browsers expire after fifteen minutes without tool activity and logins are temporary.",
        "inputSchema":{"type":"object","properties":{
            "browser_context":{"type":"string","description":"Existing context to show, default is default."},
            "title":{"type":"string","description":"Short pane title."}
        }}
    }));
    tools
}

pub fn is_tool(name: &str) -> bool {
    tool_definitions().iter().any(|tool| tool["name"] == name)
}

pub async fn call_tool(
    daemon: &Arc<crate::Daemon>,
    token: &str,
    tool: &str,
    args: &Value,
) -> Result<Value, String> {
    let session_id = daemon
        .resolve_live_session(token)
        .map_err(|e| e.to_string())?;
    let session = daemon
        .storage()
        .get_session(session_id)
        .map_err(|e| e.to_string())?;
    if tool == "show_browser" {
        let view = daemon
            .browser_call(session.worker_id, session_id, "__view", args.clone())
            .await?;
        let context = view["context"]
            .as_str()
            .ok_or("browser returned no context")?;
        let port = view["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .ok_or("browser returned no port")?;
        let path = view["path"]
            .as_str()
            .ok_or("browser returned no stream path")?;
        let pane = crate::panes::show_browser(
            daemon,
            session_id,
            context,
            port,
            path,
            args["title"].as_str(),
        )
        .await?;
        return Ok(json!({"content":[{"type":"text","text":pane.to_string()}]}));
    }
    daemon
        .browser_call(session.worker_id, session_id, tool, args.clone())
        .await
}

impl crate::Daemon {
    pub(crate) async fn browser_call(
        &self,
        worker_id: u64,
        session_id: u64,
        tool: &str,
        args: Value,
    ) -> Result<Value, String> {
        if worker_id == pm_protocol::domain::LOCAL_WORKER_ID {
            return self.browsers.call(session_id, tool, args).await;
        }
        let link = self.worker_link(worker_id).map_err(|e| e.to_string())?;
        if link.protocol_version() < pm_protocol::WORKER_PROTOCOL_MANAGED_BROWSER {
            return Err(
                "this worker cannot manage browsers. Update the worker before using browser tools"
                    .into(),
            );
        }
        if tool != "__close_session" {
            self.browser_sessions.lock().unwrap().insert(session_id);
        }
        let rx = link
            .request_browser(session_id, tool.to_string(), args.to_string())
            .map_err(|e| e.to_string())?;
        let result = tokio::time::timeout(CALL_TIMEOUT + WORKER_REPLY_GRACE, rx)
            .await
            .map_err(|_| "worker browser operation timed out; its outcome is unknown".to_string())?
            .map_err(|_| "worker browser connection closed".to_string())??;
        serde_json::from_str(&result).map_err(|e| e.to_string())
    }

    pub(crate) fn close_session_browser(self: &Arc<Self>, worker_id: u64, session_id: u64) {
        if worker_id != pm_protocol::domain::LOCAL_WORKER_ID
            && !self.browser_sessions.lock().unwrap().remove(&session_id)
        {
            return;
        }
        let daemon = self.clone();
        tokio::spawn(async move {
            if let Err(error) = daemon
                .browser_call(worker_id, session_id, "__close_session", json!({}))
                .await
            {
                tracing::debug!(session_id, %error, "could not close session browser");
            }
        });
    }
}

pub(crate) async fn serve_stream(
    daemon: Arc<crate::Daemon>,
    session_id: u64,
    port: u16,
    stream: String,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::extract::{
        ws::{Message, WebSocketUpgrade},
        FromRequestParts,
    };
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as UpstreamMessage;

    let Ok(session) = daemon.storage().get_session(session_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (mut parts, _) = request.into_parts();
    let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &daemon).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let tunnel =
        match crate::forward_route::open_worker_port(&daemon, session.worker_id, port).await {
            Ok(tunnel) => tunnel,
            Err(error) => return (StatusCode::BAD_GATEWAY, error).into_response(),
        };
    let handshake = tokio::time::timeout(
        STREAM_CONNECT_TIMEOUT,
        tokio_tungstenite::client_async(format!("ws://127.0.0.1/{stream}"), tunnel),
    )
    .await;
    let upstream = match handshake {
        Ok(Ok((upstream, _))) => upstream,
        _ => {
            return (
                StatusCode::BAD_GATEWAY,
                "Browser stream is unavailable. Ask the agent to show its browser again.",
            )
                .into_response()
        }
    };
    upgrade.max_message_size(MAX_RESPONSE_BYTES).on_upgrade(move |mut socket| async move {
        let (_write, mut read) = upstream.split();
        loop {
            tokio::select! {
                event = read.next() => {
                    let message = match event {
                        Some(Ok(UpstreamMessage::Binary(data))) => Message::Binary(data),
                        Some(Ok(UpstreamMessage::Text(data))) => Message::Text(data.as_str().into()),
                        Some(Ok(UpstreamMessage::Close(_))) | None | Some(Err(_)) => break,
                        _ => continue,
                    };
                    if !matches!(tokio::time::timeout(VIEWER_SEND_TIMEOUT, socket.send(message)).await, Ok(Ok(()))) { break; }
                }
                incoming = socket.recv() => {
                    if matches!(incoming, None | Some(Err(_)) | Some(Ok(Message::Close(_)))) { break; }
                }
            }
        }
        let _ = socket.close().await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_tools_include_page_evaluation_but_no_server_execution() {
        assert!(is_tool("browser_evaluate"));
        assert!(is_tool("browser_navigate"));
        assert!(is_tool("show_browser"));
        assert!(!is_tool("browser_run_code"));
        assert!(!is_tool("browser_run_code_unsafe"));
        let tools = tool_definitions();
        let tabs = tools.iter().find(|t| t["name"] == "browser_tabs").unwrap();
        assert!(tabs["inputSchema"]["properties"]
            .get("isolatedContext")
            .is_none());
    }
}
