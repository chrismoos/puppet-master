//! HTTP surface tests: auth endpoints via in-process router calls,
//! and the WebSocket transport end-to-end against a real listener.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use pm_daemon::plan::PlanOptionDraft;
use pm_daemon::storage::{ITEM_ATTACHMENT_FILE_MAX, ITEM_BODY_MAX};
use pm_daemon::{Daemon, DaemonConfig};
use pm_protocol::domain::{
    AgentKind, ClientEnvelope, ClientMsg, ControllerMsg, Event, ItemPriority, ItemSourceKind,
    ItemStatus, ItemWrite, PlanDecisionMode, ProjectPath, Scope, ServerMsg, SessionState,
    TerminalRunState, WorkerMsg,
};
use prost::Message as _;
use support::{test_registry, TEST_HOST, TEST_ORIGIN, TEST_TIMEOUT};
use tokio_tungstenite::tungstenite;
use tower::util::ServiceExt;

fn test_daemon() -> (Arc<Daemon>, tempfile::TempDir) {
    test_daemon_with_local_worker(true)
}

#[tokio::test]
async fn attachment_http_is_authenticated_atomic_safe_and_downloadable() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let cookie = format!("pm_session={token}");
    let bucket = daemon.create_bucket("primary").unwrap();
    let item = daemon
        .upsert_item(
            bucket,
            &ItemWrite {
                bucket_id: bucket,
                title: Some("files".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .0;
    let app = pm_daemon::http::router(daemon.clone());
    let uri = format!(
        "/api/buckets/{bucket}/items/{}/attachments?filename={}&media_type=text%2Fplain",
        item.id, "%D1%80%D0%B5%D0%B7%D1%8E%D0%BC%D0%B5.txt"
    );
    let unauthenticated = app
        .clone()
        .oneshot(Request::post(&uri).body(Body::from("hello")).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let upload = || {
        Request::post(&uri)
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("hello"))
            .unwrap()
    };
    let first = app.clone().oneshot(upload()).await.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = body_json(first).await["attachment"].clone();
    assert_eq!(first["filename"], "резюме.txt");
    assert_eq!(first["byteLength"], "5");
    let attachment_id = first["id"].as_str().unwrap();

    let second = body_json(app.clone().oneshot(upload()).await.unwrap()).await;
    assert_ne!(
        second["attachment"]["id"], first["id"],
        "duplicate bytes are independent"
    );
    assert_eq!(second["attachment"]["sha256"], first["sha256"]);

    let listed = body_json(
        app.clone()
            .oneshot(
                Request::get(format!(
                    "/api/buckets/{bucket}/items/{}/attachments",
                    item.id
                ))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(listed["attachments"].as_array().unwrap().len(), 2);

    let download = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/buckets/{bucket}/items/{}/attachments/{attachment_id}",
                item.id
            ))
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(download.headers()[header::CONTENT_TYPE], "text/plain");
    assert!(download.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .starts_with("attachment;"));
    assert_eq!(download.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        axum::body::to_bytes(download.into_body(), 100)
            .await
            .unwrap(),
        "hello"
    );

    // A download is reached by navigating to it, and a navigation sets no
    // Authorization header, so this one route takes the cookie as well. What
    // that costs is bounded by the two headers just asserted plus CORS: a page
    // on another origin can cause a download and cannot read one.
    let navigated = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/buckets/{bucket}/items/{}/attachments/{attachment_id}",
                item.id
            ))
            .header(header::COOKIE, &cookie)
            .header(header::HOST, TEST_HOST)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        navigated.status(),
        StatusCode::OK,
        "a download link has to work from a navigation"
    );

    let too_large = app
        .clone()
        .oneshot(
            Request::post(format!(
                "/api/buckets/{bucket}/items/{}/attachments?filename=large.bin",
                item.id
            ))
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .header(header::CONTENT_LENGTH, ITEM_ATTACHMENT_FILE_MAX + 1)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        daemon.list_item_attachments(bucket, item.id).unwrap().len(),
        2
    );

    let deleted = app
        .clone()
        .oneshot(
            Request::delete(format!(
                "/api/buckets/{bucket}/items/{}/attachments/{attachment_id}",
                item.id
            ))
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        daemon.list_item_attachments(bucket, item.id).unwrap().len(),
        1
    );
    assert!(daemon
        .item_notes(bucket, item.id)
        .unwrap()
        .iter()
        .any(|note| note.text.contains("removed attachment")));
}

#[tokio::test]
async fn item_search_http_round_trips_a_maximum_unicode_body() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let bucket = daemon.create_bucket("primary").unwrap();
    let prefix = "# Context\n\n- café\n- 😀\n\n";
    let body = format!(
        "{prefix}{}",
        "界".repeat(ITEM_BODY_MAX - prefix.chars().count())
    );
    daemon
        .upsert_item(
            bucket,
            &ItemWrite {
                bucket_id: bucket,
                title: Some("Large body".into()),
                body: Some(body.clone()),
                ..Default::default()
            },
            None,
        )
        .unwrap();

    let response = pm_daemon::http::router(daemon)
        .oneshot(
            Request::get(format!("/api/buckets/{bucket}/items"))
                .header(header::AUTHORIZATION, auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["items"][0]["body"], body);
}

#[tokio::test]
async fn canonical_item_and_attachment_routes_isolate_equal_numbers_across_buckets() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let one = daemon.create_bucket("one").unwrap();
    let two = daemon.create_bucket("two").unwrap();
    let one_item = daemon
        .upsert_item(
            one,
            &ItemWrite {
                bucket_id: one,
                title: Some("one".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .0;
    let two_item = daemon
        .upsert_item(
            two,
            &ItemWrite {
                bucket_id: two,
                title: Some("two".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .0;
    assert_eq!((one_item.id, two_item.id), (1, 1));
    let attachment = daemon
        .attach_item_bytes(two, 1, "two.txt", Some("text/plain"), b"two", None)
        .unwrap();
    let app = pm_daemon::http::router(daemon);

    for (bucket, title) in [(one, "one"), (two, "two")] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/api/buckets/{bucket}/items/1"))
                    .header(header::AUTHORIZATION, &auth)
                    .header(header::HOST, TEST_HOST)
                    .header(header::ORIGIN, TEST_ORIGIN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["item"]["title"], title);
    }

    let crossed = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/buckets/{one}/items/1/attachments/{}",
                attachment.id
            ))
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(crossed.status(), StatusCode::NOT_FOUND);

    let legacy = app
        .oneshot(
            Request::get(format!("/api/items/1/attachments/{}", attachment.id))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::GONE);
    assert!(body_json(legacy).await["error"]
        .as_str()
        .unwrap()
        .contains("unqualified attachment URLs are unsupported"));
}

#[tokio::test]
async fn item_search_http_includes_completed_by_default_but_stays_bounded() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let bucket = daemon.create_bucket("primary").unwrap();
    for (title, status, priority) in [
        ("Open work", ItemStatus::Planned, ItemPriority::Low),
        ("Completed history", ItemStatus::Done, ItemPriority::Urgent),
    ] {
        daemon
            .upsert_item(
                bucket,
                &ItemWrite {
                    bucket_id: bucket,
                    title: Some(title.into()),
                    status: Some(status),
                    priority: Some(priority),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
    }
    let app = pm_daemon::http::router(daemon);

    let legacy = app
        .clone()
        .oneshot(
            Request::get("/api/items/2")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::GONE);
    assert!(body_json(legacy).await["error"]
        .as_str()
        .unwrap()
        .contains("unqualified item ids are unsupported"));

    let direct = app
        .clone()
        .oneshot(
            Request::get(format!("/api/buckets/{bucket}/items/2"))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let direct = body_json(direct).await;
    assert_eq!(direct["item"]["title"], "Completed history");
    assert_eq!(direct["item"]["bucket_id"], bucket.to_string());
    assert_eq!(direct["item"]["ref"], format!("pm:item/{bucket}/2"));

    let default = app
        .clone()
        .oneshot(
            Request::get(format!("/api/buckets/{bucket}/items?limit=1"))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let default = body_json(default).await;
    assert_eq!(default["items"].as_array().unwrap().len(), 1);
    assert_eq!(default["items"][0]["title"], "Open work");
    assert_eq!(default["nextOffset"], 1);
    assert_eq!(default["counts"]["matchingTotal"], 2);
    assert_eq!(default["counts"]["byStatus"]["done"], 1);

    let excluded = app
        .oneshot(
            Request::get(format!("/api/buckets/{bucket}/items?done=false&limit=1"))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let excluded = body_json(excluded).await;
    assert_eq!(excluded["items"].as_array().unwrap().len(), 1);
    assert!(excluded["nextOffset"].is_null());
    assert_eq!(excluded["counts"]["matchingTotal"], 1);
    assert!(excluded["counts"]["byStatus"]["done"].is_null());
}

#[tokio::test]
async fn item_search_http_is_authenticated_bucket_scoped_filtered_and_paged() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let bucket = daemon.create_bucket("primary").unwrap();
    let other = daemon.create_bucket("other").unwrap();
    let project = daemon.create_project(bucket, "api", "/tmp/api").unwrap();
    for index in 0..55 {
        daemon
            .upsert_item(
                bucket,
                &ItemWrite {
                    bucket_id: bucket,
                    title: Some(format!("Needle result {index:02}")),
                    body: Some("searchable body".into()),
                    status: Some(ItemStatus::Planned),
                    priority: Some(ItemPriority::High),
                    source_kind: Some(ItemSourceKind::Github),
                    project_id: Some(project),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
    }
    daemon
        .upsert_item(
            other,
            &ItemWrite {
                bucket_id: other,
                title: Some("Needle foreign".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let app = pm_daemon::http::router(daemon);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::get(format!("/api/buckets/{bucket}/items?q=needle"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let first = app.clone().oneshot(Request::get(format!("/api/buckets/{bucket}/items?q=nEeDlE&project={project}&status=planned&priority=high&source=github&summary=planned&limit=50")).header(header::AUTHORIZATION, &auth).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = body_json(first).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 50);
    assert_eq!(first["nextOffset"], 50);
    assert_eq!(first["counts"]["bucketTotal"], 55);
    assert_eq!(first["counts"]["matchingTotal"], 55);
    assert_eq!(first["counts"]["byStatus"]["planned"], 55);
    assert!(first["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["bucket_id"].as_str() == Some("1")));
    assert!(
        first["items"][0]["id"].is_string(),
        "ids are lossless JSON strings"
    );

    let second = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/buckets/{bucket}/items?q=needle&limit=50&offset=50"
            ))
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let second = body_json(second).await;
    assert_eq!(second["items"].as_array().unwrap().len(), 5);
    assert!(second["nextOffset"].is_null());
    assert_eq!(second["counts"], first["counts"]);

    let invalid_summary = app
        .oneshot(
            Request::get(format!("/api/buckets/{bucket}/items?summary=not-real"))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_summary.status(), StatusCode::BAD_REQUEST);
}

fn test_daemon_with_local_worker(local_worker_enabled: bool) -> (Arc<Daemon>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled,
        release_channel: None,
    };
    let (daemon, _exit_rx) = Daemon::new(config).unwrap();
    (Arc::new(daemon), tmp)
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

fn json_request(method: &str, path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, TEST_HOST)
        .header(header::ORIGIN, TEST_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn native_terminal_theme(name: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "puppet-master-terminal-theme", "version": 1, "name": name,
        "colors": {
            "foreground": "#C9CEDA", "background": "#0B0E14",
            "cursor": "#FFB224", "cursorAccent": "#0B0E14",
            "selectionForeground": "#EEF1F6", "selectionBackground": "#2B3548CC",
            "black": "#2E3436", "red": "#CC0000", "green": "#4E9A06",
            "yellow": "#C4A000", "blue": "#3465A4", "magenta": "#75507B",
            "cyan": "#06989A", "white": "#D3D7CF", "brightBlack": "#555753",
            "brightRed": "#EF2929", "brightGreen": "#8AE234",
            "brightYellow": "#FCE94F", "brightBlue": "#729FCF",
            "brightMagenta": "#AD7FA8", "brightCyan": "#34E2E2",
            "brightWhite": "#EEEEEC"
        }
    })
}

fn cookie_from(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("set-cookie header")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// Stands in for the signed-in dashboard: the session a login created,
/// exchanged for the access token every API route takes.
///
/// The cookie authenticates nothing but this exchange now, so a test that wants
/// to act as the dashboard has to hold what the dashboard holds.
fn dashboard_auth(daemon: &Daemon, session_value: &str) -> String {
    let (user_id, _) = daemon
        .auth_verify_user(session_value)
        .expect("a signed-in session");
    let (token, _) = daemon
        .issue_web_access_token(user_id, session_value)
        .unwrap();
    format!("Bearer {token}")
}

/// The same for a test that signed in over HTTP and holds the response.
fn dashboard_auth_from(daemon: &Daemon, response: &axum::response::Response) -> String {
    let cookie = cookie_from(response);
    let (_, value) = cookie.split_once('=').expect("a session cookie");
    dashboard_auth(daemon, value)
}

#[tokio::test]
async fn changing_a_password_over_http_needs_the_current_one_and_a_cookie() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());

    let res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = cookie_from(&res);
    let auth = dashboard_auth_from(&daemon, &res);

    // Changing a password reauthenticates against the cookie session, so this
    // is one of the two routes the cookie is still the credential for. A bearer
    // token is not enough on purpose: it is minted without the password being
    // re-entered, and a phone holds one.
    let change = |credential: Option<(header::HeaderName, String)>, current: &str, next: &str| {
        let mut req = json_request(
            "POST",
            "/api/user/password",
            serde_json::json!({"currentPassword": current, "newPassword": next}),
        );
        if let Some((name, value)) = credential {
            req.headers_mut().insert(name, value.parse().unwrap());
        }
        app.clone().oneshot(req)
    };
    let with_cookie = || Some((header::COOKIE, cookie.clone()));

    assert_eq!(
        change(
            Some((header::AUTHORIZATION, auth.clone())),
            "hunter2hunter2",
            "correct horse battery"
        )
        .await
        .unwrap()
        .status(),
        StatusCode::UNAUTHORIZED,
        "an access token does not answer for the password"
    );

    assert_eq!(
        change(None, "hunter2hunter2", "correct horse battery")
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
        "an unauthenticated caller cannot change a password"
    );
    assert_eq!(
        change(with_cookie(), "wrong", "correct horse battery")
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        change(with_cookie(), "hunter2hunter2", "short")
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        change(with_cookie(), "hunter2hunter2", "correct horse battery")
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/me")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "the session that made the change stays signed in"
    );

    let res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/login",
            serde_json::json!({"username": "testuser", "password": "correct horse battery"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn first_run_setup_then_login_and_logout() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());

    let res = app
        .clone()
        .oneshot(Request::get("/api/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(res).await["setup"], true);

    let res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "short"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = cookie_from(&res);
    let auth = dashboard_auth_from(&daemon, &res);

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/me")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["username"], "testuser");

    let res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "mallory", "password": "password12345"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    let res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/login",
            serde_json::json!({"username": "testuser", "password": "wrong-password"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = app
        .clone()
        .oneshot(
            // Signing out is the cookie's business, since the cookie is the
            // session. The token it minted has to stop working with it.
            Request::post("/api/logout")
                .header(header::COOKIE, &cookie)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = app
        .clone()
        .oneshot(
            Request::get("/api/me")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the access token the ended session minted must not outlive it"
    );
}

#[tokio::test]
async fn authenticated_workspace_crud_persists_layout() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());
    let setup = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let auth = dashboard_auth_from(&daemon, &setup);

    let mut create = json_request(
        "POST",
        "/api/workspaces",
        serde_json::json!({
            "name": "release watch",
            "layout": {"kind": "pane", "paneId": "p1", "terminalId": "7"}
        }),
    );
    create
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    let created = app.clone().oneshot(create).await.unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = body_json(created).await;
    let id = created["id"].as_u64().unwrap();

    let listed = app
        .clone()
        .oneshot(
            Request::get("/api/workspaces")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        body_json(listed).await["workspaces"][0]["layout"]["terminalId"],
        "7"
    );

    let mut update = json_request(
        "PUT",
        &format!("/api/workspaces/{id}"),
        serde_json::json!({"name": "release", "layout": {"kind": "pane", "paneId": "p1", "terminalId": "8"}}),
    );
    update
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    let updated = app.clone().oneshot(update).await.unwrap();
    assert_eq!(body_json(updated).await["layout"]["terminalId"], "8");

    let mut second = json_request(
        "POST",
        "/api/workspaces",
        serde_json::json!({"name": "second", "layout": {"kind": "pane", "paneId": "p2", "terminalId": null}}),
    );
    second
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    let second_id = body_json(app.clone().oneshot(second).await.unwrap()).await["id"]
        .as_u64()
        .unwrap();
    let mut reorder = json_request(
        "PUT",
        "/api/workspaces/order",
        serde_json::json!({"workspace_ids": [second_id, id], "home_position": 1}),
    );
    reorder
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(reorder).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    let reordered = app
        .clone()
        .oneshot(
            Request::get("/api/workspaces")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let reordered = body_json(reordered).await;
    assert_eq!(reordered["workspaces"][0]["id"], second_id);
    assert!(reordered.get("homePosition").is_none());

    let deleted = app
        .oneshot(
            Request::delete(format!("/api/workspaces/{id}"))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn unknown_asset_and_spa_fallback() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon);

    let res = app
        .clone()
        .oneshot(
            Request::get("/definitely-missing.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// The exchange the whole design rests on: the session cookie mints an access
/// token and does nothing else, so this is the one place a cookie still carries
/// authority and the only place the origin comparison is load-bearing.
///
/// A stated origin's absence is refused rather than read as permission, the
/// comparison includes the port because `Sec-Fetch-Site` calls another port on
/// the same host same-site rather than cross-site, and fetch metadata is only
/// ever a veto.
#[tokio::test]
async fn the_token_mint_takes_a_cookie_from_the_dashboard_and_nothing_else() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let cookie = format!("pm_session={token}");
    let app = pm_daemon::http::router(daemon.clone());

    let mint = |headers: Vec<(header::HeaderName, String)>| {
        let mut builder =
            Request::post(pm_daemon::http::WEB_TOKEN_PATH).header(header::HOST, TEST_HOST);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        app.clone().oneshot(builder.body(Body::empty()).unwrap())
    };

    let minted = mint(vec![
        (header::COOKIE, cookie.clone()),
        (header::ORIGIN, TEST_ORIGIN.into()),
    ])
    .await
    .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    assert!(
        minted.headers().get(header::SET_COOKIE).is_none(),
        "the token must not be written into a cookie the browser would attach by itself"
    );
    let body = body_json(minted).await;
    let access = body["accessToken"].as_str().expect("a token").to_string();
    assert!(body["expiresAtUnixMs"].as_i64().is_some_and(|at| at > 0));

    // And it is a working credential on an ordinary API route, which the
    // cookie that minted it is not.
    let me = app
        .clone()
        .oneshot(
            Request::get("/api/me")
                .header(header::AUTHORIZATION, format!("Bearer {access}"))
                .header(header::HOST, TEST_HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);

    for (label, headers) in [
        (
            "no cookie at all",
            vec![(header::ORIGIN, TEST_ORIGIN.into())],
        ),
        ("no stated origin", vec![(header::COOKIE, cookie.clone())]),
        (
            "a sandboxed document's null origin",
            vec![
                (header::COOKIE, cookie.clone()),
                (header::ORIGIN, "null".into()),
            ],
        ),
        (
            "another port on this host, which is same-site and a different origin",
            vec![
                (header::COOKIE, cookie.clone()),
                (header::ORIGIN, format!("http://{TEST_HOST}:3999")),
            ],
        ),
        (
            "a subdomain under this host",
            vec![
                (header::COOKIE, cookie.clone()),
                (header::ORIGIN, format!("http://preview.{TEST_HOST}")),
            ],
        ),
        (
            "fetch metadata vetoing an origin that otherwise matches",
            vec![
                (header::COOKIE, cookie.clone()),
                (header::ORIGIN, TEST_ORIGIN.into()),
                (
                    header::HeaderName::from_static("sec-fetch-site"),
                    "same-site".into(),
                ),
            ],
        ),
    ] {
        let refused = mint(headers).await.unwrap();
        assert_ne!(
            refused.status(),
            StatusCode::OK,
            "the mint answered {label}"
        );
    }
}

/// Cookie tossing. A cookie is keyed by name, domain and path and ignores the
/// port, so a preview on another port of this host, or on a subdomain of it,
/// can set a second `pm_session`. The browser then sends both and the order is
/// the injecting page's to choose, so resolving the ambiguity at all would let
/// it decide which session the user is in.
///
/// Asked of the token mint, because that is the one route a cookie still
/// answers for and so the only place an ambiguous jar could buy anything.
#[tokio::test]
async fn two_session_cookies_mint_no_access_token() {
    let (daemon, _tmp) = test_daemon();
    let mine = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    // A second real session, standing in for one the injecting page holds.
    let theirs = daemon.auth_login("testuser", "hunter2hunter2").unwrap();

    let ask = |cookie: String| {
        let daemon = daemon.clone();
        async move {
            pm_daemon::http::router(daemon)
                .oneshot(
                    Request::post(pm_daemon::http::WEB_TOKEN_PATH)
                        .header(header::HOST, TEST_HOST)
                        .header(header::ORIGIN, TEST_ORIGIN)
                        .header(header::COOKIE, cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };

    assert_eq!(ask(format!("pm_session={mine}")).await, StatusCode::OK);
    for jar in [
        format!("pm_session={theirs}; pm_session={mine}"),
        format!("pm_session={mine}; pm_session={theirs}"),
        format!("other=x; pm_session={theirs}; y=2; pm_session={mine}"),
    ] {
        assert_eq!(
            ask(jar.clone()).await,
            StatusCode::UNAUTHORIZED,
            "{jar} resolved to a session"
        );
    }
    // An unrelated cookie beside the real one is ordinary and still works.
    assert_eq!(
        ask(format!("theme=dark; pm_session={mine}; tz=UTC")).await,
        StatusCode::OK
    );
}

/// The half of the ambient-authority problem CORS does not cover. A share on
/// a same-site subdomain is a different origin, so CORS hides every response
/// from it, but a request that needs no preflight still runs: `application/
/// json` is not a CORS-safelisted content type and so forces one, while a
/// handler taking raw bytes or no body at all accepts `text/plain`. Each write
/// below landed before the guard, blind but done.
#[tokio::test]
async fn a_preflight_free_write_from_a_same_site_page_is_refused() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let cookie = format!("pm_session={token}");
    let auth = dashboard_auth(&daemon, &token);
    let bucket = daemon.create_bucket("primary").unwrap();
    let item = daemon
        .upsert_item(
            bucket,
            &ItemWrite {
                bucket_id: bucket,
                title: Some("files".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .0;

    // A forward under a share domain: a different origin, the same site, so
    // the browser attaches the cookie by itself.
    const SHARE_PAGE: &str = "https://f42.share.dashboard.test";
    let attachment = format!(
        "/api/buckets/{bucket}/items/{}/attachments?filename=planted.txt",
        item.id
    );
    let writes: [(&str, &str, &str, &str); 3] = [
        ("POST", &attachment, "text/plain", "planted by a share page"),
        (
            "PUT",
            "/api/user/settings/push-web-idle-minutes",
            "text/plain",
            "5",
        ),
        ("POST", "/api/buckets/1/default", "", ""),
    ];
    let build = |method: &str,
                 uri: &str,
                 content_type: &str,
                 body: &str,
                 origin: &str,
                 credential: (header::HeaderName, &str)| {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, origin)
            .header(credential.0, credential.1);
        if !content_type.is_empty() {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    };

    for (method, uri, content_type, body) in &writes {
        let refused = pm_daemon::http::router(daemon.clone())
            .oneshot(build(
                method,
                uri,
                content_type,
                body,
                SHARE_PAGE,
                (header::COOKIE, &cookie),
            ))
            .await
            .unwrap();
        assert_eq!(
            refused.status(),
            StatusCode::FORBIDDEN,
            "{method} {uri} ran for a page on {SHARE_PAGE}"
        );
    }

    // And the cookie is refused on these routes from the dashboard's own
    // origin too, which is the stronger half: the origin check is no longer
    // the only thing standing between a shared cookie jar and a write, because
    // the cookie does not authorize an ordinary API route at all.
    for (method, uri, content_type, body) in &writes {
        let refused = pm_daemon::http::router(daemon.clone())
            .oneshot(build(
                method,
                uri,
                content_type,
                body,
                TEST_ORIGIN,
                (header::COOKIE, &cookie),
            ))
            .await
            .unwrap();
        assert_eq!(
            refused.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} took the session cookie as authority"
        );
    }

    // None of it happened, and the session is intact.
    assert!(daemon
        .list_item_attachments(bucket, item.id)
        .unwrap()
        .is_empty());
    assert!(daemon.auth_verify(&token).is_some());

    // The dashboard's own page makes every one of those writes with the token
    // it minted, so what changed is the credential and not the request.
    for (method, uri, content_type, body) in &writes {
        let allowed = pm_daemon::http::router(daemon.clone())
            .oneshot(build(
                method,
                uri,
                content_type,
                body,
                TEST_ORIGIN,
                (header::AUTHORIZATION, &auth),
            ))
            .await
            .unwrap();
        assert!(
            allowed.status().is_success(),
            "{method} {uri} was refused for the dashboard's own page: {}",
            allowed.status()
        );
    }
    assert_eq!(
        daemon.list_item_attachments(bucket, item.id).unwrap().len(),
        1
    );

    // Signing out stays the cookie's business, and a share page cannot do it
    // either.
    let refused = pm_daemon::http::router(daemon.clone())
        .oneshot(build(
            "POST",
            "/api/logout",
            "",
            "",
            SHARE_PAGE,
            (header::COOKIE, &cookie),
        ))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(daemon.auth_verify(&token).is_some());
    let allowed = pm_daemon::http::router(daemon.clone())
        .oneshot(build(
            "POST",
            "/api/logout",
            "",
            "",
            TEST_ORIGIN,
            (header::COOKIE, &cookie),
        ))
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::NO_CONTENT);
    assert!(daemon.auth_verify(&token).is_none(), "logout took effect");
}

/// The guard keys on the session cookie, not the route, because the cookie is
/// the only credential a browser attaches on its own. A caller holding a
/// bearer token already had it, so the MCP surface and the phone keep working
/// while stating no origin at all.
#[tokio::test]
async fn a_bearer_authenticated_write_needs_no_origin() {
    let env = ws_env().await;
    let bucket = env.daemon.create_bucket("mcp").unwrap();
    let tmp_project = tempfile::tempdir().unwrap();
    let project = env
        .daemon
        .create_project(bucket, "p", tmp_project.path().to_str().unwrap())
        .unwrap();
    let session = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let response = pm_daemon::http::router(env.daemon.clone())
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, TEST_HOST)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {
                            "name": "report",
                            "arguments": { "goal": "g", "headline": "h" },
                        },
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an agent's own bearer token must not need an origin"
    );
}

struct WsEnv {
    addr: std::net::SocketAddr,
    worker_addr: std::net::SocketAddr,
    /// What the dashboard authenticates API calls and socket upgrades with.
    auth: String,
    /// The session the token was minted from, for the two routes the cookie is
    /// still the credential for.
    cookie: String,
    daemon: Arc<Daemon>,
    _handle: pm_daemon::ServerHandle,
    _tmp: tempfile::TempDir,
}

async fn ws_env() -> WsEnv {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: Some("127.0.0.1:0".parse().unwrap()),
        public_url: None,
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    WsEnv {
        addr: handle.http_addr.unwrap(),
        worker_addr: handle.worker_addr.unwrap(),
        auth: dashboard_auth(&daemon, &token),
        cookie: format!("pm_session={token}"),
        daemon,
        _handle: handle,
        _tmp: tmp,
    }
}

type HostLink =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// One connection to the controller's host plane, and the material an
/// enrollment proof over it is bound to.
struct HostDial {
    link: HostLink,
    controller_key: pm_tls::KeyHash,
    exporter: [u8; 32],
}

/// Dials the host plane the way `pm worker` does: mutual TLS first, then the
/// WebSocket. Tests accept whatever key the controller presents, which is
/// what a host enrolling for the first time does before it pins one.
async fn host_dial(
    env: &WsEnv,
    identity: &pm_tls::Identity,
    path: &str,
    bearer: Option<&str>,
) -> anyhow::Result<HostDial> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let config = pm_tls::client_config(identity, &pm_tls::PeerPolicy::Pairing)?;
    let tcp = tokio::net::TcpStream::connect(env.worker_addr).await?;
    // As the real worker does: a small frame must not wait on Nagle for the
    // previous one's delayed ACK.
    tcp.set_nodelay(true)?;
    let stream = tokio_rustls::TlsConnector::from(config)
        .connect(pm_tls::peer_server_name(), tcp)
        .await?;
    let (controller_key, exporter) = {
        let (_, connection) = stream.get_ref();
        (
            pm_tls::peer_key_hash(connection.peer_certificates()).expect("controller key"),
            pm_tls::pairing::exporter(connection)?,
        )
    };
    let url = format!("wss://{}{path}", env.worker_addr);
    let mut request = url.as_str().into_client_request()?;
    if let Some(token) = bearer {
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse()?,
        );
    }
    let (link, _) = tokio_tungstenite::client_async(request, stream).await?;
    Ok(HostDial {
        link,
        controller_key,
        exporter,
    })
}

/// Opens the control link and settles trust: a recognized key is told to
/// proceed, and an unrecognized one proves the enrollment token first.
async fn host_control_link(
    env: &WsEnv,
    identity: &pm_tls::Identity,
    enrollment: Option<&str>,
) -> HostLink {
    use futures::{SinkExt, StreamExt};
    use pm_protocol::worker_frame::{self, WorkerFrame};
    use pm_tls::pairing::{Side, Transcript};

    let mut dial = host_dial(env, identity, "/worker", None)
        .await
        .expect("host plane accepts the dial");
    let tungstenite::Message::Binary(opening) = dial.link.next().await.unwrap().unwrap() else {
        panic!("expected the controller to open the link");
    };
    let listener_nonce = match worker_frame::decode(&opening) {
        Some(WorkerFrame::Ready) => return dial.link,
        Some(WorkerFrame::PairHello { nonce }) => nonce,
        other => panic!("unexpected opening frame {other:?}"),
    };
    let token = enrollment.expect("the controller opened enrollment but no token was supplied");
    let dialer_nonce = pm_tls::pairing::nonce();
    let transcript = Transcript {
        exporter: dial.exporter,
        dialer_key: identity.key_hash(),
        listener_key: dial.controller_key,
        dialer_nonce,
        listener_nonce,
    };
    dial.link
        .send(tungstenite::Message::Binary(
            worker_frame::encode_pair_proof(&dialer_nonce, &transcript.mac(token, Side::Dialer))
                .into(),
        ))
        .await
        .unwrap();
    let tungstenite::Message::Binary(accept) = dial.link.next().await.unwrap().unwrap() else {
        panic!("expected the controller's own proof");
    };
    match worker_frame::decode(&accept) {
        Some(WorkerFrame::PairAccept { mac }) => assert!(
            transcript.verify(token, Side::Listener, &mac),
            "the controller must prove the token before the host trusts it"
        ),
        other => panic!("unexpected pairing reply {other:?}"),
    }
    dial.link
}

async fn terminal_ws_connect(env: &WsEnv, terminal_id: u64, generation: u64) -> WsStream {
    let uri: tungstenite::http::Uri = format!(
        "ws://{}/ws/terminal/{terminal_id}?generation={generation}",
        env.addr
    )
    .parse()
    .unwrap();
    let builder = tungstenite::client::ClientRequestBuilder::new(uri)
        .with_header(header::AUTHORIZATION.as_str(), env.auth.clone())
        .with_sub_protocol("pm-terminal-v1");
    tokio_tungstenite::connect_async(builder).await.unwrap().0
}

async fn terminal_ws_connect_with_size(
    env: &WsEnv,
    terminal_id: u64,
    generation: u64,
    cols: u16,
    rows: u16,
) -> WsStream {
    let uri: tungstenite::http::Uri = format!(
        "ws://{}/ws/terminal/{terminal_id}?generation={generation}&cols={cols}&rows={rows}",
        env.addr
    )
    .parse()
    .unwrap();
    let builder = tungstenite::client::ClientRequestBuilder::new(uri)
        .with_header(header::AUTHORIZATION.as_str(), env.auth.clone())
        .with_sub_protocol("pm-terminal-v1");
    tokio_tungstenite::connect_async(builder).await.unwrap().0
}

async fn terminal_wait_for_replay(ws: &mut WsStream) {
    use futures::StreamExt;
    loop {
        let message = tokio::time::timeout(TEST_TIMEOUT, ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let tungstenite::Message::Binary(frame) = message else {
            continue;
        };
        if matches!(
            pm_protocol::terminal_frame::decode(&frame),
            Some(pm_protocol::terminal_frame::TerminalFrame::Output { flags, .. })
                if flags & pm_protocol::terminal_frame::FLAG_REPLAY_END != 0
        ) {
            return;
        }
    }
}

async fn terminal_wait_for_text(ws: &mut WsStream, needle: &[u8]) {
    use futures::StreamExt;
    let mut output = Vec::new();
    loop {
        let message = ws.next().await.unwrap().unwrap();
        let tungstenite::Message::Binary(frame) = message else {
            continue;
        };
        if let Some(pm_protocol::terminal_frame::TerminalFrame::Output { data, .. }) =
            pm_protocol::terminal_frame::decode(&frame)
        {
            output.extend_from_slice(data);
            if output.windows(needle.len()).any(|window| window == needle) {
                return;
            }
        }
    }
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(env: &WsEnv, authenticated: bool) -> WsStream {
    let uri: tungstenite::http::Uri = format!("ws://{}/ws", env.addr).parse().unwrap();
    let mut builder = tungstenite::client::ClientRequestBuilder::new(uri);
    if authenticated {
        builder = builder.with_header(header::AUTHORIZATION.as_str(), env.auth.clone());
    }
    let (stream, _) = tokio_tungstenite::connect_async(builder).await.unwrap();
    stream
}

async fn ws_send(ws: &mut WsStream, seq: u64, msg: ClientMsg) {
    use futures::SinkExt;
    let frame = ClientEnvelope { seq, msg }.encode_to_vec();
    ws.send(tungstenite::Message::Binary(frame.into()))
        .await
        .unwrap();
}

async fn ws_next(ws: &mut WsStream) -> Option<ServerMsg> {
    use futures::StreamExt;
    loop {
        let msg = tokio::time::timeout(TEST_TIMEOUT, ws.next())
            .await
            .expect("timed out waiting for ws message")?;
        match msg.expect("ws error") {
            tungstenite::Message::Binary(buf) => {
                return Some(ServerMsg::decode(&buf).expect("undecodable server frame"))
            }
            tungstenite::Message::Close(_) => return None,
            _ => continue,
        }
    }
}

async fn ws_request(ws: &mut WsStream, seq: u64, msg: ClientMsg) -> Result<Option<u64>, String> {
    ws_send(ws, seq, msg).await;
    loop {
        match ws_next(ws).await.expect("connection closed") {
            ServerMsg::CommandResult {
                seq: got, result, ..
            } if got == seq => return result,
            _ => continue,
        }
    }
}

async fn ws_request_data(
    ws: &mut WsStream,
    seq: u64,
    msg: ClientMsg,
) -> Result<bytes::Bytes, String> {
    ws_send(ws, seq, msg).await;
    loop {
        match ws_next(ws).await.expect("connection closed") {
            ServerMsg::CommandResult {
                seq: got,
                result,
                data,
            } if got == seq => {
                result?;
                return Ok(data);
            }
            _ => continue,
        }
    }
}

/// A reserved documentation address, used only to ask the routing table
/// which local interface would carry outbound traffic. Nothing is sent.
const ROUTE_PROBE: (&str, u16) = ("192.0.2.1", 80);

/// The non-loopback IPv4 this machine would source outbound traffic
/// from, which forward listeners can bind so a published URL has a host
/// another device could open.
fn routable_ipv4() -> Option<std::net::Ipv4Addr> {
    let probe = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    probe.connect(ROUTE_PROBE).ok()?;
    match probe.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() => Some(v4),
        _ => None,
    }
}

async fn forward_ws_env(bind: std::net::Ipv4Addr, public_url: Option<&str>) -> WsEnv {
    forward_ws_env_with_mount(
        public_url,
        pm_daemon::forward::ForwardConfig {
            bind: Some(bind.into()),
            ..Default::default()
        },
    )
    .await
}

async fn forward_ws_env_with_mount(
    public_url: Option<&str>,
    forward: pm_daemon::forward::ForwardConfig,
) -> WsEnv {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward,
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: Some("127.0.0.1:0".parse().unwrap()),
        public_url: public_url.map(str::to_string),
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    WsEnv {
        addr: handle.http_addr.unwrap(),
        worker_addr: handle.worker_addr.unwrap(),
        auth: dashboard_auth(&daemon, &token),
        cookie: format!("pm_session={token}"),
        daemon,
        _handle: handle,
        _tmp: tmp,
    }
}

/// Publishes a port on a fresh session and returns the forward.
async fn publish_a_forward(env: &WsEnv) -> pm_protocol::domain::SessionForward {
    publish_a_forward_named(env, "forwards", 5173).await
}

/// The same for a second forward in one daemon, which needs its own
/// bucket name, slug and local port.
async fn publish_a_forward_named(
    env: &WsEnv,
    name: &str,
    port: u16,
) -> pm_protocol::domain::SessionForward {
    let bucket = env.daemon.create_bucket(name).unwrap();
    let project = env.daemon.create_project(bucket, "api", "/tmp").unwrap();
    let session = env
        .daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "task",
            "prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let token = env.daemon.session_token(session).unwrap().unwrap();
    env.daemon
        .publish_port(&token, port, name, "dev server", "http")
        .await
        .unwrap()
}

/// Connects the control socket claiming a different Host, the way a
/// phone reaching the controller by a name the daemon never configured
/// does.
async fn ws_connect_with_host(env: &WsEnv, host: &str) -> WsStream {
    use tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{}/ws", env.addr)
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert(header::AUTHORIZATION, env.auth.parse().unwrap());
    headers.insert(tungstenite::http::header::HOST, host.parse().unwrap());
    let (stream, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    stream
}

async fn subscribed_forward(ws: &mut WsStream) -> pm_protocol::domain::SessionForward {
    subscribed_forwards(ws)
        .await
        .into_iter()
        .next()
        .expect("the published forward is in the snapshot")
}

async fn subscribed_forwards(ws: &mut WsStream) -> Vec<pm_protocol::domain::SessionForward> {
    ws_send(ws, 1, ClientMsg::Subscribe { scope: Scope::All }).await;
    loop {
        match ws_next(ws).await.expect("connection closed") {
            ServerMsg::Snapshot(snapshot) => return snapshot.forwards,
            _ => continue,
        }
    }
}

/// Without a configured public URL, each client gets a link back to the
/// origin it reached the controller on, so a phone and a laptop both get
/// something that works from where they are.
#[tokio::test]
async fn a_client_sees_forward_links_on_the_origin_it_reached_the_daemon_on() {
    let Some(bind) = routable_ipv4() else {
        eprintln!("skipped: this machine has no routable IPv4 to bind a forward listener on");
        return;
    };
    let env = forward_ws_env(bind, None).await;
    let forward = publish_a_forward(&env).await;
    assert_eq!(
        forward.url,
        format!("http://{}/forwards/{}/", env.addr, forward.id),
        "the canonical URL falls back to the listener's own interface"
    );

    let mut ws = ws_connect_with_host(&env, "pm.example:7676").await;
    let seen = subscribed_forward(&mut ws).await;
    assert_eq!(
        seen.url,
        format!("http://pm.example:7676/forwards/{}/", forward.id),
        "the client's own Host is overlaid on the link it renders"
    );
}

/// The share-domain mount over the real listener: the daemon serves the
/// dashboard and every forward host on one port, so a Host header is all
/// that decides which of them answers.
#[tokio::test]
async fn a_share_host_and_the_dashboard_are_served_on_one_listener() {
    let Some(bind) = routable_ipv4() else {
        eprintln!("skipped: this machine has no routable IPv4 to bind a forward listener on");
        return;
    };
    let env = forward_ws_env_with_mount(
        Some("https://pm.example"),
        pm_daemon::forward::ForwardConfig {
            bind: Some(bind.into()),
            share_domain: Some("pm-preview.example".into()),
            ..Default::default()
        },
    )
    .await;
    let forward = publish_a_forward(&env).await;
    assert_eq!(
        forward.url,
        format!("https://{}.pm-preview.example/", forward.slug)
    );

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    // The forward's own host reaches the forward, whose target is not
    // listening, so the proxy answers rather than the dashboard.
    let response = client
        .get(format!("http://{}/api/version", env.addr))
        .header("host", format!("{}.pm-preview.example", forward.slug))
        .header(header::AUTHORIZATION, env.auth.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        502,
        "a dashboard path on a forward host is proxied at the forward"
    );

    // The dashboard's own host still answers, and reports the mount.
    let dashboard = client
        .get(format!("http://{}/api/version", env.addr))
        .header("host", "pm.example")
        .header(header::AUTHORIZATION, env.auth.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(dashboard.status(), 200);
    let body: serde_json::Value = dashboard.json().await.unwrap();
    assert_eq!(body["forwardMount"]["mode"], "share-domain");
    assert_eq!(body["forwardMount"]["shareDomain"], "pm-preview.example");
}

/// A name under the share domain is a preview hostname whatever it
/// resolves to. The dashboard must never answer on one, or a stale
/// bookmark for a closed forward lands on the dashboard instead of
/// saying the preview is gone.
#[tokio::test]
async fn a_share_host_naming_no_forward_is_not_the_dashboard() {
    let Some(bind) = routable_ipv4() else {
        eprintln!("skipped: this machine has no routable IPv4 to bind a forward listener on");
        return;
    };
    let env = forward_ws_env_with_mount(
        Some("https://pm.example"),
        pm_daemon::forward::ForwardConfig {
            bind: Some(bind.into()),
            share_domain: Some("pm-preview.example".into()),
            ..Default::default()
        },
    )
    .await;
    let forward = publish_a_forward(&env).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    // `/api/version` is the discriminator: it is a real dashboard route,
    // so answering it at all would mean the dashboard was reached.
    let get = |host: String| {
        let client = client.clone();
        let addr = env.addr;
        let cookie = env.cookie.clone();
        async move {
            client
                .get(format!("http://{addr}/api/version"))
                .header("host", host)
                .header("cookie", cookie)
                .send()
                .await
                .unwrap()
                .status()
        }
    };

    assert_eq!(
        get("no-such-preview.pm-preview.example".into()).await,
        404,
        "a slug naming no forward reached the dashboard"
    );
    // The id form, for a forward that has been closed or never existed.
    assert_eq!(
        get("f4242.pm-preview.example".into()).await,
        404,
        "a stale forward id reached the dashboard"
    );
    // A live forward is named only by its slug, so its id is stale too.
    assert_eq!(
        get(format!("f{}.pm-preview.example", forward.id)).await,
        404,
        "a named forward's id host reached the dashboard"
    );
    // Closing the forward makes its own name stale in turn.
    env.daemon.close_forward(forward.id).unwrap();
    assert_eq!(
        get(format!("{}.pm-preview.example", forward.slug)).await,
        404,
        "a closed forward's name reached the dashboard"
    );

    // The share domain's apex is not a preview name, and the dashboard
    // host is still the dashboard.
    assert_eq!(get("pm-preview.example".into()).await, 200);
    assert_eq!(get("pm.example".into()).await, 200);
}

/// A share-domain forward URL is the configured domain's, so unlike a
/// path-prefix URL it does not follow the origin a client reached.
#[tokio::test]
async fn a_client_host_does_not_move_a_share_domain_forward_url() {
    let Some(bind) = routable_ipv4() else {
        eprintln!("skipped: this machine has no routable IPv4 to bind a forward listener on");
        return;
    };
    let env = forward_ws_env_with_mount(
        None,
        pm_daemon::forward::ForwardConfig {
            bind: Some(bind.into()),
            share_domain: Some("pm-preview.example".into()),
            ..Default::default()
        },
    )
    .await;
    let forward = publish_a_forward(&env).await;
    let second = publish_a_forward_named(&env, "forwards-two", 5174).await;
    let mut ws = ws_connect_with_host(&env, "pm.example:7676").await;
    let seen = subscribed_forwards(&mut ws).await;
    let url_of = |id: u64| {
        seen.iter()
            .find(|f| f.id == id)
            .map(|f| f.url.clone())
            .unwrap_or_default()
    };
    assert_eq!(
        url_of(forward.id),
        format!("http://{}.pm-preview.example:7676/", forward.slug),
        "the forward keeps its own subdomain and takes the client's scheme and port"
    );
    assert_ne!(
        url_of(forward.id),
        url_of(second.id),
        "a client renders both links, so they cannot be the same URL"
    );
}

/// The mechanism that lets forwards recover before anything is answered:
/// the browser plane's address is reserved first and served second, so a
/// client that connects in between waits instead of being handed an
/// answer the daemon is not ready to give. Without this the daemon
/// reported a live persisted forward as stopped on the first request
/// after a restart, which is a 503 a preview page renders once and keeps.
#[tokio::test]
async fn the_browser_plane_is_bound_before_it_answers_anything() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (listener, bound) = pm_daemon::http::bind("127.0.0.1:0".parse().unwrap(), false)
        .await
        .unwrap();

    // The address is bound, so the connection and the request both land.
    let mut socket = tokio::net::TcpStream::connect(bound).await.unwrap();
    socket
        .write_all(b"GET /api/version HTTP/1.1\r\nHost: pm.example\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();

    // Nothing answers it yet, which is the window recovery runs in.
    let mut reply = [0u8; 32];
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            socket.read(&mut reply)
        )
        .await
        .is_err(),
        "the bound address answered before it was served"
    );

    let env = ws_env().await;
    let task = pm_daemon::http::serve(env.daemon.clone(), listener, bound, None);
    let read = tokio::time::timeout(TEST_TIMEOUT, socket.read(&mut reply))
        .await
        .expect("the served address answers the queued request")
        .unwrap();
    assert!(
        reply[..read].starts_with(b"HTTP/1.1 200"),
        "got {:?}",
        String::from_utf8_lossy(&reply[..read])
    );
    task.abort();
}

/// The daemon reports the mount it started with, so an operator can see
/// which one is in effect without reading the process environment.
#[tokio::test]
async fn the_version_endpoint_reports_the_path_prefix_mount_by_default() {
    let env = ws_env().await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{}/api/version", env.addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["forwardMount"]["mode"], "path-prefix");
    assert!(body["forwardMount"].get("shareDomain").is_none());
    assert_eq!(
        body["channel"], "stable",
        "a host with no saved channel follows stable"
    );
}

/// The channel is what the newest-version fields are relative to, so a
/// dashboard reading them has to be told which one the host follows.
#[tokio::test]
async fn the_version_endpoint_names_the_channel_the_host_follows() {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        worker_addr: None,
        public_url: None,
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: Some("dev".into()),
    };
    let (_daemon, handle) = pm_daemon::start(config).await.unwrap();
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{}/api/version", handle.http_addr.unwrap()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["channel"], "dev");
}

/// The canonical URL is the one the agent prints into chat, so it must
/// not move under a client's Host header.
#[tokio::test]
async fn a_configured_public_url_is_not_overridden_by_a_client_host() {
    let Some(bind) = routable_ipv4() else {
        eprintln!("skipped: this machine has no routable IPv4 to bind a forward listener on");
        return;
    };
    let env = forward_ws_env(bind, Some("https://controller.example:8443")).await;
    let forward = publish_a_forward(&env).await;
    assert_eq!(
        forward.url,
        format!("https://controller.example:8443/forwards/{}/", forward.id)
    );

    let mut ws = ws_connect_with_host(&env, "pm.example:7676").await;
    let seen = subscribed_forward(&mut ws).await;
    assert_eq!(seen.url, forward.url, "--public-url stays canonical");
}

#[tokio::test]
async fn an_unauthenticated_control_socket_closes_with_the_auth_code() {
    use futures::StreamExt;
    let env = ws_env().await;
    let mut ws = ws_connect(&env, false).await;
    let msg = tokio::time::timeout(TEST_TIMEOUT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match msg {
        tungstenite::Message::Close(Some(frame)) => {
            assert_eq!(u16::from(frame.code), 4401);
        }
        other => panic!("expected close frame, got {other:?}"),
    }
}

#[tokio::test]
async fn authenticated_websocket_fetches_exact_and_bounded_ended_pages() {
    let env = ws_env().await;
    let bucket = env.daemon.create_bucket("history").unwrap();
    let project = env.daemon.create_project(bucket, "api", "/tmp").unwrap();
    let session = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "archived needle",
            "private prompt",
            None,
            pm_protocol::domain::PermissionMode::Default,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    env.daemon.kill_session(session).unwrap();
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let state = env.daemon.get_session_exact(session).unwrap().state;
            if matches!(state, SessionState::Exited | SessionState::Failed) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("killed session should become durable before querying ended history");

    let mut ws = ws_connect(&env, true).await;
    let exact = ws_request_data(
        &mut ws,
        1,
        ClientMsg::GetSession {
            session_id: session,
        },
    )
    .await
    .unwrap();
    let exact = pm_protocol::wire::SessionPage::decode(exact).unwrap();
    assert_eq!(exact.total, 1);
    assert_eq!(exact.sessions[0].id, session);

    let ended = ws_request_data(
        &mut ws,
        2,
        ClientMsg::ListEndedSessions {
            cursor: String::new(),
            limit: 500,
        },
    )
    .await
    .unwrap();
    let ended = pm_protocol::wire::SessionPage::decode(ended).unwrap();
    assert!(ended.sessions.len() <= 50);
    assert!(ended
        .sessions
        .iter()
        .any(|candidate| candidate.id == session));

    let search = ws_request_data(
        &mut ws,
        3,
        ClientMsg::SearchSessions {
            query: "archived needle".into(),
            cursor: String::new(),
            limit: 50,
        },
    )
    .await
    .unwrap();
    let search = pm_protocol::wire::SessionPage::decode(search).unwrap();
    assert_eq!(search.sessions[0].id, session);
}

#[tokio::test]
async fn websocket_snapshot_and_actions_preserve_awaiting_worker_state() {
    let env = ws_env().await;
    let bucket = env.daemon.create_bucket("remote").unwrap();
    let project = env
        .daemon
        .create_project(bucket, "project", "/remote/project")
        .unwrap();
    let (token, _) = env.daemon.create_worker_enrollment("remote-host").unwrap();
    let mut registration = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/remote",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    env.daemon
        .set_bucket_workers(bucket, &[0, registration.worker_id], 0, None)
        .unwrap();
    env.daemon
        .set_project_workers(
            project,
            &[registration.worker_id],
            Some(registration.worker_id),
        )
        .unwrap();
    let session_id = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "remote session",
            "prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            Some(registration.worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    assert!(matches!(
        registration.rx.recv().await,
        Some(ControllerMsg::Spawn { session_id: id, .. }) if id == session_id
    ));
    env.daemon.disconnect_worker(&registration.link);

    let mut ws = ws_connect(&env, true).await;
    ws_send(&mut ws, 1, ClientMsg::Subscribe { scope: Scope::All }).await;
    loop {
        if let Some(ServerMsg::Snapshot(snapshot)) = ws_next(&mut ws).await {
            let session = snapshot
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .unwrap();
            assert_eq!(session.state, SessionState::AwaitingWorker);
            assert_eq!(
                session.state_detail,
                "worker offline, resumes when it reconnects"
            );
            let worker = snapshot
                .workers
                .iter()
                .find(|worker| worker.id == registration.worker_id)
                .unwrap();
            assert_eq!(worker.hostname, "host.local");
            assert!(!worker.online);
            break;
        }
    }
    let error = ws_request(&mut ws, 2, ClientMsg::InterruptSession { session_id })
        .await
        .unwrap_err();
    assert!(error.contains("offline"));
    assert_eq!(
        env.daemon
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .state,
        SessionState::AwaitingWorker
    );
}

#[tokio::test]
async fn authenticated_same_user_websockets_snapshot_and_converge_terminal_theme() {
    let env = ws_env().await;
    env.daemon
        .set_user_terminal_theme(
            1,
            Some(&serde_json::to_vec(&native_terminal_theme("Initial")).unwrap()),
        )
        .unwrap();
    let mut first = ws_connect(&env, true).await;
    let mut second = ws_connect(&env, true).await;

    async fn subscribe_and_theme(ws: &mut WsStream, seq: u64) -> String {
        ws_send(ws, seq, ClientMsg::Subscribe { scope: Scope::All }).await;
        let mut theme = None;
        let mut acknowledged = false;
        while theme.is_none() || !acknowledged {
            match ws_next(ws).await.unwrap() {
                ServerMsg::Snapshot(snapshot) => {
                    theme = snapshot
                        .user_settings
                        .first()
                        .map(|setting| setting.value_json.clone());
                }
                ServerMsg::CommandResult {
                    seq: got, result, ..
                } if got == seq => {
                    result.unwrap();
                    acknowledged = true;
                }
                _ => {}
            }
        }
        theme.unwrap()
    }

    assert!(subscribe_and_theme(&mut first, 1).await.contains("Initial"));
    assert!(subscribe_and_theme(&mut second, 2)
        .await
        .contains("Initial"));
    env.daemon
        .set_user_terminal_theme(
            1,
            Some(&serde_json::to_vec(&native_terminal_theme("Updated elsewhere")).unwrap()),
        )
        .unwrap();

    async fn next_user_theme(ws: &mut WsStream) -> Option<String> {
        loop {
            if let ServerMsg::Event(Event::UserSettingChanged(setting)) = ws_next(ws).await.unwrap()
            {
                return setting.value_json;
            }
        }
    }

    assert!(next_user_theme(&mut first)
        .await
        .unwrap()
        .contains("Updated elsewhere"));
    assert!(next_user_theme(&mut second)
        .await
        .unwrap()
        .contains("Updated elsewhere"));
    env.daemon.set_user_terminal_theme(1, None).unwrap();
    assert_eq!(next_user_theme(&mut first).await, None);
    assert_eq!(next_user_theme(&mut second).await, None);
}

/// Registers over the host plane announcing `protocol_version` and returns
/// the controller's reply.
async fn register_announcing(env: &WsEnv, label: &str, protocol_version: u32) -> ControllerMsg {
    use futures::{SinkExt, StreamExt};

    let identity = pm_tls::Identity::generate().unwrap();
    let (enrollment, _) = env.daemon.create_worker_enrollment(label).unwrap();
    let mut worker = host_control_link(env, &identity, Some(&enrollment)).await;
    let register = WorkerMsg::Register {
        protocol_version,
        enrollment_token: enrollment,
        credential: String::new(),
        hostname: label.into(),
        platform: "test".into(),
        pm_version: String::new(),
        runtime: String::new(),
        container: String::new(),
        default_project_root: String::new(),
        live_sessions: Vec::new(),
        live_terminals: Vec::new(),
        pending_transcripts: Vec::new(),
        live_dir_shares: Vec::new(),
    };
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::worker_frame::encode_control(&register.encode_to_vec()).into(),
        ))
        .await
        .unwrap();
    let tungstenite::Message::Binary(frame) = worker.next().await.unwrap().unwrap() else {
        panic!("expected registration response");
    };
    let Some(pm_protocol::worker_frame::WorkerFrame::Control(payload)) =
        pm_protocol::worker_frame::decode(&frame)
    else {
        panic!("expected control response");
    };
    ControllerMsg::decode(payload).unwrap()
}

/// The URL a worker from before the local MCP relay composes: the host it
/// dialed paired with the port the controller reported.
fn legacy_worker_mcp_url(dialed_host: &str, mcp_base_url: &str, http_port: u32) -> String {
    if !mcp_base_url.is_empty() {
        return format!("{}/mcp", mcp_base_url.trim_end_matches('/'));
    }
    format!("http://{dialed_host}:{http_port}/mcp")
}

async fn tool_names(mcp_url: &str, session_token: &str) -> Vec<String> {
    let body: serde_json::Value = reqwest::Client::new()
        .post(mcp_url)
        .bearer_auth(session_token)
        .json(&serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.get("error").is_none(), "tools/list refused: {body}");
    body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_worker_before_the_local_relay_registers_and_reaches_working_mcp() {
    let env = ws_env().await;
    let previous = pm_protocol::WORKER_PROTOCOL_LOCAL_MCP_RELAY - 1;
    let ControllerMsg::Registered {
        worker_id,
        error,
        mcp_base_url,
        http_port,
        ..
    } = register_announcing(&env, "old-worker", previous).await
    else {
        panic!("expected a registration reply");
    };
    assert!(error.is_empty(), "an older worker must register: {error}");
    assert_ne!(worker_id, 0);
    assert_eq!(http_port, u32::from(env.addr.port()));

    let bucket = env.daemon.create_bucket("remote").unwrap();
    let project = env.daemon.create_project(bucket, "remote", "/tmp").unwrap();
    let session = env
        .daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "task",
            "prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let mcp_url = legacy_worker_mcp_url(&env.addr.ip().to_string(), &mcp_base_url, http_port);
    let tools = tool_names(&mcp_url, &token).await;
    assert!(
        tools.iter().any(|name| name == "report"),
        "the composed endpoint {mcp_url} must serve the agent tools, got {tools:?}"
    );
}

#[tokio::test]
async fn a_current_worker_is_not_handed_a_controller_mcp_address() {
    let env = ws_env().await;
    let ControllerMsg::Registered {
        error,
        mcp_base_url,
        http_port,
        ..
    } = register_announcing(&env, "current-worker", pm_protocol::WORKER_PROTOCOL_VERSION).await
    else {
        panic!("expected a registration reply");
    };
    assert!(error.is_empty(), "{error}");
    assert!(mcp_base_url.is_empty());
    assert_eq!(http_port, 0);
}

#[tokio::test]
async fn a_worker_newer_than_the_controller_still_registers() {
    let env = ws_env().await;
    let ControllerMsg::Registered {
        error, worker_id, ..
    } = register_announcing(
        &env,
        "newer-worker",
        pm_protocol::WORKER_PROTOCOL_VERSION + 1,
    )
    .await
    else {
        panic!("expected a registration reply");
    };
    assert!(error.is_empty(), "{error}");
    assert_ne!(worker_id, 0);
}

#[tokio::test]
async fn worker_transcript_upload_is_separate_and_acknowledged_after_persistence() {
    use futures::SinkExt;

    let env = ws_env().await;
    let bucket = env.daemon.create_bucket("remote").unwrap();
    let project_root = tempfile::tempdir().unwrap();
    let project = env
        .daemon
        .create_project(bucket, "remote", project_root.path().to_str().unwrap())
        .unwrap();
    let host_identity = pm_tls::Identity::generate().unwrap();
    let (enrollment, _) = env.daemon.create_worker_enrollment("remote").unwrap();
    let mut registration = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enrollment,
            credential: "",
            peer_key_hash: &host_identity.key_hash().to_hex(),
            hostname: "remote",
            platform: "test",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/tmp",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    env.daemon
        .set_bucket_workers(bucket, &[0, registration.worker_id], 0, None)
        .unwrap();
    env.daemon
        .set_project_workers(
            project,
            &[registration.worker_id],
            Some(registration.worker_id),
        )
        .unwrap();
    let session = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "remote",
            "remote",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            Some(registration.worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    let terminal_id = match registration.rx.recv().await.unwrap() {
        ControllerMsg::Spawn { terminal_id, .. } => terminal_id,
        message => panic!("unexpected message {message:?}"),
    };
    let transcript = b"durable remote transcript";
    env.daemon.apply_worker_message(
        registration.worker_id,
        WorkerMsg::TerminalExit {
            terminal_id,
            generation: 1,
            exit_code: Some(0),
            state: TerminalRunState::Exited,
            transcript_available: true,
            transcript_size: transcript.len() as u64,
            detail: String::new(),
        },
    );
    let token = match registration.rx.recv().await.unwrap() {
        ControllerMsg::Transcript {
            terminal_id: uploaded,
            generation: 1,
            token,
        } if uploaded == terminal_id => token,
        message => panic!("unexpected message {message:?}"),
    };
    let mut upload = host_dial(&env, &host_identity, "/worker/transcript", Some(&token))
        .await
        .expect("the enrolled host uploads its transcript")
        .link;
    upload
        .send(tungstenite::Message::Binary(bytes::Bytes::from_static(
            transcript,
        )))
        .await
        .unwrap();
    upload
        .send(tungstenite::Message::Close(None))
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(TEST_TIMEOUT, registration.rx.recv())
            .await
            .unwrap(),
        Some(ControllerMsg::TranscriptAck {
            terminal_id: acknowledged,
            generation: 1,
        }) if acknowledged == terminal_id
    ));
    assert_eq!(
        std::fs::read(env.daemon.terminal_scrollback_path(terminal_id, 1)).unwrap(),
        transcript
    );
    assert!(
        env.daemon
            .terminal(terminal_id)
            .unwrap()
            .scrollback_available
    );
    assert_eq!(
        env.daemon
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|item| item.id == session)
            .unwrap()
            .state,
        SessionState::Exited
    );
}

/// A session running on an enrolled remote host, with the host's control
/// channel held by the test so it can play the worker's side.
struct RemoteRelay {
    registration: pm_daemon::daemon::WorkerRegistration,
    host_identity: pm_tls::Identity,
    terminal_id: u64,
    session: u64,
    _root: tempfile::TempDir,
}

async fn remote_relay_terminal(env: &WsEnv) -> RemoteRelay {
    let bucket = env.daemon.create_bucket("relay").unwrap();
    let root = tempfile::tempdir().unwrap();
    let project = env
        .daemon
        .create_project(bucket, "relay", root.path().to_str().unwrap())
        .unwrap();
    let host_identity = pm_tls::Identity::generate().unwrap();
    let (enrollment, _) = env.daemon.create_worker_enrollment("relay").unwrap();
    let mut registration = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enrollment,
            credential: "",
            peer_key_hash: &host_identity.key_hash().to_hex(),
            hostname: "relay",
            platform: "test",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/tmp",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    env.daemon
        .set_bucket_workers(bucket, &[0, registration.worker_id], 0, None)
        .unwrap();
    env.daemon
        .set_project_workers(
            project,
            &[registration.worker_id],
            Some(registration.worker_id),
        )
        .unwrap();
    let session = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "relay",
            "relay",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            Some(registration.worker_id),
            true,
            false,
            None,
        )
        .unwrap();
    let terminal_id = match registration.rx.recv().await.unwrap() {
        ControllerMsg::Spawn { terminal_id, .. } => terminal_id,
        message => panic!("unexpected message {message:?}"),
    };
    RemoteRelay {
        registration,
        host_identity,
        terminal_id,
        session,
        _root: root,
    }
}

#[tokio::test]
async fn remote_terminal_bytes_use_the_dedicated_worker_stream_in_both_directions() {
    use futures::{SinkExt, StreamExt};

    let env = ws_env().await;
    let RemoteRelay {
        mut registration,
        host_identity,
        terminal_id,
        session,
        _root,
    } = remote_relay_terminal(&env).await;
    let mut browser = terminal_ws_connect(&env, terminal_id, 1).await;
    let (token, replay_bytes) = match tokio::time::timeout(TEST_TIMEOUT, registration.rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerMsg::TerminalAttach {
            terminal_id: attached,
            generation: 1,
            token,
            replay_bytes,
            ..
        } if attached == terminal_id => (token, replay_bytes),
        message => panic!("unexpected message {message:?}"),
    };
    assert!(replay_bytes > 0);
    let mut worker = host_dial(&env, &host_identity, "/worker/terminal", Some(&token))
        .await
        .expect("the enrolled host streams its terminal")
        .link;
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(
                1,
                pm_protocol::terminal_frame::FLAG_REPLAY
                    | pm_protocol::terminal_frame::FLAG_REPLAY_START,
                b"remote ",
            ),
        ))
        .await
        .unwrap();
    terminal_wait_for_text(&mut browser, b"remote ").await;
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(
                1,
                pm_protocol::terminal_frame::FLAG_REPLAY
                    | pm_protocol::terminal_frame::FLAG_REPLAY_END,
                b"replay",
            ),
        ))
        .await
        .unwrap();
    terminal_wait_for_replay(&mut browser).await;

    browser
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_input(1, b"browser input"),
        ))
        .await
        .unwrap();
    let worker_input = tokio::time::timeout(TEST_TIMEOUT, worker.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        worker_input,
        tungstenite::Message::Binary(ref frame)
            if matches!(
                pm_protocol::terminal_frame::decode(frame),
                Some(pm_protocol::terminal_frame::TerminalFrame::Input {
                    generation: 1,
                    submitted: false,
                    data: b"browser input",
                })
            )
    ));
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(1, 0, b"worker output"),
        ))
        .await
        .unwrap();
    terminal_wait_for_text(&mut browser, b"worker output").await;

    worker.close(None).await.unwrap();
    let browser_closed = tokio::time::timeout(TEST_TIMEOUT, async {
        while let Some(message) = browser.next().await {
            if matches!(message.unwrap(), tungstenite::Message::Close(_)) {
                return true;
            }
        }
        true
    })
    .await
    .unwrap();
    assert!(browser_closed);
    assert_eq!(
        env.daemon
            .subscribe()
            .0
            .sessions
            .into_iter()
            .find(|item| item.id == session)
            .unwrap()
            .state,
        SessionState::Working
    );
}

async fn terminal_next_binary(ws: &mut WsStream) -> Vec<u8> {
    use futures::StreamExt;
    loop {
        let message = tokio::time::timeout(TEST_TIMEOUT, ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tungstenite::Message::Binary(frame) = message {
            return frame.to_vec();
        }
    }
}

fn is_size_echo(frame: &[u8], cols: u16, rows: u16) -> bool {
    matches!(
        pm_protocol::terminal_frame::decode(frame),
        Some(pm_protocol::terminal_frame::TerminalFrame::Resize { cols: c, rows: r, .. })
            if (c, r) == (cols, rows)
    )
}

#[tokio::test]
async fn a_remote_web_attach_has_the_worker_snapshot_at_the_viewers_size() {
    use futures::SinkExt;
    const COLS: u16 = 150;
    const ROWS: u16 = 30;

    let env = ws_env().await;
    let RemoteRelay {
        mut registration,
        host_identity,
        terminal_id,
        _root,
        ..
    } = remote_relay_terminal(&env).await;
    let mut browser = terminal_ws_connect_with_size(&env, terminal_id, 1, COLS, ROWS).await;
    let (token, size) = match tokio::time::timeout(TEST_TIMEOUT, registration.rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerMsg::TerminalAttach {
            terminal_id: attached,
            token,
            size,
            ..
        } if attached == terminal_id => (token, size),
        message => panic!("unexpected message {message:?}"),
    };
    assert_eq!(
        size,
        (COLS, ROWS),
        "the worker must snapshot at the size the controller's mirror parses it at"
    );
    assert!(is_size_echo(
        &terminal_next_binary(&mut browser).await,
        COLS,
        ROWS
    ));

    // A row exactly as wide as the PTY leaves the cursor on the next row at
    // this width, and one row further down at any narrower one.
    let mut screen = "A".repeat(usize::from(COLS)).into_bytes();
    screen.extend_from_slice(b"\r\nB");
    let mut worker = host_dial(&env, &host_identity, "/worker/terminal", Some(&token))
        .await
        .expect("the enrolled host streams its terminal")
        .link;
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(
                1,
                pm_protocol::terminal_frame::FLAG_REPLAY
                    | pm_protocol::terminal_frame::FLAG_REPLAY_START
                    | pm_protocol::terminal_frame::FLAG_REPLAY_END
                    | pm_protocol::terminal_frame::FLAG_REPLAY_SNAPSHOT,
                &screen,
            ),
        ))
        .await
        .unwrap();
    terminal_wait_for_replay(&mut browser).await;

    let mut joined = terminal_ws_connect_with_size(&env, terminal_id, 1, COLS, ROWS).await;
    assert!(is_size_echo(
        &terminal_next_binary(&mut joined).await,
        COLS,
        ROWS
    ));
    let replay = terminal_next_binary(&mut joined).await;
    let Some(pm_protocol::terminal_frame::TerminalFrame::Output { data, .. }) =
        pm_protocol::terminal_frame::decode(&replay)
    else {
        panic!("the joined viewer's replay follows its size echo");
    };
    let snapshot = String::from_utf8_lossy(data);
    assert!(
        snapshot.ends_with("\x1b[2;2H\x1b[0m"),
        "the mirror laid the worker's screen out at another width: {snapshot:?}"
    );
}

#[tokio::test]
async fn ws_full_lifecycle_matches_unix_transport() {
    let env = ws_env().await;
    let tmp_project = tempfile::tempdir().unwrap();
    let mut ws = ws_connect(&env, true).await;

    let bucket_id = ws_request(
        &mut ws,
        1,
        ClientMsg::CreateBucket {
            name: "b".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let project_id = ws_request(
        &mut ws,
        2,
        ClientMsg::CreateProject {
            bucket_id,
            name: "p".into(),
            path: tmp_project.path().display().to_string(),
            worker_id: Some(0),
            allowed_worker_ids: vec![0],
        },
    )
    .await
    .unwrap()
    .unwrap();

    ws_send(&mut ws, 3, ClientMsg::Subscribe { scope: Scope::All }).await;
    loop {
        if let Some(ServerMsg::Snapshot(s)) = ws_next(&mut ws).await {
            assert_eq!(s.projects.len(), 1);
            assert_eq!(s.projects[0].worker_id, Some(0));
            break;
        }
    }

    let session_id = ws_request(
        &mut ws,
        4,
        ClientMsg::SpawnSession {
            project_id,
            agent: Some(AgentKind::Test),
            task_title: "t".into(),
            task_prompt: "ws-prompt".into(),
            cwd: String::new(),
            permission_mode: pm_protocol::domain::PermissionMode::Inherit,
            worker_id: None,
            items_api: true,
            supervisor_api: false,
            model_profile_id: None,
            host: String::new(),
            initial_cols: None,
            initial_rows: None,
        },
    )
    .await
    .unwrap()
    .unwrap();

    let terminal = env
        .daemon
        .subscribe()
        .0
        .terminals
        .into_iter()
        .find(|terminal| terminal.session_id == session_id)
        .unwrap();
    let mut terminal_ws = terminal_ws_connect(&env, terminal.id, terminal.generation).await;
    use futures::{SinkExt, StreamExt};
    loop {
        let message = terminal_ws.next().await.unwrap().unwrap();
        let tungstenite::Message::Binary(frame) = message else {
            continue;
        };
        let Some(pm_protocol::terminal_frame::TerminalFrame::Output { flags, .. }) =
            pm_protocol::terminal_frame::decode(&frame)
        else {
            continue;
        };
        if flags & pm_protocol::terminal_frame::FLAG_REPLAY_END != 0 {
            break;
        }
    }
    terminal_ws
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_input(
                terminal.generation,
                b"echo over-websocket\nexit 0\n",
            ),
        ))
        .await
        .unwrap();

    let mut acc = Vec::new();
    loop {
        let message = tokio::time::timeout(TEST_TIMEOUT, terminal_ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tungstenite::Message::Binary(frame) = message {
            if let Some(pm_protocol::terminal_frame::TerminalFrame::Output { data, .. }) =
                pm_protocol::terminal_frame::decode(&frame)
            {
                acc.extend_from_slice(data);
                if String::from_utf8_lossy(&acc).contains("OUT over-websocket") {
                    break;
                }
            }
        }
    }
    loop {
        if matches!(
            ws_next(&mut ws).await,
            Some(ServerMsg::Event(Event::SessionChanged(s)))
                if s.id == session_id && s.state == SessionState::Exited
        ) {
            break;
        }
    }
}

/// A handshake with an `Origin` of the caller's choosing, the way a page on
/// another origin opens one. Returns the refusal status when the upgrade
/// does not complete.
async fn ws_handshake_from_origin(
    env: &WsEnv,
    path: &str,
    origin: Option<&str>,
    subprotocol: Option<&str>,
) -> Result<WsStream, StatusCode> {
    use tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{}{path}", env.addr)
        .into_client_request()
        .unwrap();
    let headers = request.headers_mut();
    headers.insert(header::AUTHORIZATION, env.auth.parse().unwrap());
    if let Some(origin) = origin {
        headers.insert(header::ORIGIN, origin.parse().unwrap());
    }
    if let Some(subprotocol) = subprotocol {
        headers.insert(
            tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
            subprotocol.parse().unwrap(),
        );
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((stream, _)) => Ok(stream),
        Err(tungstenite::Error::Http(response)) => {
            Err(StatusCode::from_u16(response.status().as_u16()).unwrap())
        }
        Err(e) => panic!("handshake failed for a reason other than a status: {e}"),
    }
}

/// A page on another port of the same host is same-site with the dashboard,
/// so `SameSite=Lax` hands it the session cookie, and a WebSocket upgrade
/// has no preflight and no response gate to stop it borrowing that cookie.
/// Both refusals below turn into working sockets if the origin check goes.
#[tokio::test]
async fn a_handshake_from_another_origin_reaches_neither_the_control_socket_nor_a_terminal() {
    use futures::SinkExt;
    let env = ws_env().await;
    let tmp_project = tempfile::tempdir().unwrap();
    let bucket = env.daemon.create_bucket("origin-guard").unwrap();
    let project = env
        .daemon
        .create_project(bucket, "p", tmp_project.path().to_str().unwrap())
        .unwrap();
    let session_id = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
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
    let terminal_path = format!(
        "/ws/terminal/{}?generation={}",
        terminal.id, terminal.generation
    );

    // An unrelated listener on the same host, and the opaque origin a
    // sandboxed document sends.
    for origin in ["http://127.0.0.1:3999", "null"] {
        assert_eq!(
            ws_handshake_from_origin(&env, "/ws", Some(origin), None)
                .await
                .err(),
            Some(StatusCode::FORBIDDEN),
            "the control socket accepted a handshake from {origin}"
        );
        assert_eq!(
            ws_handshake_from_origin(&env, &terminal_path, Some(origin), Some("pm-terminal-v1"))
                .await
                .err(),
            Some(StatusCode::FORBIDDEN),
            "the terminal socket accepted a handshake from {origin}"
        );
    }

    // The dashboard's own page sends an `Origin` too, and still reads and
    // writes the terminal.
    let own = format!("http://{}", env.addr);
    assert!(ws_handshake_from_origin(&env, "/ws", Some(&own), None)
        .await
        .is_ok());
    let mut terminal_ws =
        ws_handshake_from_origin(&env, &terminal_path, Some(&own), Some("pm-terminal-v1"))
            .await
            .expect("the dashboard's own origin reaches the terminal");
    terminal_wait_for_replay(&mut terminal_ws).await;
    terminal_ws
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_input(
                terminal.generation,
                b"echo same-origin-reaches-the-pty\n",
            ),
        ))
        .await
        .unwrap();
    terminal_wait_for_text(&mut terminal_ws, b"OUT same-origin-reaches-the-pty").await;
}

#[tokio::test]
async fn terminal_ws_attach_echoes_pty_size_before_replay_and_broadcasts_changes() {
    use futures::{SinkExt, StreamExt};
    let env = ws_env().await;
    let tmp_project = tempfile::tempdir().unwrap();
    let bucket = env.daemon.create_bucket("size-echo").unwrap();
    let project = env
        .daemon
        .create_project(bucket, "p", tmp_project.path().to_str().unwrap())
        .unwrap();
    let session_id = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
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

    let mut first = terminal_ws_connect(&env, terminal.id, terminal.generation).await;
    let frame = loop {
        let message = tokio::time::timeout(TEST_TIMEOUT, first.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tungstenite::Message::Binary(frame) = message {
            break frame;
        }
    };
    let Some(pm_protocol::terminal_frame::TerminalFrame::Resize {
        generation,
        cols,
        rows,
    }) = pm_protocol::terminal_frame::decode(&frame)
    else {
        panic!("attach must deliver the PTY size before replay");
    };
    assert_eq!(generation, terminal.generation);
    assert!(cols > 0 && rows > 0);
    terminal_wait_for_replay(&mut first).await;

    let mut second = terminal_ws_connect(&env, terminal.id, terminal.generation).await;
    terminal_wait_for_replay(&mut second).await;
    second
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_resize(terminal.generation, 91, 33),
        ))
        .await
        .unwrap();
    let echoed = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let Some(Ok(tungstenite::Message::Binary(frame))) = first.next().await else {
                continue;
            };
            if let Some(pm_protocol::terminal_frame::TerminalFrame::Resize { cols, rows, .. }) =
                pm_protocol::terminal_frame::decode(&frame)
            {
                return (cols, rows);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(echoed, (91, 33));
}

#[tokio::test]
async fn terminal_ws_attach_with_initial_size_resizes_pty_and_echoes_size_before_replay() {
    use futures::StreamExt;
    let env = ws_env().await;
    let tmp_project = tempfile::tempdir().unwrap();
    let bucket = env.daemon.create_bucket("initial-size").unwrap();
    let project = env
        .daemon
        .create_project(bucket, "p", tmp_project.path().to_str().unwrap())
        .unwrap();
    let session_id = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "t",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
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

    let mut ws =
        terminal_ws_connect_with_size(&env, terminal.id, terminal.generation, 165, 45).await;
    let frame = loop {
        let message = tokio::time::timeout(TEST_TIMEOUT, ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tungstenite::Message::Binary(frame) = message {
            break frame;
        }
    };
    let Some(pm_protocol::terminal_frame::TerminalFrame::Resize {
        generation,
        cols,
        rows,
    }) = pm_protocol::terminal_frame::decode(&frame)
    else {
        panic!("attach must deliver the PTY size before replay");
    };
    assert_eq!(generation, terminal.generation);
    assert_eq!(cols, 165);
    assert_eq!(rows, 45);
    terminal_wait_for_replay(&mut ws).await;
}

#[tokio::test]
async fn a_blocked_terminal_socket_cannot_delay_another_terminal_or_control() {
    use futures::SinkExt;

    let env = ws_env().await;
    let project = tempfile::tempdir().unwrap();
    let mut control = ws_connect(&env, true).await;
    let bucket_id = ws_request(
        &mut control,
        1,
        ClientMsg::CreateBucket {
            name: "hol-bucket".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let project_id = ws_request(
        &mut control,
        2,
        ClientMsg::CreateProject {
            bucket_id,
            name: "hol-project".into(),
            path: project.path().display().to_string(),
            worker_id: None,
            allowed_worker_ids: vec![0],
        },
    )
    .await
    .unwrap()
    .unwrap();
    let spawn = |title: &str| ClientMsg::SpawnSession {
        project_id,
        agent: Some(AgentKind::Test),
        task_title: title.into(),
        task_prompt: title.into(),
        cwd: String::new(),
        permission_mode: pm_protocol::domain::PermissionMode::Inherit,
        worker_id: None,
        items_api: true,
        supervisor_api: false,
        model_profile_id: None,
        host: String::new(),
        initial_cols: None,
        initial_rows: None,
    };
    let first = ws_request(&mut control, 3, spawn("first"))
        .await
        .unwrap()
        .unwrap();
    let second = ws_request(&mut control, 4, spawn("second"))
        .await
        .unwrap()
        .unwrap();
    let snapshot = env.daemon.subscribe().0;
    let first_terminal = snapshot
        .terminals
        .iter()
        .find(|terminal| terminal.session_id == first)
        .unwrap();
    let second_terminal = snapshot
        .terminals
        .iter()
        .find(|terminal| terminal.session_id == second)
        .unwrap();
    let mut blocked = terminal_ws_connect(&env, first_terminal.id, first_terminal.generation).await;
    let mut responsive =
        terminal_ws_connect(&env, second_terminal.id, second_terminal.generation).await;
    terminal_wait_for_replay(&mut blocked).await;
    terminal_wait_for_replay(&mut responsive).await;
    blocked
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_input(
                first_terminal.generation,
                b"bigout 8000000\n",
            ),
        ))
        .await
        .unwrap();
    responsive
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_input(
                second_terminal.generation,
                b"echo independent\n",
            ),
        ))
        .await
        .unwrap();

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        terminal_wait_for_text(&mut responsive, b"OUT independent"),
    )
    .await
    .expect("second terminal queued behind blocked terminal");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        ws_request(
            &mut control,
            5,
            ClientMsg::CreateBucket {
                name: "control-still-responsive".into(),
                allowed_worker_ids: vec![0],
                default_worker_id: 0,
                is_default: false,
            },
        ),
    )
    .await
    .expect("control queued behind blocked terminal")
    .unwrap();
}

#[tokio::test]
async fn fs_endpoint_lists_only_subdirectories() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let app = pm_daemon::http::router(daemon);

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("alpha")).unwrap();
    std::fs::create_dir(root.path().join("beta")).unwrap();
    std::fs::create_dir(root.path().join(".hidden")).unwrap();
    std::fs::write(root.path().join("afile.txt"), b"x").unwrap();

    let uri = format!("/api/fs?path={}", root.path().display());
    let res = app
        .clone()
        .oneshot(
            Request::get(&uri)
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha", "beta"], "dirs only, no dotfiles, no files");
}

#[tokio::test]
async fn project_host_endpoint_names_the_condition_rather_than_calling_it_offline() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let bucket = daemon.create_bucket("primary").unwrap();
    let root = tempfile::tempdir().unwrap();
    let usable = daemon
        .create_project(bucket, "usable", root.path().to_str().unwrap())
        .unwrap();
    let gone = root.path().join("removed");
    let missing = daemon
        .create_project(bucket, "missing", gone.to_str().unwrap())
        .unwrap();
    let blanked = daemon.create_project(bucket, "blanked", "").unwrap();
    let app = pm_daemon::http::router(daemon);

    let ask = |project: u64| {
        let app = app.clone();
        let auth = auth.clone();
        async move {
            let res = app
                .oneshot(
                    Request::get(format!("/api/project-host?project={project}&worker=0"))
                        .header(header::AUTHORIZATION, &auth)
                        .header(header::HOST, TEST_HOST)
                        .header(header::ORIGIN, TEST_ORIGIN)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            body_json(res).await
        }
    };

    assert_eq!(ask(usable).await["status"], "ready");
    let missing = ask(missing).await;
    assert_eq!(missing["status"], "path-missing");
    assert_eq!(missing["path"], gone.to_string_lossy().into_owned());
    assert!(
        !missing["detail"].as_str().unwrap().contains("offline"),
        "the local host is up: {missing}"
    );
    // A project with no path of its own runs in the home directory of
    // the host it spawns on, so there is a path to report and it is
    // usable.
    let blanked = ask(blanked).await;
    assert_eq!(blanked["status"], "ready");
    assert_eq!(blanked["path"], std::env::var("HOME").unwrap_or_default());
}

#[tokio::test]
async fn project_host_endpoint_requires_auth() {
    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon);
    let res = app
        .oneshot(
            Request::get("/api/project-host?project=1&worker=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fs_endpoint_requires_auth() {
    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon);
    let res = app
        .oneshot(
            Request::get("/api/fs?path=/tmp")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn worker_enroll_endpoint_requires_auth() {
    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon);
    let res = app
        .oneshot(json_request(
            "POST",
            "/api/workers/enroll",
            serde_json::json!({"label": "build-box"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn worker_reenroll_endpoint_requires_auth_and_a_real_host() {
    let (daemon, _tmp) = test_daemon();
    let session = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let cookie = format!("pm_session={session}");
    let auth = dashboard_auth(&daemon, &session);
    let signed_in = |credential: (header::HeaderName, &str), uri: &str| {
        Request::post(uri)
            .header(credential.0, credential.1)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({}).to_string()))
            .unwrap()
    };
    let with_token = |uri: &str| signed_in((header::AUTHORIZATION, auth.as_str()), uri);

    // Re-enrolling rebinds a host's pinned key, so it takes the access
    // token that every other mutating route does. The session cookie
    // does not answer, because any page sharing the dashboard's origin
    // can send one.
    let res = pm_daemon::http::router(daemon.clone())
        .oneshot(signed_in(
            (header::COOKIE, cookie.as_str()),
            "/api/workers/3/reenroll",
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the session cookie does not answer for re-enrolling a host"
    );

    let res = pm_daemon::http::router(daemon.clone())
        .oneshot(json_request(
            "POST",
            "/api/workers/3/reenroll",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = pm_daemon::http::router(daemon.clone())
        .oneshot(with_token("/api/workers/3/reenroll"))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "a host that does not exist cannot be re-enrolled"
    );

    // The local host runs in the controller's own process, so it has no
    // enrollment to rotate.
    let res = pm_daemon::http::router(daemon)
        .oneshot(with_token("/api/workers/0/reenroll"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

/// Redeeming a re-enrollment token hands an existing trusted host to
/// whichever machine spends it: the id, name, project paths and history stay
/// put and the old key stops working. It takes the same access token that
/// removing the host outright does, and not the session cookie, which any
/// page sharing the dashboard's origin can send.
#[tokio::test]
async fn reenrolling_a_host_takes_an_access_token_not_the_cookie() {
    let env = ws_env().await;
    let host_key = pm_tls::Identity::generate().unwrap();
    let (enrollment, _) = env.daemon.create_worker_enrollment("build-box").unwrap();
    let worker_id = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enrollment,
            credential: "",
            peer_key_hash: &host_key.key_hash().to_hex(),
            hostname: "build-box",
            platform: "test",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/tmp",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap()
        .worker_id;

    let auth = env.auth.clone();
    let attempt = |body: serde_json::Value| {
        let daemon = env.daemon.clone();
        let auth = auth.clone();
        let uri = format!("/api/workers/{worker_id}/reenroll");
        async move {
            pm_daemon::http::router(daemon)
                .oneshot(
                    Request::post(&uri)
                        .header(header::AUTHORIZATION, &auth)
                        .header(header::HOST, TEST_HOST)
                        .header(header::ORIGIN, TEST_ORIGIN)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    // The session cookie alone is what a page sharing the dashboard's
    // origin can send, so it is the case this route must still refuse.
    let attempt_without_session = |body: serde_json::Value| {
        let daemon = env.daemon.clone();
        let cookie = env.cookie.clone();
        let uri = format!("/api/workers/{worker_id}/reenroll");
        async move {
            pm_daemon::http::router(daemon)
                .oneshot(
                    Request::post(&uri)
                        .header(header::COOKIE, cookie)
                        .header(header::HOST, TEST_HOST)
                        .header(header::ORIGIN, TEST_ORIGIN)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    let move_host = serde_json::json!({
        "connect_mode": "accept",
        "endpoint": "build-box.example:7677",
    });
    assert_eq!(
        attempt_without_session(move_host.clone()).await.status(),
        StatusCode::UNAUTHORIZED,
        "the session cookie alone must not rebind a host"
    );
    assert!(
        env.daemon.live_enrollment_tokens().is_empty(),
        "a refused request must not leave a redeemable token behind"
    );

    assert_eq!(attempt(move_host).await.status(), StatusCode::OK);
    assert_eq!(
        env.daemon.live_enrollment_tokens().len(),
        1,
        "the operator's own re-enrollment mints a token"
    );
}

/// What that token buys, so the cost of minting one is on the record: the
/// host id survives and the machine that held the old key is locked out.
#[tokio::test]
async fn a_redeemed_reenrollment_rebinds_the_host_and_locks_the_old_key_out() {
    let env = ws_env().await;
    let original = pm_tls::Identity::generate().unwrap();
    let (enrollment, _) = env.daemon.create_worker_enrollment("build-box").unwrap();
    fn hello<'a>(token: &'a str, key: &'a str) -> pm_daemon::daemon::WorkerHello<'a> {
        pm_daemon::daemon::WorkerHello {
            enrollment_token: token,
            credential: "",
            peer_key_hash: key,
            hostname: "build-box",
            platform: "test",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/tmp",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        }
    }
    let worker_id = env
        .daemon
        .register_worker_connection(hello(&enrollment, &original.key_hash().to_hex()))
        .unwrap()
        .worker_id;

    let (reenrollment, _) = env
        .daemon
        .create_worker_reenrollment_with_mode(worker_id, pm_protocol::domain::ConnectMode::Dial, "")
        .unwrap();
    let replacement = pm_tls::Identity::generate().unwrap();
    let rebound = env
        .daemon
        .register_worker_connection(hello(&reenrollment, &replacement.key_hash().to_hex()))
        .unwrap()
        .worker_id;
    assert_eq!(
        rebound, worker_id,
        "re-enrollment rotates the host in place rather than adding a row"
    );

    let displaced = env
        .daemon
        .register_worker_connection(hello("", &original.key_hash().to_hex()));
    assert!(
        displaced.is_err(),
        "the machine that held the old key must no longer be this host"
    );
}

/// Adding a host the controller dials has to record the address, or the
/// controller has nothing to dial and the host is silently recorded as one
/// that dials out.
#[tokio::test]
async fn enrolling_a_dialed_host_records_its_address_and_needs_one() {
    let (daemon, _tmp) = test_daemon();
    let session = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &session);

    let response = pm_daemon::http::router(daemon.clone())
        .oneshot(
            Request::post("/api/workers/enroll")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "label": "dmz-box",
                        "connect_mode": "accept",
                        "endpoint": "dmz-box.internal:7678"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let token = body_json(response).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let pending = daemon
        .subscribe()
        .0
        .workers
        .into_iter()
        .find(|worker| worker.name == "dmz-box")
        .expect("adding a Host publishes its pending row immediately");
    assert!(!pending.online);
    assert_eq!(pending.hostname, "");
    assert_eq!(pending.last_seen_at_unix_ms, None);
    assert_eq!(
        pending.connect_mode,
        pm_protocol::domain::ConnectMode::Accept
    );
    assert_eq!(pending.endpoint, "dmz-box.internal:7678");

    let registered = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-dmz",
            hostname: "dmz-box",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/srv",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    assert_eq!(
        registered.worker_id, pending.id,
        "first registration fills the pending Host rather than adding another"
    );
    let host = daemon
        .subscribe()
        .0
        .workers
        .into_iter()
        .find(|w| w.id == registered.worker_id)
        .unwrap();
    assert_eq!(host.connect_mode, pm_protocol::domain::ConnectMode::Accept);
    assert_eq!(host.endpoint, "dmz-box.internal:7678");

    let without_address = pm_daemon::http::router(daemon)
        .oneshot(
            Request::post("/api/workers/enroll")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"label": "nowhere", "connect_mode": "accept"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        without_address.status(),
        StatusCode::BAD_REQUEST,
        "a host the controller dials needs an address to dial"
    );
}

#[tokio::test]
async fn worker_enroll_mints_a_single_use_token_with_a_bounded_expiry() {
    fn hello(token: &str) -> pm_daemon::daemon::WorkerHello<'_> {
        pm_daemon::daemon::WorkerHello {
            enrollment_token: token,
            credential: "",
            peer_key_hash: "key-build-box.local",
            hostname: "build-box.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        }
    }

    let (daemon, _tmp) = test_daemon();
    let session = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &session);
    let app = pm_daemon::http::router(daemon.clone());

    let response = app
        .oneshot(
            Request::post("/api/workers/enroll")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"label": "build-box"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let minted = body_json(response).await;
    let enroll_token = minted["token"].as_str().unwrap().to_string();
    assert!(!enroll_token.is_empty());
    let pending_id = daemon
        .subscribe()
        .0
        .workers
        .into_iter()
        .find(|worker| worker.name == "build-box")
        .expect("the pending Host is visible before enrollment")
        .id;

    let now_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    // The mint response advertises a short validity window, not an
    // open-ended credential.
    const MAX_ADVERTISED_TTL_MS: i64 = 60 * 60 * 1000;
    let expires_at = minted["expiresAtUnixMs"].as_i64().unwrap();
    assert!(expires_at > now_unix_ms, "expiry is in the future");
    assert!(
        expires_at <= now_unix_ms + MAX_ADVERTISED_TTL_MS,
        "expiry stays within a short validity window"
    );

    let registration = daemon
        .register_worker_connection(hello(&enroll_token))
        .unwrap();
    assert_eq!(registration.worker_id, pending_id);
    assert!(!registration.credential.is_empty());
    assert!(
        daemon
            .register_worker_connection(hello(&enroll_token))
            .is_err(),
        "a consumed enrollment token does not enroll a second worker"
    );
}

#[tokio::test]
async fn fs_endpoint_rejects_disabled_local_worker() {
    let (daemon, _tmp) = test_daemon_with_local_worker(false);
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let app = pm_daemon::http::router(daemon);
    let res = app
        .oneshot(
            Request::get("/api/fs?path=/tmp")
                .header(header::AUTHORIZATION, auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res).await["error"],
        "local worker is disabled for this daemon"
    );
}

#[tokio::test]
async fn reports_endpoint_returns_history() {
    let (daemon, tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);

    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", tmp.path().to_str().unwrap())
        .unwrap();
    let sid = daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "t",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let stoken = daemon.session_token(sid).unwrap().unwrap();
    daemon
        .handle_agent_report(
            &stoken,
            pm_daemon::daemon::AgentReport::Report {
                goal: String::new(),
                headline: "building".into(),
                summary: None,
                note: "compiling".into(),
                glance: None,
                context: None,
                clear: Vec::new(),
                git: Default::default(),
            },
        )
        .unwrap();

    let app = pm_daemon::http::router(daemon);
    let res = app
        .oneshot(
            Request::get(format!("/api/sessions/{sid}/reports"))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    let reports = body["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["kind"], "checkpoint");
    assert_eq!(reports[0]["payload"]["headline"], "building");
    assert_eq!(reports[0]["payload"]["note"], "compiling");
}

#[tokio::test]
async fn spawn_honors_cwd_override() {
    let env_tmp = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let override_dir = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: env_tmp.path().join("pm.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: env_tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _rx) = Daemon::new(config).unwrap();
    let bucket = daemon.create_bucket("b").unwrap();
    let project = daemon
        .create_project(bucket, "p", project_dir.path().to_str().unwrap())
        .unwrap();
    // The scripted agent prints its cwd via pwd; here we just confirm
    // spawn with an override succeeds and the session is live.
    let sid = daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "t",
            "p",
            Some(override_dir.path().to_str().unwrap()),
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    assert!(daemon.mux.is_running(sid));
}

#[tokio::test]
async fn settings_are_listed_and_updated_over_http() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());

    let res = app
        .clone()
        .oneshot(Request::get("/api/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let auth = dashboard_auth_from(&daemon, &res);
    let authed_get = || {
        Request::get("/api/settings")
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap()
    };

    let res = app.clone().oneshot(authed_get()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let listed = body_json(res).await;
    assert_eq!(listed["settings"][0]["key"], "spawn.truecolor");
    assert_eq!(listed["settings"][0]["value"], "true");
    assert_eq!(listed["settings"][0]["set"], false);

    let mut update = json_request(
        "PUT",
        "/api/settings/spawn.truecolor",
        serde_json::json!({"value": "false"}),
    );
    update
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    let res = app.clone().oneshot(update).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = app.clone().oneshot(authed_get()).await.unwrap();
    let listed = body_json(res).await;
    assert_eq!(listed["settings"][0]["value"], "false");
    assert_eq!(listed["settings"][0]["set"], true);

    // Invalid values are refused; null resets to the default.
    let mut bad = json_request(
        "PUT",
        "/api/settings/spawn.truecolor",
        serde_json::json!({"value": "maybe"}),
    );
    bad.headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    let res = app.clone().oneshot(bad).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let mut reset = json_request(
        "PUT",
        "/api/settings/spawn.truecolor",
        serde_json::json!({"value": null}),
    );
    reset
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    let res = app.clone().oneshot(reset).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let res = app.clone().oneshot(authed_get()).await.unwrap();
    assert_eq!(body_json(res).await["settings"][0]["set"], false);
}

#[tokio::test]
async fn authenticated_user_terminal_theme_persists_validates_and_resets() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());

    for request in [
        Request::get("/api/user/settings")
            .body(Body::empty())
            .unwrap(),
        json_request(
            "PUT",
            "/api/user/settings/terminal-theme",
            native_terminal_theme("Night"),
        ),
        Request::delete("/api/user/settings/terminal-theme")
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    let setup = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let auth = dashboard_auth_from(&daemon, &setup);
    let mut apply = json_request(
        "PUT",
        "/api/user/settings/terminal-theme",
        native_terminal_theme("Night"),
    );
    apply
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    let applied = app.clone().oneshot(apply).await.unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
    assert_eq!(
        body_json(applied).await["terminalTheme"]["colors"]["foreground"],
        "#c9ceda",
        "server returns its normalized canonical value"
    );

    let get = |auth: &str| {
        Request::get("/api/user/settings")
            .header(header::AUTHORIZATION, auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["terminalTheme"]["name"],
        "Night"
    );

    // A malformed update is refused and leaves the previous row intact.
    let mut malformed_theme = native_terminal_theme("Broken");
    malformed_theme["colors"]["unknown"] = serde_json::json!("#ffffff");
    let mut malformed = json_request("PUT", "/api/user/settings/terminal-theme", malformed_theme);
    malformed
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(malformed).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["terminalTheme"]["name"],
        "Night"
    );

    // The preference survives a fresh login/session (and therefore a new
    // browser/device using the same account).
    let login = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/login",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let second_auth = dashboard_auth_from(&daemon, &login);
    assert_eq!(
        body_json(app.clone().oneshot(get(&second_auth)).await.unwrap()).await["terminalTheme"]
            ["name"],
        "Night"
    );

    let reset = Request::delete("/api/user/settings/terminal-theme")
        .header(header::AUTHORIZATION, &second_auth)
        .header(header::HOST, TEST_HOST)
        .header(header::ORIGIN, TEST_ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(reset).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert!(
        body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["terminalTheme"].is_null()
    );

    let oversized = Request::put("/api/user/settings/terminal-theme")
        .header(header::AUTHORIZATION, &auth)
        .header(header::HOST, TEST_HOST)
        .header(header::ORIGIN, TEST_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(vec![b' '; 64 * 1024 + 1]))
        .unwrap();
    assert_eq!(
        app.oneshot(oversized).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn authenticated_push_web_idle_minutes_persists_and_validates() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());

    assert_eq!(
        app.clone()
            .oneshot(json_request(
                "PUT",
                "/api/user/settings/push-web-idle-minutes",
                serde_json::json!(5),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let setup = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let auth = dashboard_auth_from(&daemon, &setup);
    let get = || {
        Request::get("/api/user/settings")
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap()
    };
    let put = |value: serde_json::Value| {
        let mut request = json_request("PUT", "/api/user/settings/push-web-idle-minutes", value);
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, auth.parse().unwrap());
        request
    };

    assert!(
        body_json(app.clone().oneshot(get()).await.unwrap()).await["pushWebIdleMinutes"].is_null(),
        "no stored row means the built-in default applies"
    );

    let applied = app
        .clone()
        .oneshot(put(serde_json::json!(5)))
        .await
        .unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
    assert_eq!(body_json(applied).await["pushWebIdleMinutes"], 5);
    assert_eq!(
        body_json(app.clone().oneshot(get()).await.unwrap()).await["pushWebIdleMinutes"],
        5
    );

    assert_eq!(
        app.clone()
            .oneshot(put(serde_json::json!(0)))
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "zero is a valid choice: it turns the gate off"
    );

    for rejected in [
        serde_json::json!(-1),
        serde_json::json!(1441),
        serde_json::json!(1.5),
        serde_json::json!("5"),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(put(rejected.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "accepted {rejected}"
        );
    }
    assert_eq!(
        body_json(app.oneshot(get()).await.unwrap()).await["pushWebIdleMinutes"],
        0,
        "a refused update leaves the stored row intact"
    );
}

#[tokio::test]
async fn authenticated_user_appearance_persists_validates_and_resets() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());

    for request in [
        json_request(
            "PUT",
            "/api/user/settings/appearance",
            serde_json::json!("light"),
        ),
        Request::delete("/api/user/settings/appearance")
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    let setup = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let auth = dashboard_auth_from(&daemon, &setup);
    let get = |auth: &str| {
        Request::get("/api/user/settings")
            .header(header::AUTHORIZATION, auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap()
    };

    // No stored value is how the browser is told to follow the system.
    assert!(
        body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["appearance"].is_null()
    );

    let mut apply = json_request(
        "PUT",
        "/api/user/settings/appearance",
        serde_json::json!("light"),
    );
    apply
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    let applied = app.clone().oneshot(apply).await.unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
    assert_eq!(body_json(applied).await["appearance"], "light");
    assert_eq!(
        body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["appearance"],
        "light"
    );

    // Anything but the two explicit choices is refused, and the stored
    // choice survives the attempt.
    for rejected in [
        serde_json::json!("system"),
        serde_json::json!("Light"),
        serde_json::json!(null),
        serde_json::json!({"appearance": "dark"}),
    ] {
        let mut bad = json_request("PUT", "/api/user/settings/appearance", rejected.clone());
        bad.headers_mut()
            .insert(header::AUTHORIZATION, auth.parse().unwrap());
        assert_eq!(
            app.clone().oneshot(bad).await.unwrap().status(),
            StatusCode::BAD_REQUEST,
            "accepted {rejected}"
        );
    }
    assert_eq!(
        body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["appearance"],
        "light"
    );

    // The choice belongs to the account, not to the browser that made it.
    let login = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/login",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let second_auth = dashboard_auth_from(&daemon, &login);
    assert_eq!(
        body_json(app.clone().oneshot(get(&second_auth)).await.unwrap()).await["appearance"],
        "light"
    );

    // Applying the terminal theme leaves the appearance alone: they are
    // two settings, not one.
    let mut theme = json_request(
        "PUT",
        "/api/user/settings/terminal-theme",
        native_terminal_theme("Night"),
    );
    theme
        .headers_mut()
        .insert(header::AUTHORIZATION, auth.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(theme).await.unwrap().status(),
        StatusCode::OK
    );
    let both = body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await;
    assert_eq!(both["appearance"], "light");
    assert_eq!(both["terminalTheme"]["name"], "Night");

    let reset = Request::delete("/api/user/settings/appearance")
        .header(header::AUTHORIZATION, &auth)
        .header(header::HOST, TEST_HOST)
        .header(header::ORIGIN, TEST_ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(reset).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    let after = body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await;
    assert!(after["appearance"].is_null());
    assert_eq!(after["terminalTheme"]["name"], "Night");

    let oversized = Request::put("/api/user/settings/appearance")
        .header(header::AUTHORIZATION, &auth)
        .header(header::HOST, TEST_HOST)
        .header(header::ORIGIN, TEST_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(vec![b' '; 64]))
        .unwrap();
    assert_eq!(
        app.oneshot(oversized).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn user_ui_theme_api_requires_auth_accepts_valid_choices_and_cleans_up() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());

    for request in [
        json_request(
            "PUT",
            "/api/user/settings/ui-theme",
            serde_json::json!("compact"),
        ),
        Request::delete("/api/user/settings/ui-theme")
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    let setup = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let auth = dashboard_auth_from(&daemon, &setup);
    let get = |auth: &str| {
        Request::get("/api/user/settings")
            .header(header::AUTHORIZATION, auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap()
    };

    assert!(body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["uiTheme"].is_null());

    // "compact" is no longer offered but stays a valid stored value.
    for accepted in ["standard", "graphite", "studio", "compact"] {
        let mut apply = json_request(
            "PUT",
            "/api/user/settings/ui-theme",
            serde_json::json!(accepted),
        );
        apply
            .headers_mut()
            .insert(header::AUTHORIZATION, auth.parse().unwrap());
        let applied = app.clone().oneshot(apply).await.unwrap();
        assert_eq!(applied.status(), StatusCode::OK, "rejected {accepted}");
        assert_eq!(body_json(applied).await["uiTheme"], accepted);
        assert_eq!(
            body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["uiTheme"],
            accepted
        );
    }

    for rejected in [
        serde_json::json!("modern"),
        serde_json::json!("midnight"),
        serde_json::json!("Graphite"),
        serde_json::json!("Compact"),
        serde_json::json!(null),
        serde_json::json!({"theme": "compact"}),
    ] {
        let mut bad = json_request("PUT", "/api/user/settings/ui-theme", rejected.clone());
        bad.headers_mut()
            .insert(header::AUTHORIZATION, auth.parse().unwrap());
        let rejected_response = app.clone().oneshot(bad).await.unwrap();
        assert_eq!(rejected_response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await["uiTheme"],
            "compact"
        );
    }

    let reset = Request::delete("/api/user/settings/ui-theme")
        .header(header::AUTHORIZATION, &auth)
        .header(header::HOST, TEST_HOST)
        .header(header::ORIGIN, TEST_ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(reset).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    let after = body_json(app.clone().oneshot(get(&auth)).await.unwrap()).await;
    assert!(after["uiTheme"].is_null());

    let oversized = Request::put("/api/user/settings/ui-theme")
        .header(header::AUTHORIZATION, &auth)
        .header(header::HOST, TEST_HOST)
        .header(header::ORIGIN, TEST_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(vec![b' '; 64]))
        .unwrap();
    assert_eq!(
        app.oneshot(oversized).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn user_terminal_theme_api_never_crosses_authenticated_users() {
    use argon2::password_hash::{PasswordHasher, SaltString};
    use argon2::Argon2;
    use rand::rngs::OsRng;

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("multi-user.db");
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: Some(db_path.clone()),
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _) = Daemon::new(config).unwrap();
    let alice_token = daemon.auth_setup("alice", "alice-password").unwrap();
    let bob_hash = Argon2::default()
        .hash_password(b"bob-password", &SaltString::generate(&mut OsRng))
        .unwrap()
        .to_string();
    let storage = pm_daemon::storage::Storage::open(&db_path).unwrap();
    storage.create_user("bob", &bob_hash, 2).unwrap();
    drop(storage);
    let bob_token = daemon.auth_login("bob", "bob-password").unwrap();
    let daemon = Arc::new(daemon);
    let alice_auth = dashboard_auth(&daemon, &alice_token);
    let bob_auth = dashboard_auth(&daemon, &bob_token);
    let app = pm_daemon::http::router(daemon);

    let mut alice_apply = json_request(
        "PUT",
        "/api/user/settings/terminal-theme",
        native_terminal_theme("Alice only"),
    );
    alice_apply
        .headers_mut()
        .insert(header::AUTHORIZATION, alice_auth.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(alice_apply).await.unwrap().status(),
        StatusCode::OK
    );

    let get = |auth: &str| {
        Request::get("/api/user/settings")
            .header(header::AUTHORIZATION, auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        body_json(app.clone().oneshot(get(&alice_auth)).await.unwrap()).await["terminalTheme"]
            ["name"],
        "Alice only"
    );
    assert!(
        body_json(app.clone().oneshot(get(&bob_auth)).await.unwrap()).await["terminalTheme"]
            .is_null()
    );

    let mut bob_apply = json_request(
        "PUT",
        "/api/user/settings/terminal-theme",
        native_terminal_theme("Bob only"),
    );
    bob_apply
        .headers_mut()
        .insert(header::AUTHORIZATION, bob_auth.parse().unwrap());
    assert_eq!(
        app.clone().oneshot(bob_apply).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        body_json(app.clone().oneshot(get(&alice_auth)).await.unwrap()).await["terminalTheme"]
            ["name"],
        "Alice only"
    );
    assert_eq!(
        body_json(app.oneshot(get(&bob_auth)).await.unwrap()).await["terminalTheme"]["name"],
        "Bob only"
    );
}

fn bearer_request(method: &str, path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn mobile_enroll_body(extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "deviceId": "app-install-1",
        "name": "test phone",
        "platform": "ios",
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

#[tokio::test]
async fn mobile_enroll_with_password_and_bearer_auth() {
    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon.clone());

    let bad = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/mobile/devices/enroll",
            mobile_enroll_body(
                serde_json::json!({"username": "testuser", "password": "wrong-password"}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

    let missing_proof = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/mobile/devices/enroll",
            mobile_enroll_body(serde_json::json!({"username": "testuser"})),
        ))
        .await
        .unwrap();
    assert_eq!(missing_proof.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/mobile/devices/enroll",
            mobile_enroll_body(
                serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let enrolled = body_json(res).await;
    // u64 ids are JSON strings so JavaScript clients keep full precision.
    assert!(enrolled["device"]["id"].is_string());
    assert_eq!(enrolled["device"]["appInstallationId"], "app-install-1");
    assert_eq!(
        enrolled["installationId"],
        body_json(
            app.clone()
                .oneshot(Request::get("/api/version").body(Body::empty()).unwrap())
                .await
                .unwrap()
        )
        .await["installationId"]
    );
    let access = enrolled["tokens"]["accessToken"]
        .as_str()
        .unwrap()
        .to_string();

    // The bearer token authenticates ordinary API endpoints...
    let me = app
        .clone()
        .oneshot(bearer_request("GET", "/api/me", &access))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(body_json(me).await["username"], "testuser");

    // ...and the device list, which carries the audit timestamps.
    let list = app
        .clone()
        .oneshot(bearer_request("GET", "/api/mobile/devices", &access))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let devices = body_json(list).await["devices"].clone();
    assert_eq!(devices.as_array().unwrap().len(), 1);
    assert!(devices[0]["createdAtUnixMs"].is_i64());
    assert!(devices[0]["revokedAtUnixMs"].is_null());

    let bogus = app
        .clone()
        .oneshot(bearer_request("GET", "/api/mobile/devices", "not-a-token"))
        .await
        .unwrap();
    assert_eq!(bogus.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mobile_enrollment_token_flow_is_single_use() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());
    let setup = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/setup",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let auth = dashboard_auth_from(&daemon, &setup);

    let unauthenticated = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/mobile/devices/enroll-token",
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let minted = app
        .clone()
        .oneshot(
            Request::post("/api/mobile/devices/enroll-token")
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    let minted = body_json(minted).await;
    let token = minted["token"].as_str().unwrap().to_string();
    assert!(minted["expiresAtUnixMs"].is_i64());

    let enroll = |token: String| {
        json_request(
            "POST",
            "/api/mobile/devices/enroll",
            mobile_enroll_body(serde_json::json!({"enrollToken": token})),
        )
    };
    let res = app.clone().oneshot(enroll(token.clone())).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let enrolled = body_json(res).await;
    assert!(enrolled["tokens"]["refreshToken"].is_string());

    let reused = app.clone().oneshot(enroll(token)).await.unwrap();
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn enrolling_the_same_installation_twice_keeps_one_device() {
    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon.clone());

    let login = || {
        json_request(
            "POST",
            "/api/mobile/devices/enroll",
            mobile_enroll_body(
                serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
            ),
        )
    };
    let first = body_json(app.clone().oneshot(login()).await.unwrap()).await;
    let second = body_json(app.clone().oneshot(login()).await.unwrap()).await;

    assert_eq!(second["device"]["id"], first["device"]["id"]);
    assert_eq!(second["device"]["appInstallationId"], "app-install-1");

    let listed = body_json(
        app.clone()
            .oneshot(bearer_request(
                "GET",
                "/api/mobile/devices",
                second["tokens"]["accessToken"].as_str().unwrap(),
            ))
            .await
            .unwrap(),
    )
    .await["devices"]
        .clone();
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn mobile_refresh_rotation_reuse_and_revocation() {
    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon.clone());

    let enrolled = body_json(
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/mobile/devices/enroll",
                mobile_enroll_body(
                    serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
                ),
            ))
            .await
            .unwrap(),
    )
    .await;
    let first_refresh = enrolled["tokens"]["refreshToken"]
        .as_str()
        .unwrap()
        .to_string();

    let refresh = |token: String| {
        json_request(
            "POST",
            "/api/mobile/devices/refresh",
            serde_json::json!({"refreshToken": token}),
        )
    };
    let rotated = app
        .clone()
        .oneshot(refresh(first_refresh.clone()))
        .await
        .unwrap();
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated = body_json(rotated).await["tokens"].clone();
    let rotated_access = rotated["accessToken"].as_str().unwrap().to_string();
    assert_ne!(rotated["refreshToken"].as_str().unwrap(), first_refresh);
    assert_eq!(
        app.clone()
            .oneshot(bearer_request(
                "GET",
                "/api/mobile/devices",
                &rotated_access
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // Reusing the rotated-away refresh token revokes the whole family.
    let reused = app.clone().oneshot(refresh(first_refresh)).await.unwrap();
    assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.clone()
            .oneshot(bearer_request(
                "GET",
                "/api/mobile/devices",
                &rotated_access
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(refresh(
                rotated["refreshToken"].as_str().unwrap().to_string()
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // A revoked device drops out of the listing entirely.
    let login = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/login",
            serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
        ))
        .await
        .unwrap();
    let auth = dashboard_auth_from(&daemon, &login);
    let devices = body_json(
        app.clone()
            .oneshot(
                Request::get("/api/mobile/devices")
                    .header(header::AUTHORIZATION, &auth)
                    .header(header::HOST, TEST_HOST)
                    .header(header::ORIGIN, TEST_ORIGIN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await["devices"]
        .clone();
    assert_eq!(devices.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn mobile_device_revocation_via_http_kills_both_tokens() {
    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon.clone());

    let enrolled = body_json(
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/mobile/devices/enroll",
                mobile_enroll_body(
                    serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
                ),
            ))
            .await
            .unwrap(),
    )
    .await;
    let device_id = enrolled["device"]["id"].as_str().unwrap().to_string();
    let access = enrolled["tokens"]["accessToken"]
        .as_str()
        .unwrap()
        .to_string();
    let refresh = enrolled["tokens"]["refreshToken"]
        .as_str()
        .unwrap()
        .to_string();

    // A device may revoke itself over bearer auth.
    let revoked = app
        .clone()
        .oneshot(bearer_request(
            "DELETE",
            &format!("/api/mobile/devices/{device_id}"),
            &access,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

    assert_eq!(
        app.clone()
            .oneshot(bearer_request("GET", "/api/mobile/devices", &access))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/mobile/devices/refresh",
                serde_json::json!({"refreshToken": refresh}),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn expired_mobile_tokens_fail_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("pm.db");
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: Some(db_path.clone()),
        socket_path: tmp.path().join("unused.sock"),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: true,
        release_channel: None,
    };
    let (daemon, _exit_rx) = Daemon::new(config).unwrap();
    let daemon = Arc::new(daemon);
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon.clone());

    let enrolled = body_json(
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/mobile/devices/enroll",
                mobile_enroll_body(
                    serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
                ),
            ))
            .await
            .unwrap(),
    )
    .await;
    let device_id: u64 = enrolled["device"]["id"].as_str().unwrap().parse().unwrap();

    // Plant already-expired tokens through a second storage handle on
    // the same database file.
    let storage = pm_daemon::storage::Storage::open(&db_path).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let expired_access = "feed0000access";
    let expired_refresh = "feed0000refresh";
    let user_id = storage.get_mobile_device(device_id).unwrap().user_id;
    storage
        .create_access_token(
            Some(device_id),
            user_id,
            None,
            &pm_daemon::auth::hash_token(expired_access),
            now - 2,
            now - 1,
        )
        .unwrap();
    storage
        .create_mobile_refresh_token(
            device_id,
            &pm_daemon::auth::hash_token(expired_refresh),
            now - 2,
            now - 1,
        )
        .unwrap();

    assert_eq!(
        app.clone()
            .oneshot(bearer_request("GET", "/api/mobile/devices", expired_access))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/mobile/devices/refresh",
                serde_json::json!({"refreshToken": expired_refresh}),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // The live pair from enrollment still works.
    assert_eq!(
        app.clone()
            .oneshot(bearer_request(
                "GET",
                "/api/mobile/devices",
                enrolled["tokens"]["accessToken"].as_str().unwrap(),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn version_advertises_installation_and_mobile_capability() {
    let (daemon, _tmp) = test_daemon();
    let app = pm_daemon::http::router(daemon.clone());
    let version = body_json(
        app.clone()
            .oneshot(Request::get("/api/version").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    let installation_id = version["installationId"].as_str().unwrap().to_string();
    assert!(!installation_id.is_empty());
    assert_eq!(version["mobile"]["deviceAuth"], true);
    let again = body_json(
        app.clone()
            .oneshot(Request::get("/api/version").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(again["installationId"], installation_id.as_str());
}

#[tokio::test]
async fn mobile_tokens_stay_out_of_logs() {
    let buffer: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    struct BufferWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for BufferWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let writer_buffer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || BufferWriter(writer_buffer.clone()))
        .finish();
    // A scoped subscriber is unreliable here: with no global default,
    // whichever thread first hits a callsite caches its interest, so a
    // parallel test thread without a subscriber can permanently disable
    // the very events under test. The global default sidesteps that;
    // concurrent tests' events also land in the buffer, which only
    // widens the leak check.
    tracing::subscriber::set_global_default(subscriber)
        .expect("no other test sets a global subscriber");

    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon.clone());

    let session = daemon.auth_login("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &session);
    let minted = body_json(
        app.clone()
            .oneshot(
                Request::post("/api/mobile/devices/enroll-token")
                    .header(header::AUTHORIZATION, &auth)
                    .header(header::HOST, TEST_HOST)
                    .header(header::ORIGIN, TEST_ORIGIN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let enroll_token = minted["token"].as_str().unwrap().to_string();
    let enrolled = body_json(
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/mobile/devices/enroll",
                mobile_enroll_body(serde_json::json!({"enrollToken": enroll_token})),
            ))
            .await
            .unwrap(),
    )
    .await;
    let access = enrolled["tokens"]["accessToken"]
        .as_str()
        .unwrap()
        .to_string();
    let refresh = enrolled["tokens"]["refreshToken"]
        .as_str()
        .unwrap()
        .to_string();
    let rotated = body_json(
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/mobile/devices/refresh",
                serde_json::json!({"refreshToken": refresh}),
            ))
            .await
            .unwrap(),
    )
    .await;
    app.clone()
        .oneshot(bearer_request("GET", "/api/mobile/devices", &access))
        .await
        .unwrap();

    // Socket tickets: mint over HTTP, consume once, then exercise the
    // reuse and binding-mismatch rejection paths that log warnings.
    let rotated_access = rotated["tokens"]["accessToken"].as_str().unwrap();
    let control_ticket = body_json(
        app.clone()
            .oneshot(bearer_request("POST", "/api/ws/ticket", rotated_access))
            .await
            .unwrap(),
    )
    .await["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(daemon
        .consume_control_socket_ticket(&control_ticket)
        .is_some());
    assert!(daemon
        .consume_control_socket_ticket(&control_ticket)
        .is_none());
    let (mismatched_ticket, _) = {
        let (user_id, _, device_id) = daemon.access_token_verify(rotated_access).unwrap();
        daemon
            .create_control_socket_ticket(user_id, device_id)
            .unwrap()
    };
    assert!(daemon
        .consume_terminal_attach_ticket(&mismatched_ticket, 1, 1)
        .is_none());

    let logs = String::from_utf8_lossy(&buffer.lock().unwrap()).to_string();
    assert!(
        logs.contains("mobile device enrolled") && logs.contains("mobile enrollment token minted"),
        "expected mobile auth activity in logs, got: {logs:?}"
    );
    assert!(
        logs.contains("control socket ticket minted")
            && logs.contains("control socket ticket consumed")
            && logs.contains("socket ticket rejected")
            && logs.contains("rejected on a terminal socket"),
        "expected socket ticket activity in logs, got: {logs:?}"
    );
    for secret in [
        session.as_str(),
        enroll_token.as_str(),
        access.as_str(),
        refresh.as_str(),
        rotated["tokens"]["accessToken"].as_str().unwrap(),
        rotated["tokens"]["refreshToken"].as_str().unwrap(),
        control_ticket.as_str(),
        mismatched_ticket.as_str(),
    ] {
        assert!(!logs.contains(secret), "raw token leaked into logs");
    }
}

fn bearer_json_request(
    method: &str,
    path: &str,
    token: &str,
    body: serde_json::Value,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn enroll_mobile_device(app: &axum::Router) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/mobile/devices/enroll",
            mobile_enroll_body(
                serde_json::json!({"username": "testuser", "password": "hunter2hunter2"}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

fn device_access_token(enrolled: &serde_json::Value) -> String {
    enrolled["tokens"]["accessToken"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn mint_ws_ticket(app: &axum::Router, access: &str) -> String {
    let response = app
        .clone()
        .oneshot(bearer_request("POST", "/api/ws/ticket", access))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["ticket"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn mint_attach_ticket(
    app: &axum::Router,
    access: &str,
    terminal_id: u64,
    generation: u64,
) -> String {
    let response = app
        .clone()
        .oneshot(bearer_json_request(
            "POST",
            &format!("/api/terminals/{terminal_id}/attach-ticket"),
            access,
            serde_json::json!({"generation": generation.to_string()}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["ticket"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn ws_connect_ticket(env: &WsEnv, ticket: &str) -> WsStream {
    let uri: tungstenite::http::Uri = format!("ws://{}/ws?ticket={ticket}", env.addr)
        .parse()
        .unwrap();
    let builder = tungstenite::client::ClientRequestBuilder::new(uri);
    tokio_tungstenite::connect_async(builder).await.unwrap().0
}

async fn expect_close_unauthenticated(ws: &mut WsStream) {
    use futures::StreamExt;
    let msg = tokio::time::timeout(TEST_TIMEOUT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match msg {
        tungstenite::Message::Close(Some(frame)) => {
            assert_eq!(u16::from(frame.code), 4401);
        }
        other => panic!("expected close frame, got {other:?}"),
    }
}

/// Ticket-only terminal connect (no cookie); pre-upgrade rejections
/// come back as the HTTP status.
async fn terminal_ticket_connect(
    env: &WsEnv,
    terminal_id: u64,
    generation: u64,
    ticket: &str,
) -> Result<WsStream, u16> {
    let uri: tungstenite::http::Uri = format!(
        "ws://{}/ws/terminal/{terminal_id}?generation={generation}&ticket={ticket}",
        env.addr
    )
    .parse()
    .unwrap();
    let builder =
        tungstenite::client::ClientRequestBuilder::new(uri).with_sub_protocol("pm-terminal-v1");
    match tokio_tungstenite::connect_async(builder).await {
        Ok((stream, _)) => Ok(stream),
        Err(tungstenite::Error::Http(response)) => Err(response.status().as_u16()),
        Err(other) => panic!("unexpected terminal connect error: {other:?}"),
    }
}

#[tokio::test]
async fn socket_ticket_mint_requires_device_auth() {
    let (daemon, _tmp) = test_daemon();
    daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let app = pm_daemon::http::router(daemon.clone());

    let unauthenticated = app
        .clone()
        .oneshot(Request::post("/api/ws/ticket").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    // The browser cookie cannot mint tickets; browsers keep using the
    // cookie directly on the WebSocket upgrades.
    let session = daemon.auth_login("testuser", "hunter2hunter2").unwrap();
    let with_cookie = Request::post("/api/ws/ticket")
        .header(header::COOKIE, format!("pm_session={session}"))
        .header(header::HOST, TEST_HOST)
        .header(header::ORIGIN, TEST_ORIGIN)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(with_cookie).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let enrolled = enroll_mobile_device(&app).await;
    let access = device_access_token(&enrolled);
    let minted = app
        .clone()
        .oneshot(bearer_request("POST", "/api/ws/ticket", &access))
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    let minted = body_json(minted).await;
    assert!(minted["ticket"].is_string());
    assert!(minted["expiresAtUnixMs"].is_i64());

    let missing_terminal = app
        .clone()
        .oneshot(bearer_json_request(
            "POST",
            "/api/terminals/999/attach-ticket",
            &access,
            serde_json::json!({"generation": "1"}),
        ))
        .await
        .unwrap();
    assert_eq!(missing_terminal.status(), StatusCode::NOT_FOUND);
}

/// The path a browser actually takes to the control socket.
///
/// A handshake carries no header a page can set, so the dashboard spends its
/// access token on a one-use ticket and puts that in the URL. The ticket names
/// no device, because a browser enrols nothing, and it is the same mechanism
/// the phone's terminal WebView uses for the same reason.
#[tokio::test]
async fn the_dashboard_opens_the_control_socket_with_a_one_use_ticket() {
    let env = ws_env().await;
    let app = pm_daemon::http::router(env.daemon.clone());
    let ticket = mint_ws_ticket(&app, env.auth.trim_start_matches("Bearer ")).await;

    let mut ws = ws_connect_ticket(&env, &ticket).await;
    ws_send(&mut ws, 1, ClientMsg::Subscribe { scope: Scope::All }).await;
    loop {
        match ws_next(&mut ws).await {
            Some(ServerMsg::Snapshot(_)) => break,
            Some(_) => continue,
            None => panic!("the dashboard's ticket connection closed before a snapshot"),
        }
    }

    // One use, so a preview that read the URL out of a log cannot replay it.
    let mut reused = ws_connect_ticket(&env, &ticket).await;
    expect_close_unauthenticated(&mut reused).await;

    // And the cookie the token was minted from does not open the socket.
    let uri: tungstenite::http::Uri = format!("ws://{}/ws", env.addr).parse().unwrap();
    let mut with_cookie = tokio_tungstenite::connect_async(
        tungstenite::client::ClientRequestBuilder::new(uri)
            .with_header(header::COOKIE.as_str(), env.cookie.clone()),
    )
    .await
    .unwrap()
    .0;
    expect_close_unauthenticated(&mut with_cookie).await;
}

#[tokio::test]
async fn control_ws_ticket_is_single_use() {
    let env = ws_env().await;
    let app = pm_daemon::http::router(env.daemon.clone());
    let enrolled = enroll_mobile_device(&app).await;
    let access = device_access_token(&enrolled);
    let ticket = mint_ws_ticket(&app, &access).await;

    let mut ws = ws_connect_ticket(&env, &ticket).await;
    ws_send(&mut ws, 1, ClientMsg::Subscribe { scope: Scope::All }).await;
    loop {
        match ws_next(&mut ws).await {
            Some(ServerMsg::Snapshot(_)) => break,
            Some(_) => continue,
            None => panic!("ticket connection closed before snapshot"),
        }
    }

    let mut reused = ws_connect_ticket(&env, &ticket).await;
    expect_close_unauthenticated(&mut reused).await;

    let mut bogus = ws_connect_ticket(&env, "not-a-ticket").await;
    expect_close_unauthenticated(&mut bogus).await;
}

#[tokio::test]
async fn terminal_ws_ticket_binding_and_reuse() {
    let env = ws_env().await;
    let bucket = env.daemon.create_bucket("mobile").unwrap();
    let project = env.daemon.create_project(bucket, "p", "/tmp").unwrap();
    let session = env
        .daemon
        .spawn_session(
            project,
            AgentKind::Test,
            "mobile terminal",
            "ticket prompt",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let terminal = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let Some(terminal) = env
                .daemon
                .subscribe()
                .0
                .terminals
                .into_iter()
                .find(|terminal| terminal.session_id == session)
            {
                return terminal;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let app = pm_daemon::http::router(env.daemon.clone());
    let enrolled = enroll_mobile_device(&app).await;
    let access = device_access_token(&enrolled);

    // A stale generation is rejected at mint.
    let stale = app
        .clone()
        .oneshot(bearer_json_request(
            "POST",
            &format!("/api/terminals/{}/attach-ticket", terminal.id),
            &access,
            serde_json::json!({"generation": (terminal.generation + 1).to_string()}),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    // Ticket-only auth (no cookie) attaches and replays.
    let ticket = mint_attach_ticket(&app, &access, terminal.id, terminal.generation).await;
    let mut ws = terminal_ticket_connect(&env, terminal.id, terminal.generation, &ticket)
        .await
        .expect("fresh ticket should attach");
    terminal_wait_for_replay(&mut ws).await;
    // Reuse after a successful attach is rejected before upgrade.
    assert_eq!(
        terminal_ticket_connect(&env, terminal.id, terminal.generation, &ticket)
            .await
            .err(),
        Some(401)
    );
    drop(ws);

    // URL bindings must match the mint bindings exactly, and a
    // mismatched presentation burns the ticket.
    let ticket = mint_attach_ticket(&app, &access, terminal.id, terminal.generation).await;
    assert_eq!(
        terminal_ticket_connect(&env, terminal.id, terminal.generation + 1, &ticket)
            .await
            .err(),
        Some(401)
    );
    assert_eq!(
        terminal_ticket_connect(&env, terminal.id, terminal.generation, &ticket)
            .await
            .err(),
        Some(401)
    );
    let ticket = mint_attach_ticket(&app, &access, terminal.id, terminal.generation).await;
    assert_eq!(
        terminal_ticket_connect(&env, terminal.id + 1, terminal.generation, &ticket)
            .await
            .err(),
        Some(401)
    );

    // A control ticket does not open a terminal socket, and a terminal
    // ticket does not open the control socket.
    let control = mint_ws_ticket(&app, &access).await;
    assert_eq!(
        terminal_ticket_connect(&env, terminal.id, terminal.generation, &control)
            .await
            .err(),
        Some(401)
    );
    let ticket = mint_attach_ticket(&app, &access, terminal.id, terminal.generation).await;
    let mut crossed = ws_connect_ticket(&env, &ticket).await;
    expect_close_unauthenticated(&mut crossed).await;

    // Oversized replay requests are clamped at mint.
    let huge = app
        .clone()
        .oneshot(bearer_json_request(
            "POST",
            &format!("/api/terminals/{}/attach-ticket", terminal.id),
            &access,
            serde_json::json!({
                "generation": terminal.generation.to_string(),
                "replayBytes": 100_000_000u64,
            }),
        ))
        .await
        .unwrap();
    let huge = body_json(huge).await["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    let attach = env
        .daemon
        .consume_terminal_attach_ticket(&huge, terminal.id, terminal.generation)
        .unwrap();
    assert_eq!(
        attach.replay_bytes as u64,
        pm_daemon::mobile::TERMINAL_TICKET_REPLAY_MAX_BYTES
    );
}

#[tokio::test]
async fn revoked_device_socket_tickets_are_rejected() {
    let env = ws_env().await;
    let app = pm_daemon::http::router(env.daemon.clone());
    let enrolled = enroll_mobile_device(&app).await;
    let access = device_access_token(&enrolled);
    let device_id = enrolled["device"]["id"].as_str().unwrap().to_string();
    let ticket = mint_ws_ticket(&app, &access).await;

    // Revocation between mint and use invalidates the live ticket.
    let revoked = app
        .clone()
        .oneshot(bearer_request(
            "DELETE",
            &format!("/api/mobile/devices/{device_id}"),
            &access,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

    let mut ws = ws_connect_ticket(&env, &ticket).await;
    expect_close_unauthenticated(&mut ws).await;
}

#[tokio::test]
async fn expired_socket_tickets_are_rejected() {
    let env = ws_env().await;
    env.daemon
        .set_setting(
            pm_daemon::mobile::SETTING_MOBILE_SOCKET_TICKET_TTL_SECONDS,
            Some("1"),
        )
        .unwrap();
    let app = pm_daemon::http::router(env.daemon.clone());
    let enrolled = enroll_mobile_device(&app).await;
    let access = device_access_token(&enrolled);
    let ticket = mint_ws_ticket(&app, &access).await;

    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    let mut ws = ws_connect_ticket(&env, &ticket).await;
    expect_close_unauthenticated(&mut ws).await;
}

#[tokio::test]
async fn dropping_a_bucket_host_over_http_moves_pinned_projects_to_the_named_replacement() {
    let (daemon, tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let bucket = daemon.create_bucket("primary").unwrap();
    let project = daemon
        .create_project(bucket, "api", tmp.path().to_str().unwrap())
        .unwrap();
    let host = |name: &str, key: &str| {
        let (enrollment, _) = daemon.create_worker_enrollment(name).unwrap();
        daemon
            .register_worker_connection(pm_daemon::daemon::WorkerHello {
                enrollment_token: &enrollment,
                credential: "",
                peer_key_hash: key,
                hostname: name,
                platform: "linux",
                pm_version: "",
                runtime: "",
                container: "",
                default_project_root: "/home/dev",
                live_sessions: &[],
                live_terminals: &[],
                protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
            })
            .unwrap()
            .worker_id
    };
    let retiring = host("old-box", "key-old");
    let spare = host("spare-box", "key-spare");
    daemon
        .set_bucket_workers(bucket, &[0, retiring, spare], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project, &[retiring], Some(retiring))
        .unwrap();

    let app = pm_daemon::http::router(daemon.clone());
    let mut request = json_request(
        "POST",
        &format!("/api/buckets/{bucket}/worker"),
        serde_json::json!({
            "worker_id": 0,
            "allowed_worker_ids": [0, spare],
            "replacement_worker_id": spare,
        }),
    );
    request.headers_mut().insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_str(&auth).unwrap(),
    );
    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let project = daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == project)
        .unwrap();
    assert_eq!(
        project.worker_id,
        Some(spare),
        "the named replacement takes the pinned project, not the bucket default"
    );
    assert_eq!(project.allowed_worker_ids, vec![spare]);
}

#[tokio::test]
async fn project_worker_path_http_sets_clears_validates_and_updates_payloads() {
    let (daemon, tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let bucket = daemon.create_bucket("primary").unwrap();
    let project = daemon
        .create_project(bucket, "api", tmp.path().to_str().unwrap())
        .unwrap();
    let (enrollment, _) = daemon.create_worker_enrollment("laptop").unwrap();
    let worker_id = daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enrollment,
            credential: "",
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap()
        .worker_id;
    daemon
        .set_bucket_workers(bucket, &[0, worker_id], 0, None)
        .unwrap();
    daemon
        .set_project_workers(project, &[0, worker_id], None)
        .unwrap();

    let app = pm_daemon::http::router(daemon.clone());
    let uri = format!("/api/projects/{project}/worker-path");
    let authed = |body: serde_json::Value| {
        let mut request = json_request("POST", &uri, body);
        request.headers_mut().insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&auth).unwrap(),
        );
        request
    };
    let worker_paths = || {
        daemon
            .subscribe()
            .0
            .projects
            .into_iter()
            .find(|p| p.id == project)
            .unwrap()
            .worker_paths
    };

    let unauthenticated = app
        .clone()
        .oneshot(json_request(
            "POST",
            &uri,
            serde_json::json!({ "worker": "laptop", "path": "/laptop/api" }),
        ))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert!(worker_paths().is_empty());

    let (_, mut events) = daemon.subscribe();
    let set_by_name = app
        .clone()
        .oneshot(authed(
            serde_json::json!({ "worker": "laptop", "path": "/laptop/api" }),
        ))
        .await
        .unwrap();
    assert_eq!(set_by_name.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        worker_paths(),
        vec![ProjectPath {
            worker_id,
            path: "/laptop/api".into()
        }]
    );
    match events.recv().await.unwrap() {
        Event::ProjectChanged(changed) => assert_eq!(
            changed.worker_paths,
            vec![ProjectPath {
                worker_id,
                path: "/laptop/api".into()
            }],
            "the change event carries the mapping the UI consumes"
        ),
        other => panic!("unexpected event: {other:?}"),
    }

    let update_by_numeric_id = app
        .clone()
        .oneshot(authed(
            serde_json::json!({ "worker": worker_id, "path": "/laptop/api-v2" }),
        ))
        .await
        .unwrap();
    assert_eq!(update_by_numeric_id.status(), StatusCode::NO_CONTENT);
    assert_eq!(worker_paths()[0].path, "/laptop/api-v2");

    let clear_with_null = app
        .clone()
        .oneshot(authed(serde_json::json!({
            "worker": worker_id.to_string(),
            "path": null
        })))
        .await
        .unwrap();
    assert_eq!(clear_with_null.status(), StatusCode::NO_CONTENT);
    assert!(worker_paths().is_empty());

    let set_again = app
        .clone()
        .oneshot(authed(
            serde_json::json!({ "worker": "laptop", "path": "/laptop/api" }),
        ))
        .await
        .unwrap();
    assert_eq!(set_again.status(), StatusCode::NO_CONTENT);
    let clear_with_empty = app
        .clone()
        .oneshot(authed(
            serde_json::json!({ "worker": "laptop", "path": "  " }),
        ))
        .await
        .unwrap();
    assert_eq!(clear_with_empty.status(), StatusCode::NO_CONTENT);
    assert!(worker_paths().is_empty(), "a blank path clears the entry");

    let unknown_worker = app
        .clone()
        .oneshot(authed(
            serde_json::json!({ "worker": "desk-vm", "path": "/x" }),
        ))
        .await
        .unwrap();
    assert_eq!(unknown_worker.status(), StatusCode::BAD_REQUEST);
    let error = body_json(unknown_worker).await["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        error.contains("allowed workers") && error.contains("laptop"),
        "the rejection lists the allowed workers: {error}"
    );

    daemon.set_project_workers(project, &[0], None).unwrap();
    let disallowed_worker = app
        .clone()
        .oneshot(authed(
            serde_json::json!({ "worker": "laptop", "path": "/x" }),
        ))
        .await
        .unwrap();
    assert_eq!(disallowed_worker.status(), StatusCode::BAD_REQUEST);

    let malformed_worker = app
        .clone()
        .oneshot(authed(serde_json::json!({ "worker": true, "path": "/x" })))
        .await
        .unwrap();
    assert_eq!(malformed_worker.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(malformed_worker).await["error"],
        "worker must be a worker id or name"
    );

    let missing_project = app
        .clone()
        .oneshot({
            let mut request = json_request(
                "POST",
                "/api/projects/9999/worker-path",
                serde_json::json!({ "worker": "laptop", "path": "/x" }),
            );
            request.headers_mut().insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&auth).unwrap(),
            );
            request
        })
        .await
        .unwrap();
    assert_eq!(missing_project.status(), StatusCode::BAD_REQUEST);
}

/// Connects to the daemon's HTTPS listener trusting only the certificate
/// it was started with, and returns the status line of GET /api/version.
async fn https_get_version(addr: std::net::SocketAddr, cert_pem: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio_rustls::rustls;
    use tokio_rustls::rustls::pki_types::pem::PemObject as _;

    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let mut tls = connector.connect(name, tcp).await.unwrap();
    tls.write_all(b"GET /api/version HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    tls.read_to_string(&mut response).await.unwrap();
    response.lines().next().unwrap_or_default().to_string()
}

#[tokio::test]
async fn the_http_surface_serves_https_when_given_a_certificate_and_key() {
    let tmp = tempfile::tempdir().unwrap();
    let (cert_pem, key_pem) = pm_tls::self_signed_web_cert_pem(&["localhost".to_string()]).unwrap();
    let cert_path = tmp.path().join("cert.pem");
    let key_path = tmp.path().join("key.pem");
    std::fs::write(&cert_path, &cert_pem).unwrap();
    std::fs::write(&key_path, &key_pem).unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls: Some(pm_daemon::HttpTls {
            cert_chain: cert_path,
            key: key_path,
        }),
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: false,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let addr = handle.http_addr.unwrap();

    assert_eq!(daemon.http_scheme(), "https");
    assert_eq!(daemon.login_url(), format!("https://{addr}/login"));

    let status = tokio::time::timeout(TEST_TIMEOUT, https_get_version(addr, &cert_pem))
        .await
        .unwrap();
    assert_eq!(status, "HTTP/1.0 200 OK");

    let plain = reqwest::Client::new()
        .get(format!("http://{addr}/api/version"))
        .send()
        .await;
    assert!(
        plain.is_err(),
        "plain HTTP must not be served on the TLS listener"
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn the_http_surface_refuses_to_start_with_an_unreadable_certificate() {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls: Some(pm_daemon::HttpTls {
            cert_chain: tmp.path().join("missing.pem"),
            key: tmp.path().join("missing-key.pem"),
        }),
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: false,
        release_channel: None,
    };
    let error = pm_daemon::start(config)
        .await
        .err()
        .expect("startup must fail");
    assert!(error.to_string().contains("missing.pem"), "{error}");
}

#[tokio::test]
async fn the_http_surface_stays_plain_http_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        forward: Default::default(),
        db_path: None,
        socket_path: tmp.path().join("pm.sock"),
        http_addr: Some("127.0.0.1:0".parse().unwrap()),
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: tmp.path().join("sb"),
        registry: test_registry(),
        local_worker_enabled: false,
        release_channel: None,
    };
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let addr = handle.http_addr.unwrap();
    assert_eq!(daemon.http_scheme(), "http");
    assert_eq!(daemon.login_url(), format!("http://{addr}/login"));
    let response = reqwest::get(format!("http://{addr}/api/version"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    handle.shutdown().await;
}

#[tokio::test]
async fn plan_decision_draft_http_endpoints_work() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = dashboard_auth(&daemon, &token);
    let bucket = daemon.create_bucket("primary").unwrap();
    let project_id = daemon
        .create_project(bucket, "proj", "/tmp/project")
        .unwrap();
    let session_id = daemon
        .spawn_session(
            project_id,
            AgentKind::Test,
            "sess",
            "",
            None,
            pm_protocol::domain::PermissionMode::Bypass,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let plan = daemon
        .create_plan(session_id, "Test Plan", "", "docs/plan.md", &[])
        .unwrap();
    let (_focused, decision) = daemon
        .present_plan_decision(
            session_id,
            plan.id,
            "arch",
            "Architecture",
            "",
            "",
            PlanDecisionMode::Single,
            true,
            true,
            &[
                PlanOptionDraft::new("opt1", "Option 1", ""),
                PlanOptionDraft::new("opt2", "Option 2", ""),
            ],
        )
        .unwrap();

    let app = pm_daemon::http::router(daemon.clone());

    // Unauthorized check
    let uri = format!("/api/plans/{}/decisions/{}/draft", plan.id, decision.id);
    let unauthed = app
        .clone()
        .oneshot(
            Request::put(&uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "selectedOptionKeys": ["opt1"],
                        "customLabel": "",
                        "customDetailMarkdown": "",
                        "notes": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

    // Authorized draft save
    let authed = app
        .clone()
        .oneshot(
            Request::put(&uri)
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "selectedOptionKeys": ["opt1"],
                        "customLabel": "My Custom Choice",
                        "customDetailMarkdown": "Markdown details here",
                        "notes": { "opt1": "Preferred for scale" }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authed.status(), StatusCode::OK);

    // Verify detail reflects the draft
    let detail_res = app
        .clone()
        .oneshot(
            Request::get(format!("/api/plans/{}", plan.id))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail_res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(detail_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let detail_json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        detail_json["decisions"][0]["draft"]["selectedOptionKeys"],
        serde_json::json!(["opt1"])
    );
    assert_eq!(
        detail_json["decisions"][0]["draft"]["customLabel"],
        "My Custom Choice"
    );
    assert_eq!(
        detail_json["decisions"][0]["draft"]["notes"]["opt1"],
        "Preferred for scale"
    );

    // Batch draft save
    let batch_uri = format!("/api/plans/{}/decisions/drafts", plan.id);
    let batch_res = app
        .clone()
        .oneshot(
            Request::put(&batch_uri)
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "drafts": [{
                            "decisionId": decision.id,
                            "draft": {
                                "selectedOptionKeys": ["opt2"],
                                "customLabel": "",
                                "customDetailMarkdown": "",
                                "notes": {}
                            }
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(batch_res.status(), StatusCode::OK);

    let detail_after = daemon.plan_detail(plan.id).unwrap();
    assert_eq!(
        detail_after.decisions[0]
            .draft
            .as_ref()
            .unwrap()
            .selected_option_keys,
        vec!["opt2".to_string()]
    );
}

#[tokio::test]
async fn harness_checks_require_authentication_and_validate_the_request() {
    let (daemon, _tmp) = test_daemon();
    let token = daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let bucket = daemon.create_bucket("installers").unwrap();
    let project = daemon
        .create_project(bucket, "project", _tmp.path().to_str().unwrap())
        .unwrap();
    let app = pm_daemon::http::router(daemon.clone());
    let auth = dashboard_auth(&daemon, &token);
    let request = |auth: Option<&str>, agent: &str| {
        let mut builder = Request::post("/api/harness")
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(auth) = auth {
            builder = builder.header(header::AUTHORIZATION, auth);
        }
        builder
            .body(Body::from(
                serde_json::json!({ "project": project, "worker": 0, "agent": agent }).to_string(),
            ))
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(request(None, "test"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = app
        .clone()
        .oneshot(request(Some(&auth), "test"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["status"]["state"], "ready");
    let response = app.oneshot(request(Some(&auth), "invalid")).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(response).await["error"], "unknown harness");
}

/// The dashboard fills a remote worker's controller address from the
/// public URL the operator configured, so the version payload carries it
/// as configured and says plainly when there is none.
#[tokio::test]
async fn version_reports_the_configured_public_url() {
    let version = |env: &support::TestEnv| {
        let app = pm_daemon::http::router(env.daemon.clone());
        async move {
            body_json(
                app.oneshot(Request::get("/api/version").body(Body::empty()).unwrap())
                    .await
                    .unwrap(),
            )
            .await
        }
    };
    let bare = support::daemon_env();
    assert_eq!(version(&bare).await["publicUrl"], serde_json::Value::Null);
    let named = support::daemon_env_with_public_url("https://pm.example.com");
    assert_eq!(version(&named).await["publicUrl"], "https://pm.example.com");
}
/// A viewer that opens a terminal at a new size resizes the PTY, and the
/// program redraws for it. Its first snapshot waits for that redraw instead
/// of showing the old screen and then the redraw arriving live.
#[tokio::test]
async fn a_viewer_opening_at_a_new_size_gets_a_snapshot_after_the_repaint() {
    opening_viewer_waits_for_repaint(false).await;
}

#[tokio::test]
async fn an_ownership_aware_viewer_waits_for_its_opening_resize_repaint() {
    opening_viewer_waits_for_repaint(true).await;
}

async fn opening_viewer_waits_for_repaint(ownership_aware: bool) {
    use futures::{SinkExt, StreamExt};

    let env = ws_env().await;
    let RemoteRelay {
        mut registration,
        host_identity,
        terminal_id,
        session: _session,
        _root,
    } = remote_relay_terminal(&env).await;
    let mut first = terminal_ws_connect(&env, terminal_id, 1).await;
    let token = match tokio::time::timeout(TEST_TIMEOUT, registration.rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerMsg::TerminalAttach {
            terminal_id: attached,
            token,
            ..
        } if attached == terminal_id => token,
        message => panic!("unexpected message {message:?}"),
    };
    let mut worker = host_dial(&env, &host_identity, "/worker/terminal", Some(&token))
        .await
        .expect("the enrolled host streams its terminal")
        .link;
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(
                1,
                pm_protocol::terminal_frame::FLAG_REPLAY
                    | pm_protocol::terminal_frame::FLAG_REPLAY_START
                    | pm_protocol::terminal_frame::FLAG_REPLAY_END,
                b"screen before the resize",
            ),
        ))
        .await
        .unwrap();
    terminal_wait_for_replay(&mut first).await;

    let mut second = if ownership_aware {
        const VIEWER_ID: u64 = 42;
        const FIRST_CLAIM: u64 = 1;
        let uri: tungstenite::http::Uri = format!(
            "ws://{}/ws/terminal/{terminal_id}?generation=1&cols=90&rows=24&viewer={VIEWER_ID}&claim={FIRST_CLAIM}",
            env.addr
        )
        .parse()
        .unwrap();
        let builder = tungstenite::client::ClientRequestBuilder::new(uri)
            .with_header(header::AUTHORIZATION.as_str(), env.auth.clone())
            .with_sub_protocol("pm-terminal-v1");
        tokio_tungstenite::connect_async(builder).await.unwrap().0
    } else {
        terminal_ws_connect_with_size(&env, terminal_id, 1, 90, 24).await
    };
    loop {
        let frame = tokio::time::timeout(TEST_TIMEOUT, worker.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tungstenite::Message::Binary(frame) = frame {
            if matches!(
                pm_protocol::terminal_frame::decode(&frame),
                Some(pm_protocol::terminal_frame::TerminalFrame::Resize { cols: 90, .. })
            ) {
                break;
            }
        }
    }
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(1, 0, b"\x1b[H\x1b[2J"),
        ))
        .await
        .unwrap();
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(1, 0, b"repainted at ninety"),
        ))
        .await
        .unwrap();

    let mut snapshot = Vec::new();
    loop {
        let message = tokio::time::timeout(TEST_TIMEOUT, second.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let tungstenite::Message::Binary(frame) = message else {
            continue;
        };
        if let Some(pm_protocol::terminal_frame::TerminalFrame::Output { flags, data, .. }) =
            pm_protocol::terminal_frame::decode(&frame)
        {
            assert!(
                flags & pm_protocol::terminal_frame::FLAG_REPLAY != 0,
                "output reached the viewer before its first snapshot: {:?}",
                String::from_utf8_lossy(data)
            );
            snapshot.extend_from_slice(data);
            if flags & pm_protocol::terminal_frame::FLAG_REPLAY_END != 0 {
                break;
            }
        }
    }
    let snapshot = String::from_utf8_lossy(&snapshot);
    assert!(snapshot.contains("repainted at ninety"), "{snapshot}");
}

/// A viewer that resizes asks for a snapshot on its open socket. The
/// controller holds that viewer's live output while the program repaints
/// for the new size and answers with one snapshot that already contains the
/// repaint, so the viewer never shows the half-reflowed screen in between.
#[tokio::test]
async fn an_in_band_resync_answers_with_a_snapshot_taken_after_the_repaint() {
    use futures::{SinkExt, StreamExt};

    let env = ws_env().await;
    let RemoteRelay {
        mut registration,
        host_identity,
        terminal_id,
        session: _session,
        _root,
    } = remote_relay_terminal(&env).await;
    let mut browser = terminal_ws_connect(&env, terminal_id, 1).await;
    let token = match tokio::time::timeout(TEST_TIMEOUT, registration.rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerMsg::TerminalAttach {
            terminal_id: attached,
            token,
            ..
        } if attached == terminal_id => token,
        message => panic!("unexpected message {message:?}"),
    };
    let mut worker = host_dial(&env, &host_identity, "/worker/terminal", Some(&token))
        .await
        .expect("the enrolled host streams its terminal")
        .link;
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(
                1,
                pm_protocol::terminal_frame::FLAG_REPLAY
                    | pm_protocol::terminal_frame::FLAG_REPLAY_START
                    | pm_protocol::terminal_frame::FLAG_REPLAY_END,
                b"wide screen",
            ),
        ))
        .await
        .unwrap();
    terminal_wait_for_replay(&mut browser).await;

    browser
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_resize(1, 90, 24),
        ))
        .await
        .unwrap();
    browser
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_resync(1),
        ))
        .await
        .unwrap();
    loop {
        let frame = tokio::time::timeout(TEST_TIMEOUT, worker.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tungstenite::Message::Binary(frame) = frame {
            if matches!(
                pm_protocol::terminal_frame::decode(&frame),
                Some(pm_protocol::terminal_frame::TerminalFrame::Resize { cols: 90, .. })
            ) {
                break;
            }
        }
    }
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(1, 0, b"\x1b[H\x1b[2K"),
        ))
        .await
        .unwrap();
    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(1, 0, b"repainted at ninety"),
        ))
        .await
        .unwrap();

    let mut snapshot = Vec::new();
    loop {
        let message = tokio::time::timeout(TEST_TIMEOUT, browser.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let tungstenite::Message::Binary(frame) = message else {
            continue;
        };
        match pm_protocol::terminal_frame::decode(&frame) {
            Some(pm_protocol::terminal_frame::TerminalFrame::Output { flags, data, .. }) => {
                if flags & pm_protocol::terminal_frame::FLAG_REPLAY == 0 {
                    assert!(
                        !String::from_utf8_lossy(data).contains("repainted"),
                        "the repaint reached the viewer live, ahead of its snapshot"
                    );
                    continue;
                }
                snapshot.extend_from_slice(data);
                if flags & pm_protocol::terminal_frame::FLAG_REPLAY_END != 0 {
                    break;
                }
            }
            _ => continue,
        }
    }
    let snapshot = String::from_utf8_lossy(&snapshot);
    assert!(snapshot.contains("repainted at ninety"), "{snapshot}");

    worker
        .send(tungstenite::Message::Binary(
            pm_protocol::terminal_frame::encode_output(1, 0, b" and live again"),
        ))
        .await
        .unwrap();
    terminal_wait_for_text(&mut browser, b" and live again").await;
}
