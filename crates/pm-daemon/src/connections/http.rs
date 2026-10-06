use anyhow::Result;
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{Config, Connection, Credential, MAX_DOCUMENT_BYTES};
use crate::{daemon::Daemon, http::authed_user};

pub(crate) fn router() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/api/connections", get(list).post(create))
        .route(
            "/api/connections/{id}",
            get(details).put(update).delete(remove),
        )
        .route("/api/connections/{id}/test", post(test))
        .route("/api/connections/import", post(import))
        .route("/api/connections/{id}/active", post(active))
        .route(
            "/api/connections/{id}/policy",
            get(policy).post(apply_policy),
        )
        .route("/api/connections/{id}/oauth/discover", post(oauth_discover))
        .route("/api/connections/{id}/oauth/register", post(oauth_register))
        .route("/api/connections/{id}/oauth/start", post(oauth_start))
        .route("/api/connections/oauth/callback", get(oauth_callback))
        .route("/api/connection-calls", get(calls))
        .route("/api/connection-calls/{id}", get(call_details))
        .route("/api/connection-calls/{id}/decision", post(decide))
        .route("/api/connection-approvals", get(approvals))
        .route("/api/connection-approvals/{id}", get(approval_details))
        .layer(DefaultBodyLimit::max(super::MAX_HTTP_BODY_BYTES))
}

fn response<T: serde::Serialize>(result: Result<T>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

/// A connection as the setup views read it, naming which kind of credential is stored and never its value.
fn connection_view(daemon: &Daemon, connection: Connection) -> Result<Value> {
    let mode = if connection.credential_set {
        Some(daemon.connection_credential(connection.id)?.mode())
    } else {
        None
    };
    let mut view = serde_json::to_value(connection)?;
    view["credential_mode"] = json!(mode);
    Ok(view)
}

fn connection_response(daemon: &Daemon, result: Result<Connection>) -> Response {
    response(result.and_then(|connection| connection_view(daemon, connection)))
}

async fn list(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    response(daemon.storage.connection_summaries())
}

async fn details(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    connection_response(&daemon, daemon.storage.connection(id))
}

async fn create(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(config): Json<Config>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let created = (|| {
        config.validate()?;
        daemon.storage.get_project(config.project_id)?;
        daemon.storage.create_connection(config, None)
    })();
    connection_response(&daemon, created)
}

#[derive(Deserialize)]
struct Update {
    revision: u64,
    config: Config,
    credential: Option<Credential>,
}

async fn update(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<Update>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let lock = daemon.connection_lock(id);
    let _guard = lock.lock().await;
    let updated = (|| {
        body.config.validate()?;
        daemon.storage.get_project(body.config.project_id)?;
        let previous = daemon.storage.connection(id)?;
        let credential = body
            .credential
            .map(|credential| -> Result<Option<String>> {
                if matches!(credential, Credential::None) {
                    return Ok(None);
                }
                super::transport::headers(&credential)?;
                Ok(Some(crate::secrets::seal_secret(
                    &daemon.installation_secret(),
                    &serde_json::to_string(&credential)?,
                )))
            })
            .transpose()?;
        let credential = if credential.is_none()
            && (previous.config.endpoint != body.config.endpoint
                || previous.config.project_id != body.config.project_id
                || previous.config.oauth != body.config.oauth)
        {
            Some(None)
        } else {
            credential
        };
        daemon
            .storage
            .update_connection(id, body.revision, &body.config, credential)?;
        daemon.storage.connection(id)
    })();
    connection_response(&daemon, updated)
}

#[derive(Deserialize)]
struct TestCall {
    tool: Option<String>,
    #[serde(default)]
    arguments: Value,
}

async fn test(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<TestCall>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result = daemon.test_connection(id).await;
    if let Ok(connection) = &result {
        if let Some(session) = connection.created_by_session {
            let _=daemon.send_connection_input(session,&format!("Connection {} tested successfully. Inspect get_connection and propose a tool policy for native user review. Server annotations and HTTP methods are classification suggestions only.",connection.id)).await;
        }
    }
    let result=async {
        let connection=result.map_err(|_|anyhow::anyhow!("Connection test failed. Check the endpoint and credentials. OAuth connections may need sign-in."))?;
        let mut value=connection_view(&daemon, connection.clone())?;
        if let Some(tool)=body.tool {
            let lock=daemon.connection_lock(id);
            let _guard=lock.lock().await;
            let current=daemon.storage.connection(id)?;
            if current.revision!=connection.revision { anyhow::bail!("Connection changed during testing"); }
            if current.config.rules.get(&tool).map(|rule|&rule.access)!=Some(&super::Access::Read) { anyhow::bail!("Choose a tool that you have classified as read-only"); }
            let definition = current.tools.iter().find(|candidate| candidate.name == tool).ok_or_else(||anyhow::anyhow!("Unknown test tool"))?;
            super::transport::validate_arguments(definition, &body.arguments)?;
            let credential=daemon.fresh_connection_credential(id).await?;
            match super::transport::execute(&current,&credential,&tool,&body.arguments).await? {
                super::transport::Execution::Complete(result)=>{value["test_result"]=result;},
                super::transport::Execution::Unknown(lost)=>anyhow::bail!("{lost}. The test call was not retried"),
            }
        }
        Ok(value)
    }.await;
    response(result)
}

#[derive(Deserialize)]
struct Active {
    revision: u64,
    active: bool,
}

async fn active(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<Active>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let lock = daemon.connection_lock(id);
    let _guard = lock.lock().await;
    let result = daemon
        .storage
        .activate_connection(id, body.revision, body.active)
        .and_then(|_| daemon.storage.connection(id));
    if let Ok(connection) = &result {
        if let Some(session) = connection.created_by_session {
            let _=daemon.send_connection_input(session,&format!("Connection {} is now {}. Use list_connection_tools to inspect its tools and policies.",connection.id,if connection.active {"active"} else {"disabled"})).await;
        }
    }
    connection_response(&daemon, result)
}

#[derive(Deserialize)]
struct Removal {
    revision: u64,
}

/// Removes a connection from use. Its row and call history stay, so a
/// call that named it still reads.
async fn remove(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<Removal>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let lock = daemon.connection_lock(id);
    let _guard = lock.lock().await;
    let result = daemon
        .storage
        .delete_connection(id, body.revision)
        .and_then(|_| daemon.storage.connection(id));
    if let Ok(connection) = &result {
        if let Some(session) = connection.created_by_session {
            let _ = daemon
                .send_connection_input(
                    session,
                    &format!(
                        "Connection {} was removed by the user. Do not seed it again unless asked.",
                        connection.id
                    ),
                )
                .await;
        }
    }
    connection_response(&daemon, result)
}

async fn policy(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    response(daemon.storage.connection_policy_proposal(id))
}

#[derive(Deserialize)]
struct ApplyPolicy {
    revision: u64,
    proposal_id: String,
    accept: bool,
}

async fn apply_policy(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<ApplyPolicy>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let lock = daemon.connection_lock(id);
    let _guard = lock.lock().await;
    let proposal = daemon.storage.connection_policy_proposal(id).ok().flatten();
    let session = proposal
        .as_ref()
        .and_then(|proposal| proposal["session_id"].as_u64());
    let is_update = proposal.is_some_and(|proposal| proposal["setup"].is_object());
    let result = daemon
        .storage
        .apply_connection_policy(id, body.revision, &body.proposal_id, body.accept)
        .and_then(|_| daemon.storage.connection(id));
    drop(_guard);
    if let Ok(connection) = &result {
        if let Some(session) = session {
            let outcome = if body.accept { "applied" } else { "dismissed" };
            let message = if !is_update {
                format!("Policy proposal for connection {id} was {outcome} in PM. Inspect get_connection for the current policy.")
            } else if body.accept && !connection.active {
                format!("Update proposal for connection {id} was applied in PM. The connection is inactive until the user tests and activates it.")
            } else {
                format!("Update proposal for connection {id} was {outcome} in PM. Inspect get_connection and list_connection_tools for the current setup and policy.")
            };
            let _ = daemon.send_connection_input(session, &message).await;
        }
    }
    connection_response(&daemon, result)
}

#[derive(Deserialize)]
struct CallFilter {
    session_id: Option<u64>,
}

async fn calls(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Query(filter): Query<CallFilter>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    response(
        daemon
            .storage
            .admin_connection_call_summaries(filter.session_id),
    )
}

async fn call_details(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    response(daemon.storage.connection_call(&id))
}

/// An endpoint as an approval shows it, without credentials, query or fragment.
fn display_endpoint(endpoint: &str) -> String {
    match reqwest::Url::parse(endpoint) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => String::new(),
    }
}

/// Adds the project, session and connection names a person needs to decide a call.
fn approval_context(daemon: &Daemon, summary: &mut Value) {
    let field = |key: &str| summary[key].as_u64();
    let (project, session, connection) = (
        field("project_id").and_then(|id| daemon.storage.get_project(id).ok()),
        field("session_id").and_then(|id| daemon.storage.get_session(id).ok()),
        field("connection_id").and_then(|id| daemon.storage.connection(id).ok()),
    );
    summary["project_name"] = json!(project.map(|project| project.name));
    summary["session_name"] = json!(session.as_ref().map(|session| session.display_name()));
    summary["session_state"] = json!(session.as_ref().map(|session| session.state.as_str()));
    summary["session_role"] = json!(session.as_ref().map(|session| session.role.as_str()));
    summary["session_live"] = json!(session
        .as_ref()
        .is_some_and(|session| session.state.is_live()));
    summary["connection_name"] = json!(connection.as_ref().map(|c| c.config.name.clone()));
    summary["connection_endpoint"] = json!(connection
        .as_ref()
        .map(|c| display_endpoint(&c.config.endpoint)));
    summary["connection_kind"] = json!(connection.as_ref().map(|c| c.config.kind.clone()));
    summary["connection_active"] = json!(connection.as_ref().is_some_and(|c| c.active));
    summary["connection_current"] = json!(connection
        .as_ref()
        .is_some_and(|c| { Some(c.revision) == summary["connection_revision"].as_u64() }));
    let tool = summary["tool"].as_str().unwrap_or_default().to_owned();
    if let Some(connection) = &connection {
        summary["tool_access"] = json!(connection
            .config
            .rules
            .get(&tool)
            .map(|rule| rule.access.clone())
            .unwrap_or_default());
        summary["tool_description"] = json!(connection
            .tools
            .iter()
            .find(|candidate| candidate.name == tool)
            .map(|candidate| candidate.description.clone()));
    }
}

async fn approvals(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    response(
        daemon
            .storage
            .connection_approval_summaries()
            .map(|mut summaries| {
                for summary in &mut summaries {
                    approval_context(&daemon, summary);
                }
                summaries
            }),
    )
}

async fn approval_details(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let call = match daemon.storage.connection_call(&id) {
        Ok(call) => call,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"This approval no longer exists"})),
            )
                .into_response()
        }
    };
    if !(call.requires_approval || call.status == "pending" || call.decided_by.is_some()) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"This call did not require approval"})),
        )
            .into_response();
    }
    response(
        serde_json::to_value(&call)
            .map_err(Into::into)
            .map(|mut value| {
                value["requires_approval"] = Value::Bool(true);
                approval_context(&daemon, &mut value);
                value
            }),
    )
}

#[derive(Deserialize)]
struct Decision {
    approve: bool,
}

async fn decide(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Decision>,
) -> Response {
    let Some(user) = authed_user(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let result = daemon.decide_connection_call(&id, body.approve, &user);
    if result.as_ref().is_ok_and(|call| call.status == "denied") {
        daemon.notify_connection_call(&id).await;
    }
    response(result)
}

async fn oauth_start(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    let Some(user) = authed_user(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let lock = daemon.connection_lock(id);
    let _guard = lock.lock().await;
    response(
        daemon
            .start_connection_oauth(id, &user)
            .map(|url| json!({"url":url})),
    )
}

#[derive(Deserialize)]
struct Discover {
    redirect_uri: String,
}

async fn oauth_discover(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<Discover>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    response(daemon.discover_connection_oauth(id,&body.redirect_uri).await.map_err(|_|anyhow::anyhow!("OAuth discovery failed. Enter authorization URL, token URL, and client ID manually.")))
}

async fn oauth_register(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<Discover>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let registered = daemon
        .register_connection_oauth(id, &body.redirect_uri)
        .await
        .and_then(|_| daemon.storage.connection(id));
    connection_response(&daemon, registered)
}

#[derive(Deserialize)]
struct Callback {
    state: String,
    code: Option<String>,
    error: Option<String>,
}

async fn oauth_callback(
    State(daemon): State<Arc<Daemon>>,
    Query(body): Query<Callback>,
) -> Response {
    let result = if body.error.is_some() {
        Err(anyhow::anyhow!(
            "Sign-in was declined. Return to the connection setup panel."
        ))
    } else if let Some(code) = body.code {
        daemon.complete_connection_oauth(&body.state, &code).await
    } else {
        Err(anyhow::anyhow!("Missing authorization code"))
    };
    let message = if result.is_ok() {
        "Sign-in complete. Return to PM to test and activate your connection."
    } else {
        "Sign-in failed or expired. Return to PM and start sign-in again."
    };
    Html(format!("<!doctype html><html><head><title>Connection sign-in</title></head><body><p>{message}</p></body></html>")).into_response()
}

pub(super) async fn import_schema(url: &str) -> Result<Value> {
    super::validate_endpoint(url)?;
    let mut response = super::transport::client()?.get(url).send().await?;
    if !response.status().is_success() {
        anyhow::bail!(
            "OpenAPI schema could not be fetched (HTTP {}). The URL must be publicly reachable without authentication. Save the document to a file in your working directory and pass schema_path instead",
            response.status().as_u16()
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_DOCUMENT_BYTES {
            anyhow::bail!("OpenAPI document exceeds the import limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Deserialize)]
struct Import {
    url: String,
}

async fn import(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(body): Json<Import>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    response(import_schema(&body.url).await.and_then(|schema| {
        super::openapi::import(&schema)?;
        Ok(schema)
    }))
}

#[cfg(test)]
mod tests {
    use super::display_endpoint;

    #[test]
    fn an_approval_shows_the_endpoint_without_credentials_or_query() {
        assert_eq!(
            display_endpoint("https://api.example.com/v1/mcp?api_key=secret#frag"),
            "https://api.example.com/v1/mcp"
        );
        assert_eq!(
            display_endpoint("https://user:pass@api.example.com/v1"),
            "https://api.example.com/v1"
        );
        assert_eq!(display_endpoint("not a url"), "");
    }
}
