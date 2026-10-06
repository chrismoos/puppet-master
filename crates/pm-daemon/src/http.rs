//! HTTP surface: the web UI (embedded static bundle), cookie-session
//! auth endpoints under /api, the control WebSocket at /ws, and
//! independent terminal and worker data streams.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::extract::{Path, Query, Request};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pm_protocol::domain::{
    ClientEnvelope, ControllerMsg, Item, ItemPriority, ItemQuery, ItemSourceKind, ItemStatus,
    ItemSummaryFilter, ServerMsg, WorkerMsg,
};
use pm_protocol::worker_frame::{self, WorkerFrame};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::auth::{AuthError, SESSION_COOKIE};
use crate::connection::{handle_message, ConnState, OUTGOING_CHANNEL_CAPACITY};
use crate::daemon::Daemon;

/// WebSocket close code for a missing or invalid session cookie; the
/// browser client routes to login when it sees this.
const WS_CLOSE_UNAUTHENTICATED: u16 = 4401;

const COOKIE_MAX_AGE_SECS: i64 = 30 * 24 * 60 * 60;
const WORKSPACE_NAME_MAX_CHARS: usize = 80;
const WORKSPACE_LAYOUT_MAX_BYTES: usize = 64 * 1024;

#[cfg(not(debug_assertions))]
use rust_embed::RustEmbed;

#[cfg(not(debug_assertions))]
#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct UiAssets;

/// Bounds a TLS handshake so a client that connects and goes quiet cannot
/// hold a task forever.
const TLS_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Handshakes that completed but have not been taken by the server yet.
const TLS_ACCEPT_QUEUE: usize = 64;

pub fn load_tls(
    tls: crate::daemon::HttpTls,
) -> anyhow::Result<Arc<tokio_rustls::rustls::ServerConfig>> {
    let cert_chain = std::fs::read(&tls.cert_chain)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", tls.cert_chain.display()))?;
    let key = std::fs::read(&tls.key)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", tls.key.display()))?;
    let config = pm_tls::web_server_config(&cert_chain, &key)
        .map_err(|e| anyhow::anyhow!("loading HTTP TLS from {}: {e}", tls.cert_chain.display()))?;
    if let Ok(key) = pm_tls::web_cert_key_hash(&cert_chain) {
        info!(
            public_key_sha256 = %key,
            "serving HTTPS; `pm login` shows this fingerprint when no system root trusts the certificate"
        );
    }
    Ok(config)
}

/// Whether the bound address is reachable from outside this host with
/// nothing encrypting it. Mobile bearer and refresh tokens ride these
/// requests, so an off-host plaintext bind hands them to anyone on the
/// path.
fn reachable_without_tls(bound: &SocketAddr, tls: bool) -> bool {
    !tls && !bound.ip().is_loopback()
}

/// Reserves the browser plane's address without answering on it yet.
///
/// Binding and serving are separate steps because the daemon must not
/// answer a forward request before `recover_forwards` has run: a route
/// that is not bound yet reports a live forward as stopped. The address
/// has to be known first, though, since forward listeners and forward
/// URLs are both derived from it.
pub async fn bind(
    addr: SocketAddr,
    tls: bool,
) -> anyhow::Result<(tokio::net::TcpListener, SocketAddr)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    if reachable_without_tls(&bound, tls) {
        warn!(
            addr = %bound,
            "http server is reachable off this host without TLS, so web session cookies and mobile bearer and refresh tokens cross the network in the clear. Pass --http-tls-cert and --http-tls-key, terminate TLS in front of it, or bind 127.0.0.1"
        );
    }
    Ok((listener, bound))
}

/// Starts answering on an address [`bind`] already reserved.
pub fn serve(
    daemon: Arc<Daemon>,
    listener: tokio::net::TcpListener,
    bound: SocketAddr,
    tls: Option<Arc<tokio_rustls::rustls::ServerConfig>>,
) -> JoinHandle<()> {
    let app = router(daemon);
    match tls {
        Some(config) => {
            info!(addr = %bound, "https server listening");
            let listener = TlsListener::new(listener, bound, config);
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::error!(error = %e, "https server exited");
                }
            })
        }
        None => {
            info!(addr = %bound, "http server listening");
            tokio::spawn(async move {
                if let Err(e) = axum::serve(NoDelayListener(listener), app).await {
                    tracing::error!(error = %e, "http server exited");
                }
            })
        }
    }
}

/// Handshakes run in their own tasks so one slow or hostile client cannot
/// stall every other browser behind it in the accept loop.
pub(crate) struct TlsListener {
    local: SocketAddr,
    ready: mpsc::Receiver<(
        tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
        SocketAddr,
    )>,
}

impl TlsListener {
    pub(crate) fn new(
        listener: tokio::net::TcpListener,
        local: SocketAddr,
        config: Arc<tokio_rustls::rustls::ServerConfig>,
    ) -> Self {
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        let (tx, ready) = mpsc::channel(TLS_ACCEPT_QUEUE);
        tokio::spawn(async move {
            loop {
                let (stream, remote) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await
                    {
                        Ok(Ok(tls)) => {
                            let _ = tx.send((tls, remote)).await;
                        }
                        Ok(Err(e)) => debug!(%remote, error = %e, "https handshake failed"),
                        Err(_) => debug!(%remote, "https handshake timed out"),
                    }
                });
            }
        });
        Self { local, ready }
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(accepted) => accepted,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}

/// Disables Nagle on every accepted connection: interactive terminal
/// output and keystroke echoes are small writes where Nagle's
/// interaction with delayed ACK adds perceptible latency.
pub(crate) struct NoDelayListener(tokio::net::TcpListener);

impl axum::serve::Listener for NoDelayListener {
    type Io = tokio::net::TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((stream, addr)) => {
                    let _ = stream.set_nodelay(true);
                    return (stream, addr);
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(1)).await,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

/// The browser plane. Every request passes the share-domain dispatch
/// first, so a request whose Host names a forward never reaches a
/// dashboard route.
pub fn router(daemon: Arc<Daemon>) -> Router {
    dashboard_router(daemon.clone())
        .layer(axum::middleware::from_fn_with_state(
            daemon.clone(),
            guard_cookie_mutations,
        ))
        .layer(axum::middleware::from_fn_with_state(
            daemon,
            share_host_dispatch,
        ))
}

/// Refuses a state-changing request that carries the session cookie unless it
/// came from the dashboard's own origin.
///
/// CORS hides the response from a page on a same-site neighbour, but it does
/// nothing about the effect of a request that needs no preflight, and fifteen
/// mutating handlers here take no body at all while others take raw bytes. So
/// the content type a handler happens to require is not a control, and this
/// is checked once for every method that changes something rather than at the
/// handlers that currently lack the incidental protection.
///
/// The trigger is the cookie rather than the route, because the cookie is the
/// only credential the browser attaches by itself. A request authenticated by
/// a bearer token was made by something that already held the token, so the
/// MCP surface and the phone are untouched.
///
/// The proxied forward paths are left out: a forward authenticates against
/// its own scoped cookie or token, and a request into a preview is between
/// the viewer and the agent's own server.
async fn guard_cookie_mutations(
    State(daemon): State<Arc<Daemon>>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let exempt = request.method().is_safe()
        || request.uri().path().starts_with("/forwards/")
        || session_cookie_value(request.headers()).is_none();
    if exempt {
        return next.run(request).await;
    }
    if !crate::web_origin::mutation_is_own_origin(request.headers(), daemon.public_url()) {
        return refuse_foreign_origin(request.uri().path(), request.headers());
    }
    next.run(request).await
}

/// Serves a forward that owns its own host, when the share domain is
/// configured and the request's Host names one of its forwards.
///
/// A name under the share domain that resolves to no forward is
/// answered here rather than passed on: the dashboard must not be
/// reachable on a preview hostname, and a stale bookmark for a closed
/// forward must not land on it.
async fn share_host_dispatch(
    State(daemon): State<Arc<Daemon>>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    match daemon.share_host(host.as_deref()) {
        Some(crate::daemon::ShareHost::Forward(forward_id)) => {
            serve_rooted_forward(daemon, forward_id, request).await
        }
        Some(crate::daemon::ShareHost::Unknown) => StatusCode::NOT_FOUND.into_response(),
        None => next.run(request).await,
    }
}

/// Serves one forward on a listener of its own: that forward at the root,
/// its own opening-token handshake, and nothing else of the dashboard.
pub(crate) async fn serve_forward_listener(
    daemon: Arc<Daemon>,
    listener: tokio::net::TcpListener,
    forward_id: u64,
) {
    let Ok(bound) = listener.local_addr() else {
        return;
    };
    info!(forward = forward_id, addr = %bound, "forward listener serving http");
    let app = Router::new()
        .fallback(any(rooted_forward))
        .with_state((daemon.clone(), forward_id));
    let served = match daemon.http_tls_config() {
        Some(config) => axum::serve(TlsListener::new(listener, bound, config), app).await,
        None => axum::serve(NoDelayListener(listener), app).await,
    };
    if let Err(e) = served {
        warn!(forward = forward_id, error = %e, "forward listener exited");
    }
}

async fn rooted_forward(
    State((daemon, forward_id)): State<(Arc<Daemon>, u64)>,
    request: Request,
) -> Response {
    serve_rooted_forward(daemon, forward_id, request).await
}

/// One forward at the root of its own origin. The only dashboard route it
/// answers is minting that same forward's opening token, which a first
/// visit from Safari needs and which the browser cannot reach on the
/// dashboard's origin once the preview has its own.
async fn serve_rooted_forward(daemon: Arc<Daemon>, forward_id: u64, request: Request) -> Response {
    if request.method() == axum::http::Method::POST
        && request.uri().path() == crate::forward_route::token_path(forward_id)
    {
        return forward_token_response(&daemon, request.headers(), forward_id);
    }
    crate::forward_route::proxy_rooted(daemon, forward_id, request).await
}

fn dashboard_router(daemon: Arc<Daemon>) -> Router {
    Router::new()
        .merge(crate::connections::router())
        .route("/api/version", get(version))
        .route("/api/me", get(me))
        .route("/api/setup", post(setup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/user/password", post(change_password))
        .route("/api/fs", get(list_directory))
        .route("/api/project-host", get(project_host))
        .route("/api/harness", post(harness_status))
        .route("/api/sessions/{id}/reports", get(session_reports))
        .route("/api/items/{id}", get(get_legacy_item))
        .route(
            "/api/buckets/{bucket_id}/items/{id}/attachments",
            get(item_attachments).post(upload_item_attachment),
        )
        .route(
            "/api/buckets/{bucket_id}/items/{item_id}/attachments/{attachment_id}",
            get(download_item_attachment).delete(remove_item_attachment),
        )
        .route("/api/buckets/{bucket_id}/items/{id}", get(get_item))
        .route("/api/buckets/{bucket_id}/items/{id}/notes", get(item_notes))
        .route(
            "/api/items/{item_id}/attachments/{attachment_id}",
            get(download_legacy_item_attachment),
        )
        .route("/api/buckets/{id}/items", get(bucket_items))
        .route("/api/reviews/{id}", get(review_detail))
        .route("/api/reviews/{id}/diff", get(review_diff))
        .route("/api/reviews/{id}/tree", get(review_tree))
        .route("/api/reviews/{id}/file", get(review_file))
        .route("/api/plans/{id}", get(plan_detail))
        .route(
            "/api/plans/{plan_id}/decisions/{decision_id}/respond",
            post(submit_plan_response),
        )
        .route(
            "/api/plans/{plan_id}/decisions/respond",
            post(submit_plan_responses),
        )
        .route(
            "/api/plans/{plan_id}/decisions/{decision_id}/draft",
            put(save_plan_draft),
        )
        .route(
            "/api/plans/{plan_id}/decisions/drafts",
            put(save_plan_drafts),
        )
        .route("/api/plans/{id}/messages", post(post_plan_message))
        .route("/api/settings", get(list_settings))
        .route("/api/settings/{key}", put(update_setting))
        .route("/api/user/settings", get(list_user_settings))
        .route(
            "/api/user/settings/terminal-theme",
            put(update_user_terminal_theme).delete(reset_user_terminal_theme),
        )
        .route(
            "/api/user/settings/appearance",
            put(update_user_appearance).delete(reset_user_appearance),
        )
        .route(
            "/api/user/settings/ui-theme",
            put(update_user_ui_theme).delete(reset_user_ui_theme),
        )
        .route(
            "/api/user/settings/push-web-idle-minutes",
            put(update_push_web_idle_minutes),
        )
        .route("/api/user/activity", put(note_web_activity))
        .route("/api/buckets/{id}/briefings", get(bucket_briefings))
        .route(
            "/api/workspaces",
            get(list_workspaces).post(create_workspace),
        )
        .route("/api/workspaces/order", put(reorder_workspaces))
        .route(
            "/api/workspaces/{id}",
            put(update_workspace).delete(delete_workspace),
        )
        .route("/api/mobile/devices/enroll", post(mobile_enroll))
        .route("/api/mobile/devices/refresh", post(mobile_refresh))
        .route(
            "/api/mobile/devices/enroll-token",
            post(mint_mobile_enroll_token),
        )
        .route("/api/mobile/devices", get(list_mobile_devices))
        .route(
            "/api/mobile/devices/{id}",
            axum::routing::delete(revoke_mobile_device),
        )
        .route(WEB_TOKEN_PATH, post(mint_web_access_token))
        .route("/api/ws/ticket", post(mint_control_socket_ticket))
        .route(
            "/api/terminals/{id}/attach-ticket",
            post(mint_terminal_attach_ticket),
        )
        .route(
            "/api/mobile/devices/{id}/push",
            put(register_push_endpoint).delete(unregister_push_endpoint),
        )
        .route(
            "/api/mobile/devices/{id}/foreground",
            put(set_push_foreground),
        )
        .route(
            "/api/push/policies",
            get(list_push_policies).put(put_push_policy),
        )
        .route("/api/workers/enroll", post(enroll_worker))
        .route("/api/workers/{id}", axum::routing::delete(remove_worker))
        .route("/api/workers/{id}/reenroll", post(reenroll_worker))
        .route("/api/workers/{id}/update", post(update_worker))
        .route("/api/buckets/{id}/worker", post(set_bucket_worker))
        .route("/api/buckets/{id}/default", post(set_default_bucket))
        .route("/api/projects/{id}/worker", post(set_project_worker))
        .route(
            "/api/projects/{id}/worker-path",
            post(set_project_worker_path),
        )
        .route("/api/forwards/{id}/token", post(mint_forward_token))
        .route("/forwards/{id}", any(crate::forward_route::proxy))
        .route("/forwards/{id}/", any(crate::forward_route::proxy))
        .route("/forwards/{id}/{*path}", any(crate::forward_route::proxy))
        .route("/ws", any(ws_upgrade))
        .route("/ws/terminal/{id}", any(terminal_ws_upgrade))
        .route(
            "/mcp",
            post(crate::mcp::handle).layer(axum::extract::DefaultBodyLimit::max(
                crate::connections::MAX_HTTP_BODY_BYTES,
            )),
        )
        .fallback(get(static_assets))
        .with_state(daemon)
}

/// The worker plane. It is deliberately absent from the router above: these
/// routes carry process spawns and filesystem reads, so they are served only
/// on the mutually authenticated listener in `worker_plane`, never on the
/// port a reverse proxy terminates.
pub fn worker_router(daemon: Arc<Daemon>) -> Router {
    Router::new()
        .route("/worker", any(worker_ws_upgrade))
        .route("/worker/terminal", any(worker_terminal_upgrade))
        .route("/worker/transcript", any(worker_transcript_upgrade))
        .route("/worker/stream", any(worker_stream_upgrade))
        .with_state(daemon)
}

#[derive(serde::Deserialize)]
struct FsQuery {
    /// Directory to list; empty or absent lists the user's home.
    #[serde(default)]
    path: String,
    /// Worker whose filesystem to browse; absent or 0 is the local host.
    #[serde(default)]
    worker: u64,
}

/// Lists immediate subdirectories of a path for the project/cwd
/// pickers. Directories only; never returns file contents.
/// Everything the review page needs in one read: the review, its
/// threads with anchors resolved against the live tree, the revision
/// pairs behind the view picker, and this reader's own viewer state.
async fn review_detail(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.review_detail(id, user_id).await {
        Ok(d) => Json(serde_json::json!({
            "review": crate::review::review_json(&d.review),
            "threads": d.threads.iter().map(crate::review::thread_json).collect::<Vec<_>>(),
            "revisions": d.revisions.iter().map(crate::review::revision_json).collect::<Vec<_>>(),
            "viewer": crate::review::viewer_json(&d.viewer),
            "files": d.files,
            "skipped": d.skipped.iter().map(|(path, reason)| {
                serde_json::json!({ "path": path, "reason": reason })
            }).collect::<Vec<_>>(),
            "latest_rev": d.latest_rev,
            "pending_files": d.pending_files,
            "detached": d.detached,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn plan_detail(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    if authed_identity(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.plan_detail(id) {
        Ok(detail) => Json(detail).into_response(),
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn submit_plan_response(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path((plan_id, decision_id)): axum::extract::Path<(u64, u64)>,
    Json(body): Json<crate::plan::PlanResponseInput>,
) -> Response {
    if authed_identity(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let submission = crate::plan::PlanBatchResponseInput {
        responses: vec![crate::plan::PlanDecisionResponseInput {
            decision_id,
            response: body,
        }],
    };
    accept_plan_responses(daemon, plan_id, submission)
}

async fn submit_plan_responses(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(plan_id): axum::extract::Path<u64>,
    Json(body): Json<crate::plan::PlanBatchResponseInput>,
) -> Response {
    if authed_identity(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    accept_plan_responses(daemon, plan_id, body)
}

fn accept_plan_responses(
    daemon: Arc<Daemon>,
    plan_id: u64,
    body: crate::plan::PlanBatchResponseInput,
) -> Response {
    match daemon.accept_plan_responses(plan_id, body) {
        Ok((plan, session_id, message)) => {
            let delivery_daemon = daemon.clone();
            tokio::spawn(async move {
                let _ = delivery_daemon
                    .deliver_plan_responses(session_id, &message)
                    .await;
            });
            Json(serde_json::json!({
                "plan": crate::plan::plan_json(&plan),
                "deliveryState": "queued"
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn save_plan_draft(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path((plan_id, decision_id)): axum::extract::Path<(u64, u64)>,
    Json(body): Json<crate::plan::PlanResponseInput>,
) -> Response {
    if authed_identity(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let submission = crate::plan::PlanBatchDraftInput {
        drafts: vec![crate::plan::PlanDecisionDraftInput {
            decision_id,
            draft: body,
        }],
    };
    save_plan_drafts_response(daemon, plan_id, submission)
}

async fn save_plan_drafts(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(plan_id): axum::extract::Path<u64>,
    Json(body): Json<crate::plan::PlanBatchDraftInput>,
) -> Response {
    if authed_identity(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    save_plan_drafts_response(daemon, plan_id, body)
}

fn save_plan_drafts_response(
    daemon: Arc<Daemon>,
    plan_id: u64,
    body: crate::plan::PlanBatchDraftInput,
) -> Response {
    match daemon.save_plan_drafts(plan_id, body) {
        Ok(plan) => Json(serde_json::json!({
            "plan": crate::plan::plan_json(&plan),
            "status": "saved"
        }))
        .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanMessageBody {
    decision_id: Option<u64>,
    body: String,
}

async fn post_plan_message(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(plan_id): axum::extract::Path<u64>,
    Json(body): Json<PlanMessageBody>,
) -> Response {
    if authed_identity(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon
        .post_plan_user_message(plan_id, body.decision_id, &body.body)
        .await
    {
        Ok(message) => Json(message).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct DiffQuery {
    #[serde(default)]
    view: String,
    #[serde(default)]
    context: Option<usize>,
    #[serde(default)]
    file: Option<String>,
}

/// Names the snapshot a rendered view came from. A comment written
/// against that render sends the id back, so it anchors to the code
/// the reader was looking at rather than to the tree at POST time.
const REVIEW_SNAPSHOT_HEADER: &str = "x-review-snapshot";
/// The newest tree the daemon had observed when it rendered. The page
/// asks what has changed since this one, so a reader on a stored
/// revision is measured against the live tree rather than against the
/// revision they chose to read.
const REVIEW_LIVE_HEADER: &str = "x-review-live";

/// The headers a rendered review body is served under. The snapshot id
/// rides in a header because both bodies are plain text the client
/// reads as-is.
fn review_content_type(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("avif") => "image/avif",
        _ => "text/plain; charset=utf-8",
    }
}

/// Headers for bytes out of a reviewed worktree.
///
/// The content is a file somebody committed, so it is attacker-influenced on
/// any branch an agent fetched or a contributor pushed, and one entry in
/// [`review_content_type`] can execute: an SVG served as `image/svg+xml` runs
/// its own script on top-level navigation, which is the ordinary way to look at
/// an image diff closely. `Content-Disposition: attachment` makes that a
/// download instead, and browsers ignore it for a subresource load, so the diff
/// view's own `<img>` is unaffected. `nosniff` stops the same thing happening by
/// a guessed type. The item-attachment route two hundred lines down already did
/// both; this is the same answer.
fn review_render_headers(
    snapshot_id: Option<u64>,
    content_type: &'static str,
    filename: &str,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
    );
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        content_disposition(filename),
    );
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(id) = snapshot_id {
        if let Ok(value) = axum::http::HeaderValue::from_str(&id.to_string()) {
            headers.insert(
                axum::http::HeaderName::from_static(REVIEW_SNAPSHOT_HEADER),
                value,
            );
        }
    }
    headers
}

fn with_live_snapshot(mut headers: HeaderMap, live: Option<u64>) -> HeaderMap {
    if let Some(id) = live {
        if let Ok(value) = axum::http::HeaderValue::from_str(&id.to_string()) {
            headers.insert(
                axum::http::HeaderName::from_static(REVIEW_LIVE_HEADER),
                value,
            );
        }
    }
    headers
}

/// Unified diff text for a view. Served as plain text rather than JSON
/// because the payload is large and the client parses it as a diff.
async fn review_diff(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
    axum::extract::Query(q): axum::extract::Query<DiffQuery>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let context = q.context.unwrap_or(10).min(100_000);
    match daemon
        .review_diff(id, &q.view, context, q.file.as_deref())
        .await
    {
        Ok(view) => (
            with_live_snapshot(
                review_render_headers(
                    view.snapshot_id,
                    "text/plain; charset=utf-8",
                    q.file.as_deref().unwrap_or("diff"),
                ),
                daemon.latest_review_snapshot(id).ok().flatten(),
            ),
            view.content,
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct TreeQuery {
    /// The snapshot the caller was last shown, as the render named it.
    since: u64,
}

/// What has changed in the review's tree since the caller's last render.
/// The page asks on an interval while it is open, which is what tells a
/// reader that the worktree moved under a diff nothing else would
/// refetch.
async fn review_tree(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
    axum::extract::Query(q): axum::extract::Query<TreeQuery>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.review_tree_changes(id, q.since).await {
        Ok(changed) => Json(serde_json::json!({ "changed": changed })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct FileQuery {
    file: String,
    #[serde(default)]
    view: String,
    #[serde(default)]
    side: String,
}

/// A whole file on either side of a view.
async fn review_file(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
    axum::extract::Query(q): axum::extract::Query<FileQuery>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.review_file(id, &q.view, &q.file, &q.side).await {
        Ok(view) => match view.content {
            Some(bytes) => {
                if bytes.is_empty() && crate::review_repo::is_image_path(&q.file) {
                    StatusCode::NOT_FOUND.into_response()
                } else {
                    (
                        review_render_headers(
                            view.snapshot_id,
                            review_content_type(&q.file),
                            &q.file,
                        ),
                        bytes,
                    )
                        .into_response()
                }
            }
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Which project on which host the dashboard is asking about.
#[derive(serde::Deserialize)]
struct ProjectHostQuery {
    project: u64,
    /// Absent or 0 is the local host.
    #[serde(default)]
    worker: u64,
}

/// Whether a project can run on one host, so the dashboard can name the
/// config field to change instead of leaving a reachable host looking
/// like an outage.
async fn project_host(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<ProjectHostQuery>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let project = match daemon.get_project(q.project) {
        Ok(project) => project,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };
    match daemon.project_host_state(q.project, q.worker, None).await {
        Ok(state) => Json(daemon.project_host_json(&project, q.worker, &state)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn list_directory(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<FsQuery>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // A non-local worker's filesystem is browsed by proxying the listing
    // to that worker.
    if q.worker != pm_protocol::domain::LOCAL_WORKER_ID {
        return match daemon.worker_fs_list(q.worker, q.path.clone()).await {
            Ok(listing) if listing.ok => {
                let entries: Vec<serde_json::Value> = listing
                    .entries
                    .into_iter()
                    .map(|e| serde_json::json!({ "name": e.name, "path": e.path }))
                    .collect();
                Json(serde_json::json!({
                    "dir": listing.dir,
                    "parent": listing.parent,
                    "entries": entries,
                }))
                .into_response()
            }
            Ok(listing) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": listing.error })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
        };
    }
    if !daemon.local_worker_enabled() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "local worker is disabled for this daemon" })),
        )
            .into_response();
    }
    let base = if q.path.trim().is_empty() {
        dirs_home()
    } else {
        std::path::PathBuf::from(&q.path)
    };
    let dir = if base.is_dir() {
        base.clone()
    } else {
        base.parent().map(|p| p.to_path_buf()).unwrap_or(base)
    };
    let mut entries: Vec<serde_json::Value> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| {
                serde_json::json!({
                    "name": e.file_name().to_string_lossy(),
                    "path": e.path().to_string_lossy(),
                })
            })
            .collect(),
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Json(serde_json::json!({
        "dir": dir.to_string_lossy(),
        "parent": dir.parent().map(|p| p.to_string_lossy().to_string()),
        "entries": entries,
    }))
    .into_response()
}

async fn session_reports(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.activity_reports(id) {
        Ok(reports) => {
            let items: Vec<serde_json::Value> = reports
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "tsUnixMs": r.ts_unix_ms,
                        "kind": r.kind,
                        "payload": serde_json::from_str::<serde_json::Value>(&r.payload)
                            .unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect();
            Json(serde_json::json!({ "reports": items })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn item_notes(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path((bucket_id, id)): axum::extract::Path<(u64, u64)>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.item_notes(bucket_id, id) {
        Ok(notes) => {
            let notes: Vec<serde_json::Value> =
                notes.iter().map(crate::daemon::item_note_json).collect();
            Json(serde_json::json!({ "notes": notes })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

fn attachment_json(attachment: &pm_protocol::domain::ItemAttachment) -> serde_json::Value {
    serde_json::json!({
        "id": attachment.id.to_string(),
        "bucketId": attachment.bucket_id.to_string(),
        "itemId": attachment.item_id.to_string(),
        "filename": attachment.filename,
        "mediaType": attachment.media_type,
        "byteLength": attachment.byte_length.to_string(),
        "sha256": attachment.sha256,
        "createdAtUnixMs": attachment.created_at_unix_ms.to_string(),
        "createdBySessionId": attachment.created_by_session_id.map(|id| id.to_string()),
    })
}

async fn item_attachments(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path((bucket_id, id)): Path<(u64, u64)>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.get_item(bucket_id, id) {
        Ok(_) => {}
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
    match daemon.list_item_attachments(bucket_id, id) {
        Ok(attachments) => Json(serde_json::json!({
            "attachments": attachments.iter().map(attachment_json).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct AttachmentUploadQuery {
    filename: String,
    media_type: Option<String>,
}

async fn upload_item_attachment(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path((bucket_id, id)): Path<(u64, u64)>,
    Query(query): Query<AttachmentUploadQuery>,
    body: Body,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > crate::storage::ITEM_ATTACHMENT_FILE_MAX)
    {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": format!("attachment exceeds the {}-byte limit", crate::storage::ITEM_ATTACHMENT_FILE_MAX) })),
        ).into_response();
    }
    match daemon.get_item(bucket_id, id) {
        Ok(_) => {}
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
    let content = match axum::body::to_bytes(body, crate::storage::ITEM_ATTACHMENT_FILE_MAX + 1).await {
        Ok(content) if content.len() <= crate::storage::ITEM_ATTACHMENT_FILE_MAX => content,
        Ok(_) | Err(_) => return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": format!("attachment exceeds the {}-byte limit", crate::storage::ITEM_ATTACHMENT_FILE_MAX) })),
        ).into_response(),
    };
    let media_type = query.media_type.as_deref().or_else(|| {
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
    });
    match daemon.attach_item_bytes(bucket_id, id, &query.filename, media_type, &content, None) {
        Ok(attachment) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "attachment": attachment_json(&attachment) })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

fn content_disposition(filename: &str) -> HeaderValue {
    let fallback: String = filename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || " ._-()".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect();
    let encoded = filename
        .as_bytes()
        .iter()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(byte) {
                (*byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment; filename=\"attachment\""))
}

async fn download_item_attachment(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path((bucket_id, item_id, attachment_id)): Path<(u64, u64, u64)>,
) -> Response {
    if !authed_for_download(&daemon, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.get_item(bucket_id, item_id) {
        Ok(_) => {}
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
    match daemon.get_item_attachment(bucket_id, item_id, attachment_id) {
        Ok(attachment) => {
            let mut response = Body::from(attachment.content).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&attachment.metadata.media_type)
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );
            response.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                content_disposition(&attachment.metadata.filename),
            );
            response.headers_mut().insert(
                "x-content-type-options",
                HeaderValue::from_static("nosniff"),
            );
            response
        }
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn remove_item_attachment(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path((bucket_id, item_id, attachment_id)): Path<(u64, u64, u64)>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.get_item(bucket_id, item_id) {
        Ok(_) => {}
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
    match daemon.delete_item_attachment(bucket_id, item_id, attachment_id) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

fn item_json(item: &Item) -> serde_json::Value {
    serde_json::json!({
        "id": item.id.to_string(), "bucket_id": item.bucket_id.to_string(),
        "ref": format!("pm:item/{}/{}", item.bucket_id, item.id),
        "project_id": item.project_id.map(|id| id.to_string()),
        "external_key": item.external_key, "title": item.title, "body": item.body,
        "question": item.question, "status": item.status.as_str(),
        "priority": item.priority.as_str(), "source_kind": item.source_kind.as_str(),
        "source_detail": item.source_detail, "url": item.url,
        "due_at_unix_ms": item.due_at_unix_ms.map(|value| value.to_string()),
        "snoozed_until_unix_ms": item.snoozed_until_unix_ms.map(|value| value.to_string()),
        "created_by_session_id": item.created_by_session_id.map(|id| id.to_string()),
        "created_at_unix_ms": item.created_at_unix_ms.to_string(),
        "updated_at_unix_ms": item.updated_at_unix_ms.to_string(),
        "done_at_unix_ms": item.done_at_unix_ms.map(|value| value.to_string()),
        "blocked_by": item.blocked_by.iter().map(u64::to_string).collect::<Vec<_>>(),
        "session_ids": item.session_ids.iter().map(u64::to_string).collect::<Vec<_>>(),
    })
}

async fn get_item(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path((bucket_id, id)): Path<(u64, u64)>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.get_item(bucket_id, id) {
        Ok(item) => Json(serde_json::json!({ "item": item_json(&item) })).into_response(),
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

/// Pre-v25 global ids are deliberately not resolvable through the public API.
async fn get_legacy_item(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(_id): Path<u64>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "error": "legacy unqualified item ids are unsupported; use /api/buckets/{bucket_id}/items/{item_number}"
        })),
    ).into_response()
}

async fn download_legacy_item_attachment(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path((_legacy_item_id, _attachment_id)): Path<(u64, u64)>,
) -> Response {
    if !authed_for_download(&daemon, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "error": "legacy unqualified attachment URLs are unsupported; use a bucket-qualified item URL"
        })),
    ).into_response()
}

#[derive(Default, serde::Deserialize)]
struct ItemListParams {
    q: Option<String>,
    project: Option<u64>,
    status: Option<String>,
    priority: Option<String>,
    source: Option<String>,
    done: Option<bool>,
    snoozed: Option<bool>,
    limit: Option<u32>,
    offset: Option<u32>,
    summary: Option<String>,
}

fn parse_csv<T>(
    value: Option<&str>,
    name: &str,
    parse: fn(&str) -> Option<T>,
) -> Result<Vec<T>, String> {
    value
        .unwrap_or_default()
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| parse(part).ok_or_else(|| format!("unknown {name} {part:?}")))
        .collect()
}

async fn bucket_items(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Query(params): Query<ItemListParams>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let statuses = match parse_csv(params.status.as_deref(), "status", ItemStatus::parse) {
        Ok(values) => values,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
    };
    let priorities = match parse_csv(params.priority.as_deref(), "priority", ItemPriority::parse) {
        Ok(values) => values,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
    };
    let source_kinds = match parse_csv(params.source.as_deref(), "source", ItemSourceKind::parse) {
        Ok(values) => values,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
    };
    let summary_filter = match params.summary.as_deref() {
        Some(value) => match ItemSummaryFilter::parse(value) {
            Some(filter) => Some(filter),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("unknown summary {value:?}") })),
                )
                    .into_response()
            }
        },
        None => None,
    };
    let limit = params
        .limit
        .unwrap_or(crate::storage::ITEM_QUERY_LIMIT_DEFAULT)
        .clamp(1, 100);
    let offset = params.offset.unwrap_or(0);
    let query = ItemQuery {
        bucket_id: id,
        statuses,
        search: params
            .q
            .map(|value| value.trim().chars().take(200).collect())
            .filter(|value: &String| !value.is_empty()),
        project_id: params.project,
        priorities,
        source_kinds,
        updated_since_unix_ms: None,
        // Board history is searchable by default. Callers that need an
        // active-only view must opt out explicitly with `done=false`.
        include_closed: params.done.unwrap_or(true),
        include_snoozed: params.snoozed.unwrap_or(false),
        limit: Some(limit + 1),
        offset,
        summary_filter,
    };
    match daemon.list_items_with_counts(&query) {
        Ok((mut items, counts)) => {
            let has_more = items.len() > limit as usize;
            items.truncate(limit as usize);
            let items = items.iter().map(item_json).collect::<Vec<_>>();
            Json(serde_json::json!({
                "items": items,
                "nextOffset": has_more.then_some(offset.saturating_add(limit)),
                "counts": {
                    "bucketTotal": counts.bucket_total,
                    "matchingTotal": counts.matching_total,
                    "byStatus": counts.status_counts.into_iter().map(|(status, count)|
                        (status.as_str(), count)
                    ).collect::<std::collections::BTreeMap<_, _>>(),
                },
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

/// How many briefings the history endpoint returns at most.
const BRIEFING_HISTORY_LIMIT: usize = 50;

async fn bucket_briefings(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.briefings(id, BRIEFING_HISTORY_LIMIT) {
        Ok(briefings) => {
            let briefings: Vec<serde_json::Value> = briefings
                .into_iter()
                .map(|b| {
                    serde_json::json!({
                        "id": b.id,
                        "bucketId": b.bucket_id,
                        "sessionId": b.session_id,
                        "tsUnixMs": b.ts_unix_ms,
                        "markdown": b.markdown,
                    })
                })
                .collect();
            Json(serde_json::json!({ "briefings": briefings })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn list_settings(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.settings() {
        Ok(settings) => Json(serde_json::json!({ "settings": settings })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct SettingBody {
    /// The new value; null resets the setting to its default.
    value: Option<String>,
}

async fn update_setting(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Json(body): Json<SettingBody>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.set_setting(&key, body.value.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn list_user_settings(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let settings = daemon.user_settings(user_id);
    let stored = |key: &str| {
        settings
            .iter()
            .find(|setting| setting.key == key)
            .and_then(|setting| serde_json::from_str::<serde_json::Value>(&setting.value_json).ok())
    };
    Json(serde_json::json!({
        "terminalTheme": stored(crate::terminal_theme::USER_TERMINAL_THEME_KEY),
        "appearance": stored(crate::appearance::USER_APPEARANCE_KEY),
        "uiTheme": stored(crate::ui_theme::USER_UI_THEME_KEY),
        "pushWebIdleMinutes": stored(crate::push::SETTING_PUSH_WEB_IDLE_MINUTES),
    }))
    .into_response()
}

/// Records that this user just interacted with the web UI, which holds
/// back push to their devices for as long as their idle threshold says.
/// The browser reports interaction, never that a tab is merely open.
async fn note_web_activity(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    daemon.note_web_activity(user_id);
    StatusCode::NO_CONTENT.into_response()
}

async fn update_push_web_idle_minutes(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.set_push_web_idle_minutes(user_id, Some(&body)) {
        Ok(Some(normalized)) => match normalized.parse::<i64>() {
            Ok(minutes) => {
                Json(serde_json::json!({ "pushWebIdleMinutes": minutes })).into_response()
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(None) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn update_user_terminal_theme(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if body.len() > crate::terminal_theme::TERMINAL_THEME_MAX_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": "theme exceeds the 64 KiB import limit" })),
        )
            .into_response();
    }
    match daemon.set_user_terminal_theme(user_id, Some(&body)) {
        Ok(Some(normalized)) => match serde_json::from_str::<serde_json::Value>(&normalized) {
            Ok(theme) => Json(serde_json::json!({ "terminalTheme": theme })).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(None) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn update_user_appearance(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if body.len() > crate::appearance::APPEARANCE_MAX_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": "appearance value is too large" })),
        )
            .into_response();
    }
    match daemon.set_user_appearance(user_id, Some(&body)) {
        Ok(Some(normalized)) => match serde_json::from_str::<serde_json::Value>(&normalized) {
            Ok(appearance) => Json(serde_json::json!({ "appearance": appearance })).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(None) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn reset_user_appearance(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.set_user_appearance(user_id, None) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn update_user_ui_theme(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if body.len() > crate::ui_theme::UI_THEME_MAX_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": "ui theme value is too large" })),
        )
            .into_response();
    }
    match daemon.set_user_ui_theme(user_id, Some(&body)) {
        Ok(Some(normalized)) => match serde_json::from_str::<serde_json::Value>(&normalized) {
            Ok(ui_theme) => Json(serde_json::json!({ "uiTheme": ui_theme })).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(None) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn reset_user_ui_theme(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.set_user_ui_theme(user_id, None) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn reset_user_terminal_theme(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.set_user_terminal_theme(user_id, None) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct WorkspaceBody {
    name: String,
    layout: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct WorkspaceOrderBody {
    workspace_ids: Vec<u64>,
}

fn validate_workspace(body: &WorkspaceBody) -> Result<(String, String), String> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > WORKSPACE_NAME_MAX_CHARS {
        return Err(format!(
            "workspace name must be between 1 and {WORKSPACE_NAME_MAX_CHARS} characters"
        ));
    }
    if !body.layout.is_object() {
        return Err("workspace layout must be an object".into());
    }
    let layout = serde_json::to_string(&body.layout).map_err(|e| e.to_string())?;
    if layout.len() > WORKSPACE_LAYOUT_MAX_BYTES {
        return Err("workspace layout is too large".into());
    }
    Ok((name.to_string(), layout))
}

fn workspace_json(workspace: crate::storage::Workspace) -> serde_json::Value {
    serde_json::json!({
        "id": workspace.id,
        "name": workspace.name,
        "layout": serde_json::from_str::<serde_json::Value>(&workspace.layout_json)
            .unwrap_or(serde_json::Value::Null),
        "createdAtUnixMs": workspace.created_at_unix_ms,
        "updatedAtUnixMs": workspace.updated_at_unix_ms,
        "position": workspace.position,
    })
}

async fn list_workspaces(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.list_workspaces(user_id) {
        Ok(items) => Json(serde_json::json!({
            "workspaces": items.into_iter().map(workspace_json).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn reorder_workspaces(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(body): Json<WorkspaceOrderBody>,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.reorder_workspaces(user_id, &body.workspace_ids) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn create_workspace(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(body): Json<WorkspaceBody>,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (name, layout) = match validate_workspace(&body) {
        Ok(valid) => valid,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
    };
    match daemon.create_workspace(user_id, &name, &layout) {
        Ok(workspace) => (StatusCode::CREATED, Json(workspace_json(workspace))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn update_workspace(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
    Json(body): Json<WorkspaceBody>,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (name, layout) = match validate_workspace(&body) {
        Ok(valid) => valid,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
    };
    match daemon.update_workspace(user_id, id, &name, &layout) {
        Ok(workspace) => Json(workspace_json(workspace)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn delete_workspace(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.delete_workspace(user_id, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct EnrollRequest {
    #[serde(default)]
    label: String,
    /// Which end opens the connection: "dial" (the host connects, the
    /// default) or "accept" (the controller connects to `endpoint`).
    #[serde(default)]
    connect_mode: String,
    /// host:port the controller dials. Required for an accepting host.
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    bucket_ids: Vec<u64>,
}

/// Mints an enrollment token for a host that is already known, rotating its
/// credential and pinned key without disturbing what points at it.
/// Applies a worker's pending update without waiting for it to go idle.
/// The caller has been told the agents on that host are restarted and
/// resumed, and that an in-flight turn does not survive the restart.
async fn update_worker(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.force_worker_update(id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Default, serde::Deserialize)]
struct ReenrollRequest {
    /// Which end opens the connection, restated. Omitted, the host keeps
    /// what it has.
    #[serde(default)]
    connect_mode: String,
    #[serde(default)]
    endpoint: String,
}

async fn reenroll_worker(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    body: Option<Json<ReenrollRequest>>,
) -> Response {
    // Redeeming this token rebinds the host's pinned key, so it hands an
    // existing trusted host to whichever machine spends it, keeping that
    // host's id, name, project paths and bucket defaults. A signed-in
    // session authorizes it, as it does removing the host outright.
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let req = body.map(|Json(req)| req).unwrap_or_default();
    let minted = if req.connect_mode.is_empty() {
        daemon.create_worker_reenrollment(id)
    } else {
        match connect_mode(&req.connect_mode) {
            Ok(mode) => daemon.create_worker_reenrollment_with_mode(id, mode, &req.endpoint),
            Err(message) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": message })),
                )
                    .into_response()
            }
        }
    };
    match minted {
        Ok((token, expires_at)) => {
            Json(serde_json::json!({ "token": token, "expiresAtUnixMs": expires_at }))
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Mints a one-time worker enrollment token. The operator runs
/// `pm worker --token <token>` on the machine to join it.
async fn enroll_worker(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(req): Json<EnrollRequest>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let minted = match connect_mode(&req.connect_mode) {
        Ok(mode) => daemon.create_worker_enrollment_with_buckets(
            &req.label,
            mode,
            &req.endpoint,
            &req.bucket_ids,
        ),
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": message })),
            )
                .into_response();
        }
    };
    match minted {
        Ok((token, expires_at)) => {
            Json(serde_json::json!({ "token": token, "expiresAtUnixMs": expires_at }))
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// An absent mode means the established one: the host dials the controller.
fn connect_mode(text: &str) -> Result<pm_protocol::domain::ConnectMode, String> {
    if text.is_empty() {
        return Ok(pm_protocol::domain::ConnectMode::Dial);
    }
    pm_protocol::domain::ConnectMode::parse(text)
        .ok_or_else(|| format!("unknown connection mode {text:?}, expected \"dial\" or \"accept\""))
}

async fn remove_worker(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.remove_worker(id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct WorkerAssignment {
    /// Target worker; 0 is the local worker. For a project, null clears
    /// the override so it inherits the bucket default.
    worker_id: Option<u64>,
    #[serde(default)]
    allowed_worker_ids: Vec<u64>,
    /// Where projects pinned to a worker this removes from a bucket are
    /// moved. Absent moves them to the bucket's new default worker.
    /// Ignored for a project assignment.
    #[serde(default)]
    replacement_worker_id: Option<u64>,
}

async fn set_bucket_worker(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
    Json(body): Json<WorkerAssignment>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.set_bucket_workers(
        id,
        &body.allowed_worker_ids,
        body.worker_id.unwrap_or(0),
        body.replacement_worker_id,
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn set_project_worker(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
    Json(body): Json<WorkerAssignment>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.set_project_workers(id, &body.allowed_worker_ids, body.worker_id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct WorkerPathUpdate {
    /// Target worker, as a worker id or name. Must be one of the
    /// project's allowed workers.
    worker: serde_json::Value,
    /// Absent or empty clears the entry so spawns on that worker fall
    /// back to the project's configured path.
    path: Option<String>,
}

async fn set_project_worker_path(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
    Json(body): Json<WorkerPathUpdate>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let worker = match &body.worker {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "worker must be a worker id or name" })),
            )
                .into_response()
        }
    };
    match daemon
        .resolve_project_path_worker(id, &worker)
        .and_then(|worker_id| daemon.set_project_worker_path(id, worker_id, body.path.as_deref()))
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn set_default_bucket(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<u64>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.set_default_bucket(id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
}

/// The session cookie's value, or `None` when the request carries more than
/// one of them.
///
/// A cookie is keyed by name, domain and path, and the port is no part of
/// that, so a page sharing the dashboard's host can set a second `pm_session`
/// the browser then sends alongside the real one. Taking the first would let
/// that page choose which session the user is in, so an ambiguous jar is
/// refused rather than resolved.
fn session_cookie_value(headers: &HeaderMap) -> Option<String> {
    let mut found: Option<String> = None;
    for header in headers.get_all(header::COOKIE) {
        let Ok(text) = header.to_str() else { continue };
        for part in text.split(';') {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            if name != SESSION_COOKIE {
                continue;
            }
            if found.is_some() {
                warn!(
                    "refusing a request that carries more than one {SESSION_COOKIE} cookie: \
                     something sharing this host's cookie jar set one of them"
                );
                return None;
            }
            found = Some(value.to_string());
        }
    }
    found
}

fn session_cookie(value: &str, max_age_secs: i64) -> String {
    format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}")
}

pub(crate) fn authed_user(daemon: &Daemon, headers: &HeaderMap) -> Option<String> {
    authed_identity(daemon, headers).map(|(_, username)| username)
}

/// Resolves the request's user from its bearer access token.
///
/// The session cookie does not answer here, and that is the whole of this
/// change: a browser attaches the cookie to any request to a site it shares
/// with the dashboard, so a route that reads it is a route a preview can
/// drive. A bearer token was held by whoever sent it. The cookie's remaining
/// job is being served the shell and minting one of these, plus the two
/// keyboard-present routes that take the account password as well.
pub(crate) fn authed_identity(daemon: &Daemon, headers: &HeaderMap) -> Option<(u64, String)> {
    let (user_id, username, _) = authed_token_holder(daemon, headers)?;
    Some((user_id, username))
}

/// Resolves the signed-in cookie session, for the routes the cookie is still a
/// credential for.
fn authed_cookie_session(daemon: &Daemon, headers: &HeaderMap) -> Option<(u64, String)> {
    session_cookie_value(headers).and_then(|token| daemon.auth_verify_user(&token))
}

/// Whether a request may be served a file the browser is navigating to.
///
/// A navigation sets no `Authorization` header, so a download link takes the
/// cookie or stops working. What that costs is bounded, which is why it is this
/// route and not the API: the response carries `Content-Disposition: attachment`
/// and `nosniff`, and a page on another origin cannot read a response it has no
/// CORS grant for. So a neighbour sharing this cookie jar can cause a download
/// and not a read.
fn authed_for_download(daemon: &Daemon, headers: &HeaderMap) -> bool {
    authed_identity(daemon, headers).is_some() || authed_cookie_session(daemon, headers).is_some()
}

/// The user a bearer token speaks for, and the device holding it when a phone
/// sent it. The dashboard's own token names no device, which is what the
/// device-scoped routes read to know a request is not pinned to one.
fn authed_token_holder(daemon: &Daemon, headers: &HeaderMap) -> Option<(u64, String, Option<u64>)> {
    daemon.access_token_verify(&bearer_token(headers)?)
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?.trim();
    (!token.is_empty()).then(|| token.to_string())
}

#[derive(serde::Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

/// Build metadata plus installation identity and mobile capability
/// advertisement; no auth required so clients can check compatibility
/// before logging in.
async fn version(State(daemon): State<Arc<Daemon>>) -> Response {
    // Push needs a relay to deliver through; the app reads this to know
    // whether registering an endpoint is worth anything.
    let push_configured = daemon.push_gateway_url().is_some();
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "gitRev": env!("PM_GIT_REV"),
        // The channel `pm update` on this host follows, which is what the
        // newest-version fields below are relative to.
        "channel": daemon.release_channel().unwrap_or(crate::update::STABLE_CHANNEL),
        "installationId": daemon.installation_id(),
        // What is published, not what will be installed: updating the
        // daemon would end every session it is holding.
        "latestVersion": daemon.latest_release.get().map(|release| release.version),
        "updateAvailable": daemon.latest_release.upgrade_available(),
        // Hosts connect to their own listener, not this one. The port is
        // all the controller can honestly report: the name a host reaches
        // it by is the operator's to state.
        "hostPlanePort": daemon.worker_plane_addr().map(|addr| addr.port()),
        // The base URL the operator configured, when they did, which is
        // the name a remote worker reaches this controller by.
        "publicUrl": daemon.public_url(),
        // Where this build installs pm from, so the dashboard can show
        // the install command without a second copy of the address that
        // would drift from the one the binary actually uses.
        // The dashboard defaults the add-Worker platform to this one,
        // because a Worker on this machine is the common case and the
        // controller is the only thing that knows which OS that is.
        "platform": std::env::consts::OS,
        "installCommand": format!(
            "curl -fsSL {}/install.sh | sh",
            crate::update::RELEASE_BASE_URL
        ),
        // Startup configuration, so an operator can see which mount
        // published forward URLs use without reading the daemon's
        // environment.
        "forwardMount": forward_mount_json(&daemon.forward_mount_mode()),
        "mobile": {
            "deviceAuth": true,
            "socketTickets": true,
            "push": push_configured,
        },
    }))
    .into_response()
}

/// The active forward mount, read-only: its name plus whichever value
/// selected it, so an operator can confirm the configuration took.
fn forward_mount_json(mode: &crate::forward_mount::MountMode) -> serde_json::Value {
    use crate::forward_mount::MountMode;
    let mut mount = serde_json::Map::new();
    mount.insert("mode".into(), mode.as_str().into());
    match mode {
        MountMode::ShareDomain(domain) => {
            mount.insert("shareDomain".into(), domain.as_str().into());
        }
        MountMode::PerForwardPort { lo, hi } => {
            mount.insert("sharePortRange".into(), format!("{lo}-{hi}").into());
        }
        MountMode::PathPrefix => {}
    }
    mount.into()
}

fn mobile_device_json(device: &crate::storage::MobileDevice) -> serde_json::Value {
    serde_json::json!({
        "id": device.id.to_string(),
        "name": device.name,
        "platform": device.platform,
        "appInstallationId": device.app_installation_id,
        "createdAtUnixMs": device.created_at_unix_ms,
        "lastSeenAtUnixMs": device.last_seen_at_unix_ms,
        "revokedAtUnixMs": device.revoked_at_unix_ms,
    })
}

fn mobile_tokens_json(tokens: &crate::mobile::MobileTokens) -> serde_json::Value {
    serde_json::json!({
        "accessToken": tokens.access_token,
        "accessTokenExpiresAtUnixMs": tokens.access_expires_at_unix_ms,
        "refreshToken": tokens.refresh_token,
        "refreshTokenExpiresAtUnixMs": tokens.refresh_expires_at_unix_ms,
    })
}

fn mobile_auth_error_response(e: crate::mobile::MobileAuthError) -> Response {
    use crate::mobile::MobileAuthError;
    let status = match e {
        MobileAuthError::InvalidCredentials
        | MobileAuthError::InvalidEnrollment(_)
        | MobileAuthError::InvalidToken => StatusCode::UNAUTHORIZED,
        MobileAuthError::Rejected(_) => StatusCode::BAD_REQUEST,
        MobileAuthError::DeviceNotFound | MobileAuthError::TerminalNotFound => {
            StatusCode::NOT_FOUND
        }
        MobileAuthError::StaleGeneration => StatusCode::CONFLICT,
        MobileAuthError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MobileEnrollBody {
    /// App-generated stable id for this installation.
    device_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    platform: String,
    username: Option<String>,
    password: Option<String>,
    enroll_token: Option<String>,
}

/// Enrolls a mobile device with either a username/password login or a
/// one-use enrollment token; both stay in the request body so no
/// credential touches a URL.
async fn mobile_enroll(
    State(daemon): State<Arc<Daemon>>,
    Json(body): Json<MobileEnrollBody>,
) -> Response {
    let proof = match (&body.username, &body.password, &body.enroll_token) {
        (Some(username), Some(password), None) => crate::mobile::MobileEnrollProof::Password {
            username: username.clone(),
            password: password.clone(),
        },
        (None, None, Some(token)) => crate::mobile::MobileEnrollProof::EnrollToken(token.clone()),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "provide either username and password or enrollToken"
                })),
            )
                .into_response()
        }
    };
    let request = crate::mobile::MobileEnrollRequest {
        proof,
        app_installation_id: body.device_id,
        name: body.name,
        platform: body.platform,
    };
    match daemon.mobile_enroll(request) {
        Ok(enrollment) => Json(serde_json::json!({
            "device": mobile_device_json(&enrollment.device),
            "tokens": mobile_tokens_json(&enrollment.tokens),
            "installationId": daemon.installation_id(),
        }))
        .into_response(),
        Err(e) => mobile_auth_error_response(e),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MobileRefreshBody {
    refresh_token: String,
}

async fn mobile_refresh(
    State(daemon): State<Arc<Daemon>>,
    Json(body): Json<MobileRefreshBody>,
) -> Response {
    match daemon.mobile_refresh(&body.refresh_token) {
        Ok((tokens, device_id)) => Json(serde_json::json!({
            "tokens": mobile_tokens_json(&tokens),
            "deviceId": device_id.to_string(),
        }))
        .into_response(),
        Err(e) => mobile_auth_error_response(e),
    }
}

/// Mints a one-use mobile enrollment token.
async fn mint_mobile_enroll_token(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.create_mobile_enrollment_token(user_id) {
        Ok((token, expires_at)) => Json(serde_json::json!({
            "token": token,
            "expiresAtUnixMs": expires_at,
        }))
        .into_response(),
        Err(e) => mobile_auth_error_response(e),
    }
}

/// Mints a short-lived token scoped to one forward, used by the iOS
/// app to open a forward in the native browser with auth.
async fn mint_forward_token(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(forward_id): Path<u64>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Some(destination) = query.get("destination") {
        let valid = daemon
            .storage
            .session_forward(forward_id)
            .ok()
            .and_then(|forward| {
                daemon.forward_mount(
                    &forward,
                    headers.get(header::HOST).and_then(|h| h.to_str().ok()),
                )
            })
            .is_some_and(|mount| mount.accepts_destination(destination));
        if !valid {
            return StatusCode::BAD_REQUEST.into_response();
        }
    }
    forward_token_response(&daemon, &headers, forward_id)
}

fn forward_token_response(daemon: &Arc<Daemon>, headers: &HeaderMap, forward_id: u64) -> Response {
    let Some((user_id, username)) = authed_identity(daemon, headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if daemon.storage.session_forward(forward_id).is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let token = daemon.mint_forward_token(user_id, username, forward_id);
    let expires_in_ms = crate::forward::FORWARD_TOKEN_TTL.as_millis() as u64;
    Json(serde_json::json!({
        "token": token,
        "expiresInMs": expires_in_ms,
    }))
    .into_response()
}

async fn list_mobile_devices(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.list_mobile_devices(user_id) {
        Ok(devices) => Json(serde_json::json!({
            "devices": devices.iter().map(mobile_device_json).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(e) => mobile_auth_error_response(e),
    }
}

async fn revoke_mobile_device(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    let Some((user_id, _)) = authed_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.revoke_mobile_device(user_id, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => mobile_auth_error_response(e),
    }
}

/// Resolves the token holder allowed to mint a socket ticket. A ticket is how
/// anything that cannot set a header on a handshake authenticates one, which is
/// the phone's terminal WebView and now the dashboard itself.
fn authed_ticket_minter(daemon: &Daemon, headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let (user_id, _, device_id) = authed_token_holder(daemon, headers)?;
    Some((user_id, device_id))
}

/// Where the dashboard exchanges its session cookie for an access token.
pub const WEB_TOKEN_PATH: &str = "/api/web/token";

/// Mints the dashboard's access token from its session cookie.
///
/// The only route the cookie still authenticates by itself, so this is where
/// the exactness of the origin comparison earns its keep. A page sharing this
/// site's cookie jar reaches this handler with the cookie attached, and what
/// separates it from the dashboard is the origin it was opened from, including
/// its port. `guard_cookie_mutations` would refuse it too, but this route is
/// the hinge the rest of the design hangs from and does not rest on a layer
/// being registered.
///
/// The token goes back in the body and never into a `Set-Cookie`: a credential
/// the browser attaches by itself is the thing being removed.
async fn mint_web_access_token(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    if !crate::web_origin::mutation_is_own_origin(&headers, daemon.public_url()) {
        return refuse_foreign_origin(WEB_TOKEN_PATH, &headers);
    }
    let Some(session_value) = session_cookie_value(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some((user_id, _)) = daemon.auth_verify_user(&session_value) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.issue_web_access_token(user_id, &session_value) {
        Ok((token, expires_at)) => Json(serde_json::json!({
            "accessToken": token,
            "expiresAtUnixMs": expires_at,
        }))
        .into_response(),
        Err(e) => mobile_auth_error_response(e),
    }
}

fn socket_ticket_json(ticket: String, expires_at_unix_ms: i64) -> Response {
    Json(serde_json::json!({
        "ticket": ticket,
        "expiresAtUnixMs": expires_at_unix_ms,
    }))
    .into_response()
}

/// Exchanges a bearer access token for the one-use socket ticket the
/// `/ws` upgrade consumes.
async fn mint_control_socket_ticket(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
) -> Response {
    let Some((user_id, device_id)) = authed_ticket_minter(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.create_control_socket_ticket(user_id, device_id) {
        Ok((ticket, expires_at)) => socket_ticket_json(ticket, expires_at),
        Err(e) => mobile_auth_error_response(e),
    }
}

/// Like `authed_identity`, but also reports which device the bearer token
/// belongs to so device-scoped routes can pin a device to its own resources.
/// The dashboard's token names no device and is pinned to none.
fn authed_device_identity(daemon: &Daemon, headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    authed_ticket_minter(daemon, headers)
}

fn push_endpoint_json(endpoint: &crate::storage::PushEndpoint) -> serde_json::Value {
    use crate::push::PushEventClass;
    serde_json::json!({
        "deviceId": endpoint.device_id.to_string(),
        "environment": endpoint.environment,
        "locale": endpoint.locale,
        "previewsEnabled": endpoint.previews_enabled,
        "events": {
            "needsInput": endpoint.event_mask & PushEventClass::NeedsInput.mask() != 0,
            "failed": endpoint.event_mask & PushEventClass::Failed.mask() != 0,
            "completed": endpoint.event_mask & PushEventClass::Completed.mask() != 0,
        },
        "disabled": endpoint.disabled_at_unix_ms.is_some(),
        "disabledReason": endpoint.disabled_reason,
        "updatedAtUnixMs": endpoint.updated_at_unix_ms,
    })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushEventsBody {
    #[serde(default = "default_true")]
    needs_input: bool,
    #[serde(default = "default_true")]
    failed: bool,
    #[serde(default = "default_true")]
    completed: bool,
}

fn default_true() -> bool {
    true
}

fn default_environment() -> String {
    pm_push::ENVIRONMENT_PRODUCTION.to_string()
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushRegisterBody {
    token: String,
    #[serde(default = "default_environment")]
    environment: String,
    #[serde(default)]
    locale: String,
    #[serde(default)]
    previews_enabled: bool,
    events: Option<PushEventsBody>,
    /// X25519 public key (base64), for HPKE sealing in gateway mode.
    #[serde(default)]
    public_key: String,
}

/// Registers or rotates a device's push endpoint. A bearer token may
/// only register for its own device; the platform token stays in the
/// body and is stored sealed.
async fn register_push_endpoint(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<PushRegisterBody>,
) -> Response {
    use crate::push::PushEventClass;
    let Some((user_id, bearer_device)) = authed_device_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if bearer_device.is_some_and(|device_id| device_id != id) {
        return mobile_auth_error_response(crate::mobile::MobileAuthError::DeviceNotFound);
    }
    let events = body.events.unwrap_or(PushEventsBody {
        needs_input: true,
        failed: true,
        completed: true,
    });
    let mut event_mask = 0;
    if events.needs_input {
        event_mask |= PushEventClass::NeedsInput.mask();
    }
    if events.failed {
        event_mask |= PushEventClass::Failed.mask();
    }
    if events.completed {
        event_mask |= PushEventClass::Completed.mask();
    }
    let registration = crate::push::RegisterPushEndpoint {
        token: body.token,
        environment: body.environment,
        locale: body.locale,
        previews_enabled: body.previews_enabled,
        event_mask,
        public_key: body.public_key,
    };
    match daemon.register_push_endpoint(user_id, id, registration) {
        Ok(endpoint) => Json(serde_json::json!({
            "endpoint": push_endpoint_json(&endpoint),
        }))
        .into_response(),
        Err(e) => {
            // The app discards the response body, so without this the
            // device looks enrolled and silently never receives a push.
            tracing::warn!(device = id, reason = %e, "push registration refused");
            mobile_auth_error_response(e)
        }
    }
}

async fn unregister_push_endpoint(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    let Some((user_id, bearer_device)) = authed_device_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if bearer_device.is_some_and(|device_id| device_id != id) {
        return mobile_auth_error_response(crate::mobile::MobileAuthError::DeviceNotFound);
    }
    match daemon.unregister_push_endpoint(user_id, id) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => mobile_auth_error_response(e),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachTicketBody {
    #[serde(deserialize_with = "u64_from_string_or_number")]
    generation: u64,
    replay_bytes: Option<u64>,
}

/// Terminal generation arrives as a JSON string like every id in this
/// API, but a bare number is also accepted.
fn u64_from_string_or_number<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u64, D::Error> {
    struct Visitor;
    impl serde::de::Visitor<'_> for Visitor {
        type Value = u64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a u64 as a string or number")
        }
        fn visit_u64<E>(self, value: u64) -> Result<u64, E> {
            Ok(value)
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<u64, E> {
            value.parse().map_err(serde::de::Error::custom)
        }
    }
    deserializer.deserialize_any(Visitor)
}

/// Exchanges a bearer access token for the one-use attach ticket a
/// terminal WebSocket upgrade consumes, bound to the terminal's
/// current generation and a bounded replay size.
async fn mint_terminal_attach_ticket(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(terminal_id): Path<u64>,
    Json(body): Json<AttachTicketBody>,
) -> Response {
    let Some((user_id, device_id)) = authed_ticket_minter(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.create_terminal_attach_ticket(
        user_id,
        device_id,
        terminal_id,
        body.generation,
        body.replay_bytes,
    ) {
        Ok((ticket, expires_at)) => socket_ticket_json(ticket, expires_at),
        Err(e) => mobile_auth_error_response(e),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushForegroundBody {
    /// Session id string, or null when no terminal is in the
    /// foreground. Ids stay strings in JSON to keep u64 precision.
    session_id: Option<String>,
}

/// Records the foreground-suppression hint: while a device is looking
/// at a session's terminal, that session's events are not pushed to it.
async fn set_push_foreground(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<PushForegroundBody>,
) -> Response {
    let Some((user_id, bearer_device)) = authed_device_identity(&daemon, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if bearer_device.is_some_and(|device_id| device_id != id) {
        return mobile_auth_error_response(crate::mobile::MobileAuthError::DeviceNotFound);
    }
    match daemon.storage().get_mobile_device(id) {
        Ok(device) if device.user_id == user_id => {}
        _ => {
            return mobile_auth_error_response(crate::mobile::MobileAuthError::DeviceNotFound);
        }
    }
    let session_id = match &body.session_id {
        Some(raw) => match raw.parse::<u64>() {
            Ok(session_id) => Some(session_id),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "sessionId must be a numeric string" })),
                )
                    .into_response()
            }
        },
        None => None,
    };
    daemon.set_push_foreground_hint(id, session_id);
    StatusCode::NO_CONTENT.into_response()
}

async fn list_push_policies(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    if authed_identity(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match daemon.list_push_policies() {
        Ok(policies) => Json(serde_json::json!({
            "policies": policies
                .iter()
                .map(|p| serde_json::json!({
                    "bucketId": p.bucket_id.to_string(),
                    "role": p.role,
                    "events": p.events,
                    "scope": p.scope,
                    "updatedAtUnixMs": p.updated_at_unix_ms,
                }))
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushPolicyBody {
    bucket_id: u64,
    #[serde(default)]
    role: String,
    #[serde(default)]
    events: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    remove: bool,
}

/// Sets or removes one bucket/role push policy override.
async fn put_push_policy(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(body): Json<PushPolicyBody>,
) -> Response {
    if authed_identity(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if body.remove {
        return match daemon.delete_push_policy(body.bucket_id, &body.role) {
            Ok(removed) => Json(serde_json::json!({ "removed": removed })).into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
        };
    }
    match daemon.set_push_policy(body.bucket_id, &body.role, &body.events, &body.scope) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn me(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    if let Some(username) = authed_user(&daemon, &headers) {
        return Json(serde_json::json!({ "username": username })).into_response();
    }
    if daemon.needs_setup() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "setup": true })),
        )
            .into_response();
    }
    StatusCode::UNAUTHORIZED.into_response()
}

async fn setup(State(daemon): State<Arc<Daemon>>, Json(creds): Json<Credentials>) -> Response {
    match daemon.auth_setup(&creds.username, &creds.password) {
        Ok(token) => logged_in_response(&token),
        Err(e) => auth_error_response(e),
    }
}

async fn login(State(daemon): State<Arc<Daemon>>, Json(creds): Json<Credentials>) -> Response {
    match daemon.auth_login(&creds.username, &creds.password) {
        Ok(token) => logged_in_response(&token),
        Err(e) => auth_error_response(e),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordChange {
    current_password: String,
    new_password: String,
}

/// Only the cookie session can change a password: a mobile bearer token
/// is enrolled against the account rather than proving someone is at the
/// keyboard, and the change signs the other devices out.
async fn change_password(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(body): Json<PasswordChange>,
) -> Response {
    let Some(token) = session_cookie_value(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match daemon.change_password(&token, &body.current_password, &body.new_password) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => auth_error_response(e),
    }
}

async fn logout(State(daemon): State<Arc<Daemon>>, headers: HeaderMap) -> Response {
    if let Some(token) = session_cookie_value(&headers) {
        daemon.auth_logout(&token);
    }
    (
        [(header::SET_COOKIE, session_cookie("", 0))],
        StatusCode::NO_CONTENT,
    )
        .into_response()
}

fn logged_in_response(token: &str) -> Response {
    (
        [(
            header::SET_COOKIE,
            session_cookie(token, COOKIE_MAX_AGE_SECS),
        )],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

fn auth_error_response(e: AuthError) -> Response {
    let status = match e {
        AuthError::InvalidCredentials => StatusCode::UNAUTHORIZED,
        AuthError::AlreadySetUp => StatusCode::CONFLICT,
        AuthError::EmptyUsername | AuthError::PasswordTooShort | AuthError::PasswordUnchanged => {
            StatusCode::BAD_REQUEST
        }
        AuthError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
}

#[derive(serde::Deserialize)]
struct ControlSocketQuery {
    /// One-use mobile socket ticket; the cookie and bearer paths do
    /// not use it. Consumed (and thereby invalidated) at upgrade.
    ticket: Option<String>,
}

async fn ws_upgrade(
    State(daemon): State<Arc<Daemon>>,
    Query(query): Query<ControlSocketQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !crate::web_origin::handshake_is_own_origin(&headers, daemon.public_url()) {
        return refuse_foreign_origin("/ws", &headers);
    }
    let user_id = authed_identity(&daemon, &headers)
        .map(|(user_id, _)| user_id)
        .or_else(|| {
            let ticket = query.ticket.as_deref()?;
            let (user_id, _) = daemon.consume_control_socket_ticket(ticket)?;
            Some(user_id)
        });
    let client_host = request_host(&headers);
    ws.on_upgrade(move |socket| async move {
        let Some(user_id) = user_id else {
            let _ = close_unauthenticated(socket).await;
            return;
        };
        ws_connection(daemon, socket, user_id, client_host).await;
    })
}

/// Refuses a cookie-authenticated request before it reaches the session
/// cookie, and names both sides so an operator whose deployment is refused
/// can see which origin to configure.
fn refuse_foreign_origin(route: &str, headers: &HeaderMap) -> Response {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<absent>")
            .to_string()
    };
    warn!(
        route,
        origin = value(header::ORIGIN.as_str()),
        host = value(header::HOST.as_str()),
        "refusing a request that carries the session cookie from another origin"
    );
    StatusCode::FORBIDDEN.into_response()
}

/// The Host this client used to reach the daemon, so links rendered for
/// it can point back at the same origin.
fn request_host(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
}

const TERMINAL_SUBPROTOCOL: &str = "pm-terminal-v1";
const WS_CLOSE_TERMINAL_NOT_FOUND: u16 = 4404;
const WS_CLOSE_STALE_GENERATION: u16 = 4409;
const WS_CLOSE_RESYNC_REQUIRED: u16 = 4410;
const WS_CLOSE_STREAM_CAPACITY: u16 = 4429;
const WEB_TERMINAL_REPLAY_CAP_BYTES: usize = 256 * 1024;

/// Severs the opener relationship a page that opened the dashboard would
/// otherwise keep. Not in `http::header`, which only names registered headers.
const CROSS_ORIGIN_OPENER_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-opener-policy");
const WEB_TERMINAL_REPLAY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(serde::Deserialize)]
struct TerminalStreamQuery {
    generation: u64,
    /// One-use mobile attach ticket; the cookie and bearer paths do
    /// not use it. Consumed (and thereby invalidated) at upgrade.
    ticket: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    viewer: Option<u64>,
    claim: Option<u64>,
}

async fn terminal_ws_upgrade(
    State(daemon): State<Arc<Daemon>>,
    Path(terminal_id): Path<u64>,
    Query(query): Query<TerminalStreamQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !crate::web_origin::handshake_is_own_origin(&headers, daemon.public_url()) {
        return refuse_foreign_origin("/ws/terminal", &headers);
    }
    let auth = match authed_identity(&daemon, &headers) {
        Some((user_id, _)) => Some((user_id, WEB_TERMINAL_REPLAY_CAP_BYTES)),
        None => query.ticket.as_deref().and_then(|ticket| {
            let attach =
                daemon.consume_terminal_attach_ticket(ticket, terminal_id, query.generation)?;
            Some((attach.user_id, attach.replay_bytes))
        }),
    };
    let Some((user_id, replay_cap)) = auth else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let supports_protocol = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|item| item.trim() == TERMINAL_SUBPROTOCOL)
        });
    if !supports_protocol {
        return StatusCode::BAD_REQUEST.into_response();
    }
    ws.protocols([TERMINAL_SUBPROTOCOL])
        .on_upgrade(move |socket| async move {
            if !daemon.acquire_web_terminal_stream(user_id) {
                let _ = close_terminal_socket(
                    socket,
                    WS_CLOSE_STREAM_CAPACITY,
                    "terminal stream limit reached",
                )
                .await;
                return;
            }
            terminal_ws_connection(daemon.clone(), socket, terminal_id, replay_cap, query).await;
            daemon.release_web_terminal_stream(user_id);
        })
}

async fn close_terminal_socket(
    mut socket: WebSocket,
    code: u16,
    reason: &'static str,
) -> Result<(), axum::Error> {
    socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await
}

/// How long a resized viewer waits for the program to start redrawing
/// before it takes the snapshot anyway, which covers a worker's round trip
/// plus the program's own reaction to SIGWINCH.
const RESYNC_FIRST_OUTPUT_WAIT: std::time::Duration = std::time::Duration::from_millis(120);
/// A redraw is over once output has been quiet this long.
const RESYNC_QUIET: std::time::Duration = std::time::Duration::from_millis(30);
/// The longest a resized viewer keeps its old screen.
const RESYNC_MAX_WAIT: std::time::Duration = std::time::Duration::from_millis(300);

/// Output already queued when a viewer's socket is ready to send goes out
/// as one frame rather than one per PTY read.
const COALESCE_LIMIT_BYTES: usize = 16 * 1024;

async fn terminal_ws_connection(
    daemon: Arc<Daemon>,
    socket: WebSocket,
    terminal_id: u64,
    replay_cap: usize,
    query: TerminalStreamQuery,
) {
    let TerminalStreamQuery {
        generation,
        cols: initial_cols,
        rows: initial_rows,
        viewer,
        claim,
        ..
    } = query;
    let terminal = match daemon.terminal(terminal_id) {
        Ok(terminal) => terminal,
        Err(_) => {
            let _ =
                close_terminal_socket(socket, WS_CLOSE_TERMINAL_NOT_FOUND, "terminal not found")
                    .await;
            return;
        }
    };
    if terminal.generation != generation {
        let _ = close_terminal_socket(
            socket,
            WS_CLOSE_STALE_GENERATION,
            "terminal generation changed",
        )
        .await;
        return;
    }
    let size = match (initial_cols, initial_rows) {
        (Some(cols), Some(rows))
            if cols >= crate::mux::MIN_COLS && rows >= crate::mux::MIN_ROWS =>
        {
            Some((cols, rows))
        }
        _ => None,
    };
    let attached_terminal = match daemon.storage.get_terminal(terminal_id) {
        Ok(terminal) => terminal,
        Err(_) => return,
    };
    let viewer_id = viewer.unwrap_or_else(rand::random);
    let attach = match daemon.attach_web_terminal_for_viewer(
        terminal_id,
        replay_cap,
        size,
        viewer_id,
        claim.unwrap_or_default(),
    ) {
        Ok(attach) => attach,
        Err(_) => {
            let _ =
                close_terminal_socket(socket, WS_CLOSE_TERMINAL_NOT_FOUND, "terminal unavailable")
                    .await;
            return;
        }
    };
    let (replay, mut output, mut size_rx, mut rewrite_rx) = (
        attach.replay,
        attach.output,
        attach.size_rx,
        attach.rewrite_rx,
    );
    let progress = Arc::new(attach.progress);
    let acked = progress.clone();
    let _guard = attach.guard;
    let mut ownership_rx = daemon.viewer_owners.subscribe(terminal_id, generation);
    let (mut sink, mut stream) = socket.split();
    let (resync_tx, mut resync_rx) = mpsc::channel::<()>(1);
    let (cols, rows) = attach.pty_size;
    if viewer.is_some() {
        let owner = ownership_rx.borrow_and_update().clone();
        let frame = pm_protocol::terminal_frame::encode_ownership(
            generation,
            owner.revision,
            owner.owner,
            owner.acknowledgment(viewer_id),
            if owner.revision == 0 {
                cols
            } else {
                owner.cols
            },
            if owner.revision == 0 {
                rows
            } else {
                owner.rows
            },
        );
        if sink.send(Message::Binary(frame)).await.is_err() {
            return;
        }
    }
    let size_frame = pm_protocol::terminal_frame::encode_resize(generation, cols, rows);
    if sink.send(Message::Binary(size_frame)).await.is_err() {
        return;
    }
    // A viewer that opens at a new size gets its first snapshot once the
    // program has redrawn for that size, not the screen from before it.
    let hold_first_snapshot = attach.resized;
    match replay {
        crate::daemon::WebTerminalReplay::Complete { .. } if hold_first_snapshot => {}
        crate::daemon::WebTerminalReplay::Complete { bytes } => {
            for frame in pm_protocol::terminal_frame::replay_frames_with_flags(
                generation,
                &bytes[..],
                pm_protocol::terminal_frame::FLAG_REPLAY_SNAPSHOT,
            ) {
                if sink.send(Message::Binary(frame)).await.is_err() {
                    return;
                }
            }
        }
        crate::daemon::WebTerminalReplay::Streaming(mut replay) => loop {
            let chunk = match tokio::time::timeout(WEB_TERMINAL_REPLAY_TIMEOUT, replay.recv()).await
            {
                Ok(Some(chunk)) => chunk,
                _ => return,
            };
            let end = chunk.flags & pm_protocol::terminal_frame::FLAG_REPLAY_END != 0;
            let frame =
                pm_protocol::terminal_frame::encode_output(generation, chunk.flags, &chunk.data);
            if !hold_first_snapshot && sink.send(Message::Binary(frame)).await.is_err() {
                return;
            }
            if end {
                break;
            }
        },
    }
    let write = async {
        // While a viewer waits for a resized snapshot, live output is absorbed
        // rather than sent: the program is redrawing for the new size, and the
        // snapshot taken once it goes quiet already contains that redraw.
        let mut resync: Option<(tokio::time::Instant, Option<tokio::time::Instant>)> =
            hold_first_snapshot.then(|| (tokio::time::Instant::now(), None));
        loop {
            let wake = resync.map(|(asked, last)| match last {
                None => asked + RESYNC_FIRST_OUTPUT_WAIT,
                Some(last) => (last + RESYNC_QUIET).min(asked + RESYNC_MAX_WAIT),
            });
            let settled = async {
                match wake {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = settled => {
                    resync = None;
                    let Some((snapshot, fresh, (cols, rows))) =
                        daemon.web_terminal_resnapshot(terminal_id, &progress)
                    else {
                        if hold_first_snapshot {
                            return;
                        }
                        continue;
                    };
                    output = fresh;
                    let size = pm_protocol::terminal_frame::encode_resize(generation, cols, rows);
                    if sink.send(Message::Binary(size)).await.is_err() {
                        return;
                    }
                    for frame in pm_protocol::terminal_frame::replay_frames_with_flags(
                        generation,
                        &snapshot[..],
                        pm_protocol::terminal_frame::FLAG_REPLAY_SNAPSHOT,
                    ) {
                        if sink.send(Message::Binary(frame)).await.is_err() {
                            return;
                        }
                    }
                }
                data = output.recv() => match data {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
                        if resync.is_some() =>
                    {
                        if let Some((_, last)) = resync.as_mut() {
                            *last = Some(tokio::time::Instant::now());
                        }
                    }
                    Ok(mut data) => {
                        let mut gathered: Option<Vec<u8>> = None;
                        while gathered.as_ref().map_or(data.len(), Vec::len) < COALESCE_LIMIT_BYTES {
                            match output.try_recv() {
                                Ok(more) => gathered.get_or_insert_with(|| data.to_vec()).extend_from_slice(&more),
                                Err(_) => break,
                            }
                        }
                        if let Some(gathered) = gathered {
                            data = bytes::Bytes::from(gathered);
                        }
                        crate::probe_trace::mark("d_ws_out", &data);
                        let sent = data.len();
                        let frame = pm_protocol::terminal_frame::encode_output(generation, 0, &data);
                        if sink.send(Message::Binary(frame)).await.is_err() {
                            return;
                        }
                        progress.sent(sent);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code: WS_CLOSE_RESYNC_REQUIRED,
                                reason: "terminal stream lagged".into(),
                            })))
                            .await;
                        return;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        let _ = sink.send(Message::Close(None)).await;
                        return;
                    }
                },
                Some(()) = resync_rx.recv() => {
                    if resync.is_none() {
                        resync = Some((tokio::time::Instant::now(), None));
                    }
                }
                // The program's repaint is already arriving, so the snapshot
                // waits only for it to go quiet.
                rewritten = rewrite_rx.recv() => match rewritten {
                    Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if resync.is_none() {
                            let now = tokio::time::Instant::now();
                            resync = Some((now, Some(now)));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        let _ = sink.send(Message::Close(None)).await;
                        return;
                    }
                },
                changed = ownership_rx.changed(), if viewer.is_some() => {
                    if changed.is_err() { return; }
                    let owner = ownership_rx.borrow_and_update().clone();
                    let frame = pm_protocol::terminal_frame::encode_ownership(generation, owner.revision, owner.owner, owner.acknowledgment(viewer_id), owner.cols, owner.rows);
                    if sink.send(Message::Binary(frame)).await.is_err() { return; }
                },
                size = size_rx.recv() => match size {
                    Ok((cols, rows)) => {
                        let frame =
                            pm_protocol::terminal_frame::encode_resize(generation, cols, rows);
                        if sink.send(Message::Binary(frame)).await.is_err() {
                            return;
                        }
                    }
                    // Echoes are best-effort observability of the shared
                    // PTY size; a lagged receiver still gets the newest
                    // buffered change on the next recv.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        let _ = sink.send(Message::Close(None)).await;
                        return;
                    }
                },
            }
        }
    };
    let read = async {
        while let Some(Ok(message)) = stream.next().await {
            let Message::Binary(frame) = message else {
                if matches!(message, Message::Close(_)) {
                    break;
                }
                continue;
            };
            match pm_protocol::terminal_frame::decode(&frame) {
                Some(pm_protocol::terminal_frame::TerminalFrame::Ack {
                    generation: incoming,
                    bytes,
                }) if incoming == generation => {
                    acked.acked(bytes);
                }
                Some(pm_protocol::terminal_frame::TerminalFrame::Input {
                    generation: incoming,
                    submitted,
                    data,
                }) if incoming == generation => {
                    crate::probe_trace::mark("d_ws_in", data);
                    daemon.terminal_input_for(
                        &attached_terminal,
                        bytes::Bytes::copy_from_slice(data),
                        submitted,
                    );
                }
                Some(pm_protocol::terminal_frame::TerminalFrame::Resize {
                    generation: incoming,
                    cols,
                    rows,
                }) if incoming == generation => {
                    daemon.viewer_resize(terminal_id, generation, viewer_id, 0, cols, rows);
                }
                Some(pm_protocol::terminal_frame::TerminalFrame::ResizeRequest {
                    generation: incoming,
                    request,
                    cols,
                    rows,
                }) if incoming == generation && viewer.is_some() => {
                    daemon.viewer_resize(terminal_id, generation, viewer_id, request, cols, rows);
                }
                Some(pm_protocol::terminal_frame::TerminalFrame::Resync {
                    generation: incoming,
                }) if incoming == generation => {
                    let _ = resync_tx.try_send(());
                }
                _ => break,
            }
        }
    };
    tokio::select! {
        _ = write => {}
        _ = read => {}
    }
}

async fn close_unauthenticated(mut socket: WebSocket) -> Result<(), axum::Error> {
    socket
        .send(Message::Close(Some(CloseFrame {
            code: WS_CLOSE_UNAUTHENTICATED,
            reason: "unauthenticated".into(),
        })))
        .await
}

async fn ws_connection(
    daemon: Arc<Daemon>,
    socket: WebSocket,
    user_id: u64,
    client_host: Option<String>,
) {
    let (mut sink, mut stream) = {
        use futures::StreamExt;
        socket.split()
    };
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(OUTGOING_CHANNEL_CAPACITY);

    let writer = tokio::spawn(async move {
        use futures::SinkExt;
        while let Some(msg) = out_rx.recv().await {
            let frame = Message::Binary(msg.encode_to_vec().into());
            if sink.send(frame).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let mut conn = ConnState::for_authenticated_user(user_id).with_client_host(client_host);
    loop {
        use futures::StreamExt;
        let msg = match stream.next().await {
            Some(Ok(m)) => m,
            _ => break,
        };
        match msg {
            Message::Binary(buf) => match ClientEnvelope::decode(&buf) {
                Ok(envelope) if is_terminal_transport_message(&envelope.msg) => {
                    let _ = out_tx
                        .send(ServerMsg::CommandResult {
                            seq: envelope.seq,
                            result: Err(
                                "terminal traffic requires a dedicated terminal socket".into()
                            ),
                            data: Bytes::new(),
                        })
                        .await;
                }
                Ok(envelope) => handle_message(&daemon, envelope, &out_tx, &mut conn).await,
                Err(e) => {
                    debug!(error = %e, "undecodable ws frame, closing");
                    break;
                }
            },
            Message::Close(_) => break,
            // Pings are answered by axum automatically; text frames
            // are not part of the protocol.
            Message::Text(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    conn.abort_all();
    writer.abort();
}

fn is_terminal_transport_message(msg: &pm_protocol::domain::ClientMsg) -> bool {
    matches!(
        msg,
        pm_protocol::domain::ClientMsg::AttachPty { .. }
            | pm_protocol::domain::ClientMsg::DetachPty { .. }
            | pm_protocol::domain::ClientMsg::PtyInput { .. }
            | pm_protocol::domain::ClientMsg::PtyResize { .. }
            | pm_protocol::domain::ClientMsg::AttachTerminal { .. }
            | pm_protocol::domain::ClientMsg::DetachTerminal { .. }
            | pm_protocol::domain::ClientMsg::TerminalInput { .. }
            | pm_protocol::domain::ClientMsg::TerminalResize { .. }
    )
}

/// The worker plane. A remote worker connects here and authenticates in
/// the handshake (enrollment token or stored credential) rather than by
/// browser cookie.
async fn worker_ws_upgrade(
    State(daemon): State<Arc<Daemon>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<crate::worker_plane::WorkerPeer>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| worker_connection(daemon, peer, socket))
}

/// The dial-back endpoint for forwarded streams. The worker presents
/// the single-use token from ControllerForwardOpen as a bearer; the
/// parked connection it claims is spliced to this socket.
async fn worker_stream_upgrade(
    State(daemon): State<Arc<Daemon>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<crate::worker_plane::WorkerPeer>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !is_enrolled_host(&daemon, &peer) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let Some(fwd) = daemon
        .forwards
        .claim_stream(&crate::auth::hash_token(token))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    ws.on_upgrade(move |socket| async move {
        let (frames, pumps) = crate::worker_plane::FrameLink::accepted(
            socket,
            crate::worker_plane::Keepalive::STREAM,
            crate::worker_plane::LinkId::new("forward", peer.key_hash),
        );
        crate::forward::splice_ws_tcp(frames, fwd).await;
        pumps.finish().await;
    })
}

async fn worker_terminal_upgrade(
    State(daemon): State<Arc<Daemon>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<crate::worker_plane::WorkerPeer>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !is_enrolled_host(&daemon, &peer) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    let Some(claim) = daemon
        .terminal_streams
        .claim(&crate::auth::hash_token(token), &daemon.workers)
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    ws.on_upgrade(move |socket| async move {
        let (frames, pumps) = crate::worker_plane::FrameLink::accepted_with_queue(
            socket,
            crate::worker_plane::Keepalive::STREAM,
            crate::worker_plane::LinkId::new("terminal", peer.key_hash),
            crate::worker_plane::TERMINAL_STREAM_QUEUE,
        );
        worker_terminal_connection(claim, frames).await;
        pumps.finish().await;
    })
}

pub(crate) async fn worker_terminal_connection(
    claim: crate::workers::TerminalStreamClaim,
    frames: crate::worker_plane::FrameLink,
) {
    let crate::worker_plane::FrameLink {
        out: sink,
        inbound: mut stream,
    } = frames;
    let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(256);
    let stream_epoch =
        claim
            .link
            .connect_terminal_stream(claim.terminal_id, claim.generation, input_tx);
    let write = async {
        while let Some(frame) = input_rx.recv().await {
            crate::probe_trace::mark("d_stream_tx", &frame);
            if sink.send(frame).await.is_err() {
                break;
            }
        }
    };
    let read = async {
        while let Some(frame) = stream.recv().await {
            crate::probe_trace::mark("d_stream_rx", &frame);
            let Some(pm_protocol::terminal_frame::TerminalFrame::Output {
                generation,
                flags,
                data,
            }) = pm_protocol::terminal_frame::decode(&frame)
            else {
                break;
            };
            if generation != claim.generation {
                break;
            }
            claim.link.feed_terminal_output(
                claim.terminal_id,
                generation,
                flags,
                Bytes::copy_from_slice(data),
            );
            claim.link.wait_for_viewers(claim.terminal_id).await;
        }
    };
    tokio::select! {
        _ = write => {}
        _ = read => {}
    }
    claim
        .link
        .disconnect_terminal_stream(claim.terminal_id, stream_epoch);
}

async fn worker_transcript_upgrade(
    State(daemon): State<Arc<Daemon>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<crate::worker_plane::WorkerPeer>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !is_enrolled_host(&daemon, &peer) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    let Some(claim) = daemon
        .transcript_transfers
        .claim(&crate::auth::hash_token(token), &daemon.workers)
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    ws.on_upgrade(move |socket| async move {
        let (frames, pumps) = crate::worker_plane::FrameLink::accepted(
            socket,
            crate::worker_plane::Keepalive::Off,
            crate::worker_plane::LinkId::new("transcript", peer.key_hash),
        );
        worker_transcript_connection(daemon, claim, frames).await;
        pumps.finish().await;
    })
}

pub(crate) async fn worker_transcript_connection(
    daemon: Arc<Daemon>,
    claim: crate::workers::TranscriptTransferClaim,
    frames: crate::worker_plane::FrameLink,
) {
    let mut socket = frames.inbound;
    use tokio::io::AsyncWriteExt;

    let target =
        daemon.terminal_scrollback_path(claim.transcript.terminal_id, claim.transcript.generation);
    let temporary = target.with_extension("upload");
    let Ok(mut file) = tokio::fs::File::create(&temporary).await else {
        return;
    };
    let mut received = 0u64;
    let mut valid = true;
    while let Some(chunk) = socket.recv().await {
        match chunk {
            chunk
                if chunk.len() <= pm_protocol::terminal_frame::MAX_REPLAY_CHUNK_BYTES
                    && received + chunk.len() as u64 <= claim.transcript.size =>
            {
                if file.write_all(&chunk).await.is_err() {
                    valid = false;
                    break;
                }
                received += chunk.len() as u64;
            }
            _ => {
                valid = false;
                break;
            }
        }
    }
    valid &= received == claim.transcript.size;
    valid &= file.sync_all().await.is_ok();
    drop(file);
    if valid {
        valid = tokio::fs::rename(&temporary, &target).await.is_ok();
    }
    if !valid {
        let _ = tokio::fs::remove_file(&temporary).await;
        return;
    }
    daemon
        .mark_worker_transcript_received(claim.transcript.terminal_id, claim.transcript.generation);
    let _ = claim.link.send(ControllerMsg::TranscriptAck {
        terminal_id: claim.transcript.terminal_id,
        generation: claim.transcript.generation,
    });
    // Dropping the receiver ends the pump, which closes the socket.
    drop(socket);
    claim.complete();
}

/// Whether the handshake proved a key this controller has enrolled. The
/// per-stream endpoints are already gated by a single-use token, but that
/// token is only ever sent to one host, so requiring that host's key too
/// means a leaked token is not enough on its own.
fn is_enrolled_host(daemon: &Arc<Daemon>, peer: &crate::worker_plane::WorkerPeer) -> bool {
    daemon
        .storage()
        .worker_by_key_hash(&peer.key_hash.to_hex())
        .ok()
        .flatten()
        .is_some()
}

/// Settles whether this peer may speak the control protocol at all.
///
/// A host whose key the controller already pinned is authenticated by the
/// handshake and is simply told to proceed. An unknown key gets one chance to
/// prove it holds a live enrollment token, and proves it first: the
/// controller answers with its own proof only afterwards, which is what lets
/// the host know it is enrolling with the right controller before it accepts
/// a single command.
async fn open_worker_link(
    daemon: &Arc<Daemon>,
    peer: &crate::worker_plane::WorkerPeer,
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
    stream: &mut futures::stream::SplitStream<WebSocket>,
) -> bool {
    use futures::{SinkExt, StreamExt};
    use pm_tls::pairing::{Side, Transcript};

    let known = daemon
        .storage()
        .worker_by_key_hash(&peer.key_hash.to_hex())
        .ok()
        .flatten()
        .is_some();
    if known {
        return sink
            .send(Message::Binary(worker_frame::encode_ready().into()))
            .await
            .is_ok();
    }

    let listener_nonce = pm_tls::pairing::nonce();
    if sink
        .send(Message::Binary(
            worker_frame::encode_pair_hello(&listener_nonce).into(),
        ))
        .await
        .is_err()
    {
        return false;
    }
    let Some(Ok(Message::Binary(buf))) = stream.next().await else {
        return false;
    };
    let Some(WorkerFrame::PairProof {
        nonce: dialer_nonce,
        mac,
    }) = worker_frame::decode(&buf)
    else {
        debug!(remote = %peer.remote, "unknown host did not open with an enrollment proof");
        return false;
    };

    let transcript = Transcript {
        exporter: peer.exporter,
        dialer_key: peer.key_hash,
        listener_key: peer.local_key_hash,
        dialer_nonce,
        listener_nonce,
    };
    // Enrollments are single-use and short-lived, so this is one token or
    // none. Which token matched is not recorded here: the register frame
    // presents it again and burning it there keeps that decision in one place.
    let Some(token) = daemon
        .live_enrollment_tokens()
        .into_iter()
        .find(|token| transcript.verify(token, Side::Dialer, &mac))
    else {
        debug!(remote = %peer.remote, "enrollment proof did not match any live token");
        return false;
    };

    let accept = transcript.mac(&token, Side::Listener);
    sink.send(Message::Binary(
        worker_frame::encode_pair_accept(&accept).into(),
    ))
    .await
    .is_ok()
}

async fn worker_connection(
    daemon: Arc<Daemon>,
    peer: crate::worker_plane::WorkerPeer,
    socket: WebSocket,
) {
    use futures::StreamExt;
    let (mut sink, mut stream) = socket.split();

    if !open_worker_link(&daemon, &peer, &mut sink, &mut stream).await {
        return;
    }

    // Past the handshake the two directions are the same session, so it runs
    // on frames rather than on whichever socket carried them here.
    let (frames, pumps) = crate::worker_plane::FrameLink::accepted(
        sink.reunite(stream).expect("same socket"),
        crate::worker_plane::Keepalive::CONTROL,
        crate::worker_plane::LinkId::new("control", peer.key_hash),
    );
    crate::worker_plane::run_session(&daemon, &peer.key_hash.to_hex(), frames).await;
    pumps.finish().await;
}

/// Runs one registered host: the register exchange, then relaying its
/// control messages until the link ends.
pub(crate) async fn worker_session(
    daemon: &Arc<Daemon>,
    peer_key_hash: &str,
    frames: crate::worker_plane::FrameLink,
) {
    let crate::worker_plane::FrameLink { out, mut inbound } = frames;
    let send = |msg: ControllerMsg| {
        let out = out.clone();
        async move {
            out.send(Bytes::from(worker_frame::encode_control(
                &msg.encode_to_vec(),
            )))
            .await
            .is_ok()
        }
    };

    let register = match inbound.recv().await {
        Some(buf) => match worker_frame::decode(&buf) {
            Some(WorkerFrame::Control(payload)) => WorkerMsg::decode(payload).ok(),
            _ => None,
        },
        _ => None,
    };
    let Some(WorkerMsg::Register {
        protocol_version,
        enrollment_token,
        credential,
        hostname,
        platform,
        pm_version,
        runtime,
        container,
        default_project_root,
        live_sessions,
        live_terminals,
        pending_transcripts,
        live_dir_shares,
    }) = register
    else {
        debug!("worker connection did not open with a register frame");
        return;
    };

    let protocol = match pm_protocol::negotiate_worker_protocol(protocol_version) {
        Ok(protocol) => {
            info!(
                hostname,
                pm_version,
                worker_protocol = protocol.version(),
                controller_protocol = pm_protocol::WORKER_PROTOCOL_VERSION,
                controller_mcp_address_shim = protocol.needs_controller_mcp_address(),
                "worker registering"
            );
            protocol
        }
        Err(refusal) => {
            warn!(%refusal, "refusing worker registration");
            send(ControllerMsg::Registered {
                worker_id: 0,
                credential: String::new(),
                mcp_base_url: String::new(),
                http_port: 0,
                pm_version: crate::pm_build_version().to_string(),
                error: refusal.to_string(),
            })
            .await;
            return;
        }
    };

    let registration = match daemon.register_worker_connection(crate::daemon::WorkerHello {
        enrollment_token: &enrollment_token,
        credential: &credential,
        peer_key_hash,
        hostname: &hostname,
        platform: &platform,
        pm_version: &pm_version,
        runtime: &runtime,
        container: &container,
        default_project_root: &default_project_root,
        protocol_version: protocol.version(),
        live_sessions: &live_sessions,
        live_terminals: &live_terminals,
    }) {
        Ok(r) => r,
        Err(e) => {
            send(ControllerMsg::Registered {
                worker_id: 0,
                credential: String::new(),
                mcp_base_url: String::new(),
                http_port: 0,
                pm_version: crate::pm_build_version().to_string(),
                error: e.to_string(),
            })
            .await;
            return;
        }
    };

    let (mcp_base_url, http_port) = if protocol.needs_controller_mcp_address() {
        (
            daemon.agent_mcp_base_url().unwrap_or_default(),
            u32::from(daemon.http_port()),
        )
    } else {
        (String::new(), 0)
    };
    let acknowledged = send(ControllerMsg::Registered {
        worker_id: registration.worker_id,
        credential: registration.credential.clone(),
        mcp_base_url,
        http_port,
        pm_version: crate::pm_build_version().to_string(),
        error: String::new(),
    })
    .await;
    if !acknowledged {
        schedule_worker_grace(daemon, &registration);
        daemon.disconnect_worker(&registration.link);
        return;
    }
    daemon.enqueue_worker_transcripts(&registration.link, pending_transcripts);
    {
        let daemon = daemon.clone();
        let worker_id = registration.worker_id;
        tokio::spawn(async move {
            daemon
                .reconcile_worker_dir_shares(worker_id, &live_dir_shares)
                .await;
        });
    }

    let worker_id = registration.worker_id;
    let epoch = registration.epoch;
    let mut rx = registration.rx;

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if out
                .send(Bytes::from(worker_frame::encode_control(
                    &msg.encode_to_vec(),
                )))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    loop {
        let buf = tokio::select! {
            // A newer connection for this worker retired this link. The
            // old socket is not always dead when that happens, so stop
            // relaying on this side rather than applying its messages
            // beside the connection that replaced it.
            () = registration.link.superseded() => break,
            frame = inbound.recv() => match frame {
                Some(buf) => buf,
                None => break,
            },
        };
        match worker_frame::decode(&buf) {
            Some(WorkerFrame::Control(payload)) => match WorkerMsg::decode(payload) {
                Ok(m) => daemon.apply_worker_message(worker_id, m),
                Err(e) => {
                    debug!(error = %e, "undecodable worker control frame, closing");
                    break;
                }
            },
            // Pairing is settled before the link opens, so a proof frame
            // arriving here is a peer trying to renegotiate trust
            // mid-session.
            _ => break,
        }
    }

    writer.abort();
    // A superseded link's worker is online on its newer connection, so
    // the disconnect bookkeeping would announce a host leaving that
    // never left, and resume sessions that are already running.
    if registration.link.is_superseded() {
        info!(
            worker = worker_id,
            "worker control link closed: a newer connection replaced it"
        );
        return;
    }
    daemon.disconnect_worker(&registration.link);
    // Keep the worker's sessions alive for the grace period; fail them
    // only if no newer connection arrives.
    let daemon = daemon.clone();
    tokio::spawn(async move {
        tokio::time::sleep(crate::daemon::WORKER_RECONNECT_GRACE).await;
        daemon.fail_worker_if_still_gone(worker_id, epoch);
    });
}

fn schedule_worker_grace(daemon: &Arc<Daemon>, registration: &crate::daemon::WorkerRegistration) {
    let daemon = daemon.clone();
    let worker_id = registration.worker_id;
    let epoch = registration.epoch;
    tokio::spawn(async move {
        tokio::time::sleep(crate::daemon::WORKER_RECONNECT_GRACE).await;
        daemon.fail_worker_if_still_gone(worker_id, epoch);
    });
}

async fn static_assets(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let candidate = if path.is_empty() { "index.html" } else { path };
    let asset = load_ui_asset(candidate).await;
    let asset = if asset.is_none() && !candidate.contains('.') {
        load_ui_asset("index.html").await
    } else {
        asset
    };
    match asset {
        Some(content) => {
            let mime = mime_guess::from_path(candidate).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    // A preview can `window.open` the dashboard and keep the
                    // handle, which is a real same-origin window whatever the
                    // preview's own origin is. Severing the opener relationship
                    // is what stops it scripting the page that holds the token.
                    (CROSS_ORIGIN_OPENER_POLICY, "same-origin".to_string()),
                ],
                Body::from(content),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            "not found (build the web UI with `npm run build` in web/)",
        )
            .into_response(),
    }
}

#[cfg(not(debug_assertions))]
async fn load_ui_asset(candidate: &str) -> Option<Vec<u8>> {
    UiAssets::get(candidate).map(|content| content.data.into_owned())
}

#[cfg(debug_assertions)]
async fn load_ui_asset(candidate: &str) -> Option<Vec<u8>> {
    let root = std::env::var_os("PM_WEB_ASSETS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist"));
    let path = debug_asset_path(&root, candidate)?;
    tokio::fs::read(path).await.ok()
}

#[cfg(debug_assertions)]
fn debug_asset_path(root: &std::path::Path, candidate: &str) -> Option<std::path::PathBuf> {
    let relative = std::path::Path::new(candidate);
    relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
        .then(|| root.join(relative))
}

#[cfg(test)]
mod tests {
    #[test]
    fn enrollment_bucket_access_is_optional_and_accepts_multiple_buckets() {
        let legacy: super::EnrollRequest = serde_json::from_str(r#"{"label":"worker"}"#).unwrap();
        assert!(legacy.bucket_ids.is_empty());
        let selected: super::EnrollRequest =
            serde_json::from_str(r#"{"label":"worker","bucket_ids":[3,8]}"#).unwrap();
        assert_eq!(selected.bucket_ids, vec![3, 8]);
    }

    #[cfg(debug_assertions)]
    use super::debug_asset_path;
    use super::reachable_without_tls;
    use std::net::SocketAddr;

    // The function under test only exists where it is used, which is a
    // build serving assets from the worktree.
    #[cfg(debug_assertions)]
    #[test]
    fn debug_asset_paths_stay_beneath_the_configured_root() {
        let root = std::path::Path::new("/tmp/web-dist");
        assert_eq!(
            debug_asset_path(root, "assets/app.js"),
            Some(root.join("assets/app.js"))
        );
        assert_eq!(debug_asset_path(root, "../secret"), None);
        assert_eq!(debug_asset_path(root, "/absolute"), None);
    }

    #[test]
    fn only_an_off_host_plaintext_bind_is_flagged() {
        let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
        assert!(reachable_without_tls(&addr("0.0.0.0:7777"), false));
        assert!(reachable_without_tls(&addr("192.168.1.10:7777"), false));
        assert!(reachable_without_tls(&addr("[::]:7777"), false));

        assert!(!reachable_without_tls(&addr("127.0.0.1:7777"), false));
        assert!(!reachable_without_tls(&addr("[::1]:7777"), false));
        assert!(!reachable_without_tls(&addr("0.0.0.0:7777"), true));
        assert!(!reachable_without_tls(&addr("192.168.1.10:7777"), true));
    }
}

#[derive(serde::Deserialize)]
struct HarnessRequest {
    project: u64,
    worker: u64,
    agent: Option<String>,
    #[serde(default)]
    install: bool,
}

async fn harness_status(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(q): Json<HarnessRequest>,
) -> Response {
    if authed_user(&daemon, &headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let agent = match q.agent.as_deref() {
        None => None,
        Some(name) => match pm_protocol::domain::AgentKind::parse(name) {
            Some(agent) => Some(agent),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "unknown harness" })),
                )
                    .into_response()
            }
        },
    };
    match daemon
        .harness_status(q.project, q.worker, agent, q.install)
        .await
    {
        Ok((agent, status)) => {
            Json(serde_json::json!({ "agent": agent.as_str(), "status": status })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
