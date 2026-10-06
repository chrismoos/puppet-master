//! The MCP endpoint agents on this host post to.
//!
//! The control link is the one route to the controller this host has
//! proven, whichever end opened it. The controller's browser plane may be
//! bound to loopback, sit behind NAT, or simply not be routable from here,
//! so agents are never pointed at it. The host binds a loopback endpoint
//! of its own, hands that to each agent, and carries every request up the
//! control link, where the controller answers as it would a direct post.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use pm_protocol::domain::WorkerMsg;
use tokio::sync::oneshot;
use tracing::{info, warn};

/// A relayed request holds a slot in memory until the controller answers, so
/// the number in flight is capped rather than growing with whatever the
/// agents do.
const MAX_IN_FLIGHT: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Matches the body the controller's own MCP listener accepts, so a tool
/// call that a direct post would take is never refused for being relayed.
/// Anything larger is a mistake, and relaying it would tie up the control
/// link the terminals share.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<(u32, String)>>>>;

#[derive(Clone)]
pub struct McpRelay {
    pending: Pending,
    next_id: Arc<AtomicU64>,
    send: Arc<dyn Fn(WorkerMsg) + Send + Sync>,
}

impl McpRelay {
    pub fn new(send: impl Fn(WorkerMsg) + Send + Sync + 'static) -> Self {
        Self {
            pending: Default::default(),
            next_id: Arc::new(AtomicU64::new(1)),
            send: Arc::new(send),
        }
    }

    /// Delivers the controller's answer to whichever request is waiting.
    pub fn resolve(&self, req_id: u64, status: u32, body: String) {
        if let Some(waiter) = self.pending.lock().unwrap().remove(&req_id) {
            let _ = waiter.send((status, body));
        }
    }

    /// Fails every waiting request. The control link is how answers come
    /// back, so a drop means none of them can be answered.
    pub fn abandon(&self) {
        self.pending.lock().unwrap().clear();
    }

    async fn call(&self, bearer: String, body: String) -> Option<(u32, String)> {
        let (tx, rx) = oneshot::channel();
        let req_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.len() >= MAX_IN_FLIGHT {
                return None;
            }
            pending.insert(req_id, tx);
        }
        (self.send)(WorkerMsg::McpRequest {
            req_id,
            bearer,
            body,
        });
        let answer = tokio::time::timeout(REQUEST_TIMEOUT, rx).await;
        self.pending.lock().unwrap().remove(&req_id);
        answer.ok()?.ok()
    }
}

/// Binds the loopback endpoint and returns the URL to hand to agents.
pub async fn serve(relay: McpRelay) -> anyhow::Result<String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let app = Router::new().route("/mcp", post(handle)).with_state(relay);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            warn!(error = %e, "agent report relay stopped");
        }
    });
    info!(%addr, "relaying agent reports to the controller");
    Ok(format!("http://{addr}/mcp"))
}

async fn handle(State(relay): State<McpRelay>, headers: HeaderMap, body: String) -> Response {
    let Some(bearer) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if body.len() > MAX_BODY_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    match relay.call(bearer.to_string(), body).await {
        Some((status, body)) if body.is_empty() => StatusCode::from_u16(status as u16)
            .unwrap_or(StatusCode::ACCEPTED)
            .into_response(),
        Some((status, body)) => (
            StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK),
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        // The controller is unreachable or the relay is saturated. Either
        // way the agent should see a failure rather than a fabricated reply.
        None => StatusCode::BAD_GATEWAY.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_reaches_the_waiting_request() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let seen = sent.clone();
        let relay = McpRelay::new(move |msg| seen.lock().unwrap().push(msg));
        let call = relay.clone();
        let waiting = tokio::spawn(async move { call.call("tok".into(), "{}".into()).await });
        tokio::task::yield_now().await;
        let req_id = match sent.lock().unwrap().first() {
            Some(WorkerMsg::McpRequest { req_id, bearer, .. }) => {
                assert_eq!(bearer, "tok");
                *req_id
            }
            other => panic!("expected a relayed request, got {other:?}"),
        };
        relay.resolve(req_id, 200, "{\"ok\":true}".into());
        assert_eq!(
            waiting.await.unwrap(),
            Some((200, "{\"ok\":true}".to_string()))
        );
    }

    /// A dropped control link cannot answer, so the request must fail rather
    /// than wait out its timeout holding a slot.
    #[tokio::test]
    async fn abandoning_the_link_fails_waiting_requests() {
        let relay = McpRelay::new(|_| {});
        let call = relay.clone();
        let waiting = tokio::spawn(async move { call.call("tok".into(), "{}".into()).await });
        tokio::task::yield_now().await;
        relay.abandon();
        assert_eq!(waiting.await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_body_the_controller_would_take_is_relayed_and_a_larger_one_refused() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let seen = sent.clone();
        let relay = McpRelay::new(move |msg| seen.lock().unwrap().push(msg));
        let url = serve(relay.clone()).await.unwrap();
        let client = reqwest::Client::new();

        let padding = "x".repeat(MAX_BODY_BYTES - 64);
        let accepted = client.post(&url).bearer_auth("tok").body(format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"pad\":\"{padding}\"}}"
        ));
        let answered = tokio::spawn(accepted.send());
        let req_id = loop {
            if let Some(WorkerMsg::McpRequest { req_id, .. }) = sent.lock().unwrap().first() {
                break *req_id;
            }
            tokio::task::yield_now().await;
        };
        relay.resolve(req_id, 200, "{}".into());
        assert_eq!(answered.await.unwrap().unwrap().status(), 200);

        let refused = client
            .post(&url)
            .bearer_auth("tok")
            .body("x".repeat(MAX_BODY_BYTES + 1))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            sent.lock().unwrap().len(),
            1,
            "nothing oversized is relayed"
        );
    }

    #[tokio::test]
    async fn the_relay_refuses_more_than_it_can_hold() {
        let relay = McpRelay::new(|_| {});
        let mut waiting = Vec::new();
        for _ in 0..MAX_IN_FLIGHT {
            let call = relay.clone();
            waiting.push(tokio::spawn(async move {
                call.call("tok".into(), "{}".into()).await
            }));
            tokio::task::yield_now().await;
        }
        assert_eq!(relay.call("tok".into(), "{}".into()).await, None);
    }
}
