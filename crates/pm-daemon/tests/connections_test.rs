mod support;

use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use support::connections::*;
use support::*;

#[tokio::test]
async fn rest_reads_execute_and_writes_execute_only_once_after_native_approval() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let bearer = signed_in_bearer(&env.daemon);
    let count = Arc::new(AtomicUsize::new(0));
    let (endpoint, server) = rest_upstream(count.clone()).await;
    let connection =
        seed_and_finish(&env, &bearer, &token, &endpoint, "openapi", Some(schema())).await;
    let id = connection["id"].clone();
    let read = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":id,"tool":"list_orders","arguments":{},"request_id":"read"}),
    )
    .await;
    assert_eq!(
        settled(&env, &token, &read["id"]).await["status"],
        "succeeded"
    );
    let failure = tool(&env, &token, "call_connection_tool", json!({"connection_id":id,"tool":"list_orders","arguments":{"query":{"q":"fail"}},"request_id":"failing-read"})).await;
    let failed = settled(&env, &token, &failure["id"]).await;
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["result"]["status"], StatusCode::BAD_REQUEST.as_u16());
    assert_eq!(failed["result"]["isError"], true);
    assert!(failed["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Invalid query"));
    let args = json!({"connection_id":id,"tool":"update_order","arguments":{"path":{"id":"first"},"body":{"name":"approved"}},"request_id":"write","justification":"Rename order","wait_ms":SHORTEST_WAIT_MS});
    let pending = tool(&env, &token, "call_connection_tool", args.clone()).await;
    assert_eq!(pending["status"], "pending");
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let path = format!(
        "/api/connection-calls/{}/decision",
        pending["id"].as_str().unwrap()
    );
    let (first, second) = tokio::join!(
        api(&env, &bearer, "POST", &path, json!({"approve":true})),
        api(&env, &bearer, "POST", &path, json!({"approve":true}))
    );
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(second.0, StatusCode::OK);
    let result = settled(&env, &token, &pending["id"]).await;
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let details_path = format!("/api/connection-calls/{}", pending["id"].as_str().unwrap());
    let (status, details) = api(&env, &bearer, "GET", &details_path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(details["arguments"], args["arguments"]);
    assert_eq!(details["result"], result["result"]);
    let (status, _) = api(&env, "", "GET", &details_path, Value::Null).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, history) = api(&env, &bearer, "GET", "/api/connection-calls", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let summary = history
        .as_array()
        .unwrap()
        .iter()
        .find(|call| call["id"] == pending["id"])
        .unwrap();
    assert!(summary["arguments"].is_null());
    assert!(summary["result"].is_null());
    assert_eq!(summary["has_result"], true);
    let duplicate = tool(&env, &token, "call_connection_tool", args).await;
    assert_eq!(duplicate["id"], pending["id"]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    server.abort();
}

/// The inbox lists only approvals, newest first, and deciding there keeps once-only execution.
#[tokio::test]
async fn approvals_inbox_lists_newest_first_and_decides_once_with_exact_arguments() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let bearer = signed_in_bearer(&env.daemon);
    let count = Arc::new(AtomicUsize::new(0));
    let (endpoint, server) = rest_upstream(count.clone()).await;
    let connection =
        seed_and_finish(&env, &bearer, &token, &endpoint, "openapi", Some(schema())).await;
    let id = connection["id"].clone();
    let read = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":id,"tool":"list_orders","arguments":{},"request_id":"read"}),
    )
    .await;
    assert_eq!(
        settled(&env, &token, &read["id"]).await["status"],
        "succeeded"
    );
    let write = |order: &str| json!({"connection_id":id,"tool":"update_order","arguments":{"path":{"id":order},"body":{"name":format!("renamed {order}")}},"request_id":order,"justification":format!("Rename {order}"),"wait_ms":SHORTEST_WAIT_MS});
    let older = tool(&env, &token, "call_connection_tool", write("older")).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let newer = tool(&env, &token, "call_connection_tool", write("newer")).await;
    assert_eq!(older["status"], "pending");
    assert_eq!(newer["status"], "pending");

    let (status, _) = api(&env, "", "GET", "/api/connection-approvals", Value::Null).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, inbox) = api(
        &env,
        &bearer,
        "GET",
        "/api/connection-approvals",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{inbox}");
    let ids: Vec<&Value> = inbox
        .as_array()
        .unwrap()
        .iter()
        .map(|call| &call["id"])
        .collect();
    assert_eq!(
        ids,
        vec![&newer["id"], &older["id"]],
        "newest first, reads excluded"
    );
    let first = &inbox[0];
    assert!(
        first["arguments"].is_null(),
        "listings never carry arguments"
    );
    assert_eq!(first["status"], "pending");
    assert_eq!(first["tool"], "update_order");
    assert_eq!(first["justification"], "Rename newer");
    assert_eq!(first["connection_name"], "orders");
    assert_eq!(first["connection_kind"], "openapi");
    assert_eq!(first["tool_access"], "write");
    assert_eq!(first["session_live"], true);
    assert!(first["project_name"].is_string());
    assert!(first["session_name"].is_string());
    assert!(!inbox.to_string().contains("placeholder-credential"));

    let detail =
        |call: &Value| format!("/api/connection-approvals/{}", call["id"].as_str().unwrap());
    let (status, _) = api(&env, &bearer, "GET", &detail(&read), Value::Null).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an allowed read is not an approval"
    );
    let (status, _) = api(&env, "", "GET", &detail(&older), Value::Null).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, shown) = api(&env, &bearer, "GET", &detail(&older), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["arguments"], write("older")["arguments"]);
    assert_eq!(shown["connection_current"], true);

    let decide = |call: &Value| {
        format!(
            "/api/connection-calls/{}/decision",
            call["id"].as_str().unwrap()
        )
    };
    let (status, denied) = api(
        &env,
        &bearer,
        "POST",
        &decide(&newer),
        json!({"approve":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(denied["status"], "denied");
    let (_, late) = api(
        &env,
        &bearer,
        "POST",
        &decide(&newer),
        json!({"approve":true}),
    )
    .await;
    assert_eq!(
        late["status"], "denied",
        "a decided approval cannot be reversed"
    );
    let approve = decide(&older);
    let (first, second) = tokio::join!(
        api(&env, &bearer, "POST", &approve, json!({"approve":true})),
        api(&env, &bearer, "POST", &approve, json!({"approve":true}))
    );
    assert_eq!((first.0, second.0), (StatusCode::OK, StatusCode::OK));
    assert_eq!(
        settled(&env, &token, &older["id"]).await["status"],
        "succeeded"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "the approved write ran once, the denied one never"
    );

    let (_, inbox) = api(
        &env,
        &bearer,
        "GET",
        "/api/connection-approvals",
        Value::Null,
    )
    .await;
    let statuses: Vec<(&Value, &Value)> = inbox
        .as_array()
        .unwrap()
        .iter()
        .map(|call| (&call["id"], &call["status"]))
        .collect();
    assert_eq!(
        statuses,
        vec![
            (&newer["id"], &json!("denied")),
            (&older["id"], &json!("succeeded"))
        ]
    );
    let (_, shown) = api(&env, &bearer, "GET", &detail(&newer), Value::Null).await;
    assert!(shown["decided_by"].is_string());
    server.abort();
}

#[tokio::test]
async fn downstream_mcp_annotations_do_not_bypass_policy_and_tool_results_are_preserved() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let bearer = signed_in_bearer(&env.daemon);
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let router=Router::new().route("/mcp",post(move |Json(request):Json<Value>| {
        let calls=calls.clone();async move {
            if request["id"].is_null() {return StatusCode::ACCEPTED.into_response();}
            let result=match request["method"].as_str().unwrap() {
                "initialize"=>json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}),
                "tools/list"=>json!({"tools":[{"name":"read","description":"Read a record","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}},{"name":"write","inputSchema":{"type":"object"}}]}),
                "tools/call"=>{calls.fetch_add(1,Ordering::SeqCst);json!({"content":[{"type":"text","text":"placeholder-credential result"}],"isError":false})},
                _=>json!({}),
            };
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let connection = seed_and_finish(&env, &bearer, &token, &endpoint, "mcp", None).await;
    let read = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":connection["id"],"tool":"read","arguments":{},"request_id":"read"}),
    )
    .await;
    let result = settled(&env, &token, &read["id"]).await;
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["result"]["content"][0]["text"], "[redacted] result");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let pending=tool(&env,&token,"call_connection_tool",json!({"connection_id":connection["id"],"tool":"write","arguments":{},"request_id":"write","justification":"Write test","wait_ms":SHORTEST_WAIT_MS})).await;
    assert_eq!(pending["status"], "pending");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn native_configuration_requires_user_auth_and_workers_cannot_access_other_projects() {
    let env = daemon_env();
    let worker = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(worker).unwrap().unwrap();
    let supervisor = spawn_supervisor(&env);
    let sup_token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let bucket = env.daemon.create_bucket("other").unwrap();
    let project = env
        .daemon
        .create_project(bucket, "other", env.project_root().to_str().unwrap())
        .unwrap();
    let endpoint = format!("http://{}", std::net::Ipv4Addr::LOCALHOST);
    let denied = api(
        &env,
        "",
        "POST",
        "/api/connections",
        json!({"name":"unauthorized","project_id":env.project_id,"endpoint":endpoint}),
    )
    .await;
    assert_eq!(denied.0, StatusCode::UNAUTHORIZED);
    let refused = tool(
        &env,
        &sup_token,
        "seed_connection",
        json!({"name":"other","project_id":project,"kind":"mcp","endpoint":endpoint}),
    )
    .await;
    assert_eq!(refused["isError"], true);
    let bearer = signed_in_bearer(&env.daemon);
    let (status, foreign) = api(
        &env,
        &bearer,
        "POST",
        "/api/connections",
        json!({"name":"other","project_id":project,"kind":"mcp","endpoint":endpoint}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let own = tool(
        &env,
        &token,
        "seed_connection",
        json!({"name":"own","project_id":project,"kind":"mcp","endpoint":endpoint}),
    )
    .await;
    assert_eq!(own["config"]["project_id"], env.project_id);
    assert_eq!(
        tool(&env, &token, "list_connections", json!({}))
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        tool(&env, &sup_token, "list_connections", json!({}))
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        tool(
            &env,
            &token,
            "get_connection",
            json!({"connection_id":foreign["id"]})
        )
        .await["isError"],
        true
    );
}

#[tokio::test]
async fn large_rest_tool_lists_are_filtered_and_paged_without_full_schemas() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let paths = (0..125)
        .map(|index| {
            (
                format!("/record/{index}"),
                json!({"get":{"operationId":format!("record_{index:03}")}}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let connection=tool(&env,&token,"seed_connection",json!({"name":"large","kind":"openapi","endpoint":format!("http://{}",std::net::Ipv4Addr::LOCALHOST),"schema_path":schema_file(&env,"large.json",&json!({"openapi":"3.1.0","paths":paths}))})).await;
    let page = tool(
        &env,
        &token,
        "list_connection_tools",
        json!({"connection_id":connection["id"],"offset":50,"limit":50}),
    )
    .await;
    assert_eq!(page["total"], 125);
    assert_eq!(page["tools"].as_array().unwrap().len(), 50);
    assert_eq!(page["tools"][0]["name"], "record_050");
    assert!(page["tools"][0].get("input_schema").is_none());
    let filtered = tool(
        &env,
        &token,
        "list_connection_tools",
        json!({"connection_id":connection["id"],"query":"record_124"}),
    )
    .await;
    assert_eq!(filtered["total"], 1);
    let detail = tool(
        &env,
        &token,
        "describe_connection_tool",
        json!({"connection_id":connection["id"],"tool":"record_124"}),
    )
    .await;
    assert_eq!(detail["tool"]["input_schema"]["type"], "object");
    let catalog = tool(&env, &token, "list_connections", json!({})).await;
    assert_eq!(catalog[0]["tool_count"], 125);
    let bearer = signed_in_bearer(&env.daemon);
    let (status, summaries) = api(&env, &bearer, "GET", "/api/connections", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summaries[0]["tool_count"], 125);
    assert_eq!(summaries[0]["tools"], json!([]));
    assert_eq!(summaries[0]["config"]["schema"], Value::Null);
    assert_eq!(summaries[0]["config"]["rules"], json!({}));
}

#[tokio::test]
async fn oauth_pkce_callback_is_one_use_and_refresh_uses_configured_client_auth() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    let env = daemon_env();
    let bearer = signed_in_bearer(&env.daemon);
    let exchanges = Arc::new(std::sync::Mutex::new(Vec::<HashMap<String, String>>::new()));
    let captured = exchanges.clone();
    let router=Router::new().route("/token",post(move |headers:axum::http::HeaderMap,axum::Form(form):axum::Form<HashMap<String,String>>| {
        let captured=captured.clone();
        async move {
            assert_eq!(headers["authorization"], "Basic Y2xpZW50OnBsYWNlaG9sZGVyLXNlY3JldA==");
            assert!(!form.contains_key("resource"));
            assert!(!form.contains_key("client_secret"));
            let refresh=form["grant_type"]=="refresh_token";
            captured.lock().unwrap().push(form);
            Json(json!({"token_type":"Bearer","access_token":if refresh {"refreshed-access"} else {"initial-access"},"refresh_token":"placeholder-refresh","expires_in":if refresh {3600} else {1}}))
        }
    })).route("/",get(|headers:axum::http::HeaderMap|async move {
        if headers.get("authorization").and_then(|value|value.to_str().ok())==Some("Bearer refreshed-access") {StatusCode::OK} else {StatusCode::UNAUTHORIZED}
    }));
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let project = env.project_id;
    let (status,created)=api(&env,&bearer,"POST","/api/connections",json!({"name":"OAuth API","project_id":project,"kind":"openapi","endpoint":endpoint,"schema":schema(),"oauth":{"authorization_url":format!("{endpoint}/authorize"),"token_url":format!("{endpoint}/token"),"client_id":"client","scopes":"orders","redirect_uri":format!("{TEST_ORIGIN}/api/connections/oauth/callback"),"token_auth_method":"client_secret_basic"}})).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["id"].as_u64().unwrap();
    let (status,updated)=api(&env,&bearer,"PUT",&format!("/api/connections/{id}"),json!({"revision":created["revision"],"config":created["config"],"credential":{"mode":"oauth","access_token":"","client_secret":"placeholder-secret"}})).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let (status, _) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/test"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, start) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/oauth/start"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let url = reqwest::Url::parse(start["url"].as_str().unwrap()).unwrap();
    let query = url.query_pairs().into_owned().collect::<HashMap<_, _>>();
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!query.contains_key("resource"));
    let callback = format!(
        "/api/connections/oauth/callback?state={}&code=placeholder-code",
        query["state"]
    );
    let (status, html) = api(&env, "", "GET", &callback, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert_eq!(exchanges.lock().unwrap().len(), 1);
    let verifier = exchanges.lock().unwrap()[0]["code_verifier"].clone();
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes())),
        query["code_challenge"]
    );
    let (_, html) = api(&env, "", "GET", &callback, json!({})).await;
    assert!(html.as_str().unwrap().contains("Sign-in failed"), "{html}");
    assert_eq!(exchanges.lock().unwrap().len(), 1);
    let (status, tested) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/test"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(exchanges.lock().unwrap().len(), 2);
    assert_eq!(
        exchanges.lock().unwrap()[1]["refresh_token"],
        "placeholder-refresh"
    );
    assert!(!tested.to_string().contains("refreshed-access"));
    server.abort();
}

#[tokio::test]
async fn an_agent_seeds_a_large_openapi_document_from_a_file_in_its_working_directory() {
    const LARGE_DESCRIPTION_BYTES: usize = 2 * 1024 * 1024 + 128 * 1024;
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let document = json!({"openapi":"3.1.0","info":{"description":"a".repeat(LARGE_DESCRIPTION_BYTES)},"paths":{"/records":{"get":{"operationId":"records"}}}});
    schema_file(&env, "large-api.json", &document);
    // The working directory is compared by its real path, and macOS reaches
    // its temporary directory through a symlink.
    let absolute = std::fs::canonicalize(env.project_root())
        .unwrap()
        .join("large-api.json");
    for path in ["large-api.json", absolute.to_str().unwrap()] {
        let draft = tool(
            &env,
            &token,
            "seed_connection",
            json!({"name":"Large API","kind":"openapi","endpoint":format!("http://{}",std::net::Ipv4Addr::LOCALHOST),"schema_path":path}),
        )
        .await;
        assert_eq!(draft["tool_count"], 1, "{draft}");
        assert_eq!(draft["config"]["schema"], Value::Null);
    }
}

async fn orders_connection(
    env: &TestEnv,
) -> (
    String,
    String,
    Value,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let session = spawn_test_session(env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let bearer = signed_in_bearer(&env.daemon);
    let count = Arc::new(AtomicUsize::new(0));
    let (endpoint, server) = rest_upstream(count.clone()).await;
    let connection =
        seed_and_finish(env, &bearer, &token, &endpoint, "openapi", Some(schema())).await;
    (token, bearer, connection["id"].clone(), count, server)
}

fn order_write(connection: &Value, wait_ms: u64) -> Value {
    json!({"connection_id":connection,"tool":"update_order","arguments":{"path":{"id":"first"},"body":{"name":"approved"}},"request_id":"write","justification":"Rename order","wait_ms":wait_ms})
}

#[tokio::test]
async fn an_allowed_call_returns_its_result_in_the_same_response() {
    let env = daemon_env();
    let (token, _bearer, connection, count, server) = orders_connection(&env).await;
    let read = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":connection,"tool":"list_orders","arguments":{},"request_id":"read"}),
    )
    .await;
    assert_eq!(read["status"], "succeeded", "{read}");
    assert!(read["result"].is_object());
    assert!(read["waiting"].is_null());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn an_approval_given_during_the_wait_returns_the_result_in_the_same_response() {
    let env = daemon_env();
    let (token, bearer, connection, count, server) = orders_connection(&env).await;
    let approve = async {
        let pending = tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                let (_, calls) =
                    api(&env, &bearer, "GET", "/api/connection-calls", Value::Null).await;
                let pending = calls
                    .as_array()
                    .and_then(|calls| calls.iter().find(|call| call["status"] == "pending"))
                    .map(|call| call["id"].as_str().unwrap().to_owned());
                if let Some(id) = pending {
                    break id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let path = format!("/api/connection-calls/{pending}/decision");
        api(&env, &bearer, "POST", &path, json!({"approve":true})).await
    };
    let longest_wait_ms = 20_000;
    let (call, decision) = tokio::join!(
        tool(
            &env,
            &token,
            "call_connection_tool",
            order_write(&connection, longest_wait_ms)
        ),
        approve
    );
    assert_eq!(decision.0, StatusCode::OK);
    assert_eq!(call["status"], "succeeded", "{call}");
    assert_eq!(call["decided_by"], decision.1["decided_by"]);
    assert!(call["waiting"].is_null());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn an_unsettled_call_holds_for_the_shortest_wait_and_says_how_to_keep_waiting() {
    let env = daemon_env();
    let (token, _bearer, connection, count, server) = orders_connection(&env).await;
    let below_the_floor_ms = 1;
    let started = std::time::Instant::now();
    let pending = tool(
        &env,
        &token,
        "call_connection_tool",
        order_write(&connection, below_the_floor_ms),
    )
    .await;
    assert!(started.elapsed() >= Duration::from_millis(SHORTEST_WAIT_MS));
    assert_eq!(pending["status"], "pending", "{pending}");
    assert_eq!(pending["waiting"]["stalled"], false);
    assert!(pending["waiting"]["elapsed_ms"].as_u64().unwrap() > 0);
    assert!(pending["waiting"]["advice"]
        .as_str()
        .unwrap()
        .contains("get_connection_call"));

    let again = tool(
        &env,
        &token,
        "get_connection_call",
        json!({"call_id":pending["id"],"wait_ms":SHORTEST_WAIT_MS}),
    )
    .await;
    assert_eq!(again["id"], pending["id"]);
    assert_eq!(again["status"], "pending");
    assert_eq!(again["waiting"]["stalled"], false);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn a_wait_that_is_not_a_number_is_refused_before_a_call_is_made() {
    let env = daemon_env();
    let (token, bearer, connection, count, server) = orders_connection(&env).await;
    let mut args = order_write(&connection, SHORTEST_WAIT_MS);
    args["wait_ms"] = json!("soon");
    let refused = tool(&env, &token, "call_connection_tool", args).await;
    assert_eq!(refused["isError"], true, "{refused}");
    let (_, calls) = api(&env, &bearer, "GET", "/api/connection-calls", Value::Null).await;
    assert_eq!(calls, json!([]));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn a_connection_reports_when_it_was_tested_and_which_credential_it_holds() {
    let env = daemon_env();
    let (_token, bearer, connection, _count, server) = orders_connection(&env).await;
    let path = format!("/api/connections/{connection}");
    let (status, details) = api(&env, &bearer, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(details["credential_mode"], "bearer");
    assert!(!details.to_string().contains("placeholder-credential"));
    let tested_at = details["tested_at"].as_i64().expect("a test time");
    assert!(tested_at <= unix_ms());

    let (_, listed) = api(&env, &bearer, "GET", "/api/connections", Value::Null).await;
    assert_eq!(listed[0]["tested_at"], tested_at);
    assert!(listed[0]["credential_mode"].is_null());

    let (status, cleared) = api(
        &env,
        &bearer,
        "PUT",
        &path,
        json!({"revision":details["revision"],"config":details["config"],"credential":{"mode":"none"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["credential_set"], false);
    assert!(cleared["credential_mode"].is_null());
    server.abort();
}

#[tokio::test]
async fn a_call_records_when_it_was_decided_and_when_it_finished() {
    let env = daemon_env();
    let (token, bearer, connection, _count, server) = orders_connection(&env).await;
    let pending = tool(
        &env,
        &token,
        "call_connection_tool",
        order_write(&connection, SHORTEST_WAIT_MS),
    )
    .await;
    assert_eq!(pending["status"], "pending", "{pending}");
    assert!(pending["decided_at"].is_null());
    assert!(pending["finished_at"].is_null());
    let path = format!(
        "/api/connection-calls/{}/decision",
        pending["id"].as_str().unwrap()
    );
    let (status, _) = api(&env, &bearer, "POST", &path, json!({"approve":true})).await;
    assert_eq!(status, StatusCode::OK);
    let done = settled(&env, &token, &pending["id"]).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    let created = done["created_at"].as_i64().unwrap();
    let decided = done["decided_at"].as_i64().expect("a decision time");
    let finished = done["finished_at"].as_i64().expect("a finish time");
    assert!(created <= decided && decided <= finished);

    let read = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":connection,"tool":"list_orders","arguments":{},"request_id":"read"}),
    )
    .await;
    assert_eq!(read["status"], "succeeded", "{read}");
    assert!(read["decided_at"].is_null());
    assert!(read["finished_at"].is_i64());
    server.abort();
}

#[tokio::test]
async fn a_setup_request_can_no_longer_be_relayed_to_an_agent_from_a_connection() {
    let env = daemon_env();
    let (_token, bearer, connection, _count, server) = orders_connection(&env).await;
    let (status, _) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{connection}/assist"),
        json!({"session_id":1,"text":"allow reads"}),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    server.abort();
}

fn schema_with_lookup() -> Value {
    let mut next = schema();
    next["paths"]["/orders/{id}"]["get"] = json!({"operationId":"get_order","parameters":[{"in":"path","name":"id","required":true,"schema":{"type":"string"}}]});
    next["paths"]["/orders/{id}"]["put"]["parameters"]
        .as_array_mut()
        .unwrap()
        .push(json!({"in":"query","name":"notify","schema":{"type":"string"}}));
    next
}

async fn accept(env: &TestEnv, bearer: &str, id: &Value, proposal: &Value) -> (StatusCode, Value) {
    api(
        env,
        bearer,
        "POST",
        &format!("/api/connections/{id}/policy"),
        json!({"revision":proposal["revision"],"proposal_id":proposal["id"],"accept":true}),
    )
    .await
}

#[tokio::test]
async fn an_accepted_update_applies_setup_tools_and_policy_and_keeps_the_connection_active() {
    let env = daemon_env();
    let (token, bearer, id, _count, server) = orders_connection(&env).await;
    let live = tool(&env, &token, "get_connection", json!({"connection_id":id})).await;
    let held = tool(
        &env,
        &token,
        "call_connection_tool",
        order_write(&id, SHORTEST_WAIT_MS),
    )
    .await;
    assert_eq!(held["status"], "pending", "{held}");

    let proposed = tool(&env, &token, "propose_connection_update", json!({"connection_id":id,"revision":live["revision"],"config":{"name":"orders v2"},"schema_path":schema_file(&env,"openapi-v2.json",&schema_with_lookup()),"explanation":"Adds the order lookup"})).await;
    assert_eq!(
        proposed["status"], "pending_user_confirmation",
        "{proposed}"
    );
    let proposal = &proposed["proposal"];
    assert_eq!(proposal["changes"]["requires_activation"], false);
    assert_eq!(
        proposal["changes"]["tools"]["added"][0]["name"],
        "get_order"
    );
    assert_eq!(
        proposal["changes"]["tools"]["changed"],
        json!(["update_order"])
    );
    assert_eq!(proposal["changes"]["tools"]["removed"], json!([]));
    assert_eq!(
        proposal["changes"]["fields"],
        json!([{"field":"name","from":"orders","to":"orders v2"},{"field":"schema"}])
    );
    assert_eq!(
        proposal["policy"]["rules"],
        json!({"list_orders":{"access":"read","policy":null}})
    );
    assert!(proposal["setup"]["schema"].is_null());

    let unchanged = tool(&env, &token, "get_connection", json!({"connection_id":id})).await;
    assert_eq!(unchanged["config"]["name"], "orders");
    assert_eq!(unchanged["revision"], live["revision"]);
    assert_eq!(unchanged["policy_proposal"]["id"], proposal["id"]);

    let as_agent = accept(&env, &format!("Bearer {token}"), &id, proposal).await;
    assert_eq!(as_agent.0, StatusCode::UNAUTHORIZED);
    let mut other = proposal.clone();
    other["id"] = json!("another-proposal");
    assert_eq!(
        accept(&env, &bearer, &id, &other).await.0,
        StatusCode::BAD_REQUEST
    );

    let (status, applied) = accept(&env, &bearer, &id, proposal).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["active"], true);
    assert_eq!(applied["tested_revision"], applied["revision"]);
    assert_ne!(applied["revision"], live["revision"]);
    assert_eq!(applied["config"]["name"], "orders v2");
    assert_eq!(applied["credential_set"], true);
    assert!(applied["policy_proposal"].is_null());
    assert_eq!(
        applied["config"]["rules"],
        json!({"list_orders":{"access":"read","policy":null}})
    );
    assert_eq!(
        tool(
            &env,
            &token,
            "get_connection_call",
            json!({"call_id":held["id"]})
        )
        .await["status"],
        "canceled"
    );
    let listed = tool(
        &env,
        &token,
        "list_connection_tools",
        json!({"connection_id":id}),
    )
    .await;
    let policy_of = |name: &str| {
        listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .map(|tool| (tool["classification"].clone(), tool["policy"].clone()))
            .unwrap()
    };
    assert_eq!(policy_of("get_order"), (json!("unknown"), json!("approve")));
    assert_eq!(
        policy_of("update_order"),
        (json!("unknown"), json!("approve"))
    );
    assert_eq!(policy_of("list_orders"), (json!("read"), json!("allow")));
    let read = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":id,"tool":"list_orders","arguments":{},"request_id":"read-after-update"}),
    )
    .await;
    assert_eq!(read["status"], "succeeded", "{read}");
    assert_eq!(
        accept(&env, &bearer, &id, proposal).await.0,
        StatusCode::BAD_REQUEST
    );
    server.abort();
}

#[tokio::test]
async fn an_update_that_moves_the_endpoint_deactivates_and_drops_the_credential_and_rules() {
    let env = daemon_env();
    let (token, bearer, id, _count, server) = orders_connection(&env).await;
    let (moved, other_server) = rest_upstream(Arc::new(AtomicUsize::new(0))).await;
    let live = tool(&env, &token, "get_connection", json!({"connection_id":id})).await;
    let proposed = tool(&env, &token, "propose_connection_update", json!({"connection_id":id,"revision":live["revision"],"config":{"endpoint":moved},"explanation":"The service moved"})).await;
    let proposal = &proposed["proposal"];
    assert_eq!(
        proposal["changes"]["requires_activation"], true,
        "{proposed}"
    );
    assert_eq!(proposal["policy"]["rules"], json!({}));
    let (status, applied) = accept(&env, &bearer, &id, proposal).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["active"], false);
    assert_eq!(applied["credential_set"], false);
    assert!(applied["tested_revision"].is_null());
    assert_eq!(applied["config"]["endpoint"], moved);
    assert_eq!(applied["config"]["rules"], json!({}));
    let refused = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":id,"tool":"list_orders","arguments":{},"request_id":"read-after-move"}),
    )
    .await;
    assert_eq!(refused["isError"], true);
    server.abort();
    other_server.abort();
}

#[tokio::test]
async fn an_update_proposal_is_refused_when_it_is_empty_stale_or_carries_what_it_must_not() {
    let env = daemon_env();
    let (token, _bearer, id, _count, server) = orders_connection(&env).await;
    let live = tool(&env, &token, "get_connection", json!({"connection_id":id})).await;
    let propose = |config: Value, revision: Value| {
        let (env, token, id) = (&env, &token, &id);
        async move {
            tool(env, token, "propose_connection_update", json!({"connection_id":id,"revision":revision,"config":config,"explanation":"Reviewed change"})).await
        }
    };
    for config in [
        json!({}),
        json!({"credential":{"mode":"bearer","token":"placeholder-credential"}}),
        json!({"rules":{"list_orders":{"access":"read"}}}),
        json!({"read_policy":"deny"}),
    ] {
        let refused = propose(config.clone(), live["revision"].clone()).await;
        assert_eq!(refused["isError"], true, "{config}");
    }
    let unknown_tool = tool(&env, &token, "propose_connection_update", json!({"connection_id":id,"revision":live["revision"],"config":{},"policy_path":schema_file(&env,"unknown.json",&json!({"rules":{"missing_tool":{"access":"read"}}})),"explanation":"Reviewed change"})).await;
    assert_eq!(unknown_tool["isError"], true, "{unknown_tool}");
    let stale = propose(json!({"name":"renamed"}), json!(1)).await;
    assert_eq!(stale["isError"], true, "{stale}");
    let after = tool(&env, &token, "get_connection", json!({"connection_id":id})).await;
    assert!(after["policy_proposal"].is_null());
    assert_eq!(after["active"], true);
    server.abort();
}

/// Removal is the user's, needs the current revision, and is a soft
/// delete: the connection leaves the list and the agent's tools, its
/// waiting call is canceled, and its page still reads for the history.
#[tokio::test]
async fn a_removed_connection_leaves_the_list_and_keeps_its_history() {
    let env = daemon_env();
    let (token, bearer, id, _count, _server) = orders_connection(&env).await;
    let id = id.as_u64().unwrap();
    let path = format!("/api/connections/{id}");
    let (status, connection) = api(&env, &bearer, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let pending = tool(&env, &token, "call_connection_tool", json!({"connection_id":id,"tool":"update_order","arguments":{"path":{"id":"first"},"body":{"name":"held"}},"request_id":"held-write","justification":"Rename order","wait_ms":SHORTEST_WAIT_MS})).await;
    assert_eq!(pending["status"], "pending");

    let (status, _) = api(
        &env,
        "",
        "DELETE",
        &path,
        json!({"revision":connection["revision"]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, stale) = api(&env, &bearer, "DELETE", &path, json!({"revision":0})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{stale}");
    let (status, removed) = api(
        &env,
        &bearer,
        "DELETE",
        &path,
        json!({"revision":connection["revision"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert!(removed["deleted_at"].is_number());
    assert_eq!(removed["active"], false);

    let (status, listed) = api(&env, &bearer, "GET", "/api/connections", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed.as_array().unwrap().is_empty(), "{listed}");
    assert!(tool(&env, &token, "list_connections", json!({}))
        .await
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        tool(&env, &token, "get_connection", json!({"connection_id":id})).await["isError"],
        true
    );
    let (status, details) = api(&env, &bearer, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(details["deleted_at"].is_number());
    let (status, call) = api(
        &env,
        &bearer,
        "GET",
        &format!("/api/connection-calls/{}", pending["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(call["status"], "canceled");
    let (status, again) = api(
        &env,
        &bearer,
        "POST",
        &format!("{path}/active"),
        json!({"revision":removed["revision"],"active":true}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{again}");
}

#[tokio::test]
async fn a_schema_url_that_needs_a_login_is_refused_with_the_way_around_it() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    // The upstream root answers 401 to a request that carries no credential.
    let (endpoint, server) = rest_upstream(Arc::new(AtomicUsize::new(0))).await;
    let refused = tool(
        &env,
        &token,
        "seed_connection",
        json!({"name":"Orders API","kind":"openapi","endpoint":endpoint,"schema_url":endpoint}),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");
    let message = refused["content"][0]["text"].as_str().unwrap();
    assert!(message.contains("HTTP 401"), "{message}");
    assert!(message.contains("publicly reachable without authentication"));
    assert!(message.contains("pass schema_path"));
    server.abort();
}

#[tokio::test]
async fn the_seeding_tools_say_a_schema_url_must_be_public() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let listed = pm_daemon::mcp::dispatch(
        &env.daemon,
        &token,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await
    .body
    .unwrap();
    for name in ["seed_connection", "propose_connection_update"] {
        let definition = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["name"] == name)
            .unwrap();
        for text in [
            definition["description"].as_str().unwrap(),
            definition["inputSchema"]["properties"]["schema_url"]["description"]
                .as_str()
                .unwrap(),
        ] {
            assert!(
                text.contains("publicly reachable without authentication"),
                "{name}: {text}"
            );
            assert!(text.contains("Puppet Master share"), "{name}: {text}");
        }
    }
}

#[tokio::test]
async fn an_inline_openapi_document_is_refused_by_every_tool_that_takes_one() {
    let env = daemon_env();
    let (token, _bearer, connection, _count, server) = orders_connection(&env).await;
    let live = tool(
        &env,
        &token,
        "get_connection",
        json!({"connection_id":connection}),
    )
    .await;
    let draft = tool(
        &env,
        &token,
        "seed_connection",
        json!({"name":"draft","kind":"openapi","endpoint":format!("http://{}",std::net::Ipv4Addr::LOCALHOST),"schema_path":schema_file(&env,"draft.json",&schema())}),
    )
    .await;
    let refusals = [
        tool(&env, &token, "seed_connection", json!({"name":"inline","kind":"openapi","endpoint":format!("http://{}",std::net::Ipv4Addr::LOCALHOST),"schema":schema()})).await,
        tool(&env, &token, "update_connection_draft", json!({"connection_id":draft["id"],"revision":draft["revision"],"config":{"name":"draft","project_id":draft["config"]["project_id"],"kind":"openapi","endpoint":draft["config"]["endpoint"],"schema":schema()}})).await,
        tool(&env, &token, "propose_connection_update", json!({"connection_id":connection,"revision":live["revision"],"config":{"schema":schema()},"explanation":"New document"})).await,
    ];
    for refused in refusals {
        assert_eq!(refused["isError"], true, "{refused}");
        let message = refused["content"][0]["text"].as_str().unwrap();
        assert!(
            message.contains("An inline schema is not accepted"),
            "{message}"
        );
        assert!(message.contains("schema_path"));
        assert!(message.contains("publicly reachable without authentication"));
    }
    server.abort();
}

#[tokio::test]
async fn a_schema_path_must_name_one_json_file_inside_the_working_directory() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), schema().to_string()).unwrap();
    std::fs::write(env.project_root().join("notes.txt"), "not json").unwrap();
    let seed = |extra: Value| {
        let mut args = json!({"name":"orders","kind":"openapi","endpoint":format!("http://{}",std::net::Ipv4Addr::LOCALHOST)});
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        tool(&env, &token, "seed_connection", args)
    };
    for (extra, expected) in [
        (
            json!({"schema_path":outside.path()}),
            "outside the session working directory",
        ),
        (
            json!({"schema_path":"../openapi.json"}),
            "traversal is not allowed",
        ),
        (json!({"schema_path":"missing.json"}), "schema_path:"),
        (
            json!({"schema_path":"notes.txt"}),
            "does not hold a JSON document",
        ),
        (
            json!({"schema_path":"notes.txt","schema_url":"https://example.invalid/openapi.json"}),
            "not both",
        ),
    ] {
        let refused = seed(extra).await;
        assert_eq!(refused["isError"], true, "{refused}");
        let message = refused["content"][0]["text"].as_str().unwrap();
        assert!(message.contains(expected), "{expected}: {message}");
    }
}

#[tokio::test]
async fn a_draft_keeps_its_document_until_a_file_replaces_it() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "stay-open");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let endpoint = format!("http://{}", std::net::Ipv4Addr::LOCALHOST);
    let draft = tool(
        &env,
        &token,
        "seed_connection",
        json!({"name":"orders","kind":"openapi","endpoint":endpoint,"schema_path":schema_file(&env,"openapi.json",&schema())}),
    )
    .await;
    assert_eq!(draft["tool_count"], 2, "{draft}");
    let config = |name: &str| json!({"name":name,"project_id":draft["config"]["project_id"],"kind":"openapi","endpoint":endpoint});

    let renamed = tool(
        &env,
        &token,
        "update_connection_draft",
        json!({"connection_id":draft["id"],"revision":draft["revision"],"config":config("orders renamed")}),
    )
    .await;
    assert_eq!(renamed["config"]["name"], "orders renamed", "{renamed}");
    assert_eq!(renamed["config"]["schema"], schema());

    let replaced = tool(
        &env,
        &token,
        "update_connection_draft",
        json!({"connection_id":draft["id"],"revision":renamed["revision"],"config":config("orders renamed"),"schema_path":schema_file(&env,"openapi-v2.json",&schema_with_lookup())}),
    )
    .await;
    assert_eq!(
        replaced["config"]["schema"],
        schema_with_lookup(),
        "{replaced}"
    );
}

#[tokio::test]
async fn a_policy_proposal_names_only_what_changes_and_comes_from_a_file() {
    let env = daemon_env();
    let (token, bearer, id, _count, server) = orders_connection(&env).await;
    let live = tool(&env, &token, "get_connection", json!({"connection_id":id})).await;
    assert_eq!(
        live["config"]["rules"],
        json!({"list_orders":{"access":"read","policy":null},"update_order":{"access":"write","policy":null}})
    );

    let inline = tool(&env, &token, "propose_connection_policy", json!({"connection_id":id,"revision":live["revision"],"policy":{"read_policy":"deny","rules":{}},"explanation":"Inline"})).await;
    assert_eq!(inline["isError"], true, "{inline}");
    let message = inline["content"][0]["text"].as_str().unwrap();
    assert!(
        message.contains("An inline policy is not accepted"),
        "{message}"
    );
    assert!(message.contains("policy_path"));

    let unchanged = tool(&env, &token, "propose_connection_policy", json!({"connection_id":id,"revision":live["revision"],"policy_path":schema_file(&env,"same.json",&json!({"rules":{"list_orders":{"access":"read"}}})),"explanation":"Same"})).await;
    assert_eq!(unchanged["isError"], true, "{unchanged}");
    assert!(unchanged["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("changes nothing"));

    let malformed = tool(&env, &token, "propose_connection_policy", json!({"connection_id":id,"revision":live["revision"],"policy_path":schema_file(&env,"odd.json",&json!({"rules":{},"tools":[]})),"explanation":"Odd"})).await;
    assert_eq!(malformed["isError"], true, "{malformed}");

    // One tool gains an override, one loses its rule, and the untouched tool keeps its classification.
    let proposed = tool(&env, &token, "propose_connection_policy", json!({"connection_id":id,"revision":live["revision"],"policy_path":schema_file(&env,"change.json",&json!({"unknown_policy":"deny","rules":{"update_order":{"access":"write","policy":"deny"},"list_orders":null}})),"explanation":"Lock writes down"})).await;
    assert_eq!(
        proposed["status"], "pending_user_confirmation",
        "{proposed}"
    );
    assert_eq!(
        proposed["proposal"]["policy"],
        json!({"read_policy":"allow","write_policy":"approve","unknown_policy":"deny","rules":{"update_order":{"access":"write","policy":"deny"}}})
    );
    let (status, applied) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/policy"),
        json!({"revision":live["revision"],"proposal_id":proposed["proposal"]["id"],"accept":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["config"]["unknown_policy"], "deny");
    assert_eq!(
        applied["config"]["rules"],
        json!({"update_order":{"access":"write","policy":"deny"}})
    );
    server.abort();
}

#[tokio::test]
async fn an_update_proposal_takes_its_policy_from_a_file_beside_the_setup_changes() {
    let env = daemon_env();
    let (token, bearer, id, _count, server) = orders_connection(&env).await;
    let live = tool(&env, &token, "get_connection", json!({"connection_id":id})).await;
    let proposed = tool(&env, &token, "propose_connection_update", json!({"connection_id":id,"revision":live["revision"],"config":{"name":"orders v2"},"schema_path":schema_file(&env,"openapi-v2.json",&schema_with_lookup()),"policy_path":schema_file(&env,"policy-v2.json",&json!({"write_policy":"deny","rules":{"get_order":{"access":"read"}}})),"explanation":"Adds the order lookup and locks writes"})).await;
    assert_eq!(
        proposed["status"], "pending_user_confirmation",
        "{proposed}"
    );
    assert_eq!(
        proposed["proposal"]["policy"],
        // update_order gains a parameter in the new document, so it is left for a fresh classification.
        json!({"read_policy":"allow","write_policy":"deny","unknown_policy":"approve","rules":{"get_order":{"access":"read","policy":null},"list_orders":{"access":"read","policy":null}}})
    );
    let (status, applied) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/policy"),
        json!({"revision":live["revision"],"proposal_id":proposed["proposal"]["id"],"accept":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["config"]["name"], "orders v2");
    assert_eq!(applied["config"]["write_policy"], "deny");
    assert_eq!(
        applied["config"]["rules"]["get_order"],
        json!({"access":"read","policy":null})
    );
    server.abort();
}

async fn self_registering_upstream(
    registrations: Arc<std::sync::Mutex<Vec<Value>>>,
    offers_registration: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    use std::collections::HashMap;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let origin = endpoint.clone();
    let resource = endpoint.clone();
    let issuer = endpoint.clone();
    let router = Router::new()
        .route(
            "/",
            get(move |headers: axum::http::HeaderMap| {
                let origin = origin.clone();
                async move {
                    if headers.get("authorization").and_then(|v| v.to_str().ok())
                        == Some("Bearer registered-access")
                    {
                        StatusCode::OK.into_response()
                    } else {
                        (
                            StatusCode::UNAUTHORIZED,
                            [(
                                "www-authenticate",
                                format!("Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource\", scope=\"orders:read\""),
                            )],
                        )
                            .into_response()
                    }
                }
            }),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(move || {
                let resource = resource.clone();
                async move {
                    Json(json!({"resource":resource,"authorization_servers":[resource],"scopes_supported":["orders:read","orders:write"]}))
                }
            }),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(move || {
                let issuer = issuer.clone();
                async move {
                    let mut metadata = json!({"issuer":issuer,"authorization_endpoint":format!("{issuer}/authorize"),"token_endpoint":format!("{issuer}/token"),"token_endpoint_auth_methods_supported":["none"],"code_challenge_methods_supported":["S256"]});
                    if offers_registration {
                        metadata["registration_endpoint"] = json!(format!("{issuer}/register"));
                    }
                    Json(metadata)
                }
            }),
        )
        .route(
            "/register",
            post(move |Json(body): Json<Value>| {
                let registrations = registrations.clone();
                async move {
                    registrations.lock().unwrap().push(body);
                    (
                        StatusCode::CREATED,
                        Json(json!({"client_id":"registered-client","client_id_issued_at":1,"token_endpoint_auth_method":"none"})),
                    )
                }
            }),
        )
        .route(
            "/token",
            post(
                |headers: axum::http::HeaderMap,
                 axum::Form(form): axum::Form<HashMap<String, String>>| async move {
                    assert!(headers.get("authorization").is_none());
                    assert_eq!(form["client_id"], "registered-client");
                    assert_eq!(form["grant_type"], "authorization_code");
                    Json(json!({"token_type":"Bearer","access_token":"registered-access","expires_in":3600}))
                },
            ),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (endpoint, server)
}

#[tokio::test]
async fn oauth_sign_in_registers_a_client_when_the_server_offers_it() {
    use std::collections::HashMap;
    let env = daemon_env();
    let bearer = signed_in_bearer(&env.daemon);
    let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (endpoint, server) = self_registering_upstream(registrations.clone(), true).await;
    let redirect = format!("{TEST_ORIGIN}/api/connections/oauth/callback");
    let (status,created)=api(&env,&bearer,"POST","/api/connections",json!({"name":"OAuth API","project_id":env.project_id,"kind":"openapi","endpoint":endpoint,"schema":schema(),"oauth":{"authorization_url":"","token_url":"","client_id":"","scopes":"","redirect_uri":redirect}})).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["id"].as_u64().unwrap();
    let (status, registered) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/oauth/register"),
        json!({"redirect_uri":redirect}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    let oauth = &registered["config"]["oauth"];
    assert_eq!(oauth["client_id"], "registered-client");
    assert_eq!(oauth["registered"], true);
    assert_eq!(oauth["token_auth_method"], "none");
    assert_eq!(oauth["authorization_url"], format!("{endpoint}/authorize"));
    assert_eq!(oauth["token_url"], format!("{endpoint}/token"));
    assert_eq!(
        oauth["registration_endpoint"],
        format!("{endpoint}/register")
    );
    assert_eq!(oauth["scopes"], "orders:read");
    assert_eq!(registered["credential_mode"], "oauth");
    assert_eq!(registered["active"], false);
    let request = registrations.lock().unwrap()[0].clone();
    assert_eq!(request["redirect_uris"], json!([redirect]));
    assert_eq!(request["token_endpoint_auth_method"], "none");
    assert_eq!(
        request["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(request["scope"], "orders:read");
    assert!(request["client_name"]
        .as_str()
        .is_some_and(|name| !name.is_empty()));

    let (status, start) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/oauth/start"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let url = reqwest::Url::parse(start["url"].as_str().unwrap()).unwrap();
    let query = url.query_pairs().into_owned().collect::<HashMap<_, _>>();
    assert_eq!(query["client_id"], "registered-client");
    assert_eq!(query["scope"], "orders:read");
    let callback = format!(
        "/api/connections/oauth/callback?state={}&code=placeholder-code",
        query["state"]
    );
    let (status, html) = api(&env, "", "GET", &callback, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        html.as_str().unwrap().contains("Sign-in complete"),
        "{html}"
    );
    let (status, tested) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/test"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tested}");
    assert_eq!(tested["tested_revision"], tested["revision"]);
    assert_eq!(registrations.lock().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn a_server_without_registration_still_discovers_endpoints_and_asks_for_a_client_id() {
    let env = daemon_env();
    let bearer = signed_in_bearer(&env.daemon);
    let registrations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (endpoint, server) = self_registering_upstream(registrations.clone(), false).await;
    let redirect = format!("{TEST_ORIGIN}/api/connections/oauth/callback");
    let (_,created)=api(&env,&bearer,"POST","/api/connections",json!({"name":"OAuth API","project_id":env.project_id,"kind":"openapi","endpoint":endpoint,"schema":schema()})).await;
    let id = created["id"].as_u64().unwrap();
    let (status, discovered) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/oauth/discover"),
        json!({"redirect_uri":redirect}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{discovered}");
    assert_eq!(discovered["token_url"], format!("{endpoint}/token"));
    assert!(discovered["registration_endpoint"].is_null());
    assert_eq!(discovered["scopes"], "orders:read");
    let (status, refused) = api(
        &env,
        &bearer,
        "POST",
        &format!("/api/connections/{id}/oauth/register"),
        json!({"redirect_uri":redirect}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("does not register clients"),
        "{refused}"
    );
    assert!(registrations.lock().unwrap().is_empty());
    let (_, unchanged) = api(
        &env,
        &bearer,
        "GET",
        &format!("/api/connections/{id}"),
        json!({}),
    )
    .await;
    assert_eq!(unchanged["revision"], created["revision"]);
    assert!(unchanged["config"]["oauth"].is_null());
    server.abort();
}

fn isolation_session(env: &TestEnv, project: u64, supervisor: bool) -> String {
    let id = env
        .daemon
        .spawn_session(
            project,
            pm_protocol::domain::AgentKind::Test,
            "isolation",
            "stay-open",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            supervisor,
            None,
        )
        .unwrap();
    env.daemon.session_token(id).unwrap().unwrap()
}

async fn isolation_move(env: &TestEnv, bearer: &str, connection: &Value, project: u64) -> Value {
    let mut config = connection["config"].clone();
    config["project_id"] = json!(project);
    let (status, moved) = api(
        env,
        bearer,
        "PUT",
        &format!("/api/connections/{}", connection["id"]),
        json!({"revision":connection["revision"],"config":config}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    moved
}

#[tokio::test]
async fn connection_isolation_covers_discovery_calls_history_and_setup_targets() {
    let env = daemon_env();
    let bearer = signed_in_bearer(&env.daemon);
    let bucket = env.daemon.get_project(env.project_id).unwrap().bucket_id;
    let sibling = env
        .daemon
        .create_project(bucket, "sibling", env.project_root().to_str().unwrap())
        .unwrap();
    let foreign_bucket = env.daemon.create_bucket("foreign").unwrap();
    let foreign = env
        .daemon
        .create_project(
            foreign_bucket,
            "foreign",
            env.project_root().to_str().unwrap(),
        )
        .unwrap();
    let worker = isolation_session(&env, env.project_id, false);
    let other_worker = isolation_session(&env, env.project_id, false);
    let supervisor = isolation_session(&env, env.project_id, true);
    let sibling_worker = isolation_session(&env, sibling, false);
    let foreign_supervisor = isolation_session(&env, foreign, true);
    let count = Arc::new(AtomicUsize::new(0));
    let (endpoint, server) = rest_upstream(count.clone()).await;
    let own = seed_and_finish(&env, &bearer, &worker, &endpoint, "openapi", Some(schema())).await;
    let sibling_connection = seed_and_finish(
        &env,
        &bearer,
        &sibling_worker,
        &endpoint,
        "openapi",
        Some(schema()),
    )
    .await;
    let foreign_connection = seed_and_finish(
        &env,
        &bearer,
        &foreign_supervisor,
        &endpoint,
        "openapi",
        Some(schema()),
    )
    .await;
    for (token, expected) in [
        (&worker, vec![own["id"].clone()]),
        (
            &supervisor,
            vec![own["id"].clone(), sibling_connection["id"].clone()],
        ),
        (&foreign_supervisor, vec![foreign_connection["id"].clone()]),
    ] {
        let list = tool(&env, token, "list_connections", json!({})).await;
        assert_eq!(
            list.as_array()
                .unwrap()
                .iter()
                .map(|c| c["id"].clone())
                .collect::<Vec<_>>(),
            expected
        );
    }
    for (token, blocked) in [
        (&worker, &sibling_connection),
        (&worker, &foreign_connection),
        (&supervisor, &foreign_connection),
        (&foreign_supervisor, &own),
    ] {
        for name in [
            "get_connection",
            "list_connection_tools",
            "describe_connection_tool",
            "update_connection_draft",
            "propose_connection_policy",
            "propose_connection_update",
            "call_connection_tool",
        ] {
            let result = tool(&env, token, name, json!({"connection_id":blocked["id"],"revision":blocked["revision"],"tool":"list_orders","arguments":{},"request_id":"blocked"})).await;
            assert_eq!(result["isError"], true, "{name}: {result}");
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("scope"),
                "{name}: {result}"
            );
        }
    }
    for token in [&worker, &supervisor] {
        for path in [
            "/api/connections".to_owned(),
            format!("/api/connections/{}", foreign_connection["id"]),
            "/api/connection-calls".to_owned(),
            "/api/connection-approvals".to_owned(),
        ] {
            let (status, _) =
                api(&env, &format!("Bearer {token}"), "GET", &path, Value::Null).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        }
    }
    let mut calls = Vec::new();
    for (token, connection) in [
        (&worker, &own),
        (&sibling_worker, &sibling_connection),
        (&foreign_supervisor, &foreign_connection),
    ] {
        let result = tool(&env, token, "call_connection_tool", json!({"connection_id":connection["id"],"tool":"list_orders","arguments":{},"request_id":"allowed"})).await;
        assert_eq!(result["status"], "succeeded", "{result}");
        calls.push(result);
    }
    let sibling_read = tool(&env, &supervisor, "call_connection_tool", json!({"connection_id":sibling_connection["id"],"tool":"list_orders","arguments":{},"request_id":"sibling-read"})).await;
    assert_eq!(sibling_read["status"], "succeeded");
    for (token, allowed, denied) in [
        (&worker, vec![&calls[0]], vec![&calls[1], &calls[2]]),
        (
            &supervisor,
            vec![&calls[0], &calls[1], &sibling_read],
            vec![&calls[2]],
        ),
        (
            &foreign_supervisor,
            vec![&calls[2]],
            vec![&calls[0], &calls[1], &sibling_read],
        ),
        (&other_worker, vec![], vec![&calls[0]]),
    ] {
        for call in &allowed {
            let result = tool(
                &env,
                token,
                "get_connection_call",
                json!({"call_id":call["id"]}),
            )
            .await;
            assert_eq!(result["status"], "succeeded");
        }
        for call in denied {
            assert_eq!(
                tool(
                    &env,
                    token,
                    "get_connection_call",
                    json!({"call_id":call["id"]})
                )
                .await["isError"],
                true
            );
        }
        let history = tool(&env, token, "list_connection_calls", json!({})).await;
        let actual: std::collections::BTreeSet<_> = history
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        let expected: std::collections::BTreeSet<_> =
            allowed.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(actual, expected);
        assert!(history
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["arguments"].is_null() && c["result"].is_null()));
    }
    let draft_args = json!({"name":"draft","project_id":sibling,"kind":"mcp","endpoint":endpoint});
    let draft = tool(&env, &supervisor, "seed_connection", draft_args.clone()).await;
    assert_eq!(draft["config"]["project_id"], sibling);
    let mut foreign_args = draft_args.clone();
    foreign_args["project_id"] = json!(foreign);
    assert_eq!(
        tool(&env, &supervisor, "seed_connection", foreign_args.clone()).await["isError"],
        true
    );
    let worker_draft = tool(&env, &worker, "seed_connection", foreign_args.clone()).await;
    assert_eq!(worker_draft["config"]["project_id"], env.project_id);
    assert_eq!(
        tool(
            &env,
            &supervisor,
            "update_connection_draft",
            json!({"connection_id":draft["id"],"revision":draft["revision"],"config":foreign_args})
        )
        .await["isError"],
        true
    );
    assert_eq!(tool(&env, &supervisor, "propose_connection_update", json!({"connection_id":own["id"],"revision":own["revision"],"config":{"project_id":foreign},"explanation":"Move connection"})).await["isError"], true);
    let kept = tool(&env, &worker, "update_connection_draft", json!({"connection_id":worker_draft["id"],"revision":worker_draft["revision"],"config":foreign_args})).await;
    assert_eq!(kept["config"]["project_id"], env.project_id);
    let kept = tool(&env, &worker, "propose_connection_update", json!({"connection_id":own["id"],"revision":own["revision"],"config":{"project_id":foreign,"name":"renamed"},"explanation":"Rename connection"})).await;
    assert_eq!(kept["proposal"]["setup"]["project_id"], env.project_id);
    let moved = isolation_move(&env, &bearer, &sibling_connection, foreign).await;
    assert_eq!(moved["config"]["project_id"], foreign);
    assert_eq!(
        tool(
            &env,
            &supervisor,
            "get_connection_call",
            json!({"call_id":calls[1]["id"]})
        )
        .await["isError"],
        true
    );
    assert_eq!(
        tool(
            &env,
            &foreign_supervisor,
            "get_connection_call",
            json!({"call_id":calls[1]["id"]})
        )
        .await["isError"],
        true
    );
    let history = tool(&env, &supervisor, "list_connection_calls", json!({})).await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn moving_a_connection_revokes_pending_calls_and_result_access() {
    for supervisor in [false, true] {
        let env = daemon_env();
        let bearer = signed_in_bearer(&env.daemon);
        let token = isolation_session(&env, env.project_id, supervisor);
        let bucket = if supervisor {
            env.daemon.create_bucket("foreign").unwrap()
        } else {
            env.daemon.get_project(env.project_id).unwrap().bucket_id
        };
        let target = env
            .daemon
            .create_project(bucket, "target", env.project_root().to_str().unwrap())
            .unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let (endpoint, server) = rest_upstream(count.clone()).await;
        let connection =
            seed_and_finish(&env, &bearer, &token, &endpoint, "openapi", Some(schema())).await;
        let pending = tool(&env, &token, "call_connection_tool", json!({"connection_id":connection["id"],"tool":"update_order","arguments":{"path":{"id":"first"},"body":{"name":"blocked"}},"request_id":"pending","justification":"Update order","wait_ms":SHORTEST_WAIT_MS})).await;
        assert_eq!(pending["status"], "pending");
        isolation_move(&env, &bearer, &connection, target).await;
        let (status, _) = api(
            &env,
            &bearer,
            "POST",
            &format!(
                "/api/connection-calls/{}/decision",
                pending["id"].as_str().unwrap()
            ),
            json!({"approve":true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, result) = api(
            &env,
            &bearer,
            "GET",
            &format!("/api/connection-calls/{}", pending["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
        assert_eq!(result["status"], "canceled");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(
            tool(
                &env,
                &token,
                "get_connection_call",
                json!({"call_id":pending["id"]})
            )
            .await["isError"],
            true
        );
        server.abort();
    }
}

#[tokio::test]
async fn live_demotion_revokes_sibling_connections_and_pending_call_results() {
    let env = daemon_env();
    let bearer = signed_in_bearer(&env.daemon);
    let supervisor = spawn_supervisor(&env);
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let bucket = env.daemon.get_project(env.project_id).unwrap().bucket_id;
    let sibling = env
        .daemon
        .create_project(bucket, "sibling", env.project_root().to_str().unwrap())
        .unwrap();
    let sibling_token = isolation_session(&env, sibling, false);
    let count = Arc::new(AtomicUsize::new(0));
    let (endpoint, server) = rest_upstream(count.clone()).await;
    let connection = seed_and_finish(
        &env,
        &bearer,
        &sibling_token,
        &endpoint,
        "openapi",
        Some(schema()),
    )
    .await;
    let pending = tool(&env, &token, "call_connection_tool", json!({"connection_id":connection["id"],"tool":"update_order","arguments":{"path":{"id":"first"},"body":{"name":"blocked"}},"request_id":"pending","justification":"Update order","wait_ms":SHORTEST_WAIT_MS})).await;
    assert_eq!(pending["status"], "pending");
    let wait = {
        let env_daemon = env.daemon.clone();
        let token = token.clone();
        let call_id = pending["id"].clone();
        tokio::spawn(async move {
            pm_daemon::mcp::dispatch(&env_daemon, &token, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_connection_call","arguments":{"call_id":call_id,"wait_ms":SHORTEST_WAIT_MS}}})).await.body.unwrap()["result"].clone()
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    env.daemon
        .update_session_apis(supervisor, None, Some(false))
        .unwrap();
    let result = tokio::time::timeout(TEST_TIMEOUT, wait)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(
        tool(
            &env,
            &token,
            "get_connection",
            json!({"connection_id":connection["id"]})
        )
        .await["isError"],
        true
    );
    assert!(tool(&env, &token, "list_connection_calls", json!({}))
        .await
        .as_array()
        .unwrap()
        .is_empty());
    let (status, _) = api(
        &env,
        &bearer,
        "POST",
        &format!(
            "/api/connection-calls/{}/decision",
            pending["id"].as_str().unwrap()
        ),
        json!({"approve":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let (_, result) = api(
                &env,
                &bearer,
                "GET",
                &format!("/api/connection-calls/{}", pending["id"].as_str().unwrap()),
                Value::Null,
            )
            .await;
            if result["status"] == "failed" {
                assert!(result["error"].as_str().unwrap().contains("scope"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn a_call_whose_upstream_is_gone_records_why_its_outcome_is_unknown() {
    let env = daemon_env();
    let (token, _bearer, connection, _count, server) = orders_connection(&env).await;
    server.abort();
    let _ = server.await;
    let read = tool(
        &env,
        &token,
        "call_connection_tool",
        json!({"connection_id":connection,"tool":"list_orders","arguments":{},"request_id":"read"}),
    )
    .await;
    let result = settled(&env, &token, &read["id"]).await;
    assert_eq!(result["status"], "outcome_unknown", "{result}");
    let error = result["error"].as_str().unwrap();
    assert!(
        error.starts_with("The downstream request failed: "),
        "{error}"
    );
    assert!(
        error.ends_with("Check the upstream before retrying this operation."),
        "{error}"
    );
}
