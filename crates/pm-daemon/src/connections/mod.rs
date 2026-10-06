mod http;
mod oauth;
mod openapi;
mod scope;
mod store;
mod transport;
mod update;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::daemon::Daemon;
use crate::storage::now_unix_ms;
use pm_protocol::domain::{Session, SessionRole};

pub(crate) use http::router;
pub(crate) use store::{migrate, SCHEMA};

pub const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CATALOG_TOOLS: usize = 2000;
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_HTTP_BODY_BYTES: usize = MAX_DOCUMENT_BYTES + 128 * 1024;
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;
const MAX_CONNECTION_NAME_BYTES: usize = 100;
const MAX_REQUEST_ID_BYTES: usize = 200;
const TOOL_PAGE_SIZE: u64 = 50;
const TOOL_PAGE_LIMIT: u64 = 200;
const APPROVAL_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// A held call returns inside the downstream request timeout, so a call that
/// needs no approval normally comes back settled.
const CALL_WAIT_DEFAULT_MS: u64 = 20_000;
/// The floor keeps an agent from polling in a tight loop.
const CALL_WAIT_MIN_MS: u64 = 5_000;
const CALL_WAIT_MAX_MS: u64 = 20_000;
const CALL_STALLED_AFTER_MS: i64 = 5 * 60 * 1000;
/// Cancellation and expiry settle a call without a wake, so a held call also
/// rechecks on this interval.
const CALL_RECHECK: Duration = Duration::from_secs(1);

const SCHEMA_URL_HELP: &str = "URL of an OpenAPI 3 JSON document. The controller fetches it with no credentials, so it must be publicly reachable without authentication. A Puppet Master share, forward or dashboard URL does not work.";
const SCHEMA_PATH_HELP: &str = "Path to an OpenAPI 3 JSON document inside your session working directory, absolute or relative to it. The worker running your session reads it. Traversal and symlinks are rejected. Maximum 4 MiB.";
const INLINE_SCHEMA_REFUSAL: &str = "An inline schema is not accepted. Save the OpenAPI document to a file in your working directory and pass schema_path, or pass a schema_url that is publicly reachable without authentication";

const POLICY_PATH_HELP: &str = "Path to a JSON policy file inside your session working directory, absolute or relative to it. It holds only what changes: any of read_policy, write_policy and unknown_policy (allow, approve or deny), and rules mapping a tool name to {access:read|write|unknown,policy?:allow|approve|deny}, or to null to clear that tool's rule. Tools it does not name keep their classification. Maximum 4 MiB.";
const INLINE_POLICY_REFUSAL: &str = "An inline policy is not accepted. Write the policy to a JSON file in your working directory, naming only the defaults and tools that change, and pass policy_path";

/// The policy changes an agent names by file, or none when it names no file.
async fn supplied_policy(
    daemon: &Daemon,
    session: &Session,
    args: &Value,
    inline: &[&Value],
) -> Result<Option<Value>> {
    if inline.iter().any(|value| !value.is_null()) {
        bail!(INLINE_POLICY_REFUSAL);
    }
    match args["policy_path"].as_str() {
        Some(path) => Ok(Some(
            daemon
                .session_json_file(session, path, "policy_path")
                .await?,
        )),
        None => Ok(None),
    }
}

/// The OpenAPI document an agent names by file or by URL, or none when it names neither.
async fn supplied_schema(
    daemon: &Daemon,
    session: &Session,
    args: &Value,
    inline: &Value,
) -> Result<Option<Value>> {
    if !inline.is_null() {
        bail!(INLINE_SCHEMA_REFUSAL);
    }
    match (args["schema_path"].as_str(), args["schema_url"].as_str()) {
        (Some(_), Some(_)) => bail!("Supply schema_path or schema_url, not both"),
        (Some(path), None) => Ok(Some(
            daemon
                .session_json_file(session, path, "schema_path")
                .await?,
        )),
        (None, Some(url)) => Ok(Some(http::import_schema(url).await?)),
        (None, None) => Ok(None),
    }
}

fn call_is_settled(status: &str) -> bool {
    !matches!(status, "pending" | "authorized" | "executing")
}

fn call_wait(args: &Value) -> Result<Duration> {
    let wait_ms = match args.get("wait_ms") {
        None | Some(Value::Null) => CALL_WAIT_DEFAULT_MS,
        Some(value) => value
            .as_u64()
            .context("wait_ms must be a positive integer")?,
    };
    Ok(Duration::from_millis(
        wait_ms.clamp(CALL_WAIT_MIN_MS, CALL_WAIT_MAX_MS),
    ))
}

fn unsettled_call_response(call: &Call, now: i64) -> Result<Value> {
    let elapsed_ms = now.saturating_sub(call.created_at);
    let stalled = elapsed_ms >= CALL_STALLED_AFTER_MS;
    let advice = if stalled {
        "No progress for over 5 minutes. A notice is delivered to this session when the call settles, so continue other work, or keep waiting with get_connection_call. Do not repeat the operation."
    } else {
        "The wait ended before the call settled. Wait again with get_connection_call. Do not repeat the operation."
    };
    let mut response = serde_json::to_value(call)?;
    response["waiting"] = json!({"elapsed_ms": elapsed_ms, "stalled": stalled, "advice": advice});
    Ok(response)
}

struct CallWaiter<'a> {
    daemon: &'a Daemon,
    call_id: String,
}

impl<'a> CallWaiter<'a> {
    fn register(daemon: &'a Daemon, call_id: &str) -> Self {
        *daemon
            .connection_call_waiters
            .lock()
            .unwrap()
            .entry(call_id.to_owned())
            .or_insert(0) += 1;
        Self {
            daemon,
            call_id: call_id.to_owned(),
        }
    }
}

impl Drop for CallWaiter<'_> {
    fn drop(&mut self) {
        let mut waiters = self.daemon.connection_call_waiters.lock().unwrap();
        if let Some(count) = waiters.get_mut(&self.call_id) {
            *count -= 1;
            if *count == 0 {
                waiters.remove(&self.call_id);
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    Allow,
    #[default]
    Approve,
    Deny,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Mcp,
    Openapi,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRule {
    #[serde(default)]
    pub access: Access,
    #[serde(default)]
    pub policy: Option<Policy>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDraft {
    pub read_policy: Policy,
    pub write_policy: Policy,
    pub unknown_policy: Policy,
    pub rules: BTreeMap<String, ToolRule>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TokenAuthMethod {
    #[default]
    None,
    ClientSecretPost,
    ClientSecretBasic,
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct OAuthConfig {
    #[serde(default)]
    pub token_auth_method: TokenAuthMethod,
    pub authorization_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scopes: String,
    pub redirect_uri: String,
    pub registration_endpoint: Option<String>,
    /// A registered client is replaced on the next sign-in rather than reused.
    pub registered: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    pub project_id: u64,
    #[serde(default)]
    pub kind: Kind,
    pub endpoint: String,
    #[serde(default)]
    pub schema: Option<Value>,
    #[serde(default)]
    pub read_policy: Policy,
    #[serde(default)]
    pub write_policy: Policy,
    #[serde(default)]
    pub unknown_policy: Policy,
    #[serde(default)]
    pub rules: BTreeMap<String, ToolRule>,
    #[serde(default)]
    pub oauth: Option<OAuthConfig>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Connection {
    pub id: u64,
    pub revision: u64,
    pub config: Config,
    pub active: bool,
    pub created_by_session: Option<u64>,
    pub credential_set: bool,
    pub tested_revision: Option<u64>,
    /// When the connection last passed a test; a record from before this was kept reads none.
    #[serde(default)]
    pub tested_at: Option<i64>,
    /// When the user removed it. A removed connection is read only as
    /// the history its calls point at.
    #[serde(default)]
    pub deleted_at: Option<i64>,
    pub tools: Vec<Tool>,
    pub tool_count: usize,
    pub policy_proposal: Option<Value>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub suggested_access: Access,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<openapi::Operation>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Credential {
    #[default]
    None,
    Bearer {
        token: String,
    },
    Basic {
        username: String,
        password: String,
    },
    ApiKey {
        header: String,
        value: String,
    },
    #[serde(rename = "oauth")]
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<i64>,
        client_secret: Option<String>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Call {
    pub id: String,
    pub request_id: String,
    pub session_id: u64,
    pub project_id: u64,
    pub connection_id: u64,
    pub connection_revision: u64,
    pub tool: String,
    pub arguments: Value,
    pub justification: String,
    pub status: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub decided_by: Option<String>,
    /// When a user approved or denied the call; older records read none.
    #[serde(default)]
    pub decided_at: Option<i64>,
    /// When execution settled; older records read none.
    #[serde(default)]
    pub finished_at: Option<i64>,
    pub result: Option<Value>,
    pub error: Option<String>,
    /// Whether policy held the call for a decision; older records read false and are recognized by status or decision.
    #[serde(default)]
    pub requires_approval: bool,
}

impl Credential {
    /// The kind of credential, as the setup views name it.
    pub(crate) fn mode(&self) -> &'static str {
        match self {
            Credential::None => "none",
            Credential::Bearer { .. } => "bearer",
            Credential::Basic { .. } => "basic",
            Credential::ApiKey { .. } => "api_key",
            Credential::OAuth { .. } => "oauth",
        }
    }
}

pub fn validate_endpoint(endpoint: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(endpoint).context("Invalid endpoint")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("Use an HTTP or HTTPS endpoint without embedded credentials or a fragment");
    }
    Ok(url)
}

impl Config {
    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() || self.name.len() > MAX_CONNECTION_NAME_BYTES {
            bail!("Connection name must contain 1–100 bytes");
        }
        validate_endpoint(&self.endpoint)?;
        if let Some(schema) = &self.schema {
            if serde_json::to_vec(schema)?.len() > MAX_DOCUMENT_BYTES {
                bail!("OpenAPI document exceeds the import limit");
            }
            openapi::import(schema)?;
        }
        if self.kind == Kind::Openapi && self.schema.is_none() {
            bail!("REST connections require an OpenAPI document");
        }
        if let Some(oauth) = &self.oauth {
            for endpoint in [
                &oauth.authorization_url,
                &oauth.token_url,
                &oauth.redirect_uri,
            ] {
                if !endpoint.is_empty() {
                    validate_endpoint(endpoint)?;
                }
            }
        }
        Ok(())
    }
}

impl Daemon {
    pub(super) fn connection_lock(&self, id: u64) -> Arc<tokio::sync::Mutex<()>> {
        self.connection_locks
            .lock()
            .unwrap()
            .entry(id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Reads a JSON document an agent names by `argument` from the session's working directory, on whichever worker runs it.
    async fn session_json_file(
        &self,
        session: &Session,
        path: &str,
        argument: &str,
    ) -> Result<Value> {
        let content = if session.worker_id == pm_protocol::domain::LOCAL_WORKER_ID {
            crate::attachments::scoped_local_file(&session.cwd, path)
                .map_err(|error| anyhow::anyhow!("{argument}: {error}"))?
                .0
        } else {
            let read = self
                .worker_file_read(
                    session.worker_id,
                    session.cwd.clone(),
                    path.to_owned(),
                    MAX_DOCUMENT_BYTES as u64,
                )
                .await
                .map_err(|error| anyhow::anyhow!("{argument}: {error}"))?;
            if !read.ok {
                bail!("{argument}: {}", read.error);
            }
            read.content
        };
        if content.len() > MAX_DOCUMENT_BYTES {
            bail!("{argument} names a file over the 4 MiB limit");
        }
        serde_json::from_slice(&content)
            .with_context(|| format!("{argument} does not hold a JSON document"))
    }

    pub(crate) fn connection_session(&self, token: &str) -> Result<Session> {
        Ok(self
            .storage
            .get_session(self.resolve_live_session(token)?)?)
    }

    fn connection_scope(&self, session: &Session) -> Result<scope::ConnectionScope> {
        self.storage.connection_scope(session)
    }

    pub(crate) fn connections_for_session(&self, session: &Session) -> Result<Vec<Connection>> {
        let scope = self.connection_scope(session)?;
        Ok(self
            .storage
            .connection_summaries()?
            .into_iter()
            .filter(|connection| scope.contains_project(connection.config.project_id))
            .collect())
    }

    pub(crate) fn seed_connection(
        &self,
        session: &Session,
        mut config: Config,
    ) -> Result<Connection> {
        if session.role != SessionRole::Supervisor {
            config.project_id = session.project_id;
        }
        config.read_policy = Policy::Allow;
        config.write_policy = Policy::Approve;
        config.unknown_policy = Policy::Approve;
        config.rules.clear();
        config.validate()?;
        self.connection_scope(session)?
            .require_project(config.project_id)?;
        self.storage.create_connection(config, Some(session.id))
    }

    pub(crate) fn connection_for_session(&self, session: &Session, id: u64) -> Result<Connection> {
        let c = self.storage.connection(id)?;
        self.connection_scope(session)?
            .require_project(c.config.project_id)?;
        if c.deleted_at.is_some() {
            bail!("Connection was removed");
        }
        Ok(c)
    }

    pub(crate) async fn test_connection(&self, id: u64) -> Result<Connection> {
        let lock = self.connection_lock(id);
        let _guard = lock.lock().await;
        let c = self.storage.connection(id)?;
        let credential = self.fresh_connection_credential(id).await?;
        let tools = transport::discover(&c.config, &credential).await?;
        self.storage.record_connection_test(id, c.revision, tools)?;
        self.storage.connection(id)
    }

    pub(super) fn connection_credential(&self, id: u64) -> Result<Credential> {
        match self.storage.connection_secret(id)? {
            Some(sealed) => {
                let plain = crate::secrets::open_secret(&self.installation_secret(), &sealed)
                    .context("Connection credential cannot be decrypted")?;
                Ok(serde_json::from_str(&plain)?)
            }
            None => Ok(Credential::None),
        }
    }

    fn effective_policy(c: &Connection, tool: &str) -> Policy {
        c.config
            .rules
            .get(tool)
            .and_then(|rule| rule.policy.clone())
            .unwrap_or_else(|| {
                match c
                    .config
                    .rules
                    .get(tool)
                    .map(|r| &r.access)
                    .unwrap_or(&Access::Unknown)
                {
                    Access::Read => c.config.read_policy.clone(),
                    Access::Write => c.config.write_policy.clone(),
                    Access::Unknown => c.config.unknown_policy.clone(),
                }
            })
    }

    pub(crate) async fn request_connection_call(
        self: &Arc<Self>,
        session: &Session,
        args: &Value,
    ) -> Result<Call> {
        let connection_id = args["connection_id"]
            .as_u64()
            .context("connection_id is required")?;
        let c = self.connection_for_session(session, connection_id)?;
        let tool = args["tool"].as_str().context("tool is required")?;
        let arguments = args.get("arguments").cloned().unwrap_or(json!({}));
        let request_id = args["request_id"]
            .as_str()
            .context("A stable request_id is required")?;
        if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID_BYTES {
            bail!("request_id must contain 1–200 bytes");
        }
        if !arguments.is_object() || serde_json::to_vec(&arguments)?.len() > MAX_DOCUMENT_BYTES {
            bail!("Invalid tool arguments");
        }
        if let Some(previous) = self
            .storage
            .connection_call_by_request(session.id, request_id)?
        {
            if previous.connection_id != connection_id
                || previous.tool != tool
                || previous.arguments != arguments
            {
                bail!("request_id already identifies a different call");
            }
            return Ok(previous);
        }
        if !c.active || c.tested_revision != Some(c.revision) {
            bail!("Connection is inactive or needs testing");
        }
        let definition = c
            .tools
            .iter()
            .find(|t| t.name == tool)
            .context("Unknown tool. Discover tools first")?;
        transport::validate_arguments(definition, &arguments)?;
        let policy = Self::effective_policy(&c, tool);
        let justification = args["justification"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_owned();
        if policy == Policy::Approve && justification.is_empty() {
            bail!("An approval requires a justification");
        }
        let now = now_unix_ms();
        let call = Call {
            id: store::random_id(),
            request_id: request_id.into(),
            session_id: session.id,
            project_id: session.project_id,
            connection_id,
            connection_revision: c.revision,
            tool: tool.into(),
            arguments,
            justification,
            status: match policy {
                Policy::Allow => "authorized",
                Policy::Approve => "pending",
                Policy::Deny => "denied",
            }
            .into(),
            created_at: now,
            expires_at: now + APPROVAL_TTL_MS,
            decided_by: None,
            decided_at: None,
            finished_at: None,
            result: None,
            error: None,
            requires_approval: policy == Policy::Approve,
        };
        let (call, inserted) = self.storage.create_live_connection_call(call)?;
        if inserted && policy == Policy::Approve {
            self.queue_connection_approval_push(&call);
        }
        if inserted && policy == Policy::Allow {
            self.spawn_connection_execution(call.id.clone());
        }
        Ok(call)
    }

    fn authorize_connection_execution(&self, call: &Call) -> Result<Connection> {
        let connection = self.storage.connection(call.connection_id)?;
        let session = self.storage.get_session(call.session_id)?;
        self.session_connection_call(&session, &call.id)?;
        if !connection.active
            || connection.revision != call.connection_revision
            || !session.state.is_live()
            || call.expires_at <= now_unix_ms()
            || Self::effective_policy(&connection, &call.tool) == Policy::Deny
        {
            bail!("Connection, session, or authorization changed. Submit a new call");
        }
        Ok(connection)
    }

    fn spawn_connection_execution(self: &Arc<Self>, id: String) {
        let daemon = self.clone();
        tokio::spawn(async move {
            if let Err(error) = daemon.execute_connection_call(&id).await {
                tracing::warn!(call = %id, %error, "connection call could not be settled");
            }
        });
    }

    async fn execute_connection_call(&self, id: &str) -> Result<()> {
        let connection_id = self.storage.connection_call(id)?.connection_id;
        let lock = self.connection_lock(connection_id);
        let _guard = lock.lock().await;
        let Some(call) = self.storage.claim_connection_call(id)? else {
            return Ok(());
        };
        let c = match self.authorize_connection_execution(&call) {
            Ok(value) => value,
            Err(error) => {
                self.storage
                    .finish_connection_call(id, "failed", None, Some(error.to_string()))?;
                drop(_guard);
                self.notify_connection_call(id).await;
                return Ok(());
            }
        };
        let credential = match self.fresh_connection_credential(call.connection_id).await {
            Ok(credential) => credential,
            Err(_) => {
                self.storage.finish_connection_call(
                    id,
                    "failed",
                    None,
                    Some("Credentials need attention. Reconnect in Manage → Connections.".into()),
                )?;
                drop(_guard);
                self.notify_connection_call(id).await;
                return Ok(());
            }
        };
        if let Err(error) = self.authorize_connection_execution(&call) {
            self.storage
                .finish_connection_call(id, "failed", None, Some(error.to_string()))?;
            drop(_guard);
            self.notify_connection_call(id).await;
            return Ok(());
        }
        let started = std::time::Instant::now();
        match transport::execute(&c, &credential, &call.tool, &call.arguments).await {
            Ok(transport::Execution::Complete(result)) => {
                let failed = result["isError"].as_bool().unwrap_or(false);
                self.storage.finish_connection_call(
                    id,
                    if failed { "failed" } else { "succeeded" },
                    Some(result),
                    None,
                )?;
            }
            Ok(transport::Execution::Unknown(lost)) => {
                tracing::warn!(
                    call = %id,
                    connection = call.connection_id,
                    tool = %call.tool,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    reason = %lost,
                    "connection call outcome is unknown"
                );
                self.storage.finish_connection_call(
                    id,
                    "outcome_unknown",
                    None,
                    Some(format!(
                        "{lost}. Check the upstream before retrying this operation."
                    )),
                )?;
            }
            Err(error) => {
                tracing::warn!(
                    call = %id,
                    connection = call.connection_id,
                    tool = %call.tool,
                    error = %format!("{error:#}"),
                    "connection call was refused before invoking the tool"
                );
                self.storage.finish_connection_call(
                    id,
                    "failed",
                    None,
                    Some(format!(
                        "The call was refused before invoking the tool: {error}"
                    )),
                )?;
            }
        }
        drop(_guard);
        self.notify_connection_call(id).await;
        Ok(())
    }

    /// Holds until the call settles or the wait runs out. An unsettled call
    /// comes back with advice on how to keep waiting.
    pub(crate) async fn await_connection_call(
        &self,
        session: &Session,
        id: &str,
        wait: Duration,
    ) -> Result<Value> {
        let owned = self.session_connection_call(session, id)?.session_id == session.id;
        let waiter = owned.then(|| CallWaiter::register(self, id));
        let mut settled = self.connection_call_settled.subscribe();
        let deadline = Instant::now() + wait;
        loop {
            if call_is_settled(&self.session_connection_call(session, id)?.status) {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let _ = tokio::time::timeout(remaining.min(CALL_RECHECK), settled.changed()).await;
        }
        // A settle that saw this waiter skipped its notice, so the result has
        // to be read after the waiter is gone.
        drop(waiter);
        let call = self.session_connection_call(session, id)?;
        if call_is_settled(&call.status) {
            Ok(serde_json::to_value(call)?)
        } else {
            unsettled_call_response(&call, now_unix_ms())
        }
    }

    pub(crate) async fn notify_connection_call(&self, id: &str) {
        if let Ok(call) = self.storage.connection_call(id) {
            self.connection_call_settled
                .send_modify(|settles| *settles = settles.wrapping_add(1));
            if self
                .connection_call_waiters
                .lock()
                .unwrap()
                .contains_key(&call.id)
            {
                return;
            }
            let _ = self.send_connection_input(call.session_id, &format!("Connection call {} settled as {}. Use get_connection_call to retrieve its result. Do not repeat the operation.", call.id, call.status)).await;
        }
    }

    pub(crate) fn decide_connection_call(
        self: &Arc<Self>,
        id: &str,
        approve: bool,
        username: &str,
    ) -> Result<Call> {
        self.storage.decide_connection_call(id, approve, username)?;
        let call = self.storage.connection_call(id)?;
        if call.status == "authorized" {
            self.spawn_connection_execution(id.into());
        }
        Ok(call)
    }

    pub(crate) fn resume_connection_calls(self: &Arc<Self>) -> Result<()> {
        for id in self.storage.recover_connection_calls()? {
            self.spawn_connection_execution(id);
        }
        Ok(())
    }

    pub(crate) fn session_connection_call(&self, session: &Session, id: &str) -> Result<Call> {
        let call = self.storage.connection_call(id)?;
        let scope = self.connection_scope(session)?;
        let owner = self.storage.get_session(call.session_id)?;
        let connection = self.storage.connection(call.connection_id)?;
        scope.require_call(
            call.session_id,
            call.project_id,
            owner.project_id,
            connection.config.project_id,
        )?;
        Ok(call)
    }
}

pub(crate) fn tool_definitions() -> Vec<Value> {
    let connection_id = json!({"type":"integer","minimum":1});
    let wait_ms = json!({"type":"integer","minimum":CALL_WAIT_MIN_MS,"maximum":CALL_WAIT_MAX_MS,"default":CALL_WAIT_DEFAULT_MS});
    let make = |name: &str, description: &str, properties: Value, required: Vec<&str>| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}});
    vec![
        make("list_connections","List accessible MCP and REST connections. Workers see their project. Supervisors see projects in their bucket.",json!({}),vec![]),
        make("get_connection","Read one accessible connection's setup metadata and policy, without credentials.",json!({"connection_id":connection_id,"include_schema":{"type":"boolean"}}),vec!["connection_id"]),
        make("list_connection_tools","Filter and page compact cached tool descriptions and effective policies. Use describe_connection_tool for a single input schema.",json!({"connection_id":connection_id,"query":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200}}),vec!["connection_id"]),
        make("describe_connection_tool","Describe one connection tool, input schema, and effective policy.",json!({"connection_id":connection_id,"tool":{"type":"string"}}),vec!["connection_id","tool"]),
        make("seed_connection","Prepare an inactive MCP or OpenAPI connection for the user to finish in the native session setup panel. Supply metadata and schemas only. Never supply credentials. REST requires its OpenAPI document as schema_path or schema_url, never inline. Save the document to a file in your working directory and pass schema_path. Use schema_url only for a document that is publicly reachable without authentication: the controller fetches it with no credentials, so a Puppet Master share, forward or dashboard URL, which requires a login, cannot be used. The user tests and activates the connection.",json!({"name":{"type":"string"},"project_id":{"type":"integer"},"kind":{"type":"string","enum":["mcp","openapi"]},"endpoint":{"type":"string"},"schema_path":{"type":"string","description":SCHEMA_PATH_HELP},"schema_url":{"type":"string","description":SCHEMA_URL_HELP},"oauth":{"type":"object"}}),vec!["name","kind","endpoint"]),
        make("update_connection_draft","Revise an inactive connection draft without credentials. Supply the current revision and full setup metadata in config, without a schema: the draft keeps its OpenAPI document unless schema_path or schema_url replaces it. Cannot modify an active connection or apply policy. User edits win on revision conflicts.",json!({"connection_id":connection_id,"revision":{"type":"integer"},"config":{"type":"object"},"schema_path":{"type":"string","description":SCHEMA_PATH_HELP},"schema_url":{"type":"string","description":SCHEMA_URL_HELP}}),vec!["connection_id","revision","config"]),
        make("propose_connection_policy","Propose policy changes for native user review. Never applies the proposal. Classify tools deliberately. Downstream readOnlyHint and HTTP methods are suggestions, not authority. The policy is never passed inline: write a JSON file in your working directory and pass policy_path. The file names only what changes, so a tool it leaves out keeps its classification and you do not resend the whole catalog.",json!({"connection_id":connection_id,"revision":{"type":"integer"},"policy_path":{"type":"string","description":POLICY_PATH_HELP},"explanation":{"type":"string"}}),vec!["connection_id","revision","policy_path","explanation"]),
        make("propose_connection_update","Propose an update to a connection, active or not, for native user review. Never applies it and never carries credentials. Supply the current revision, an explanation, and a config holding only the fields to change: name, endpoint, oauth. A new OpenAPI document goes beside config as schema_path, a file in your working directory, or as schema_url, never inline. Policy defaults and tool classifications go beside config as policy_path, a JSON file naming only what changes, never inline. Omitted fields keep their current value, and a tool the policy file leaves out keeps its classification when its definition is unchanged. A schema_url is fetched by the controller with no credentials, so it must be publicly reachable without authentication, and a Puppet Master share, forward or dashboard URL cannot be used: save the document to a file and pass schema_path instead. The user sees the changed fields, the tools added, removed and changed, and the policy changes, and applies them together. The connection stays active unless the endpoint, kind, project or OAuth settings change, which deactivates it until the user tests and activates it.",json!({"connection_id":connection_id,"revision":{"type":"integer"},"config":{"type":"object"},"schema_path":{"type":"string","description":SCHEMA_PATH_HELP},"schema_url":{"type":"string","description":SCHEMA_URL_HELP},"policy_path":{"type":"string","description":POLICY_PATH_HELP},"explanation":{"type":"string"}}),vec!["connection_id","revision","explanation"]),
        make("call_connection_tool","Request a downstream tool call and wait up to wait_ms for it to settle, including time spent awaiting approval. A settled call returns its result. An unsettled call returns its durable call ID with a waiting field: wait again with get_connection_call rather than repeating the operation. Reuse request_id to recover a request without duplicate execution.",json!({"connection_id":connection_id,"tool":{"type":"string"},"arguments":{"type":"object"},"request_id":{"type":"string"},"justification":{"type":"string"},"wait_ms":wait_ms.clone()}),vec!["connection_id","tool","arguments","request_id"]),
        make("get_connection_call","Wait up to wait_ms for a call to settle, then return its approval/execution state and result. A call still unsettled carries a waiting field; once it reports stalled, a notice arrives in this session when the call settles. outcome_unknown means check upstream before retrying.",json!({"call_id":{"type":"string"},"wait_ms":wait_ms}),vec!["call_id"]),
        make("list_connection_calls","Recover this session's recent calls. Supervisors can inspect calls within their bucket.",json!({}),vec![]),
    ]
}

pub(crate) async fn dispatch(
    daemon: &Arc<Daemon>,
    token: &str,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let session = daemon.connection_session(token)?;
    if let Some(id) = args.get("connection_id") {
        daemon.connection_for_session(
            &session,
            id.as_u64().context("connection_id must be an integer")?,
        )?;
    }
    let id = || {
        args["connection_id"]
            .as_u64()
            .context("connection_id is required")
    };
    match name {
        "list_connections"=>Ok(json!(daemon.connections_for_session(&session)?.into_iter().map(|c|json!({"id":c.id,"revision":c.revision,"name":c.config.name,"kind":c.config.kind,"project_id":c.config.project_id,"endpoint":c.config.endpoint,"active":c.active,"credential_set":c.credential_set,"tool_count":c.tool_count})).collect::<Vec<_>>())),
        "get_connection"=>{
            let mut connection=daemon.connection_for_session(&session,id()?)?;
            connection.tools.clear();
            if args["include_schema"]!=true {
                connection.config.schema=None;
                if let Some(setup)=connection.policy_proposal.as_mut().and_then(|proposal|proposal["setup"].as_object_mut()) { setup.remove("schema"); }
            }
            Ok(serde_json::to_value(connection)?)
        },
        "list_connection_tools" | "describe_connection_tool"=>{
            let connection=daemon.connection_for_session(&session,id()?)?;
            let query=args["query"].as_str().unwrap_or("").to_lowercase();
            let tools:Vec<_>=connection.tools.iter().filter(|tool| {
                if name=="describe_connection_tool" { tool.name==args["tool"].as_str().unwrap_or("") }
                else { format!("{} {}",tool.name,tool.description).to_lowercase().contains(&query) }
            }).collect::<Vec<_>>();
            if name=="describe_connection_tool" {
                let tool=tools.first().context("Unknown tool")?;
                return Ok(json!({"revision":connection.revision,"tool":tool,"policy":Daemon::effective_policy(&connection,&tool.name),"classification":connection.config.rules.get(&tool.name).map(|r|&r.access).unwrap_or(&Access::Unknown)}));
            }
            let total=tools.len();
            let offset=args["offset"].as_u64().unwrap_or(0) as usize;
            let limit=args["limit"].as_u64().unwrap_or(TOOL_PAGE_SIZE).clamp(1,TOOL_PAGE_LIMIT) as usize;
            let summaries=tools.into_iter().skip(offset).take(limit).map(|tool|json!({"name":tool.name,"description":tool.description,"suggested_access":tool.suggested_access,"policy":Daemon::effective_policy(&connection,&tool.name),"classification":connection.config.rules.get(&tool.name).map(|r|&r.access).unwrap_or(&Access::Unknown)})).collect::<Vec<_>>();
            Ok(json!({"revision":connection.revision,"total":total,"offset":offset,"tools":summaries}))
        },
        "seed_connection"=>{
            let project = if session.role == SessionRole::Supervisor {
                args["project_id"].as_u64().unwrap_or(session.project_id)
            } else { session.project_id };
            daemon.connection_scope(&session)?.require_project(project)?;
            let mut input=args.clone();
            let schema=supplied_schema(daemon,&session,args,&args["schema"]).await?;
            let fields=input.as_object_mut().context("Expected setup metadata")?;
            fields.remove("schema_url");
            fields.remove("schema_path");
            if let Some(schema)=schema { fields.insert("schema".into(),schema); }
            if input["project_id"].is_null() { input["project_id"]=json!(session.project_id); }
            let config:Config=serde_json::from_value(input)?;
            let mut connection=daemon.seed_connection(&session,config)?;
            connection.config.schema=None;
            connection.tools.clear();
            Ok(serde_json::to_value(connection)?)
        },
        "update_connection_draft"=>{
            let id=id()?;
            let lock=daemon.connection_lock(id);
            let _guard=lock.lock().await;
            let connection=daemon.connection_for_session(&session,id)?;
            if connection.active { bail!("Active connection edits require user review. Propose policy changes separately"); }
            let mut supplied=args["config"].clone();
            let schema=supplied_schema(daemon,&session,args,&supplied["schema"]).await?;
            supplied["schema"]=match schema {
                Some(schema)=>schema,
                None if supplied["kind"]=="openapi"=>json!(connection.config.schema),
                None=>Value::Null,
            };
            let mut config:Config=serde_json::from_value(supplied)?;
            if session.role!=SessionRole::Supervisor { config.project_id=session.project_id; }
            config.read_policy=connection.config.read_policy.clone(); config.write_policy=connection.config.write_policy.clone(); config.unknown_policy=connection.config.unknown_policy.clone(); config.rules=connection.config.rules.clone();
            config.validate()?;
            daemon.connection_scope(&session)?.require_project(config.project_id)?;
            let revision=args["revision"].as_u64().context("revision is required")?;
            let credential=if config.endpoint!=connection.config.endpoint || config.project_id!=connection.config.project_id || config.oauth!=connection.config.oauth {Some(None)} else {None};
            daemon.storage.update_connection(id,revision,&config,credential)?;
            Ok(serde_json::to_value(daemon.storage.connection(id)?)?)
        },
        "propose_connection_policy"=>{
            let connection=daemon.connection_for_session(&session,id()?)?;
            let revision=args["revision"].as_u64().context("revision is required")?;
            let patch=supplied_policy(daemon,&session,args,&[&args["policy"]]).await?.context("policy_path is required")?;
            let current=update::policy_of(&connection.config);
            let policy=update::patched_policy(&current,&patch)?;
            for tool in policy.rules.keys() {
                if !connection.tools.iter().any(|t|&t.name==tool) { anyhow::bail!("Unknown tool in policy: {tool}"); }
            }
            if serde_json::to_value(&policy)?==serde_json::to_value(&current)? { bail!("The policy changes nothing"); }
            let explanation=args["explanation"].as_str().context("Explain the proposed policy")?;
            let proposal=json!({"id":store::random_id(),"revision":revision,"session_id":session.id,"policy":policy,"explanation":explanation});
            daemon.storage.propose_connection_policy(connection.id,revision,&proposal)?;
            Ok(json!({"status":"pending_user_confirmation","connection_id":connection.id,"proposal":proposal}))
        },
        "propose_connection_update"=>{
            let connection=daemon.connection_for_session(&session,id()?)?;
            let revision=args["revision"].as_u64().context("revision is required")?;
            let explanation=args["explanation"].as_str().context("Explain the proposed update")?;
            let mut supplied=args["config"].clone();
            if let Some(schema)=supplied_schema(daemon,&session,args,&supplied["schema"]).await? {
                if !supplied.is_object() { supplied=json!({}); }
                supplied["schema"]=schema;
            }
            let project=(session.role!=SessionRole::Supervisor).then_some(session.project_id);
            let policy=supplied_policy(daemon,&session,args,&[&supplied["rules"],&supplied["read_policy"],&supplied["write_policy"],&supplied["unknown_policy"]]).await?;
            let candidate=update::candidate(&connection,&supplied,project,policy.as_ref())?;
            daemon.connection_scope(&session)?.require_project(candidate.config.project_id)?;
            let changes=update::changes(&connection,&candidate)?;
            let mut proposal=json!({"id":store::random_id(),"revision":revision,"session_id":session.id,"policy":update::policy_of(&candidate.config),"explanation":explanation,"setup":update::Setup::of(&candidate.config),"changes":changes});
            daemon.storage.propose_connection_policy(connection.id,revision,&proposal)?;
            if let Some(setup)=proposal["setup"].as_object_mut() { setup.remove("schema"); }
            Ok(json!({"status":"pending_user_confirmation","connection_id":connection.id,"proposal":proposal}))
        },
        "call_connection_tool" => {
            let wait = call_wait(args)?;
            let call = daemon.request_connection_call(&session, args).await?;
            daemon.await_connection_call(&session, &call.id, wait).await
        }
        "get_connection_call" => {
            let wait = call_wait(args)?;
            let call_id = args["call_id"].as_str().context("call_id is required")?;
            daemon.await_connection_call(&session, call_id, wait).await
        }
        "list_connection_calls"=>Ok(serde_json::to_value(daemon.storage.connection_call_summaries_in_scope(&daemon.connection_scope(&session)?)?)?),
        _=>bail!("Unknown connection tool"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTHORIZATION_TEST_TIMEOUT: Duration = Duration::from_secs(5);
    const OAUTH_TEST_LIFETIME_SECS: u64 = 3600;

    fn pending_call(created_at: i64) -> Call {
        Call {
            id: "call".into(),
            request_id: "request".into(),
            session_id: 1,
            project_id: 1,
            connection_id: 1,
            connection_revision: 1,
            tool: "update_order".into(),
            arguments: json!({}),
            justification: "Rename order".into(),
            status: "pending".into(),
            created_at,
            expires_at: created_at + APPROVAL_TTL_MS,
            decided_by: None,
            decided_at: None,
            finished_at: None,
            result: None,
            error: None,
            requires_approval: true,
        }
    }

    fn authorization_daemon() -> (Daemon, tempfile::TempDir) {
        use crate::daemon::DaemonConfig;
        let temp = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(DaemonConfig {
            stale_turn_quiet_ms: None,
            hook_silence_grace_ms: None,
            forward: Default::default(),
            db_path: None,
            socket_path: temp.path().join("unused.sock"),
            http_addr: None,
            http_tls: None,
            worker_addr: None,
            public_url: None,
            scrollback_dir: temp.path().join("scrollback"),
            registry: pm_adapters::AdapterRegistry::empty(),
            local_worker_enabled: true,
            release_channel: None,
        })
        .unwrap()
        .0;
        (daemon, temp)
    }

    #[tokio::test]
    async fn legacy_authorized_calls_outside_scope_fail_before_credentials_are_used() {
        use pm_protocol::domain::{AgentKind, PermissionMode};
        let (daemon, temp) = authorization_daemon();
        let bucket = daemon.storage.create_bucket("own").unwrap();
        let foreign_bucket = daemon.storage.create_bucket("foreign").unwrap();
        let own = daemon
            .storage
            .create_project(bucket.id, "own", temp.path().to_str().unwrap())
            .unwrap();
        let sibling = daemon
            .storage
            .create_project(bucket.id, "sibling", temp.path().to_str().unwrap())
            .unwrap();
        let foreign = daemon
            .storage
            .create_project(foreign_bucket.id, "foreign", temp.path().to_str().unwrap())
            .unwrap();
        for (supervisor, target) in [(false, sibling.id), (true, foreign.id)] {
            let session = daemon
                .storage
                .create_session(
                    own.id,
                    AgentKind::ClaudeCode,
                    "task",
                    "prompt",
                    PermissionMode::Default,
                    0,
                    true,
                    supervisor,
                    None,
                    now_unix_ms(),
                )
                .unwrap();
            let config: Config = serde_json::from_value(
                json!({"name":"legacy","project_id":target,"endpoint":"http://127.0.0.1"}),
            )
            .unwrap();
            let connection = daemon
                .storage
                .create_connection(config, Some(session.id))
                .unwrap();
            daemon
                .storage
                .conn
                .lock()
                .unwrap()
                .execute(
                    "UPDATE connections SET active=1, tested_revision=revision WHERE id=?1",
                    [connection.id],
                )
                .unwrap();
            let mut call = pending_call(now_unix_ms());
            call.id = store::random_id();
            call.session_id = session.id;
            call.project_id = own.id;
            call.connection_id = connection.id;
            call.connection_revision = connection.revision;
            call.status = "authorized".into();
            daemon.storage.create_connection_call(call.clone()).unwrap();
            daemon.execute_connection_call(&call.id).await.unwrap();
            let result = daemon.storage.connection_call(&call.id).unwrap();
            assert_eq!(result.status, "failed");
            assert!(result.error.unwrap().contains("scope"));
            assert!(result.result.is_none());
        }
    }

    #[tokio::test]
    async fn a_demotion_during_oauth_refresh_prevents_the_downstream_request() {
        use axum::{
            routing::{get, post},
            Json, Router,
        };
        use pm_protocol::domain::{AgentKind, PermissionMode};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (daemon, temp) = authorization_daemon();
        let daemon = Arc::new(daemon);
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let requests = Arc::new(AtomicUsize::new(0));
        let router = Router::new().route("/token", post({
            let started = started.clone();
            let release = release.clone();
            move || {
                let started = started.clone();
                let release = release.clone();
                async move {
                    started.notify_one();
                    release.notified().await;
                    Json(json!({"token_type":"Bearer","access_token":"placeholder-refreshed","expires_in":OAUTH_TEST_LIFETIME_SECS}))
                }
            }
        })).route("/orders", get({
            let requests = requests.clone();
            move || {
                requests.fetch_add(1, Ordering::SeqCst);
                async { Json(json!({"orders":[]})) }
            }
        }));
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let bucket = daemon.storage.create_bucket("own").unwrap();
        let own = daemon
            .storage
            .create_project(bucket.id, "own", temp.path().to_str().unwrap())
            .unwrap();
        let sibling = daemon
            .storage
            .create_project(bucket.id, "sibling", temp.path().to_str().unwrap())
            .unwrap();
        let session = daemon
            .storage
            .create_session(
                own.id,
                AgentKind::ClaudeCode,
                "task",
                "prompt",
                PermissionMode::Default,
                0,
                true,
                true,
                None,
                now_unix_ms(),
            )
            .unwrap();
        let schema = json!({"openapi":"3.1.0","paths":{"/orders":{"get":{"operationId":"read"}}}});
        let config: Config = serde_json::from_value(json!({"name":"refresh","project_id":sibling.id,"kind":"openapi","endpoint":endpoint,"schema":schema,"rules":{"read":{"access":"read"}},"oauth":{"token_url":format!("{endpoint}/token"),"client_id":"placeholder-client","token_auth_method":"none"}})).unwrap();
        let connection = daemon
            .storage
            .create_connection(config, Some(session.id))
            .unwrap();
        daemon
            .storage
            .record_connection_test(
                connection.id,
                connection.revision,
                openapi::import(&schema).unwrap(),
            )
            .unwrap();
        daemon
            .save_connection_credential(
                connection.id,
                &Credential::OAuth {
                    access_token: "placeholder-expired".into(),
                    refresh_token: Some("placeholder-refresh".into()),
                    expires_at: Some(now_unix_ms()),
                    client_secret: None,
                },
            )
            .unwrap();
        daemon
            .storage
            .activate_connection(connection.id, connection.revision, true)
            .unwrap();
        let mut call = pending_call(now_unix_ms());
        call.id = store::random_id();
        call.session_id = session.id;
        call.project_id = own.id;
        call.connection_id = connection.id;
        call.connection_revision = connection.revision;
        call.tool = "read".into();
        call.status = "authorized".into();
        daemon.storage.create_connection_call(call.clone()).unwrap();
        let execution = {
            let daemon = daemon.clone();
            let id = call.id.clone();
            tokio::spawn(async move {
                daemon.execute_connection_call(&id).await.unwrap();
            })
        };
        tokio::time::timeout(AUTHORIZATION_TEST_TIMEOUT, started.notified())
            .await
            .unwrap();
        daemon
            .update_session_apis(session.id, None, Some(false))
            .unwrap();
        release.notify_one();
        tokio::time::timeout(AUTHORIZATION_TEST_TIMEOUT, execution)
            .await
            .unwrap()
            .unwrap();
        let result = daemon.storage.connection_call(&call.id).unwrap();
        assert_eq!(result.status, "failed");
        assert!(result.error.unwrap().contains("scope"));
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[test]
    fn a_wait_defaults_and_is_held_between_the_floor_and_the_cap() {
        let wait = |args: Value| call_wait(&args).unwrap();
        assert_eq!(wait(json!({})), Duration::from_millis(CALL_WAIT_DEFAULT_MS));
        assert_eq!(
            wait(json!({"wait_ms":0})),
            Duration::from_millis(CALL_WAIT_MIN_MS)
        );
        assert_eq!(
            wait(json!({"wait_ms":CALL_WAIT_MAX_MS + 1})),
            Duration::from_millis(CALL_WAIT_MAX_MS)
        );
        assert_eq!(wait(json!({"wait_ms":7_000})), Duration::from_millis(7_000));
        assert!(call_wait(&json!({"wait_ms":-1})).is_err());
    }

    #[test]
    fn an_unsettled_call_reports_stalled_once_it_is_five_minutes_old() {
        let created_at = 1_000_000;
        let just_short = unsettled_call_response(
            &pending_call(created_at),
            created_at + CALL_STALLED_AFTER_MS - 1,
        )
        .unwrap();
        assert_eq!(just_short["status"], "pending");
        assert_eq!(just_short["waiting"]["stalled"], false);
        assert!(just_short["waiting"]["advice"]
            .as_str()
            .unwrap()
            .contains("Wait again with get_connection_call"));

        let stalled = unsettled_call_response(
            &pending_call(created_at),
            created_at + CALL_STALLED_AFTER_MS,
        )
        .unwrap();
        assert_eq!(stalled["waiting"]["stalled"], true);
        assert_eq!(stalled["waiting"]["elapsed_ms"], CALL_STALLED_AFTER_MS);
        assert!(stalled["waiting"]["advice"]
            .as_str()
            .unwrap()
            .contains("A notice is delivered to this session"));
    }
}
