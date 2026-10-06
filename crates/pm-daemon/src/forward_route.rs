use std::sync::Arc;
use std::time::Duration;

use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use hyper::body::Body as _;
use hyper_util::rt::TokioIo;
use pm_protocol::domain::LOCAL_WORKER_ID;

use crate::daemon::Daemon;
use crate::forward::{FORWARD_TOKEN_TTL, STREAM_DIAL_TIMEOUT};
use crate::forward_proxy::ForwardStream;
use crate::forward_upstream::{multiplexed_handshake, Lease, PooledBody, ShareKeepalive};

const PROXY_BUFFER_BYTES: usize = 64 * 1024;
const FORWARD_COOKIE_PREFIX: &str = "pm_fwd_";

/// What every cookie this controller sets begins with.
///
/// A preview must not be handed the dashboard's cookies, and naming each one
/// here would mean a cookie added later is forwarded until somebody remembers
/// this list. A prefix cannot be forgotten, and a test holds the controller's
/// own cookie names to it. A forwarded app that sets its own `pm_` cookie loses
/// it, which is the safe direction to be wrong in.
const CONTROLLER_COOKIE_PREFIX: &str = "pm_";

fn is_controller_cookie(name: &str) -> bool {
    name.trim().starts_with(CONTROLLER_COOKIE_PREFIX)
}
const TOKEN_PARAMETER: &str = "fwd_token";

/// The dashboard endpoint that mints a forward's opening token, which is
/// the one dashboard route a rooted forward mount also serves.
pub(crate) fn token_path(forward_id: u64) -> String {
    format!("/api/forwards/{forward_id}/token")
}

/// The dashboard's own `/forwards/{id}/` mount.
pub(crate) async fn proxy(State(daemon): State<Arc<Daemon>>, request: Request) -> Response {
    let path = request.uri().path();
    let Some((id, rest)) = path.strip_prefix("/forwards/").and_then(|p| {
        let (id, rest) = p.split_once('/').unwrap_or((p, ""));
        id.parse::<u64>().ok().map(|id| (id, rest.to_string()))
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let bare = path == format!("/forwards/{id}");
    // Under a rooted mount the forward lives on its own origin, so the
    // dashboard's own path is not a second, half-mounted way in.
    if !matches!(
        daemon.forward_mount_mode(),
        crate::forward_mount::MountMode::PathPrefix
    ) {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve(daemon, id, rest, bare, request).await
}

/// A forward that owns its whole origin: a share-domain host, or a
/// listener bound for this forward alone. Every path belongs to the
/// forwarded application, so nothing is stripped but the leading slash.
pub(crate) async fn proxy_rooted(daemon: Arc<Daemon>, id: u64, request: Request) -> Response {
    let rest = request.uri().path().trim_start_matches('/').to_string();
    serve(daemon, id, rest, false, request).await
}

/// `bare` marks a request to `/forwards/{id}` with no trailing slash,
/// which redirects into the mount so relative URLs resolve inside it.
async fn serve(
    daemon: Arc<Daemon>,
    id: u64,
    rest: String,
    bare: bool,
    mut request: Request,
) -> Response {
    let Ok(forward) = daemon.storage.session_forward(id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !crate::forward_proxy::is_http_scheme(&forward.scheme) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !daemon.forwards.is_active(id) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "This forward is stopped. Resume its session and server.",
        )
            .into_response();
    }
    let cookie_name = format!("{FORWARD_COOKIE_PREFIX}{id}");
    let cookies = request
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ");
    let cookie_token = cookies
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find_map(|(name, value)| (name == cookie_name).then_some(value));
    let query_token = request
        .uri()
        .query()
        .unwrap_or("")
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find_map(|(name, value)| (name == TOKEN_PARAMETER).then_some(value.to_string()));
    let valid_query = query_token
        .as_deref()
        .filter(|t| daemon.verify_forward_token(t, id).is_some());
    let authenticated = crate::http::authed_identity(&daemon, request.headers()).is_some()
        || cookie_token.is_some_and(|t| daemon.verify_forward_token(t, id).is_some())
        || valid_query.is_some();
    // Only a page open can follow a redirect without losing something. A
    // POST would come back as a GET without its body, and a request that
    // did not ask for HTML would get the login page where it expected an
    // asset or JSON, which fails as a decode error rather than as auth.
    let navigation = request.method() == axum::http::Method::GET
        && request
            .headers()
            .get(header::ACCEPT)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|a| a.contains("text/html"));
    if !authenticated {
        // A query token that did not verify is the one credential the
        // handoff cannot replace, because it just minted that one: going
        // back for another would loop. A cookie a restart wiped is worth
        // the trip, since minting is exactly what fixes it.
        if query_token.is_none() && navigation {
            let mut response = StatusCode::SEE_OTHER.into_response();
            let host = request
                .headers()
                .get(header::HOST)
                .and_then(|h| h.to_str().ok());
            let Some(mount) = daemon.forward_mount(&forward, host) else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let destination = if mount.prefix.is_none() {
                format!(
                    "{}{}",
                    mount.url.trim_end_matches('/'),
                    request
                        .uri()
                        .path_and_query()
                        .map(|p| p.as_str())
                        .unwrap_or("/")
                )
            } else {
                format!(
                    "{}{rest}{}",
                    mount.url,
                    request
                        .uri()
                        .query()
                        .map(|q| format!("?{q}"))
                        .unwrap_or_default()
                )
            };
            let Some(location) = handoff_location(&daemon.login_url(), id, &mount, &destination)
            else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            if let Ok(value) = HeaderValue::from_str(&location) {
                response.headers_mut().insert(header::LOCATION, value);
            }
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            // Drop the credential that failed, so a reader who declines
            // the handoff is not left presenting it on every later
            // request.
            if cookie_token.is_some() {
                let secure = if mount.is_secure() { "; Secure" } else { "" };
                let cleared = format!(
                    "{cookie_name}=; Path={}; Max-Age=0; HttpOnly; SameSite=Lax{secure}",
                    mount.cookie_path()
                );
                if let Ok(value) = HeaderValue::from_str(&cleared) {
                    response.headers_mut().insert(header::SET_COOKIE, value);
                }
            }
            return response;
        }
        let message = if query_token.is_some() || cookie_token.is_some() {
            "This forward link has expired. Reopen it from Puppet Master."
        } else {
            "Sign in to Puppet Master, then reopen this forward."
        };
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CACHE_CONTROL, "no-store")],
            message,
        )
            .into_response();
    }
    let Some(mount) = daemon.forward_mount(
        &forward,
        request
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok()),
    ) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "This controller cannot name a public URL for its forwards. Pass --public-url.",
        )
            .into_response();
    };
    // The root of the mount, which a mounted redirect is composed
    // against and which a rooted forward reaches at `/`.
    let root = mount.prefix.clone().unwrap_or_else(|| "/".to_string());
    let clean_query = request
        .uri()
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty() && p.split('=').next() != Some(TOKEN_PARAMETER))
        .collect::<Vec<_>>()
        .join("&");
    let query_suffix = if clean_query.is_empty() {
        String::new()
    } else {
        format!("?{clean_query}")
    };
    if (valid_query.is_some() && navigation) || bare {
        let mut response = StatusCode::TEMPORARY_REDIRECT.into_response();
        let location = format!("{root}{rest}{query_suffix}");
        let Ok(location) = HeaderValue::from_str(&location) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        response.headers_mut().insert(header::LOCATION, location);
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response.headers_mut().insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        if let Some(token) = valid_query {
            let secure = if mount.is_secure() { "; Secure" } else { "" };
            let cookie = format!(
                "{cookie_name}={token}; Path={}; Max-Age={}; HttpOnly; SameSite=Lax{secure}",
                mount.cookie_path(),
                FORWARD_TOKEN_TTL.as_secs()
            );
            if let Ok(value) = HeaderValue::from_str(&cookie) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
        }
        return response;
    }
    let upgrade = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let downstream_upgrade = upgrade.then(|| hyper::upgrade::on(&mut request));
    // Read while the header is still here: `strip_hop_headers` removes it.
    let client_close = asks_to_close(request.headers());
    let session = match daemon.storage.get_session(forward.session_id) {
        Ok(session) => session,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    // A share's own server speaks HTTP/2, so one connection carries
    // every request for it. An upgrade still takes the HTTP/1 path,
    // because HTTP/2 cannot carry one.
    let multiplexed = !upgrade && daemon.dir_share_speaks_h2(id, session.worker_id);
    let authority = format!("127.0.0.1:{}", forward.worker_port);
    // HTTP/2 names the target in pseudo-headers, and those come from the
    // URI rather than from `Host`, so it needs the absolute form.
    let target = if multiplexed {
        format!("http://{authority}/{rest}{query_suffix}")
    } else {
        format!("/{rest}{query_suffix}")
    };
    let Ok(uri) = target.parse::<Uri>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    *request.uri_mut() = uri;
    *request.version_mut() = if multiplexed {
        axum::http::Version::HTTP_2
    } else {
        axum::http::Version::HTTP_11
    };
    strip_hop_headers(request.headers_mut(), upgrade);
    strip_credentials(request.headers_mut());
    for name in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-forwarded-prefix",
    ] {
        request.headers_mut().remove(name);
    }
    if let Ok(url) = reqwest::Url::parse(&mount.base) {
        if let Some(host) = url.host_str() {
            let authority = url
                .port()
                .map(|p| format!("{host}:{p}"))
                .unwrap_or_else(|| host.to_string());
            if let Ok(value) = HeaderValue::from_str(&authority) {
                request.headers_mut().insert("x-forwarded-host", value);
            }
        }
        if let Ok(value) = HeaderValue::from_str(url.scheme()) {
            request.headers_mut().insert("x-forwarded-proto", value);
        }
    }
    // A rooted forward is at the root of its own origin, so there is no
    // prefix for the application to compose its URLs against.
    if let Some(prefix) = mount.prefix.as_deref() {
        if let Ok(value) = HeaderValue::from_str(prefix.trim_end_matches('/')) {
            request.headers_mut().insert("x-forwarded-prefix", value);
        }
    }
    request
        .headers_mut()
        .insert(header::HOST, HeaderValue::from_str(&authority).unwrap());
    if multiplexed {
        return serve_multiplexed(&daemon, id, &forward, session.worker_id, request, &mount).await;
    }
    let replay = replayable_copy(&request);
    let mut lease = match acquire(&daemon, id, session.worker_id, forward.worker_port).await {
        Ok(lease) => lease,
        Err(message) => return (StatusCode::BAD_GATEWAY, message).into_response(),
    };
    let response_timeout = daemon.forward_response_timeout();
    let mut response = match send_upstream(&mut lease, request, response_timeout).await {
        Sent::Answered(response) => response,
        Sent::TimedOut => return timed_out(),
        // A connection taken from the pool may have been closed by the
        // target while it sat there, which is worth one more attempt on
        // a connection opened for it. A connection just opened has no
        // such excuse, and a request carrying a body cannot be repeated
        // because its body may already have been read.
        Sent::Failed => match replay.filter(|_| lease.reused()) {
            Some(again) => {
                drop(lease);
                let mut fresh =
                    match open_lease(&daemon, id, session.worker_id, forward.worker_port).await {
                        Ok(fresh) => fresh,
                        Err(message) => return (StatusCode::BAD_GATEWAY, message).into_response(),
                    };
                match send_upstream(&mut fresh, again, response_timeout).await {
                    Sent::Answered(response) => {
                        lease = fresh;
                        response
                    }
                    Sent::TimedOut => return timed_out(),
                    Sent::Failed => return refused(),
                }
            }
            None => return refused(),
        },
    };
    let switching = upgrade && response.status() == StatusCode::SWITCHING_PROTOCOLS;
    if switching {
        let upstream_upgrade = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            if let (Ok(downstream), Ok(upstream)) =
                tokio::join!(downstream_upgrade.unwrap(), upstream_upgrade)
            {
                let _ = tokio::io::copy_bidirectional(
                    &mut TokioIo::new(downstream),
                    &mut TokioIo::new(upstream),
                )
                .await;
            }
        });
    }
    // A 101 has turned the connection into a tunnel, and a close asked
    // for by either side ends it after this exchange. Both answers have
    // to be read before `strip_hop_headers` takes the header away.
    let reusable = !switching && !client_close && !asks_to_close(response.headers());
    strip_hop_headers(response.headers_mut(), switching);
    rewrite_response_headers(response.headers_mut(), mount.prefix.as_deref());
    if !reusable {
        return response.map(Body::new);
    }
    let (parts, incoming) = response.into_parts();
    Response::from_parts(parts, Body::new(PooledBody::new(incoming, lease)))
}

fn refused() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        "The forwarded server closed or returned an invalid HTTP response.",
    )
        .into_response()
}

fn timed_out() -> Response {
    (
        StatusCode::GATEWAY_TIMEOUT,
        "The forwarded server did not respond in time.",
    )
        .into_response()
}

/// Whether a `Connection` header nominates `close`.
fn asks_to_close(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("close"))
}

/// A copy of a request that may be sent a second time, for the requests
/// where that is sound: no body to replay, so nothing can have been
/// read from it, and a method that repeating does not act on twice.
fn replayable_copy(request: &Request) -> Option<Request> {
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) || request.body().size_hint().exact() != Some(0)
    {
        return None;
    }
    let mut copy = Request::new(Body::empty());
    *copy.method_mut() = request.method().clone();
    *copy.uri_mut() = request.uri().clone();
    *copy.version_mut() = request.version();
    *copy.headers_mut() = request.headers().clone();
    Some(copy)
}

/// What came back from an attempt to send a request upstream.
enum Sent {
    Answered(hyper::Response<hyper::body::Incoming>),
    Failed,
    TimedOut,
}

async fn send_upstream(lease: &mut Lease, request: Request, response_timeout: Duration) -> Sent {
    match tokio::time::timeout(response_timeout, lease.sender().send_request(request)).await {
        Ok(Ok(response)) => Sent::Answered(response),
        Ok(Err(_)) => Sent::Failed,
        Err(_) => Sent::TimedOut,
    }
}

/// Carries one request down the single HTTP/2 connection a directory
/// share's forward holds. There is no lease to give back: the
/// connection is shared by every request for this forward and lives
/// until the forward does.
async fn serve_multiplexed(
    daemon: &Arc<Daemon>,
    id: u64,
    forward: &pm_protocol::domain::SessionForward,
    worker: u64,
    request: Request,
    mount: &crate::forward_mount::ForwardMount,
) -> Response {
    let port = forward.worker_port;
    let (mut sender, alive) = match multiplexed_upstream(daemon, id, worker, port).await {
        Ok(upstream) => upstream,
        Err(message) => return (StatusCode::BAD_GATEWAY, message).into_response(),
    };
    let response_timeout = daemon.forward_response_timeout();
    let mut response =
        match tokio::time::timeout(response_timeout, sender.send_request(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => {
                // The next request opens a new connection rather than
                // finding this one still installed and just as broken.
                daemon.upstreams.forget_multiplexed(id, port, &alive);
                return refused();
            }
            // A slow target and a tunnel that died half-open look the same
            // from here, and a connection left installed on the second
            // stays dead until the forward closes.
            Err(_) => {
                daemon.upstreams.forget_multiplexed(id, port, &alive);
                return timed_out();
            }
        };
    strip_hop_headers(response.headers_mut(), false);
    rewrite_response_headers(response.headers_mut(), mount.prefix.as_deref());
    response.map(Body::new)
}

/// The forward's one HTTP/2 connection, opened if it has none. The gate
/// is what makes it one connection and not one per concurrent request.
async fn multiplexed_upstream(
    daemon: &Arc<Daemon>,
    id: u64,
    worker: u64,
    port: u16,
) -> Result<
    (
        hyper::client::conn::http2::SendRequest<Body>,
        Arc<AtomicBool>,
    ),
    &'static str,
> {
    if let Some(upstream) = daemon.upstreams.multiplexed(id, port) {
        return Ok(upstream);
    }
    let gate = daemon.upstreams.opening_gate(id, port);
    let _opening = gate.lock().await;
    if let Some(upstream) = daemon.upstreams.multiplexed(id, port) {
        return Ok(upstream);
    }
    let target = open_target(daemon, id, worker, port).await?;
    let (sender, alive) = multiplexed_handshake(TokioIo::new(target), ShareKeepalive::DEFAULT)
        .await
        .map_err(|_| "The published directory's server did not complete an HTTP/2 handshake.")?;
    daemon
        .upstreams
        .install_multiplexed(id, port, sender.clone(), alive.clone());
    Ok((sender, alive))
}

/// An upstream for this request: a pooled one when the forward has one
/// to spare, else a newly opened one.
async fn acquire(
    daemon: &Arc<Daemon>,
    id: u64,
    worker: u64,
    port: u16,
) -> Result<Lease, &'static str> {
    if let Some(lease) = daemon.upstreams.acquire(id, port).await {
        return Ok(lease);
    }
    open_lease(daemon, id, worker, port).await
}

async fn open_lease(
    daemon: &Arc<Daemon>,
    id: u64,
    worker: u64,
    port: u16,
) -> Result<Lease, &'static str> {
    let target = open_target(daemon, id, worker, port).await?;
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(target))
        .await
        .map_err(|_| "The forwarded server did not answer as an HTTP server.")?;
    let alive = Arc::new(AtomicBool::new(true));
    let ended = alive.clone();
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
        ended.store(false, Ordering::Relaxed);
    });
    Ok(daemon.upstreams.lease_new(id, port, sender, alive))
}

fn strip_hop_headers(headers: &mut HeaderMap, upgrade: bool) {
    let nominated = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>();
    for name in nominated {
        if !(upgrade && name.eq_ignore_ascii_case("upgrade")) {
            headers.remove(name);
        }
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        if !(upgrade && matches!(name, "connection" | "upgrade")) {
            headers.remove(name);
        }
    }
}

fn strip_credentials(headers: &mut HeaderMap) {
    headers.remove(header::AUTHORIZATION);
    let cookies = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|c| {
            c.split_once('=')
                .is_some_and(|(name, _)| !is_controller_cookie(name))
        })
        .collect::<Vec<_>>()
        .join("; ");
    headers.remove(header::COOKIE);
    if !cookies.is_empty() {
        if let Ok(value) = HeaderValue::from_str(&cookies) {
            headers.insert(header::COOKIE, value);
        }
    }
    headers.remove(header::REFERER);
}

/// Mounts the application's own paths under the forward's prefix.
/// `prefix` is `None` for a forward that owns its whole origin, where the
/// application's paths already resolve and mounting them would break
/// them. `Domain` is dropped either way: on a share domain it would let
/// one forward set a cookie every other forward reads.
fn rewrite_response_headers(headers: &mut HeaderMap, prefix: Option<&str>) {
    if let (Some(prefix), Some(location)) = (
        prefix,
        headers.get(header::LOCATION).and_then(|h| h.to_str().ok()),
    ) {
        if location.starts_with('/') && !location.starts_with("//") && !location.starts_with(prefix)
        {
            if let Ok(value) = HeaderValue::from_str(&format!("{prefix}{}", &location[1..])) {
                headers.insert(header::LOCATION, value);
            }
        }
    }
    let cookies = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|cookie| {
            let mut parts = cookie.split(';');
            let pair = parts.next()?;
            let (name, _) = pair.split_once('=')?;
            if is_controller_cookie(name) {
                return None;
            }
            let mut attributes = Vec::new();
            let mut path = prefix.map(str::to_string);
            for part in parts {
                let key = part.trim().split('=').next().unwrap_or("");
                if key.eq_ignore_ascii_case("domain") {
                    continue;
                }
                match (key.eq_ignore_ascii_case("path"), prefix) {
                    (true, Some(prefix)) => {
                        if let Some((_, p)) = part.trim().split_once('=') {
                            path = Some(if p.starts_with(prefix) {
                                p.to_string()
                            } else {
                                format!("{prefix}{}", p.trim_start_matches('/'))
                            });
                        }
                    }
                    _ => attributes.push(part.trim()),
                }
            }
            let mut value = pair.to_string();
            if let Some(path) = &path {
                value.push_str(&format!("; Path={path}"));
            }
            for attribute in attributes {
                value.push_str("; ");
                value.push_str(attribute);
            }
            HeaderValue::from_str(&value).ok()
        })
        .collect::<Vec<_>>();
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        headers.append(header::SET_COOKIE, cookie);
    }
    headers.remove("service-worker-allowed");
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

async fn open_target(
    daemon: &Arc<Daemon>,
    id: u64,
    worker: u64,
    port: u16,
) -> Result<ForwardStream, &'static str> {
    if worker == LOCAL_WORKER_ID {
        return match tokio::time::timeout(
            STREAM_DIAL_TIMEOUT,
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await
        {
            Ok(Ok(tcp)) => {
                daemon.record_forward_target_reachable(id, true);
                Ok(ForwardStream::plain(tcp))
            }
            _ => {
                daemon.record_forward_target_reachable(id, false);
                Err("The forwarded server is not listening.")
            }
        };
    }
    let link = daemon
        .workers
        .get(worker)
        .ok_or("The forward's worker is offline.")?;
    let token = crate::auth::generate_token();
    let hash = crate::auth::hash_token(&token);
    let (client, tunnel) = tokio::io::duplex(PROXY_BUFFER_BYTES);
    daemon
        .forwards
        .park_stream(hash.clone(), ForwardStream::duplex(tunnel));
    let result = match link.request_forward_open(port, token) {
        Ok(result) => result,
        Err(_) => {
            daemon.forwards.claim_stream(&hash);
            return Err("The forward's worker disconnected.");
        }
    };
    match tokio::time::timeout(STREAM_DIAL_TIMEOUT, result).await {
        Ok(Ok(result)) if result.ok => {
            daemon.record_forward_target_reachable(id, true);
            let daemon = daemon.clone();
            tokio::spawn(async move {
                tokio::time::sleep(STREAM_DIAL_TIMEOUT).await;
                daemon.forwards.claim_stream(&hash);
            });
            Ok(ForwardStream::duplex(client))
        }
        _ => {
            daemon.record_forward_target_reachable(id, false);
            daemon.forwards.claim_stream(&hash);
            Err("The worker could not connect to the forwarded server.")
        }
    }
}

fn handoff_location(
    login_url: &str,
    id: u64,
    mount: &crate::forward_mount::ForwardMount,
    destination: &str,
) -> Option<String> {
    if !mount.accepts_destination(destination) {
        return None;
    }
    let mut encoded = reqwest::Url::parse(login_url).ok()?;
    encoded
        .query_pairs_mut()
        .append_pair("id", &id.to_string())
        .append_pair("destination", destination);
    let query = encoded.query()?;
    Some(format!(
        "{}/#/forward-open?{query}",
        login_url.strip_suffix("/login")?
    ))
}

#[cfg(test)]
mod tests {

    /// The prefix rule is only safe if every cookie this controller sets really
    /// does begin with it. Pinned here rather than trusted, so adding one that
    /// does not fails a test instead of being forwarded to previews.
    #[test]
    fn every_controller_cookie_name_carries_the_prefix() {
        for name in [
            crate::auth::SESSION_COOKIE,
            FORWARD_COOKIE_PREFIX,
            "pm_fwd",
            &format!("{FORWARD_COOKIE_PREFIX}7"),
        ] {
            assert!(
                is_controller_cookie(name),
                "{name} would be forwarded to a preview"
            );
        }
    }

    /// A forwarded application's own cookies still reach it, or the proxy would
    /// break every app that keeps state.
    #[test]
    fn an_applications_own_cookies_are_left_alone() {
        for name in ["session", "csrftoken", "sid", "theme", "_ga", "PMSESSION"] {
            assert!(!is_controller_cookie(name), "{name} was stripped");
        }
    }
    use super::*;

    /// What a second attempt is allowed to carry. The gate matters more
    /// than the copy: repeating anything else would either act twice on
    /// the target or send a body that has already been read.
    #[test]
    fn only_a_bodyless_safe_request_is_copied_for_a_second_attempt() {
        let get = axum::http::Request::builder()
            .method("GET")
            .uri("/page?x=1")
            .header("cookie", "app=keep")
            .body(Body::empty())
            .unwrap();
        let copy = replayable_copy(&get).expect("a bodyless GET may be repeated");
        assert_eq!(copy.method(), axum::http::Method::GET);
        assert_eq!(copy.uri(), "/page?x=1");
        assert_eq!(copy.headers()["cookie"], "app=keep");

        let posted = axum::http::Request::builder()
            .method("POST")
            .uri("/")
            .body(Body::empty())
            .unwrap();
        assert!(
            replayable_copy(&posted).is_none(),
            "a POST is not ours to repeat"
        );
        let carrying = axum::http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::from("payload"))
            .unwrap();
        assert!(
            replayable_copy(&carrying).is_none(),
            "a body may already have been read"
        );
    }

    #[test]
    fn a_close_is_recognised_among_other_nominated_headers() {
        let mut headers = HeaderMap::new();
        assert!(!asks_to_close(&headers));
        headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        assert!(!asks_to_close(&headers));
        headers.insert(header::CONNECTION, HeaderValue::from_static("x-hop, Close"));
        assert!(asks_to_close(&headers));
    }

    #[test]
    fn configured_app_paths_are_not_prefixed_twice() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::LOCATION,
            HeaderValue::from_static("/pm/forwards/7/login"),
        );
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_static("app=ok; Path=/pm/forwards/7/"),
        );
        rewrite_response_headers(&mut headers, Some("/pm/forwards/7/"));
        assert_eq!(headers[header::LOCATION], "/pm/forwards/7/login");
        assert_eq!(headers[header::SET_COOKIE], "app=ok; Path=/pm/forwards/7/");
    }

    #[test]
    fn connection_nominated_headers_are_removed_without_dropping_websocket_upgrade() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("upgrade, x-hop"),
        );
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert("x-hop", HeaderValue::from_static("private"));
        strip_hop_headers(&mut headers, true);
        assert!(!headers.contains_key("x-hop"));
        assert_eq!(headers[header::UPGRADE], "websocket");
        strip_hop_headers(&mut headers, false);
        assert!(!headers.contains_key(header::UPGRADE));
        assert!(!headers.contains_key(header::CONNECTION));
    }
}
