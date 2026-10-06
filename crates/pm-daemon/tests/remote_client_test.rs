//! The client library against a controller's network listener: signing
//! in, keeping the login current, and carrying the unix socket's protocol
//! over the dashboard's WebSockets.

mod support;

use std::sync::Arc;

use pm_client::remote::{
    self, ControllerUrl, LoginRequest, RemoteError, RemoteStore, Revocation, TrustProbe,
};
use pm_client::{Client, ClientError, Remote, Target};
use pm_daemon::{Daemon, DaemonConfig};
use pm_protocol::domain::{AgentKind, ClientMsg, PermissionMode, Scope, ServerMsg, Snapshot};
use pm_tls::WebTrust;
use support::{test_registry, TEST_TIMEOUT};

const USERNAME: &str = "testuser";
const PASSWORD: &str = "hunter2hunter2";
const DEVICE_NAME: &str = "pm on test-host";

struct Env {
    url: ControllerUrl,
    daemon: Arc<Daemon>,
    user_id: u64,
    store: RemoteStore,
    _handle: pm_daemon::ServerHandle,
    tmp: tempfile::TempDir,
}

async fn env_with_tls(tls: bool) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let http_tls = tls.then(|| {
        let (cert, key) = pm_tls::self_signed_web_cert_pem(&["127.0.0.1".to_string()]).unwrap();
        let cert_chain = tmp.path().join("cert.pem");
        let key_path = tmp.path().join("key.pem");
        std::fs::write(&cert_chain, cert).unwrap();
        std::fs::write(&key_path, key).unwrap();
        pm_daemon::HttpTls {
            cert_chain,
            key: key_path,
        }
    });
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: None,
        public_url: None,
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let session = daemon.auth_setup(USERNAME, PASSWORD).unwrap();
    let (user_id, _) = daemon.auth_verify_user(&session).unwrap();
    let scheme = if tls { "https" } else { "http" };
    let url = ControllerUrl::parse(&format!("{scheme}://{}", handle.http_addr.unwrap())).unwrap();
    Env {
        url,
        daemon,
        user_id,
        store: RemoteStore::new(tmp.path().join("remotes")),
        _handle: handle,
        tmp,
    }
}

async fn env() -> Env {
    env_with_tls(false).await
}

async fn login_as(
    env: &Env,
    name: &str,
    password: &str,
    trust: WebTrust,
) -> Result<(), RemoteError> {
    remote::login(
        &env.store,
        LoginRequest {
            name,
            url: &env.url,
            trust,
            username: USERNAME,
            password,
            device_name: DEVICE_NAME,
        },
    )
    .await
    .map(|_| ())
}

async fn login(env: &Env, name: &str) -> Target {
    login_as(env, name, PASSWORD, WebTrust::SystemRoots)
        .await
        .unwrap();
    target(env, name)
}

fn target(env: &Env, name: &str) -> Target {
    Target::Remote(Remote {
        store: env.store.clone(),
        name: name.to_string(),
    })
}

async fn snapshot(client: &mut Client) -> Snapshot {
    client
        .request(ClientMsg::Subscribe { scope: Scope::All })
        .await
        .unwrap();
    loop {
        match tokio::time::timeout(TEST_TIMEOUT, client.next_msg())
            .await
            .unwrap()
        {
            Some(ServerMsg::Snapshot(snapshot)) => return snapshot,
            Some(_) => continue,
            None => panic!("connection closed before the snapshot"),
        }
    }
}

/// Reads terminal output until it contains `needle`, returning whether
/// each message read was marked as replay.
async fn output_until(client: &mut Client, terminal_id: u64, needle: &[u8]) -> Vec<bool> {
    let mut seen = Vec::new();
    let mut replay_marks = Vec::new();
    loop {
        let msg = tokio::time::timeout(TEST_TIMEOUT, client.next_msg())
            .await
            .expect("terminal output did not arrive")
            .expect("connection closed while reading terminal output");
        if let ServerMsg::PtyOutput {
            terminal_id: from,
            data,
            replay,
            ..
        } = msg
        {
            assert_eq!(from, terminal_id);
            seen.extend_from_slice(&data);
            replay_marks.push(replay);
            if seen.windows(needle.len()).any(|window| window == needle) {
                return replay_marks;
            }
        }
    }
}

fn spawn_session(env: &Env) -> (u64, pm_protocol::domain::Terminal) {
    let bucket = env.daemon.create_bucket("b").unwrap();
    let project = env
        .daemon
        .create_project(bucket, "p", env.tmp.path().to_str().unwrap())
        .unwrap();
    let session_id = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let terminal = env
        .daemon
        .subscribe()
        .0
        .terminals
        .into_iter()
        .find(|terminal| terminal.session_id == session_id)
        .unwrap();
    (session_id, terminal)
}

#[tokio::test]
async fn a_login_carries_commands_and_the_snapshot_like_the_unix_socket() {
    let env = env().await;
    let target = login(&env, "prod").await;

    let devices = env.daemon.list_mobile_devices(env.user_id).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, DEVICE_NAME);
    assert_eq!(devices[0].platform, "cli");

    let mut client = Client::open(&target).await.unwrap();
    let bucket_id = client
        .request(ClientMsg::CreateBucket {
            name: "from-remote".into(),
            allowed_worker_ids: vec![pm_protocol::domain::LOCAL_WORKER_ID],
            default_worker_id: pm_protocol::domain::LOCAL_WORKER_ID,
            is_default: false,
        })
        .await
        .unwrap()
        .expect("creating a bucket returns its id");
    let snapshot = snapshot(&mut client).await;
    assert!(snapshot
        .buckets
        .iter()
        .any(|b| b.id == bucket_id && b.name == "from-remote"));

    let error = client
        .request(ClientMsg::KillSession { session_id: 9999 })
        .await
        .unwrap_err();
    assert!(matches!(error, ClientError::Daemon(_)), "{error:?}");
}

#[tokio::test]
async fn a_wrong_password_saves_nothing() {
    let env = env().await;
    let error = login_as(&env, "prod", "not-the-password", WebTrust::SystemRoots)
        .await
        .unwrap_err();
    assert!(matches!(error, RemoteError::InvalidCredentials), "{error}");
    assert!(env.store.names().unwrap().is_empty());
    assert!(env
        .daemon
        .list_mobile_devices(env.user_id)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn signing_in_again_under_a_name_keeps_one_device() {
    let env = env().await;
    login(&env, "prod").await;
    login(&env, "prod").await;
    assert_eq!(
        env.daemon.list_mobile_devices(env.user_id).unwrap().len(),
        1
    );
    login(&env, "second").await;
    assert_eq!(
        env.daemon.list_mobile_devices(env.user_id).unwrap().len(),
        2
    );
}

#[tokio::test]
async fn an_expired_access_token_is_rotated_once_however_many_commands_race() {
    let env = env().await;
    let target = login(&env, "prod").await;
    let mut stale = env.store.load("prod").unwrap();
    stale.access_token_expires_at_unix_ms = 0;
    env.store.save("prod", &stale).unwrap();

    const RACING_COMMANDS: usize = 8;
    let racing = (0..RACING_COMMANDS).map(|_| {
        let target = target.clone();
        tokio::spawn(async move {
            let mut client = Client::open(&target).await.unwrap();
            snapshot(&mut client).await;
        })
    });
    for command in racing.collect::<Vec<_>>() {
        command.await.unwrap();
    }

    let rotated = env.store.load("prod").unwrap();
    assert_ne!(rotated.access_token, stale.access_token);
    assert_ne!(rotated.refresh_token, stale.refresh_token);
    // A refresh token presented by more than one of the racing commands
    // would have revoked the device, and the next command would be refused.
    let mut client = Client::open(&target).await.unwrap();
    snapshot(&mut client).await;
    assert_eq!(
        env.daemon.list_mobile_devices(env.user_id).unwrap().len(),
        1
    );
}

#[tokio::test]
async fn a_revoked_login_is_reported_as_one_and_not_as_a_dropped_connection() {
    let env = env().await;
    let target = login(&env, "prod").await;
    let device_id = env.daemon.list_mobile_devices(env.user_id).unwrap()[0].id;
    env.daemon
        .revoke_mobile_device(env.user_id, device_id)
        .unwrap();

    let client = Client::open(&target).await.unwrap();
    let error = tokio::time::timeout(
        TEST_TIMEOUT,
        client.request(ClientMsg::Subscribe { scope: Scope::All }),
    )
    .await
    .unwrap()
    .unwrap_err();
    let ClientError::Unauthenticated(message) = error else {
        panic!("expected a refused login, got {error:?}");
    };
    assert!(message.contains("pm login"), "{message}");
}

#[tokio::test]
async fn a_name_that_was_never_signed_in_says_how_to_sign_in() {
    let env = env().await;
    let error = Client::open(&target(&env, "prod")).await.err().unwrap();
    assert!(error.to_string().contains("pm login"), "{error}");
}

#[tokio::test]
async fn a_terminal_attaches_replays_and_takes_input_over_a_login() {
    let env = env().await;
    let target = login(&env, "prod").await;
    let (_, terminal) = spawn_session(&env);

    let mut client = Client::open(&target).await.unwrap();
    client
        .request(ClientMsg::AttachTerminal {
            terminal_id: terminal.id,
        })
        .await
        .unwrap();
    client
        .send(ClientMsg::TerminalResize {
            terminal_id: terminal.id,
            cols: 101,
            rows: 37,
        })
        .unwrap();
    client
        .send(ClientMsg::TerminalInput {
            terminal_id: terminal.id,
            data: "echo typed-over-a-login\n".into(),
        })
        .unwrap();
    let marks = output_until(&mut client, terminal.id, b"OUT typed-over-a-login").await;
    assert_eq!(marks.first(), Some(&true), "the replay arrives first");
    assert_eq!(marks.last(), Some(&false), "live output follows the replay");

    client
        .request(ClientMsg::DetachTerminal {
            terminal_id: terminal.id,
        })
        .await
        .unwrap();
    // The control socket outlives the terminal's.
    snapshot(&mut client).await;
}

#[tokio::test]
async fn a_session_attaches_by_its_id_to_its_agent_terminal() {
    let env = env().await;
    let target = login(&env, "prod").await;
    let (session_id, terminal) = spawn_session(&env);

    let mut client = Client::open(&target).await.unwrap();
    client
        .request(ClientMsg::AttachPty { session_id })
        .await
        .unwrap();
    client
        .send(ClientMsg::PtyInput {
            session_id,
            data: "echo by-session-id\n".into(),
        })
        .unwrap();
    output_until(&mut client, terminal.id, b"OUT by-session-id").await;
}

#[tokio::test]
async fn attaching_a_terminal_that_does_not_exist_fails_the_request() {
    let env = env().await;
    let target = login(&env, "prod").await;
    let client = Client::open(&target).await.unwrap();
    let error = tokio::time::timeout(
        TEST_TIMEOUT,
        client.request(ClientMsg::AttachTerminal { terminal_id: 4242 }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        matches!(&error, ClientError::Daemon(message) if message.contains("4242")),
        "{error:?}"
    );
}

#[tokio::test]
async fn logging_out_revokes_the_device_and_forgets_the_login() {
    let env = env().await;
    login(&env, "prod").await;
    assert_eq!(
        remote::logout(&env.store, "prod").await.unwrap(),
        Revocation::Revoked
    );
    assert!(env.store.names().unwrap().is_empty());
    assert!(env
        .daemon
        .list_mobile_devices(env.user_id)
        .unwrap()
        .is_empty());
    assert!(matches!(
        remote::logout(&env.store, "prod").await,
        Err(RemoteError::NotLoggedIn(_))
    ));
}

#[tokio::test]
async fn an_untrusted_certificate_is_reached_only_through_its_pinned_key() {
    let env = env_with_tls(true).await;
    let TrustProbe::Untrusted { key } = remote::probe_trust(&env.url).await.unwrap() else {
        panic!("a self-signed certificate must not be trusted by system roots");
    };

    let error = login_as(&env, "prod", PASSWORD, WebTrust::SystemRoots)
        .await
        .unwrap_err();
    assert!(matches!(error, RemoteError::Unreachable { .. }), "{error}");
    assert!(env
        .daemon
        .list_mobile_devices(env.user_id)
        .unwrap()
        .is_empty());

    login_as(&env, "prod", PASSWORD, WebTrust::Pinned(key))
        .await
        .unwrap();
    assert_eq!(
        env.store.load("prod").unwrap().pinned_key,
        Some(key.to_hex())
    );
    let mut client = Client::open(&target(&env, "prod")).await.unwrap();
    snapshot(&mut client).await;

    // A different key at the same address is a different server, whatever
    // it says about itself.
    let mut moved = env.store.load("prod").unwrap();
    moved.pinned_key = Some(pm_tls::Identity::generate().unwrap().key_hash().to_hex());
    env.store.save("prod", &moved).unwrap();
    let error = Client::open(&target(&env, "prod")).await.err().unwrap();
    assert!(error.to_string().contains("not trusted"), "{error}");
}

#[tokio::test]
async fn a_plaintext_listener_has_no_certificate_to_judge() {
    let env = env().await;
    assert_eq!(
        remote::probe_trust(&env.url).await.unwrap(),
        TrustProbe::Plaintext
    );
}
