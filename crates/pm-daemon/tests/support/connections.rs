//! What the connection tests share: a signed-in API client, an agent tool
//! call, and a REST upstream behind a tested, active connection.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tower::ServiceExt;

use super::*;

const POLL_DELAY: Duration = Duration::from_millis(10);
/// The shortest hold a call tool accepts. A test that expects a call to stay
/// unsettled pays this much for it.
pub const SHORTEST_WAIT_MS: u64 = 5_000;

pub async fn api(
    env: &TestEnv,
    bearer: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = pm_daemon::http::router(env.daemon.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", bearer)
                .header("host", TEST_HOST)
                .header("origin", TEST_ORIGIN)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(
        response.into_body(),
        pm_daemon::connections::MAX_DOCUMENT_BYTES,
    )
    .await
    .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
    )
}

pub async fn tool(env: &TestEnv, token: &str, name: &str, args: Value) -> Value {
    let outcome=pm_daemon::mcp::dispatch(&env.daemon,token,json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).await;
    let result = outcome.body.unwrap()["result"].clone();
    if result["isError"] == true {
        return result;
    }
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

pub fn schema() -> Value {
    let request_body = json!({"required":true,"content":{"application/json":{"schema":{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}}}});
    json!({"openapi":"3.1.0","paths":{
        "/orders":{"get":{"operationId":"list_orders","parameters":[{"in":"query","name":"q","schema":{"type":"string"}}]}},
        "/orders/{id}":{"put":{"operationId":"update_order","parameters":[{"in":"path","name":"id","required":true,"schema":{"type":"string"}}],"requestBody":request_body}}
    }})
}

pub async fn rest_upstream(counter: Arc<AtomicUsize>) -> (String, tokio::task::JoinHandle<()>) {
    let count = counter.clone();
    let router = Router::new()
        .route(
            "/",
            get(|headers: axum::http::HeaderMap| async move {
                if headers.get("authorization").and_then(|h| h.to_str().ok())
                    == Some("Bearer placeholder-credential")
                {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            }),
        )
        .route(
            "/orders",
            get(
                |headers: axum::http::HeaderMap,
                 axum::extract::Query(query): axum::extract::Query<
                    std::collections::HashMap<String, String>,
                >| async move {
                    assert_eq!(headers["authorization"], "Bearer placeholder-credential");
                    if query.get("q").is_some_and(|value| value == "fail") {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error":"Invalid query"})),
                        )
                    } else {
                        (StatusCode::OK, Json(json!({"orders":["first"]})))
                    }
                },
            ),
        )
        .route(
            "/orders/{id}",
            axum::routing::put(
                move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                    let count = count.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer placeholder-credential");
                        count.fetch_add(1, Ordering::SeqCst);
                        Json(body)
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}"), task)
}

/// Saves a JSON document where a session's agent would, and names it the way the agent passes it.
pub fn schema_file(env: &TestEnv, name: &str, schema: &Value) -> String {
    std::fs::write(env.project_root().join(name), schema.to_string()).unwrap();
    name.to_owned()
}

pub async fn seed_and_finish(
    env: &TestEnv,
    bearer: &str,
    token: &str,
    endpoint: &str,
    kind: &str,
    schema: Option<Value>,
) -> Value {
    let mut args = json!({"name":"orders","kind":kind,"endpoint":endpoint});
    if let Some(schema) = schema {
        args["schema_path"] = json!(schema_file(env, "openapi.json", &schema));
    }
    let mut connection = tool(env, token, "seed_connection", args).await;
    assert!(connection["id"].is_number(), "{connection}");
    assert_eq!(connection["active"], false);
    let id = connection["id"].as_u64().unwrap();
    let (status, full) = api(
        env,
        bearer,
        "GET",
        &format!("/api/connections/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    connection = full;
    let (status,updated)=api(env,bearer,"PUT",&format!("/api/connections/{id}"),json!({"revision":connection["revision"],"config":connection["config"],"credential":{"mode":"bearer","token":"placeholder-credential"}})).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert!(!updated.to_string().contains("placeholder-credential"));
    connection = updated;
    let (status, tested) = api(
        env,
        bearer,
        "POST",
        &format!("/api/connections/{id}/test"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(tested["tested_revision"], connection["revision"]);
    let rules = if kind == "openapi" {
        json!({"list_orders":{"access":"read"},"update_order":{"access":"write"}})
    } else {
        json!({"read":{"access":"read"},"write":{"access":"write"}})
    };
    let proposed=tool(env,token,"propose_connection_policy",json!({"connection_id":id,"revision":tested["revision"],"policy_path":schema_file(env,"policy.json",&json!({"read_policy":"allow","write_policy":"approve","unknown_policy":"approve","rules":rules})),"explanation":"Allow reviewed reads and require approval for writes"})).await;
    assert_eq!(proposed["status"], "pending_user_confirmation");
    let unchanged = tool(env, token, "get_connection", json!({"connection_id":id})).await;
    assert_eq!(unchanged["config"]["rules"], json!({}));
    let (status,confirmed)=api(env,bearer,"POST",&format!("/api/connections/{id}/policy"),json!({"revision":tested["revision"],"proposal_id":proposed["proposal"]["id"],"accept":true})).await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    let (status, active) = api(
        env,
        bearer,
        "POST",
        &format!("/api/connections/{id}/active"),
        json!({"revision":confirmed["revision"],"active":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{active}");
    active
}

pub async fn settled(env: &TestEnv, token: &str, id: &Value) -> Value {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let call = tool(env, token, "get_connection_call", json!({"call_id":id})).await;
            if !matches!(call["status"].as_str(), Some("authorized" | "executing")) {
                break call;
            }
            tokio::time::sleep(POLL_DELAY).await;
        }
    })
    .await
    .unwrap()
}
