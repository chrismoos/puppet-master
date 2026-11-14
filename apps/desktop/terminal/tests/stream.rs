//! The socket client against a stand-in controller speaking the same frames.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use pm_native_terminal::protocol::{
    self, FLAG_REPLAY, FLAG_REPLAY_END, FLAG_REPLAY_GHOSTTY, FLAG_REPLAY_SNAPSHOT,
    FLAG_REPLAY_START,
};
use pm_native_terminal::stream::{Attach, Event, Stream};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;

const GENERATION: u64 = 3;
const VIEWER: u64 = 11;
const WAIT: Duration = Duration::from_secs(5);

fn output(flags: u8, data: &[u8]) -> Message {
    let frame = [&[0x01][..], &GENERATION.to_le_bytes(), &[flags], data].concat();
    Message::Binary(frame.into())
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !check() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
// The handshake callback's error type is tungstenite's own, and large.
#[allow(clippy::result_large_err)]
async fn a_snapshot_replay_then_live_output_build_the_screen_and_input_reaches_the_controller() {
    let mut worker = pm_ghostty::Terminal::new(40, 6).unwrap();
    worker.write(b"restored from \x1b[32mthe worker\x1b[0m\r\n$ ");
    let snapshot = worker.snapshot().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let seen = received.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            tcp,
            |request: &Request, mut response: Response| {
                let asked = request
                    .headers()
                    .get("Sec-WebSocket-Protocol")
                    .and_then(|v| v.to_str().ok());
                assert_eq!(asked, Some(protocol::SUBPROTOCOL));
                assert!(request
                    .uri()
                    .query()
                    .unwrap_or("")
                    .contains("ticket=one-use"));
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    protocol::SUBPROTOCOL.parse().unwrap(),
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        let (half, rest) = snapshot.split_at(snapshot.len() / 2);
        let flags = FLAG_REPLAY | FLAG_REPLAY_SNAPSHOT | FLAG_REPLAY_GHOSTTY;
        socket
            .send(output(flags | FLAG_REPLAY_START, half))
            .await
            .unwrap();
        socket
            .send(output(flags | FLAG_REPLAY_END, rest))
            .await
            .unwrap();
        socket.send(output(0, b"echo live")).await.unwrap();
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Binary(frame) = message {
                seen.lock().unwrap().push(frame.to_vec());
            }
        }
    });

    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let log = events.clone();
    let stream = Stream::open(
        Attach {
            url: format!("ws://127.0.0.1:{port}/ws/terminal/7?generation={GENERATION}&viewer={VIEWER}&ticket=one-use"),
            generation: GENERATION,
            viewer: VIEWER,
            claim: None,
        },
        move |event| log.lock().unwrap().push(event),
    );
    let screen = stream.screen();
    let row = |y: u16| {
        let mut guard = screen.lock().unwrap();
        guard
            .as_mut()
            .map(|vt| vt.update().unwrap().row_text(y))
            .unwrap_or_default()
    };
    eventually("the live output on the restored screen", || {
        row(1) == "$ echo live"
    })
    .await;
    assert_eq!(row(0), "restored from the worker");
    assert!(events.lock().unwrap().contains(&Event::Changed));

    stream.input(b"ls\r".to_vec(), true);
    stream.claim(100, 30);
    eventually("the ack, the input and the claim", || {
        received.lock().unwrap().len() >= 3
    })
    .await;
    let frames = received.lock().unwrap().clone();
    assert_eq!(
        frames[0],
        protocol::ack(GENERATION, "echo live".len() as u32),
        "live output is acknowledged"
    );
    assert_eq!(frames[1], protocol::input(GENERATION, b"ls\r", true));
    assert_eq!(frames[2], protocol::resize_request(GENERATION, 1, 100, 30));
    drop(stream);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::result_large_err)]
async fn the_first_claim_rides_in_the_url_and_program_copies_are_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let query: Arc<Mutex<String>> = Arc::default();
    let seen_query = query.clone();
    let received: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let seen = received.clone();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            tcp,
            |request: &Request, mut response: Response| {
                *seen_query.lock().unwrap() = request.uri().query().unwrap_or("").to_string();
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    protocol::SUBPROTOCOL.parse().unwrap(),
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        socket
            .send(output(
                FLAG_REPLAY | FLAG_REPLAY_START | FLAG_REPLAY_END,
                b"$ ",
            ))
            .await
            .unwrap();
        socket
            .send(output(0, b"\x1b]52;c;Y29waWVk\x07"))
            .await
            .unwrap();
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Binary(frame) = message {
                seen.lock().unwrap().push(frame.to_vec());
            }
        }
    });
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    let log = events.clone();
    let stream = Stream::open(
        Attach {
            url: format!("ws://127.0.0.1:{port}/ws/terminal/7?generation={GENERATION}&viewer={VIEWER}&ticket=t"),
            generation: GENERATION,
            viewer: VIEWER,
            claim: Some((120, 40)),
        },
        move |event| log.lock().unwrap().push(event),
    );
    eventually("the program's copy", || {
        events
            .lock()
            .unwrap()
            .contains(&Event::Clipboard("copied".into()))
    })
    .await;
    assert!(
        query.lock().unwrap().ends_with("&cols=120&rows=40&claim=1"),
        "query was {}",
        query.lock().unwrap()
    );
    stream.claim(100, 30);
    eventually("the later claim", || {
        received.lock().unwrap().iter().any(|f| f[0] == 0x08)
    })
    .await;
    let claim = received
        .lock()
        .unwrap()
        .iter()
        .find(|f| f[0] == 0x08)
        .cloned()
        .unwrap();
    assert_eq!(
        claim,
        protocol::resize_request(GENERATION, 2, 100, 30),
        "a later claim counts on from the URL's"
    );
    drop(stream);
    server.abort();
}
