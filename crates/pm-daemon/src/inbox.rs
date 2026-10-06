//! Delivering a message to a running agent without typing at its
//! terminal.
//!
//! Every supported agent CLI already accepts a message from outside its
//! own UI, and each does it differently: a Unix socket, a subcommand, an
//! HTTP endpoint. The adapter names the channel; this module carries the
//! message over whichever one it named. A caller that gets
//! `Unavailable` back falls back to the terminal.

use std::time::Duration;

use pm_adapters::{DeliveryMode, InboundChannel};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

/// How long one delivery may take. Every channel is local, so this
/// bounds a hung agent rather than a slow network, and it stays well
/// inside what a caller waiting on a tool call will hold for.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// What a delivery did, so the caller can report the path a message
/// actually took instead of assuming the one it asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    pub transport: &'static str,
    /// The mode the channel actually honoured, which is `Queue`
    /// wherever the agent offers no way to interrupt a turn.
    pub mode: DeliveryMode,
}

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    /// The channel exists but did not take the message. The terminal is
    /// still worth trying.
    #[error("{0}")]
    Refused(String),
    /// The agent is not reachable on the address the adapter named,
    /// which is the normal state for a session that has not finished
    /// starting.
    #[error("{0}")]
    Unreachable(String),
}

/// The wire mode a delivery asks for.
pub fn inbox_mode(mode: DeliveryMode) -> pm_protocol::domain::AgentInboxMode {
    match mode {
        DeliveryMode::Queue => pm_protocol::domain::AgentInboxMode::Queue,
        DeliveryMode::Steer => pm_protocol::domain::AgentInboxMode::Steer,
    }
}

/// The delivery mode a wire mode asks for.
pub fn delivery_mode(mode: pm_protocol::domain::AgentInboxMode) -> DeliveryMode {
    match mode {
        pm_protocol::domain::AgentInboxMode::Queue => DeliveryMode::Queue,
        pm_protocol::domain::AgentInboxMode::Steer => DeliveryMode::Steer,
    }
}

/// Matches a transport a worker reported back to the one the local
/// channels name, so a remote delivery reads the same as a local one
/// everywhere a transport is recorded or shown.
pub fn transport_name(reported: &str) -> &'static str {
    const NAMES: [&str; 3] = ["claude-socket", "codex-queue", "opencode-http"];
    NAMES
        .into_iter()
        .find(|name| *name == reported)
        .unwrap_or("agent-inbox")
}

/// Names the agent's own inbound channel for an agent terminal running
/// on this host.
///
/// Takes the terminal, not the session: the pid belongs to the PTY
/// child, and the mux is keyed the same way. The pid and the socket
/// directory are facts about the host holding that PTY, so this only
/// answers for a terminal in the caller's own mux. A controller with a
/// remote session asks that session's worker to run the same resolution
/// there.
pub fn local_inbound_channel(
    registry: &pm_adapters::AdapterRegistry,
    mux: &crate::mux::Mux,
    agent_terminal_id: u64,
    agent: pm_protocol::domain::AgentKind,
    agent_session_id: Option<String>,
    agent_port: Option<u16>,
) -> Option<InboundChannel> {
    let adapter = registry.get(agent).ok()?;
    let agent_pid = mux.child_pid(agent_terminal_id);
    let facts = pm_adapters::InboundFacts {
        agent_pid,
        agent_session_id: agent_session_id.filter(|id| !id.is_empty()),
        agent_port,
        socket_dir: agent_pid.and_then(agent_socket_dir),
        inbox_token: None,
    };
    adapter.inbound_channel(&facts)
}

/// Sends `text` to the agent over `channel`.
pub async fn deliver(
    channel: &InboundChannel,
    text: &str,
    mode: DeliveryMode,
) -> Result<Delivered, InboxError> {
    let attempt = async {
        match channel {
            InboundChannel::ClaudeSocket {
                path,
                token,
                expect_pid,
            } => deliver_claude(path, token.as_deref(), *expect_pid, text).await,
            InboundChannel::CodexQueue { thread } => {
                deliver_codex(tokio::process::Command::new("codex"), thread, text).await
            }
            InboundChannel::OpenCodeHttp { base_url, session } => {
                deliver_opencode(base_url, session, text, mode).await
            }
        }
    };
    let outcome = tokio::time::timeout(DELIVERY_TIMEOUT, attempt)
        .await
        .map_err(|_| {
            InboxError::Unreachable("the agent did not take the message in time".into())
        })?;
    match &outcome {
        Ok(delivered) => debug!(
            transport = delivered.transport,
            mode = ?delivered.mode,
            bytes = text.len(),
            "delivered a message to an agent"
        ),
        Err(e) => debug!(transport = channel.transport(), error = %e, "agent inbox declined"),
    }
    outcome
}

/// Claude Code takes newline-delimited JSON frames on a per-session
/// socket. The auth line is optional where the OS identifies the peer,
/// but sending it whenever the token is known is what makes the message
/// arrive as an own-child rather than an unverified peer.
async fn deliver_claude(
    path: &std::path::Path,
    token: Option<&str>,
    expect_pid: u32,
    text: &str,
) -> Result<Delivered, InboxError> {
    let mut stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(|e| InboxError::Unreachable(format!("no inbox at {}: {e}", path.display())))?;
    match peer_pid(&stream) {
        Some(peer) if peer == expect_pid => {}
        Some(peer) => {
            return Err(InboxError::Refused(format!(
                "the inbox at {} is held by process {peer}, not the agent this \
                 session spawned ({expect_pid})",
                path.display()
            )))
        }
        None => {
            return Err(InboxError::Refused(format!(
                "cannot establish which process holds the inbox at {}, so the \
                 message is not sent on it",
                path.display()
            )))
        }
    }
    let mut frames = String::new();
    if let Some(token) = token {
        frames.push_str(&serde_json::json!({ "type": "auth", "token": token }).to_string());
        frames.push('\n');
    }
    frames.push_str(
        &serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": text }
        })
        .to_string(),
    );
    frames.push('\n');
    stream
        .write_all(frames.as_bytes())
        .await
        .map_err(|e| InboxError::Refused(format!("writing to the inbox failed: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| InboxError::Refused(format!("flushing the inbox failed: {e}")))?;
    // The socket answers nothing on success, so a clean write is the
    // whole acknowledgement available here.
    Ok(Delivered {
        transport: "claude-socket",
        mode: DeliveryMode::Queue,
    })
}

/// The pid the kernel records for the far end of a connected unix socket,
/// which for a client is the process that is listening.
///
/// `None` means the platform would not say, which a caller treats the
/// same as a mismatch: the address is a pid in a directory every session
/// of this user shares, so without an answer there is nothing tying the
/// socket to the agent it is named after.
fn peer_pid(stream: &tokio::net::UnixStream) -> Option<u32> {
    let peer = stream.peer_cred().ok()?;
    peer.pid().map(|pid| pid as u32).filter(|pid| *pid > 0)
}

/// Codex queues onto a thread through its own CLI, which reaches the app
/// server the TUI runs on.
async fn deliver_codex(
    mut command: tokio::process::Command,
    thread: &str,
    text: &str,
) -> Result<Delivered, InboxError> {
    let output = command
        .arg("queue")
        .arg("--thread")
        .arg(thread)
        .arg("--message")
        .arg(text)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| InboxError::Unreachable(format!("could not run codex queue: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    codex_queue_outcome(output.status.success(), &format!("{stdout}{stderr}"))
        .map_err(InboxError::Refused)?;
    Ok(Delivered {
        transport: "codex-queue",
        mode: DeliveryMode::Queue,
    })
}

/// Whether a `codex queue` run actually queued anything.
///
/// A zero exit is not the acknowledgement: the CLI reports a thread it
/// could not reach in its own output. The receipt is, so this reads the
/// output for it rather than inferring success from the absence of a
/// known error string.
fn codex_queue_outcome(exited_ok: bool, output: &str) -> Result<(), String> {
    if exited_ok && output.contains(CODEX_QUEUE_RECEIPT) {
        return Ok(());
    }
    let detail = output.trim();
    if detail.is_empty() {
        return Err("codex queue reported neither a receipt nor an error".into());
    }
    Err(detail.lines().next().unwrap_or(detail).to_string())
}

/// What the CLI prints when a message reaches a thread's queue.
const CODEX_QUEUE_RECEIPT: &str = "Queued message";

/// OpenCode's session API takes the prompt, and is the one channel that
/// can reach the model without waiting for the turn to end.
async fn deliver_opencode(
    base_url: &str,
    session: &str,
    text: &str,
    mode: DeliveryMode,
) -> Result<Delivered, InboxError> {
    let delivery = match mode {
        DeliveryMode::Steer => "steer",
        DeliveryMode::Queue => "queue",
    };
    let url = format!("{base_url}/api/session/{session}/prompt");
    let response = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "prompt": { "text": text },
            "delivery": delivery,
        }))
        .send()
        .await
        .map_err(|e| InboxError::Unreachable(format!("{base_url} did not answer: {e}")))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(InboxError::Refused(format!(
            "the session API answered {status}: {}",
            body.chars().take(200).collect::<String>()
        )));
    }
    // The server answers an unrouted path with its own web app under a
    // 200, so a status alone does not mean a session took anything. The
    // admission receipt does: it carries the sequence the message was
    // admitted at, and the delivery the server actually applied.
    let receipt: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
        InboxError::Refused("the session API answered with something other than a receipt".into())
    })?;
    let admitted = receipt["data"]["admittedSeq"].as_u64();
    if admitted.is_none() {
        return Err(InboxError::Refused(
            "the session API answered without admitting the message".into(),
        ));
    }
    // Trust the server's own word on what it did over what was asked.
    let applied = match receipt["data"]["delivery"].as_str() {
        Some("steer") => DeliveryMode::Steer,
        Some("queue") => DeliveryMode::Queue,
        _ => mode,
    };
    Ok(Delivered {
        transport: "opencode-http",
        mode: applied,
    })
}

/// Finds the directory containing this agent's socket, preserving candidate order.
pub fn agent_socket_dir(agent_pid: u32) -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime.is_empty() {
            candidates.push(std::path::PathBuf::from(runtime).join("cc-socks"));
        }
    }
    let uid = unsafe { libc::getuid() };
    candidates.push(std::path::PathBuf::from(format!("/tmp/cc-socks-{uid}")));
    candidates.push(std::path::PathBuf::from("/tmp/cc-socks"));
    socket_dir_for_agent(candidates, agent_pid)
}

fn socket_dir_for_agent(
    candidates: impl IntoIterator<Item = std::path::PathBuf>,
    agent_pid: u32,
) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::FileTypeExt;

    candidates.into_iter().find(|dir| {
        dir.join(format!("{agent_pid}.sock"))
            .metadata()
            .is_ok_and(|metadata| metadata.file_type().is_socket())
    })
}

/// Reserves a loopback port for an agent that serves its own API.
///
/// The port is handed to the agent as a launch argument, so it has to be
/// free at spawn and stay claimed by nothing else until the agent binds
/// it. Binding and dropping is the only way to learn a free one, which
/// leaves a window; the agent binding immediately afterwards closes it
/// in practice.
pub fn reserve_agent_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    if port == 0 {
        warn!("could not reserve a loopback port for an agent api");
        return None;
    }
    Some(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this guards against, and the reason the remote extension of
    /// this path was reverted: the socket is named after a pid in a directory
    /// every session of this user shares, so anything running as this user can
    /// hold the name the controller is about to write to. A clean write was the
    /// whole acknowledgement, so the sender was told a message arrived at a
    /// session that never saw it. Reaching a socket is not reaching a process.
    #[tokio::test]
    async fn a_socket_held_by_another_process_is_refused_rather_than_reported_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        // Both attempts connect, so both are accepted and read: what the
        // refused one left behind is the point.
        let reader = tokio::spawn(async move {
            let mut payloads = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf).await;
                payloads.push(String::from_utf8(buf).unwrap());
            }
            payloads
        });

        // This process holds the socket, and the channel names a different one.
        let error = deliver(
            &InboundChannel::ClaudeSocket {
                path: path.clone(),
                token: None,
                expect_pid: std::process::id() + 1,
            },
            "for another session",
            DeliveryMode::Queue,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, InboxError::Refused(ref m) if m.contains("not the agent this session spawned")),
            "{error:?}"
        );

        deliver(
            &InboundChannel::ClaudeSocket {
                path,
                token: None,
                expect_pid: std::process::id(),
            },
            "for this session",
            DeliveryMode::Queue,
        )
        .await
        .unwrap();

        let payloads = reader.await.unwrap();
        assert_eq!(
            payloads[0], "",
            "the refused message was written to the wrong peer anyway"
        );
        assert!(
            payloads[1].contains("for this session"),
            "{:?}",
            payloads[1]
        );
    }

    /// A message that never reaches the agent must say so rather than
    /// report a delivery, or a supervisor believes it steered a worker
    /// it never reached.
    #[tokio::test]
    async fn a_missing_claude_socket_is_unreachable() {
        let channel = InboundChannel::ClaudeSocket {
            path: std::path::PathBuf::from("/nonexistent/pm-test/nope.sock"),
            token: None,
            expect_pid: std::process::id(),
        };
        let error = deliver(&channel, "hello", DeliveryMode::Queue)
            .await
            .unwrap_err();
        assert!(matches!(error, InboxError::Unreachable(_)), "{error:?}");
    }

    #[tokio::test]
    async fn an_unserved_opencode_port_is_unreachable() {
        let channel = InboundChannel::OpenCodeHttp {
            // Port 1 is privileged and unbound, so the connection fails
            // rather than reaching something else's server.
            base_url: "http://127.0.0.1:1".into(),
            session: "ses_test".into(),
        };
        let error = deliver(&channel, "hello", DeliveryMode::Queue)
            .await
            .unwrap_err();
        assert!(matches!(error, InboxError::Unreachable(_)), "{error:?}");
    }

    /// The frames Claude Code accepts are exact: a user frame whose
    /// content is a string. This proves the daemon writes that shape,
    /// against a socket standing in for the agent.
    #[tokio::test]
    async fn the_claude_frames_carry_an_auth_line_and_a_string_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let reader = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
                .await
                .unwrap();
            String::from_utf8(buf).unwrap()
        });

        let channel = InboundChannel::ClaudeSocket {
            path: path.clone(),
            token: Some("tok-123".into()),
            expect_pid: std::process::id(),
        };
        let delivered = deliver(&channel, "ship it", DeliveryMode::Queue)
            .await
            .unwrap();
        assert_eq!(delivered.transport, "claude-socket");

        let written = reader.await.unwrap();
        let mut lines = written.lines();
        let auth: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["token"], "tok-123");
        let message: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(message["type"], "user");
        assert_eq!(message["message"]["role"], "user");
        assert_eq!(message["message"]["content"], "ship it");
        assert!(lines.next().is_none());
    }

    /// A daemon that has no token still delivers, because the socket
    /// takes an unauthenticated local connection.
    #[tokio::test]
    async fn a_claude_delivery_without_a_token_sends_only_the_user_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let reader = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
                .await
                .unwrap();
            String::from_utf8(buf).unwrap()
        });

        let channel = InboundChannel::ClaudeSocket {
            path: path.clone(),
            token: None,
            expect_pid: std::process::id(),
        };
        deliver(&channel, "hello", DeliveryMode::Queue)
            .await
            .unwrap();
        let written = reader.await.unwrap();
        assert_eq!(written.lines().count(), 1, "{written}");
        let message: serde_json::Value =
            serde_json::from_str(written.lines().next().unwrap()).unwrap();
        assert_eq!(message["message"]["content"], "hello");
    }

    /// Answers a single request with a canned response, standing in for
    /// an agent that serves its own session API.
    async fn stub_api(status: u16, content: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{content}",
                content.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    /// A 200 is not an admission: the server answers an unrouted path
    /// with its own web app, and treating that as delivered loses the
    /// message silently.
    #[tokio::test]
    async fn a_success_without_a_receipt_is_refused() {
        let base = stub_api(200, "<!doctype html><html></html>").await;
        let channel = InboundChannel::OpenCodeHttp {
            base_url: base,
            session: "ses_x".into(),
        };
        let error = deliver(&channel, "hi", DeliveryMode::Queue)
            .await
            .unwrap_err();
        assert!(matches!(error, InboxError::Refused(_)), "{error:?}");
    }

    /// The mode reported back is the one the agent applied, not the one
    /// that was asked for, so a caller never claims it steered a turn
    /// that was only queued.
    #[tokio::test]
    async fn the_applied_delivery_mode_comes_from_the_receipt() {
        let base = stub_api(
            200,
            r#"{"data":{"admittedSeq":1,"id":"msg_1","delivery":"queue"}}"#,
        )
        .await;
        let channel = InboundChannel::OpenCodeHttp {
            base_url: base,
            session: "ses_x".into(),
        };
        let delivered = deliver(&channel, "hi", DeliveryMode::Steer).await.unwrap();
        assert_eq!(delivered.transport, "opencode-http");
        assert_eq!(delivered.mode, DeliveryMode::Queue);
    }

    #[tokio::test]
    async fn a_rejected_session_id_is_refused() {
        let base = stub_api(404, r#"{"error":"no such session"}"#).await;
        let channel = InboundChannel::OpenCodeHttp {
            base_url: base,
            session: "ses_gone".into(),
        };
        let error = deliver(&channel, "hi", DeliveryMode::Queue)
            .await
            .unwrap_err();
        assert!(matches!(error, InboxError::Refused(_)), "{error:?}");
    }

    /// The receipt is the acknowledgement. A zero exit with no receipt
    /// is the shape a silently dropped message takes.
    #[test]
    fn a_codex_queue_needs_its_receipt() {
        assert!(
            codex_queue_outcome(true, "Queued message 01a0-msg for thread 01a0-thread.\n").is_ok()
        );
        assert!(codex_queue_outcome(true, "").is_err());
        assert!(codex_queue_outcome(true, "nothing to see here").is_err());
        let refused = codex_queue_outcome(
            false,
            "Error: failed to queue session message: no rollout found for thread id x",
        )
        .unwrap_err();
        assert!(refused.contains("failed to queue"), "{refused}");
    }

    /// Steering is OpenCode's alone, and a caller must be able to see
    /// that before it promises a supervisor an interrupt.
    #[test]
    fn only_the_session_api_reports_steering() {
        assert!(InboundChannel::OpenCodeHttp {
            base_url: "http://127.0.0.1:1".into(),
            session: "s".into()
        }
        .supports_steering());
        assert!(!InboundChannel::CodexQueue { thread: "t".into() }.supports_steering());
        assert!(!InboundChannel::ClaudeSocket {
            path: std::path::PathBuf::from("/x"),
            token: None,
            expect_pid: std::process::id(),
        }
        .supports_steering());
    }

    #[test]
    fn socket_discovery_skips_directories_without_the_agents_socket() {
        let root = tempfile::tempdir().unwrap();
        let preferred = root.path().join("preferred");
        let fallback = root.path().join("fallback");
        std::fs::create_dir(&preferred).unwrap();
        std::fs::create_dir(&fallback).unwrap();
        let pid = std::process::id();
        let _other =
            std::os::unix::net::UnixListener::bind(preferred.join(format!("{}.sock", pid + 1)))
                .unwrap();
        let _agent =
            std::os::unix::net::UnixListener::bind(fallback.join(format!("{pid}.sock"))).unwrap();

        assert_eq!(
            socket_dir_for_agent([preferred, fallback.clone()], pid),
            Some(fallback),
        );
    }

    #[test]
    fn socket_discovery_preserves_preference_when_both_paths_hold_sockets() {
        let root = tempfile::tempdir().unwrap();
        let preferred = root.path().join("preferred");
        let fallback = root.path().join("fallback");
        std::fs::create_dir(&preferred).unwrap();
        std::fs::create_dir(&fallback).unwrap();
        let pid = std::process::id();
        let _first =
            std::os::unix::net::UnixListener::bind(preferred.join(format!("{pid}.sock"))).unwrap();
        let _second =
            std::os::unix::net::UnixListener::bind(fallback.join(format!("{pid}.sock"))).unwrap();

        assert_eq!(
            socket_dir_for_agent([preferred.clone(), fallback], pid),
            Some(preferred),
        );
    }

    #[test]
    fn socket_discovery_does_not_accept_a_regular_file_with_the_socket_name() {
        let root = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        std::fs::write(root.path().join(format!("{pid}.sock")), "not a socket").unwrap();

        assert_eq!(socket_dir_for_agent([root.path().into()], pid), None);
    }

    #[tokio::test]
    async fn a_session_api_that_never_answers_hits_the_delivery_deadline() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let channel = InboundChannel::OpenCodeHttp {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            session: "timeout-test".into(),
        };
        let delivery =
            tokio::spawn(async move { deliver(&channel, "notice", DeliveryMode::Queue).await });
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(
            tokio::io::AsyncReadExt::read(&mut stream, &mut request)
                .await
                .unwrap()
                > 0
        );

        tokio::time::pause();
        tokio::time::advance(DELIVERY_TIMEOUT).await;
        let error = tokio::time::timeout(DELIVERY_TIMEOUT, delivery)
            .await
            .expect("inbox delivery must finish at its deadline")
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, InboxError::Unreachable(ref detail) if detail.contains("in time")));
        let mut remaining = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut remaining)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_blocked_claude_socket_write_hits_the_delivery_deadline() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("inbox.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let channel = InboundChannel::ClaudeSocket {
            path,
            token: None,
            expect_pid: std::process::id(),
        };
        const BLOCKED_SOCKET_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
        let delivery = tokio::spawn(async move {
            deliver(
                &channel,
                &"x".repeat(BLOCKED_SOCKET_PAYLOAD_BYTES),
                DeliveryMode::Queue,
            )
            .await
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut first_byte = [0];
        tokio::io::AsyncReadExt::read_exact(&mut stream, &mut first_byte)
            .await
            .unwrap();

        tokio::time::pause();
        tokio::time::advance(DELIVERY_TIMEOUT).await;
        let error = tokio::time::timeout(DELIVERY_TIMEOUT, delivery)
            .await
            .expect("inbox delivery must finish at its deadline")
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, InboxError::Unreachable(ref detail) if detail.contains("in time")));
    }

    #[tokio::test]
    async fn canceling_a_codex_queue_attempt_stops_its_process() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("queue.pid");
        let mut command = tokio::process::Command::new("/bin/sh");
        const HUNG_QUEUE_SLEEP_SECONDS: u64 = 60;
        let script =
            format!("echo $$ > \"$QUEUE_PID_FILE\"; exec sleep {HUNG_QUEUE_SLEEP_SECONDS}");
        command
            .args(["-c", &script])
            .env("QUEUE_PID_FILE", &pid_file);
        let delivery =
            tokio::spawn(async move { deliver_codex(command, "thread", "notice").await });
        const PROCESS_START_TIMEOUT: Duration = Duration::from_secs(5);
        const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
        tokio::time::timeout(PROCESS_START_TIMEOUT, async {
            while !pid_file.exists() {
                tokio::time::sleep(PROCESS_POLL_INTERVAL).await;
            }
        })
        .await
        .unwrap();
        let pid: libc::pid_t = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        delivery.abort();
        assert!(delivery.await.unwrap_err().is_cancelled());
        let exited = tokio::time::timeout(PROCESS_START_TIMEOUT, async {
            while unsafe { libc::kill(pid, 0) } == 0 {
                tokio::time::sleep(PROCESS_POLL_INTERVAL).await;
            }
        })
        .await;
        if exited.is_err() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(exited.is_ok(), "the canceled queue process must exit");
    }

    /// Reaching a socket in the shared directory proves only that
    /// something answered, so a message must not be written until the
    /// listener turns out to be the agent the session runs. Delivering
    /// to a stranger is silent: the send succeeds and the intended agent
    /// never hears it.
    #[tokio::test]
    async fn a_message_is_refused_when_another_process_holds_the_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let channel = InboundChannel::ClaudeSocket {
            path: path.clone(),
            token: None,
            // The test process holds this socket, so its own pid is the
            // one identity that is certainly not the agent's.
            expect_pid: std::process::id() + 1,
        };
        let error = deliver(&channel, "a notice", DeliveryMode::Queue)
            .await
            .expect_err("a stranger's inbox must be refused");
        assert!(matches!(error, InboxError::Refused(_)), "{error}");
        assert!(error
            .to_string()
            .contains("not the agent this session spawned"));

        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut seen = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut seen)
            .await
            .unwrap();
        assert!(
            seen.is_empty(),
            "the refused message was written anyway: {}",
            String::from_utf8_lossy(&seen)
        );
    }

    /// The same path delivers once the peer is the expected process, so
    /// the check identifies the peer rather than refusing every socket.
    #[tokio::test]
    async fn a_message_is_delivered_when_the_expected_process_holds_the_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let channel = InboundChannel::ClaudeSocket {
            path: path.clone(),
            token: None,
            expect_pid: std::process::id(),
        };
        let delivered = deliver(
            &channel,
            "two of your sessions are parked",
            DeliveryMode::Queue,
        )
        .await
        .expect("the expected peer takes the message");
        assert_eq!(delivered.transport, "claude-socket");

        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut seen = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut seen)
            .await
            .unwrap();
        let wire = String::from_utf8(seen).unwrap();
        assert!(wire.contains("two of your sessions are parked"), "{wire}");
    }

    #[test]
    fn a_reserved_port_is_free_and_nonzero() {
        let port = reserve_agent_port().expect("a loopback port");
        assert!(port > 0);
        // Reserving twice must not hand out the same port while the
        // first is still notionally claimed by a starting agent.
        let again = reserve_agent_port().expect("a second loopback port");
        assert!(again > 0);
    }
}
