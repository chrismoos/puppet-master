//! End-to-end over the real transport: daemon + unix socket server on
//! one side, pm-client on the other, scripted agent in a real PTY.

mod support;

use bytes::Bytes;
use pm_client::Client;
use pm_protocol::domain::{
    AgentKind, ClientMsg, Event, HookKind, ItemQuery, ItemWrite, RespondTarget, Scope, ServerMsg,
    SessionState, Snapshot,
};
use support::{test_registry, TEST_TIMEOUT};

struct E2e {
    client: Client,
    daemon: std::sync::Arc<pm_daemon::Daemon>,
    _handle: pm_daemon::ServerHandle,
    _tmp: tempfile::TempDir,
}

async fn e2e() -> E2e {
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
    let (daemon, handle) = pm_daemon::start(config).await.unwrap();
    let client = Client::connect(&socket_path).await.unwrap();
    E2e {
        client,
        daemon,
        _handle: handle,
        _tmp: tmp,
    }
}

async fn next_msg(client: &mut Client) -> ServerMsg {
    tokio::time::timeout(TEST_TIMEOUT, client.next_msg())
        .await
        .expect("timed out waiting for server message")
        .expect("connection closed")
}

async fn await_snapshot(client: &mut Client) -> Snapshot {
    loop {
        if let ServerMsg::Snapshot(s) = next_msg(client).await {
            return s;
        }
    }
}

async fn await_session_state(client: &mut Client, session_id: u64, state: SessionState) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let ServerMsg::Event(Event::SessionChanged(s)) = next_msg(client).await {
                if s.id == session_id && s.state == state {
                    return;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("session {session_id} never reached {state:?}"))
}

async fn await_pty_output(client: &mut Client, needle: &str) -> String {
    let mut acc = Vec::new();
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if let ServerMsg::PtyOutput { data, .. } = next_msg(client).await {
                acc.extend_from_slice(&data);
                if String::from_utf8_lossy(&acc).contains(needle) {
                    return String::from_utf8_lossy(&acc).into_owned();
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for pty output {needle:?}"))
}

#[tokio::test]
async fn full_lifecycle_over_the_socket() {
    let mut e = e2e().await;
    let tmp_project = tempfile::tempdir().unwrap();

    let bucket_id = e
        .client
        .request(ClientMsg::CreateBucket {
            name: "work".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();
    let project_id = e
        .client
        .request(ClientMsg::CreateProject {
            bucket_id,
            name: "api".into(),
            path: tmp_project.path().display().to_string(),
            worker_id: None,
            allowed_worker_ids: vec![0],
        })
        .await
        .unwrap()
        .unwrap();

    e.client
        .request(ClientMsg::Subscribe { scope: Scope::All })
        .await
        .unwrap();
    let snapshot = await_snapshot(&mut e.client).await;
    assert_eq!(snapshot.buckets.len(), 1);
    assert_eq!(snapshot.projects.len(), 1);
    assert!(snapshot.sessions.is_empty());

    let session_id = e
        .client
        .request(ClientMsg::SpawnSession {
            project_id,
            agent: Some(AgentKind::Test),
            task_title: "demo".into(),
            task_prompt: "e2e-prompt".into(),
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
    await_session_state(&mut e.client, session_id, SessionState::Working).await;

    e.client
        .request(ClientMsg::AttachPty { session_id })
        .await
        .unwrap();
    await_pty_output(&mut e.client, "READY e2e-prompt").await;

    e.client
        .send(ClientMsg::PtyInput {
            session_id,
            data: Bytes::from_static(b"echo through-the-socket\n"),
        })
        .unwrap();
    await_pty_output(&mut e.client, "OUT through-the-socket").await;

    e.client
        .send(ClientMsg::PtyInput {
            session_id,
            data: Bytes::from_static(b"exit 0\n"),
        })
        .unwrap();
    await_session_state(&mut e.client, session_id, SessionState::Exited).await;
}

#[tokio::test]
async fn stop_hook_nudge_travels_over_the_socket() {
    let e = e2e().await;
    let tmp_project = tempfile::tempdir().unwrap();
    let bucket_id = e
        .client
        .request(ClientMsg::CreateBucket {
            name: "b".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();
    let project_id = e
        .client
        .request(ClientMsg::CreateProject {
            bucket_id,
            name: "p".into(),
            path: tmp_project.path().display().to_string(),
            worker_id: None,
            allowed_worker_ids: vec![0],
        })
        .await
        .unwrap()
        .unwrap();
    let session_id = e
        .client
        .request(ClientMsg::SpawnSession {
            project_id,
            agent: Some(AgentKind::Test),
            task_title: "demo".into(),
            task_prompt: "e2e".into(),
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
    let token = e.daemon.session_token(session_id).unwrap().unwrap();

    let hook = |kind| ClientMsg::HookEvent {
        session_token: token.clone(),
        kind,
        detail: String::new(),
        agent_session_id: String::new(),
        transcript_path: String::new(),
        background_work: false,
    };

    // A turn that ends with no headline carries the nudge in the reply data.
    let data = e
        .client
        .request_data(hook(HookKind::TurnEnded))
        .await
        .unwrap();
    assert!(
        !data.is_empty() && String::from_utf8_lossy(&data).contains("report"),
        "an unnamed turn end should carry a report nudge over the socket"
    );

    // Once the agent names the session, a later turn end carries nothing.
    e.daemon
        .handle_agent_report(
            &token,
            pm_daemon::daemon::AgentReport::Report {
                goal: String::new(),
                headline: "named now".into(),
                summary: None,
                note: String::new(),
                glance: None,
                context: None,
                clear: Vec::new(),
                git: Default::default(),
            },
        )
        .unwrap();
    let after = e
        .client
        .request_data(hook(HookKind::TurnEnded))
        .await
        .unwrap();
    assert!(
        after.is_empty(),
        "a named session must not carry a nudge, got {:?}",
        String::from_utf8_lossy(&after)
    );
}

#[tokio::test]
async fn second_viewer_gets_replay_over_the_socket() {
    let mut e = e2e().await;
    let tmp_project = tempfile::tempdir().unwrap();
    let bucket_id = e
        .client
        .request(ClientMsg::CreateBucket {
            name: "b".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();
    let project_id = e
        .client
        .request(ClientMsg::CreateProject {
            bucket_id,
            name: "p".into(),
            path: tmp_project.path().display().to_string(),
            worker_id: None,
            allowed_worker_ids: vec![0],
        })
        .await
        .unwrap()
        .unwrap();
    let session_id = e
        .client
        .request(ClientMsg::SpawnSession {
            project_id,
            agent: Some(AgentKind::Test),
            task_title: "t".into(),
            task_prompt: "replay-me".into(),
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

    e.client
        .request(ClientMsg::AttachPty { session_id })
        .await
        .unwrap();
    await_pty_output(&mut e.client, "READY replay-me").await;

    let mut second = Client::connect(&e._handle.socket_path).await.unwrap();
    second
        .request(ClientMsg::AttachPty { session_id })
        .await
        .unwrap();
    await_pty_output(&mut second, "READY replay-me").await;
}

#[tokio::test]
async fn errors_come_back_as_command_results() {
    let e = e2e().await;

    let err = e
        .client
        .request(ClientMsg::SpawnSession {
            project_id: 999,
            agent: Some(AgentKind::Test),
            task_title: "t".into(),
            task_prompt: "p".into(),
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
        .unwrap_err();
    assert!(err.to_string().contains("project"), "{err}");

    let err = e
        .client
        .request(ClientMsg::AttachPty { session_id: 42 })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");

    let err = e
        .client
        .request(ClientMsg::DeleteBucket { id: 7 })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}

#[tokio::test]
async fn list_workspaces_returns_all_users_layouts_as_json() {
    let e = e2e().await;

    let empty = e
        .client
        .request_data(ClientMsg::ListWorkspaces)
        .await
        .unwrap();
    assert_eq!(&empty[..], b"[]");

    e.daemon.auth_setup("owner", "hunter2hunter2").unwrap();
    let workspace = e
        .daemon
        .create_workspace(
            1,
            "release watch",
            r#"{"kind":"pane","paneId":"p1","terminalId":"7"}"#,
        )
        .unwrap();

    let data = e
        .client
        .request_data(ClientMsg::ListWorkspaces)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(parsed[0]["id"].as_u64(), Some(workspace.id));
    assert_eq!(parsed[0]["name"], "release watch");
    assert_eq!(parsed[0]["layout"]["terminalId"], "7");
    assert_eq!(parsed[0]["position"], 0);
}

#[tokio::test]
async fn item_commands_round_trip_over_the_socket() {
    use pm_protocol::domain::{ItemQuery, ItemStatus, ItemWrite, RespondTarget};

    let mut e = e2e().await;
    let bucket_id = e
        .client
        .request(ClientMsg::CreateBucket {
            name: "work".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();

    let item_id = e
        .client
        .request(ClientMsg::UpsertItem(Box::new(ItemWrite {
            bucket_id,
            title: Some("reply to alice".into()),
            question: Some("Should we answer now?".into()),
            note: Some("she asked twice".into()),
            ..Default::default()
        })))
        .await
        .unwrap()
        .unwrap();

    let routed = e
        .client
        .request(ClientMsg::RespondToItem {
            bucket_id,
            item_id,
            text: "Yes, answer today.".into(),
            target: RespondTarget::ReplyOnly,
        })
        .await
        .unwrap();
    assert_eq!(routed, None);

    // Status change referencing only the item id (the CLI's done path).
    e.client
        .request(ClientMsg::UpsertItem(Box::new(ItemWrite {
            bucket_id,
            id: Some(item_id),
            status: Some(ItemStatus::Done),
            ..Default::default()
        })))
        .await
        .unwrap();

    let data = e
        .client
        .request_data(ClientMsg::ListItems(ItemQuery {
            bucket_id,
            include_closed: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    let items: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(items[0]["id"].as_u64(), Some(item_id));
    assert_eq!(items[0]["status"], "done");
    assert_eq!(items[0]["question"], "");

    let data = e
        .client
        .request_data(ClientMsg::ItemNotes { bucket_id, item_id })
        .await
        .unwrap();
    let detail: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(detail["item"]["title"], "reply to alice");
    let kinds: Vec<&str> = detail["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["created", "note", "note", "user_reply", "status"]);

    // Snooze, then subscribe: the snapshot carries the snoozed item.
    e.client
        .request(ClientMsg::SnoozeItem {
            bucket_id,
            id: item_id,
            until_unix_ms: Some(i64::MAX),
        })
        .await
        .unwrap();
    e.client
        .request(ClientMsg::Subscribe { scope: Scope::All })
        .await
        .unwrap();
    let snapshot = await_snapshot(&mut e.client).await;
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].snoozed_until_unix_ms, Some(i64::MAX));

    e.client
        .request(ClientMsg::DeleteItem {
            bucket_id,
            id: item_id,
        })
        .await
        .unwrap();
    let data = e
        .client
        .request_data(ClientMsg::ListItems(ItemQuery {
            bucket_id,
            include_closed: true,
            include_snoozed: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    let items: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(items.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn item_body_limit_is_lossless_and_explicit_over_the_socket() {
    use pm_daemon::storage::ITEM_BODY_MAX;
    use pm_protocol::domain::{ItemQuery, ItemWrite};

    let e = e2e().await;
    let bucket_id = e
        .client
        .request(ClientMsg::CreateBucket {
            name: "large items".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();
    let body = format!("{}😀", "界".repeat(ITEM_BODY_MAX - 1));
    let item_id = e
        .client
        .request(ClientMsg::UpsertItem(Box::new(ItemWrite {
            bucket_id,
            title: Some("boundary".into()),
            body: Some(body.clone()),
            ..Default::default()
        })))
        .await
        .unwrap()
        .unwrap();

    let error = e
        .client
        .request(ClientMsg::UpsertItem(Box::new(ItemWrite {
            bucket_id,
            id: Some(item_id),
            body: Some(format!("{body}x")),
            ..Default::default()
        })))
        .await
        .unwrap_err();
    let error = error.to_string();
    assert!(error.contains("body is 65537 Unicode scalar values"));
    assert!(error.contains("limit is 65536"));

    let data = e
        .client
        .request_data(ClientMsg::ListItems(ItemQuery {
            bucket_id,
            ..Default::default()
        }))
        .await
        .unwrap();
    let items: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(items[0]["body"], body);
}

#[tokio::test]
async fn unqualified_protocol_item_commands_never_resolve_old_surrogates() {
    let e = e2e().await;
    let one = e
        .client
        .request(ClientMsg::CreateBucket {
            name: "identity one".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();
    let two = e
        .client
        .request(ClientMsg::CreateBucket {
            name: "identity two".into(),
            allowed_worker_ids: vec![0],
            default_worker_id: 0,
            is_default: false,
        })
        .await
        .unwrap()
        .unwrap();
    for (bucket_id, title) in [(one, "one"), (two, "two")] {
        let id = e
            .client
            .request(ClientMsg::UpsertItem(Box::new(ItemWrite {
                bucket_id,
                title: Some(title.into()),
                ..Default::default()
            })))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id, 1);
    }

    let legacy_surrogate = 2;
    let attempts = [
        e.client
            .request(ClientMsg::UpsertItem(Box::new(ItemWrite {
                id: Some(legacy_surrogate),
                title: Some("stolen".into()),
                ..Default::default()
            })))
            .await
            .unwrap_err(),
        e.client
            .request(ClientMsg::DeleteItem {
                bucket_id: 0,
                id: legacy_surrogate,
            })
            .await
            .unwrap_err(),
        e.client
            .request(ClientMsg::SnoozeItem {
                bucket_id: 0,
                id: legacy_surrogate,
                until_unix_ms: Some(10),
            })
            .await
            .unwrap_err(),
        e.client
            .request(ClientMsg::RespondToItem {
                bucket_id: 0,
                item_id: legacy_surrogate,
                text: "stolen".into(),
                target: RespondTarget::ReplyOnly,
            })
            .await
            .unwrap_err(),
    ];
    for error in attempts {
        assert!(
            error
                .to_string()
                .contains("unqualified legacy item ids are unsupported")
                || error.to_string().contains("a bucket is required"),
            "{error}"
        );
    }
    let notes = e
        .client
        .request_data(ClientMsg::ItemNotes {
            bucket_id: 0,
            item_id: legacy_surrogate,
        })
        .await
        .unwrap_err();
    assert!(notes
        .to_string()
        .contains("unqualified legacy item ids are unsupported"));

    for (bucket_id, title) in [(one, "one"), (two, "two")] {
        let data = e
            .client
            .request_data(ClientMsg::ListItems(ItemQuery {
                bucket_id,
                ..Default::default()
            }))
            .await
            .unwrap();
        let items: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(items[0]["title"], title);
    }
}
