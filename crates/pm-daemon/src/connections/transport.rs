use anyhow::{bail, Context, Result};
use base64::Engine;
use rmcp::{
    model::CallToolRequestParams,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::{json, Value};
use std::time::Duration;

use super::{
    validate_endpoint, Access, Config, Connection, Credential, Kind, Tool, MAX_RESULT_BYTES,
};

pub(super) enum Execution {
    Complete(Value),
    Unknown(Lost),
}

/// Why a call that reached the downstream service has no result.
pub(super) enum Lost {
    TimedOut(Duration),
    Failed(String),
}

impl std::fmt::Display for Lost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Lost::TimedOut(limit) => write!(
                f,
                "No downstream response arrived within {}s",
                limit.as_secs()
            ),
            Lost::Failed(error) => write!(f, "The downstream request failed: {error}"),
        }
    }
}

const NETWORK_TIMEOUT: Duration = Duration::from_secs(20);
/// Tool calls get longer than setup traffic because downstream tools such as
/// generated answers routinely take tens of seconds, and the agent polls the
/// call rather than holding a request open.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) fn headers(credential: &Credential) -> Result<reqwest::header::HeaderMap> {
    use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION};
    let mut headers = reqwest::header::HeaderMap::new();
    match credential {
        Credential::None => {}
        Credential::Bearer { token }
        | Credential::OAuth {
            access_token: token,
            ..
        } => {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }
        Credential::Basic { username, password } => {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{username}:{password}"))
                ))?,
            );
        }
        Credential::ApiKey { header, value } => {
            let name = HeaderName::from_bytes(header.as_bytes())?;
            if matches!(
                name.as_str(),
                "host"
                    | "content-length"
                    | "transfer-encoding"
                    | "connection"
                    | "proxy-authorization"
            ) {
                bail!("Invalid API key header");
            }
            headers.insert(name, HeaderValue::from_str(value)?);
        }
    }
    for value in headers.values_mut() {
        value.set_sensitive(true);
    }
    Ok(headers)
}

pub(super) fn client() -> Result<reqwest::Client> {
    client_within(NETWORK_TIMEOUT)
}

fn client_within(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()?)
}

async fn mcp(
    config: &Config,
    credential: &Credential,
) -> Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    let mut transport_config =
        StreamableHttpClientTransportConfig::with_uri(config.endpoint.clone());
    transport_config.reinit_on_expired_session = false;
    transport_config.max_sse_event_size = MAX_RESULT_BYTES;
    transport_config.custom_headers = headers(credential)?
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let client = reqwest_mcp::Client::builder()
        .redirect(reqwest_mcp::redirect::Policy::none())
        .build()?;
    Ok(tokio::time::timeout(
        NETWORK_TIMEOUT,
        ().serve(StreamableHttpClientTransport::with_client(
            client,
            transport_config,
        )),
    )
    .await
    .context("MCP initialization timed out")??)
}

async fn mcp_tools(
    service: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
) -> Result<Vec<rmcp::model::Tool>> {
    let mut tools = Vec::new();
    let mut cursor = None;
    let mut cursors = std::collections::HashSet::new();
    let mut bytes = 0usize;
    loop {
        let page = service
            .list_tools(Some(
                rmcp::model::PaginatedRequestParams::default().with_cursor(cursor),
            ))
            .await?;
        bytes = bytes.saturating_add(serde_json::to_vec(&page.tools)?.len());
        if tools.len().saturating_add(page.tools.len()) > super::MAX_CATALOG_TOOLS
            || bytes > super::MAX_CATALOG_BYTES
        {
            bail!("Downstream tool catalog exceeds the supported limit");
        }
        tools.extend(page.tools);
        let Some(next) = page.next_cursor else {
            return Ok(tools);
        };
        if !cursors.insert(next.clone()) {
            bail!("Downstream server repeated a tool pagination cursor");
        }
        cursor = Some(next);
    }
}

pub(super) async fn discover(config: &Config, credential: &Credential) -> Result<Vec<Tool>> {
    if config.kind == Kind::Openapi {
        let tools = super::openapi::import(config.schema.as_ref().context("Missing schema")?)?;
        let response = client()?
            .head(&config.endpoint)
            .headers(headers(credential)?)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            bail!("Credentials were refused by the API");
        }
        if response.status().is_server_error() {
            bail!("The API is unavailable");
        }
        if response.status().is_redirection() {
            bail!("API redirected. Configure the final API endpoint");
        }
        return Ok(tools);
    }
    let service = mcp(config, credential).await?;
    let result = tokio::time::timeout(NETWORK_TIMEOUT, mcp_tools(&service))
        .await
        .context("MCP discovery timed out")?;
    let _ = service.cancel().await;
    let mut names = std::collections::HashSet::new();
    let mut tools = result?
        .into_iter()
        .map(|tool| {
            if !names.insert(tool.name.to_string()) {
                bail!("Downstream server returned duplicate tool names");
            }
            let value = serde_json::to_value(&tool)?;
            Ok(Tool {
                name: tool.name.to_string(),
                description: tool.description.map(|s| s.to_string()).unwrap_or_default(),
                input_schema: value["inputSchema"].clone(),
                suggested_access: if value["annotations"]["readOnlyHint"].as_bool() == Some(true) {
                    Access::Read
                } else {
                    Access::Unknown
                },
                operation: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(tools)
}

pub(super) fn validate_arguments(tool: &Tool, args: &Value) -> Result<()> {
    if let Some(operation) = &tool.operation {
        if let Some(reason) = &operation.unsupported {
            bail!("{reason}");
        }
    }
    let validator = jsonschema::validator_for(&tool.input_schema)
        .context("Tool has an unsupported input schema")?;
    if let Err(error) = validator.validate(args) {
        bail!("Invalid tool arguments: {error}");
    }
    Ok(())
}

fn parameter_string(value: &Value) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(_) | Value::Bool(_) => Ok(value.to_string()),
        Value::Array(a) => Ok(a
            .iter()
            .map(parameter_string)
            .collect::<Result<Vec<_>>>()?
            .join(",")),
        _ => bail!("Unsupported parameter value"),
    }
}

pub(super) async fn execute(
    connection: &Connection,
    credential: &Credential,
    name: &str,
    args: &Value,
) -> Result<Execution> {
    execute_within(connection, credential, name, args, CALL_TIMEOUT).await
}

async fn execute_within(
    connection: &Connection,
    credential: &Credential,
    name: &str,
    args: &Value,
    timeout: Duration,
) -> Result<Execution> {
    let tool = connection
        .tools
        .iter()
        .find(|tool| tool.name == name)
        .context("Unknown tool")?;
    validate_arguments(tool, args)?;
    if connection.config.kind == Kind::Mcp {
        let service = mcp(&connection.config, credential).await?;
        let live = tokio::time::timeout(NETWORK_TIMEOUT, mcp_tools(&service)).await??;
        let current = live
            .iter()
            .find(|tool| tool.name == name)
            .context("Tool was removed from downstream server")?;
        let current_schema = serde_json::to_value(current)?["inputSchema"].clone();
        if current_schema != tool.input_schema {
            bail!("Downstream tool schema changed. Test the connection again");
        }
        let request = CallToolRequestParams::new(name.to_owned()).with_arguments(
            args.as_object()
                .context("Arguments must be an object")?
                .clone(),
        );
        let result = tokio::time::timeout(timeout, service.call_tool_once(request)).await;
        let _ = service.cancel().await;
        let value = match result {
            Ok(Ok(result)) => result,
            Ok(Err(rmcp::ServiceError::McpError(error))) => {
                let text = error.message.to_string();
                return Ok(Execution::Complete(redact(
                    json!({"isError":true,"error":error,"content":[{"type":"text","text":text}]}),
                    credential,
                )?));
            }
            Ok(Err(error)) => return Ok(failed(&error, credential)),
            Err(_) => return Ok(Execution::Unknown(Lost::TimedOut(timeout))),
        };
        let value = match value {
            rmcp::model::CallToolResponse::Complete(result) => serde_json::to_value(result)?,
            _ => {
                json!({"isError":true,"content":[{"type":"text","text":"This tool requires downstream task or interactive input support. It was not automatically retried."}]})
            }
        };
        if serde_json::to_vec(&value)?.len() > MAX_RESULT_BYTES {
            return Ok(Execution::Complete(
                json!({"isError":true,"content":[{"type":"text","text":"Downstream result exceeds the storage limit"}]}),
            ));
        }
        return Ok(Execution::Complete(redact(value, credential)?));
    }
    let operation = tool.operation.as_ref().context("Missing REST operation")?;
    let mut path = operation.path.clone();
    let mut query = Vec::<(String, String)>::new();
    let mut request_headers = headers(credential)?;
    for parameter in &operation.parameters {
        let group = match parameter.location.as_str() {
            "path" => "path",
            "query" => "query",
            _ => "headers",
        };
        let Some(value) = args[group].get(&parameter.name) else {
            continue;
        };
        match parameter.location.as_str() {
            "path" => {
                let part = parameter_string(value)?;
                if matches!(part.as_str(), "." | "..") {
                    bail!("Dot segments are not valid path arguments");
                }
                let mut encoder = validate_endpoint(&connection.config.endpoint)?;
                encoder.set_path("");
                encoder
                    .path_segments_mut()
                    .map_err(|_| anyhow::anyhow!("Cannot encode path"))?
                    .push(&part);
                path = path.replace(
                    &format!("{{{}}}", parameter.name),
                    encoder.path().trim_start_matches('/'),
                );
            }
            "query" if parameter.explode && value.is_array() => {
                for v in value.as_array().unwrap() {
                    query.push((parameter.name.clone(), parameter_string(v)?));
                }
            }
            "query" => query.push((parameter.name.clone(), parameter_string(value)?)),
            "header" => {
                let name = reqwest::header::HeaderName::from_bytes(parameter.name.as_bytes())?;
                if request_headers.contains_key(&name)
                    || matches!(
                        name.as_str(),
                        "authorization"
                            | "cookie"
                            | "host"
                            | "proxy-authorization"
                            | "content-length"
                            | "transfer-encoding"
                    )
                {
                    bail!("Tool arguments cannot override connection credentials or transport headers");
                }
                request_headers.insert(
                    name,
                    reqwest::header::HeaderValue::from_str(&parameter_string(value)?)?,
                );
            }
            _ => bail!("Unsupported parameter location"),
        }
    }
    if path.contains('{') {
        bail!("Missing REST path parameter");
    }
    let mut base = connection.config.endpoint.trim_end_matches('/').to_owned();
    base.push('/');
    let url = validate_endpoint(&base)?.join(path.trim_start_matches('/'))?;
    if url.origin() != validate_endpoint(&base)?.origin()
        || !url.path().starts_with(validate_endpoint(&base)?.path())
    {
        bail!("REST operation cannot change the connection origin");
    }
    let mut request = client_within(timeout)?
        .request(operation.method.parse()?, url)
        .headers(request_headers)
        .query(&query);
    if let Some(body) = args.get("body") {
        request = request.json(body);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return Ok(rest_lost(&error, timeout, credential)),
    };
    let status = response.status();
    let mut response = response;
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => return Ok(rest_lost(&error, timeout, credential)),
        };
        if bytes.len() + chunk.len() > MAX_RESULT_BYTES {
            return Ok(Execution::Complete(
                json!({"isError":true,"status":status.as_u16(),"content":[{"type":"text","text":"API result exceeds the storage limit"}]}),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Execution::Complete(redact(
        json!({"isError":!status.is_success(),"status":status.as_u16(),"content":[{"type":"text","text":String::from_utf8_lossy(&bytes)}]}),
        credential,
    )?))
}

fn failed(error: &dyn std::error::Error, credential: &Credential) -> Execution {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    let text = redact(Value::String(text), credential)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unreadable error".into());
    Execution::Unknown(Lost::Failed(text))
}

fn rest_lost(error: &reqwest::Error, timeout: Duration, credential: &Credential) -> Execution {
    if error.is_timeout() {
        Execution::Unknown(Lost::TimedOut(timeout))
    } else {
        failed(error, credential)
    }
}

fn redact(value: Value, credential: &Credential) -> Result<Value> {
    let mut secrets = Vec::<String>::new();
    match credential {
        Credential::Bearer { token } => secrets.push(token.clone()),
        Credential::ApiKey { value, .. } => secrets.push(value.clone()),
        Credential::Basic { username, password } => {
            secrets.push(password.clone());
            secrets.push(
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")),
            );
        }
        Credential::OAuth {
            access_token,
            refresh_token,
            client_secret,
            ..
        } => {
            secrets.push(access_token.clone());
            secrets.extend(refresh_token.clone());
            secrets.extend(client_secret.clone());
        }
        Credential::None => {}
    }
    secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
    secrets.retain(|secret| !secret.is_empty());
    fn walk(value: &mut Value, secrets: &[String]) {
        match value {
            Value::String(text) => {
                for secret in secrets {
                    *text = text.replace(secret, "[redacted]");
                }
            }
            Value::Object(object) => {
                let entries = std::mem::take(object);
                for (mut key, mut value) in entries {
                    for secret in secrets {
                        key = key.replace(secret, "[redacted]");
                    }
                    walk(&mut value, secrets);
                    while object.contains_key(&key) {
                        key.push('_');
                    }
                    object.insert(key, value);
                }
            }
            Value::Array(array) => {
                for value in array {
                    walk(value, secrets);
                }
            }
            _ => {}
        }
    }
    let mut value = value;
    walk(&mut value, &secrets);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse, routing::get, routing::post, Json};

    const SHORT_LIMIT: Duration = Duration::from_millis(300);
    const SLOW_REPLY: Duration = Duration::from_secs(5);
    const JSON_RPC_INVALID_PARAMS: i64 = -32602;

    async fn serve(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    fn connection(kind: &str, endpoint: String, operation: Option<Value>) -> Connection {
        serde_json::from_value(json!({
            "id": 1, "revision": 1, "active": true, "created_by_session": null,
            "credential_set": false, "tested_revision": 1, "tool_count": 1,
            "policy_proposal": null,
            "config": {"name": "test", "project_id": 1, "kind": kind, "endpoint": endpoint},
            "tools": [{"name": "ask", "description": "", "input_schema": {"type": "object"},
                       "suggested_access": "read", "operation": operation}],
        }))
        .unwrap()
    }

    /// An MCP server whose `ask` tool answers `call` after `delay`.
    async fn mcp_server(delay: Duration, call: Value) -> Connection {
        let router = axum::Router::new().route(
            "/mcp",
            post(move |Json(request): Json<Value>| {
                let call = call.clone();
                async move {
                    if request["id"].is_null() {
                        return StatusCode::ACCEPTED.into_response();
                    }
                    let reply = match request["method"].as_str().unwrap() {
                        "initialize" => json!({"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"test","version":"1"}}}),
                        "tools/list" => json!({"result":{"tools":[{"name":"ask","inputSchema":{"type":"object"}}]}}),
                        "tools/call" => {
                            tokio::time::sleep(delay).await;
                            call
                        }
                        _ => json!({"result":{}}),
                    };
                    let mut body = json!({"jsonrpc":"2.0","id":request["id"]});
                    body.as_object_mut()
                        .unwrap()
                        .extend(reply.as_object().unwrap().clone());
                    Json(body).into_response()
                }
            }),
        );
        connection("mcp", format!("{}/mcp", serve(router).await), None)
    }

    fn rest_connection(endpoint: String) -> Connection {
        connection(
            "openapi",
            endpoint,
            Some(json!({"method":"GET","path":"/ask","parameters":[],"unsupported":null})),
        )
    }

    async fn run(connection: &Connection) -> Execution {
        execute_within(
            connection,
            &Credential::None,
            "ask",
            &json!({}),
            SHORT_LIMIT,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_mcp_call_that_outlasts_its_limit_reports_a_timeout() {
        let connection = mcp_server(SLOW_REPLY, json!({"result":{"content":[]}})).await;
        match run(&connection).await {
            Execution::Unknown(Lost::TimedOut(limit)) => assert_eq!(limit, SHORT_LIMIT),
            Execution::Unknown(lost) => panic!("expected a timeout, got {lost}"),
            Execution::Complete(value) => panic!("expected a timeout, got {value}"),
        }
    }

    #[tokio::test]
    async fn an_mcp_error_reply_is_a_failed_result_rather_than_a_lost_one() {
        let connection = mcp_server(
            Duration::ZERO,
            json!({"error":{"code":JSON_RPC_INVALID_PARAMS,"message":"repoName is required"}}),
        )
        .await;
        let Execution::Complete(value) = run(&connection).await else {
            panic!("an error reply is a settled outcome");
        };
        assert_eq!(value["isError"], true);
        assert_eq!(value["content"][0]["text"], "repoName is required");
        assert_eq!(value["error"]["code"], JSON_RPC_INVALID_PARAMS);
    }

    #[tokio::test]
    async fn a_rest_call_that_outlasts_its_limit_reports_a_timeout() {
        let router = axum::Router::new().route(
            "/ask",
            get(|| async {
                tokio::time::sleep(SLOW_REPLY).await;
                "late"
            }),
        );
        let connection = rest_connection(serve(router).await);
        assert!(matches!(
            run(&connection).await,
            Execution::Unknown(Lost::TimedOut(SHORT_LIMIT))
        ));
    }

    #[tokio::test]
    async fn a_rest_call_whose_connection_drops_names_the_failure() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                drop(socket);
            }
        });
        let Execution::Unknown(Lost::Failed(reason)) = run(&rest_connection(endpoint)).await else {
            panic!("a dropped connection is a lost outcome with a reason");
        };
        assert!(reason.contains("error sending request"), "{reason}");
        assert!(
            Lost::Failed(reason.clone())
                .to_string()
                .starts_with("The downstream request failed: "),
            "{reason}"
        );
    }

    #[test]
    fn redaction_preserves_json_types_and_nested_content() {
        let credential = Credential::OAuth {
            access_token: "credential-token".into(),
            refresh_token: Some("refresh-token".into()),
            client_secret: Some("client-secret".into()),
            expires_at: None,
        };
        let result = redact(json!({"content":[{"type":"text","text":"credential-token refresh-token client-secret"}],"structuredContent":{"count":7,"ok":true,"value":null}}), &credential).unwrap();
        assert_eq!(
            result["content"][0]["text"],
            "[redacted] [redacted] [redacted]"
        );
        let keyed = redact(
            json!({"credential-token":"credential-token", "[redacted]":"kept"}),
            &credential,
        )
        .unwrap();
        assert!(!keyed.to_string().contains("credential-token"));
        assert_eq!(keyed.as_object().unwrap().len(), 2);
        assert_eq!(
            result["structuredContent"],
            json!({"count":7,"ok":true,"value":null})
        );
    }
}
