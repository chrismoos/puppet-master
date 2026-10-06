//! The remote transport: the daemon's client protocol over the
//! controller's WebSockets instead of its unix socket.
//!
//! The controller carries commands and events on `/ws` and each attached
//! terminal on a socket of its own, where the unix socket carries both on
//! one connection. Terminal messages are translated here so a caller
//! attaches, types and resizes the same way over either transport.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pm_protocol::domain::{ClientEnvelope, ClientMsg, Scope, ServerMsg, Terminal, TerminalKind};
use pm_protocol::terminal_frame::{self, TerminalFrame};
use pm_tls::{KeyHash, WebTrust};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, StatusCode};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::WebSocketStream;

use crate::remote::{ControllerUrl, RemoteStore};
use crate::{Client, Dispatch, Target};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Keeps an idle control socket from being dropped by a proxy or a NAT
/// table between here and the controller.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

const CONTROL_PATH: &str = "/ws";
const TERMINAL_SUBPROTOCOL: &str = "pm-terminal-v1";

/// The close code the controller sends when it does not accept the
/// credential a control socket was opened with.
const WS_CLOSE_UNAUTHENTICATED: u16 = 4401;

/// One saved login, named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub store: RemoteStore,
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DialError {
    #[error("{0}")]
    Connect(String),
    /// The server's certificate was not accepted, as opposed to the
    /// connection failing for a reason that says nothing about who it is.
    #[error("its certificate is not trusted ({0})")]
    Certificate(String),
    #[error("the controller did not accept this login")]
    Unauthorized,
    #[error("{0}")]
    Handshake(String),
}

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

type Socket = WebSocketStream<Box<dyn Io>>;

async fn open_tcp(url: &ControllerUrl) -> Result<TcpStream, DialError> {
    let connect = TcpStream::connect((url.bare_host(), url.port));
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, connect)
        .await
        .map_err(|_| DialError::Connect("timed out connecting".to_string()))?
        .map_err(|e| DialError::Connect(e.to_string()))?;
    let _ = tcp.set_nodelay(true);
    Ok(tcp)
}

async fn open_tls(
    url: &ControllerUrl,
    trust: &WebTrust,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, DialError> {
    let config = crate::remote::tls_config(trust).map_err(|e| DialError::Connect(e.to_string()))?;
    let name = rustls::pki_types::ServerName::try_from(url.bare_host().to_string())
        .map_err(|e| DialError::Connect(e.to_string()))?;
    let tcp = open_tcp(url).await?;
    let handshake = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp);
    tokio::time::timeout(CONNECT_TIMEOUT, handshake)
        .await
        .map_err(|_| DialError::Connect("timed out in the TLS handshake".to_string()))?
        .map_err(|error| {
            match error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<rustls::Error>())
            {
                Some(rustls::Error::InvalidCertificate(reason)) => {
                    DialError::Certificate(format!("{reason:?}"))
                }
                _ => DialError::Connect(error.to_string()),
            }
        })
}

/// Completes a TLS handshake and reports the key the server presented.
pub(crate) async fn tls_handshake(
    url: &ControllerUrl,
    trust: &WebTrust,
) -> Result<KeyHash, DialError> {
    let stream = open_tls(url, trust).await?;
    let (_, connection) = stream.get_ref();
    pm_tls::peer_key_hash(connection.peer_certificates())
        .ok_or_else(|| DialError::Connect("the server presented no certificate".to_string()))
}

async fn open_socket(
    url: &ControllerUrl,
    trust: &WebTrust,
    path_and_query: &str,
    access_token: &str,
    subprotocol: Option<&str>,
) -> Result<Socket, DialError> {
    let invalid = |e: &dyn std::fmt::Display| DialError::Handshake(e.to_string());
    let mut request = url
        .ws_url(path_and_query)
        .into_client_request()
        .map_err(|e| invalid(&e))?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {access_token}")
            .parse()
            .map_err(|e| invalid(&e))?,
    );
    if let Some(subprotocol) = subprotocol {
        request.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            subprotocol.parse().map_err(|e| invalid(&e))?,
        );
    }
    let stream: Box<dyn Io> = if url.secure {
        Box::new(open_tls(url, trust).await?)
    } else {
        Box::new(open_tcp(url).await?)
    };
    let upgrade = tokio_tungstenite::client_async(request, stream);
    match tokio::time::timeout(CONNECT_TIMEOUT, upgrade).await {
        Err(_) => Err(DialError::Connect(
            "timed out opening the WebSocket".to_string(),
        )),
        Ok(Ok((socket, _))) => Ok(socket),
        Ok(Err(tungstenite::Error::Http(response)))
            if response.status() == StatusCode::UNAUTHORIZED =>
        {
            Err(DialError::Unauthorized)
        }
        Ok(Err(tungstenite::Error::Http(response))) => Err(DialError::Handshake(format!(
            "the controller answered {}",
            response.status()
        ))),
        Ok(Err(e)) => Err(DialError::Handshake(e.to_string())),
    }
}

async fn open_authenticated(
    remote: &Remote,
    path_and_query: &str,
    subprotocol: Option<&str>,
) -> anyhow::Result<Socket> {
    let profile = remote.store.current(&remote.name).await?;
    let url = profile.controller_url()?;
    open_socket(
        &url,
        &profile.trust()?,
        path_and_query,
        &profile.access_token,
        subprotocol,
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "cannot connect to controller {:?} at {}: {e}",
            remote.name,
            url.http_base()
        )
    })
}

/// Opens the control socket and starts the tasks that carry it.
pub(crate) async fn start(
    remote: &Remote,
    mut out_rx: mpsc::UnboundedReceiver<ClientEnvelope>,
    dispatch: Dispatch,
) -> anyhow::Result<()> {
    let socket = open_authenticated(remote, CONTROL_PATH, None).await?;
    let (mut sink, mut stream) = socket.split();

    let mut terminals = Terminals {
        remote: remote.clone(),
        dispatch: dispatch.clone(),
        links: HashMap::new(),
    };
    tokio::spawn(async move {
        let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let frame = tokio::select! {
                envelope = out_rx.recv() => {
                    let Some(envelope) = envelope else { break };
                    match terminals.route(envelope).await {
                        Some(envelope) => Message::Binary(envelope.encode_to_vec().into()),
                        None => continue,
                    }
                }
                _ = keepalive.tick() => Message::Ping(Bytes::new()),
            };
            if sink.send(frame).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let name = remote.name.clone();
    tokio::spawn(async move {
        let mut refusal = None;
        while let Some(Ok(message)) = stream.next().await {
            match message {
                Message::Binary(buf) => {
                    let Ok(msg) = ServerMsg::decode(&buf) else {
                        break;
                    };
                    if !dispatch.deliver(msg).await {
                        break;
                    }
                }
                Message::Close(frame) => {
                    if frame.is_some_and(|f| u16::from(f.code) == WS_CLOSE_UNAUTHENTICATED) {
                        refusal = Some(format!(
                            "controller {name:?} did not accept the saved login, \
                             sign in again with `pm login`"
                        ));
                    }
                    break;
                }
                _ => {}
            }
        }
        dispatch.close(refusal);
    });
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PtyAddress {
    Session(u64),
    Terminal(u64),
}

enum TerminalOp {
    Input(Bytes),
    Resize { cols: u16, rows: u16 },
}

struct Link {
    ops: mpsc::UnboundedSender<TerminalOp>,
    task: JoinHandle<()>,
    attach_seq: u64,
}

/// The terminal sockets one client holds open, keyed the way the caller
/// addressed them.
struct Terminals {
    remote: Remote,
    dispatch: Dispatch,
    links: HashMap<PtyAddress, Link>,
}

impl Drop for Terminals {
    fn drop(&mut self) {
        for link in self.links.values() {
            link.task.abort();
        }
    }
}

fn command_result(seq: u64, result: Result<Option<u64>, String>) -> ServerMsg {
    ServerMsg::CommandResult {
        seq,
        result,
        data: Bytes::new(),
    }
}

impl Terminals {
    /// Handles a terminal message here and returns anything else for the
    /// control socket.
    async fn route(&mut self, envelope: ClientEnvelope) -> Option<ClientEnvelope> {
        let seq = envelope.seq;
        match envelope.msg {
            ClientMsg::AttachPty { session_id } => {
                self.attach(PtyAddress::Session(session_id), seq).await
            }
            ClientMsg::AttachTerminal { terminal_id } => {
                self.attach(PtyAddress::Terminal(terminal_id), seq).await
            }
            ClientMsg::DetachPty { session_id } => {
                self.detach(PtyAddress::Session(session_id), seq).await
            }
            ClientMsg::DetachTerminal { terminal_id } => {
                self.detach(PtyAddress::Terminal(terminal_id), seq).await
            }
            ClientMsg::PtyInput { session_id, data } => {
                self.op(PtyAddress::Session(session_id), TerminalOp::Input(data))
            }
            ClientMsg::TerminalInput { terminal_id, data } => {
                self.op(PtyAddress::Terminal(terminal_id), TerminalOp::Input(data))
            }
            ClientMsg::PtyResize {
                session_id,
                cols,
                rows,
            } => self.op(
                PtyAddress::Session(session_id),
                TerminalOp::Resize { cols, rows },
            ),
            ClientMsg::TerminalResize {
                terminal_id,
                cols,
                rows,
            } => self.op(
                PtyAddress::Terminal(terminal_id),
                TerminalOp::Resize { cols, rows },
            ),
            msg => return Some(ClientEnvelope { seq, msg }),
        }
        None
    }

    async fn close_link(&mut self, address: PtyAddress) {
        let Some(link) = self.links.remove(&address) else {
            return;
        };
        link.task.abort();
        self.dispatch
            .deliver(command_result(
                link.attach_seq,
                Err("attach canceled".to_string()),
            ))
            .await;
    }

    async fn attach(&mut self, address: PtyAddress, seq: u64) {
        self.close_link(address).await;
        let (ops, ops_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_link(
            self.remote.clone(),
            self.dispatch.clone(),
            address,
            seq,
            ops_rx,
        ));
        self.links.insert(
            address,
            Link {
                ops,
                task,
                attach_seq: seq,
            },
        );
    }

    async fn detach(&mut self, address: PtyAddress, seq: u64) {
        self.close_link(address).await;
        self.dispatch.deliver(command_result(seq, Ok(None))).await;
    }

    fn op(&mut self, address: PtyAddress, op: TerminalOp) {
        if let Some(link) = self.links.get(&address) {
            let _ = link.ops.send(op);
        }
    }
}

/// Finds the terminal an address names, with the generation its socket
/// must be opened for. Boxed because it opens a client of its own, which
/// would otherwise make the future of opening a client contain itself.
fn find_terminal(
    remote: Remote,
    address: PtyAddress,
) -> Pin<Box<dyn Future<Output = Result<Terminal, String>> + Send>> {
    Box::pin(async move {
        let mut client = Client::open(&Target::Remote(remote))
            .await
            .map_err(|e| e.to_string())?;
        client
            .request(ClientMsg::Subscribe { scope: Scope::All })
            .await
            .map_err(|e| e.to_string())?;
        let snapshot = loop {
            match client.next_msg().await {
                Some(ServerMsg::Snapshot(snapshot)) => break snapshot,
                Some(_) => continue,
                None => return Err("connection closed before the terminal was found".to_string()),
            }
        };
        let found = snapshot.terminals.into_iter().find(|t| match address {
            PtyAddress::Terminal(id) => t.id == id,
            PtyAddress::Session(id) => t.session_id == id && t.kind == TerminalKind::Agent,
        });
        found.ok_or_else(|| match address {
            PtyAddress::Terminal(id) => format!("terminal {id} not found"),
            PtyAddress::Session(id) => format!("session {id} has no agent terminal"),
        })
    })
}

async fn open_link(remote: &Remote, address: PtyAddress) -> Result<(Socket, Terminal), String> {
    let terminal = find_terminal(remote.clone(), address).await?;
    let path = format!(
        "/ws/terminal/{}?generation={}",
        terminal.id, terminal.generation
    );
    let socket = open_authenticated(remote, &path, Some(TERMINAL_SUBPROTOCOL))
        .await
        .map_err(|e| e.to_string())?;
    Ok((socket, terminal))
}

/// Carries one attached terminal: its socket's output becomes `PtyOutput`
/// messages, and the attach is acknowledged once the replay has arrived,
/// which is the order the unix socket delivers them in.
async fn run_link(
    remote: Remote,
    dispatch: Dispatch,
    address: PtyAddress,
    attach_seq: u64,
    mut ops: mpsc::UnboundedReceiver<TerminalOp>,
) {
    let (socket, terminal) = match open_link(&remote, address).await {
        Ok(opened) => opened,
        Err(error) => {
            dispatch
                .deliver(command_result(attach_seq, Err(error)))
                .await;
            return;
        }
    };
    let generation = terminal.generation;
    let (mut sink, mut stream) = socket.split();
    let mut acknowledged = false;
    let mut close_reason = String::new();
    loop {
        tokio::select! {
            message = stream.next() => {
                let Some(Ok(message)) = message else { break };
                let frame = match message {
                    Message::Binary(frame) => frame,
                    Message::Close(frame) => {
                        if let Some(frame) = frame {
                            close_reason = frame.reason.to_string();
                        }
                        break;
                    }
                    _ => continue,
                };
                let Some(TerminalFrame::Output { flags, data, .. }) =
                    terminal_frame::decode(&frame)
                else {
                    continue;
                };
                let replay = flags & terminal_frame::FLAG_REPLAY != 0;
                let output = ServerMsg::PtyOutput {
                    session_id: terminal.session_id,
                    terminal_id: terminal.id,
                    generation,
                    data: Bytes::copy_from_slice(data),
                    replay,
                };
                if !dispatch.deliver(output).await {
                    return;
                }
                let replay_over = !replay || flags & terminal_frame::FLAG_REPLAY_END != 0;
                if !acknowledged && replay_over {
                    acknowledged = true;
                    dispatch.deliver(command_result(attach_seq, Ok(None))).await;
                }
            }
            op = ops.recv() => {
                let frame = match op {
                    Some(TerminalOp::Input(data)) => terminal_frame::encode_input(generation, &data),
                    Some(TerminalOp::Resize { cols, rows }) => {
                        terminal_frame::encode_resize(generation, cols, rows)
                    }
                    None => break,
                };
                if sink.send(Message::Binary(frame)).await.is_err() {
                    break;
                }
            }
        }
    }
    if !acknowledged {
        let error = if close_reason.is_empty() {
            "the terminal closed before it could be attached".to_string()
        } else {
            close_reason
        };
        dispatch
            .deliver(command_result(attach_seq, Err(error)))
            .await;
    }
}
