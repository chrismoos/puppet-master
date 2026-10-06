// The scripted agent's inbox directory is named by a process-wide
// environment variable, so these tests live in their own binary.

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::{atomic::AtomicUsize, Arc};
use std::time::Duration;
use support::connections::*;
use support::*;

/// The inbox directory is process-wide, so one test at a time.
static ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Long enough for a notice that was sent to arrive.
const NOTICE_GRACE: Duration = Duration::from_millis(500);

async fn settle_notice(inbox: &mut AgentInbox, within: Duration) -> Option<String> {
    tokio::time::timeout(within, async {
        loop {
            match inbox.lines.recv().await {
                Some(line) if line.contains("settled as") => break line,
                Some(_) => {}
                None => std::future::pending().await,
            }
        }
    })
    .await
    .ok()
}

#[tokio::test]
async fn a_result_returned_by_the_wait_sends_no_notice() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let bearer = signed_in_bearer(&env.daemon);
    let (endpoint, server) = rest_upstream(Arc::new(AtomicUsize::new(0))).await;
    let connection =
        seed_and_finish(&env, &bearer, &token, &endpoint, "openapi", Some(schema())).await;
    let mut inbox = agent_inbox(&env, session);

    let read = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":connection["id"],"tool":"list_orders","arguments":{},"request_id":"read"}),
    )
    .await;
    assert_eq!(read["status"], "succeeded", "{read}");
    assert_eq!(settle_notice(&mut inbox, NOTICE_GRACE).await, None);
    server.abort();
}

#[tokio::test]
async fn a_call_that_settles_after_its_wait_gave_up_sends_a_notice() {
    let _lock = ENV_MUTEX.lock().await;
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let bearer = signed_in_bearer(&env.daemon);
    let (endpoint, server) = rest_upstream(Arc::new(AtomicUsize::new(0))).await;
    let connection =
        seed_and_finish(&env, &bearer, &token, &endpoint, "openapi", Some(schema())).await;
    let mut inbox = agent_inbox(&env, session);

    let pending = tool(&env, &token, "call_connection_tool", json!({"connection_id":connection["id"],"tool":"update_order","arguments":{"path":{"id":"first"},"body":{"name":"approved"}},"request_id":"write","justification":"Rename order","wait_ms":SHORTEST_WAIT_MS})).await;
    assert_eq!(pending["status"], "pending", "{pending}");
    let path = format!(
        "/api/connection-calls/{}/decision",
        pending["id"].as_str().unwrap()
    );
    let (status, _) = api(&env, &bearer, "POST", &path, json!({"approve":true})).await;
    assert_eq!(status, StatusCode::OK);

    let notice = settle_notice(&mut inbox, TEST_TIMEOUT)
        .await
        .expect("a settle notice");
    assert!(notice.contains(pending["id"].as_str().unwrap()));
    assert!(notice.contains("settled as succeeded"));
    let _: Value = settled(&env, &token, &pending["id"]).await;
    server.abort();
}
