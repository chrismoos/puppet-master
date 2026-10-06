//! Transport-neutral client connection handling, shared by the unix
//! socket server and the WebSocket transport. PtyInput and PtyResize
//! are fire-and-forget; every other client message gets a
//! CommandResult.

use std::collections::HashMap;
use std::sync::Arc;

use pm_protocol::domain::{ClientEnvelope, ClientMsg, ServerMsg};
use prost::Message as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::daemon::{Daemon, DaemonError};

/// Outgoing messages buffered per connection; a client that stops
/// reading past this is disconnected rather than wedging the daemon.
pub const OUTGOING_CHANNEL_CAPACITY: usize = 4096;

#[derive(Default)]
pub struct ConnState {
    subscriber_task: Option<JoinHandle<()>>,
    pty_forward_tasks: HashMap<PtyAddress, PtyForwardTask>,
    authenticated_user_id: Option<u64>,
    /// The Host this client reached the daemon on, so forward links it
    /// renders point back at the origin it already uses. Unset on the
    /// unix socket, which has no request origin.
    client_host: Option<String>,
}

struct PtyForwardTask {
    handle: JoinHandle<()>,
    attach_seq: u64,
}

#[cfg(test)]
mod tests {
    use super::PtyAddress;

    #[test]
    fn session_and_terminal_forwarders_have_distinct_keys() {
        assert_ne!(PtyAddress::Session(7), PtyAddress::Terminal(7));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PtyAddress {
    Session(u64),
    Terminal(u64),
}

async fn abort_pty_forward(
    conn: &mut ConnState,
    address: PtyAddress,
    out_tx: &mpsc::Sender<ServerMsg>,
) {
    let Some(task) = conn.pty_forward_tasks.remove(&address) else {
        return;
    };
    task.handle.abort();
    let _ = out_tx
        .send(ServerMsg::CommandResult {
            seq: task.attach_seq,
            result: Err("attach canceled".into()),
            data: bytes::Bytes::new(),
        })
        .await;
}

impl ConnState {
    pub fn for_authenticated_user(user_id: u64) -> Self {
        Self {
            subscriber_task: None,
            pty_forward_tasks: HashMap::new(),
            authenticated_user_id: Some(user_id),
            client_host: None,
        }
    }

    pub fn with_client_host(mut self, client_host: Option<String>) -> Self {
        self.client_host = client_host;
        self
    }

    pub fn abort_all(&mut self) {
        if let Some(t) = self.subscriber_task.take() {
            t.abort();
        }
        for (_, task) in self.pty_forward_tasks.drain() {
            task.handle.abort();
        }
    }
}

impl Drop for ConnState {
    fn drop(&mut self) {
        self.abort_all();
    }
}

pub async fn handle_message(
    daemon: &Arc<Daemon>,
    envelope: ClientEnvelope,
    out_tx: &mpsc::Sender<ServerMsg>,
    conn: &mut ConnState,
) {
    let seq = envelope.seq;
    let reply = |result: Result<Option<u64>, String>| ServerMsg::CommandResult {
        seq,
        result,
        data: bytes::Bytes::new(),
    };

    match envelope.msg {
        ClientMsg::Subscribe { scope } => {
            if let Some(t) = conn.subscriber_task.take() {
                t.abort();
            }
            let d = daemon.clone();
            let tx = out_tx.clone();
            let client_host = conn.client_host.clone();
            if let Some(user_id) = conn.authenticated_user_id {
                let (mut snapshot, mut events, mut user_settings) =
                    daemon.subscribe_for_user(user_id);
                daemon.overlay_client_forward_urls(&mut snapshot.forwards, client_host.as_deref());
                let _ = out_tx.send(ServerMsg::Snapshot(snapshot)).await;
                let _ = out_tx.send(reply(Ok(None))).await;
                conn.subscriber_task = Some(tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            result = events.recv() => match result {
                                Ok(event) => {
                                    let event = d.event_for_client(event, client_host.as_deref());
                                    if d.event_in_scope(&event, scope)
                                        && tx.send(ServerMsg::Event(event)).await.is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    warn!(missed = n, user_id, "subscriber lagged, dropping event stream");
                                    break;
                                }
                                Err(_) => break,
                            },
                            result = user_settings.recv() => match result {
                                Ok(setting) => {
                                    if tx.send(ServerMsg::Event(
                                        pm_protocol::domain::Event::UserSettingChanged(setting)
                                    )).await.is_err() {
                                        break;
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    warn!(missed = n, user_id, "user-setting subscriber lagged");
                                    break;
                                }
                                Err(_) => break,
                            },
                        }
                    }
                }));
            } else {
                let (mut snapshot, mut events) = daemon.subscribe();
                daemon.overlay_client_forward_urls(&mut snapshot.forwards, client_host.as_deref());
                let _ = out_tx.send(ServerMsg::Snapshot(snapshot)).await;
                let _ = out_tx.send(reply(Ok(None))).await;
                conn.subscriber_task = Some(tokio::spawn(async move {
                    loop {
                        match events.recv().await {
                            Ok(event) => {
                                let event = d.event_for_client(event, client_host.as_deref());
                                if d.event_in_scope(&event, scope)
                                    && tx.send(ServerMsg::Event(event)).await.is_err()
                                {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!(missed = n, "subscriber lagged, dropping event stream");
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }));
            }
        }

        ClientMsg::SpawnSession {
            project_id,
            agent,
            task_title,
            task_prompt,
            cwd,
            permission_mode,
            worker_id,
            items_api,
            supervisor_api,
            model_profile_id,
            host,
            initial_cols,
            initial_rows,
        } => {
            let d = daemon.clone();
            let cwd = (!cwd.is_empty()).then_some(cwd);
            let initial_size = match (initial_cols, initial_rows) {
                (Some(cols), Some(rows))
                    if cols >= crate::mux::MIN_COLS && rows >= crate::mux::MIN_ROWS =>
                {
                    Some((cols, rows))
                }
                _ => None,
            };
            // Asking the host about the project path needs to await, so
            // it happens before the blocking launch rather than inside it.
            let prepared = async {
                let worker_id = if host.trim().is_empty() {
                    worker_id
                } else if worker_id.is_some() {
                    return Err(DaemonError::Rejected(
                        "host and worker_id cannot both be set".into(),
                    ));
                } else {
                    Some(d.resolve_spawn_host(project_id, &host)?)
                };
                d.ensure_spawnable(project_id, worker_id, cwd.as_deref())
                    .await?;
                Ok(worker_id)
            }
            .await;
            let result = match prepared {
                Err(e) => Err(e.to_string()),
                Ok(worker_id) => tokio::task::spawn_blocking(move || {
                    d.spawn_session_with_agent_override(
                        project_id,
                        agent,
                        &task_title,
                        &task_prompt,
                        cwd.as_deref(),
                        permission_mode,
                        worker_id,
                        items_api,
                        supervisor_api,
                        None,
                        model_profile_id,
                        initial_size,
                    )
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string())),
            };
            let _ = out_tx.send(reply(result.map(Some))).await;
        }

        ClientMsg::AttachPty { session_id } => {
            let address = PtyAddress::Session(session_id);
            abort_pty_forward(conn, address, out_tx).await;
            let daemon = daemon.clone();
            let tx = out_tx.clone();
            let handle = tokio::spawn(async move {
                let reply = |result: Result<Option<u64>, String>| ServerMsg::CommandResult {
                    seq,
                    result,
                    data: bytes::Bytes::new(),
                };
                let (replay, mut rx, guard) = match daemon.attach(session_id).await {
                    Ok(attach) => attach,
                    Err(error) => {
                        let _ = tx.send(reply(Err(error.to_string()))).await;
                        return;
                    }
                };
                let terminal = daemon.agent_terminal(session_id).ok();
                let terminal_id = terminal
                    .as_ref()
                    .map(|terminal| terminal.id)
                    .unwrap_or(session_id);
                let generation = terminal
                    .as_ref()
                    .map(|terminal| terminal.generation)
                    .unwrap_or(1);
                if tx
                    .send(ServerMsg::PtyOutput {
                        session_id,
                        terminal_id,
                        generation,
                        data: replay,
                        replay: true,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                if tx.send(reply(Ok(None))).await.is_err() {
                    return;
                }
                let _guard = guard;
                loop {
                    match rx.recv().await {
                        Ok(data) => {
                            if tx
                                .send(ServerMsg::PtyOutput {
                                    session_id,
                                    terminal_id,
                                    generation,
                                    data,
                                    replay: false,
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                session = session_id,
                                missed = n,
                                "viewer lagged behind pty output, detaching"
                            );
                            break;
                        }
                        Err(_) => break,
                    }
                }
            });
            conn.pty_forward_tasks.insert(
                address,
                PtyForwardTask {
                    handle,
                    attach_seq: seq,
                },
            );
        }

        ClientMsg::DetachPty { session_id } => {
            abort_pty_forward(conn, PtyAddress::Session(session_id), out_tx).await;
            let _ = out_tx.send(reply(Ok(None))).await;
        }

        ClientMsg::PtyInput { session_id, data } => {
            daemon.pty_input(session_id, data);
        }

        ClientMsg::PtyResize {
            session_id,
            cols,
            rows,
        } => {
            daemon.pty_resize(session_id, cols, rows);
        }

        ClientMsg::InterruptSession { session_id } => {
            let result = daemon
                .interrupt_session(session_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::KillSession { session_id } => {
            let result = daemon
                .kill_session(session_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::CreateBucket {
            name,
            allowed_worker_ids,
            default_worker_id,
            is_default,
        } => {
            let result = daemon
                .create_bucket_with_workers(
                    &name,
                    &allowed_worker_ids,
                    default_worker_id,
                    is_default,
                )
                .map(Some)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::DeleteBucket { id } => {
            let result = daemon
                .delete_bucket(id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::CreateProject {
            bucket_id,
            name,
            path,
            worker_id,
            allowed_worker_ids,
        } => {
            let result = daemon
                .create_project_with_workers(
                    bucket_id,
                    &name,
                    &path,
                    worker_id,
                    &allowed_worker_ids,
                )
                .map(Some)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::UpdateProject {
            project_id,
            path,
            permission_mode,
            worker_id,
        } => {
            let result = daemon
                .update_project(project_id, path.as_deref(), permission_mode, worker_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::DeleteProject { id } => {
            let result = daemon
                .delete_project(id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::OpenReview {
            session_id,
            worktree,
            base,
            head,
            pathspec,
            files,
            source_file,
            label,
            reset,
        } => {
            let ctx = crate::review::ReviewContext {
                worktree,
                base,
                head,
                pathspec,
                files: (!files.is_empty()).then_some(files),
                source_file,
                label,
            };
            let result = daemon
                .open_review(session_id, &ctx, reset)
                .await
                .map(|r| Some(r.id))
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::AddReviewComment {
            review_id,
            path,
            line,
            side,
            excerpt,
            body,
            send,
            anchor_snapshot_id,
            choice,
        } => {
            let result = daemon
                .add_review_comment(&crate::review::NewComment {
                    review_id,
                    path,
                    line,
                    side,
                    excerpt,
                    body,
                    send,
                    anchor_snapshot_id,
                    choice: choice.map(|c| *c),
                })
                .await
                .map(Some)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::EditReviewComment {
            message_id,
            body,
            choice,
        } => {
            let result = daemon
                .edit_review_comment(message_id, &body, choice.as_deref())
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::DeleteReviewThread { thread_id } => {
            let result = daemon
                .delete_review_thread(thread_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::SendReviewThreads {
            review_id,
            thread_ids,
        } => {
            let result = daemon
                .send_review_threads(review_id, &thread_ids)
                .await
                .map(|n| Some(n as u64))
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::ResolveReviewThread {
            thread_id,
            resolved,
        } => {
            let result = daemon
                .resolve_review_thread(thread_id, resolved)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::ReplyReviewThread { thread_id, body } => {
            let result = daemon
                .reply_review_thread(thread_id, &body)
                .await
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::PostReviewReply {
            thread_id,
            body,
            addressed,
        } => {
            // A reply posted over the client socket is attributed to no
            // session; the agent's own path goes through MCP.
            let result = daemon
                .post_review_reply(0, thread_id, &body, addressed)
                .await
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::AdvanceReview { review_id, rev } => {
            let result = match conn.authenticated_user_id {
                Some(user_id) => daemon
                    .advance_review(review_id, user_id, rev)
                    .await
                    .map(|_| None)
                    .map_err(|e| e.to_string()),
                None => Err("advancing a review needs an authenticated user".to_string()),
            };
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::FinishReview { review_id } => {
            let result = daemon
                .finish_review(review_id)
                .await
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::SetReviewViewerState(update) => {
            let result = match conn.authenticated_user_id {
                Some(user_id) => daemon
                    .set_review_viewer_state(user_id, &update)
                    .map(|_| None)
                    .map_err(|e| e.to_string()),
                None => Err("viewer state needs an authenticated user".to_string()),
            };
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::ListReviews {
            session_id,
            include_finished,
        } => {
            let result = daemon
                .list_reviews(session_id, include_finished)
                .map_err(|e| e.to_string());
            let msg = match result {
                Ok(reviews) => {
                    let reviews: Vec<serde_json::Value> =
                        reviews.iter().map(crate::review::review_json).collect();
                    ServerMsg::CommandResult {
                        seq,
                        result: Ok(None),
                        data: serde_json::to_vec(&reviews).unwrap_or_default().into(),
                    }
                }
                Err(e) => reply(Err(e)),
            };
            let _ = out_tx.send(msg).await;
        }

        ClientMsg::ResolveRev {
            session_id,
            worktree,
            rev,
        } => {
            let msg = match daemon.resolve_rev(session_id, &worktree, &rev).await {
                Ok(sha) => ServerMsg::CommandResult {
                    seq,
                    result: Ok(None),
                    data: sha.into_bytes().into(),
                },
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }

        ClientMsg::SetProjectWorkerPath {
            project_id,
            worker,
            path,
        } => {
            let result = daemon
                .resolve_project_path_worker(project_id, &worker)
                .and_then(|worker_id| {
                    daemon.set_project_worker_path(project_id, worker_id, path.as_deref())
                })
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::HookEvent {
            session_token,
            kind,
            detail,
            agent_session_id,
            transcript_path,
            background_work,
        } => {
            let msg = match daemon.handle_hook_event(
                &session_token,
                kind,
                &detail,
                &agent_session_id,
                &transcript_path,
                background_work,
            ) {
                Ok(nudge) => ServerMsg::CommandResult {
                    seq,
                    result: Ok(None),
                    data: nudge.map(bytes::Bytes::from).unwrap_or_default(),
                },
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }

        ClientMsg::ResumeSession { session_id } => {
            let d = daemon.clone();
            let result = tokio::task::spawn_blocking(move || d.resume_session(session_id))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()));
            let _ = out_tx.send(reply(result.map(Some))).await;
        }

        ClientMsg::SetBucketPermissionMode { bucket_id, mode } => {
            let result = daemon
                .set_bucket_permission_mode(bucket_id, mode)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::SetProjectPermissionMode { project_id, mode } => {
            let result = daemon
                .set_project_permission_mode(project_id, mode)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::SetBucketDefaultAgent { bucket_id, agent } => {
            let result = daemon
                .set_bucket_default_agent(bucket_id, agent)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::SetProjectDefaultAgent { project_id, agent } => {
            let result = daemon
                .set_project_default_agent(project_id, agent)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::CreateModelProfile { name, api_key } => {
            let result = daemon
                .create_model_profile(&name, api_key.as_deref())
                .map(Some)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::UpdateModelProfile {
            id,
            name,
            api_key,
            clear_api_key,
        } => {
            let result = daemon
                .update_model_profile(id, name.as_deref(), api_key.as_deref(), clear_api_key)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::DeleteModelProfile { id } => {
            let result = daemon
                .delete_model_profile(id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::SetModelProfileEndpoint {
            profile_id,
            dialect,
            model,
            base_url,
            background_model,
        } => {
            let result = daemon
                .set_model_profile_endpoint(
                    profile_id,
                    dialect,
                    &model,
                    &base_url,
                    &background_model,
                )
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::DeleteModelProfileEndpoint {
            profile_id,
            dialect,
        } => {
            let result = daemon
                .delete_model_profile_endpoint(profile_id, dialect)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::SetBucketModelProfile {
            bucket_id,
            model_profile_id,
        } => {
            let result = daemon
                .set_bucket_model_profile(bucket_id, model_profile_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::SetProjectModelProfile {
            project_id,
            model_profile_id,
        } => {
            let result = daemon
                .set_project_model_profile(project_id, model_profile_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }

        ClientMsg::CreateShell { session_id, title } => {
            let result = daemon
                .create_shell(session_id, &title)
                .map(Some)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::RestartTerminal { terminal_id } => {
            let result = daemon
                .restart_terminal(terminal_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::CloseTerminal { terminal_id } => {
            abort_pty_forward(conn, PtyAddress::Terminal(terminal_id), out_tx).await;
            let result = daemon
                .close_terminal(terminal_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::CloseForward { forward_id } => {
            let result = daemon
                .close_forward(forward_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::AttachTerminal { terminal_id } => {
            let address = PtyAddress::Terminal(terminal_id);
            abort_pty_forward(conn, address, out_tx).await;
            let daemon = daemon.clone();
            let tx = out_tx.clone();
            let handle = tokio::spawn(async move {
                let reply = |result: Result<Option<u64>, String>| ServerMsg::CommandResult {
                    seq,
                    result,
                    data: bytes::Bytes::new(),
                };
                let attach = daemon.attach_terminal(terminal_id).await;
                let (replay, mut rx, guard) = match attach {
                    Ok(attach) => attach,
                    Err(error) => {
                        let _ = tx.send(reply(Err(error.to_string()))).await;
                        return;
                    }
                };
                let terminal = daemon.terminal(terminal_id).ok();
                let session_id = terminal
                    .as_ref()
                    .map(|terminal| terminal.session_id)
                    .unwrap_or_default();
                let generation = terminal
                    .as_ref()
                    .map(|terminal| terminal.generation)
                    .unwrap_or_default();
                if tx
                    .send(ServerMsg::PtyOutput {
                        session_id,
                        terminal_id,
                        generation,
                        data: replay,
                        replay: true,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                if tx.send(reply(Ok(None))).await.is_err() {
                    return;
                }
                let _guard = guard;
                while let Ok(data) = rx.recv().await {
                    if tx
                        .send(ServerMsg::PtyOutput {
                            session_id,
                            terminal_id,
                            generation,
                            data,
                            replay: false,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            conn.pty_forward_tasks.insert(
                address,
                PtyForwardTask {
                    handle,
                    attach_seq: seq,
                },
            );
        }
        ClientMsg::DetachTerminal { terminal_id } => {
            abort_pty_forward(conn, PtyAddress::Terminal(terminal_id), out_tx).await;
            let _ = out_tx.send(reply(Ok(None))).await;
        }
        ClientMsg::TerminalInput { terminal_id, data } => daemon.terminal_input(terminal_id, data),
        ClientMsg::MarkSessionSeen { session_id } => {
            let result = daemon
                .mark_session_seen(session_id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::TerminalResize {
            terminal_id,
            cols,
            rows,
        } => daemon.terminal_resize(terminal_id, cols, rows),
        ClientMsg::TerminalTranscript {
            terminal_id,
            generation: _,
        } => {
            let _ = out_tx
                .send(reply(Err(format!(
                    "terminal {terminal_id} transcript transfer is not supported by this protocol"
                ))))
                .await;
        }
        ClientMsg::UpsertItem(write) => {
            let msg = match daemon.upsert_item(write.bucket_id, &write, None) {
                Ok((item, outcome, _)) => ServerMsg::CommandResult {
                    seq,
                    result: Ok(Some(item.id)),
                    data: serde_json::to_vec(&serde_json::json!({
                        "id": item.id,
                        "outcome": outcome.as_str(),
                    }))
                    .unwrap_or_default()
                    .into(),
                },
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::DeleteItem { bucket_id, id } => {
            let result = daemon
                .delete_item(bucket_id, id)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::SnoozeItem {
            bucket_id,
            id,
            until_unix_ms,
        } => {
            let result = daemon
                .snooze_item(bucket_id, id, until_unix_ms)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::ListItems(query) => {
            let msg = match daemon.list_items(&query) {
                Ok(items) => {
                    let items: Vec<serde_json::Value> =
                        items.iter().map(crate::daemon::item_json).collect();
                    ServerMsg::CommandResult {
                        seq,
                        result: Ok(None),
                        data: serde_json::to_vec(&items).unwrap_or_default().into(),
                    }
                }
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::ItemNotes { bucket_id, item_id } => {
            let msg = match daemon
                .get_item(bucket_id, item_id)
                .and_then(|item| Ok((item, daemon.item_notes(bucket_id, item_id)?)))
            {
                Ok((item, notes)) => {
                    let notes: Vec<serde_json::Value> =
                        notes.iter().map(crate::daemon::item_note_json).collect();
                    ServerMsg::CommandResult {
                        seq,
                        result: Ok(None),
                        data: serde_json::to_vec(&serde_json::json!({
                            "item": crate::daemon::item_json(&item),
                            "notes": notes,
                        }))
                        .unwrap_or_default()
                        .into(),
                    }
                }
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::SetSetting { key, value } => {
            let result = daemon
                .set_setting(&key, value.as_deref())
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::UpdateSessionApis {
            session_id,
            items_api,
            supervisor_api,
            role,
        } => {
            let result = daemon
                .update_session_role_apis(
                    session_id,
                    items_api,
                    supervisor_api,
                    role.or_else(|| {
                        supervisor_api.map(|v| {
                            if v {
                                pm_protocol::domain::SessionRole::Supervisor
                            } else {
                                pm_protocol::domain::SessionRole::Worker
                            }
                        })
                    }),
                )
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::ListInstructions {
            bucket_id,
            project_id,
        } => {
            let result = daemon
                .list_instructions(bucket_id, project_id)
                .and_then(|layers| {
                    let mut values = Vec::with_capacity(layers.len());
                    for layer in layers {
                        let history = daemon
                            .instruction_history(layer.id)?
                            .into_iter()
                            .map(|revision| {
                                serde_json::json!({
                                    "revision": revision.revision,
                                    "markdown": revision.markdown,
                                    "note": revision.note,
                                    "updated_at_unix_ms": revision.updated_at_unix_ms,
                                    "updated_by_session_id": revision.updated_by_session_id,
                                })
                            })
                            .collect::<Vec<_>>();
                        values.push(serde_json::json!({
                            "id": layer.id,
                            "bucket_id": layer.bucket_id,
                            "project_id": layer.project_id,
                            "target": layer.target.as_str(),
                            "markdown": layer.markdown,
                            "revision": layer.revision,
                            "updated_at_unix_ms": layer.updated_at_unix_ms,
                            "updated_by_session_id": layer.updated_by_session_id,
                            "history": history,
                        }));
                    }
                    serde_json::to_vec(&values)
                        .map_err(|e| crate::daemon::DaemonError::Rejected(e.to_string()))
                });
            let msg = match result {
                Ok(data) => ServerMsg::CommandResult {
                    seq,
                    result: Ok(None),
                    data: data.into(),
                },
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::GetEffectiveInstructions {
            bucket_id,
            project_id,
            role,
        } => {
            let result = daemon.effective_instructions(bucket_id, project_id, role);
            let msg = match result {
                Ok(data) => ServerMsg::CommandResult {
                    seq,
                    result: Ok(None),
                    data: data.into_bytes().into(),
                },
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::SetInstructions {
            bucket_id,
            project_id,
            target,
            markdown,
            expected_revision,
            note,
        } => {
            let result = daemon
                .set_instructions(
                    bucket_id,
                    project_id,
                    target,
                    &markdown,
                    expected_revision,
                    &note,
                    None,
                )
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::RevertInstructions {
            layer_id,
            revision,
            expected_revision,
            note,
        } => {
            let result = daemon
                .revert_instructions(layer_id, revision, expected_revision, &note, None)
                .map(|_| None)
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::RespondToItem {
            bucket_id,
            item_id,
            text,
            target,
        } => {
            let result = daemon
                .respond_to_item(bucket_id, item_id, &text, target)
                .await
                .map_err(|e| e.to_string());
            let _ = out_tx.send(reply(result)).await;
        }
        ClientMsg::ListSettings => {
            let msg = match daemon.settings() {
                Ok(settings) => ServerMsg::CommandResult {
                    seq,
                    result: Ok(None),
                    data: serde_json::to_vec(&settings).unwrap_or_default().into(),
                },
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::GetSession { session_id } => {
            let msg = match daemon.get_session_exact(session_id) {
                Ok(session) => session_page_result(seq, vec![session], String::new(), 1),
                Err(error) => reply(Err(error.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::ListEndedSessions { cursor, limit } => {
            let msg = match daemon.list_ended_sessions(&cursor, limit) {
                Ok(page) => session_page_result(seq, page.sessions, page.next_cursor, page.total),
                Err(error) => reply(Err(error.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::SearchSessions {
            query,
            cursor,
            limit,
        } => {
            let msg = match daemon.search_sessions(&query, &cursor, limit) {
                Ok(page) => session_page_result(seq, page.sessions, page.next_cursor, page.total),
                Err(error) => reply(Err(error.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
        ClientMsg::SessionForwardInventory { session_token } => {
            let msg = match daemon.forward_inventory_for_token(&session_token) {
                Ok(inventory) => ServerMsg::CommandResult {
                    seq,
                    result: Ok(None),
                    data: bytes::Bytes::from(inventory),
                },
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }

        ClientMsg::ListWorkspaces => {
            let msg = match daemon.all_workspaces() {
                Ok(workspaces) => {
                    let items: Vec<serde_json::Value> = workspaces
                        .into_iter()
                        .map(|w| {
                            serde_json::json!({
                                "id": w.id,
                                "name": w.name,
                                "layout": serde_json::from_str::<serde_json::Value>(&w.layout_json)
                                    .unwrap_or(serde_json::Value::Null),
                                "position": w.position,
                            })
                        })
                        .collect();
                    ServerMsg::CommandResult {
                        seq,
                        result: Ok(None),
                        data: serde_json::to_vec(&items).unwrap_or_default().into(),
                    }
                }
                Err(e) => reply(Err(e.to_string())),
            };
            let _ = out_tx.send(msg).await;
        }
    }
}

fn session_page_result(
    seq: u64,
    sessions: Vec<pm_protocol::domain::Session>,
    next_cursor: String,
    total: u64,
) -> ServerMsg {
    let page = pm_protocol::wire::SessionPage {
        sessions: sessions.into_iter().map(Into::into).collect(),
        next_cursor,
        total,
    };
    ServerMsg::CommandResult {
        seq,
        result: Ok(None),
        data: page.encode_to_vec().into(),
    }
}
