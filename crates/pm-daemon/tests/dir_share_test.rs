//! Published directories: the share a session owns, the forward that
//! carries it, and what happens to both across a stop and a rebind.

mod support;

use support::*;
use tower::ServiceExt;

const PUBLIC_URL: &str = "https://controller.example:8443";

/// The dashboard credential these tests reach the forward with. A bearer token
/// rather than a cookie, because the cookie stopped authenticating anything but
/// the mint that produces this.
fn setup_user(env: &TestEnv) -> String {
    support::signed_in_bearer(&env.daemon)
}

/// Writes an artifact directory under the session's working directory,
/// which is the project root every test session is spawned in.
fn artifacts(env: &TestEnv, name: &str) -> std::path::PathBuf {
    let dir = env.project_root().join(name);
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(dir.join("report.html"), "<p>the report</p>").unwrap();
    std::fs::write(dir.join("nested/data.json"), "{\"ok\":true}").unwrap();
    std::fs::write(dir.join(".env"), "SECRET=1").unwrap();
    dir
}

async fn get(env: &TestEnv, uri: &str, auth: &str) -> (axum::http::StatusCode, String) {
    let response = pm_daemon::http::router(env.daemon.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .header(axum::http::header::AUTHORIZATION, auth)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Publishes one directory from a session of its own. Slugs are unique
/// across the controller, so each call takes a fresh one.
async fn publish(env: &TestEnv, path: &str) -> (u64, String, pm_protocol::domain::SessionForward) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let slug = format!(
        "share-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let session = spawn_test_session(env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let forward = env
        .daemon
        .publish_dir(&token, path, &slug, "artifacts")
        .await
        .unwrap();
    (session, token, forward)
}

/// A published directory is reached only by its name, so a session the
/// user has shut down keeps nothing worth having: the share closes and the
/// next session can publish under that name.
#[tokio::test]
async fn killing_a_session_closes_its_published_directory_and_frees_the_slug() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    artifacts(&env, "out");
    let (session, _token, forward) = publish(&env, "out").await;
    let slug = forward.slug.clone();

    env.daemon.kill_session(session).unwrap();

    let still_there = env
        .daemon
        .subscribe()
        .0
        .forwards
        .into_iter()
        .any(|f| f.id == forward.id);
    assert!(
        !still_there,
        "the share's forward goes with the session that published it"
    );

    let next = spawn_test_session(&env, "next");
    let token = env.daemon.session_token(next).unwrap().unwrap();
    let again = env
        .daemon
        .publish_dir(&token, "out", &slug, "artifacts")
        .await
        .unwrap();
    assert_eq!(again.slug, slug);
    assert_eq!(
        again.session_id, next,
        "the name belongs to the session that took it"
    );
}

#[tokio::test]
async fn a_published_directory_serves_its_files_through_the_forward() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "out");
    let (_session, _token, forward) = publish(&env, "out").await;

    assert_eq!(
        forward.url,
        format!("https://controller.example:8443/forwards/{}/", forward.id)
    );
    assert_eq!(forward.source_path, "out");
    assert!(forward.worker_port != 0, "a served share has a bound port");

    let (status, body) = get(
        &env,
        &format!("/forwards/{}/report.html", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "<p>the report</p>");

    let (status, body) = get(
        &env,
        &format!("/forwards/{}/nested/data.json", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "{\"ok\":true}");
}

#[tokio::test]
async fn published_text_declares_utf8_and_preserves_unicode_for_get_and_head() {
    use axum::http::{header, Method, Request, StatusCode};

    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let dir = artifacts(&env, "utf8");
    let text = "Puppet Master is MIT licensed — café 日本語 🎉";
    let files = [
        ("report.html", "text/html", format!("<p>{text}</p>")),
        ("notes.md", "text/markdown", text.to_string()),
        ("notes.txt", "text/plain", text.to_string()),
    ];
    for (name, _, body) in &files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    let (_session, _token, forward) = publish(&env, "utf8").await;

    for (name, media_type, body) in files {
        for method in [Method::GET, Method::HEAD] {
            let response = pm_daemon::http::router(env.daemon.clone())
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(format!("/forwards/{}/{name}", forward.id))
                        .header(header::AUTHORIZATION, &auth)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                format!("{media_type}; charset=utf-8")
            );
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                body.len().to_string()
            );
            let received = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            if method == Method::HEAD {
                assert!(received.is_empty());
            } else {
                assert_eq!(received.as_ref(), body.as_bytes());
            }
        }
    }
}

#[tokio::test]
async fn published_binary_preserves_its_content_type_and_bytes() {
    use axum::http::{header, Request, StatusCode};

    let env = daemon_env_with_public_url(PUBLIC_URL);
    let auth = setup_user(&env);
    let dir = artifacts(&env, "binary");
    let data = b"\0\xff\xfe\x80";
    std::fs::write(dir.join("data.bin"), data).unwrap();
    let (_session, _token, forward) = publish(&env, "binary").await;
    let response = pm_daemon::http::router(env.daemon.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/forwards/{}/data.bin", forward.id))
                .header(header::AUTHORIZATION, &auth)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    let received = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(received.as_ref(), data);
}

#[tokio::test]
async fn a_directory_without_an_index_lists_its_files_with_relative_links() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "listing");
    let (_session, _token, forward) = publish(&env, "listing").await;

    let (status, body) = get(&env, &format!("/forwards/{}/", forward.id), &cookie).await;
    assert_eq!(status, 200);
    assert!(body.contains("href=\"report.html\""), "{body}");
    assert!(body.contains("href=\"nested/\""), "{body}");
    assert!(
        !body.contains(".env"),
        "the listing must not name dot-files: {body}"
    );
    assert!(
        !body.contains("href=\"/"),
        "a listing link must stay inside the mount: {body}"
    );
}

#[tokio::test]
async fn an_index_html_is_served_at_the_share_root() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    let dir = artifacts(&env, "site");
    std::fs::write(dir.join("index.html"), "<h1>home</h1>").unwrap();
    let (_session, _token, forward) = publish(&env, "site").await;

    let (status, body) = get(&env, &format!("/forwards/{}/", forward.id), &cookie).await;
    assert_eq!(status, 200);
    assert_eq!(body, "<h1>home</h1>");
}

#[tokio::test]
async fn dot_files_are_not_served_even_when_asked_for_by_name() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "secrets");
    let (_session, _token, forward) = publish(&env, "secrets").await;

    let (status, _) = get(&env, &format!("/forwards/{}/.env", forward.id), &cookie).await;
    assert_eq!(status, 404);
}

/// What a by-name refusal cannot do, through the real serving path.
///
/// The documented promise was that key material is never served. The
/// implementation was a list of names, and a probe against it served
/// pm_shared_key, AuthKey_*.p8, secrets.yaml, service-account.json, *.der,
/// *.ppk and *.kdbx. pm_shared_key is not a hypothetical: a file of exactly
/// that name, holding an OpenSSH private key, sits in the directory sessions on
/// this project run from.
///
/// Adding those names would have left out the next one, so the check asks the
/// bytes. A key under a name nobody would think to refuse is the case that
/// settles which shape is right.
#[tokio::test]
async fn a_key_is_not_served_whatever_it_is_called() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    let dir = artifacts(&env, "out");
    // The real file's own first line, under three names: one the old list knew,
    // one it did not, and one no list would ever carry.
    let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
    for name in ["server.key", "pm_shared_key", "report.txt"] {
        std::fs::write(dir.join(name), key).unwrap();
    }
    std::fs::write(dir.join("notes.txt"), "an ordinary artifact").unwrap();
    let (_session, _token, forward) = publish(&env, "out").await;

    for name in ["server.key", "pm_shared_key", "report.txt"] {
        let (status, _) = get(&env, &format!("/forwards/{}/{name}", forward.id), &cookie).await;
        assert_eq!(status, 404, "{name} was served");
    }

    // And the share still works, or the check would have been removed.
    let (status, body) = get(
        &env,
        &format!("/forwards/{}/notes.txt", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "an ordinary artifact");
    let (status, _) = get(
        &env,
        &format!("/forwards/{}/report.html", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn publishing_refuses_a_path_outside_the_session_working_directory() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();

    let error = env
        .daemon
        .publish_dir(&token, "../elsewhere", "escape", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("inside the session working directory"),
        "{error}"
    );

    let error = env
        .daemon
        .publish_dir(&token, "/etc", "absolute", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("inside the session working directory"),
        "{error}"
    );
}

#[tokio::test]
async fn publishing_refuses_a_repository_root_and_says_what_to_do_instead() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let dir = artifacts(&env, "repo");
    std::fs::create_dir(dir.join(".git")).unwrap();
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();

    let error = env
        .daemon
        .publish_dir(&token, "repo", "repo-root", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("git repository"), "{error}");
    assert!(error.contains("subdirectory"), "{error}");
}

#[tokio::test]
async fn a_refused_publish_leaves_no_share_and_frees_its_slug() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();

    assert!(env
        .daemon
        .publish_dir(&token, "missing", "retry-me", "")
        .await
        .is_err());
    assert!(env.daemon.list_dir_shares(&token).unwrap().is_empty());

    artifacts(&env, "later");
    assert!(env
        .daemon
        .publish_dir(&token, "later", "retry-me", "")
        .await
        .is_ok());
}

#[tokio::test]
async fn a_share_and_a_port_cannot_hold_the_same_slug() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    artifacts(&env, "taken");
    let (_session, token, forward) = publish(&env, "taken").await;

    let other = spawn_test_session(&env, "p");
    let other_token = env.daemon.session_token(other).unwrap().unwrap();
    let error = env
        .daemon
        .publish_port(&other_token, 5173, &forward.slug, "vite", "http")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("already published"), "{error}");

    env.daemon
        .publish_port(&token, 5173, "a-port", "vite", "http")
        .await
        .unwrap();
    artifacts(&env, "other");
    let error = env
        .daemon
        .publish_dir(&other_token, "other", "a-port", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("already published"), "{error}");
}

#[tokio::test]
async fn one_session_cannot_publish_the_same_directory_twice() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    artifacts(&env, "once");
    let (_session, token, _forward) = publish(&env, "once").await;

    let error = env
        .daemon
        .publish_dir(&token, "once", "again", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("already shares"), "{error}");
}

#[tokio::test]
async fn unpublish_port_refuses_a_share_and_names_the_tool_that_closes_it() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    artifacts(&env, "wrong-tool");
    let (_session, token, forward) = publish(&env, "wrong-tool").await;

    let error = env
        .daemon
        .unpublish_port(&token, forward.worker_port)
        .unwrap_err()
        .to_string();
    assert!(error.contains("unpublish_dir"), "{error}");
    assert!(error.contains(&forward.slug), "{error}");
    assert_eq!(env.daemon.list_dir_shares(&token).unwrap().len(), 1);
}

#[tokio::test]
async fn unpublishing_closes_the_url_and_leaves_the_files_alone() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    let dir = artifacts(&env, "closing");
    let (_session, token, forward) = publish(&env, "closing").await;

    env.daemon.unpublish_dir(&token, &forward.slug).unwrap();

    let (status, _) = get(
        &env,
        &format!("/forwards/{}/report.html", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 404);
    assert!(env.daemon.list_dir_shares(&token).unwrap().is_empty());
    assert!(
        dir.join("report.html").exists(),
        "the files are not ours to delete"
    );
}

/// The dashboard closes a share with the same button it closes a
/// forward with, so that has to close the share and not just its
/// forward, or the share would outlive the URL with a server still up.
#[tokio::test]
async fn closing_a_shares_forward_closes_the_share() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "close-button");
    let (_session, token, forward) = publish(&env, "close-button").await;

    env.daemon.close_forward(forward.id).unwrap();

    assert!(
        env.daemon.list_dir_shares(&token).unwrap().is_empty(),
        "the share record goes with its forward"
    );
    let (status, _) = get(
        &env,
        &format!("/forwards/{}/report.html", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 404);
    // The slug is free again, which it would not be if a row survived.
    artifacts(&env, "reuse");
    assert!(env
        .daemon
        .publish_dir(&token, "reuse", &forward.slug, "")
        .await
        .is_ok());
}

#[tokio::test]
async fn unpublishing_refuses_another_sessions_share() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    artifacts(&env, "mine");
    let (_session, _token, forward) = publish(&env, "mine").await;
    let other = spawn_test_session(&env, "p");
    let other_token = env.daemon.session_token(other).unwrap().unwrap();

    let error = env
        .daemon
        .unpublish_dir(&other_token, &forward.slug)
        .unwrap_err()
        .to_string();
    assert!(error.contains("publishes no directory"), "{error}");
}

#[tokio::test]
async fn listing_reports_each_share_with_its_path_and_url() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    artifacts(&env, "first");
    artifacts(&env, "second");
    let (_session, token, forward) = publish(&env, "first").await;
    env.daemon
        .publish_dir(&token, "second", "share-second", "more")
        .await
        .unwrap();

    let shares = env.daemon.list_dir_shares(&token).unwrap();
    assert_eq!(shares.len(), 2);
    assert_eq!(shares[0].0.path, "first");
    assert_eq!(shares[0].1.url, forward.url);
    assert_eq!(shares[1].0.path, "second");
    assert!(shares[1].1.url.ends_with('/'), "{:?}", shares[1].1.url);
}

#[tokio::test]
async fn a_stop_takes_the_share_down_and_a_resume_brings_the_same_url_back() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "lifecycle");
    let (session, _token, forward) = publish(&env, "lifecycle").await;

    env.daemon.unbind_session_forwards(session);
    env.daemon.stop_session_dir_shares(session);
    let (status, _) = get(
        &env,
        &format!("/forwards/{}/report.html", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 503, "a stopped session stops serving its share");

    env.daemon.start_session_dir_shares(session).await;
    let (status, body) = get(
        &env,
        &format!("/forwards/{}/report.html", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 200, "the same URL works again after a resume");
    assert_eq!(body, "<p>the report</p>");

    let restored = env
        .daemon
        .list_dir_shares(&env.daemon.session_token(session).unwrap().unwrap())
        .unwrap();
    assert_eq!(restored[0].1.id, forward.id, "the forward id is in the URL");
    assert_eq!(restored[0].1.url, forward.url);
}

#[tokio::test]
async fn a_rebind_moves_the_port_and_keeps_the_forward() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    artifacts(&env, "rebind");
    let (session, token, forward) = publish(&env, "rebind").await;

    env.daemon.stop_session_dir_shares(session);
    env.daemon.unbind_session_forwards(session);
    env.daemon.start_session_dir_shares(session).await;

    let restored = env.daemon.list_dir_shares(&token).unwrap()[0].1.clone();
    assert_eq!(restored.id, forward.id);
    assert_eq!(restored.url, forward.url);
    assert_eq!(restored.slug, forward.slug);
    assert_ne!(
        restored.worker_port, 0,
        "a restored share answers on a port again"
    );
}

/// A share whose server is still up keeps that server, and the port the
/// forward already dials, rather than being served a second time. The
/// second server would displace the first and abort it.
#[tokio::test]
async fn a_rebind_with_the_server_still_up_keeps_its_port() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "still-up");
    let (session, token, forward) = publish(&env, "still-up").await;

    // Only the listener goes, which is what a forward reconciler pass
    // leaves behind when the session drops out of a live state and
    // comes back before its server is torn down.
    env.daemon.unbind_session_forwards(session);
    env.daemon.start_session_dir_shares(session).await;

    let restored = env.daemon.list_dir_shares(&token).unwrap()[0].1.clone();
    assert_eq!(restored.id, forward.id);
    assert_eq!(
        restored.worker_port, forward.worker_port,
        "the share answers on the port it was already serving"
    );
    let (status, body) = get(
        &env,
        &format!("/forwards/{}/report.html", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "<p>the report</p>");
}

#[tokio::test]
async fn the_resume_inventory_omits_published_directories() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    artifacts(&env, "artifacts-dir");
    let (_session, token, share) = publish(&env, "artifacts-dir").await;

    let inventory = env.daemon.forward_inventory_for_token(&token).unwrap();
    assert!(
        inventory.is_empty(),
        "a share has no server of the agent's to restart: {inventory}"
    );

    env.daemon
        .publish_port(&token, 5173, "a-dev-server", "vite", "http")
        .await
        .unwrap();
    let inventory = env.daemon.forward_inventory_for_token(&token).unwrap();
    assert!(inventory.contains("a-dev-server"), "{inventory}");
    assert!(
        !inventory.contains(&share.slug) && !inventory.contains("artifacts-dir"),
        "only the port forward is listed: {inventory}"
    );
}

#[tokio::test]
async fn a_share_survives_a_controller_restart() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "restart");
    let (session, _token, forward) = publish(&env, "restart").await;

    // What a restart leaves behind: the rows, and no server for them.
    env.daemon.stop_session_dir_shares(session);
    env.daemon.unbind_session_forwards(session);
    env.daemon.recover_local_dir_shares().await;

    let (status, body) = get(
        &env,
        &format!("/forwards/{}/report.html", forward.id),
        &cookie,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "<p>the report</p>");
}

/// A session on a remote worker, holding the worker's control channel so
/// a test answers its serve requests itself. The real worker is not in
/// the picture: what is under test is the controller's side of it.
struct RemoteShareEnv {
    _tmp: tempfile::TempDir,
    daemon: std::sync::Arc<pm_daemon::Daemon>,
    reg: pm_daemon::daemon::WorkerRegistration,
    worker_id: u64,
    session: u64,
    token: String,
    root: String,
}

async fn remote_share_env(protocol_version: u32) -> RemoteShareEnv {
    use pm_protocol::domain::{AgentKind, ControllerMsg, PermissionMode};

    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: None,
        public_url: Some(PUBLIC_URL.to_string()),
        http_addr: None,
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _channels) = pm_daemon::Daemon::new(config).unwrap();
    let daemon = std::sync::Arc::new(daemon);
    let identity = pm_tls::Identity::generate().unwrap();
    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();
    std::fs::create_dir(tmp.path().join("out")).unwrap();
    let (enroll, _) = daemon.create_worker_enrollment("laptop").unwrap();
    let mut reg = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enroll,
            credential: "",
            peer_key_hash: &identity.key_hash().to_hex(),
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version,
        })
        .unwrap();
    let worker_id = reg.worker_id;
    daemon
        .set_bucket_workers(bucket, &[0, worker_id], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project, &[worker_id], Some(worker_id))
        .unwrap();
    let session = daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
            .await
            .unwrap(),
        Some(ControllerMsg::Spawn { .. })
    ));
    let token = daemon.session_token(session).unwrap().unwrap();
    let root = tmp.path().to_string_lossy().into_owned();
    RemoteShareEnv {
        _tmp: tmp,
        daemon,
        reg,
        worker_id,
        session,
        token,
        root,
    }
}
fn answer_dir_share_serve(
    daemon: &std::sync::Arc<pm_daemon::Daemon>,
    worker_id: u64,
    share_id: u64,
    port: u16,
) {
    daemon.apply_worker_message(
        worker_id,
        pm_protocol::domain::WorkerMsg::DirShareBound {
            share_id,
            ok: true,
            error: String::new(),
            port,
        },
    );
}

/// Publishes `out` from the remote session and answers the worker's
/// serve request with `port`.
async fn publish_remote_dir(
    env: &mut RemoteShareEnv,
    slug: &str,
    port: u16,
) -> (u64, pm_protocol::domain::SessionForward) {
    use pm_protocol::domain::ControllerMsg;

    let publishing = {
        let daemon = env.daemon.clone();
        let token = env.token.clone();
        let slug = slug.to_string();
        tokio::spawn(async move { daemon.publish_dir(&token, "out", &slug, "").await })
    };
    let received = tokio::time::timeout(TEST_TIMEOUT, env.reg.rx.recv())
        .await
        .unwrap();
    let share_id = match received {
        Some(ControllerMsg::DirShareServe {
            share_id,
            root,
            path,
        }) => {
            assert_eq!(root, env.root);
            assert_eq!(path, "out");
            share_id
        }
        other => panic!("expected a serve request, got {other:?}"),
    };
    answer_dir_share_serve(&env.daemon, env.worker_id, share_id, port);
    (share_id, publishing.await.unwrap().unwrap())
}

/// The remote path: the worker binds the server, and on reconnect the
/// controller adopts what it still serves, moves the forward to the port
/// it reports, and stops anything no share claims.
#[tokio::test]
async fn a_reconnecting_worker_keeps_its_shares_and_is_told_to_drop_the_rest() {
    use pm_protocol::domain::{ControllerMsg, WorkerDirShare};

    let mut env = remote_share_env(pm_protocol::WORKER_PROTOCOL_VERSION).await;
    let (share_id, forward) = publish_remote_dir(&mut env, "remote-share", 44_100).await;
    assert_eq!(forward.worker_port, 44_100);
    assert_eq!(forward.source_path, "out");

    // Reconnecting: this share moved to a new port, and share 999 is one
    // the controller has no record of.
    env.daemon
        .reconcile_worker_dir_shares(
            env.worker_id,
            &[
                WorkerDirShare {
                    share_id,
                    port: 44_200,
                },
                WorkerDirShare {
                    share_id: 999,
                    port: 44_300,
                },
            ],
        )
        .await;

    let stops: Vec<u64> = std::iter::from_fn(|| env.reg.rx.try_recv().ok())
        .filter_map(|msg| match msg {
            ControllerMsg::DirShareStop { share_id } => Some(share_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        stops,
        vec![999],
        "a server no share claims must be stopped, and an adopted one left alone"
    );

    let adopted = env.daemon.list_dir_shares(&env.token).unwrap()[0].1.clone();
    assert_eq!(adopted.id, forward.id, "the forward in the URL is the same");
    assert_eq!(adopted.url, forward.url);
    assert_eq!(adopted.worker_port, 44_200, "it moved to the reported port");
}

/// A worker restart runs both of the controller's share-restore paths at
/// once: registration reconciles the shares it wants served, and every
/// session coming back live starts its own. Asking a worker to serve one
/// share twice over leaves two servers where it tracks one, and the
/// forward can end up holding the port of the one that went. So a share
/// is bound by one caller at a time, however many ask.
#[tokio::test]
async fn the_two_restore_paths_bind_a_share_one_at_a_time() {
    use pm_protocol::domain::ControllerMsg;

    const SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

    let mut env = remote_share_env(pm_protocol::WORKER_PROTOCOL_VERSION).await;
    let (share_id, forward) = publish_remote_dir(&mut env, "restart-share", 44_100).await;

    // What the restart leaves behind: the share rows, no listener, and a
    // worker announcing nothing, because its process is new.
    env.daemon.unbind_session_forwards(env.session);
    env.daemon.stop_session_dir_shares(env.session);
    while env.reg.rx.try_recv().is_ok() {}

    let reconciling = {
        let daemon = env.daemon.clone();
        let worker_id = env.worker_id;
        tokio::spawn(async move { daemon.reconcile_worker_dir_shares(worker_id, &[]).await })
    };
    let restarting = {
        let daemon = env.daemon.clone();
        let session = env.session;
        tokio::spawn(async move { daemon.start_session_dir_shares(session).await })
    };

    let received = tokio::time::timeout(TEST_TIMEOUT, env.reg.rx.recv())
        .await
        .unwrap();
    match received {
        Some(ControllerMsg::DirShareServe {
            share_id: asked, ..
        }) => assert_eq!(asked, share_id),
        other => panic!("expected a serve request, got {other:?}"),
    }
    tokio::time::sleep(SETTLE).await;
    assert!(
        env.reg.rx.try_recv().is_err(),
        "a share with a bind already in flight must not be asked to serve again"
    );

    // Answering releases whichever caller was waiting its turn, and it
    // asks in its own right. A worker serves a share it already holds on
    // the port it already holds, so every answer names the same one.
    answer_dir_share_serve(&env.daemon, env.worker_id, share_id, 44_400);
    loop {
        let received = tokio::time::timeout(SETTLE, env.reg.rx.recv()).await;
        match received {
            Ok(Some(ControllerMsg::DirShareServe {
                share_id: asked, ..
            })) => answer_dir_share_serve(&env.daemon, env.worker_id, asked, 44_400),
            Ok(other) => panic!("expected a serve request, got {other:?}"),
            Err(_) => break,
        }
    }
    tokio::time::timeout(TEST_TIMEOUT, reconciling)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(TEST_TIMEOUT, restarting)
        .await
        .unwrap()
        .unwrap();

    let restored = env.daemon.list_dir_shares(&env.token).unwrap()[0].1.clone();
    assert_eq!(
        restored.id, forward.id,
        "the forward in the URL is the same"
    );
    assert_eq!(
        restored.worker_port, 44_400,
        "the forward holds a port the worker is still serving on"
    );
}

/// A host too old to serve a directory is refused with that reason
/// rather than sent a message it cannot decode.
#[tokio::test]
async fn publishing_refuses_a_worker_that_cannot_serve_directories() {
    let mut env = remote_share_env(pm_protocol::WORKER_PROTOCOL_DIR_SHARE - 1).await;

    let error = env
        .daemon
        .publish_dir(&env.token, "out", "old-host", "")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("cannot serve a published directory"),
        "{error}"
    );
    assert!(error.contains("publish its port"), "{error}");
    assert!(
        env.daemon.list_dir_shares(&env.token).unwrap().is_empty(),
        "a refused publish records nothing"
    );
    assert!(
        env.reg.rx.try_recv().is_err(),
        "nothing is sent to a host that cannot decode it"
    );
}

/// Closing a share takes its server down but leaves the forward's route
/// registered, so nothing else here drops the connections the proxy
/// held to it. A reusable one would keep the directory readable.
#[tokio::test]
async fn closing_a_share_drops_the_upstream_connections_it_held() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "pooled");
    let (_session, token, forward) = publish(&env, "pooled").await;
    let page = format!("/forwards/{}/report.html", forward.id);

    assert_eq!(get(&env, &page, &cookie).await.0, 200);
    assert_eq!(
        env.daemon.idle_upstreams(forward.id),
        1,
        "a finished request leaves its connection reusable"
    );

    env.daemon.unpublish_dir(&token, &forward.slug).unwrap();
    assert_eq!(env.daemon.idle_upstreams(forward.id), 0);
    assert_eq!(get(&env, &page, &cookie).await.0, 404);
}

#[tokio::test]
async fn stopping_a_session_drops_its_shares_upstream_connections() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "stopped");
    let (session, _token, forward) = publish(&env, "stopped").await;
    let page = format!("/forwards/{}/report.html", forward.id);

    assert_eq!(get(&env, &page, &cookie).await.0, 200);
    assert_eq!(env.daemon.idle_upstreams(forward.id), 1);

    env.daemon.stop_session_dir_shares(session);
    assert_eq!(env.daemon.idle_upstreams(forward.id), 0);

    env.daemon.unbind_session_forwards(session);
    assert_eq!(get(&env, &page, &cookie).await.0, 503);

    env.daemon.start_session_dir_shares(session).await;
    assert_eq!(
        get(&env, &page, &cookie).await.0,
        200,
        "a resume serves the same URL again"
    );
}

/// Killing a session closes the directories it published, which has to
/// take the proxy's connections to them with it. A reusable connection
/// outliving the share would be a way into a directory the user has
/// shut down.
#[tokio::test]
async fn killing_a_session_drops_the_upstream_connections_to_its_share() {
    let env = daemon_env_with_public_url(PUBLIC_URL);
    let cookie = setup_user(&env);
    artifacts(&env, "killed");
    let (session, _token, forward) = publish(&env, "killed").await;
    let page = format!("/forwards/{}/report.html", forward.id);

    assert_eq!(get(&env, &page, &cookie).await.0, 200);
    assert_eq!(
        env.daemon.idle_upstreams(forward.id),
        1,
        "a finished request leaves its connection reusable"
    );

    env.daemon.kill_session(session).unwrap();

    assert_eq!(
        env.daemon.idle_upstreams(forward.id),
        0,
        "a killed session's share must leave no reusable connection"
    );
    assert_eq!(
        get(&env, &page, &cookie).await.0,
        404,
        "and the share is no longer reachable"
    );
}

/// A remote directory share with the worker plane actually running.
/// The stand-in worker does what `pm worker` does: resolves the share
/// root, binds a real directory server on it, reports the port, and
/// relays every forward stream the controller asks for. That makes the
/// whole remote path real, including which HTTP version the share's own
/// server speaks, which is the thing under test.
struct ServedRemoteShare {
    _tmp: tempfile::TempDir,
    daemon: std::sync::Arc<pm_daemon::Daemon>,
    auth: String,
    forward: pm_protocol::domain::SessionForward,
    token: String,
    opens: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    relay: tokio::task::JoinHandle<()>,
    /// Freezes every tunnel open at the moment it is notified.
    stall: std::sync::Arc<tokio::sync::Notify>,
    _server: pm_daemon::dir_server::DirShareServer,
}

impl ServedRemoteShare {
    fn forward_opens(&self) -> usize {
        self.opens.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn get(&self, path: &str) -> (axum::http::StatusCode, String) {
        use tower::ServiceExt;
        let request = axum::http::Request::builder()
            .uri(format!("/forwards/{}/{path}", self.forward.id))
            .header(axum::http::header::AUTHORIZATION, &self.auth)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tokio::time::timeout(
            TEST_TIMEOUT,
            pm_daemon::http::router(self.daemon.clone()).oneshot(request),
        )
        .await
        .expect("the remote share answers")
        .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }
}

async fn served_remote_share(protocol_version: u32, slug: &str) -> ServedRemoteShare {
    served_remote_share_with(protocol_version, slug, Default::default()).await
}

async fn served_remote_share_with(
    protocol_version: u32,
    slug: &str,
    forward: pm_daemon::forward::ForwardConfig,
) -> ServedRemoteShare {
    use pm_protocol::domain::{AgentKind, ControllerMsg, PermissionMode, WorkerMsg};
    use std::sync::atomic::AtomicUsize;

    let tmp = tempfile::tempdir().unwrap();
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward,
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: Some("127.0.0.1:0".parse().unwrap()),
        public_url: Some(PUBLIC_URL.to_string()),
        http_addr: None,
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let worker_addr = handle.worker_addr.unwrap();
    let identity = pm_tls::Identity::generate().unwrap();
    let session = daemon.auth_setup("testuser", "longenoughpassword").unwrap();
    let auth = support::dashboard_bearer(&daemon, &session);

    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("report.html"), "<p>the report</p>").unwrap();
    // Named so no list would catch it, and armoured so content has to answer.
    std::fs::write(
        out.join("notes.txt"),
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEA\n",
    )
    .unwrap();

    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();
    let (enroll, _) = daemon.create_worker_enrollment("laptop").unwrap();
    let mut reg = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enroll,
            credential: "",
            peer_key_hash: &identity.key_hash().to_hex(),
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version,
        })
        .unwrap();
    let worker_id = reg.worker_id;
    daemon
        .set_bucket_workers(bucket, &[0, worker_id], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project, &[worker_id], Some(worker_id))
        .unwrap();
    let session = daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            PermissionMode::Inherit,
            Some(worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
            .await
            .unwrap(),
        Some(ControllerMsg::Spawn { .. })
    ));
    let token = daemon.session_token(session).unwrap().unwrap();

    let publishing = {
        let daemon = daemon.clone();
        let token = token.clone();
        let slug = slug.to_string();
        tokio::spawn(async move { daemon.publish_dir(&token, "out", &slug, "artifacts").await })
    };
    let (share_id, root, path) = match tokio::time::timeout(TEST_TIMEOUT, reg.rx.recv())
        .await
        .unwrap()
    {
        Some(ControllerMsg::DirShareServe {
            share_id,
            root,
            path,
        }) => (share_id, root, path),
        other => panic!("expected a serve request, got {other:?}"),
    };
    // What `pm worker` does with that request.
    let resolved = pm_daemon::dir_server::resolve_share_root(&root, &path).unwrap();
    let server = pm_daemon::dir_server::serve(resolved).await.unwrap();
    daemon.apply_worker_message(
        worker_id,
        WorkerMsg::DirShareBound {
            share_id,
            ok: true,
            error: String::new(),
            port: server.port(),
        },
    );
    let forward = publishing.await.unwrap().unwrap();

    let opens = std::sync::Arc::new(AtomicUsize::new(0));
    let stall = std::sync::Arc::new(tokio::sync::Notify::new());
    let relay = tokio::spawn(relay_forward_opens(
        daemon.clone(),
        worker_id,
        reg.rx,
        worker_addr,
        identity.clone(),
        opens.clone(),
        Some(stall.clone()),
    ));

    ServedRemoteShare {
        _tmp: tmp,
        daemon,
        auth,
        forward,
        token,
        opens,
        relay,
        stall,
        _server: server,
    }
}

/// The share's one connection rides a tunnel that can die without
/// closing. Nothing on the controller's side fails then, so the request
/// on it runs into the response timeout, and the timeout is what
/// replaces the connection: the next request opens a tunnel of its own
/// and is answered on it.
#[tokio::test]
async fn a_share_whose_tunnel_stops_answering_times_out_once_and_is_reached_again() {
    let share = served_remote_share_with(
        pm_protocol::WORKER_PROTOCOL_DIR_SHARE_H2,
        "h2-remote-stalled",
        pm_daemon::forward::ForwardConfig {
            response_timeout: Some(std::time::Duration::from_millis(500)),
            ..Default::default()
        },
    )
    .await;
    let answered = (axum::http::StatusCode::OK, "<p>the report</p>".to_string());
    assert_eq!(share.get("report.html").await, answered);
    assert_eq!(share.forward_opens(), 1);

    share.stall.notify_waiters();
    let (status, _) = share.get("report.html").await;
    assert_eq!(
        status,
        axum::http::StatusCode::GATEWAY_TIMEOUT,
        "a request on the dead tunnel is reported as a timeout"
    );
    assert_eq!(
        share.daemon.idle_upstreams(share.forward.id),
        0,
        "the connection that timed out is no longer held"
    );

    assert_eq!(share.get("report.html").await, answered);
    assert_eq!(
        share.forward_opens(),
        2,
        "the request after the timeout asked the worker for a tunnel of its own"
    );
    assert_eq!(share.daemon.idle_upstreams(share.forward.id), 1);
    share.relay.abort();
}

/// What multiplexing buys over pooling, and the assertion that tells
/// them apart: HTTP/1 cannot carry concurrent requests on one
/// connection, so a pool would have opened one per request.
#[tokio::test]
async fn concurrent_requests_through_a_remote_share_share_one_h2_connection() {
    let share = served_remote_share(pm_protocol::WORKER_PROTOCOL_DIR_SHARE_H2, "h2-remote").await;

    let answers = futures::future::join_all((0..12).map(|_| share.get("report.html"))).await;
    for (status, body) in &answers {
        assert_eq!(
            (*status, body.as_str()),
            (axum::http::StatusCode::OK, "<p>the report</p>")
        );
    }
    assert_eq!(
        share.forward_opens(),
        1,
        "twelve concurrent requests asked the worker to dial once"
    );
    assert_eq!(
        share.daemon.idle_upstreams(share.forward.id),
        1,
        "and one connection is what the forward holds"
    );
    share.relay.abort();
}

/// The gate's whole point: a worker that cannot speak h2c still serves
/// its share, over the HTTP/1 pool from the commit before this one.
#[tokio::test]
async fn a_worker_below_the_h2_protocol_still_serves_its_share_over_the_pool() {
    let share =
        served_remote_share(pm_protocol::WORKER_PROTOCOL_DIR_SHARE_H2 - 1, "h1-remote").await;

    for _ in 0..4 {
        assert_eq!(
            share.get("report.html").await,
            (axum::http::StatusCode::OK, "<p>the report</p>".to_string())
        );
    }
    assert_eq!(
        share.forward_opens(),
        1,
        "sequential requests reuse the pooled HTTP/1 connection"
    );
    assert_eq!(share.daemon.idle_upstreams(share.forward.id), 1);

    // And this is what says it is really on the HTTP/1 path rather than
    // quietly h2: one HTTP/1 connection cannot carry these at once, so
    // the pool has to dial more.
    let answers = futures::future::join_all((0..8).map(|_| share.get("report.html"))).await;
    for (status, body) in &answers {
        assert_eq!(
            (*status, body.as_str()),
            (axum::http::StatusCode::OK, "<p>the report</p>")
        );
    }
    assert!(
        share.forward_opens() > 1,
        "HTTP/1 cannot multiplex, so concurrent requests needed more connections"
    );
    assert!(
        share.daemon.idle_upstreams(share.forward.id)
            <= pm_daemon::forward_upstream::MAX_IDLE_PER_TARGET
    );
    share.relay.abort();
}

/// The key-material refusal answers on the remote h2 path too.
///
/// The refusal reads the first bytes of the file it is about to serve, and the
/// share's own server does that whichever HTTP version carries the request. A
/// multiplexed tunnel changes the transport and not the read, and this says so
/// rather than leaving it to be reasoned about: the file is named notes.txt, so
/// nothing but its content can refuse it.
#[tokio::test]
async fn a_key_is_refused_through_a_multiplexed_remote_share() {
    let share =
        served_remote_share(pm_protocol::WORKER_PROTOCOL_DIR_SHARE_H2, "h2-remote-key").await;

    assert_eq!(
        share.get("report.html").await.0,
        axum::http::StatusCode::OK,
        "the share itself serves"
    );
    assert_eq!(
        share.get("notes.txt").await.0,
        axum::http::StatusCode::NOT_FOUND,
        "an armoured key was served over the h2 tunnel"
    );
    share.relay.abort();
}

/// An h2 connection goes when its share does, exactly as a pooled one
/// does. A multiplexed tunnel surviving a closed share would be the
/// same regression in a different shape.
#[tokio::test]
async fn closing_a_remote_share_drops_its_h2_connection() {
    let share = served_remote_share(
        pm_protocol::WORKER_PROTOCOL_DIR_SHARE_H2,
        "h2-remote-closed",
    )
    .await;

    assert_eq!(share.get("report.html").await.0, axum::http::StatusCode::OK);
    assert_eq!(share.daemon.idle_upstreams(share.forward.id), 1);

    share
        .daemon
        .unpublish_dir(&share.token, &share.forward.slug)
        .unwrap();

    assert_eq!(
        share.daemon.idle_upstreams(share.forward.id),
        0,
        "a closed share leaves no multiplexed connection"
    );
    assert_eq!(
        share.get("report.html").await.0,
        axum::http::StatusCode::NOT_FOUND
    );
    share.relay.abort();
}

/// And when the forward alone is taken out of service.
#[tokio::test]
async fn stopping_a_remote_shares_session_drops_its_h2_connection() {
    let share = served_remote_share(
        pm_protocol::WORKER_PROTOCOL_DIR_SHARE_H2,
        "h2-remote-stopped",
    )
    .await;

    assert_eq!(share.get("report.html").await.0, axum::http::StatusCode::OK);
    assert_eq!(share.daemon.idle_upstreams(share.forward.id), 1);

    let session = share.forward.session_id;
    share.daemon.stop_session_dir_shares(session);
    assert_eq!(share.daemon.idle_upstreams(share.forward.id), 0);

    share.daemon.unbind_session_forwards(session);
    assert_eq!(
        share.get("report.html").await.0,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(share.daemon.idle_upstreams(share.forward.id), 0);
    share.relay.abort();
}
