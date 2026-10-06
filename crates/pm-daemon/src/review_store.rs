//! Persistence for reviews: the entity, its threads and messages, the
//! content-addressed snapshot store, the dispatch queue, and per-user
//! viewer state.
//!
//! Snapshots are stored here rather than as refs in the user's
//! repository, so a review leaves nothing behind in the tree it reads
//! and works the same when that tree lives on a remote worker.

use pm_protocol::domain::{
    Review, ReviewAnchorStatus, ReviewAuthor, ReviewChoiceAnswer, ReviewChoiceSelect,
    ReviewMessage, ReviewMode, ReviewRevision, ReviewSide, ReviewSnapshotKind, ReviewState,
    ReviewThread, ReviewThreadState, ReviewViewerState, ReviewViewerStateUpdate,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;

use crate::storage::{now_unix_ms, Result, Storage, StorageError};

/// Untracked files above this size are listed as skipped instead of
/// spliced into the diff, so a stray core dump cannot flood a review.
pub const UNTRACKED_MAX_BYTES: u64 = 512 * 1024;

/// Comment bodies and replies. Long enough for a real explanation,
/// bounded so a runaway agent cannot fill the database.
pub const REVIEW_BODY_MAX: usize = 16_000;

/// Excerpts are a hunk, not a file.
pub const REVIEW_EXCERPT_MAX: usize = 8_000;

/// How long a claimed event may go unanswered before it is handed out again.
/// A claim is normally cleared by the reply that answers it, so a response
/// lost in transit would otherwise strand its file for the life of the
/// review: the agent never sees the thread, so it can never reply, so the
/// claim never clears.
pub const REVIEW_CLAIM_STALE_MS: i64 = 60_000;

pub const REVIEW_SCHEMA: &str = "
CREATE TABLE reviews (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    project_id INTEGER NOT NULL,
    worker_id INTEGER NOT NULL DEFAULT 0,
    worktree TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('range','file')),
    base TEXT NOT NULL DEFAULT '',
    head TEXT NOT NULL DEFAULT '',
    pathspec TEXT NOT NULL DEFAULT '',
    source_file TEXT NOT NULL DEFAULT '',
    label TEXT NOT NULL DEFAULT '',
    range_key TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','finished')),
    revision INTEGER NOT NULL DEFAULT 0,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    explicit_files TEXT NOT NULL DEFAULT '',
    base_snapshot_id INTEGER NOT NULL DEFAULT 0,
    skipped TEXT NOT NULL DEFAULT '',
    UNIQUE(worktree, range_key)
);
CREATE TABLE review_threads (
    id INTEGER PRIMARY KEY,
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    line INTEGER NOT NULL,
    side TEXT NOT NULL DEFAULT 'right' CHECK(side IN ('right','left')),
    excerpt TEXT NOT NULL DEFAULT '',
    anchor_snapshot_id INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'draft'
        CHECK(state IN ('draft','sent','answered','resolved')),
    created_rev INTEGER NOT NULL DEFAULT 0,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE INDEX review_threads_by_review ON review_threads(review_id);
CREATE TABLE review_messages (
    id INTEGER PRIMARY KEY,
    thread_id INTEGER NOT NULL REFERENCES review_threads(id) ON DELETE CASCADE,
    author TEXT NOT NULL CHECK(author IN ('user','session')),
    session_id INTEGER NOT NULL DEFAULT 0,
    body TEXT NOT NULL,
    addressed INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 0,
    changes_rev INTEGER NOT NULL DEFAULT 0,
    changed_files TEXT NOT NULL DEFAULT '',
    choice TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL
);
CREATE INDEX review_messages_by_thread ON review_messages(thread_id);
CREATE TABLE review_blobs (
    sha TEXT PRIMARY KEY,
    bytes BLOB NOT NULL
);
CREATE TABLE review_snapshots (
    id INTEGER PRIMARY KEY,
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE review_snapshot_files (
    snapshot_id INTEGER NOT NULL REFERENCES review_snapshots(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    blob_sha TEXT NOT NULL REFERENCES review_blobs(sha),
    PRIMARY KEY (snapshot_id, path)
);
CREATE TABLE review_revisions (
    id INTEGER PRIMARY KEY,
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    rev INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('sent','received')),
    snapshot_id INTEGER NOT NULL REFERENCES review_snapshots(id),
    files TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    UNIQUE(review_id, rev, kind)
);
CREATE TABLE review_events (
    id INTEGER PRIMARY KEY,
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    thread_id INTEGER NOT NULL REFERENCES review_threads(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    claimed_at_unix_ms INTEGER,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE INDEX review_events_by_review ON review_events(review_id, id);
CREATE TABLE review_viewer_state (
    review_id INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL,
    pinned_rev INTEGER NOT NULL DEFAULT 0,
    view TEXT NOT NULL DEFAULT '',
    layout TEXT NOT NULL DEFAULT 'side-by-side',
    context INTEGER NOT NULL DEFAULT 10,
    viewed_files TEXT NOT NULL DEFAULT '',
    preview_off_files TEXT NOT NULL DEFAULT '',
    last_thread_id INTEGER NOT NULL DEFAULT 0,
    scroll TEXT NOT NULL DEFAULT '{}',
    file_list_collapsed INTEGER NOT NULL DEFAULT 0,
    drafts TEXT NOT NULL DEFAULT '{}',
    seen TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (review_id, user_id)
);
";

/// The part of a review the git layer reads. Kept separate from
/// `Review` so the git code does not depend on counts, state, or ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewTarget {
    pub worktree: String,
    pub base: String,
    /// Empty means the diff tracks the working tree.
    pub head: String,
    pub pathspec: Vec<String>,
    pub source_file: String,
}

impl From<&Review> for ReviewTarget {
    fn from(r: &Review) -> Self {
        ReviewTarget {
            worktree: r.worktree.clone(),
            base: r.base.clone(),
            head: r.head.clone(),
            pathspec: r.pathspec.clone(),
            source_file: r.source_file.clone(),
        }
    }
}

fn mode_str(m: ReviewMode) -> &'static str {
    match m {
        ReviewMode::Range => "range",
        ReviewMode::File => "file",
    }
}

fn parse_mode(s: &str) -> ReviewMode {
    match s {
        "file" => ReviewMode::File,
        _ => ReviewMode::Range,
    }
}

fn state_str(s: ReviewState) -> &'static str {
    match s {
        ReviewState::Open => "open",
        ReviewState::Finished => "finished",
    }
}

fn parse_state(s: &str) -> ReviewState {
    match s {
        "finished" => ReviewState::Finished,
        _ => ReviewState::Open,
    }
}

fn thread_state_str(s: ReviewThreadState) -> &'static str {
    match s {
        ReviewThreadState::Draft => "draft",
        ReviewThreadState::Sent => "sent",
        ReviewThreadState::Answered => "answered",
        ReviewThreadState::Resolved => "resolved",
    }
}

fn parse_thread_state(s: &str) -> ReviewThreadState {
    match s {
        "sent" => ReviewThreadState::Sent,
        "answered" => ReviewThreadState::Answered,
        "resolved" => ReviewThreadState::Resolved,
        _ => ReviewThreadState::Draft,
    }
}

fn side_str(s: ReviewSide) -> &'static str {
    match s {
        ReviewSide::Right => "right",
        ReviewSide::Left => "left",
    }
}

fn parse_side(s: &str) -> ReviewSide {
    match s {
        "left" => ReviewSide::Left,
        _ => ReviewSide::Right,
    }
}

fn author_str(a: ReviewAuthor) -> &'static str {
    match a {
        ReviewAuthor::User => "user",
        ReviewAuthor::Session => "session",
    }
}

fn parse_author(s: &str) -> ReviewAuthor {
    match s {
        "session" => ReviewAuthor::Session,
        _ => ReviewAuthor::User,
    }
}

fn kind_str(k: ReviewSnapshotKind) -> &'static str {
    match k {
        ReviewSnapshotKind::Sent => "sent",
        ReviewSnapshotKind::Received => "received",
    }
}

fn parse_kind(s: &str) -> ReviewSnapshotKind {
    match s {
        "received" => ReviewSnapshotKind::Received,
        _ => ReviewSnapshotKind::Sent,
    }
}

/// Newline-joined, because a path cannot contain one and this keeps the
/// stored form readable when inspecting the database by hand.
fn join_list(v: &[String]) -> String {
    v.join("\n")
}

/// The answer is stored as JSON rather than as columns because it is
/// read and written whole and never queried by its parts.
fn choice_json(c: &ReviewChoiceAnswer) -> String {
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
    .to_string()
}

fn parse_choice(s: &str) -> Option<ReviewChoiceAnswer> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let strings = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let text = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    Some(ReviewChoiceAnswer {
        choice_id: text("choice_id"),
        select: match v.get("select").and_then(|x| x.as_str()) {
            Some("many") => ReviewChoiceSelect::Many,
            _ => ReviewChoiceSelect::One,
        },
        option_ids: strings("option_ids"),
        option_labels: strings("option_labels"),
        other_text: text("other_text"),
        notes: text("notes"),
    })
}

fn split_list(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split('\n').map(str::to_string).collect()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

pub fn blob_sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub(crate) fn row_to_review(row: &rusqlite::Row) -> rusqlite::Result<Review> {
    let mode: String = row.get(5)?;
    let state: String = row.get(12)?;
    let pathspec: String = row.get(8)?;
    Ok(Review {
        id: row.get::<_, i64>(0)? as u64,
        session_id: row.get::<_, i64>(1)? as u64,
        project_id: row.get::<_, i64>(2)? as u64,
        worker_id: row.get::<_, i64>(3)? as u64,
        worktree: row.get(4)?,
        mode: parse_mode(&mode),
        base: row.get(6)?,
        head: row.get(7)?,
        pathspec: split_list(&pathspec),
        source_file: row.get(9)?,
        label: row.get(10)?,
        range_key: row.get(11)?,
        state: parse_state(&state),
        revision: row.get::<_, i64>(13)? as u32,
        created_at_unix_ms: row.get(14)?,
        updated_at_unix_ms: row.get(15)?,
        draft_count: 0,
        open_count: 0,
        answered_count: 0,
        resolved_count: 0,
        explicit_files: split_list(&row.get::<_, String>(16)?),
        thread_latest_message: std::collections::BTreeMap::new(),
    })
}

pub(crate) const REVIEW_SELECT: &str =
    "SELECT id, session_id, project_id, worker_id, worktree, mode, \
     base, head, pathspec, source_file, label, range_key, state, revision, \
     created_at_unix_ms, updated_at_unix_ms, explicit_files FROM reviews";

/// What a review's threads add up to, for the row and the tab badge.
/// A thread the agent is mid-answer on still counts as open, because
/// the reviewer is still owed something.
fn fill_thread_latest(conn: &Connection, review: &mut Review) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT t.id, MAX(m.id) FROM review_threads t \
         JOIN review_messages m ON m.thread_id = t.id \
         WHERE t.review_id = ?1 GROUP BY t.id",
    )?;
    let rows = stmt.query_map(params![review.id as i64], |r| {
        Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))
    })?;
    for row in rows {
        let (thread, message) = row?;
        review.thread_latest_message.insert(thread, message);
    }
    Ok(())
}

fn fill_counts(conn: &Connection, review: &mut Review) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT state, COUNT(*) FROM review_threads WHERE review_id = ?1 GROUP BY state",
    )?;
    let rows = stmt.query_map(params![review.id as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32))
    })?;
    for row in rows {
        let (state, n) = row?;
        match parse_thread_state(&state) {
            ReviewThreadState::Draft => review.draft_count = n,
            ReviewThreadState::Sent => review.open_count += n,
            ReviewThreadState::Answered => review.answered_count = n,
            ReviewThreadState::Resolved => review.resolved_count = n,
        }
    }
    Ok(())
}

impl Storage {
    pub fn create_review(&self, r: &Review) -> Result<Review> {
        let conn = self.conn.lock().unwrap();
        let now = now_unix_ms();
        conn.execute(
            "INSERT INTO reviews (session_id, project_id, worker_id, worktree, mode, base, head, \
             pathspec, source_file, label, range_key, state, revision, created_at_unix_ms, \
             updated_at_unix_ms, explicit_files) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'open',0,?12,?12,?13)",
            params![
                r.session_id as i64,
                r.project_id as i64,
                r.worker_id as i64,
                r.worktree,
                mode_str(r.mode),
                r.base,
                r.head,
                join_list(&r.pathspec),
                r.source_file,
                r.label,
                r.range_key,
                now,
                join_list(&r.explicit_files),
            ],
        )?;
        let id = conn.last_insert_rowid() as u64;
        drop(conn);
        self.get_review(id)
    }

    pub fn get_review(&self, id: u64) -> Result<Review> {
        let conn = self.conn.lock().unwrap();
        let mut review = conn
            .query_row(
                &format!("{REVIEW_SELECT} WHERE id = ?1"),
                params![id as i64],
                row_to_review,
            )
            .optional()?
            .ok_or(StorageError::NotFound("review", id))?;
        fill_counts(&conn, &mut review)?;
        fill_thread_latest(&conn, &mut review)?;
        Ok(review)
    }

    /// The review covering this target in this worktree, if one exists.
    /// Identity is worktree plus normalized range, so the same range
    /// under a different pathspec is a different review.
    pub fn find_review(&self, worktree: &str, range_key: &str) -> Result<Option<Review>> {
        let conn = self.conn.lock().unwrap();
        let found = conn
            .query_row(
                &format!("{REVIEW_SELECT} WHERE worktree = ?1 AND range_key = ?2"),
                params![worktree, range_key],
                row_to_review,
            )
            .optional()?;
        match found {
            Some(mut review) => {
                fill_counts(&conn, &mut review)?;
                fill_thread_latest(&conn, &mut review)?;
                Ok(Some(review))
            }
            None => Ok(None),
        }
    }

    pub fn list_reviews(&self, session_id: u64, include_finished: bool) -> Result<Vec<Review>> {
        let conn = self.conn.lock().unwrap();
        let mut sql = REVIEW_SELECT.to_string();
        let mut clauses: Vec<String> = Vec::new();
        if session_id != 0 {
            clauses.push(format!("session_id = {}", session_id as i64));
        }
        if !include_finished {
            clauses.push("state = 'open'".into());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY id");
        let mut reviews: Vec<Review> = conn
            .prepare(&sql)?
            .query_map([], row_to_review)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for review in &mut reviews {
            fill_counts(&conn, review)?;
            fill_thread_latest(&conn, review)?;
        }
        Ok(reviews)
    }

    /// Points a review at a session, so a review reopened from a fresh
    /// session is answered by that one rather than the ended original.
    pub fn set_review_session(&self, id: u64, session_id: u64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE reviews SET session_id = ?2, updated_at_unix_ms = ?3 WHERE id = ?1",
            params![id as i64, session_id as i64, now_unix_ms()],
        )?;
        Ok(())
    }

    pub fn set_review_state(&self, id: u64, state: ReviewState) -> Result<Review> {
        self.conn.lock().unwrap().execute(
            "UPDATE reviews SET state = ?2, updated_at_unix_ms = ?3 WHERE id = ?1",
            params![id as i64, state_str(state), now_unix_ms()],
        )?;
        self.get_review(id)
    }

    /// The base side is immutable, so it is captured once and its
    /// snapshot id kept rather than re-read on every view.
    pub fn set_review_base_snapshot(&self, id: u64, snapshot_id: u64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE reviews SET base_snapshot_id = ?2 WHERE id = ?1",
            params![id as i64, snapshot_id as i64],
        )?;
        Ok(())
    }

    pub fn review_base_snapshot(&self, id: u64) -> Result<Option<u64>> {
        let found: i64 = self.conn.lock().unwrap().query_row(
            "SELECT base_snapshot_id FROM reviews WHERE id = ?1",
            params![id as i64],
            |r| r.get(0),
        )?;
        Ok((found > 0).then_some(found as u64))
    }

    /// Files the reader left out and why, refreshed by each capture so
    /// the page reports what it is not showing.
    pub fn set_review_skipped(&self, id: u64, skipped: &[(String, String)]) -> Result<()> {
        let encoded: Vec<String> = skipped
            .iter()
            .map(|(path, reason)| format!("{path}\t{reason}"))
            .collect();
        self.conn.lock().unwrap().execute(
            "UPDATE reviews SET skipped = ?2 WHERE id = ?1",
            params![id as i64, join_list(&encoded)],
        )?;
        Ok(())
    }

    pub fn review_skipped(&self, id: u64) -> Result<Vec<(String, String)>> {
        let raw: String = self.conn.lock().unwrap().query_row(
            "SELECT skipped FROM reviews WHERE id = ?1",
            params![id as i64],
            |r| r.get(0),
        )?;
        Ok(split_list(&raw)
            .into_iter()
            .filter_map(|line| {
                line.split_once('\t')
                    .map(|(p, r)| (p.to_string(), r.to_string()))
            })
            .collect())
    }

    pub fn set_review_revision(&self, id: u64, rev: u32) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE reviews SET revision = ?2, updated_at_unix_ms = ?3 WHERE id = ?1",
            params![id as i64, rev as i64, now_unix_ms()],
        )?;
        Ok(())
    }

    /// Clears every thread, message, snapshot, and revision of a review
    /// while keeping its identity, so `--reset` starts clean without
    /// invalidating the URL the reviewer already has.
    pub fn reset_review(&self, id: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM review_threads WHERE review_id = ?1",
            params![id as i64],
        )?;
        conn.execute(
            "DELETE FROM review_events WHERE review_id = ?1",
            params![id as i64],
        )?;
        conn.execute(
            "DELETE FROM review_revisions WHERE review_id = ?1",
            params![id as i64],
        )?;
        conn.execute(
            "DELETE FROM review_snapshots WHERE review_id = ?1",
            params![id as i64],
        )?;
        conn.execute(
            "UPDATE reviews SET revision = 0 WHERE id = ?1",
            params![id as i64],
        )?;
        Ok(())
    }

    // ---- threads and messages ----

    pub fn add_review_thread(&self, t: &ReviewThread) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO review_threads (review_id, path, line, side, excerpt, \
             anchor_snapshot_id, state, created_rev, created_at_unix_ms) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                t.review_id as i64,
                t.path,
                t.line as i64,
                side_str(t.side),
                truncate_chars(&t.excerpt, REVIEW_EXCERPT_MAX),
                t.anchor_snapshot_id as i64,
                thread_state_str(t.state),
                t.created_rev as i64,
                now_unix_ms(),
            ],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    pub fn get_review_thread(&self, id: u64) -> Result<ReviewThread> {
        let conn = self.conn.lock().unwrap();
        let mut thread = conn
            .query_row(
                "SELECT id, review_id, path, line, side, excerpt, anchor_snapshot_id, state, \
                 created_rev, created_at_unix_ms FROM review_threads WHERE id = ?1",
                params![id as i64],
                row_to_thread,
            )
            .optional()?
            .ok_or(StorageError::NotFound("review thread", id))?;
        thread.messages = messages_for(&conn, id)?;
        Ok(thread)
    }

    pub fn review_threads(&self, review_id: u64) -> Result<Vec<ReviewThread>> {
        let conn = self.conn.lock().unwrap();
        let mut threads: Vec<ReviewThread> = conn
            .prepare(
                "SELECT id, review_id, path, line, side, excerpt, anchor_snapshot_id, state, \
                 created_rev, created_at_unix_ms FROM review_threads WHERE review_id = ?1 \
                 ORDER BY path, line, id",
            )?
            .query_map(params![review_id as i64], row_to_thread)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for thread in &mut threads {
            thread.messages = messages_for(&conn, thread.id)?;
        }
        Ok(threads)
    }

    pub fn set_review_thread_state(&self, id: u64, state: ReviewThreadState) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE review_threads SET state = ?2 WHERE id = ?1",
            params![id as i64, thread_state_str(state)],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("review thread", id));
        }
        Ok(())
    }

    pub fn delete_review_thread(&self, id: u64) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "DELETE FROM review_threads WHERE id = ?1",
            params![id as i64],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("review thread", id));
        }
        Ok(())
    }

    pub fn add_review_message(&self, m: &ReviewMessage) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO review_messages (thread_id, author, session_id, body, addressed, \
             revision, changes_rev, changed_files, choice, created_at_unix_ms) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                m.thread_id as i64,
                author_str(m.author),
                m.session_id as i64,
                truncate_chars(&m.body, REVIEW_BODY_MAX),
                m.addressed as i64,
                m.revision as i64,
                m.changes_rev as i64,
                join_list(&m.changed_files),
                m.choice.as_ref().map(choice_json).unwrap_or_default(),
                now_unix_ms(),
            ],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    /// Rewrites a message in place. A `None` answer clears whatever
    /// answer the message carried, so an edit never leaves the old
    /// decision behind next to the new prose.
    pub fn edit_review_message(
        &self,
        id: u64,
        body: &str,
        choice: Option<&ReviewChoiceAnswer>,
    ) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE review_messages SET body = ?2, choice = ?3 WHERE id = ?1",
            params![
                id as i64,
                truncate_chars(body, REVIEW_BODY_MAX),
                choice.map(choice_json).unwrap_or_default(),
            ],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("review message", id));
        }
        Ok(conn.query_row(
            "SELECT thread_id FROM review_messages WHERE id = ?1",
            params![id as i64],
            |r| Ok(r.get::<_, i64>(0)? as u64),
        )?)
    }

    // ---- snapshots ----

    /// Stores a manifest whose blobs may already be present.
    ///
    /// A file whose SHA the store already holds arrives with no bytes,
    /// because the reader was told not to send them. Its manifest row
    /// still points at the existing blob, so the snapshot is complete
    /// either way. A withheld blob the store does not actually have is
    /// a protocol error, not a silently empty file.
    pub fn put_review_manifest(
        &self,
        review_id: u64,
        files: &[(String, String, Option<Vec<u8>>)],
    ) -> Result<u64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO review_snapshots (review_id, created_at_unix_ms) VALUES (?1,?2)",
            params![review_id as i64, now_unix_ms()],
        )?;
        let snapshot_id = tx.last_insert_rowid();
        for (path, sha, bytes) in files {
            match bytes {
                Some(bytes) => {
                    tx.execute(
                        "INSERT OR IGNORE INTO review_blobs (sha, bytes) VALUES (?1,?2)",
                        params![sha, bytes],
                    )?;
                }
                None => {
                    let present: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM review_blobs WHERE sha = ?1",
                        params![sha],
                        |r| r.get(0),
                    )?;
                    if present == 0 {
                        return Err(StorageError::Conflict(format!(
                            "blob {sha} for {path} was withheld but is not stored"
                        )));
                    }
                }
            }
            tx.execute(
                "INSERT OR REPLACE INTO review_snapshot_files (snapshot_id, path, blob_sha) \
                 VALUES (?1,?2,?3)",
                params![snapshot_id, path, sha],
            )?;
        }
        tx.commit()?;
        Ok(snapshot_id as u64)
    }

    /// The blob SHAs of a review's most recent snapshot. Sent with the
    /// next capture so the reader withholds bytes that have not
    /// changed, which is most of them.
    pub fn latest_snapshot_shas(
        &self,
        review_id: u64,
    ) -> Result<std::collections::BTreeSet<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT f.blob_sha FROM review_snapshot_files f \
                 WHERE f.snapshot_id = ( \
                   SELECT MAX(id) FROM review_snapshots WHERE review_id = ?1)",
            )?
            .query_map(params![review_id as i64], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()?)
    }

    /// Stores a tree as content-addressed blobs plus a manifest.
    /// Unchanged files reuse their blob, so a revision costs only what
    /// actually changed.
    pub fn put_review_snapshot(&self, review_id: u64, files: &[(String, Vec<u8>)]) -> Result<u64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO review_snapshots (review_id, created_at_unix_ms) VALUES (?1,?2)",
            params![review_id as i64, now_unix_ms()],
        )?;
        let snapshot_id = tx.last_insert_rowid();
        for (path, bytes) in files {
            let sha = blob_sha(bytes);
            tx.execute(
                "INSERT OR IGNORE INTO review_blobs (sha, bytes) VALUES (?1,?2)",
                params![sha, bytes],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO review_snapshot_files (snapshot_id, path, blob_sha) \
                 VALUES (?1,?2,?3)",
                params![snapshot_id, path, sha],
            )?;
        }
        tx.commit()?;
        Ok(snapshot_id as u64)
    }

    /// Adds files to a snapshot that already exists. The base
    /// snapshot's file set is fixed when the review opens, but a file
    /// can start differing after that, and its base side has to be
    /// readable once it does.
    pub fn extend_review_snapshot(
        &self,
        snapshot_id: u64,
        files: &[(String, Vec<u8>)],
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for (path, bytes) in files {
            let sha = blob_sha(bytes);
            tx.execute(
                "INSERT OR IGNORE INTO review_blobs (sha, bytes) VALUES (?1,?2)",
                params![sha, bytes],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO review_snapshot_files (snapshot_id, path, blob_sha) \
                 VALUES (?1,?2,?3)",
                params![snapshot_id as i64, path, sha],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The newest snapshot taken for a review, which is the most recent
    /// state of its tree the store has seen.
    pub fn newest_review_snapshot(&self, review_id: u64) -> Result<Option<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT MAX(id) FROM review_snapshots WHERE review_id = ?1",
                params![review_id as i64],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .map(|id| id as u64))
    }

    pub fn snapshot_manifest(&self, snapshot_id: u64) -> Result<BTreeMap<String, String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT path, blob_sha FROM review_snapshot_files WHERE snapshot_id = ?1 \
                 ORDER BY path",
            )?
            .query_map(params![snapshot_id as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<BTreeMap<_, _>, _>>()?)
    }

    /// Whether a snapshot holds one file, which is what makes it usable
    /// as an anchor for a comment on that file.
    pub fn snapshot_has_file(&self, snapshot_id: u64, path: &str) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT 1 FROM review_snapshot_files WHERE snapshot_id = ?1 AND path = ?2",
                params![snapshot_id as i64, path],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The review a snapshot was captured for, so an id handed in by a
    /// client can be checked against the review it claims to describe.
    pub fn snapshot_review(&self, snapshot_id: u64) -> Result<Option<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT review_id FROM review_snapshots WHERE id = ?1",
                params![snapshot_id as i64],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|id| id as u64))
    }

    pub fn snapshot_file(&self, snapshot_id: u64, path: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT b.bytes FROM review_snapshot_files f \
                 JOIN review_blobs b ON b.sha = f.blob_sha \
                 WHERE f.snapshot_id = ?1 AND f.path = ?2",
                params![snapshot_id as i64, path],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?)
    }

    pub fn read_blob(&self, sha: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT bytes FROM review_blobs WHERE sha = ?1",
                params![sha],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?)
    }

    pub fn record_review_revision(
        &self,
        review_id: u64,
        rev: u32,
        kind: ReviewSnapshotKind,
        snapshot_id: u64,
        files: &[String],
    ) -> Result<ReviewRevision> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO review_revisions \
             (review_id, rev, kind, snapshot_id, files, created_at_unix_ms) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                review_id as i64,
                rev as i64,
                kind_str(kind),
                snapshot_id as i64,
                join_list(files),
                now_unix_ms(),
            ],
        )?;
        let id = conn.last_insert_rowid() as u64;
        Ok(ReviewRevision {
            id,
            review_id,
            rev,
            kind,
            snapshot_id,
            created_at_unix_ms: now_unix_ms(),
            files: files.to_vec(),
        })
    }

    pub fn review_revisions(&self, review_id: u64) -> Result<Vec<ReviewRevision>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id, review_id, rev, kind, snapshot_id, files, created_at_unix_ms \
                 FROM review_revisions WHERE review_id = ?1 ORDER BY rev, kind",
            )?
            .query_map(params![review_id as i64], |row| {
                let kind: String = row.get(3)?;
                let files: String = row.get(5)?;
                Ok(ReviewRevision {
                    id: row.get::<_, i64>(0)? as u64,
                    review_id: row.get::<_, i64>(1)? as u64,
                    rev: row.get::<_, i64>(2)? as u32,
                    kind: parse_kind(&kind),
                    snapshot_id: row.get::<_, i64>(4)? as u64,
                    files: split_list(&files),
                    created_at_unix_ms: row.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---- dispatch queue ----

    /// Queues a thread for the agent. The path rides along so the
    /// claim can serialize per file.
    pub fn enqueue_review_event(&self, review_id: u64, thread_id: u64, path: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO review_events (review_id, thread_id, path, created_at_unix_ms) \
             VALUES (?1,?2,?3,?4)",
            params![review_id as i64, thread_id as i64, path, now_unix_ms()],
        )?;
        Ok(())
    }

    /// Takes the oldest queued event whose file is not already claimed
    /// by an in-flight one. Threads on unrelated files dispatch
    /// concurrently; two threads on one file never do.
    pub fn claim_review_event(&self, review_id: u64) -> Result<Option<(u64, u64)>> {
        let conn = self.conn.lock().unwrap();
        let found = conn
            .query_row(
                "SELECT id, thread_id FROM review_events e \
                 WHERE e.review_id = ?1 AND e.claimed_at_unix_ms IS NULL \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM review_events c \
                     WHERE c.review_id = e.review_id AND c.path = e.path \
                       AND c.claimed_at_unix_ms IS NOT NULL) \
                 ORDER BY e.id LIMIT 1",
                params![review_id as i64],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
            )
            .optional()?;
        if let Some((id, _)) = found {
            conn.execute(
                "UPDATE review_events SET claimed_at_unix_ms = ?2 WHERE id = ?1",
                params![id as i64, now_unix_ms()],
            )?;
        }
        Ok(found)
    }

    pub fn finish_review_event(&self, event_id: u64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM review_events WHERE id = ?1",
            params![event_id as i64],
        )?;
        Ok(())
    }

    /// Releases a claim without removing the event, so a session that
    /// died mid-answer does not strand its file.
    pub fn release_review_claims(&self, review_id: u64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE review_events SET claimed_at_unix_ms = NULL WHERE review_id = ?1",
            params![review_id as i64],
        )?;
        Ok(())
    }

    /// Releases claims older than the cutoff so their files are dispatchable
    /// again. Returns how many it released.
    pub fn reclaim_stale_review_events(&self, older_than_unix_ms: i64) -> Result<usize> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE review_events SET claimed_at_unix_ms = NULL \
             WHERE claimed_at_unix_ms IS NOT NULL AND claimed_at_unix_ms <= ?1",
            params![older_than_unix_ms],
        )?)
    }

    pub fn queued_review_events(&self, review_id: u64) -> Result<u32> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM review_events WHERE review_id = ?1",
            params![review_id as i64],
            |r| Ok(r.get::<_, i64>(0)? as u32),
        )?)
    }

    pub fn event_for_thread(&self, thread_id: u64) -> Result<Option<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM review_events WHERE thread_id = ?1 ORDER BY id LIMIT 1",
                params![thread_id as i64],
                |r| Ok(r.get::<_, i64>(0)? as u64),
            )
            .optional()?)
    }

    // ---- viewer state ----

    pub fn review_viewer_state(&self, review_id: u64, user_id: u64) -> Result<ReviewViewerState> {
        let conn = self.conn.lock().unwrap();
        let found = conn
            .query_row(
                "SELECT review_id, user_id, pinned_rev, view, layout, context, viewed_files, \
                 preview_off_files, last_thread_id, scroll, file_list_collapsed, drafts, seen \
                 FROM review_viewer_state WHERE review_id = ?1 AND user_id = ?2",
                params![review_id as i64, user_id as i64],
                |row| {
                    let viewed: String = row.get(6)?;
                    let preview_off: String = row.get(7)?;
                    let scroll: String = row.get(9)?;
                    Ok(ReviewViewerState {
                        review_id: row.get::<_, i64>(0)? as u64,
                        user_id: row.get::<_, i64>(1)? as u64,
                        pinned_rev: row.get::<_, i64>(2)? as u32,
                        view: row.get(3)?,
                        layout: row.get(4)?,
                        context: row.get::<_, i64>(5)? as u32,
                        viewed_files: split_list(&viewed),
                        preview_off_files: split_list(&preview_off),
                        last_thread_id: row.get::<_, i64>(8)? as u64,
                        scroll: serde_json::from_str(&scroll).unwrap_or_default(),
                        file_list_collapsed: row.get::<_, i64>(10)? != 0,
                        drafts: serde_json::from_str(&row.get::<_, String>(11)?)
                            .unwrap_or_default(),
                        seen: serde_json::from_str(&row.get::<_, String>(12)?).unwrap_or_default(),
                    })
                },
            )
            .optional()?;
        Ok(found.unwrap_or(ReviewViewerState {
            review_id,
            user_id,
            pinned_rev: 0,
            view: String::new(),
            layout: "side-by-side".into(),
            context: 10,
            viewed_files: Vec::new(),
            preview_off_files: Vec::new(),
            last_thread_id: 0,
            scroll: BTreeMap::new(),
            file_list_collapsed: false,
            drafts: BTreeMap::new(),
            seen: BTreeMap::new(),
        }))
    }

    /// Applies a partial update, leaving every absent field as stored.
    /// Scroll positions accumulate per view key rather than replacing
    /// the map, so switching views and back lands where the reader was.
    pub fn set_review_viewer_state(
        &self,
        user_id: u64,
        u: &ReviewViewerStateUpdate,
    ) -> Result<ReviewViewerState> {
        let mut current = self.review_viewer_state(u.review_id, user_id)?;
        if let Some(v) = u.pinned_rev {
            current.pinned_rev = v;
        }
        if let Some(v) = &u.view {
            current.view = v.clone();
        }
        if let Some(v) = &u.layout {
            current.layout = v.clone();
        }
        if let Some(v) = u.context {
            current.context = v;
        }
        if let Some(v) = &u.viewed_files {
            current.viewed_files = v.clone();
        }
        if let Some(v) = &u.preview_off_files {
            current.preview_off_files = v.clone();
        }
        if let Some(v) = u.last_thread_id {
            current.last_thread_id = v;
        }
        if let (Some(key), Some(top)) = (&u.scroll_key, u.scroll_top) {
            current.scroll.insert(key.clone(), top);
        }
        if let Some(v) = u.file_list_collapsed {
            current.file_list_collapsed = v;
        }
        if let (Some(thread), Some(message)) = (u.seen_thread, u.seen_message) {
            // Only ever forward: a thread scrolling back into view must
            // not un-see a newer reply that arrived meanwhile.
            let current = current.seen.entry(thread).or_default();
            *current = (*current).max(message);
        }
        if let Some(key) = &u.draft_key {
            // An empty body is a cleared draft, not a stored blank one.
            match u.draft_body.as_deref().unwrap_or("") {
                "" => {
                    current.drafts.remove(key);
                }
                body => {
                    current
                        .drafts
                        .insert(key.clone(), truncate_chars(body, REVIEW_BODY_MAX));
                }
            }
        }
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO review_viewer_state \
             (review_id, user_id, pinned_rev, view, layout, context, viewed_files, \
              preview_off_files, last_thread_id, scroll, file_list_collapsed, drafts, seen) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                current.review_id as i64,
                user_id as i64,
                current.pinned_rev as i64,
                current.view,
                current.layout,
                current.context as i64,
                join_list(&current.viewed_files),
                join_list(&current.preview_off_files),
                current.last_thread_id as i64,
                serde_json::to_string(&current.scroll).unwrap_or_else(|_| "{}".into()),
                current.file_list_collapsed as i64,
                serde_json::to_string(&current.drafts).unwrap_or_else(|_| "{}".into()),
                serde_json::to_string(&current.seen).unwrap_or_else(|_| "{}".into()),
            ],
        )?;
        Ok(current)
    }
}

fn row_to_thread(row: &rusqlite::Row) -> rusqlite::Result<ReviewThread> {
    let side: String = row.get(4)?;
    let state: String = row.get(7)?;
    let line = row.get::<_, i64>(3)? as u32;
    Ok(ReviewThread {
        id: row.get::<_, i64>(0)? as u64,
        review_id: row.get::<_, i64>(1)? as u64,
        path: row.get(2)?,
        line,
        side: parse_side(&side),
        excerpt: row.get(5)?,
        anchor_snapshot_id: row.get::<_, i64>(6)? as u64,
        // Filled in by the anchoring pass; until then the stored line
        // is the best answer available.
        current_line: line,
        anchor_status: ReviewAnchorStatus::Unknown,
        current_excerpt: String::new(),
        state: parse_thread_state(&state),
        created_rev: row.get::<_, i64>(8)? as u32,
        created_at_unix_ms: row.get(9)?,
        messages: Vec::new(),
        changed_ahead: false,
    })
}

fn messages_for(conn: &Connection, thread_id: u64) -> rusqlite::Result<Vec<ReviewMessage>> {
    conn.prepare(
        "SELECT id, thread_id, author, session_id, body, addressed, revision, changes_rev, \
         changed_files, created_at_unix_ms, choice FROM review_messages WHERE thread_id = ?1 \
         ORDER BY id",
    )?
    .query_map(params![thread_id as i64], |row| {
        let author: String = row.get(2)?;
        let changed: String = row.get(8)?;
        let choice: String = row.get(10)?;
        Ok(ReviewMessage {
            id: row.get::<_, i64>(0)? as u64,
            thread_id: row.get::<_, i64>(1)? as u64,
            author: parse_author(&author),
            session_id: row.get::<_, i64>(3)? as u64,
            body: row.get(4)?,
            addressed: row.get::<_, i64>(5)? != 0,
            revision: row.get::<_, i64>(6)? as u32,
            changes_rev: row.get::<_, i64>(7)? as u32,
            changed_files: split_list(&changed),
            created_at_unix_ms: row.get(9)?,
            choice: parse_choice(&choice),
        })
    })?
    .collect()
}
