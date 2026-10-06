//! The controller dialing a host that cannot dial back.
//!
//! The host here is played by a plain TLS listener speaking the worker
//! plane, so what is under test is the controller's own dialer: that it
//! finds the target, proves the enrollment token, and drives the
//! registration that follows.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use pm_protocol::domain::{ControllerMsg, WorkerMsg};
use pm_protocol::worker_frame::{self, WorkerFrame};
use pm_tls::pairing::{Side, Transcript};
use pm_tls::{Identity, PeerPolicy};
use tokio::net::TcpListener;
use tokio_rustls::TlsStream;
use tokio_tungstenite::tungstenite::Message;

const TEST_TIMEOUT: Duration = Duration::from_secs(20);

fn config(tmp: &tempfile::TempDir) -> pm_daemon::DaemonConfig {
    pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: None,
        public_url: None,
        http_addr: None,
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: pm_adapters::AdapterRegistry::standard(),
        local_worker_enabled: true,
        release_channel: None,
    }
}

/// What the host observed while the controller connected to it.
struct HostOutcome {
    enrolled: bool,
    registered: Option<ControllerMsg>,
}

/// Plays a host that the controller dials: accepts one connection, runs the
/// listener half of the pairing proof, then registers.
async fn listening_host(
    listener: TcpListener,
    identity: Identity,
    token: String,
) -> anyhow::Result<HostOutcome> {
    let (tcp, _) = listener.accept().await?;
    let tls = pm_tls::server_config(&identity, &PeerPolicy::Pairing)?;
    let stream = tokio_rustls::TlsAcceptor::from(tls).accept(tcp).await?;
    let (controller_key, exporter) = {
        let (_, connection) = stream.get_ref();
        (
            pm_tls::peer_key_hash(connection.peer_certificates()).expect("controller key"),
            pm_tls::pairing::exporter(connection)?,
        )
    };
    let mut link = tokio_tungstenite::accept_async(TlsStream::from(stream)).await?;

    // The host is the listener, so it opens enrollment and the controller
    // proves the token before the host answers anything.
    let listener_nonce = pm_tls::pairing::nonce();
    link.send(Message::Binary(
        worker_frame::encode_pair_hello(&listener_nonce).into(),
    ))
    .await?;
    let Some(Ok(Message::Binary(proof))) = link.next().await else {
        anyhow::bail!("controller sent no proof");
    };
    let Some(WorkerFrame::PairProof {
        nonce: dialer_nonce,
        mac,
    }) = worker_frame::decode(&proof)
    else {
        anyhow::bail!("controller sent no proof");
    };
    let transcript = Transcript {
        exporter,
        dialer_key: controller_key,
        listener_key: identity.key_hash(),
        dialer_nonce,
        listener_nonce,
    };
    let enrolled = transcript.verify(&token, Side::Dialer, &mac);
    if !enrolled {
        return Ok(HostOutcome {
            enrolled,
            registered: None,
        });
    }
    link.send(Message::Binary(
        worker_frame::encode_pair_accept(&transcript.mac(&token, Side::Listener)).into(),
    ))
    .await?;

    let register = WorkerMsg::Register {
        protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        enrollment_token: token,
        credential: String::new(),
        hostname: "dmz-box".into(),
        platform: "linux".into(),
        pm_version: "0.1.0+test".into(),
        runtime: String::new(),
        container: String::new(),
        default_project_root: "/srv".into(),
        live_sessions: Vec::new(),
        live_terminals: Vec::new(),
        pending_transcripts: Vec::new(),
        live_dir_shares: Vec::new(),
    };
    link.send(Message::Binary(
        worker_frame::encode_control(&register.encode_to_vec()).into(),
    ))
    .await?;

    let registered = match link.next().await {
        Some(Ok(Message::Binary(buf))) => match worker_frame::decode(&buf) {
            Some(WorkerFrame::Control(payload)) => ControllerMsg::decode(payload).ok(),
            _ => None,
        },
        _ => None,
    };
    Ok(HostOutcome {
        enrolled,
        registered,
    })
}

/// The whole inverted direction, end to end: an enrollment names an address,
/// the controller finds it, dials it, proves the token, and the host that
/// answers is registered as a host of this controller.
#[tokio::test]
async fn the_controller_dials_an_enrolled_host_and_registers_it() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let host_identity = Identity::generate().unwrap();

    let (daemon, _handle) = pm_daemon::start(config(&tmp)).await.unwrap();
    let (token, _expires) = daemon
        .create_dialed_worker_enrollment("dmz-box", &endpoint)
        .unwrap();
    let pending_id = daemon
        .subscribe()
        .0
        .workers
        .into_iter()
        .find(|worker| worker.name == "dmz-box")
        .expect("the pending Host is visible before it answers")
        .id;

    let host = tokio::spawn(listening_host(listener, host_identity.clone(), token));
    let outcome = tokio::time::timeout(TEST_TIMEOUT, host)
        .await
        .expect("the controller dials the enrolled address")
        .unwrap()
        .unwrap();

    assert!(
        outcome.enrolled,
        "the controller must prove the enrollment token to the host"
    );
    let worker_id = match outcome.registered {
        Some(ControllerMsg::Registered {
            worker_id,
            error,
            credential,
            ..
        }) => {
            assert!(error.is_empty(), "registration was refused: {error}");
            assert!(!credential.is_empty(), "enrollment issues a credential");
            worker_id
        }
        other => panic!("expected a registration, got {other:?}"),
    };

    let host = wait_for_worker(&daemon, worker_id).await;
    assert_eq!(worker_id, pending_id);
    assert_eq!(host.hostname, "dmz-box");
    assert_eq!(
        host.connect_mode,
        pm_protocol::domain::ConnectMode::Accept,
        "a host the controller dialed is recorded as one it dials"
    );
    assert_eq!(host.endpoint, endpoint);
}

/// Without the token the controller cannot enroll, so a host that never
/// minted one for this address stays unenrolled however reachable it is.
#[tokio::test]
async fn a_host_the_controller_cannot_prove_itself_to_is_not_enrolled() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let host_identity = Identity::generate().unwrap();

    let (daemon, _handle) = pm_daemon::start(config(&tmp)).await.unwrap();
    let (_token, _expires) = daemon
        .create_dialed_worker_enrollment("dmz-box", &endpoint)
        .unwrap();

    // The host expects a different token than the one this controller holds.
    let host = tokio::spawn(listening_host(
        listener,
        host_identity,
        "a-different-token".into(),
    ));
    let outcome = tokio::time::timeout(TEST_TIMEOUT, host)
        .await
        .expect("the controller dials")
        .unwrap()
        .unwrap();
    assert!(!outcome.enrolled);
    assert!(outcome.registered.is_none());
    let pending = daemon
        .subscribe()
        .0
        .workers
        .into_iter()
        .find(|worker| worker.name == "dmz-box")
        .expect("a Host stays visible while its enrollment is pending");
    assert!(!pending.online);
    assert_eq!(pending.hostname, "");
    assert_eq!(pending.last_seen_at_unix_ms, None);
}

async fn wait_for_worker(
    daemon: &Arc<pm_daemon::Daemon>,
    worker_id: u64,
) -> pm_protocol::domain::Worker {
    for _ in 0..100 {
        if let Some(worker) = daemon
            .subscribe()
            .0
            .workers
            .into_iter()
            .find(|w| w.id == worker_id)
        {
            return worker;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the dialed host never appeared in the registry");
}
