//! Review orchestration: opening one, capturing snapshots, resolving
//! anchors against the live tree, dispatching threads to the agent,
//! and rendering the diff a reviewer reads.

use pm_protocol::domain::{
    Event, Review, ReviewAnchorStatus, ReviewAuthor, ReviewChoiceAnswer, ReviewChoiceSelect,
    ReviewMessage, ReviewMode, ReviewRevision, ReviewSide, ReviewSnapshotKind, ReviewState,
    ReviewThread, ReviewThreadState, ReviewViewerState, ReviewViewerStateUpdate,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tracing::{debug, info, warn};

use crate::daemon::{Daemon, DaemonError};
use crate::review_diff;

/// How many lines of today's file to show around an anchored line.
const EXCERPT_CONTEXT: u32 = 4;

/// Marks a skip that came from the base side. A capture reports what it
/// left out on its own side only, so the two are kept apart in the one
/// list the page reads.
const BASE_SKIP: &str = "unreadable at the base: ";

/// What the reviewer's page needs in one read.
#[derive(Debug, Clone)]
pub struct ReviewDetail {
    pub review: Review,
    pub threads: Vec<ReviewThread>,
    pub revisions: Vec<ReviewRevision>,
    pub viewer: ReviewViewerState,
    pub files: Vec<String>,
    /// Untracked files left out, with the reason.
    pub skipped: Vec<(String, String)>,
    /// Newest revision available, so the page can offer to advance.
    pub latest_rev: u32,
    /// Files changed between the reader's pinned revision and the
    /// newest one.
    pub pending_files: Vec<String>,
    /// The worktree this review was opened against is no longer readable,
    /// so what is shown is the newest captured revision rather than the
    /// live tree. Everything already recorded still reads.
    pub detached: bool,
}

/// Where a review reads and what range it covers, as the caller states
/// it. PM does not infer any of this: a session's working directory is
/// fixed at spawn and the agent moves, so anything derived from it is a
/// guess that renders convincingly when wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewContext {
    /// Absolute path of the tree to read.
    pub worktree: String,
    /// Resolved SHA of the base side.
    pub base: String,
    /// Resolved SHA of the head side; empty tracks the working tree.
    pub head: String,
    pub pathspec: Vec<String>,
    /// When set, exactly these paths are the review's scope.
    pub files: Option<Vec<String>>,
    /// File mode: the document under review, which need not be in any
    /// repository. Its baseline is empty, so it reads as all added.
    pub source_file: String,
    /// Human label for the row and the page.
    pub label: String,
}

impl ReviewContext {
    /// The review's identity. Resolved SHAs plus scope, so a review
    /// keeps meaning what it meant even after the refs behind it move.
    pub fn range_key(&self) -> String {
        if !self.source_file.is_empty() {
            return format!("file:{}", self.source_file);
        }
        let scope = match &self.files {
            Some(files) => format!(" files:{}", files.join(",")),
            None if self.pathspec.is_empty() => String::new(),
            None => format!(" -- {}", self.pathspec.join(" ")),
        };
        let head = if self.head.is_empty() {
            "WORKING"
        } else {
            &self.head
        };
        format!("{}..{}{scope}", self.base, head)
    }
}

/// A comment the reviewer is placing. Grouped rather than passed as
/// seven positional arguments, which is easy to get wrong at the call
/// site and impossible to read.
#[derive(Debug, Clone)]
pub struct NewComment {
    pub review_id: u64,
    pub path: String,
    pub line: u32,
    pub side: ReviewSide,
    /// The hunk the comment was written against.
    pub excerpt: String,
    pub body: String,
    /// Sends it immediately rather than leaving it a draft.
    pub send: bool,
    /// The snapshot the reviewer's page was rendering, which is what
    /// `line` counts against. `None` from an agent or an older client,
    /// and then the tree is captured fresh.
    pub anchor_snapshot_id: Option<u64>,
    /// Set when the comment answers a marked option list.
    pub choice: Option<ReviewChoiceAnswer>,
}

/// A rendered view together with the snapshot its right-hand side came
/// from. A comment written against that render anchors to the snapshot
/// rather than to the tree as it stands when the comment arrives.
#[derive(Debug, Clone)]
pub struct RenderedView<T> {
    pub content: T,
    pub snapshot_id: Option<u64>,
}

/// The two sides of a view, with the snapshot backing the right-hand
/// one when that side has a durable identity.
struct ViewSides {
    old: BTreeMap<String, Vec<u8>>,
    new: BTreeMap<String, Vec<u8>>,
    snapshot_id: Option<u64>,
}

/// Which side of a review a capture reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureKind {
    /// The immutable base. Captured once, at open.
    Base,
    /// The head side, which is the working tree unless the range froze.
    Head,
}

/// File mode's scope is the one document, named relative to the
/// directory that holds it.
fn file_mode_path(source_file: &str) -> String {
    Path::new(source_file)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| source_file.to_string())
}

/// What to call a review nobody named.
fn default_label(ctx: &ReviewContext) -> String {
    if !ctx.source_file.is_empty() {
        return file_mode_path(&ctx.source_file);
    }
    let base = ctx.base.chars().take(8).collect::<String>();
    if ctx.head.is_empty() {
        format!("since {base}")
    } else {
        format!("{base}..{}", ctx.head.chars().take(8).collect::<String>())
    }
}

/// How long the daemon waits between checks while holding a review
/// call open. Short enough that a reply feels immediate, cheap enough
/// that holding one open costs nothing.
const REVIEW_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

const REVIEW_WAKE_MIN_INTERVAL_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewWakeReason {
    CommentsSent(u32),
    Finished,
}

#[derive(Debug, Default)]
pub(crate) struct ReviewWakeRuntime {
    waiting: std::sync::Mutex<std::collections::HashMap<u64, usize>>,
    last_wake_unix_ms: std::sync::Mutex<std::collections::HashMap<u64, i64>>,
}

impl ReviewWakeRuntime {
    pub(crate) fn is_waiting(&self, session_id: u64) -> bool {
        self.waiting
            .lock()
            .unwrap()
            .get(&session_id)
            .copied()
            .unwrap_or(0)
            > 0
    }

    pub(crate) fn increment_waiting(&self, session_id: u64) {
        let mut waiting = self.waiting.lock().unwrap();
        *waiting.entry(session_id).or_insert(0) += 1;
    }

    pub(crate) fn decrement_waiting(&self, session_id: u64) {
        let mut waiting = self.waiting.lock().unwrap();
        if let Some(count) = waiting.get_mut(&session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                waiting.remove(&session_id);
            }
        }
    }

    pub(crate) fn throttle_allows(&self, session_id: u64, now: i64) -> bool {
        let last = self
            .last_wake_unix_ms
            .lock()
            .unwrap()
            .get(&session_id)
            .copied()
            .unwrap_or(0);
        now.saturating_sub(last) >= REVIEW_WAKE_MIN_INTERVAL_MS
    }

    pub(crate) fn record_wake(&self, session_id: u64, now: i64) {
        self.last_wake_unix_ms
            .lock()
            .unwrap()
            .insert(session_id, now);
    }
}

pub(crate) struct ReviewWaitGuard<'a> {
    runtime: &'a ReviewWakeRuntime,
    session_id: u64,
}

impl Drop for ReviewWaitGuard<'_> {
    fn drop(&mut self) {
        self.runtime.decrement_waiting(self.session_id);
    }
}

/// The outcome of waiting for review work.
#[derive(Debug, Clone)]
pub enum ReviewWait {
    Event(Box<ReviewEvent>),
    /// Every review this session answers is finished.
    Finished,
    /// Nothing arrived inside the window; the agent should call back.
    TimedOut,
}

/// One queued piece of work for the agent.
#[derive(Debug, Clone)]
pub struct ReviewEvent {
    pub event_id: u64,
    pub review: Review,
    pub thread: ReviewThread,
}

/// A review as the CLI and the web API see it.
pub fn review_json(r: &Review) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "session_id": r.session_id,
        "project_id": r.project_id,
        "worker_id": r.worker_id,
        "worktree": r.worktree,
        "mode": match r.mode { ReviewMode::File => "file", _ => "range" },
        "base": r.base,
        "head": r.head,
        "pathspec": r.pathspec,
        "source_file": r.source_file,
        "label": r.label,
        "range_key": r.range_key,
        "state": match r.state { ReviewState::Finished => "finished", _ => "open" },
        "revision": r.revision,
        "draft_count": r.draft_count,
        "open_count": r.open_count,
        "answered_count": r.answered_count,
        "resolved_count": r.resolved_count,
        "created_at_unix_ms": r.created_at_unix_ms,
        "updated_at_unix_ms": r.updated_at_unix_ms,
    })
}

pub fn thread_json(t: &ReviewThread) -> serde_json::Value {
    serde_json::json!({
        "id": t.id,
        "review_id": t.review_id,
        "path": t.path,
        "line": t.line,
        "side": match t.side { ReviewSide::Left => "left", _ => "right" },
        "excerpt": t.excerpt,
        "current_line": t.current_line,
        "anchor_status": match t.anchor_status {
            ReviewAnchorStatus::Same => "same",
            ReviewAnchorStatus::Moved => "moved",
            ReviewAnchorStatus::Changed => "changed",
            ReviewAnchorStatus::Unknown => "unknown",
        },
        "current_excerpt": t.current_excerpt,
        "state": match t.state {
            ReviewThreadState::Draft => "draft",
            ReviewThreadState::Sent => "sent",
            ReviewThreadState::Answered => "answered",
            ReviewThreadState::Resolved => "resolved",
        },
        "created_rev": t.created_rev,
        "changed_ahead": t.changed_ahead,
        "created_at_unix_ms": t.created_at_unix_ms,
        "messages": t.messages.iter().map(message_json).collect::<Vec<_>>(),
    })
}

pub fn message_json(m: &ReviewMessage) -> serde_json::Value {
    serde_json::json!({
        "id": m.id,
        "thread_id": m.thread_id,
        "author": match m.author { ReviewAuthor::Session => "session", _ => "user" },
        "session_id": m.session_id,
        "body": m.body,
        "addressed": m.addressed,
        "revision": m.revision,
        "changes_rev": m.changes_rev,
        "changed_files": m.changed_files,
        "created_at_unix_ms": m.created_at_unix_ms,
        "choice": m.choice.as_ref().map(choice_answer_json),
    })
}

pub fn choice_answer_json(c: &ReviewChoiceAnswer) -> serde_json::Value {
    serde_json::json!({
        "choice_id": c.choice_id,
        "select": match c.select {
            ReviewChoiceSelect::Many => "many",
            ReviewChoiceSelect::One => "one",
        },
        "option_ids": c.option_ids,
        "option_labels": c.option_labels,
        "other_text": c.other_text,
        "notes": c.notes,
    })
}

pub fn revision_json(r: &ReviewRevision) -> serde_json::Value {
    serde_json::json!({
        "rev": r.rev,
        "kind": match r.kind { ReviewSnapshotKind::Received => "received", _ => "sent" },
        "files": r.files,
        "created_at_unix_ms": r.created_at_unix_ms,
    })
}

pub fn viewer_json(v: &ReviewViewerState) -> serde_json::Value {
    serde_json::json!({
        "pinned_rev": v.pinned_rev,
        "view": v.view,
        "layout": v.layout,
        "context": v.context,
        "viewed_files": v.viewed_files,
        "preview_off_files": v.preview_off_files,
        "last_thread_id": v.last_thread_id,
        "scroll": v.scroll,
        "file_list_collapsed": v.file_list_collapsed,
        "drafts": v.drafts,
    })
}

impl Daemon {
    /// Opens a review for a target, or attaches to the one already
    /// covering it. Identity is worktree plus normalized range, so
    /// reopening the same range from a different session continues the
    /// same conversation rather than starting a parallel one.
    pub async fn open_review(
        &self,
        session_id: u64,
        ctx: &ReviewContext,
        reset: bool,
    ) -> Result<Review, DaemonError> {
        let session = self.storage.get_session(session_id)?;
        let mut ctx = ctx.clone();
        // File mode's worktree is only ever the directory holding the
        // document, so derive it rather than making the caller know
        // that. Passing the wrong one produced an empty review.
        if ctx.worktree.trim().is_empty() && !ctx.source_file.is_empty() {
            ctx.worktree = Path::new(&ctx.source_file)
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
        }
        let ctx = &ctx;
        if ctx.worktree.trim().is_empty() {
            return Err(DaemonError::Rejected(
                "a review needs the worktree it reads; a session's launch directory is not it"
                    .into(),
            ));
        }
        // A document that is not where the review would read it is a
        // mistake worth reporting. Left alone it captures nothing and
        // opens a review of a blank page, which looks like the feature
        // is broken rather than like a wrong argument.
        if !ctx.source_file.is_empty() {
            let expected = Path::new(&ctx.worktree).join(file_mode_path(&ctx.source_file));
            let op = crate::review_repo::RepoOp::ReadFile {
                worktree: ctx.worktree.clone(),
                rev: String::new(),
                path: file_mode_path(&ctx.source_file),
            };
            if matches!(
                self.repo_op(session.worker_id, &op).await?,
                crate::review_repo::RepoAnswer::ReadFile(None)
            ) {
                return Err(DaemonError::Rejected(format!(
                    "{} not found; a file review reads <worktree>/<file name>, so it looked for {}",
                    ctx.source_file,
                    expected.display()
                )));
            }
        }
        if ctx.source_file.is_empty() && ctx.base.trim().is_empty() {
            return Err(DaemonError::Rejected(
                "a review needs a resolved base SHA; resolve the ref before opening".into(),
            ));
        }

        let range_key = ctx.range_key();
        let existing = self.storage.find_review(&ctx.worktree, &range_key)?;
        let review = match existing {
            Some(review) => {
                if reset {
                    self.storage.reset_review(review.id)?;
                }
                // A review outlives its opening session, so point it at
                // whoever is answering now.
                self.storage.set_review_session(review.id, session_id)?;
                if review.state == ReviewState::Finished {
                    self.storage
                        .set_review_state(review.id, ReviewState::Open)?;
                }
                self.storage.get_review(review.id)?
            }
            None => self.storage.create_review(&Review {
                session_id,
                project_id: session.project_id,
                worker_id: session.worker_id,
                worktree: ctx.worktree.clone(),
                mode: if ctx.source_file.is_empty() {
                    ReviewMode::Range
                } else {
                    ReviewMode::File
                },
                base: ctx.base.clone(),
                head: ctx.head.clone(),
                pathspec: ctx.pathspec.clone(),
                source_file: ctx.source_file.clone(),
                label: if ctx.label.is_empty() {
                    default_label(ctx)
                } else {
                    ctx.label.clone()
                },
                range_key,
                explicit_files: ctx.files.clone().unwrap_or_default(),
                ..Default::default()
            })?,
        };

        // A review snapshots at open, so the reader is pinned from the
        // first frame even on a session that is already mid-task. The
        // base side is captured once here and never re-read, because it
        // cannot change.
        if review.revision == 0 {
            if let Err(error) = self.capture_first_revision(&review).await {
                // The row exists but holds nothing to review. Left open it
                // would keep the session waiting on it forever, since a
                // wait ends only when no review is open.
                self.storage
                    .set_review_state(review.id, ReviewState::Finished)?;
                return Err(error);
            }
        }

        let review = self.storage.get_review(review.id)?;
        info!(review = review.id, worktree = %review.worktree, "review opened");
        self.publish(Event::ReviewChanged(review.clone()));
        Ok(review)
    }

    /// Captures a new review's base and first head. File mode's baseline
    /// is deliberately empty, so the whole document reads as added.
    async fn capture_first_revision(&self, review: &Review) -> Result<(), DaemonError> {
        if review.source_file.is_empty() {
            let base = self.capture(review, CaptureKind::Base, &[]).await?;
            self.storage.set_review_base_snapshot(review.id, base)?;
        }
        let commented = self.commented_paths(review.id)?;
        let head = self.capture(review, CaptureKind::Head, &commented).await?;
        self.storage
            .record_review_revision(review.id, 1, ReviewSnapshotKind::Sent, head, &[])?;
        self.storage.set_review_revision(review.id, 1)?;
        Ok(())
    }

    /// Resolves a revision on the host that holds the tree. The MCP
    /// contract requires SHAs, but a client cannot resolve a ref
    /// against a worktree it cannot reach.
    pub async fn resolve_rev(
        &self,
        session_id: u64,
        worktree: &str,
        rev: &str,
    ) -> Result<String, DaemonError> {
        let session = self.storage.get_session(session_id)?;
        let op = crate::review_repo::RepoOp::Resolve {
            worktree: worktree.to_string(),
            rev: rev.to_string(),
        };
        match self.repo_op(session.worker_id, &op).await? {
            crate::review_repo::RepoAnswer::Resolve(Some(sha)) => Ok(sha),
            crate::review_repo::RepoAnswer::Resolve(None) => Err(DaemonError::Rejected(format!(
                "{rev} does not resolve in {worktree}"
            ))),
            _ => Err(DaemonError::Rejected("expected a resolve answer".into())),
        }
    }

    /// The dashboard address for a review, so the agent can hand the
    /// reviewer a link rather than an id.
    pub fn review_url(&self, review_id: u64) -> String {
        match self.agent_mcp_base_url() {
            Some(base) => format!("{base}/#/review/{review_id}"),
            None => format!(
                "{}://127.0.0.1:{}/#/review/{review_id}",
                self.http_scheme(),
                self.http_port()
            ),
        }
    }

    /// The read a review runs against its tree, with the blob SHAs the
    /// caller already holds so their bytes are left out of the answer.
    fn capture_op(
        &self,
        review: &Review,
        kind: CaptureKind,
        extra: &[String],
        known: BTreeSet<String>,
    ) -> crate::review_repo::RepoOp {
        let mut files = if review.explicit_files.is_empty() {
            None
        } else {
            Some(review.explicit_files.clone())
        };
        if !review.source_file.is_empty() {
            // File mode's scope is the one document, named relative to
            // the directory that holds it.
            files = Some(vec![file_mode_path(&review.source_file)]);
        }
        crate::review_repo::RepoOp::Capture(crate::review_repo::CaptureOp {
            worktree: review.worktree.clone(),
            base: review.base.clone(),
            head: review.head.clone(),
            pathspec: review.pathspec.clone(),
            files,
            known,
            at_base: kind == CaptureKind::Base,
            // A comment must have an anchor for its own file even when
            // that file is not otherwise in scope, and it keeps needing
            // one after the edit it asked for stops the file differing.
            extra: extra.to_vec(),
        })
    }

    /// Runs a capture on the host holding the review's tree and stores
    /// the manifest it returns.
    async fn capture(
        &self,
        review: &Review,
        kind: CaptureKind,
        extra: &[String],
    ) -> Result<u64, DaemonError> {
        // The reader withholds bytes for blobs already stored, so a
        // later round ships a manifest and only what actually changed.
        let known = self.storage.latest_snapshot_shas(review.id)?;
        let op = self.capture_op(review, kind, extra, known);
        let answer = self.repo_op(review.worker_id, &op).await?;
        let crate::review_repo::RepoAnswer::Capture(answer) = answer else {
            return Err(DaemonError::Rejected("expected a capture answer".into()));
        };
        // What the reader left out, so the page can report what it is
        // not showing rather than silently omitting it. Base-side skips
        // are not this capture's to refresh, so they are kept for as
        // long as their file is still in the review.
        let mut skipped: Vec<(String, String)> = self
            .storage
            .review_skipped(review.id)?
            .into_iter()
            .filter(|(path, reason)| {
                reason.starts_with(BASE_SKIP) && answer.files.iter().any(|f| &f.path == path)
            })
            .collect();
        skipped.extend(answer.skipped.iter().cloned());
        skipped.sort();
        self.storage.set_review_skipped(review.id, &skipped)?;
        let manifest: Vec<(String, String, Option<Vec<u8>>)> = answer
            .files
            .into_iter()
            .map(|f| (f.path, f.sha, f.content))
            .collect();
        Ok(self.storage.put_review_manifest(review.id, &manifest)?)
    }

    fn snapshot_contents(
        &self,
        snapshot_id: u64,
    ) -> Result<BTreeMap<String, Vec<u8>>, DaemonError> {
        let manifest = self.storage.snapshot_manifest(snapshot_id)?;
        let mut out = BTreeMap::new();
        for (path, sha) in manifest {
            // A manifest is only written once its blobs are stored, so a
            // blob that is gone is a broken snapshot, not an empty file.
            let Some(bytes) = self.storage.read_blob(&sha)? else {
                return Err(DaemonError::Rejected(format!(
                    "review snapshot {snapshot_id} lost the content of {path}"
                )));
            };
            out.insert(path, bytes);
        }
        Ok(out)
    }

    /// The base side, captured once at open and never re-read because
    /// it cannot change.
    fn base_contents(&self, review: &Review) -> Result<BTreeMap<String, Vec<u8>>, DaemonError> {
        match self.storage.review_base_snapshot(review.id)? {
            Some(id) => self.snapshot_contents(id),
            None => Ok(BTreeMap::new()),
        }
    }

    /// The live tree, captured fresh. The capture is durable, so its id
    /// identifies the revision the reader is about to be shown and can
    /// anchor a comment written against it.
    async fn live_contents(
        &self,
        review: &Review,
    ) -> Result<(u64, BTreeMap<String, Vec<u8>>), DaemonError> {
        let (snapshot, contents, _) = self.live_or_captured(review).await?;
        Ok((snapshot, contents))
    }

    /// The live tree, or the newest capture when the worktree it was
    /// opened against has gone. A review outlives its branch: the
    /// worktree is removed once the work merges, and the reader still
    /// has to be able to open what they commented on. Everything needed
    /// was stored at capture time, so only a fresh capture needs the
    /// tree. The flag says which of the two this is.
    async fn live_or_captured(
        &self,
        review: &Review,
    ) -> Result<(u64, BTreeMap<String, Vec<u8>>, bool), DaemonError> {
        let commented = self.commented_paths(review.id)?;
        match self.capture(review, CaptureKind::Head, &commented).await {
            Ok(snapshot) => Ok((snapshot, self.snapshot_contents(snapshot)?, false)),
            Err(error) => {
                let Some(snapshot) = self.newest_snapshot(review.id) else {
                    return Err(error);
                };
                warn!(
                    review = review.id,
                    worktree = %review.worktree,
                    %error,
                    "review worktree unreadable, serving the newest capture"
                );
                Ok((snapshot, self.snapshot_contents(snapshot)?, true))
            }
        }
    }

    /// The most recently captured tree for a review, whichever side it
    /// came from, used when the worktree can no longer be read.
    fn newest_snapshot(&self, review_id: u64) -> Option<u64> {
        self.storage
            .review_revisions(review_id)
            .ok()?
            .into_iter()
            .max_by_key(|r| (r.rev, r.kind == ReviewSnapshotKind::Received, r.id))
            .map(|r| r.snapshot_id)
    }

    /// The live tree, guaranteed to include every file a thread is
    /// anchored to. A comment whose file has since left the diff still
    /// has to be locatable, or it loses both its line and its excerpt.
    async fn live_with_threads(
        &self,
        review: &Review,
        threads: &[ReviewThread],
    ) -> Result<(BTreeMap<String, Vec<u8>>, bool), DaemonError> {
        let (_, mut live, detached) = self.live_or_captured(review).await?;
        for thread in threads {
            if live.contains_key(&thread.path) {
                continue;
            }
            let op = crate::review_repo::RepoOp::ReadFile {
                worktree: review.worktree.clone(),
                rev: review.head.clone(),
                path: thread.path.clone(),
            };
            // One unreadable file is not worth losing the review over: the
            // thread keeps the excerpt it was written against.
            match self.repo_op(review.worker_id, &op).await {
                Ok(crate::review_repo::RepoAnswer::ReadFile(bytes)) => {
                    live.insert(thread.path.clone(), bytes.unwrap_or_default());
                }
                Ok(_) => {}
                Err(error) if detached => {
                    debug!(
                        review = review.id,
                        path = %thread.path,
                        %error,
                        "commented file unreadable in a detached review"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok((live, detached))
    }

    fn snapshot_for(&self, review_id: u64, rev: u32, kind: ReviewSnapshotKind) -> Option<u64> {
        self.storage
            .review_revisions(review_id)
            .ok()?
            .into_iter()
            .find(|r| r.rev == rev && r.kind == kind)
            .map(|r| r.snapshot_id)
    }

    /// Renders the diff for a view selector.
    ///
    /// An empty view is the live working tree. `round:N` is the pair
    /// that shows exactly what the agent changed in round N; `delta:N`
    /// is round to round; `cum:N` and `sent:N` run from the base.
    pub async fn review_diff(
        &self,
        review_id: u64,
        view: &str,
        context: usize,
        file: Option<&str>,
    ) -> Result<RenderedView<String>, DaemonError> {
        let review = self.storage.get_review(review_id)?;
        let sides = self.sides_for_view(&review, view).await?;
        let (old, new) = match file {
            Some(f) => (
                sides.old.into_iter().filter(|(p, _)| p == f).collect(),
                sides.new.into_iter().filter(|(p, _)| p == f).collect(),
            ),
            None => (sides.old, sides.new),
        };
        let threads = self.storage.review_threads(review_id)?;
        let anchors = self.anchors_for_view(&threads, &new)?;
        Ok(RenderedView {
            content: review_diff::diff_manifests(&old, &new, context, &anchors),
            snapshot_id: sides.snapshot_id,
        })
    }

    /// The newest snapshot taken of a review's tree, whatever produced
    /// it. Rendering the working tree captures one and so does an agent
    /// reply, so this is the most recent state of the tree the daemon
    /// has actually observed, and the thing a later read is stale
    /// against.
    pub fn latest_review_snapshot(&self, review_id: u64) -> Result<Option<u64>, DaemonError> {
        Ok(self.storage.newest_review_snapshot(review_id)?)
    }

    /// Paths whose live content no longer matches what `since` holds.
    ///
    /// This is the same read the renderer runs, with the snapshot's own
    /// blob SHAs declared as known, so a tree that has not moved sends
    /// no bytes back at all and nothing is written to the store. Only
    /// the path-and-SHA list is used, which is what makes it cheap
    /// enough to run on an interval while someone is reading, and what
    /// makes it behave the same for a worktree on this host and one
    /// reached through a worker.
    ///
    /// A tree that cannot be read has not been seen to move: a review
    /// whose worktree is gone serves its last capture, and reporting
    /// that as a change would ask the reader to refresh onto nothing.
    pub async fn review_tree_changes(
        &self,
        review_id: u64,
        since: u64,
    ) -> Result<Vec<String>, DaemonError> {
        let review = self.storage.get_review(review_id)?;
        let was = self.storage.snapshot_manifest(since)?;
        let commented = self.commented_paths(review_id)?;
        let known: BTreeSet<String> = was.values().cloned().collect();
        let op = self.capture_op(&review, CaptureKind::Head, &commented, known);
        let answer = match self.repo_op(review.worker_id, &op).await {
            Ok(crate::review_repo::RepoAnswer::Capture(answer)) => answer,
            Ok(_) => return Err(DaemonError::Rejected("expected a capture answer".into())),
            Err(error) => {
                debug!(
                    review = review_id,
                    worktree = %review.worktree,
                    %error,
                    "review worktree unreadable while checking for changes"
                );
                return Ok(Vec::new());
            }
        };
        let now: BTreeMap<String, String> = answer
            .files
            .into_iter()
            .map(|file| (file.path, file.sha))
            .collect();
        let mut changed: Vec<String> = now
            .iter()
            .filter(|(path, sha)| was.get(*path) != Some(sha))
            .map(|(path, _)| path.clone())
            .chain(was.keys().filter(|path| !now.contains_key(*path)).cloned())
            .collect();
        changed.sort();
        changed.dedup();
        Ok(changed)
    }

    async fn sides_for_view(&self, review: &Review, view: &str) -> Result<ViewSides, DaemonError> {
        let mut sides = self.raw_sides_for_view(review, view).await?;
        self.fill_gaps_from_base(review, &mut sides).await?;
        Ok(sides)
    }

    /// Gives both sides an entry for every path either of them holds.
    ///
    /// A snapshot lists what differed from the base when it was taken,
    /// so a path one side is missing held the base content at that
    /// moment: a file first touched after the review opened is absent
    /// from the base, and one whose edit was undone drops out of the
    /// head. Either way the answer is the base, and a side left absent
    /// would be reported as unreadable.
    async fn fill_gaps_from_base(
        &self,
        review: &Review,
        sides: &mut ViewSides,
    ) -> Result<(), DaemonError> {
        let gaps: Vec<String> = sides
            .old
            .keys()
            .filter(|p| !sides.new.contains_key(*p))
            .chain(sides.new.keys().filter(|p| !sides.old.contains_key(*p)))
            .cloned()
            .collect();
        if gaps.is_empty() {
            return Ok(());
        }
        // File mode has no base to read on purpose, so its baseline is
        // an empty file rather than an unknown one and the document
        // reads as added.
        let base = if review.source_file.is_empty() && !review.base.is_empty() {
            self.base_covering(review, &gaps).await?
        } else {
            gaps.iter().map(|p| (p.clone(), Vec::new())).collect()
        };
        for path in gaps {
            let Some(bytes) = base.get(&path) else {
                continue;
            };
            for side in [&mut sides.old, &mut sides.new] {
                side.entry(path.clone()).or_insert_with(|| bytes.clone());
            }
        }
        Ok(())
    }

    /// The base side, extended to cover `paths`. The base snapshot's
    /// file set is whatever differed when the review opened, so a file
    /// first touched afterwards is not in it. What this reads is folded
    /// into that snapshot, which stays true because the base cannot
    /// change, so the read happens once per path.
    ///
    /// This is `CaptureOp::extra` pointed the other way: that keeps a
    /// file on the head side once it stops differing, this puts one on
    /// the base side once it starts.
    async fn base_covering(
        &self,
        review: &Review,
        paths: &[String],
    ) -> Result<BTreeMap<String, Vec<u8>>, DaemonError> {
        let mut base = self.base_contents(review)?;
        let missing: Vec<String> = paths
            .iter()
            .filter(|p| !base.contains_key(*p))
            .cloned()
            .collect();
        if missing.is_empty() {
            return Ok(base);
        }
        let op = crate::review_repo::RepoOp::Capture(crate::review_repo::CaptureOp {
            worktree: review.worktree.clone(),
            base: review.base.clone(),
            head: review.head.clone(),
            pathspec: Vec::new(),
            files: Some(missing.clone()),
            known: Default::default(),
            at_base: true,
            extra: Vec::new(),
        });
        let crate::review_repo::RepoAnswer::Capture(answer) =
            self.repo_op(review.worker_id, &op).await?
        else {
            return Err(DaemonError::Rejected("expected a capture answer".into()));
        };
        let read: Vec<(String, Vec<u8>)> = answer
            .files
            .into_iter()
            .map(|f| (f.path, f.content.unwrap_or_default()))
            .collect();
        if let Some(snapshot) = self.storage.review_base_snapshot(review.id)? {
            self.storage.extend_review_snapshot(snapshot, &read)?;
        }
        // A base read that failed leaves the path with no left-hand
        // side at all, so say so rather than let it draw as new.
        self.record_base_skips(review.id, &missing, &answer.skipped)?;
        base.extend(read);
        Ok(base)
    }

    /// Replaces whatever was recorded for the paths just attempted at
    /// the base, keeping every other file the reader was told about. A
    /// path that reads cleanly this time loses the entry it had.
    fn record_base_skips(
        &self,
        review_id: u64,
        attempted: &[String],
        failed: &[(String, String)],
    ) -> Result<(), DaemonError> {
        let stored = self.storage.review_skipped(review_id)?;
        let mut kept: Vec<(String, String)> = stored
            .iter()
            .filter(|(path, _)| !attempted.contains(path))
            .cloned()
            .collect();
        if kept.len() == stored.len() && failed.is_empty() {
            return Ok(());
        }
        for (path, reason) in failed {
            warn!(review = review_id, %path, %reason, "review could not read a file at its base");
            kept.push((path.clone(), format!("{BASE_SKIP}{reason}")));
        }
        kept.sort();
        self.storage.set_review_skipped(review_id, &kept)?;
        Ok(())
    }

    async fn raw_sides_for_view(
        &self,
        review: &Review,
        view: &str,
    ) -> Result<ViewSides, DaemonError> {
        let parse = |v: &str| -> Option<(String, u32)> {
            let (kind, n) = v.split_once(':')?;
            Some((kind.to_string(), n.parse().ok()?))
        };
        let empty = || ViewSides {
            old: BTreeMap::new(),
            new: BTreeMap::new(),
            snapshot_id: None,
        };
        let stored = |old: BTreeMap<String, Vec<u8>>, id: u64| {
            Ok::<_, DaemonError>(ViewSides {
                old,
                new: self.snapshot_contents(id)?,
                snapshot_id: Some(id),
            })
        };
        match parse(view) {
            Some((kind, n)) => {
                let received = self.snapshot_for(review.id, n, ReviewSnapshotKind::Received);
                let sent = self.snapshot_for(review.id, n, ReviewSnapshotKind::Sent);
                match kind.as_str() {
                    "round" => match (sent, received) {
                        (Some(a), Some(b)) => stored(self.snapshot_contents(a)?, b),
                        _ => Ok(empty()),
                    },
                    "delta" => {
                        let prev = self.snapshot_for(
                            review.id,
                            n.saturating_sub(1),
                            ReviewSnapshotKind::Received,
                        );
                        let old = match prev {
                            Some(a) => self.snapshot_contents(a)?,
                            None => self.base_contents(review)?,
                        };
                        match received {
                            Some(b) => stored(old, b),
                            // Nothing was received for that round, so
                            // both sides are the round before it and
                            // the view shows no change.
                            None => Ok(ViewSides {
                                old: old.clone(),
                                new: old,
                                snapshot_id: prev,
                            }),
                        }
                    }
                    "sent" => match sent {
                        Some(a) => stored(self.base_contents(review)?, a),
                        None => Ok(empty()),
                    },
                    // "cum" and anything unrecognized fall back to the
                    // cumulative view, which is always meaningful.
                    _ => match received {
                        Some(b) => stored(self.base_contents(review)?, b),
                        None => self.live_sides(review).await,
                    },
                }
            }
            None => self.live_sides(review).await,
        }
    }

    /// The working tree against the base, and the snapshot the reader
    /// is being shown it from.
    async fn live_sides(&self, review: &Review) -> Result<ViewSides, DaemonError> {
        let old = self.base_contents(review)?;
        let (snapshot, new) = self.live_contents(review).await?;
        Ok(ViewSides {
            old,
            new,
            snapshot_id: Some(snapshot),
        })
    }

    /// One file's content on either side of a view.
    pub async fn review_file(
        &self,
        review_id: u64,
        view: &str,
        file: &str,
        side: &str,
    ) -> Result<RenderedView<Option<Vec<u8>>>, DaemonError> {
        let review = self.storage.get_review(review_id)?;
        let sides = self.sides_for_view(&review, view).await?;
        let content = if side == "old" || side == "left" || side == "base" {
            sides.old.get(file).cloned()
        } else {
            sides.new.get(file).cloned()
        };
        Ok(RenderedView {
            content,
            snapshot_id: sides.snapshot_id,
        })
    }

    /// Everything the reviewer's page needs, with anchors resolved
    /// against the live tree.
    pub async fn review_detail(
        &self,
        review_id: u64,
        user_id: u64,
    ) -> Result<ReviewDetail, DaemonError> {
        let review = self.storage.get_review(review_id)?;
        let viewer = self.storage.review_viewer_state(review_id, user_id)?;
        let revisions = self.storage.review_revisions(review_id)?;

        // One capture serves both the scope and the anchoring, rather
        // than reading the tree twice.
        let mut threads = self.storage.review_threads(review_id)?;
        let (live, detached) = self.live_with_threads(&review, &threads).await?;
        let mut files: Vec<String> = live.keys().cloned().collect();
        files.sort();
        files.dedup();
        let skipped = self.storage.review_skipped(review_id)?;

        self.resolve_anchors(&mut threads, &live)?;

        let latest_rev = revisions
            .iter()
            .filter(|r| r.kind == ReviewSnapshotKind::Received)
            .map(|r| r.rev)
            .max()
            .unwrap_or(0);

        // What the reader would see by advancing, so the page can say
        // how much is waiting without moving anything. A reader who has
        // never advanced is still on the snapshot taken at open, which
        // is revision 1 — not whatever round the review has since
        // reached.
        let pinned = if viewer.pinned_rev == 0 {
            1
        } else {
            viewer.pinned_rev
        };
        let pending_files = match (
            self.snapshot_for(review.id, pinned, ReviewSnapshotKind::Sent),
            self.snapshot_for(review.id, latest_rev, ReviewSnapshotKind::Received),
        ) {
            (Some(a), Some(b)) if latest_rev >= pinned => {
                let old = self.snapshot_contents(a)?;
                let new = self.snapshot_contents(b)?;
                let mut changed: Vec<String> = new
                    .iter()
                    .filter(|(p, v)| old.get(*p) != Some(v))
                    .map(|(p, _)| p.clone())
                    .collect();
                changed.sort();
                changed
            }
            _ => Vec::new(),
        };

        // A thread whose line changed in work the reader has not
        // advanced to is marked, so they never comment blind. The guard
        // above is `>=`, so this also covers the round they are reading:
        // its Received snapshot holds changes its Sent one does not. The
        // page decides what to do with that, and only offers the advance
        // to a reader who is actually behind, since advancing at the head
        // pins the revision already pinned.
        if !pending_files.is_empty() {
            for thread in &mut threads {
                thread.changed_ahead = pending_files.contains(&thread.path);
            }
        }

        Ok(ReviewDetail {
            review,
            threads,
            revisions,
            viewer,
            files,
            skipped,
            latest_rev,
            pending_files,
            detached,
        })
    }

    /// Every file the review holds a comment on. A capture keeps these
    /// whether or not they still differ, so a comment never outlives
    /// the snapshot that would show its code.
    fn commented_paths(&self, review_id: u64) -> Result<Vec<String>, DaemonError> {
        let mut paths: Vec<String> = self
            .storage
            .review_threads(review_id)?
            .into_iter()
            .map(|t| t.path)
            .collect();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Where each thread's line lands in the text a view renders, so
    /// the view can keep those lines on screen. Resolved against the
    /// view rather than the working tree, because a reader pinned to an
    /// earlier revision is reading that revision's code.
    fn anchors_for_view(
        &self,
        threads: &[ReviewThread],
        new: &BTreeMap<String, Vec<u8>>,
    ) -> Result<BTreeMap<String, Vec<u32>>, DaemonError> {
        let mut out: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        let mut cache: BTreeMap<(u64, String), Option<Vec<review_diff::Hunk>>> = BTreeMap::new();
        for thread in threads {
            // A left-side comment describes the base, which the view
            // renders from the other side of the diff.
            if thread.side == ReviewSide::Left {
                continue;
            }
            let Some(bytes) = new.get(&thread.path) else {
                continue;
            };
            let text = String::from_utf8_lossy(bytes).to_string();
            let key = (thread.anchor_snapshot_id, thread.path.clone());
            if !cache.contains_key(&key) {
                let anchored = self
                    .storage
                    .snapshot_file(thread.anchor_snapshot_id, &thread.path)?;
                let hunks =
                    anchored.map(|b| review_diff::hunks(&String::from_utf8_lossy(&b), &text));
                cache.insert(key.clone(), hunks);
            }
            let line = match cache.get(&key).and_then(|h| h.as_ref()) {
                Some(hunks) => match review_diff::remap_line(thread.line, hunks) {
                    review_diff::Remap::Same(n)
                    | review_diff::Remap::Moved(n)
                    | review_diff::Remap::Changed(n) => n,
                },
                None => thread.line,
            };
            out.entry(thread.path.clone()).or_default().push(line);
        }
        for lines in out.values_mut() {
            lines.sort_unstable();
            lines.dedup();
        }
        Ok(out)
    }

    /// Maps every thread's line forward through the edits since it was
    /// written. Computed per distinct (snapshot, path) pair rather than
    /// per thread, because many comments share a file.
    fn resolve_anchors(
        &self,
        threads: &mut [ReviewThread],
        live: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), DaemonError> {
        let mut cache: BTreeMap<(u64, String), Option<Vec<review_diff::Hunk>>> = BTreeMap::new();
        for thread in threads.iter_mut() {
            let current_text = live
                .get(&thread.path)
                .map(|b| String::from_utf8_lossy(b).to_string())
                .unwrap_or_default();

            // A left-side comment describes the base, which never moves.
            if thread.side == ReviewSide::Left {
                thread.current_line = thread.line;
                thread.anchor_status = ReviewAnchorStatus::Same;
                thread.current_excerpt = String::new();
                continue;
            }

            let key = (thread.anchor_snapshot_id, thread.path.clone());
            if !cache.contains_key(&key) {
                let anchored = self
                    .storage
                    .snapshot_file(thread.anchor_snapshot_id, &thread.path)?;
                let hunks = anchored.map(|bytes| {
                    review_diff::hunks(&String::from_utf8_lossy(&bytes), &current_text)
                });
                cache.insert(key.clone(), hunks);
            }
            match cache.get(&key).and_then(|h| h.as_ref()) {
                Some(hunks) => {
                    let (line, status) = match review_diff::remap_line(thread.line, hunks) {
                        review_diff::Remap::Same(n) => (n, ReviewAnchorStatus::Same),
                        review_diff::Remap::Moved(n) => (n, ReviewAnchorStatus::Moved),
                        review_diff::Remap::Changed(n) => (n, ReviewAnchorStatus::Changed),
                    };
                    thread.current_line = line;
                    thread.anchor_status = status;
                }
                None => {
                    thread.current_line = thread.line;
                    thread.anchor_status = ReviewAnchorStatus::Unknown;
                }
            }
            thread.current_excerpt =
                review_diff::excerpt_around(&current_text, thread.current_line, EXCERPT_CONTEXT);
        }
        Ok(())
    }

    // ---- reviewer actions ----

    /// The snapshot a client says its render came from, once it has
    /// been checked. It anchors a comment only if it belongs to this
    /// review and holds the commented file; a stale or forged id falls
    /// back to a fresh capture rather than anchoring into a snapshot
    /// that describes something else.
    fn supplied_anchor(
        &self,
        review_id: u64,
        snapshot_id: Option<u64>,
        path: &str,
    ) -> Result<Option<u64>, DaemonError> {
        let Some(id) = snapshot_id else {
            return Ok(None);
        };
        let owner = self.storage.snapshot_review(id)?;
        if owner != Some(review_id) {
            warn!(
                review = review_id,
                snapshot = id,
                owner = owner.unwrap_or(0),
                "ignoring an anchor snapshot from another review"
            );
            return Ok(None);
        }
        if !self.storage.snapshot_has_file(id, path)? {
            warn!(
                review = review_id,
                snapshot = id,
                path,
                "ignoring an anchor snapshot that does not hold the commented file"
            );
            return Ok(None);
        }
        Ok(Some(id))
    }

    pub async fn add_review_comment(&self, c: &NewComment) -> Result<u64, DaemonError> {
        let NewComment {
            review_id,
            path,
            line,
            side,
            excerpt,
            body,
            send,
            anchor_snapshot_id,
            choice,
        } = c;
        let (review_id, line, side, send) = (*review_id, *line, *side, *send);
        if body.trim().is_empty() {
            return Err(DaemonError::Rejected("comment body is empty".into()));
        }
        let review = self.storage.get_review(review_id)?;
        // The line came from a render of some revision, so the anchor
        // has to be that revision and not whatever the tree holds by
        // the time the comment arrives.
        let anchor = match self.supplied_anchor(review_id, *anchor_snapshot_id, path)? {
            Some(id) => id,
            None => {
                self.capture(&review, CaptureKind::Head, std::slice::from_ref(path))
                    .await?
            }
        };
        let thread_id = self.storage.add_review_thread(&ReviewThread {
            review_id,
            path: path.clone(),
            line,
            side,
            excerpt: excerpt.clone(),
            anchor_snapshot_id: anchor,
            state: ReviewThreadState::Draft,
            created_rev: review.revision,
            ..Default::default()
        })?;
        self.storage.add_review_message(&ReviewMessage {
            thread_id,
            author: ReviewAuthor::User,
            body: body.clone(),
            revision: review.revision,
            choice: choice.clone(),
            ..Default::default()
        })?;
        if send {
            self.send_review_threads(review_id, &[thread_id]).await?;
        } else {
            self.publish_review(review_id)?;
        }
        Ok(thread_id)
    }

    pub fn edit_review_comment(
        &self,
        message_id: u64,
        body: &str,
        choice: Option<&ReviewChoiceAnswer>,
    ) -> Result<(), DaemonError> {
        let thread_id = self.storage.edit_review_message(message_id, body, choice)?;
        let thread = self.storage.get_review_thread(thread_id)?;
        self.publish_review(thread.review_id)?;
        Ok(())
    }

    pub fn delete_review_thread(&self, thread_id: u64) -> Result<(), DaemonError> {
        let thread = self.storage.get_review_thread(thread_id)?;
        self.storage.delete_review_thread(thread_id)?;
        self.publish_review(thread.review_id)?;
        Ok(())
    }

    /// Hands threads to the agent. Named threads go individually;
    /// naming none sends every draft, which is the toolbar's batch.
    pub async fn send_review_threads(
        &self,
        review_id: u64,
        thread_ids: &[u64],
    ) -> Result<u32, DaemonError> {
        let review = self.storage.get_review(review_id)?;
        let threads = self.storage.review_threads(review_id)?;
        let selected: Vec<&ReviewThread> = if thread_ids.is_empty() {
            threads
                .iter()
                .filter(|t| t.state == ReviewThreadState::Draft)
                .collect()
        } else {
            threads
                .iter()
                .filter(|t| thread_ids.contains(&t.id))
                .collect()
        };
        if selected.is_empty() {
            return Ok(0);
        }

        // The tree the reviewer handed over. Captured once per round so
        // `round:N` compares like with like.
        let rev = review.revision.max(1);
        if self
            .snapshot_for(review_id, rev, ReviewSnapshotKind::Sent)
            .is_none()
        {
            let commented = self.commented_paths(review_id)?;
            let snapshot = self.capture(&review, CaptureKind::Head, &commented).await?;
            self.storage.record_review_revision(
                review_id,
                rev,
                ReviewSnapshotKind::Sent,
                snapshot,
                &[],
            )?;
        }

        let mut sent = 0;
        for thread in selected {
            self.storage
                .set_review_thread_state(thread.id, ReviewThreadState::Sent)?;
            self.storage
                .enqueue_review_event(review_id, thread.id, &thread.path)?;
            sent += 1;
        }
        self.storage.set_review_revision(review_id, rev)?;
        self.publish_review(review_id)?;
        info!(review = review_id, threads = sent, "review threads sent");
        if sent > 0 {
            self.wake_review_agent_if_idle(
                review_id,
                review.session_id,
                ReviewWakeReason::CommentsSent(sent),
            )
            .await;
        }
        Ok(sent)
    }

    /// The reviewer's own reply inside a thread.
    ///
    /// It appends to the conversation rather than opening a second
    /// thread on the same line, and hands the thread back to the agent
    /// the way a first comment does — a follow-up nobody answers is
    /// worse than no follow-up.
    pub async fn reply_review_thread(&self, thread_id: u64, body: &str) -> Result<(), DaemonError> {
        if body.trim().is_empty() {
            return Err(DaemonError::Rejected("reply body is empty".into()));
        }
        let thread = self.storage.get_review_thread(thread_id)?;
        let review = self.storage.get_review(thread.review_id)?;
        self.storage.add_review_message(&ReviewMessage {
            thread_id,
            author: ReviewAuthor::User,
            body: body.to_string(),
            revision: review.revision,
            ..Default::default()
        })?;
        // Sending it is what reopens a thread the reviewer had resolved
        // or the agent had answered, so the round trip carries on.
        self.send_review_threads(thread.review_id, &[thread_id])
            .await?;
        Ok(())
    }

    pub fn resolve_review_thread(&self, thread_id: u64, resolved: bool) -> Result<(), DaemonError> {
        let thread = self.storage.get_review_thread(thread_id)?;
        let state = if resolved {
            ReviewThreadState::Resolved
        } else if thread
            .messages
            .iter()
            .any(|m| m.author == ReviewAuthor::Session)
        {
            ReviewThreadState::Answered
        } else {
            ReviewThreadState::Draft
        };
        self.storage.set_review_thread_state(thread_id, state)?;
        self.publish_review(thread.review_id)?;
        Ok(())
    }

    /// Moves the reader onto a revision. Zero takes the newest.
    pub async fn advance_review(
        &self,
        review_id: u64,
        user_id: u64,
        rev: u32,
    ) -> Result<(), DaemonError> {
        let detail = self.review_detail(review_id, user_id).await?;
        let target = if rev == 0 { detail.latest_rev } else { rev };
        self.storage.set_review_viewer_state(
            user_id,
            &ReviewViewerStateUpdate {
                review_id,
                pinned_rev: Some(target),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    pub async fn finish_review(&self, review_id: u64) -> Result<Review, DaemonError> {
        let review = self
            .storage
            .set_review_state(review_id, ReviewState::Finished)?;
        self.storage.release_review_claims(review_id)?;
        info!(review = review_id, "review finished");
        // The live list holds open reviews, which is what the snapshot
        // carries too. Publishing a change instead would leave a
        // finished review in every connected client until it reloaded.
        self.publish(Event::ReviewRemoved(review_id));
        self.wake_review_agent_if_idle(review_id, review.session_id, ReviewWakeReason::Finished)
            .await;
        Ok(review)
    }

    pub fn set_review_viewer_state(
        &self,
        user_id: u64,
        update: &ReviewViewerStateUpdate,
    ) -> Result<ReviewViewerState, DaemonError> {
        Ok(self.storage.set_review_viewer_state(user_id, update)?)
    }

    pub(crate) fn review_wait_guard(&self, session_id: u64) -> ReviewWaitGuard<'_> {
        self.review_wake_runtime.increment_waiting(session_id);
        ReviewWaitGuard {
            runtime: &self.review_wake_runtime,
            session_id,
        }
    }

    pub fn is_session_awaiting_review(&self, session_id: u64) -> bool {
        self.review_wake_runtime.is_waiting(session_id)
    }

    pub(crate) async fn wake_review_agent_if_idle(
        &self,
        review_id: u64,
        session_id: u64,
        reason: ReviewWakeReason,
    ) {
        if self.is_session_awaiting_review(session_id) {
            return;
        }
        let Ok(session) = self.storage.get_session(session_id) else {
            return;
        };
        if !session.state.is_live() {
            return;
        }
        let now = crate::daemon::now_unix_ms();
        if matches!(reason, ReviewWakeReason::CommentsSent(_))
            && !self.review_wake_runtime.throttle_allows(session_id, now)
        {
            return;
        }
        self.review_wake_runtime.record_wake(session_id, now);

        let notice = match reason {
            ReviewWakeReason::CommentsSent(threads_sent) => format!(
                "[puppet-master] Review notice: New review comments were sent on review {} ({} thread{}). Call next_review_event to receive and address them.",
                review_id,
                threads_sent,
                if threads_sent == 1 { "" } else { "s" }
            ),
            ReviewWakeReason::Finished => format!(
                "[puppet-master] Review notice: Review {} was finished. No further comments are pending.",
                review_id
            ),
        };

        match self.send_review_input(session_id, &notice).await {
            Ok(outcome) => {
                info!(
                    review = review_id,
                    session = session_id,
                    delivery = outcome.delivery,
                    "review wake notice delivered to agent"
                );
            }
            Err(e) => {
                warn!(
                    review = review_id,
                    session = session_id,
                    error = %e,
                    "could not deliver review wake notice to agent"
                );
            }
        }
    }

    // ---- agent side ----

    /// Takes the next thread for this session's review, or `None` when
    /// nothing is queued. Threads whose file is already claimed stay
    /// queued, so two events never edit one file at once.
    /// Waits for the next comment rather than reporting there is none.
    ///
    /// An agent that is told "nothing yet" goes back to whatever it was
    /// doing and does not ask again, which leaves the reviewer waiting
    /// on a round trip nobody is holding open. So the call blocks: the
    /// agent stays in the review until the reviewer is done with it.
    ///
    /// It is bounded anyway, because an MCP client will not hold a
    /// request forever. A timeout says plainly that the agent should
    /// call straight back.
    pub async fn await_review_event(
        &self,
        session_id: u64,
        wait: std::time::Duration,
    ) -> Result<ReviewWait, DaemonError> {
        let _guard = self.review_wait_guard(session_id);
        let deadline = std::time::Instant::now() + wait;
        loop {
            if let Some(event) = self.next_review_event(session_id).await? {
                return Ok(ReviewWait::Event(Box::new(event)));
            }
            // Nothing open means nothing will ever arrive, so the agent
            // is released rather than parked on a finished review.
            if self.storage.list_reviews(session_id, false)?.is_empty() {
                return Ok(ReviewWait::Finished);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(ReviewWait::TimedOut);
            }
            tokio::time::sleep(REVIEW_POLL_INTERVAL).await;
        }
    }

    /// Releases claims older than the cutoff. A claim is normally cleared by
    /// the reply that answers it, so one whose response never reached the
    /// agent would otherwise strand its file for the life of the review.
    pub fn reclaim_stale_review_claims(
        &self,
        older_than_unix_ms: i64,
    ) -> Result<usize, DaemonError> {
        Ok(self
            .storage
            .reclaim_stale_review_events(older_than_unix_ms)?)
    }

    pub async fn next_review_event(
        &self,
        session_id: u64,
    ) -> Result<Option<ReviewEvent>, DaemonError> {
        let stale_before =
            crate::daemon::now_unix_ms() - crate::review_store::REVIEW_CLAIM_STALE_MS;
        self.reclaim_stale_review_claims(stale_before)?;
        for review in self.storage.list_reviews(session_id, false)? {
            if let Some((event_id, thread_id)) = self.storage.claim_review_event(review.id)? {
                let mut threads = vec![self.storage.get_review_thread(thread_id)?];
                let (_, live) = self.live_contents(&review).await?;
                self.resolve_anchors(&mut threads, &live)?;
                return Ok(Some(ReviewEvent {
                    event_id,
                    review,
                    thread: threads.remove(0),
                }));
            }
        }
        Ok(None)
    }

    /// Records an agent's reply, closes out its queued event, and
    /// captures the tree the round produced.
    pub async fn post_review_reply(
        &self,
        session_id: u64,
        thread_id: u64,
        body: &str,
        addressed: bool,
    ) -> Result<Review, DaemonError> {
        if body.trim().is_empty() {
            return Err(DaemonError::Rejected("reply body is empty".into()));
        }
        let thread = self.storage.get_review_thread(thread_id)?;
        let review = self.storage.get_review(thread.review_id)?;
        let rev = review.revision.max(1);

        // The tree after this round. Recording it before the message
        // means the reply can name the revision it produced.
        let commented = self.commented_paths(review.id)?;
        let snapshot = self.capture(&review, CaptureKind::Head, &commented).await?;
        let previous = self
            .snapshot_for(review.id, rev, ReviewSnapshotKind::Sent)
            .map(|id| self.snapshot_contents(id))
            .transpose()?
            .unwrap_or_default();
        let current = self.snapshot_contents(snapshot)?;
        let mut changed: Vec<String> = current
            .iter()
            .filter(|(p, v)| previous.get(*p) != Some(v))
            .map(|(p, _)| p.clone())
            .collect();
        changed.sort();
        // A reply that edited nothing — answering a question, or
        // declining a change — has no revision to offer. Recording one
        // anyway advertises "updates in Rev N" that contains no files,
        // which reads as the review losing track of itself.
        let produced_changes = !changed.is_empty();
        if produced_changes {
            self.storage.record_review_revision(
                review.id,
                rev,
                ReviewSnapshotKind::Received,
                snapshot,
                &changed,
            )?;
        }

        // The link a reply carries points at this thread's own file, so
        // "changes for this thread" is scoped rather than the whole round.
        let mine: Vec<String> = changed
            .iter()
            .filter(|p| **p == thread.path)
            .cloned()
            .collect();
        let linked = if !produced_changes {
            Vec::new()
        } else if mine.is_empty() {
            changed.clone()
        } else {
            mine.clone()
        };
        self.storage.add_review_message(&ReviewMessage {
            thread_id,
            author: ReviewAuthor::Session,
            session_id,
            body: body.to_string(),
            addressed,
            revision: rev,
            changes_rev: if produced_changes { rev } else { 0 },
            changed_files: linked,
            ..Default::default()
        })?;
        self.storage
            .set_review_thread_state(thread_id, ReviewThreadState::Answered)?;
        if let Some(event_id) = self.storage.event_for_thread(thread_id)? {
            self.storage.finish_review_event(event_id)?;
        }

        // The next round starts once nothing is left from this one, and
        // only if this one actually produced something. An empty round
        // would leave a gap in the revision picker.
        if produced_changes && self.storage.queued_review_events(review.id)? == 0 {
            self.storage.set_review_revision(review.id, rev + 1)?;
        }
        let review = self.storage.get_review(review.id)?;
        self.publish(Event::ReviewChanged(review.clone()));
        Ok(review)
    }

    /// Moves a session onto a worker. Tests use it to reach the remote
    /// path without standing up a second host.
    pub fn set_session_worker_for_test(&self, id: u64, worker_id: u64) -> Result<(), DaemonError> {
        self.storage.set_session_worker(id, worker_id)?;
        Ok(())
    }

    /// Reads a review by id. Used by tests that assert on counts.
    pub fn get_review_for_test(&self, id: u64) -> Review {
        self.storage.get_review(id).expect("review exists")
    }

    pub fn review_status(&self, session_id: u64) -> Result<Vec<Review>, DaemonError> {
        Ok(self.storage.list_reviews(session_id, false)?)
    }

    pub fn list_reviews(
        &self,
        session_id: u64,
        include_finished: bool,
    ) -> Result<Vec<Review>, DaemonError> {
        Ok(self.storage.list_reviews(session_id, include_finished)?)
    }

    fn publish_review(&self, review_id: u64) -> Result<(), DaemonError> {
        let review = self.storage.get_review(review_id)?;
        self.publish(Event::ReviewChanged(review));
        Ok(())
    }
}
