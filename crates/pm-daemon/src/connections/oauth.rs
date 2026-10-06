use anyhow::{bail, Context, Result};
use base64::Engine;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    store::random_id, transport, validate_endpoint, Credential, Kind, OAuthConfig, TokenAuthMethod,
};
use crate::{daemon::Daemon, storage::now_unix_ms};

const OAUTH_STATE_TTL_MS: i64 = 10 * 60 * 1000;
const TOKEN_EXPIRY_MARGIN_MS: i64 = 60_000;
const SECOND_MS: i64 = 1000;

fn token_credential(value: Value, old: Credential) -> Result<Credential> {
    let access_token = value["access_token"]
        .as_str()
        .context("OAuth response has no access token")?
        .to_owned();
    if !value["token_type"]
        .as_str()
        .unwrap_or("Bearer")
        .eq_ignore_ascii_case("bearer")
    {
        bail!("Unsupported OAuth token type");
    }
    let (refresh_token, client_secret) = match old {
        Credential::OAuth {
            refresh_token,
            client_secret,
            ..
        } => (refresh_token, client_secret),
        _ => (None, None),
    };
    if access_token.is_empty() {
        bail!("OAuth response has an empty access token");
    }
    let expires_at = if let Some(value) = value.get("expires_in") {
        let seconds = value
            .as_i64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
            .context("Invalid OAuth token lifetime")?;
        if seconds <= 0 {
            bail!("OAuth token is already expired");
        }
        Some(
            seconds
                .checked_mul(SECOND_MS)
                .and_then(|ms| now_unix_ms().checked_add(ms))
                .context("Invalid OAuth token lifetime")?,
        )
    } else {
        None
    };
    Ok(Credential::OAuth {
        access_token,
        refresh_token: value["refresh_token"]
            .as_str()
            .map(str::to_owned)
            .or(refresh_token),
        client_secret,
        expires_at,
    })
}

async fn response_json(mut response: reqwest::Response) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > super::MAX_DOCUMENT_BYTES {
            bail!("OAuth response is too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn token_request<'a>(
    oauth: &OAuthConfig,
    secret: Option<&'a str>,
    mut form: Vec<(&str, &'a str)>,
) -> Result<reqwest::RequestBuilder> {
    let request = transport::client()?.post(&oauth.token_url);
    match oauth.token_auth_method {
        TokenAuthMethod::None => Ok(request.form(&form)),
        TokenAuthMethod::ClientSecretPost => {
            form.push((
                "client_secret",
                secret.context("OAuth client secret is required")?,
            ));
            Ok(request.form(&form))
        }
        TokenAuthMethod::ClientSecretBasic => {
            let encode = |value: &str| {
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
            };
            Ok(request
                .basic_auth(
                    encode(&oauth.client_id),
                    Some(encode(secret.context("OAuth client secret is required")?)),
                )
                .form(&form))
        }
    }
}

async fn is_invalid_client(response: reqwest::Response) -> bool {
    matches!(response_json(response).await, Ok(body) if body["error"] == "invalid_client")
}

const REGISTERED_CLIENT_NAME: &str = "Puppet Master";

fn challenge_param(challenge: &str, name: &str) -> Option<String> {
    let (_, tail) = challenge.split_once(&format!("{name}=\""))?;
    tail.split('"').next().map(str::to_owned)
}

fn scope_list(value: &Value) -> String {
    value
        .as_array()
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// RFC 8414 metadata lives under the well-known prefix with the issuer path
/// inserted after it, or at the origin root for an issuer with no path.
fn well_known(base: &reqwest::Url, suffix: &str, insert_path: bool) -> reqwest::Url {
    let mut url = base.clone();
    let path = if insert_path {
        format!("/.well-known/{suffix}{}", base.path().trim_end_matches('/'))
    } else {
        format!("/.well-known/{suffix}")
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn same_resource(left: &str, right: &str) -> bool {
    match (reqwest::Url::parse(left), reqwest::Url::parse(right)) {
        (Ok(left), Ok(right)) => {
            left.origin() == right.origin()
                && left.path().trim_end_matches('/') == right.path().trim_end_matches('/')
                && left.query() == right.query()
        }
        _ => left == right,
    }
}

/// Plain HTTP would expose a client secret, so only loopback skips TLS.
fn require_tls(url: &reqwest::Url) -> Result<()> {
    let loopback = url.host().is_some_and(|host| match host {
        url::Host::Domain(domain) => domain == "localhost",
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    });
    if url.scheme() != "https" && !loopback {
        bail!("OAuth endpoints must use HTTPS");
    }
    Ok(())
}

fn auth_method_name(method: TokenAuthMethod) -> &'static str {
    match method {
        TokenAuthMethod::None => "none",
        TokenAuthMethod::ClientSecretPost => "client_secret_post",
        TokenAuthMethod::ClientSecretBasic => "client_secret_basic",
    }
}

fn auth_method_from(name: &str) -> Option<TokenAuthMethod> {
    match name {
        "none" => Some(TokenAuthMethod::None),
        "client_secret_post" => Some(TokenAuthMethod::ClientSecretPost),
        "client_secret_basic" => Some(TokenAuthMethod::ClientSecretBasic),
        _ => None,
    }
}

impl Daemon {
    pub(super) fn save_connection_credential(
        &self,
        id: u64,
        credential: &Credential,
    ) -> Result<()> {
        let sealed = crate::secrets::seal_secret(
            &self.installation_secret(),
            &serde_json::to_string(credential)?,
        );
        self.storage.conn.lock().unwrap().execute(
            "UPDATE connections SET credential=?1 WHERE id=?2",
            params![sealed, id],
        )?;
        Ok(())
    }

    pub(super) async fn fresh_connection_credential(&self, id: u64) -> Result<Credential> {
        let credential = self.connection_credential(id)?;
        let Credential::OAuth {
            ref access_token,
            ref refresh_token,
            expires_at,
            ref client_secret,
            ..
        } = credential
        else {
            return Ok(credential);
        };
        if access_token.is_empty() {
            bail!("Complete OAuth sign-in before testing or calling tools");
        }
        if expires_at.is_none_or(|expiry| expiry > now_unix_ms() + TOKEN_EXPIRY_MARGIN_MS) {
            return Ok(credential);
        }
        let refresh = refresh_token
            .as_ref()
            .context("OAuth sign-in expired. Sign in again")?;
        let connection = self.storage.connection(id)?;
        let oauth = connection
            .config
            .oauth
            .context("OAuth configuration is missing")?;
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", oauth.client_id.as_str()),
        ];
        if connection.config.kind == Kind::Mcp {
            form.push(("resource", connection.config.endpoint.as_str()));
        }
        let response = token_request(&oauth, client_secret.as_deref(), form)?
            .send()
            .await?;
        if !response.status().is_success() {
            if oauth.registered && is_invalid_client(response).await {
                bail!("OAuth client registration expired. Register and sign in again");
            }
            bail!("OAuth refresh failed. Sign in again");
        }
        let credential = token_credential(response_json(response).await?, credential)?;
        self.save_connection_credential(id, &credential)?;
        Ok(credential)
    }

    pub(super) async fn discover_connection_oauth(
        &self,
        id: u64,
        redirect_uri: &str,
    ) -> Result<OAuthConfig> {
        validate_endpoint(redirect_uri)?;
        let connection = self.storage.connection(id)?;
        let endpoint = validate_endpoint(&connection.config.endpoint)?;
        let client = transport::client()?;
        let response = client.get(endpoint.clone()).send().await?;
        let challenge = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_default();
        let mut candidates = Vec::new();
        if let Some(url) = challenge_param(&challenge, "resource_metadata") {
            candidates.push(validate_endpoint(&url)?);
        }
        candidates.push(well_known(&endpoint, "oauth-protected-resource", true));
        candidates.push(well_known(&endpoint, "oauth-protected-resource", false));
        let mut issuer = None;
        let mut scopes = challenge_param(&challenge, "scope").unwrap_or_default();
        for url in candidates {
            let response = client.get(url).send().await?;
            if !response.status().is_success() {
                continue;
            }
            let metadata = response_json(response).await?;
            if metadata["resource"]
                .as_str()
                .is_none_or(|resource| !same_resource(resource, &connection.config.endpoint))
            {
                bail!("OAuth resource metadata does not match this connection");
            }
            issuer = metadata["authorization_servers"][0]
                .as_str()
                .map(str::to_owned);
            if scopes.is_empty() {
                scopes = scope_list(&metadata["scopes_supported"]);
            }
            if issuer.is_some() {
                break;
            }
        }
        // Without resource metadata the server is taken to be its own authorization server.
        let issuer = issuer.unwrap_or_else(|| endpoint.origin().ascii_serialization());
        let issuer_url = validate_endpoint(&issuer)?;
        let mut metadata = None;
        for (suffix, inserted) in [
            ("oauth-authorization-server", true),
            ("openid-configuration", true),
            ("oauth-authorization-server", false),
            ("openid-configuration", false),
        ] {
            let response = client
                .get(well_known(&issuer_url, suffix, inserted))
                .send()
                .await?;
            if response.status().is_success() {
                metadata = Some(response_json(response).await?);
                break;
            }
        }
        let metadata = metadata.context(
            "OAuth discovery unavailable. Enter the provider's OAuth endpoints and client ID",
        )?;
        if metadata["issuer"]
            .as_str()
            .is_none_or(|found| !same_resource(found, &issuer))
        {
            bail!("OAuth server issuer mismatch");
        }
        let endpoint_of = |key: &str| -> Result<String> {
            let url = metadata[key]
                .as_str()
                .with_context(|| format!("Missing OAuth {}", key.replace('_', " ")))?;
            require_tls(&validate_endpoint(url)?)?;
            Ok(url.to_owned())
        };
        let registration_endpoint = match metadata["registration_endpoint"].as_str() {
            Some(url) => {
                require_tls(&validate_endpoint(url)?)?;
                Some(url.to_owned())
            }
            None => None,
        };
        let current = connection.config.oauth.as_ref();
        Ok(OAuthConfig {
            token_auth_method: if metadata["token_endpoint_auth_methods_supported"]
                .as_array()
                .is_none_or(|methods| methods.iter().any(|method| method == "none"))
            {
                TokenAuthMethod::None
            } else {
                TokenAuthMethod::ClientSecretBasic
            },
            authorization_url: endpoint_of("authorization_endpoint")?,
            token_url: endpoint_of("token_endpoint")?,
            client_id: current.map(|o| o.client_id.clone()).unwrap_or_default(),
            scopes: current
                .map(|o| o.scopes.clone())
                .filter(|scopes| !scopes.is_empty())
                .unwrap_or(scopes),
            redirect_uri: redirect_uri.into(),
            registration_endpoint,
            registered: current.is_some_and(|o| o.registered),
        })
    }

    /// Replaces the connection's OAuth setup and credential with the registered client.
    pub(super) async fn register_connection_oauth(
        &self,
        id: u64,
        redirect_uri: &str,
    ) -> Result<()> {
        let mut oauth = self.discover_connection_oauth(id, redirect_uri).await?;
        let registration = oauth.registration_endpoint.clone().context(
            "This server does not register clients automatically. Enter a client ID from the provider",
        )?;
        let mut request = json!({
            "client_name": REGISTERED_CLIENT_NAME,
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": auth_method_name(oauth.token_auth_method),
        });
        if !oauth.scopes.is_empty() {
            request["scope"] = json!(oauth.scopes);
        }
        let response = transport::client()?
            .post(&registration)
            .json(&request)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("The server refused to register a client. Enter a client ID from the provider");
        }
        let registered = response_json(response).await?;
        let client_id = registered["client_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("Client registration returned no client id")?;
        oauth.client_id = client_id.to_owned();
        oauth.registered = true;
        if let Some(method) = registered["token_endpoint_auth_method"].as_str() {
            oauth.token_auth_method = auth_method_from(method).context(
                "Client registration requires an unsupported token endpoint authentication",
            )?;
        }
        let client_secret = registered["client_secret"].as_str().map(str::to_owned);
        if oauth.token_auth_method != TokenAuthMethod::None && client_secret.is_none() {
            bail!("Client registration returned no client secret");
        }
        let lock = self.connection_lock(id);
        let _guard = lock.lock().await;
        let connection = self.storage.connection(id)?;
        let mut config = connection.config;
        config.oauth = Some(oauth);
        let credential = Credential::OAuth {
            access_token: String::new(),
            refresh_token: None,
            expires_at: None,
            client_secret,
        };
        let sealed = crate::secrets::seal_secret(
            &self.installation_secret(),
            &serde_json::to_string(&credential)?,
        );
        self.storage
            .update_connection(id, connection.revision, &config, Some(Some(sealed)))?;
        Ok(())
    }

    pub(super) fn start_connection_oauth(&self, id: u64, username: &str) -> Result<String> {
        let connection = self.storage.connection(id)?;
        let oauth = connection
            .config
            .oauth
            .context("Configure OAuth endpoints and a client ID first")?;
        if oauth.client_id.trim().is_empty()
            || oauth.redirect_uri.is_empty()
            || oauth.token_url.is_empty()
        {
            bail!(
                "Configure the OAuth client ID, token endpoint, and redirect URI before signing in"
            );
        }
        let state = random_id();
        let verifier = random_id();
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let secret = crate::secrets::seal_secret(
            &self.installation_secret(),
            &json!({"verifier":verifier,"username":username}).to_string(),
        );
        self.storage.conn.lock().unwrap().execute("INSERT INTO connection_oauth(state,connection_id,revision,expires_at,secret) VALUES(?1,?2,?3,?4,?5)",params![state,id,connection.revision,now_unix_ms()+OAUTH_STATE_TTL_MS,secret])?;
        let mut url = validate_endpoint(&oauth.authorization_url)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", oauth.client_id.as_str()),
            ("redirect_uri", oauth.redirect_uri.as_str()),
            ("scope", oauth.scopes.as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        if connection.config.kind == Kind::Mcp {
            url.query_pairs_mut()
                .append_pair("resource", &connection.config.endpoint);
        }
        Ok(url.to_string())
    }

    pub(super) async fn complete_connection_oauth(&self, state: &str, code: &str) -> Result<()> {
        let record: Option<(u64, u64, String)> = {
            let mut conn = self.storage.conn.lock().unwrap();
            let tx = conn.transaction()?;
            let record = tx.query_row("SELECT connection_id,revision,secret FROM connection_oauth WHERE state=?1 AND expires_at>?2",params![state,now_unix_ms()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            tx.execute(
                "DELETE FROM connection_oauth WHERE state=?1 OR expires_at<=?2",
                params![state, now_unix_ms()],
            )?;
            tx.commit()?;
            record
        };
        let (id, revision, sealed) = record.context("OAuth request expired or was already used")?;
        let lock = self.connection_lock(id);
        let _guard = lock.lock().await;
        let connection = self.storage.connection(id)?;
        if connection.revision != revision {
            bail!("Connection changed during sign-in. Start again");
        }
        let state: Value = serde_json::from_str(
            &crate::secrets::open_secret(&self.installation_secret(), &sealed)
                .context("OAuth state could not be decrypted")?,
        )?;
        let oauth = connection
            .config
            .oauth
            .context("OAuth configuration missing")?;
        let old = self.connection_credential(id)?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", oauth.client_id.as_str()),
            ("redirect_uri", oauth.redirect_uri.as_str()),
            (
                "code_verifier",
                state["verifier"].as_str().context("Missing verifier")?,
            ),
        ];
        if connection.config.kind == Kind::Mcp {
            form.push(("resource", connection.config.endpoint.as_str()));
        }
        let secret = match &old {
            Credential::OAuth { client_secret, .. } => client_secret.as_deref(),
            _ => None,
        };
        let response = token_request(&oauth, secret, form)?.send().await?;
        if !response.status().is_success() {
            if oauth.registered && is_invalid_client(response).await {
                bail!("OAuth client registration expired. Register and sign in again");
            }
            bail!("OAuth token exchange failed. Start sign-in again");
        }
        let credential = token_credential(response_json(response).await?, old)?;
        let config = self.storage.connection(id)?.config;
        let sealed = crate::secrets::seal_secret(
            &self.installation_secret(),
            &serde_json::to_string(&credential)?,
        );
        self.storage
            .update_connection(id, revision, &config, Some(Some(sealed)))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_is_allowed_only_on_loopback() {
        for url in [
            "http://localhost:8080/register",
            "http://127.0.0.1/token",
            "https://auth.example.test/register",
        ] {
            assert!(
                require_tls(&reqwest::Url::parse(url).unwrap()).is_ok(),
                "{url}"
            );
        }
        assert!(
            require_tls(&reqwest::Url::parse("http://auth.example.test/register").unwrap())
                .is_err()
        );
    }

    #[test]
    fn metadata_paths_insert_the_issuer_path_or_sit_at_the_root() {
        let issuer = reqwest::Url::parse("https://mcp.example.test/tenant/a?x=1").unwrap();
        assert_eq!(
            well_known(&issuer, "oauth-authorization-server", true).as_str(),
            "https://mcp.example.test/.well-known/oauth-authorization-server/tenant/a"
        );
        assert_eq!(
            well_known(&issuer, "openid-configuration", false).as_str(),
            "https://mcp.example.test/.well-known/openid-configuration"
        );
    }

    #[test]
    fn resources_match_up_to_a_trailing_slash_and_challenges_yield_their_parameters() {
        assert!(same_resource(
            "https://mcp.example.test/mcp/",
            "https://mcp.example.test/mcp"
        ));
        assert!(!same_resource(
            "https://mcp.example.test/mcp",
            "https://mcp.example.test/other"
        ));
        let challenge = "Bearer realm=\"mcp\", resource_metadata=\"https://mcp.example.test/.well-known/oauth-protected-resource\", scope=\"a b\"";
        assert_eq!(
            challenge_param(challenge, "resource_metadata").as_deref(),
            Some("https://mcp.example.test/.well-known/oauth-protected-resource")
        );
        assert_eq!(challenge_param(challenge, "scope").as_deref(), Some("a b"));
        assert_eq!(challenge_param(challenge, "nonce"), None);
    }
}
