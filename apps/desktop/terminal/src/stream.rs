//! One terminal's socket to the controller, feeding its screen.
//!
//! The page that owns the terminal mints the socket's one-use ticket and
//! hands over the whole URL, so the app needs no login of its own.

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{self, Incoming, Replay, FLAG_REPLAY};
use crate::vt::Vt;

/// What the socket reports back to whoever shows the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The screen changed and should be drawn again.
    Changed,
    /// Another viewer resized the PTY; this screen now has that size.
    PtySize { cols: u16, rows: u16 },
    /// Which viewer owns the size: this one when `local`.
    Ownership { local: bool, cols: u16, rows: u16 },
    /// A program copied text to the clipboard (OSC 52).
    Clipboard(String),
    /// The shell reported the program now in the foreground.
    Command(String),
    /// The socket closed; a new ticket is needed to attach again.
    Closed { reason: String },
}

pub struct Attach {
    /// The socket URL with its ticket, such as
    /// `wss://pm.example.com/ws/terminal/7?generation=2&viewer=5&ticket=…`.
    pub url: String,
    pub generation: u64,
    pub viewer: u64,
    /// The grid to claim as the socket opens, as the browser's first connect does.
    pub claim: Option<(u16, u16)>,
}

/// The request id of a claim made in the socket URL; later claims count up from it.
const URL_CLAIM: u64 = 1;

enum Command {
    Input(Vec<u8>, bool),
    Claim { cols: u16, rows: u16 },
    Resync,
}

/// A running socket. Dropping it closes the socket.
pub struct Stream {
    commands: mpsc::UnboundedSender<Command>,
    screen: Arc<Mutex<Option<Vt>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Stream {
    /// Opens the socket on the current tokio runtime. `on_event` is called
    /// from the socket's task, so it should only signal, never block.
    pub fn open(attach: Attach, on_event: impl Fn(Event) + Send + Sync + 'static) -> Stream {
        Self::open_on(&tokio::runtime::Handle::current(), attach, on_event)
    }

    /// Opens the socket on `runtime`, for a caller outside any runtime.
    pub fn open_on(
        runtime: &tokio::runtime::Handle,
        attach: Attach,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> Stream {
        let (commands, receiver) = mpsc::unbounded_channel();
        let screen = Arc::new(Mutex::new(None));
        let task = runtime.spawn(run(attach, screen.clone(), receiver, Arc::new(on_event)));
        Stream {
            commands,
            screen,
            task,
        }
    }

    /// The screen, once the first replay has arrived.
    pub fn screen(&self) -> Arc<Mutex<Option<Vt>>> {
        self.screen.clone()
    }

    /// Sends typed or pasted bytes. `submitted` marks input ending in Enter.
    pub fn input(&self, data: Vec<u8>, submitted: bool) {
        let _ = self.commands.send(Command::Input(data, submitted));
    }

    /// Claims the terminal's size for this viewer.
    pub fn claim(&self, cols: u16, rows: u16) {
        let _ = self.commands.send(Command::Claim { cols, rows });
    }

    /// Asks for a fresh snapshot, such as after the screen was resized here.
    pub fn resync(&self) {
        let _ = self.commands.send(Command::Resync);
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(
    attach: Attach,
    screen: Arc<Mutex<Option<Vt>>>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    on_event: Arc<dyn Fn(Event) + Send + Sync>,
) {
    let reason = match session(&attach, &screen, &mut commands, &*on_event).await {
        Ok(()) => "the controller closed the terminal".to_string(),
        Err(error) => error,
    };
    on_event(Event::Closed { reason });
}

async fn session(
    attach: &Attach,
    screen: &Mutex<Option<Vt>>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    on_event: &(dyn Fn(Event) + Send + Sync),
) -> Result<(), String> {
    let url = match attach.claim {
        Some((cols, rows)) => format!("{}&cols={cols}&rows={rows}&claim={URL_CLAIM}", attach.url),
        None => attach.url.clone(),
    };
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| error.to_string())?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(protocol::SUBPROTOCOL),
    );
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|error| format!("could not open the terminal: {error}"))?;
    let (mut writer, mut reader) = socket.split();
    let generation = attach.generation;
    let mut replay = Replay::default();
    let mut clipboard = crate::osc::OscWatch::default();
    let mut request_id: u64 = if attach.claim.is_some() { URL_CLAIM } else { 0 };
    loop {
        tokio::select! {
            message = reader.next() => {
                let Some(message) = message else { return Ok(()) };
                let message = message.map_err(|error| format!("the terminal connection failed: {error}"))?;
                let Message::Binary(frame) = message else {
                    if matches!(message, Message::Close(_)) { return Ok(()); }
                    continue;
                };
                let parsed = handle(generation, attach.viewer, &frame, screen, &mut replay, &mut clipboard, on_event)?;
                if parsed > 0 {
                    writer
                        .send(Message::Binary(protocol::ack(generation, parsed).into()))
                        .await
                        .map_err(|error| error.to_string())?;
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()) };
                let frame = match command {
                    Command::Input(data, submitted) => protocol::input(generation, &data, submitted),
                    Command::Claim { cols, rows } => {
                        request_id += 1;
                        protocol::resize_request(generation, request_id, cols, rows)
                    }
                    Command::Resync => protocol::resync(generation),
                };
                writer.send(Message::Binary(frame.into())).await.map_err(|error| error.to_string())?;
            }
        }
    }
}

/// Applies one frame to the screen and returns how many output bytes it
/// parsed, which the controller expects acknowledged.
fn handle(
    generation: u64,
    viewer: u64,
    frame: &[u8],
    screen: &Mutex<Option<Vt>>,
    replay: &mut Replay,
    clipboard: &mut crate::osc::OscWatch,
    on_event: &(dyn Fn(Event) + Send + Sync),
) -> Result<u32, String> {
    match protocol::decode(frame) {
        Some(Incoming::Output {
            generation: g,
            flags,
            data,
        }) if g == generation => {
            if flags & FLAG_REPLAY != 0 {
                let Some(complete) = replay.push(flags, &data) else {
                    return Ok(0);
                };
                let restored = if complete.ghostty_snapshot {
                    Vt::restore(&complete.data).map_err(|error| error.to_string())?
                } else {
                    let (cols, rows) = screen.lock().unwrap().as_ref().map_or((80, 24), |vt| {
                        (vt.frame().cols.max(1), vt.frame().rows.max(1))
                    });
                    let mut vt = Vt::new(cols, rows).map_err(|error| error.to_string())?;
                    vt.write(&complete.data);
                    vt
                };
                *screen.lock().unwrap() = Some(restored);
                on_event(Event::Changed);
                return Ok(0);
            }
            let mut guard = screen.lock().unwrap();
            let Some(vt) = guard.as_mut() else {
                return Ok(0);
            };
            vt.write(&data);
            drop(guard);
            for found in clipboard.push(&data) {
                on_event(match found {
                    crate::osc::Osc::Copy(text) => Event::Clipboard(text),
                    crate::osc::Osc::Command(command) => Event::Command(command),
                });
            }
            on_event(Event::Changed);
            Ok(u32::try_from(data.len()).unwrap_or(u32::MAX))
        }
        Some(Incoming::PtySize {
            generation: g,
            cols,
            rows,
        }) if g == generation => {
            if let Some(vt) = screen.lock().unwrap().as_mut() {
                vt.resize(cols, rows, 0, 0)
                    .map_err(|error| error.to_string())?;
            }
            on_event(Event::PtySize { cols, rows });
            on_event(Event::Changed);
            Ok(0)
        }
        Some(Incoming::Ownership(ownership)) if ownership.generation == generation => {
            on_event(Event::Ownership {
                local: ownership.owner == viewer,
                cols: ownership.cols,
                rows: ownership.rows,
            });
            Ok(0)
        }
        _ => Ok(0),
    }
}
