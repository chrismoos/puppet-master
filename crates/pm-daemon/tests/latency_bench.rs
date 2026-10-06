//! Terminal-latency baselines. Each layer of the keystroke->render path
//! is measured in isolation so a regression in the parts we control (the
//! server core and the transport) is visible against a recorded number.
//! These are `#[ignore]` (timing, machine-dependent) — run explicitly:
//!
//!   cargo test -p pm-daemon --test latency_bench -- --ignored --nocapture
//!
//! The browser terms (protobuf-es decode + xterm.js paint) are not
//! covered here; xterm paints once per requestAnimationFrame, an ~8ms
//! (60Hz) / ~4ms (120Hz) floor set by the display, not our code.

mod support;

use std::time::{Duration, Instant};

use bytes::Bytes;
use pm_client::Client;
use pm_daemon::mux::Mux;
use pm_protocol::domain::{AgentKind, ClientMsg, ServerMsg};
use support::test_registry;

const MANY_SHELL_COUNT: usize = 32;

/// Prints percentiles for a set of microsecond samples under `label`.
fn report(label: &str, mut samples: Vec<u128>) {
    samples.sort_unstable();
    let pct = |p: f64| samples[((samples.len() as f64 * p) as usize).min(samples.len() - 1)];
    let mean = samples.iter().sum::<u128>() / samples.len() as u128;
    println!(
        "{label}: n={} mean={}us p50={}us p90={}us p99={}us max={}us",
        samples.len(),
        mean,
        pct(0.50),
        pct(0.90),
        pct(0.99),
        *samples.last().unwrap()
    );
}

/// Server core: input -> PTY -> tty echo -> broadcast, no transport.
#[test]
#[ignore]
fn mux_echo_rtt() {
    let (mux, _channels) = Mux::new();
    let spec = pm_adapters::CommandSpec {
        program: "/bin/cat".into(),
        args: vec![],
        env: Vec::new(),
        cwd: std::env::temp_dir(),
    };
    mux.spawn(1, 1, 7, &spec, false, false, true, None).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let (_replay, mut rx) = mux.attach(1).unwrap();

    let mut samples = Vec::new();
    for i in 0..500u32 {
        while rx.try_recv().is_ok() {}
        // The tty echoes "\n" as "\r\n", so match just the id substring.
        let needle = format!("m{i}");
        let t0 = Instant::now();
        mux.input(1, Bytes::from(format!("{needle}\n"))).unwrap();
        loop {
            match rx.blocking_recv() {
                Ok(d) if d.windows(needle.len()).any(|w| w == needle.as_bytes()) => {
                    samples.push(t0.elapsed().as_micros());
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    report("mux_echo_rtt (server core)", samples);
    mux.kill(1).ok();
}

/// Full transport: pm-client -> unix socket -> daemon -> PTY (scripted
/// agent echoes a line) -> broadcast -> socket -> client. The delta over
/// mux_echo_rtt is the protobuf encode/decode + framing + task-hop cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn socket_echo_rtt() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("pm.sock");
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: socket_path.clone(),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (_daemon, _handle) = pm_daemon::start(config).await.unwrap();
    let mut client = Client::connect(&socket_path).await.unwrap();

    let project = tempfile::tempdir().unwrap();
    let bucket = client
        .request(ClientMsg::CreateBucket {
            name: "b".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();
    let project_id = client
        .request(ClientMsg::CreateProject {
            bucket_id: bucket,
            name: "p".into(),
            path: project.path().display().to_string(),
            worker_id: None,
            allowed_worker_ids: vec![0],
        })
        .await
        .unwrap()
        .unwrap();
    let session_id = client
        .request(ClientMsg::SpawnSession {
            project_id,
            agent: Some(AgentKind::Test),
            task_title: "t".into(),
            task_prompt: "bench".into(),
            cwd: String::new(),
            permission_mode: pm_protocol::domain::PermissionMode::Inherit,
            worker_id: None,
            items_api: true,
            supervisor_api: false,
            model_profile_id: None,
            host: String::new(),
            initial_cols: None,
            initial_rows: None,
        })
        .await
        .unwrap()
        .unwrap();
    client
        .request(ClientMsg::AttachPty { session_id })
        .await
        .unwrap();
    read_until(&mut client, "READY bench").await;

    let mut samples = Vec::new();
    for i in 0..300u32 {
        let needle = format!("OUT m{i}");
        let t0 = Instant::now();
        client
            .send(ClientMsg::PtyInput {
                session_id,
                data: Bytes::from(format!("echo m{i}\n")),
            })
            .unwrap();
        read_until(&mut client, &needle).await;
        samples.push(t0.elapsed().as_micros());
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    report("socket_echo_rtt (full transport)", samples);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn many_shell_lifecycle_latency() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("pm.sock");
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: socket_path.clone(),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("scrollback"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (_daemon, _handle) = pm_daemon::start(config).await.unwrap();
    let client = Client::connect(&socket_path).await.unwrap();
    let bucket_id = client
        .request(ClientMsg::CreateBucket {
            name: "b".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();
    let project_id = client
        .request(ClientMsg::CreateProject {
            bucket_id,
            name: "p".into(),
            path: tmp.path().display().to_string(),
            worker_id: None,
            allowed_worker_ids: vec![0],
        })
        .await
        .unwrap()
        .unwrap();
    let session_id = client
        .request(ClientMsg::SpawnSession {
            project_id,
            agent: Some(AgentKind::Test),
            task_title: "t".into(),
            task_prompt: "many-shells".into(),
            cwd: String::new(),
            permission_mode: pm_protocol::domain::PermissionMode::Inherit,
            worker_id: None,
            items_api: true,
            supervisor_api: false,
            model_profile_id: None,
            host: String::new(),
            initial_cols: None,
            initial_rows: None,
        })
        .await
        .unwrap()
        .unwrap();

    let mut shell_ids = Vec::with_capacity(MANY_SHELL_COUNT);
    let mut create_samples = Vec::with_capacity(MANY_SHELL_COUNT);
    for _ in 0..MANY_SHELL_COUNT {
        let started = Instant::now();
        let terminal_id = client
            .request(ClientMsg::CreateShell {
                session_id,
                title: String::new(),
            })
            .await
            .unwrap()
            .unwrap();
        create_samples.push(started.elapsed().as_micros());
        shell_ids.push(terminal_id);
    }
    report("create_shell with growing inventory", create_samples);

    let started = Instant::now();
    client
        .request(ClientMsg::AttachPty { session_id })
        .await
        .unwrap();
    println!(
        "agent attach after {MANY_SHELL_COUNT} shells: {}us",
        started.elapsed().as_micros()
    );

    let mut close_samples = Vec::with_capacity(MANY_SHELL_COUNT);
    for terminal_id in shell_ids {
        let started = Instant::now();
        client
            .request(ClientMsg::CloseTerminal { terminal_id })
            .await
            .unwrap();
        close_samples.push(started.elapsed().as_micros());
    }
    report("close_shell with shrinking inventory", close_samples);
}

async fn read_until(client: &mut Client, needle: &str) {
    let mut acc = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ServerMsg::PtyOutput { data, .. }) = client.next_msg().await {
                acc.extend_from_slice(&data);
                if String::from_utf8_lossy(&acc).contains(needle) {
                    return;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}"));
}
