use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use pm_protocol::domain::{
    AgentKind, AgentSelectionSource, Bucket, BucketBriefing, ConnectMode, ContextField,
    ContextKind, ContextSeverity, InstructionLayer, InstructionRevision, InstructionTarget, Item,
    ItemAttachment, ItemPriority, ItemQuery, ItemRef, ItemSourceKind, ItemStatus,
    ItemSummaryFilter, ModelDialect, ModelProfile, ModelProfileEndpoint, ModelProfileSource,
    PermissionMode, Project, ProjectPath, Session, SessionContext, SessionForward, SessionGit,
    SessionRole, SessionState, Snapshot, Terminal, TerminalKind, TerminalRunState, Worker,
};
use rusqlite::{params, Connection, OptionalExtension};

const SUPERVISOR_SNOOZE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS supervisor_snoozes (
    session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    until_unix_ms INTEGER NOT NULL,
    suppress_completion INTEGER NOT NULL DEFAULT 0,
    silent_revision INTEGER
);";

const SCHEMA_VERSION: i64 = 56;
pub const SESSION_RECENT_GRACE_MS: i64 = 60_000;
pub const SESSION_PAGE_SIZE: u32 = 50;
pub const SESSION_PAGE_LIMIT_MAX: u32 = 50;
pub const INSTRUCTION_MARKDOWN_MAX: usize = 32_000;

pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as i64
}

/// Timeline kind for agent-authored `checkpoint` notes in
/// `activity_reports`, distinct from the live status kinds.
pub const CHECKPOINT_KIND: &str = "checkpoint";
/// Activity kind an agent's flag_blocked call records.
pub const BLOCKED_KIND: &str = "blocked";
/// Timeline kind for notes the daemon writes about a user's action on the
/// session. They read like checkpoints but are not the agent's doing, so
/// the derived activity clock skips them.
pub const USER_NOTE_KIND: &str = "user-note";

/// Bounds on the agent-authored context bags, enforced before storage
/// so a misbehaving agent cannot flood a row. The glance bag feeds the
/// list row (tiny chips); the detail bag feeds the info panel.
pub const HEADLINE_MAX: usize = 100;
/// Bound on an agent-reported goal, which names the session in every list.
pub const GOAL_MAX: usize = 100;
/// Bound on a goal seeded from the task prompt's first sentence, kept
/// shorter than GOAL_MAX so a seeded name still fits one list row.
pub const GOAL_SEED_MAX: usize = 60;
pub const GLANCE_MAX_FIELDS: usize = 3;
pub const GLANCE_VALUE_MAX: usize = 48;
pub const CONTEXT_MAX_FIELDS: usize = 20;
pub const CONTEXT_VALUE_MAX: usize = 256;
pub const CONTEXT_LABEL_MAX: usize = 40;
pub const CONTEXT_KEY_MAX: usize = 48;
/// Git values are refs and paths, not prose.
pub const GIT_VALUE_MAX: usize = 200;

/// A partial write of a session's git location. A `None` field is left
/// as stored, which is what lets an agent report a branch change
/// without restating the worktree it already reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionGitUpdate {
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub repo_root: Option<String>,
    pub commit: Option<String>,
    pub upstream: Option<String>,
    pub dirty: Option<bool>,
}

impl SessionGitUpdate {
    /// True when the update would write nothing, so the caller can skip
    /// the statement entirely.
    pub fn is_empty(&self) -> bool {
        self.branch.is_none()
            && self.worktree.is_none()
            && self.repo_root.is_none()
            && self.commit.is_none()
            && self.upstream.is_none()
            && self.dirty.is_none()
    }
}

/// Bounds on agent-authored item and briefing text enforced before
/// storage. Item bodies reject overflow; the other item fields keep
/// their existing truncate-and-note contract.
pub const ITEM_TITLE_MAX: usize = 200;
pub const ITEM_BODY_MAX: usize = 65_536;
pub const ITEM_NOTE_MAX: usize = 1_000;
pub const ITEM_KEY_MAX: usize = 200;
pub const ITEM_SOURCE_DETAIL_MAX: usize = 100;
pub const ITEM_URL_MAX: usize = 1_000;
pub const BRIEFING_MAX: usize = 32_000;
pub const ITEM_BATCH_MAX: usize = 100;
pub const ITEM_DEPS_MAX: usize = 20;
pub const ITEM_QUERY_LIMIT_DEFAULT: u32 = 50;
pub const ITEM_QUERY_LIMIT_MAX: u32 = 200;
/// Attachment limits are deliberately modest because bytes live in the main SQLite database
/// and therefore increase database files, backups, and exported snapshots one-for-one.
pub const ITEM_ATTACHMENT_FILE_MAX: usize = 10 * 1024 * 1024;
pub const ITEM_ATTACHMENT_TOTAL_MAX: usize = 50 * 1024 * 1024;
pub const ITEM_ATTACHMENT_FILENAME_MAX: usize = 255;

/// Bounded aggregate metadata for an item query. The status facet lets the
/// board account for view policies (notably collapsed completed history)
/// without fetching every matching item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemQueryCounts {
    pub bucket_total: u64,
    pub matching_total: u64,
    pub status_counts: Vec<(ItemStatus, u64)>,
}

/// A directory a session publishes. The share is the durable intent:
/// its path and slug outlive every worker bounce, while the loopback
/// server that answers for it, and the port that server holds, do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDirShare {
    pub id: u64,
    pub session_id: u64,
    /// Relative to the session working directory, as the agent gave it.
    pub path: String,
    pub slug: String,
    pub label: String,
    pub created_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemAttachmentContent {
    pub metadata: ItemAttachment,
    pub content: Vec<u8>,
}

fn checked_attachment_total(existing: usize, added: usize) -> Result<usize> {
    let total = existing.saturating_add(added);
    if total > ITEM_ATTACHMENT_TOTAL_MAX {
        return Err(StorageError::Validation {
            field: "item attachments",
            limit: ITEM_ATTACHMENT_TOTAL_MAX,
            actual: total,
            unit: "bytes",
        });
    }
    Ok(total)
}

/// Closed items stay in snapshots this long after their last update so
/// the board can show "done recently"; older history via `ListItems`.
pub const ITEM_SNAPSHOT_CLOSED_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The controller's embedded local worker, present in every database.
const LOCAL_WORKER_ID: u64 = pm_protocol::domain::LOCAL_WORKER_ID;

const MIGRATE_V1_TO_V2: &str = "ALTER TABLE sessions ADD COLUMN session_token TEXT;";

const MIGRATE_V2_TO_V3: &str = "
ALTER TABLE sessions ADD COLUMN activity TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN progress_percent INTEGER;
";

const MIGRATE_V3_TO_V4: &str = "ALTER TABLE sessions ADD COLUMN transcript_path TEXT;";

const MIGRATE_V4_TO_V5: &str = "
ALTER TABLE buckets ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'default';
ALTER TABLE projects ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'inherit';
";

const MIGRATE_V5_TO_V6: &str = "
ALTER TABLE sessions ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'default';
UPDATE sessions
SET permission_mode = COALESCE(
    (
        SELECT CASE
            WHEN projects.permission_mode != 'inherit' THEN projects.permission_mode
            ELSE buckets.permission_mode
        END
        FROM projects
        JOIN buckets ON buckets.id = projects.bucket_id
        WHERE projects.id = sessions.project_id
    ),
    'default'
);
";

const MIGRATE_V6_TO_V7: &str = "
CREATE TABLE workers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    hostname TEXT NOT NULL DEFAULT '',
    platform TEXT NOT NULL DEFAULT '',
    default_project_root TEXT NOT NULL DEFAULT '',
    credential_hash TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    last_seen_at_unix_ms INTEGER
);
CREATE TABLE worker_enrollments (
    token_hash TEXT PRIMARY KEY,
    label TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    used_at_unix_ms INTEGER
);
CREATE TABLE project_paths (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    worker_id INTEGER NOT NULL REFERENCES workers(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    PRIMARY KEY (project_id, worker_id)
);
INSERT INTO workers (id, name, created_at_unix_ms) VALUES (0, 'local', 0);
ALTER TABLE buckets ADD COLUMN default_worker_id INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN worker_id INTEGER;
ALTER TABLE sessions ADD COLUMN worker_id INTEGER NOT NULL DEFAULT 0;
INSERT INTO project_paths (project_id, worker_id, path)
    SELECT id, 0, path FROM projects;
";

const MIGRATE_V7_TO_V8: &str = "
ALTER TABLE sessions ADD COLUMN cwd TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN agent_resumable INTEGER NOT NULL DEFAULT 0;
UPDATE sessions SET cwd = COALESCE(
    (SELECT path FROM project_paths WHERE project_id = sessions.project_id AND worker_id = sessions.worker_id),
    (SELECT path FROM projects WHERE id = sessions.project_id),
    ''
);
CREATE TABLE terminals (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('agent', 'shell')),
    title TEXT NOT NULL,
    cwd TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_terminals_agent ON terminals(session_id) WHERE kind = 'agent';
CREATE INDEX idx_terminals_session ON terminals(session_id);
CREATE TABLE terminal_runs (
    id INTEGER PRIMARY KEY,
    terminal_id INTEGER NOT NULL REFERENCES terminals(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('starting', 'running', 'exited', 'failed')),
    started_at_unix_ms INTEGER,
    ended_at_unix_ms INTEGER,
    exit_code INTEGER,
    scrollback_available INTEGER NOT NULL DEFAULT 0,
    UNIQUE(terminal_id, generation)
);
INSERT INTO terminals (id, session_id, kind, title, cwd, created_at_unix_ms)
    SELECT id, id, 'agent', 'Agent', cwd, created_at_unix_ms FROM sessions;
INSERT INTO terminal_runs (terminal_id, generation, state, started_at_unix_ms, ended_at_unix_ms, exit_code, scrollback_available)
    SELECT id, 1,
        CASE WHEN state IN ('exited', 'failed') THEN state ELSE 'running' END,
        created_at_unix_ms, ended_at_unix_ms, exit_code,
        CASE WHEN transcript_path IS NULL THEN 0 ELSE 1 END
    FROM sessions;
";

const MIGRATE_V8_TO_V9: &str = "
ALTER TABLE sessions ADD COLUMN desired_running INTEGER NOT NULL DEFAULT 0;
UPDATE sessions SET desired_running = 1 WHERE state NOT IN ('exited', 'failed');
ALTER TABLE terminals ADD COLUMN desired_running INTEGER NOT NULL DEFAULT 0;
UPDATE terminals SET desired_running = 1
WHERE id IN (
    SELECT terminal_id FROM terminal_runs r
    WHERE generation = (
        SELECT MAX(r2.generation) FROM terminal_runs r2 WHERE r2.terminal_id = r.terminal_id
    ) AND state IN ('starting', 'running')
);
";

const MIGRATE_V9_TO_V10: &str = "
ALTER TABLE sessions ADD COLUMN headline TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN summary TEXT NOT NULL DEFAULT '';
CREATE TABLE session_glance (
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    key TEXT NOT NULL,
    label TEXT NOT NULL,
    value TEXT NOT NULL,
    kind TEXT NOT NULL,
    severity TEXT NOT NULL,
    PRIMARY KEY (session_id, position)
);
CREATE TABLE session_context (
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    label TEXT NOT NULL,
    value TEXT NOT NULL,
    kind TEXT NOT NULL,
    severity TEXT NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY (session_id, key)
);
";

const MIGRATE_V10_TO_V12: &str = "
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE workspaces (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    layout_json TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    position INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_workspaces_user ON workspaces(user_id, id);
";

const MIGRATE_V11_TO_V12: &str = "
ALTER TABLE workspaces ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
UPDATE workspaces SET position = id;
";

const MIGRATE_V12_TO_V13: &str = "
CREATE TABLE session_forwards (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    worker_port INTEGER NOT NULL,
    listener_port INTEGER NOT NULL,
    label TEXT NOT NULL DEFAULT '',
    scheme TEXT NOT NULL DEFAULT 'http',
    created_at_unix_ms INTEGER NOT NULL,
    UNIQUE(session_id, worker_port)
);
";

const MIGRATE_V13_TO_V14: &str = "
ALTER TABLE sessions ADD COLUMN items_api INTEGER NOT NULL DEFAULT 1;
CREATE TABLE items (
    id INTEGER PRIMARY KEY,
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    item_number INTEGER NOT NULL CHECK(item_number > 0),
    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    external_key TEXT,
    title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK (status IN
        ('inbox','planned','in_progress','blocked','blocked_external','done','dropped')),
    priority TEXT NOT NULL CHECK (priority IN ('urgent','high','normal','low')),
    source_kind TEXT NOT NULL CHECK (source_kind IN
        ('email','slack','github','jira','teams','telegram','human','agent','other')),
    source_detail TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    due_at_unix_ms INTEGER,
    snoozed_until_unix_ms INTEGER,
    created_by_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    done_at_unix_ms INTEGER,
    UNIQUE (bucket_id, external_key),
    UNIQUE (bucket_id, item_number)
);
CREATE TABLE bucket_item_sequences (
    bucket_id INTEGER PRIMARY KEY REFERENCES buckets(id) ON DELETE CASCADE,
    next_item_number INTEGER NOT NULL CHECK(next_item_number > 0)
);
CREATE INDEX idx_items_bucket ON items(bucket_id, status);
CREATE TABLE item_deps (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    depends_on_item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    PRIMARY KEY (item_id, depends_on_item_id)
);
CREATE TABLE item_sessions (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    PRIMARY KEY (item_id, session_id)
);
CREATE TABLE item_notes (
    id INTEGER PRIMARY KEY,
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    ts_unix_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created','status','note')),
    text TEXT NOT NULL
);
CREATE INDEX idx_item_notes_item ON item_notes(item_id);
CREATE TABLE bucket_briefings (
    id INTEGER PRIMARY KEY,
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    ts_unix_ms INTEGER NOT NULL,
    markdown TEXT NOT NULL
);
CREATE INDEX idx_briefings_bucket ON bucket_briefings(bucket_id, id);
";

const MIGRATE_V14_TO_V15: &str = "
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

const MIGRATE_V15_TO_V16: &str = "
CREATE TABLE IF NOT EXISTS session_activity (
    session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    last_agent_activity_at_unix_ms INTEGER NOT NULL DEFAULT 0,
    last_user_interaction_at_unix_ms INTEGER NOT NULL DEFAULT 0,
    last_user_submit_at_unix_ms INTEGER NOT NULL DEFAULT 0,
    last_agent_turn_at_unix_ms INTEGER NOT NULL DEFAULT 0
);
";

const MIGRATE_V16_TO_V17: &str = "
ALTER TABLE sessions ADD COLUMN supervisor_api INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN spawned_by_session_id INTEGER REFERENCES sessions(id);
";

const MIGRATE_V17_TO_V18: &str = "
BEGIN;
ALTER TABLE items ADD COLUMN question TEXT NOT NULL DEFAULT '';
ALTER TABLE item_notes RENAME TO item_notes_v17;
CREATE TABLE item_notes (
    id INTEGER PRIMARY KEY,
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    ts_unix_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created','status','note','user_reply')),
    text TEXT NOT NULL
);
INSERT INTO item_notes (id, item_id, session_id, ts_unix_ms, kind, text)
    SELECT id, item_id, session_id, ts_unix_ms, kind, text FROM item_notes_v17;
DROP TABLE item_notes_v17;
CREATE INDEX idx_item_notes_item ON item_notes(item_id);
COMMIT;
";

const MIGRATE_V18_TO_V19: &str = "
CREATE TABLE IF NOT EXISTS user_settings (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (user_id, key)
);
";

const MIGRATE_V19_TO_V20: &str = "
ALTER TABLE sessions ADD COLUMN role TEXT NOT NULL DEFAULT 'worker'
    CHECK(role IN ('worker','supervisor'));
UPDATE sessions SET role = CASE WHEN supervisor_api = 1 THEN 'supervisor' ELSE 'worker' END;
UPDATE sessions SET items_api = 1, supervisor_api = 1 WHERE role = 'supervisor';
-- Reviews. Defined in review_store.rs so the schema sits with the
-- queries that use it.
CREATE TABLE instruction_layers (
    id INTEGER PRIMARY KEY,
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
    target TEXT NOT NULL CHECK(target IN ('all','worker','supervisor')),
    markdown TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at_unix_ms INTEGER NOT NULL,
    updated_by_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL
);
CREATE UNIQUE INDEX instruction_layers_scope
    ON instruction_layers(bucket_id, COALESCE(project_id, 0), target);
CREATE TABLE instruction_revisions (
    layer_id INTEGER NOT NULL REFERENCES instruction_layers(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    markdown TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    updated_at_unix_ms INTEGER NOT NULL,
    updated_by_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    PRIMARY KEY(layer_id, revision)
);
CREATE TABLE session_instruction_snapshots (
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    terminal_generation INTEGER NOT NULL,
    compiled_markdown TEXT NOT NULL,
    source_revisions_json TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY(session_id, terminal_generation)
);
";

const MIGRATE_V20_TO_V21: &str = "
CREATE TRIGGER IF NOT EXISTS items_body_length_insert
BEFORE INSERT ON items
WHEN length(NEW.body) > 65536
BEGIN
    SELECT RAISE(ABORT, 'item body exceeds 65536 Unicode scalar values');
END;
CREATE TRIGGER IF NOT EXISTS items_body_length_update
BEFORE UPDATE OF body ON items
WHEN length(NEW.body) > 65536
BEGIN
    SELECT RAISE(ABORT, 'item body exceeds 65536 Unicode scalar values');
END;
";

const MIGRATE_V21_TO_V22: &str = "ALTER TABLE users DROP COLUMN workspace_home_position;";

const MIGRATE_V22_TO_V23: &str = "
ALTER TABLE buckets ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0;
CREATE TABLE bucket_workers (
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    worker_id INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    PRIMARY KEY (bucket_id, worker_id)
);
CREATE TABLE project_workers (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    worker_id INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    PRIMARY KEY (project_id, worker_id)
);
INSERT OR IGNORE INTO bucket_workers (bucket_id, worker_id)
    SELECT id, default_worker_id FROM buckets;
INSERT OR IGNORE INTO bucket_workers (bucket_id, worker_id)
    SELECT bucket_id, worker_id FROM projects WHERE worker_id IS NOT NULL;
INSERT OR IGNORE INTO project_workers (project_id, worker_id)
    SELECT p.id, bw.worker_id FROM projects p JOIN bucket_workers bw ON bw.bucket_id = p.bucket_id
    WHERE p.worker_id IS NULL;
INSERT OR IGNORE INTO project_workers (project_id, worker_id)
    SELECT id, worker_id FROM projects WHERE worker_id IS NOT NULL;
UPDATE buckets SET is_default = 1 WHERE id = (
    SELECT id FROM buckets ORDER BY position, id LIMIT 1
);
CREATE UNIQUE INDEX idx_one_default_bucket ON buckets(is_default) WHERE is_default = 1;
";

const MIGRATE_V23_TO_V24: &str = "
CREATE TABLE item_attachments (
    id INTEGER PRIMARY KEY,
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    media_type TEXT NOT NULL,
    byte_length INTEGER NOT NULL,
    sha256 BLOB NOT NULL,
    content BLOB NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    created_by_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL
);
CREATE INDEX idx_item_attachments_item_time
    ON item_attachments(item_id, created_at_unix_ms, id);
CREATE INDEX idx_item_attachments_digest ON item_attachments(sha256);
";

const MIGRATE_V24_TO_V25: &str = "
ALTER TABLE items ADD COLUMN item_number INTEGER;
UPDATE items AS target
SET item_number = (
    SELECT COUNT(*) FROM items AS preceding
    WHERE preceding.bucket_id = target.bucket_id AND preceding.id <= target.id
);
CREATE UNIQUE INDEX idx_items_bucket_number ON items(bucket_id, item_number);
CREATE TRIGGER items_item_number_required_insert
BEFORE INSERT ON items WHEN NEW.item_number IS NULL OR NEW.item_number <= 0
BEGIN SELECT RAISE(ABORT, 'items.item_number must be positive'); END;
CREATE TRIGGER items_item_number_required_update
BEFORE UPDATE OF item_number ON items WHEN NEW.item_number IS NULL OR NEW.item_number <= 0
BEGIN SELECT RAISE(ABORT, 'items.item_number must be positive'); END;
CREATE TABLE bucket_item_sequences (
    bucket_id INTEGER PRIMARY KEY REFERENCES buckets(id) ON DELETE CASCADE,
    next_item_number INTEGER NOT NULL CHECK(next_item_number > 0)
);
INSERT INTO bucket_item_sequences (bucket_id, next_item_number)
SELECT bucket_id, MAX(item_number) + 1 FROM items GROUP BY bucket_id;
";

// FTS stores only deliberately searchable metadata. In particular it does not
// include task_prompt, transcript/scrollback, item text, instructions, context
// bags, or activity-report payloads. The one-time backfill cost is linear in
// session count; subsequent writes are maintained by triggers.
const MIGRATE_V25_TO_V26: &str = r#"
CREATE VIRTUAL TABLE session_search USING fts5(
    task_title, headline, summary, activity, project_name, bucket_name, cwd,
    agent_session_id, tokenize='unicode61'
);
INSERT INTO session_search(rowid, task_title, headline, summary, activity,
    project_name, bucket_name, cwd, agent_session_id)
SELECT s.id, s.task_title, s.headline, s.summary, s.activity, p.name, b.name,
       s.cwd, COALESCE(s.agent_session_id, '')
FROM sessions s JOIN projects p ON p.id=s.project_id JOIN buckets b ON b.id=p.bucket_id;
CREATE TRIGGER session_search_insert AFTER INSERT ON sessions BEGIN
  INSERT INTO session_search(rowid, task_title, headline, summary, activity,
      project_name, bucket_name, cwd, agent_session_id)
  SELECT NEW.id, NEW.task_title, NEW.headline, NEW.summary, NEW.activity,
         p.name, b.name, NEW.cwd, COALESCE(NEW.agent_session_id, '')
  FROM projects p JOIN buckets b ON b.id=p.bucket_id WHERE p.id=NEW.project_id;
END;
CREATE TRIGGER session_search_delete AFTER DELETE ON sessions BEGIN
  DELETE FROM session_search WHERE rowid=OLD.id;
END;
CREATE TRIGGER session_search_update AFTER UPDATE OF project_id, task_title, headline,
    summary, activity, cwd, agent_session_id ON sessions BEGIN
  DELETE FROM session_search WHERE rowid=OLD.id;
  INSERT INTO session_search(rowid, task_title, headline, summary, activity,
      project_name, bucket_name, cwd, agent_session_id)
  SELECT NEW.id, NEW.task_title, NEW.headline, NEW.summary, NEW.activity,
         p.name, b.name, NEW.cwd, COALESCE(NEW.agent_session_id, '')
  FROM projects p JOIN buckets b ON b.id=p.bucket_id WHERE p.id=NEW.project_id;
END;
CREATE TRIGGER session_search_project_name AFTER UPDATE OF name ON projects BEGIN
  DELETE FROM session_search WHERE rowid IN (SELECT id FROM sessions WHERE project_id=NEW.id);
  INSERT INTO session_search(rowid, task_title, headline, summary, activity,
      project_name, bucket_name, cwd, agent_session_id)
  SELECT s.id, s.task_title, s.headline, s.summary, s.activity, NEW.name, b.name,
         s.cwd, COALESCE(s.agent_session_id, '')
  FROM sessions s JOIN buckets b ON b.id=NEW.bucket_id WHERE s.project_id=NEW.id;
END;
CREATE TRIGGER session_search_bucket_name AFTER UPDATE OF name ON buckets BEGIN
  DELETE FROM session_search WHERE rowid IN
      (SELECT s.id FROM sessions s JOIN projects p ON p.id=s.project_id WHERE p.bucket_id=NEW.id);
  INSERT INTO session_search(rowid, task_title, headline, summary, activity,
      project_name, bucket_name, cwd, agent_session_id)
  SELECT s.id, s.task_title, s.headline, s.summary, s.activity, p.name, NEW.name,
         s.cwd, COALESCE(s.agent_session_id, '')
  FROM sessions s JOIN projects p ON p.id=s.project_id WHERE p.bucket_id=NEW.id;
END;
CREATE INDEX idx_sessions_ended_page ON sessions(ended_at_unix_ms DESC, id DESC)
    WHERE state IN ('exited','failed');
"#;

const MIGRATE_V27_TO_V28: &str = "
ALTER TABLE buckets ADD COLUMN default_agent TEXT;
ALTER TABLE projects ADD COLUMN default_agent TEXT;
ALTER TABLE sessions ADD COLUMN agent_source TEXT NOT NULL DEFAULT 'explicit';
";

// The notifications table existed in fresh schemas before it had a durable
// consumer. Make it an explicit migration now that NeedsInput attention uses it.
const MIGRATE_V28_TO_V29: &str = "
CREATE TABLE notifications (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    ts_unix_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
    read_at_unix_ms INTEGER
);
CREATE INDEX idx_notifications_unread ON notifications(session_id) WHERE read_at_unix_ms IS NULL;
";

// Mobile tokens are stored only as sha256 hashes, mirroring
// auth_sessions and worker_enrollments.
const MIGRATE_V29_TO_V30: &str = "
CREATE TABLE mobile_devices (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL DEFAULT '',
    platform TEXT NOT NULL DEFAULT '',
    app_installation_id TEXT NOT NULL,
    refresh_family_id TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    last_seen_at_unix_ms INTEGER,
    revoked_at_unix_ms INTEGER
);
CREATE TABLE mobile_refresh_tokens (
    id INTEGER PRIMARY KEY,
    device_id INTEGER NOT NULL REFERENCES mobile_devices(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    used_at_unix_ms INTEGER
);
CREATE TABLE mobile_access_tokens (
    token_hash TEXT PRIMARY KEY,
    device_id INTEGER NOT NULL REFERENCES mobile_devices(id) ON DELETE CASCADE,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE mobile_enrollment_tokens (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    used_at_unix_ms INTEGER
);
CREATE INDEX idx_mobile_devices_user ON mobile_devices(user_id, id);
CREATE INDEX idx_mobile_refresh_device ON mobile_refresh_tokens(device_id);
CREATE INDEX idx_mobile_access_device ON mobile_access_tokens(device_id);
";

// One-use socket tickets consumed by the mobile control and terminal
// WebSocket upgrades. Control tickets leave the terminal binding NULL;
// values are stored only as sha256 hashes like every other token.
const MIGRATE_V30_TO_V31: &str = "
CREATE TABLE socket_tickets (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id INTEGER NOT NULL REFERENCES mobile_devices(id) ON DELETE CASCADE,
    terminal_id INTEGER,
    generation INTEGER,
    replay_bytes INTEGER,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    consumed_at_unix_ms INTEGER
);
CREATE INDEX idx_socket_tickets_device ON socket_tickets(device_id);
";

// Push delivery state must survive a daemon restart so the same
// committed transition never alerts a device twice; the unique
// (event_id, device_id) pair is the dedupe key.
// Which child transition a Supervisor has already been told about.
// In-memory marks do not survive a restart, and without this a daemon
// cannot tell a transition it already announced from one that parked
// while nobody was watching.
const MIGRATE_V36_TO_V37: &str = "
ALTER TABLE sessions ADD COLUMN announced_wake_key TEXT;
";

// Where a session sits in git, as its agent reports it. Held on the
// session rather than in the context bags so a row can render it as
// dedicated chrome without spending one of the three glance slots.
const MIGRATE_V37_TO_V38: &str = "
ALTER TABLE sessions ADD COLUMN git_branch TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN git_worktree TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN git_repo_root TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN git_commit TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN git_upstream TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN git_dirty INTEGER;
";

const MIGRATE_V31_TO_V32: &str = "
ALTER TABLE sessions ADD COLUMN state_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN working_since_unix_ms INTEGER;
CREATE TABLE mobile_push_endpoints (
    device_id INTEGER PRIMARY KEY REFERENCES mobile_devices(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK(provider IN ('apns','fcm','gateway')),
    token_ciphertext TEXT NOT NULL,
    environment TEXT NOT NULL CHECK(environment IN ('production','sandbox')),
    locale TEXT NOT NULL DEFAULT '',
    previews_enabled INTEGER NOT NULL DEFAULT 0,
    event_mask INTEGER NOT NULL,
    public_key TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    disabled_at_unix_ms INTEGER,
    disabled_reason TEXT NOT NULL DEFAULT ''
);
CREATE TABLE notification_deliveries (
    id INTEGER PRIMARY KEY,
    event_id TEXT NOT NULL,
    device_id INTEGER NOT NULL REFERENCES mobile_devices(id) ON DELETE CASCADE,
    session_id INTEGER NOT NULL,
    state TEXT NOT NULL,
    collapse_id TEXT NOT NULL,
    status TEXT NOT NULL,
    provider_message_id TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_unix_ms INTEGER NOT NULL,
    last_error TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    UNIQUE(event_id, device_id)
);
CREATE INDEX idx_notification_deliveries_due
    ON notification_deliveries(next_attempt_at_unix_ms) WHERE status = 'pending';
CREATE TABLE push_policies (
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT '' CHECK(role IN ('', 'worker', 'supervisor')),
    events TEXT NOT NULL,
    scope TEXT NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY (bucket_id, role)
);
";

// Model profiles are controller-global like workers. A session stores
// the profile id, not a copy of its values, so a resume reselects the
// entry and picks up edits.
const MIGRATE_V32_TO_V33: &str = "
CREATE TABLE model_profiles (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    api_key_ciphertext TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE model_profile_endpoints (
    profile_id INTEGER NOT NULL REFERENCES model_profiles(id) ON DELETE CASCADE,
    dialect TEXT NOT NULL,
    model TEXT NOT NULL,
    base_url TEXT NOT NULL DEFAULT '',
    background_model TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (profile_id, dialect)
);
ALTER TABLE sessions ADD COLUMN model_profile_id INTEGER REFERENCES model_profiles(id) ON DELETE SET NULL;
ALTER TABLE sessions ADD COLUMN model_profile_source TEXT;
";

// Split out so the profile tables still land on a database that never
// grew a catalog, matching how the default-agent migration guards its
// own catalog columns.
const MIGRATE_V32_TO_V33_CATALOG: &str = "
ALTER TABLE buckets ADD COLUMN model_profile_id INTEGER REFERENCES model_profiles(id);
ALTER TABLE projects ADD COLUMN model_profile_id INTEGER REFERENCES model_profiles(id);
";

// Every remote spawn used to record its resolved directory here, so the
// first launch froze a copy of the project path that then outranked
// every later edit to it. Nothing writes these rows now, and clearing
// them returns each project to its configured path. That clean-up belongs
// to this one upgrade: every row written since is a path an operator set
// by hand, so the guard stays pinned to the version below and never to
// SCHEMA_VERSION, which widens with each release and would delete
// configured paths on every upgrade.
const PROJECT_PATH_WRITEBACK_SCHEMA_VERSION: i64 = 34;
const MIGRATE_V33_TO_V34: &str = "DELETE FROM project_paths;";

const MIGRATE_V34_TO_V35: &str =
    "ALTER TABLE workers ADD COLUMN pm_version TEXT NOT NULL DEFAULT '';";

// Existing workers carry no pinned key, so they cannot connect until they
// re-enroll. The enrollment columns give that re-enrollment a target: a
// token bound to a worker id rotates that row in place instead of adding a
// second host, which is what keeps bucket defaults, project overrides, and
// session history attached across the move to mutual TLS.
const MIGRATE_V35_TO_V36_WORKERS: &str = "
ALTER TABLE workers ADD COLUMN peer_key_hash TEXT;
ALTER TABLE workers ADD COLUMN connect_mode TEXT NOT NULL DEFAULT 'dial';
ALTER TABLE workers ADD COLUMN endpoint TEXT NOT NULL DEFAULT '';
";

const MIGRATE_V35_TO_V36_ENROLLMENTS: &str = "
ALTER TABLE worker_enrollments ADD COLUMN token_ciphertext TEXT NOT NULL DEFAULT '';
ALTER TABLE worker_enrollments ADD COLUMN worker_id INTEGER;
ALTER TABLE worker_enrollments ADD COLUMN connect_mode TEXT NOT NULL DEFAULT 'dial';
ALTER TABLE worker_enrollments ADD COLUMN endpoint TEXT NOT NULL DEFAULT '';
";

// A host's pinned key is its identity, so two rows sharing one key are the
// same machine enrolled twice: the second row silently left the first one's
// per-worker project paths, bucket defaults, and session history attached to
// an id nothing connects as any more. Merging keeps the oldest row, which is
// the id that configuration points at, and moves what the newest
// registration reported onto it, so the result is what rebinding would have
// produced had a fresh enrollment always rebound a known key. The unique
// index then makes a second row for one key impossible.
const MIGRATE_V43_TO_V44: &str = "
BEGIN;

CREATE TEMP TABLE worker_merge AS
SELECT w.id AS dup_id,
       (SELECT MIN(o.id) FROM workers o WHERE o.peer_key_hash = w.peer_key_hash) AS keep_id
FROM workers w
WHERE w.peer_key_hash IS NOT NULL AND w.peer_key_hash <> ''
  AND w.id <> (SELECT MIN(o.id) FROM workers o WHERE o.peer_key_hash = w.peer_key_hash);

UPDATE workers SET
    name = (SELECT d.name FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id ORDER BY d.id DESC LIMIT 1),
    hostname = (SELECT d.hostname FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id ORDER BY d.id DESC LIMIT 1),
    platform = (SELECT d.platform FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id ORDER BY d.id DESC LIMIT 1),
    pm_version = (SELECT d.pm_version FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id ORDER BY d.id DESC LIMIT 1),
    default_project_root = (SELECT d.default_project_root FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id ORDER BY d.id DESC LIMIT 1),
    credential_hash = (SELECT d.credential_hash FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id ORDER BY d.id DESC LIMIT 1),
    connect_mode = (SELECT d.connect_mode FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id ORDER BY d.id DESC LIMIT 1),
    endpoint = (SELECT d.endpoint FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id ORDER BY d.id DESC LIMIT 1),
    last_seen_at_unix_ms = (SELECT MAX(d.last_seen_at_unix_ms) FROM workers d JOIN worker_merge m ON m.dup_id = d.id
            WHERE m.keep_id = workers.id)
WHERE id IN (SELECT keep_id FROM worker_merge);

UPDATE sessions SET worker_id = (SELECT keep_id FROM worker_merge WHERE dup_id = sessions.worker_id)
WHERE worker_id IN (SELECT dup_id FROM worker_merge);

UPDATE projects SET worker_id = (SELECT keep_id FROM worker_merge WHERE dup_id = projects.worker_id)
WHERE worker_id IN (SELECT dup_id FROM worker_merge);

UPDATE buckets SET default_worker_id = (SELECT keep_id FROM worker_merge WHERE dup_id = buckets.default_worker_id)
WHERE default_worker_id IN (SELECT dup_id FROM worker_merge);

UPDATE worker_enrollments SET worker_id = (SELECT keep_id FROM worker_merge WHERE dup_id = worker_enrollments.worker_id)
WHERE worker_id IN (SELECT dup_id FROM worker_merge);

UPDATE OR IGNORE project_paths SET worker_id = (SELECT keep_id FROM worker_merge WHERE dup_id = project_paths.worker_id)
WHERE worker_id IN (SELECT dup_id FROM worker_merge);

UPDATE OR IGNORE project_workers SET worker_id = (SELECT keep_id FROM worker_merge WHERE dup_id = project_workers.worker_id)
WHERE worker_id IN (SELECT dup_id FROM worker_merge);

UPDATE OR IGNORE bucket_workers SET worker_id = (SELECT keep_id FROM worker_merge WHERE dup_id = bucket_workers.worker_id)
WHERE worker_id IN (SELECT dup_id FROM worker_merge);

DELETE FROM workers WHERE id IN (SELECT dup_id FROM worker_merge);

DROP TABLE worker_merge;

CREATE UNIQUE INDEX IF NOT EXISTS workers_peer_key_hash
    ON workers(peer_key_hash) WHERE peer_key_hash IS NOT NULL AND peer_key_hash <> '';

COMMIT;
";

/// The watchdog that recovers a turn whose end was never reported needs to
/// know whether a generation's lifecycle hooks ever worked. That was only
/// ever held in memory, so a restart left every session still mid-turn on a
/// worker permanently unwatched. Recording it on the run makes the answer
/// outlive the process that observed it.
const MIGRATE_V45_TO_V46: &str =
    "ALTER TABLE terminal_runs ADD COLUMN hook_seen_at_unix_ms INTEGER;";

/// An agent that serves its own session API is told which loopback port
/// to bind, and that port is the whole address of its inbound channel.
/// Holding it on the session means a message can still be delivered
/// after a daemon restart, which is exactly when a supervisor is trying
/// to pick its workers back up.
const MIGRATE_V46_TO_V47: &str = "ALTER TABLE sessions ADD COLUMN agent_port INTEGER;";

/// A message between sessions, and the single reply it may carry back.
///
/// The reply capability lives here rather than in memory because the
/// two ends of one exchange are separate processes with separate
/// lifetimes: a worker can be answering while the daemon restarts or a
/// worker host reconnects, and a reply token that did not survive that
/// would strand the sender waiting on an answer nobody can give.
const MIGRATE_V47_TO_V48: &str = "
CREATE TABLE IF NOT EXISTS agent_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_session_id INTEGER NOT NULL,
    to_session_id INTEGER NOT NULL,
    body TEXT NOT NULL,
    transport TEXT NOT NULL DEFAULT '',
    reply_token TEXT,
    reply_expires_at_unix_ms INTEGER,
    reply_used_at_unix_ms INTEGER,
    reply_body TEXT,
    replied_at_unix_ms INTEGER,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_messages_to ON agent_messages(to_session_id);
CREATE INDEX IF NOT EXISTS idx_agent_messages_from ON agent_messages(from_session_id);
";

const MIGRATE_V48_TO_V49: &str = crate::plan_store::PLAN_SCHEMA;

// One-retry rule for refresh tokens: a lost-response retry returns the
// stored successor pair instead of revoking. The retry is spent by a
// successor rotation or first successor access-token use.
/// A directory share's forward is bound to whatever ephemeral port the
/// worker currently holds, so the port cannot identify it the way it
/// identifies a published port. The unique key becomes partial, and the
/// table is rebuilt because SQLite cannot drop a table constraint.
// A worker reports the container runtime holding it from protocol 20
// on. An existing row has never reported one, and a worker on the host
// never will, so the empty default is also the true value for both.
// Added by column probe like the other `workers` additions rather than
// by stamped version, so it applies to a database at any version.
const MIGRATE_WORKERS_ADD_RUNTIME: &str = "
ALTER TABLE workers ADD COLUMN runtime TEXT NOT NULL DEFAULT '';
ALTER TABLE workers ADD COLUMN container TEXT NOT NULL DEFAULT '';";

const MIGRATE_V54_TO_V55: &str = "
CREATE TABLE session_dir_shares (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    slug TEXT NOT NULL,
    label TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    UNIQUE(session_id, path)
);
CREATE TABLE session_forwards_rebuilt (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    worker_port INTEGER NOT NULL,
    listener_port INTEGER NOT NULL,
    slug TEXT NOT NULL DEFAULT '',
    label TEXT NOT NULL DEFAULT '',
    scheme TEXT NOT NULL DEFAULT 'http',
    created_at_unix_ms INTEGER NOT NULL,
    dir_share_id INTEGER REFERENCES session_dir_shares(id) ON DELETE CASCADE
);
INSERT INTO session_forwards_rebuilt
    (id, session_id, worker_port, listener_port, slug, label, scheme, created_at_unix_ms)
    SELECT id, session_id, worker_port, listener_port, slug, label, scheme, created_at_unix_ms
    FROM session_forwards;
DROP TABLE session_forwards;
ALTER TABLE session_forwards_rebuilt RENAME TO session_forwards;
CREATE UNIQUE INDEX idx_session_forwards_slug
    ON session_forwards(slug) WHERE slug != '';
CREATE UNIQUE INDEX idx_session_forwards_port
    ON session_forwards(session_id, worker_port) WHERE dir_share_id IS NULL;
CREATE INDEX idx_session_forwards_dir_share
    ON session_forwards(dir_share_id) WHERE dir_share_id IS NOT NULL;
";

const MIGRATE_V50_TO_V51: &str = "
ALTER TABLE mobile_refresh_tokens ADD COLUMN successor_refresh_hash TEXT;
ALTER TABLE mobile_refresh_tokens ADD COLUMN successor_access_hash TEXT;
ALTER TABLE mobile_refresh_tokens ADD COLUMN retry_at_unix_ms INTEGER;
ALTER TABLE mobile_access_tokens ADD COLUMN first_used_at_unix_ms INTEGER;
";

// The dashboard authenticates with an access token of its own, and a browser
// is not a device: it enrols nothing, holds no refresh token and is listed
// nowhere. So the column naming a device is optional and the user the token
// speaks for is recorded on the row itself. SQLite cannot relax a NOT NULL, so
// the table is rebuilt. Live device tokens are carried over rather than
// dropped, which saves every phone an unnecessary refresh.
const MIGRATE_ACCESS_TOKENS_WITHOUT_A_DEVICE: &str = "
CREATE TABLE mobile_access_tokens_rebuilt (
    token_hash TEXT PRIMARY KEY,
    device_id INTEGER REFERENCES mobile_devices(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    session_token_hash TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    first_used_at_unix_ms INTEGER
);
INSERT INTO mobile_access_tokens_rebuilt
    (token_hash, device_id, user_id, session_token_hash, created_at_unix_ms,
     expires_at_unix_ms, first_used_at_unix_ms)
SELECT t.token_hash, t.device_id, d.user_id, NULL, t.created_at_unix_ms,
       t.expires_at_unix_ms, t.first_used_at_unix_ms
FROM mobile_access_tokens t JOIN mobile_devices d ON d.id = t.device_id;
DROP TABLE mobile_access_tokens;
ALTER TABLE mobile_access_tokens_rebuilt RENAME TO mobile_access_tokens;
CREATE INDEX idx_mobile_access_device ON mobile_access_tokens(device_id);
CREATE INDEX idx_mobile_access_session
    ON mobile_access_tokens(session_token_hash)
    WHERE session_token_hash IS NOT NULL;
";

// The same for the one-use tickets a WebSocket upgrade consumes, which the
// dashboard now mints for the same reason the phone's terminal WebView does:
// a handshake carries no header a page can set. A ticket lives for seconds,
// so these are dropped rather than carried.
const MIGRATE_SOCKET_TICKETS_WITHOUT_A_DEVICE: &str = "
CREATE TABLE socket_tickets_rebuilt (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id INTEGER REFERENCES mobile_devices(id) ON DELETE CASCADE,
    terminal_id INTEGER,
    generation INTEGER,
    replay_bytes INTEGER,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    consumed_at_unix_ms INTEGER
);
DROP TABLE socket_tickets;
ALTER TABLE socket_tickets_rebuilt RENAME TO socket_tickets;
CREATE INDEX idx_socket_tickets_device ON socket_tickets(device_id);
";

const MIGRATE_V51_TO_V52: &str = "
ALTER TABLE sessions ADD COLUMN goal TEXT NOT NULL DEFAULT '';
";

const MIGRATE_V52_TO_V53: &str = "
ALTER TABLE session_activity ADD COLUMN last_user_submit_at_unix_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_activity ADD COLUMN last_agent_turn_at_unix_ms INTEGER NOT NULL DEFAULT 0;
";

// Forwards published before this are left unnamed and keep reaching
// their share host by id. The index is partial so those empty slugs do
// not collide with each other.
const MIGRATE_V53_TO_V54: &str = "
ALTER TABLE session_forwards ADD COLUMN slug TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX IF NOT EXISTS idx_session_forwards_slug
    ON session_forwards(slug) WHERE slug != '';
";

// Rebuilds the session search index with the goal column. Runs whenever
// the index lacks it, which covers both upgrades and the fresh-database
// path through MIGRATE_V25_TO_V26.
const SESSION_SEARCH_WITH_GOAL: &str = r#"
DROP TRIGGER IF EXISTS session_search_insert;
DROP TRIGGER IF EXISTS session_search_delete;
DROP TRIGGER IF EXISTS session_search_update;
DROP TRIGGER IF EXISTS session_search_project_name;
DROP TRIGGER IF EXISTS session_search_bucket_name;
DROP TABLE IF EXISTS session_search;
CREATE VIRTUAL TABLE session_search USING fts5(
    task_title, goal, headline, summary, activity, project_name, bucket_name, cwd,
    agent_session_id, tokenize='unicode61'
);
INSERT INTO session_search(rowid, task_title, goal, headline, summary, activity,
    project_name, bucket_name, cwd, agent_session_id)
SELECT s.id, s.task_title, s.goal, s.headline, s.summary, s.activity, p.name, b.name,
       s.cwd, COALESCE(s.agent_session_id, '')
FROM sessions s JOIN projects p ON p.id=s.project_id JOIN buckets b ON b.id=p.bucket_id;
CREATE TRIGGER session_search_insert AFTER INSERT ON sessions BEGIN
  INSERT INTO session_search(rowid, task_title, goal, headline, summary, activity,
      project_name, bucket_name, cwd, agent_session_id)
  SELECT NEW.id, NEW.task_title, NEW.goal, NEW.headline, NEW.summary, NEW.activity,
         p.name, b.name, NEW.cwd, COALESCE(NEW.agent_session_id, '')
  FROM projects p JOIN buckets b ON b.id=p.bucket_id WHERE p.id=NEW.project_id;
END;
CREATE TRIGGER session_search_delete AFTER DELETE ON sessions BEGIN
  DELETE FROM session_search WHERE rowid=OLD.id;
END;
CREATE TRIGGER session_search_update AFTER UPDATE OF project_id, task_title, goal, headline,
    summary, activity, cwd, agent_session_id ON sessions BEGIN
  DELETE FROM session_search WHERE rowid=OLD.id;
  INSERT INTO session_search(rowid, task_title, goal, headline, summary, activity,
      project_name, bucket_name, cwd, agent_session_id)
  SELECT NEW.id, NEW.task_title, NEW.goal, NEW.headline, NEW.summary, NEW.activity,
         p.name, b.name, NEW.cwd, COALESCE(NEW.agent_session_id, '')
  FROM projects p JOIN buckets b ON b.id=p.bucket_id WHERE p.id=NEW.project_id;
END;
CREATE TRIGGER session_search_project_name AFTER UPDATE OF name ON projects BEGIN
  DELETE FROM session_search WHERE rowid IN (SELECT id FROM sessions WHERE project_id=NEW.id);
  INSERT INTO session_search(rowid, task_title, goal, headline, summary, activity,
      project_name, bucket_name, cwd, agent_session_id)
  SELECT s.id, s.task_title, s.goal, s.headline, s.summary, s.activity, NEW.name, b.name,
         s.cwd, COALESCE(s.agent_session_id, '')
  FROM sessions s JOIN buckets b ON b.id=NEW.bucket_id WHERE s.project_id=NEW.id;
END;
CREATE TRIGGER session_search_bucket_name AFTER UPDATE OF name ON buckets BEGIN
  DELETE FROM session_search WHERE rowid IN
      (SELECT s.id FROM sessions s JOIN projects p ON p.id=s.project_id WHERE p.bucket_id=NEW.id);
  INSERT INTO session_search(rowid, task_title, goal, headline, summary, activity,
      project_name, bucket_name, cwd, agent_session_id)
  SELECT s.id, s.task_title, s.goal, s.headline, s.summary, s.activity, p.name, NEW.name,
         s.cwd, COALESCE(s.agent_session_id, '')
  FROM sessions s JOIN projects p ON p.id=s.project_id WHERE p.bucket_id=NEW.id;
END;
"#;

/// One address the controller should be dialing, and how it will be let in:
/// a pinned key for a host already enrolled, or a sealed enrollment token
/// for one that is not yet.
/// One message handed to a session, with the single reply it may carry
/// back when the sender asked for one.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentMessage {
    pub id: u64,
    pub from_session_id: u64,
    pub to_session_id: u64,
    pub body: String,
    pub transport: String,
    pub reply_token: Option<String>,
    pub reply_expires_at_unix_ms: Option<i64>,
    pub reply_used_at_unix_ms: Option<i64>,
    pub reply_body: Option<String>,
    pub replied_at_unix_ms: Option<i64>,
}

impl AgentMessage {
    /// Whether this message can still be answered: it asked for a
    /// reply, nothing has spent the capability, and it has not aged out.
    pub fn awaits_reply(&self, now_unix_ms: i64) -> bool {
        self.reply_token.is_some()
            && self.reply_used_at_unix_ms.is_none()
            && self
                .reply_expires_at_unix_ms
                .is_none_or(|expires| now_unix_ms < expires)
    }
}

fn row_to_agent_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMessage> {
    Ok(AgentMessage {
        id: row.get::<_, i64>(0)? as u64,
        from_session_id: row.get::<_, i64>(1)? as u64,
        to_session_id: row.get::<_, i64>(2)? as u64,
        body: row.get(3)?,
        transport: row.get(4)?,
        reply_token: row.get(5)?,
        reply_expires_at_unix_ms: row.get(6)?,
        reply_used_at_unix_ms: row.get(7)?,
        reply_body: row.get(8)?,
        replied_at_unix_ms: row.get(9)?,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct DialTarget {
    pub endpoint: String,
    pub peer_key_hash: Option<String>,
    pub token_ciphertext: Option<String>,
}

/// A burned enrollment token. `worker_id` names the host to re-enroll in
/// place, and is absent when the token enrolls a new one.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumedEnrollment {
    pub label: String,
    pub worker_id: Option<u64>,
    pub connect_mode: ConnectMode,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPage {
    pub sessions: Vec<Session>,
    pub next_cursor: String,
    pub total: u64,
}

const BACKFILL_SESSION_ACTIVITY: &str = "
INSERT OR IGNORE INTO session_activity (
    session_id, last_agent_activity_at_unix_ms, last_user_interaction_at_unix_ms
)
SELECT id,
       0,
       0
FROM sessions;
";

const SCHEMA: &str = "
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE user_settings (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (user_id, key)
);
CREATE TABLE auth_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE mobile_devices (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL DEFAULT '',
    platform TEXT NOT NULL DEFAULT '',
    app_installation_id TEXT NOT NULL,
    refresh_family_id TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    last_seen_at_unix_ms INTEGER,
    revoked_at_unix_ms INTEGER
);
CREATE TABLE mobile_refresh_tokens (
    id INTEGER PRIMARY KEY,
    device_id INTEGER NOT NULL REFERENCES mobile_devices(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    used_at_unix_ms INTEGER
);
CREATE TABLE mobile_access_tokens (
    token_hash TEXT PRIMARY KEY,
    device_id INTEGER NOT NULL REFERENCES mobile_devices(id) ON DELETE CASCADE,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE mobile_enrollment_tokens (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    used_at_unix_ms INTEGER
);
CREATE INDEX idx_mobile_devices_user ON mobile_devices(user_id, id);
CREATE UNIQUE INDEX idx_mobile_devices_installation
    ON mobile_devices(user_id, app_installation_id)
    WHERE revoked_at_unix_ms IS NULL;
CREATE INDEX idx_mobile_refresh_device ON mobile_refresh_tokens(device_id);
CREATE INDEX idx_mobile_access_device ON mobile_access_tokens(device_id);
CREATE TABLE socket_tickets (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id INTEGER NOT NULL REFERENCES mobile_devices(id) ON DELETE CASCADE,
    terminal_id INTEGER,
    generation INTEGER,
    replay_bytes INTEGER,
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    consumed_at_unix_ms INTEGER
);
CREATE INDEX idx_socket_tickets_device ON socket_tickets(device_id);
CREATE TABLE mobile_push_endpoints (
    device_id INTEGER PRIMARY KEY REFERENCES mobile_devices(id) ON DELETE CASCADE,
    token_ciphertext TEXT NOT NULL,
    environment TEXT NOT NULL CHECK(environment IN ('production','sandbox')),
    locale TEXT NOT NULL DEFAULT '',
    previews_enabled INTEGER NOT NULL DEFAULT 0,
    event_mask INTEGER NOT NULL,
    public_key TEXT NOT NULL DEFAULT '',
    push_counter INTEGER NOT NULL DEFAULT 0,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    disabled_at_unix_ms INTEGER,
    disabled_reason TEXT NOT NULL DEFAULT ''
);
CREATE TABLE notification_deliveries (
    id INTEGER PRIMARY KEY,
    event_id TEXT NOT NULL,
    device_id INTEGER NOT NULL REFERENCES mobile_devices(id) ON DELETE CASCADE,
    session_id INTEGER NOT NULL,
    state TEXT NOT NULL,
    collapse_id TEXT NOT NULL,
    status TEXT NOT NULL,
    provider_message_id TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_unix_ms INTEGER NOT NULL,
    last_error TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    UNIQUE(event_id, device_id)
);
CREATE INDEX idx_notification_deliveries_due
    ON notification_deliveries(next_attempt_at_unix_ms) WHERE status = 'pending';
CREATE TABLE push_policies (
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT '' CHECK(role IN ('', 'worker', 'supervisor')),
    events TEXT NOT NULL,
    scope TEXT NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY (bucket_id, role)
);
CREATE TABLE workers (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    hostname TEXT NOT NULL DEFAULT '',
    platform TEXT NOT NULL DEFAULT '',
    pm_version TEXT NOT NULL DEFAULT '',
    runtime TEXT NOT NULL DEFAULT '',
    container TEXT NOT NULL DEFAULT '',
    default_project_root TEXT NOT NULL DEFAULT '',
    credential_hash TEXT,
    peer_key_hash TEXT,
    connect_mode TEXT NOT NULL DEFAULT 'dial',
    endpoint TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    last_seen_at_unix_ms INTEGER
);
-- A host's pinned key is its identity, so a second row for one key would
-- strand the first row's per-worker paths and session history.
CREATE UNIQUE INDEX workers_peer_key_hash
    ON workers(peer_key_hash) WHERE peer_key_hash IS NOT NULL AND peer_key_hash <> '';
CREATE TABLE worker_enrollments (
    token_hash TEXT PRIMARY KEY,
    token_ciphertext TEXT NOT NULL DEFAULT '',
    label TEXT NOT NULL DEFAULT '',
    worker_id INTEGER,
    connect_mode TEXT NOT NULL DEFAULT 'dial',
    endpoint TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    used_at_unix_ms INTEGER
);
CREATE TABLE model_profiles (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    api_key_ciphertext TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE model_profile_endpoints (
    profile_id INTEGER NOT NULL REFERENCES model_profiles(id) ON DELETE CASCADE,
    dialect TEXT NOT NULL,
    model TEXT NOT NULL,
    base_url TEXT NOT NULL DEFAULT '',
    background_model TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (profile_id, dialect)
);
CREATE TABLE buckets (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    position INTEGER NOT NULL DEFAULT 0,
    permission_mode TEXT NOT NULL DEFAULT 'default',
    default_agent TEXT,
    default_worker_id INTEGER NOT NULL DEFAULT 0,
    is_default INTEGER NOT NULL DEFAULT 0,
    model_profile_id INTEGER REFERENCES model_profiles(id)
);
CREATE TABLE projects (
    id INTEGER PRIMARY KEY,
    bucket_id INTEGER NOT NULL REFERENCES buckets(id),
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    permission_mode TEXT NOT NULL DEFAULT 'inherit',
    default_agent TEXT,
    worker_id INTEGER,
    model_profile_id INTEGER REFERENCES model_profiles(id),
    UNIQUE(bucket_id, name)
);
CREATE TABLE project_paths (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    worker_id INTEGER NOT NULL REFERENCES workers(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    PRIMARY KEY (project_id, worker_id)
);
CREATE TABLE bucket_workers (
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    worker_id INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    PRIMARY KEY (bucket_id, worker_id)
);
CREATE TABLE project_workers (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    worker_id INTEGER NOT NULL REFERENCES workers(id) ON DELETE RESTRICT,
    PRIMARY KEY (project_id, worker_id)
);
CREATE UNIQUE INDEX idx_one_default_bucket ON buckets(is_default) WHERE is_default = 1;
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    agent TEXT NOT NULL,
    agent_source TEXT NOT NULL DEFAULT 'explicit',
    state TEXT NOT NULL,
    task_title TEXT NOT NULL,
    task_prompt TEXT NOT NULL,
    agent_session_id TEXT,
    created_at_unix_ms INTEGER NOT NULL,
    ended_at_unix_ms INTEGER,
    exit_code INTEGER,
    state_detail TEXT NOT NULL DEFAULT '',
    session_token TEXT,
    activity TEXT NOT NULL DEFAULT '',
    progress_percent INTEGER,
    transcript_path TEXT,
    permission_mode TEXT NOT NULL DEFAULT 'default',
    worker_id INTEGER NOT NULL DEFAULT 0,
    cwd TEXT NOT NULL DEFAULT '',
    agent_resumable INTEGER NOT NULL DEFAULT 0,
    desired_running INTEGER NOT NULL DEFAULT 0,
    goal TEXT NOT NULL DEFAULT '',
    headline TEXT NOT NULL DEFAULT '',
    summary TEXT NOT NULL DEFAULT '',
    items_api INTEGER NOT NULL DEFAULT 1,
    supervisor_api INTEGER NOT NULL DEFAULT 0,
    spawned_by_session_id INTEGER REFERENCES sessions(id)
    ,role TEXT NOT NULL DEFAULT 'worker' CHECK(role IN ('worker','supervisor'))
    ,state_revision INTEGER NOT NULL DEFAULT 0
    ,working_since_unix_ms INTEGER
    ,model_profile_id INTEGER REFERENCES model_profiles(id) ON DELETE SET NULL
    ,model_profile_source TEXT
    ,announced_wake_key TEXT
    ,git_branch TEXT NOT NULL DEFAULT ''
    ,git_worktree TEXT NOT NULL DEFAULT ''
    ,git_repo_root TEXT NOT NULL DEFAULT ''
    ,git_commit TEXT NOT NULL DEFAULT ''
    ,git_upstream TEXT NOT NULL DEFAULT ''
    ,git_dirty INTEGER
);
CREATE TABLE instruction_layers (
    id INTEGER PRIMARY KEY,
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
    target TEXT NOT NULL CHECK(target IN ('all','worker','supervisor')),
    markdown TEXT NOT NULL DEFAULT '', revision INTEGER NOT NULL DEFAULT 1,
    updated_at_unix_ms INTEGER NOT NULL,
    updated_by_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL
);
CREATE UNIQUE INDEX instruction_layers_scope ON instruction_layers(bucket_id, COALESCE(project_id, 0), target);
CREATE TABLE instruction_revisions (
    layer_id INTEGER NOT NULL REFERENCES instruction_layers(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL, markdown TEXT NOT NULL, note TEXT NOT NULL DEFAULT '',
    updated_at_unix_ms INTEGER NOT NULL,
    updated_by_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    PRIMARY KEY(layer_id, revision)
);
CREATE TABLE session_instruction_snapshots (
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    terminal_generation INTEGER NOT NULL, compiled_markdown TEXT NOT NULL,
    source_revisions_json TEXT NOT NULL, content_hash TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY(session_id, terminal_generation)
);
CREATE TABLE session_glance (
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    key TEXT NOT NULL,
    label TEXT NOT NULL,
    value TEXT NOT NULL,
    kind TEXT NOT NULL,
    severity TEXT NOT NULL,
    PRIMARY KEY (session_id, position)
);
CREATE TABLE session_context (
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    label TEXT NOT NULL,
    value TEXT NOT NULL,
    kind TEXT NOT NULL,
    severity TEXT NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY (session_id, key)
);
CREATE TABLE terminals (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('agent', 'shell')),
    title TEXT NOT NULL,
    cwd TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    desired_running INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX idx_terminals_agent ON terminals(session_id) WHERE kind = 'agent';
CREATE INDEX idx_terminals_session ON terminals(session_id);
CREATE TABLE terminal_runs (
    id INTEGER PRIMARY KEY,
    terminal_id INTEGER NOT NULL REFERENCES terminals(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('starting', 'running', 'exited', 'failed')),
    started_at_unix_ms INTEGER,
    ended_at_unix_ms INTEGER,
    exit_code INTEGER,
    scrollback_available INTEGER NOT NULL DEFAULT 0,
    hook_seen_at_unix_ms INTEGER,
    UNIQUE(terminal_id, generation)
);
CREATE TABLE activity_reports (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    ts_unix_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE session_activity (
    session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    last_agent_activity_at_unix_ms INTEGER NOT NULL DEFAULT 0,
    last_user_interaction_at_unix_ms INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE notifications (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    ts_unix_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
    read_at_unix_ms INTEGER
);
CREATE TABLE workspaces (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    layout_json TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE session_dir_shares (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    slug TEXT NOT NULL,
    label TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    UNIQUE(session_id, path)
);
CREATE TABLE session_forwards (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    worker_port INTEGER NOT NULL,
    listener_port INTEGER NOT NULL,
    slug TEXT NOT NULL DEFAULT '',
    label TEXT NOT NULL DEFAULT '',
    scheme TEXT NOT NULL DEFAULT 'http',
    created_at_unix_ms INTEGER NOT NULL,
    dir_share_id INTEGER REFERENCES session_dir_shares(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX idx_session_forwards_slug
    ON session_forwards(slug) WHERE slug != '';
CREATE UNIQUE INDEX idx_session_forwards_port
    ON session_forwards(session_id, worker_port) WHERE dir_share_id IS NULL;
CREATE INDEX idx_session_forwards_dir_share
    ON session_forwards(dir_share_id) WHERE dir_share_id IS NOT NULL;
CREATE TABLE items (
    id INTEGER PRIMARY KEY,
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    item_number INTEGER NOT NULL CHECK(item_number > 0),
    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    external_key TEXT,
    title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    question TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK (status IN
        ('inbox','planned','in_progress','blocked','blocked_external','done','dropped')),
    priority TEXT NOT NULL CHECK (priority IN ('urgent','high','normal','low')),
    source_kind TEXT NOT NULL CHECK (source_kind IN
        ('email','slack','github','jira','teams','telegram','human','agent','other')),
    source_detail TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    due_at_unix_ms INTEGER,
    snoozed_until_unix_ms INTEGER,
    created_by_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    done_at_unix_ms INTEGER,
    UNIQUE (bucket_id, external_key),
    UNIQUE (bucket_id, item_number)
);
CREATE TABLE bucket_item_sequences (
    bucket_id INTEGER PRIMARY KEY REFERENCES buckets(id) ON DELETE CASCADE,
    next_item_number INTEGER NOT NULL CHECK(next_item_number > 0)
);
CREATE TABLE item_deps (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    depends_on_item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    PRIMARY KEY (item_id, depends_on_item_id)
);
CREATE TABLE item_sessions (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    PRIMARY KEY (item_id, session_id)
);
CREATE TABLE item_notes (
    id INTEGER PRIMARY KEY,
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    ts_unix_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created','status','note','user_reply')),
    text TEXT NOT NULL
);
CREATE TABLE item_attachments (
    id INTEGER PRIMARY KEY,
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    media_type TEXT NOT NULL,
    byte_length INTEGER NOT NULL,
    sha256 BLOB NOT NULL,
    content BLOB NOT NULL,
    created_at_unix_ms INTEGER NOT NULL,
    created_by_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL
);
CREATE TABLE bucket_briefings (
    id INTEGER PRIMARY KEY,
    bucket_id INTEGER NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
    session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    ts_unix_ms INTEGER NOT NULL,
    markdown TEXT NOT NULL
);
CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE INDEX idx_sessions_project ON sessions(project_id);
CREATE INDEX idx_reports_session ON activity_reports(session_id);
CREATE INDEX idx_notifications_unread ON notifications(session_id) WHERE read_at_unix_ms IS NULL;
CREATE INDEX idx_workspaces_user ON workspaces(user_id, id);
CREATE INDEX idx_items_bucket ON items(bucket_id, status);
CREATE INDEX idx_item_notes_item ON item_notes(item_id);
CREATE INDEX idx_item_attachments_item_time ON item_attachments(item_id, created_at_unix_ms, id);
CREATE INDEX idx_item_attachments_digest ON item_attachments(sha256);
CREATE INDEX idx_briefings_bucket ON bucket_briefings(bucket_id, id);
INSERT INTO workers (id, name, created_at_unix_ms) VALUES (0, 'local', 0);
";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0} not found: {1}")]
    NotFound(&'static str, u64),
    #[error("{0}")]
    Conflict(String),
    #[error("validation failed: {field} is {actual} {unit}, limit is {limit}")]
    Validation {
        field: &'static str,
        limit: usize,
        actual: usize,
        unit: &'static str,
    },
    #[error("database schema version {0} is newer than this binary supports ({SCHEMA_VERSION})")]
    SchemaTooNew(i64),
}

pub type Result<T> = std::result::Result<T, StorageError>;

pub fn validate_item_body(body: Option<&str>) -> Result<()> {
    let Some(body) = body else {
        return Ok(());
    };
    let actual = body.chars().count();
    if actual > ITEM_BODY_MAX {
        return Err(StorageError::Validation {
            field: "body",
            limit: ITEM_BODY_MAX,
            actual,
            unit: "Unicode scalar values",
        });
    }
    Ok(())
}

/// A historical agent report row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityReport {
    pub ts_unix_ms: i64,
    pub kind: String,
    pub payload: String,
}

/// One entry in an item's append-only timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemNote {
    pub id: u64,
    pub session_id: Option<u64>,
    pub ts_unix_ms: i64,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOutcome {
    Created,
    Updated,
    Unchanged,
}

impl ItemOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
        }
    }
}

/// One item write, from the MCP path (agent) or a client command
/// (human). `None` fields leave the stored value untouched; on create
/// they take defaults.
#[derive(Debug, Clone, Default)]
pub struct ItemUpsert {
    /// Update by id, else by (bucket, external_key), else create.
    pub id: Option<u64>,
    pub external_key: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub question: Option<String>,
    pub status: Option<ItemStatus>,
    pub priority: Option<ItemPriority>,
    pub source_kind: Option<ItemSourceKind>,
    pub source_detail: Option<String>,
    pub url: Option<String>,
    pub project_id: Option<u64>,
    pub clear_project: bool,
    pub due_at_unix_ms: Option<i64>,
    pub clear_due: bool,
    /// `Some` replaces all "blocked by" edges.
    pub blocked_by: Option<Vec<u64>>,
    /// Appends a timeline note.
    pub note: Option<String>,
    /// Records that a session works this item.
    pub link_session_id: Option<u64>,
}

/// One enrolled mobile app installation with its audit timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileDevice {
    pub id: u64,
    pub user_id: u64,
    pub name: String,
    pub platform: String,
    pub app_installation_id: String,
    pub refresh_family_id: String,
    pub created_at_unix_ms: i64,
    pub last_seen_at_unix_ms: Option<i64>,
    pub revoked_at_unix_ms: Option<i64>,
}

/// One stored refresh token row of a device's rotating family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileRefreshToken {
    pub id: u64,
    pub device_id: u64,
    pub expires_at_unix_ms: i64,
    pub used_at_unix_ms: Option<i64>,
    pub successor_refresh_hash: Option<String>,
    pub successor_access_hash: Option<String>,
    pub retry_at_unix_ms: Option<i64>,
}

/// Terminal bindings a terminal attach ticket was minted with. A
/// control socket ticket carries no binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalTicketBinding {
    pub terminal_id: u64,
    pub generation: u64,
    pub replay_bytes: u64,
}

/// Who a live access token speaks for, and whether its first use is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessTokenHolder {
    /// The device holding it, or `None` when the dashboard does.
    pub device_id: Option<u64>,
    pub user_id: u64,
    pub first_used: bool,
}

/// A consumed one-use socket ticket with the identity and bindings it
/// was minted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketTicket {
    pub user_id: u64,
    /// The device that minted it, or `None` when the dashboard did.
    pub device_id: Option<u64>,
    pub terminal: Option<TerminalTicketBinding>,
}

/// One device's registered push endpoint. The platform token is stored
/// only as ciphertext sealed with the installation secret and is never
/// returned by list APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushEndpoint {
    pub device_id: u64,
    pub token_ciphertext: String,
    pub environment: String,
    pub locale: String,
    pub previews_enabled: bool,
    pub event_mask: u32,
    /// X25519 public key (base64) for HPKE sealing.
    pub public_key: String,
    /// Monotonic counter per device, included in sealed payloads so the
    /// notification service extension can reject replays.
    pub push_counter: u64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub disabled_at_unix_ms: Option<i64>,
    pub disabled_reason: String,
}

/// One queued or settled push delivery. The (event_id, device_id)
/// pair is unique, which is what makes re-derived transitions after a
/// restart or a provider retry collapse into one alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationDelivery {
    pub id: u64,
    pub event_id: String,
    pub device_id: u64,
    pub session_id: u64,
    pub state: String,
    pub collapse_id: String,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub attempt_count: u32,
    pub next_attempt_at_unix_ms: i64,
    pub last_error: String,
    pub created_at_unix_ms: i64,
}

pub const DELIVERY_PENDING: &str = "pending";
pub const DELIVERY_SENT: &str = "sent";
pub const DELIVERY_FAILED: &str = "failed";
pub const DELIVERY_UNREGISTERED: &str = "unregistered";
pub const DELIVERY_SUPPRESSED: &str = "suppressed";

/// A per-bucket notification policy override; an empty role applies to
/// both roles. Controller-wide defaults live in daemon settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPolicy {
    pub bucket_id: u64,
    pub role: String,
    pub events: String,
    pub scope: String,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: u64,
    pub name: String,
    pub layout_json: String,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub position: u32,
}

/// One agent report applied to a session's live fields plus the
/// append-only history row.
pub struct ActivityUpdate<'a> {
    pub state: SessionState,
    pub state_detail: &'a str,
    pub activity: &'a str,
    pub progress_percent: Option<u32>,
    pub kind: &'a str,
    pub payload: &'a str,
    pub ts_unix_ms: i64,
}

/// Debounced terminal activity checkpoint for one session. A missing
/// side leaves its stored timestamp unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionActivityUpdate {
    pub session_id: u64,
    pub last_agent_activity_at_unix_ms: Option<i64>,
    pub last_user_interaction_at_unix_ms: Option<i64>,
    pub last_user_submit_at_unix_ms: Option<i64>,
}

/// All queries are sub-millisecond on a local file, so a mutex around
/// one connection is sufficient; async callers treat calls as cheap.
pub struct Storage {
    pub(crate) conn: Mutex<Connection>,
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?, true)
    }

    pub fn open_in_memory() -> Result<Self> {
        // In-memory stores are an explicit low-level/testing facility rather
        // than a persisted installation, so they retain their empty baseline.
        Self::init(Connection::open_in_memory()?, false)
    }

    fn init(mut conn: Connection, seed_fresh_database: bool) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StorageError::SchemaTooNew(version));
        }
        // These migrations are idempotent, so they can run before the older
        // sequential migrations without leaving a half-upgraded database
        // unrecoverable if one of those later steps fails.
        if (1..SCHEMA_VERSION).contains(&version) {
            conn.execute_batch(MIGRATE_V15_TO_V16)?;
            conn.execute_batch(MIGRATE_V18_TO_V19)?;
        }
        match version {
            0 => {
                conn.execute_batch(SCHEMA)?;
                if seed_fresh_database {
                    seed_fresh_default_bucket(&mut conn)?;
                }
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            1 => {
                conn.execute_batch(MIGRATE_V1_TO_V2)?;
                conn.execute_batch(MIGRATE_V2_TO_V3)?;
                conn.execute_batch(MIGRATE_V3_TO_V4)?;
                conn.execute_batch(MIGRATE_V4_TO_V5)?;
                conn.execute_batch(MIGRATE_V5_TO_V6)?;
                conn.execute_batch(MIGRATE_V6_TO_V7)?;
                conn.execute_batch(MIGRATE_V7_TO_V8)?;
                conn.execute_batch(MIGRATE_V8_TO_V9)?;
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            2 => {
                conn.execute_batch(MIGRATE_V2_TO_V3)?;
                conn.execute_batch(MIGRATE_V3_TO_V4)?;
                conn.execute_batch(MIGRATE_V4_TO_V5)?;
                conn.execute_batch(MIGRATE_V5_TO_V6)?;
                conn.execute_batch(MIGRATE_V6_TO_V7)?;
                conn.execute_batch(MIGRATE_V7_TO_V8)?;
                conn.execute_batch(MIGRATE_V8_TO_V9)?;
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            3 => {
                conn.execute_batch(MIGRATE_V3_TO_V4)?;
                conn.execute_batch(MIGRATE_V4_TO_V5)?;
                conn.execute_batch(MIGRATE_V5_TO_V6)?;
                conn.execute_batch(MIGRATE_V6_TO_V7)?;
                conn.execute_batch(MIGRATE_V7_TO_V8)?;
                conn.execute_batch(MIGRATE_V8_TO_V9)?;
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            4 => {
                conn.execute_batch(MIGRATE_V4_TO_V5)?;
                conn.execute_batch(MIGRATE_V5_TO_V6)?;
                conn.execute_batch(MIGRATE_V6_TO_V7)?;
                conn.execute_batch(MIGRATE_V7_TO_V8)?;
                conn.execute_batch(MIGRATE_V8_TO_V9)?;
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            5 => {
                conn.execute_batch(MIGRATE_V5_TO_V6)?;
                conn.execute_batch(MIGRATE_V6_TO_V7)?;
                conn.execute_batch(MIGRATE_V7_TO_V8)?;
                conn.execute_batch(MIGRATE_V8_TO_V9)?;
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            6 => {
                conn.execute_batch(MIGRATE_V6_TO_V7)?;
                conn.execute_batch(MIGRATE_V7_TO_V8)?;
                conn.execute_batch(MIGRATE_V8_TO_V9)?;
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            7 => {
                conn.execute_batch(MIGRATE_V7_TO_V8)?;
                conn.execute_batch(MIGRATE_V8_TO_V9)?;
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            8 => {
                conn.execute_batch(MIGRATE_V8_TO_V9)?;
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            9 => {
                conn.execute_batch(MIGRATE_V9_TO_V10)?;
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            10 => {
                conn.execute_batch(MIGRATE_V10_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            11 => {
                conn.execute_batch(MIGRATE_V11_TO_V12)?;
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            12 => {
                conn.execute_batch(MIGRATE_V12_TO_V13)?;
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            13 => {
                conn.execute_batch(MIGRATE_V13_TO_V14)?;
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            14 => {
                conn.execute_batch(MIGRATE_V14_TO_V15)?;
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            15 => {
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            16 => {
                conn.execute_batch(MIGRATE_V16_TO_V17)?;
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            17 => {
                conn.execute_batch(MIGRATE_V17_TO_V18)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            18 => {
                conn.execute_batch(MIGRATE_V18_TO_V19)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            19 => {
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            20 => {
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            21 => {
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            22..=SCHEMA_VERSION => {}
            v => return Err(StorageError::SchemaTooNew(v)),
        }
        if (1..SCHEMA_VERSION).contains(&version) {
            let has_role: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='role'",
                [],
                |r| r.get(0),
            )?;
            if has_role == 0 {
                conn.execute_batch(MIGRATE_V19_TO_V20)?;
            }
        }
        conn.execute_batch(MIGRATE_V20_TO_V21)?;
        let has_workspace_home_position: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name='workspace_home_position'",
            [],
            |row| row.get(0),
        )?;
        if has_workspace_home_position > 0 {
            conn.execute_batch(MIGRATE_V21_TO_V22)?;
        }
        let has_buckets: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='buckets'",
            [],
            |row| row.get(0),
        )?;
        let has_bucket_default: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('buckets') WHERE name='is_default'",
            [],
            |row| row.get(0),
        )?;
        if has_buckets > 0 && has_bucket_default == 0 {
            conn.execute_batch(MIGRATE_V22_TO_V23)?;
        }
        let has_item_attachments: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='item_attachments'",
            [],
            |row| row.get(0),
        )?;
        if has_item_attachments == 0 {
            conn.execute_batch(MIGRATE_V23_TO_V24)?;
        }
        let has_item_number: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('items') WHERE name='item_number'",
            [],
            |row| row.get(0),
        )?;
        if has_item_number == 0 {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute_batch(MIGRATE_V24_TO_V25)?;
            rewrite_durable_legacy_item_links(&tx)?;
            tx.commit()?;
        }
        let has_session_search: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_search'",
            [],
            |row| row.get(0),
        )?;
        let has_session_search_sources: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions','projects','buckets')",
            [],
            |row| row.get(0),
        )?;
        if has_session_search == 0 && has_session_search_sources == 3 {
            conn.execute_batch(MIGRATE_V25_TO_V26)?;
        }
        let has_default_agent: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('buckets') WHERE name='default_agent'",
            [],
            |row| row.get(0),
        )?;
        if has_buckets > 0 && has_default_agent == 0 {
            conn.execute_batch(MIGRATE_V27_TO_V28)?;
        }
        let has_sessions: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
            [],
            |row| row.get(0),
        )?;
        let has_notifications: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notifications'",
            [],
            |row| row.get(0),
        )?;
        if has_sessions > 0 && has_notifications == 0 {
            conn.execute_batch(MIGRATE_V28_TO_V29)?;
        }
        let has_mobile_devices: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='mobile_devices'",
            [],
            |row| row.get(0),
        )?;
        if has_mobile_devices == 0 {
            conn.execute_batch(MIGRATE_V29_TO_V30)?;
        }
        let has_socket_tickets: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='socket_tickets'",
            [],
            |row| row.get(0),
        )?;
        if has_socket_tickets == 0 {
            conn.execute_batch(MIGRATE_V30_TO_V31)?;
        }
        let has_push_endpoints: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='mobile_push_endpoints'",
            [],
            |row| row.get(0),
        )?;
        if has_push_endpoints == 0 {
            conn.execute_batch(MIGRATE_V31_TO_V32)?;
        }
        let has_model_profiles: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_profiles'",
            [],
            |row| row.get(0),
        )?;
        if has_sessions > 0 && has_model_profiles == 0 {
            conn.execute_batch(MIGRATE_V32_TO_V33)?;
            if has_buckets > 0 {
                conn.execute_batch(MIGRATE_V32_TO_V33_CATALOG)?;
            }
        }
        let has_project_paths: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='project_paths'",
            [],
            |row| row.get(0),
        )?;
        if has_project_paths > 0 && (1..PROJECT_PATH_WRITEBACK_SCHEMA_VERSION).contains(&version) {
            conn.execute_batch(MIGRATE_V33_TO_V34)?;
        }
        let has_workers: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workers'",
            [],
            |row| row.get(0),
        )?;
        let has_worker_pm_version: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('workers') WHERE name='pm_version'",
            [],
            |row| row.get(0),
        )?;
        if has_workers > 0 && has_worker_pm_version == 0 {
            conn.execute_batch(MIGRATE_V34_TO_V35)?;
        }
        let has_worker_peer_key: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('workers') WHERE name='peer_key_hash'",
            [],
            |row| row.get(0),
        )?;
        if has_workers > 0 && has_worker_peer_key == 0 {
            conn.execute_batch(MIGRATE_V35_TO_V36_WORKERS)?;
        }
        let has_worker_runtime: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('workers') WHERE name='runtime'",
            [],
            |row| row.get(0),
        )?;
        if has_workers > 0 && has_worker_runtime == 0 {
            conn.execute_batch(MIGRATE_WORKERS_ADD_RUNTIME)?;
        }
        let has_sealed_enrollment: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('worker_enrollments') \
             WHERE name='token_ciphertext'",
            [],
            |row| row.get(0),
        )?;
        let has_enrollments: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='worker_enrollments'",
            [],
            |row| row.get(0),
        )?;
        if has_enrollments > 0 && has_sealed_enrollment == 0 {
            conn.execute_batch(MIGRATE_V35_TO_V36_ENROLLMENTS)?;
        }
        let has_announced_wake_key: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='announced_wake_key'",
            [],
            |row| row.get(0),
        )?;
        if has_announced_wake_key == 0 {
            conn.execute_batch(MIGRATE_V36_TO_V37)?;
        }
        let has_git_branch: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='git_branch'",
            [],
            |row| row.get(0),
        )?;
        if has_git_branch == 0 {
            conn.execute_batch(MIGRATE_V37_TO_V38)?;
        }
        let has_reviews: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='reviews'",
            [],
            |row| row.get(0),
        )?;
        if has_reviews == 0 {
            conn.execute_batch(crate::review_store::REVIEW_SCHEMA)?;
        }
        let has_review_scope: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('reviews') WHERE name='explicit_files'",
            [],
            |row| row.get(0),
        )?;
        let has_drafts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('review_viewer_state') WHERE name='drafts'",
            [],
            |row| row.get(0),
        )?;
        if has_reviews > 0 && has_drafts == 0 {
            conn.execute_batch(
                "ALTER TABLE review_viewer_state ADD COLUMN drafts TEXT NOT NULL DEFAULT '{}';",
            )?;
        }
        let has_seen: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('review_viewer_state') WHERE name='seen'",
            [],
            |row| row.get(0),
        )?;
        if has_reviews > 0 && has_seen == 0 {
            conn.execute_batch(
                "ALTER TABLE review_viewer_state ADD COLUMN seen TEXT NOT NULL DEFAULT '{}';",
            )?;
        }
        let has_message_choice: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('review_messages') WHERE name='choice'",
            [],
            |row| row.get(0),
        )?;
        if has_reviews > 0 && has_message_choice == 0 {
            conn.execute_batch(
                "ALTER TABLE review_messages ADD COLUMN choice TEXT NOT NULL DEFAULT '';",
            )?;
        }
        if has_reviews > 0 && has_review_scope == 0 {
            conn.execute_batch(
                "ALTER TABLE reviews ADD COLUMN explicit_files TEXT NOT NULL DEFAULT '';
                 ALTER TABLE reviews ADD COLUMN base_snapshot_id INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE reviews ADD COLUMN skipped TEXT NOT NULL DEFAULT '';",
            )?;
        }
        // Gateway push: add public_key column and widen the provider
        // CHECK to accept 'gateway'. The constraint change requires a
        // table recreation since SQLite cannot ALTER constraints.
        let has_public_key: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('mobile_push_endpoints') WHERE name='public_key'",
            [],
            |row| row.get(0),
        )?;
        if has_push_endpoints > 0 && has_public_key == 0 {
            conn.execute_batch(
                "CREATE TABLE mobile_push_endpoints_new (
                    device_id INTEGER PRIMARY KEY REFERENCES mobile_devices(id) ON DELETE CASCADE,
                    provider TEXT NOT NULL CHECK(provider IN ('apns','fcm','gateway')),
                    token_ciphertext TEXT NOT NULL,
                    environment TEXT NOT NULL CHECK(environment IN ('production','sandbox')),
                    locale TEXT NOT NULL DEFAULT '',
                    previews_enabled INTEGER NOT NULL DEFAULT 0,
                    event_mask INTEGER NOT NULL,
                    public_key TEXT NOT NULL DEFAULT '',
                    created_at_unix_ms INTEGER NOT NULL,
                    updated_at_unix_ms INTEGER NOT NULL,
                    disabled_at_unix_ms INTEGER,
                    disabled_reason TEXT NOT NULL DEFAULT ''
                );
                INSERT INTO mobile_push_endpoints_new
                    (device_id, provider, token_ciphertext, environment, locale,
                     previews_enabled, event_mask, created_at_unix_ms, updated_at_unix_ms,
                     disabled_at_unix_ms, disabled_reason)
                    SELECT device_id, provider, token_ciphertext, environment, locale,
                           previews_enabled, event_mask, created_at_unix_ms, updated_at_unix_ms,
                           disabled_at_unix_ms, disabled_reason
                    FROM mobile_push_endpoints;
                DROP TABLE mobile_push_endpoints;
                ALTER TABLE mobile_push_endpoints_new RENAME TO mobile_push_endpoints;",
            )?;
        }
        // Add push_counter column for replay rejection in sealed notifications.
        let has_push_counter: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('mobile_push_endpoints') WHERE name='push_counter'",
            [],
            |row| row.get(0),
        )?;
        if has_push_endpoints > 0 && has_push_counter == 0 {
            conn.execute_batch(
                "ALTER TABLE mobile_push_endpoints ADD COLUMN push_counter INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        // The relay is the only delivery path, so an endpoint no longer
        // names a provider. Dropping the column needs a table rebuild
        // because it carries a CHECK constraint.
        let has_provider: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('mobile_push_endpoints') WHERE name='provider'",
            [],
            |row| row.get(0),
        )?;
        if has_push_endpoints > 0 && has_provider > 0 {
            conn.execute_batch(
                "CREATE TABLE mobile_push_endpoints_new (
                    device_id INTEGER PRIMARY KEY REFERENCES mobile_devices(id) ON DELETE CASCADE,
                    token_ciphertext TEXT NOT NULL,
                    environment TEXT NOT NULL CHECK(environment IN ('production','sandbox')),
                    locale TEXT NOT NULL DEFAULT '',
                    previews_enabled INTEGER NOT NULL DEFAULT 0,
                    event_mask INTEGER NOT NULL,
                    public_key TEXT NOT NULL DEFAULT '',
                    push_counter INTEGER NOT NULL DEFAULT 0,
                    created_at_unix_ms INTEGER NOT NULL,
                    updated_at_unix_ms INTEGER NOT NULL,
                    disabled_at_unix_ms INTEGER,
                    disabled_reason TEXT NOT NULL DEFAULT ''
                );
                INSERT INTO mobile_push_endpoints_new
                    (device_id, token_ciphertext, environment, locale, previews_enabled,
                     event_mask, public_key, push_counter, created_at_unix_ms,
                     updated_at_unix_ms, disabled_at_unix_ms, disabled_reason)
                    SELECT device_id, token_ciphertext, environment, locale, previews_enabled,
                           event_mask, public_key, push_counter, created_at_unix_ms,
                           updated_at_unix_ms, disabled_at_unix_ms, disabled_reason
                    FROM mobile_push_endpoints;
                DROP TABLE mobile_push_endpoints;
                ALTER TABLE mobile_push_endpoints_new RENAME TO mobile_push_endpoints;",
            )?;
        }
        let has_worker_key_index: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='workers_peer_key_hash'",
            [],
            |row| row.get(0),
        )?;
        if has_workers > 0 && has_worker_key_index == 0 {
            conn.execute_batch(MIGRATE_V43_TO_V44)?;
        }
        // One live device row per (user, app installation): re-enrolling
        // an installation reuses its row instead of stacking a new one,
        // which also stopped one phone collecting a push endpoint per
        // login. Older rows for the same installation are revoked so the
        // partial index can be created.
        let has_installation_index: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
             AND name='idx_mobile_devices_installation'",
            [],
            |row| row.get(0),
        )?;
        if has_installation_index == 0 {
            let superseded = "id NOT IN (SELECT MAX(id) FROM mobile_devices \
                 WHERE revoked_at_unix_ms IS NULL GROUP BY user_id, app_installation_id) \
                 AND revoked_at_unix_ms IS NULL";
            conn.execute(
                &format!(
                    "DELETE FROM mobile_access_tokens WHERE device_id IN \
                     (SELECT id FROM mobile_devices WHERE {superseded})"
                ),
                [],
            )?;
            conn.execute(
                &format!(
                    "DELETE FROM mobile_push_endpoints WHERE device_id IN \
                     (SELECT id FROM mobile_devices WHERE {superseded})"
                ),
                [],
            )?;
            conn.execute(
                &format!("UPDATE mobile_devices SET revoked_at_unix_ms = ?1 WHERE {superseded}"),
                params![now_unix_ms()],
            )?;
            conn.execute_batch(
                "CREATE UNIQUE INDEX idx_mobile_devices_installation
                    ON mobile_devices(user_id, app_installation_id)
                    WHERE revoked_at_unix_ms IS NULL;",
            )?;
        }
        let has_terminal_runs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='terminal_runs'",
            [],
            |row| row.get(0),
        )?;
        let has_hook_seen: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('terminal_runs') \
             WHERE name='hook_seen_at_unix_ms'",
            [],
            |row| row.get(0),
        )?;
        if has_terminal_runs > 0 && has_hook_seen == 0 {
            conn.execute_batch(MIGRATE_V45_TO_V46)?;
        }
        let has_agent_port: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='agent_port'",
            [],
            |row| row.get(0),
        )?;
        if has_agent_port == 0 {
            conn.execute_batch(MIGRATE_V46_TO_V47)?;
        }
        conn.execute_batch(MIGRATE_V47_TO_V48)?;
        conn.execute_batch(MIGRATE_V48_TO_V49)?;
        let has_plan_batch_key: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('plan_decisions') WHERE name='batch_key'",
            [],
            |row| row.get(0),
        )?;
        if has_plan_batch_key == 0 {
            conn.execute_batch("ALTER TABLE plan_decisions ADD COLUMN batch_key TEXT;")?;
        }
        let has_plan_batch_position: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('plan_decisions') WHERE name='batch_position'",
            [],
            |row| row.get(0),
        )?;
        if has_plan_batch_position == 0 {
            conn.execute_batch("ALTER TABLE plan_decisions ADD COLUMN batch_position INTEGER;")?;
        }
        let has_require_selection: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('plan_decisions') WHERE name='require_selection'",
            [],
            |row| row.get(0),
        )?;
        if has_require_selection == 0 {
            conn.execute_batch("ALTER TABLE plan_decisions ADD COLUMN require_selection INTEGER NOT NULL DEFAULT 1;")?;
        }
        let has_is_recommended: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('plan_options') WHERE name='is_recommended'",
            [],
            |row| row.get(0),
        )?;
        if has_is_recommended == 0 {
            conn.execute_batch(
                "ALTER TABLE plan_options ADD COLUMN is_recommended INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        let has_successor_refresh_hash: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('mobile_refresh_tokens') \
             WHERE name='successor_refresh_hash'",
            [],
            |row| row.get(0),
        )?;
        if has_successor_refresh_hash == 0 {
            conn.execute_batch(MIGRATE_V50_TO_V51)?;
        }
        if has_sessions > 0 {
            let has_goal: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='goal'",
                [],
                |row| row.get(0),
            )?;
            if has_goal == 0 {
                conn.execute_batch(MIGRATE_V51_TO_V52)?;
            }
        }
        let has_user_submit: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('session_activity') \
             WHERE name='last_user_submit_at_unix_ms'",
            [],
            |row| row.get(0),
        )?;
        if has_user_submit == 0 {
            conn.execute_batch(MIGRATE_V52_TO_V53)?;
        }
        let has_forward_slug: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('session_forwards') WHERE name='slug'",
            [],
            |row| row.get(0),
        )?;
        let has_session_forwards: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_forwards'",
            [],
            |row| row.get(0),
        )?;
        if has_session_forwards > 0 && has_forward_slug == 0 {
            conn.execute_batch(MIGRATE_V53_TO_V54)?;
        }
        let has_dir_share_id: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('session_forwards') WHERE name='dir_share_id'",
            [],
            |row| row.get(0),
        )?;
        if has_session_forwards > 0 && has_dir_share_id == 0 {
            conn.execute_batch(MIGRATE_V54_TO_V55)?;
        }
        // A database old enough to predate either table reaches this on its way
        // through the schema and gets the repair on a later open instead.
        let has_project_workers: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
             AND name IN ('projects', 'project_workers', 'bucket_workers')",
            [],
            |row| row.get(0),
        )?;
        if has_project_workers == 3 {
            let projects_without_a_worker: i64 = conn.query_row(
                "SELECT COUNT(*) FROM projects p \
                 WHERE NOT EXISTS (SELECT 1 FROM project_workers pw WHERE pw.project_id = p.id)",
                [],
                |row| row.get(0),
            )?;
            if projects_without_a_worker > 0 {
                conn.execute_batch(REPAIR_EMPTY_PROJECT_WORKERS)?;
            }
        }
        if has_session_search_sources == 3 {
            let search_has_goal: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_search') WHERE name='goal'",
                [],
                |row| row.get(0),
            )?;
            if search_has_goal == 0 {
                conn.execute_batch(SESSION_SEARCH_WITH_GOAL)?;
            }
        }
        // Renaming a table resolves every foreign key in the schema, so the
        // rebuild needs the tables these rows point at to be present. A
        // partially migrated database reaches this with neither, and gets the
        // rebuild on a later open once they exist.
        let has_users: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='users'",
            [],
            |row| row.get(0),
        )?;
        let access_token_device_required: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('mobile_access_tokens') \
             WHERE name='device_id' AND \"notnull\" = 1",
            [],
            |row| row.get(0),
        )?;
        if has_users > 0 && access_token_device_required > 0 {
            conn.execute_batch(MIGRATE_ACCESS_TOKENS_WITHOUT_A_DEVICE)?;
        }
        let socket_ticket_device_required: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('socket_tickets') \
             WHERE name='device_id' AND \"notnull\" = 1",
            [],
            |row| row.get(0),
        )?;
        if has_users > 0 && socket_ticket_device_required > 0 {
            conn.execute_batch(MIGRATE_SOCKET_TICKETS_WITHOUT_A_DEVICE)?;
        }
        conn.execute_batch(crate::connections::SCHEMA)?;
        crate::connections::migrate(&conn)?;
        conn.execute_batch(SUPERVISOR_SNOOZE_SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        conn.execute_batch(BACKFILL_SESSION_ACTIVITY)?;
        // Blank-keyed or blank-valued fields predate ingestion dropping them
        // and render as empty chips; sweeping on every open keeps the cleanup
        // version-independent and costs one scan of two small tables. The
        // guard covers pre-context-bag databases mid-migration in tests.
        let has_context_bags: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \n             AND name IN ('session_glance', 'session_context')",
            [],
            |row| row.get(0),
        )?;
        if has_context_bags == 2 {
            conn.execute_batch(
                "DELETE FROM session_glance WHERE trim(key) = '' OR trim(value) = ''; \n                 DELETE FROM session_context WHERE trim(key) = '' OR trim(value) = '';",
            )?;
        }
        Ok(Storage {
            conn: Mutex::new(conn),
        })
    }

    pub fn create_bucket(&self, name: &str) -> Result<Bucket> {
        self.create_bucket_with_workers(name, &[LOCAL_WORKER_ID], LOCAL_WORKER_ID, false)
    }

    pub fn create_bucket_with_workers(
        &self,
        name: &str,
        allowed_worker_ids: &[u64],
        default_worker_id: u64,
        is_default: bool,
    ) -> Result<Bucket> {
        if allowed_worker_ids.is_empty() || !allowed_worker_ids.contains(&default_worker_id) {
            return Err(StorageError::Conflict(
                "select at least one allowed worker and choose its default".into(),
            ));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let bucket_count: i64 = tx.query_row("SELECT COUNT(*) FROM buckets", [], |r| r.get(0))?;
        if is_default {
            tx.execute("UPDATE buckets SET is_default = 0", [])?;
        }
        tx.execute(
            "INSERT INTO buckets (name, position, default_worker_id, is_default) VALUES (?1, (SELECT COALESCE(MAX(position), -1) + 1 FROM buckets), ?2, ?3)",
            params![name, default_worker_id as i64, is_default || bucket_count == 0],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StorageError::Conflict(format!("bucket {name:?} already exists"))
            }
            e => e.into(),
        })?;
        let id = tx.last_insert_rowid() as u64;
        for worker_id in allowed_worker_ids {
            tx.execute(
                "INSERT INTO bucket_workers (bucket_id, worker_id) VALUES (?1, ?2)",
                params![id as i64, *worker_id as i64],
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.get_bucket(id)
    }

    pub fn get_bucket(&self, id: u64) -> Result<Bucket> {
        let conn = self.conn.lock().unwrap();
        let mut bucket = conn
            .query_row(
                "SELECT id, name, position, permission_mode, default_worker_id, is_default, default_agent, model_profile_id FROM buckets WHERE id = ?1",
                params![id as i64],
                row_to_bucket,
            )
            .optional()?
            .ok_or(StorageError::NotFound("bucket", id))?;
        bucket.allowed_worker_ids = worker_ids_for(&conn, "bucket_workers", "bucket_id", id)?;
        Ok(bucket)
    }

    pub fn delete_bucket(&self, id: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let projects: i64 = conn.query_row(
            "SELECT COUNT(*) FROM projects WHERE bucket_id = ?1",
            params![id as i64],
            |r| r.get(0),
        )?;
        if projects > 0 {
            return Err(StorageError::Conflict(format!(
                "bucket has {projects} project(s), delete them first"
            )));
        }
        let was_default: bool = conn
            .query_row(
                "SELECT is_default FROM buckets WHERE id = ?1",
                params![id as i64],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        let n = conn.execute("DELETE FROM buckets WHERE id = ?1", params![id as i64])?;
        if n == 0 {
            return Err(StorageError::NotFound("bucket", id));
        }
        if was_default {
            conn.execute(
                "UPDATE buckets SET is_default = 1 WHERE id = (SELECT id FROM buckets ORDER BY position, id LIMIT 1)",
                [],
            )?;
        }
        Ok(())
    }

    pub fn create_project(&self, bucket_id: u64, name: &str, path: &str) -> Result<Project> {
        let workers = self.get_bucket(bucket_id)?.allowed_worker_ids;
        self.create_project_with_workers(bucket_id, name, path, None, &workers)
    }

    pub fn create_project_with_worker(
        &self,
        bucket_id: u64,
        name: &str,
        path: &str,
        worker_id: Option<u64>,
    ) -> Result<Project> {
        let workers = self.get_bucket(bucket_id)?.allowed_worker_ids;
        self.create_project_with_workers(bucket_id, name, path, worker_id, &workers)
    }

    /// Names a worker the way an error a person reads should name it.
    /// Worker 0 has no row of its own, and an id can outlive its row
    /// when a host is removed mid-edit.
    fn worker_label(&self, id: u64) -> String {
        if id == LOCAL_WORKER_ID {
            return "the local Host".into();
        }
        match self.get_worker(id) {
            Ok(worker) => format!("Host {:?}", worker.name),
            Err(_) => format!("Host {id}"),
        }
    }

    /// Rejects a project worker allowlist that would break one of the
    /// hierarchy's rules, naming the host at fault so the caller can act
    /// on the answer instead of rediscovering which rule it broke.
    fn check_project_workers(
        &self,
        bucket: &Bucket,
        allowed_worker_ids: &[u64],
        worker_id: Option<u64>,
    ) -> Result<()> {
        if allowed_worker_ids.is_empty() {
            return Err(StorageError::Conflict(
                "a project needs at least one allowed Host".into(),
            ));
        }
        if let Some(id) = allowed_worker_ids
            .iter()
            .find(|id| !bucket.allowed_worker_ids.contains(id))
        {
            return Err(StorageError::Conflict(format!(
                "{} is not an allowed Host in bucket {:?}",
                self.worker_label(*id),
                bucket.name
            )));
        }
        if let Some(id) = worker_id.filter(|id| !allowed_worker_ids.contains(id)) {
            return Err(StorageError::Conflict(format!(
                "the project's default {} is not one of its allowed Hosts",
                self.worker_label(id)
            )));
        }
        if worker_id.is_none() && !allowed_worker_ids.contains(&bucket.default_worker_id) {
            return Err(StorageError::Conflict(format!(
                "this project inherits bucket {:?}'s default {}, so that Host must stay in its allowed Hosts or the project needs its own default",
                bucket.name,
                self.worker_label(bucket.default_worker_id)
            )));
        }
        Ok(())
    }

    pub fn create_project_with_workers(
        &self,
        bucket_id: u64,
        name: &str,
        path: &str,
        worker_id: Option<u64>,
        allowed_worker_ids: &[u64],
    ) -> Result<Project> {
        let bucket = self.get_bucket(bucket_id)?;
        self.check_project_workers(&bucket, allowed_worker_ids, worker_id)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO projects (bucket_id, name, path, worker_id) VALUES (?1, ?2, ?3, ?4)",
            params![bucket_id as i64, name, path, worker_id.map(|id| id as i64)],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StorageError::Conflict(format!("project {name:?} already exists in this bucket"))
            }
            e => e.into(),
        })?;
        let id = tx.last_insert_rowid() as u64;
        for worker_id in allowed_worker_ids {
            tx.execute(
                "INSERT INTO project_workers (project_id, worker_id) VALUES (?1, ?2)",
                params![id as i64, *worker_id as i64],
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.get_project(id)
    }

    pub fn set_bucket_permission_mode(&self, id: u64, mode: PermissionMode) -> Result<Bucket> {
        let stored = match mode {
            PermissionMode::Inherit => PermissionMode::Default,
            other => other,
        };
        let n = self.conn.lock().unwrap().execute(
            "UPDATE buckets SET permission_mode = ?2 WHERE id = ?1",
            params![id as i64, stored.as_str()],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("bucket", id));
        }
        self.get_bucket(id)
    }

    pub fn set_project_permission_mode(&self, id: u64, mode: PermissionMode) -> Result<Project> {
        self.update_project(id, None, Some(mode), None)
    }

    pub fn set_bucket_default_agent(&self, id: u64, agent: Option<AgentKind>) -> Result<Bucket> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE buckets SET default_agent = ?2 WHERE id = ?1",
            params![id as i64, agent.map(|agent| agent.as_str())],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("bucket", id));
        }
        self.get_bucket(id)
    }

    pub fn set_project_default_agent(&self, id: u64, agent: Option<AgentKind>) -> Result<Project> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE projects SET default_agent = ?2 WHERE id = ?1",
            params![id as i64, agent.map(|agent| agent.as_str())],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("project", id));
        }
        self.get_project(id)
    }

    pub fn set_bucket_model_profile(&self, id: u64, profile_id: Option<u64>) -> Result<Bucket> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE buckets SET model_profile_id = ?2 WHERE id = ?1",
            params![id as i64, profile_id.map(|v| v as i64)],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("bucket", id));
        }
        self.get_bucket(id)
    }

    pub fn set_project_model_profile(&self, id: u64, profile_id: Option<u64>) -> Result<Project> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE projects SET model_profile_id = ?2 WHERE id = ?1",
            params![id as i64, profile_id.map(|v| v as i64)],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("project", id));
        }
        self.get_project(id)
    }

    /// Creates a profile. The credential arrives already sealed; this
    /// layer never sees or returns a plaintext key.
    pub fn create_model_profile(
        &self,
        name: &str,
        api_key_ciphertext: Option<&str>,
        now: i64,
    ) -> Result<ModelProfile> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO model_profiles (name, api_key_ciphertext, created_at_unix_ms, updated_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?3)",
            params![name, api_key_ciphertext, now],
        )
        .map_err(conflict_on_constraint(|| {
            format!("a model profile named {name:?} already exists")
        }))?;
        let id = conn.last_insert_rowid() as u64;
        drop(conn);
        self.get_model_profile(id)
    }

    /// Renames and/or replaces the credential. `None` fields leave the
    /// stored value untouched, so editing a profile keeps its key.
    pub fn update_model_profile(
        &self,
        id: u64,
        name: Option<&str>,
        api_key_ciphertext: Option<Option<&str>>,
        now: i64,
    ) -> Result<ModelProfile> {
        let conn = self.conn.lock().unwrap();
        if let Some(name) = name {
            let n = conn
                .execute(
                    "UPDATE model_profiles SET name = ?2, updated_at_unix_ms = ?3 WHERE id = ?1",
                    params![id as i64, name, now],
                )
                .map_err(conflict_on_constraint(|| {
                    format!("a model profile named {name:?} already exists")
                }))?;
            if n == 0 {
                return Err(StorageError::NotFound("model profile", id));
            }
        }
        if let Some(ciphertext) = api_key_ciphertext {
            let n = conn.execute(
                "UPDATE model_profiles SET api_key_ciphertext = ?2, updated_at_unix_ms = ?3 \
                 WHERE id = ?1",
                params![id as i64, ciphertext, now],
            )?;
            if n == 0 {
                return Err(StorageError::NotFound("model profile", id));
            }
        }
        drop(conn);
        self.get_model_profile(id)
    }

    pub fn get_model_profile(&self, id: u64) -> Result<ModelProfile> {
        let conn = self.conn.lock().unwrap();
        let mut profile = conn
            .query_row(
                "SELECT id, name, api_key_ciphertext, created_at_unix_ms, updated_at_unix_ms \
                 FROM model_profiles WHERE id = ?1",
                params![id as i64],
                row_to_model_profile,
            )
            .optional()?
            .ok_or(StorageError::NotFound("model profile", id))?;
        profile.endpoints = model_profile_endpoints(&conn, id)?;
        Ok(profile)
    }

    pub fn list_model_profiles(&self) -> Result<Vec<ModelProfile>> {
        let conn = self.conn.lock().unwrap();
        let mut profiles = conn
            .prepare(
                "SELECT id, name, api_key_ciphertext, created_at_unix_ms, updated_at_unix_ms \
                 FROM model_profiles ORDER BY name",
            )?
            .query_map([], row_to_model_profile)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for profile in &mut profiles {
            profile.endpoints = model_profile_endpoints(&conn, profile.id)?;
        }
        Ok(profiles)
    }

    /// The stored ciphertext, or None when the profile has no key.
    pub fn model_profile_key_ciphertext(&self, id: u64) -> Result<Option<String>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT api_key_ciphertext FROM model_profiles WHERE id = ?1",
                params![id as i64],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or(StorageError::NotFound("model profile", id))
    }

    pub fn delete_model_profile(&self, id: u64) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "DELETE FROM model_profiles WHERE id = ?1",
            params![id as i64],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("model profile", id));
        }
        Ok(())
    }

    pub fn set_model_profile_endpoint(
        &self,
        profile_id: u64,
        dialect: ModelDialect,
        model: &str,
        base_url: &str,
        background_model: &str,
        now: i64,
    ) -> Result<ModelProfile> {
        self.get_model_profile(profile_id)?;
        {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO model_profile_endpoints \
                 (profile_id, dialect, model, base_url, background_model) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(profile_id, dialect) DO UPDATE SET \
                 model = excluded.model, base_url = excluded.base_url, \
                 background_model = excluded.background_model",
                params![
                    profile_id as i64,
                    dialect.as_str(),
                    model,
                    base_url,
                    background_model
                ],
            )?;
            conn.execute(
                "UPDATE model_profiles SET updated_at_unix_ms = ?2 WHERE id = ?1",
                params![profile_id as i64, now],
            )?;
        }
        self.get_model_profile(profile_id)
    }

    pub fn delete_model_profile_endpoint(
        &self,
        profile_id: u64,
        dialect: ModelDialect,
    ) -> Result<ModelProfile> {
        {
            let conn = self.conn.lock().unwrap();
            let n = conn.execute(
                "DELETE FROM model_profile_endpoints WHERE profile_id = ?1 AND dialect = ?2",
                params![profile_id as i64, dialect.as_str()],
            )?;
            if n == 0 {
                return Err(StorageError::NotFound("model profile endpoint", profile_id));
            }
        }
        self.get_model_profile(profile_id)
    }

    /// Buckets and projects that reference a profile, by name, for the
    /// message a rejected delete carries.
    pub fn model_profile_referents(&self, id: u64) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut referents = conn
            .prepare("SELECT name FROM buckets WHERE model_profile_id = ?1 ORDER BY name")?
            .query_map(params![id as i64], |row| {
                Ok(format!("bucket {:?}", row.get::<_, String>(0)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        referents.extend(
            conn.prepare("SELECT name FROM projects WHERE model_profile_id = ?1 ORDER BY name")?
                .query_map(params![id as i64], |row| {
                    Ok(format!("project {:?}", row.get::<_, String>(0)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        Ok(referents)
    }

    /// Ids of resumable sessions that spawned through this profile.
    /// Their resume reselects the entry, so the profile must survive.
    pub fn sessions_holding_model_profile(&self, id: u64) -> Result<Vec<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id FROM sessions WHERE model_profile_id = ?1 \
                 AND (agent_resumable != 0 OR state NOT IN ('exited', 'failed')) ORDER BY id",
            )?
            .query_map(params![id as i64], |row| Ok(row.get::<_, i64>(0)? as u64))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Resumable sessions that would reselect one specific entry: those
    /// on the profile whose agent runs that dialect.
    pub fn sessions_holding_model_profile_agents(&self, id: u64) -> Result<Vec<(u64, AgentKind)>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id, agent FROM sessions WHERE model_profile_id = ?1 \
                 AND (agent_resumable != 0 OR state NOT IN ('exited', 'failed')) ORDER BY id",
            )?
            .query_map(params![id as i64], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(id, agent)| AgentKind::parse(&agent).map(|agent| (id, agent)))
            .collect())
    }

    pub fn set_bucket_default_worker(&self, id: u64, worker_id: u64) -> Result<Bucket> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE buckets SET default_worker_id = ?2 WHERE id = ?1",
            params![id as i64, worker_id as i64],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("bucket", id));
        }
        self.get_bucket(id)
    }

    /// Applies a bucket's Host allowlist and repairs the projects under
    /// it, rather than refusing while any of them still references a Host
    /// being removed. A project pinned to a removed Host moves to
    /// `replacement_worker_id`, defaulting to the bucket's new default.
    /// Returns the bucket with every project the repair changed.
    pub fn set_bucket_workers(
        &self,
        id: u64,
        allowed_worker_ids: &[u64],
        default_worker_id: u64,
        replacement_worker_id: Option<u64>,
    ) -> Result<(Bucket, Vec<Project>)> {
        if allowed_worker_ids.is_empty() {
            return Err(StorageError::Conflict(
                "a bucket needs at least one allowed Host".into(),
            ));
        }
        if !allowed_worker_ids.contains(&default_worker_id) {
            return Err(StorageError::Conflict(format!(
                "the bucket's default {} is not one of its allowed Hosts",
                self.worker_label(default_worker_id)
            )));
        }
        if let Some(worker_id) =
            replacement_worker_id.filter(|worker_id| !allowed_worker_ids.contains(worker_id))
        {
            return Err(StorageError::Conflict(format!(
                "replacement {} is not one of the bucket's allowed Hosts",
                self.worker_label(worker_id)
            )));
        }
        let replacement = replacement_worker_id.unwrap_or(default_worker_id);
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE buckets SET default_worker_id=?2 WHERE id=?1",
            params![id as i64, default_worker_id as i64],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("bucket", id));
        }
        let removed: Vec<u64> = {
            let mut stmt = tx.prepare("SELECT worker_id FROM bucket_workers WHERE bucket_id=?1")?;
            let rows = stmt
                .query_map(params![id as i64], |r| Ok(r.get::<_, i64>(0)? as u64))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .filter(|worker_id| !allowed_worker_ids.contains(worker_id))
                .collect()
        };
        let mut changed = std::collections::BTreeSet::new();
        for worker_id in &removed {
            {
                let mut stmt = tx.prepare(
                    "SELECT p.id FROM projects p WHERE p.bucket_id=?1 AND (p.worker_id=?2 \
                     OR EXISTS (SELECT 1 FROM project_workers pw WHERE pw.project_id=p.id AND pw.worker_id=?2))",
                )?;
                for row in stmt.query_map(params![id as i64, *worker_id as i64], |r| {
                    Ok(r.get::<_, i64>(0)? as u64)
                })? {
                    changed.insert(row?);
                }
            }
            // Widen before narrowing so a project whose only Host is
            // being removed is never momentarily left with none.
            tx.execute(
                "INSERT OR IGNORE INTO project_workers(project_id,worker_id) \
                 SELECT pw.project_id, ?3 FROM project_workers pw JOIN projects p ON p.id=pw.project_id \
                 WHERE p.bucket_id=?1 AND pw.worker_id=?2",
                params![id as i64, *worker_id as i64, replacement as i64],
            )?;
            tx.execute(
                "UPDATE projects SET worker_id=?3 WHERE bucket_id=?1 AND worker_id=?2",
                params![id as i64, *worker_id as i64, replacement as i64],
            )?;
            // A launch path is kept when its Host leaves the bucket's allow
            // list, matching set_project_workers. Selection is gated on the
            // allow list, so a dormant path cannot make a disallowed Host
            // selectable, and a deleted Host still takes its paths with it
            // through the foreign key.
            tx.execute(
                "DELETE FROM project_workers WHERE worker_id=?2 AND project_id IN \
                 (SELECT id FROM projects WHERE bucket_id=?1)",
                params![id as i64, *worker_id as i64],
            )?;
        }
        {
            let mut stmt = tx.prepare(
                "SELECT p.id FROM projects p WHERE p.bucket_id=?1 AND p.worker_id IS NULL \
                 AND NOT EXISTS (SELECT 1 FROM project_workers pw WHERE pw.project_id=p.id AND pw.worker_id=?2)",
            )?;
            for row in stmt.query_map(params![id as i64, default_worker_id as i64], |r| {
                Ok(r.get::<_, i64>(0)? as u64)
            })? {
                changed.insert(row?);
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO project_workers(project_id,worker_id) \
             SELECT p.id, ?2 FROM projects p WHERE p.bucket_id=?1 AND p.worker_id IS NULL",
            params![id as i64, default_worker_id as i64],
        )?;
        tx.execute(
            "DELETE FROM bucket_workers WHERE bucket_id=?1",
            params![id as i64],
        )?;
        for worker_id in allowed_worker_ids {
            tx.execute(
                "INSERT INTO bucket_workers(bucket_id,worker_id) VALUES(?1,?2)",
                params![id as i64, *worker_id as i64],
            )?;
        }
        tx.commit()?;
        drop(conn);
        let bucket = self.get_bucket(id)?;
        let projects = changed
            .into_iter()
            .map(|project_id| self.get_project(project_id))
            .collect::<Result<Vec<_>>>()?;
        Ok((bucket, projects))
    }

    pub fn set_default_bucket(&self, id: u64) -> Result<Vec<Bucket>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM buckets WHERE id=?1)",
            params![id as i64],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(StorageError::NotFound("bucket", id));
        }
        tx.execute("UPDATE buckets SET is_default=0 WHERE is_default=1", [])?;
        tx.execute(
            "UPDATE buckets SET is_default=1 WHERE id=?1",
            params![id as i64],
        )?;
        tx.commit()?;
        drop(conn);
        let snapshot = self.snapshot(0)?;
        Ok(snapshot.buckets)
    }

    /// Sets a project's worker override; None clears it so the project
    /// inherits its bucket's default worker.
    pub fn set_project_worker(&self, id: u64, worker_id: Option<u64>) -> Result<Project> {
        self.update_project(id, None, None, Some(worker_id))
    }

    pub fn set_project_workers(
        &self,
        id: u64,
        allowed_worker_ids: &[u64],
        worker_id: Option<u64>,
    ) -> Result<Project> {
        let project = self.get_project(id)?;
        let bucket = self.get_bucket(project.bucket_id)?;
        self.check_project_workers(&bucket, allowed_worker_ids, worker_id)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE projects SET worker_id=?2 WHERE id=?1",
            params![id as i64, worker_id.map(|v| v as i64)],
        )?;
        tx.execute(
            "DELETE FROM project_workers WHERE project_id=?1",
            params![id as i64],
        )?;
        for worker_id in allowed_worker_ids {
            tx.execute(
                "INSERT INTO project_workers(project_id,worker_id) VALUES(?1,?2)",
                params![id as i64, *worker_id as i64],
            )?;
        }
        // A launch path is kept when its worker leaves the allow list.
        // Selection is gated on the allow list, so a dormant path can
        // never make a disallowed host selectable, and a worker that is
        // genuinely deleted takes its paths with it through the foreign
        // key. Discarding them here made saving a project's hosts throw
        // away directories the operator had configured by hand.
        tx.commit()?;
        drop(conn);
        self.get_project(id)
    }

    pub fn update_project(
        &self,
        id: u64,
        path: Option<&str>,
        permission_mode: Option<PermissionMode>,
        worker_id: Option<Option<u64>>,
    ) -> Result<Project> {
        let permission_mode = permission_mode.map(|mode| mode.as_str());
        let (set_worker, worker_id) = match worker_id {
            Some(worker_id) => (true, worker_id),
            None => (false, None),
        };
        let n = self.conn.lock().unwrap().execute(
            "UPDATE projects SET \
             path = COALESCE(?2, path), \
             permission_mode = COALESCE(?3, permission_mode), \
             worker_id = CASE WHEN ?4 THEN ?5 ELSE worker_id END \
             WHERE id = ?1",
            params![
                id as i64,
                path,
                permission_mode,
                set_worker,
                worker_id.map(|worker_id| worker_id as i64)
            ],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("project", id));
        }
        self.get_project(id)
    }

    pub fn get_project(&self, id: u64) -> Result<Project> {
        let conn = self.conn.lock().unwrap();
        let mut project = conn
            .query_row(
                "SELECT id, bucket_id, name, path, permission_mode, worker_id, default_agent, model_profile_id FROM projects WHERE id = ?1",
                params![id as i64],
                row_to_project,
            )
            .optional()?
            .ok_or(StorageError::NotFound("project", id))?;
        project.allowed_worker_ids = worker_ids_for(&conn, "project_workers", "project_id", id)?;
        project.worker_paths = project_paths_for(&conn, id)?;
        Ok(project)
    }

    pub fn delete_project(&self, id: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let live: i64 = conn.query_row(
            "SELECT COUNT(*) FROM terminals t JOIN sessions s ON s.id = t.session_id \
             JOIN terminal_runs r ON r.terminal_id = t.id \
             WHERE s.project_id = ?1 AND r.generation = (SELECT MAX(r2.generation) FROM terminal_runs r2 WHERE r2.terminal_id = t.id) \
             AND r.state IN ('starting', 'running')",
            params![id as i64],
            |r| r.get(0),
        )?;
        if live > 0 {
            return Err(StorageError::Conflict(format!(
                "project has {live} live session(s)"
            )));
        }
        let n = conn.execute("DELETE FROM projects WHERE id = ?1", params![id as i64])?;
        if n == 0 {
            return Err(StorageError::NotFound("project", id));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_session(
        &self,
        project_id: u64,
        agent: AgentKind,
        task_title: &str,
        task_prompt: &str,
        permission_mode: PermissionMode,
        worker_id: u64,
        items_api: bool,
        supervisor_api: bool,
        spawned_by_session_id: Option<u64>,
        created_at_unix_ms: i64,
    ) -> Result<Session> {
        self.create_session_with_agent_source(
            project_id,
            agent,
            AgentSelectionSource::Explicit,
            task_title,
            task_prompt,
            permission_mode,
            worker_id,
            items_api,
            supervisor_api,
            spawned_by_session_id,
            created_at_unix_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_agent_source(
        &self,
        project_id: u64,
        agent: AgentKind,
        agent_source: AgentSelectionSource,
        task_title: &str,
        task_prompt: &str,
        permission_mode: PermissionMode,
        worker_id: u64,
        items_api: bool,
        supervisor_api: bool,
        spawned_by_session_id: Option<u64>,
        created_at_unix_ms: i64,
    ) -> Result<Session> {
        self.create_session_with_model_profile(
            project_id,
            agent,
            agent_source,
            task_title,
            task_prompt,
            permission_mode,
            worker_id,
            items_api,
            supervisor_api,
            spawned_by_session_id,
            None,
            created_at_unix_ms,
        )
    }

    /// The session records the profile id, not a copy of its values, so
    /// a resume reselects the entry and picks up edits made since.
    #[allow(clippy::too_many_arguments)]
    pub fn create_session_with_model_profile(
        &self,
        project_id: u64,
        agent: AgentKind,
        agent_source: AgentSelectionSource,
        task_title: &str,
        task_prompt: &str,
        permission_mode: PermissionMode,
        worker_id: u64,
        items_api: bool,
        supervisor_api: bool,
        spawned_by_session_id: Option<u64>,
        model_profile: Option<(u64, ModelProfileSource)>,
        created_at_unix_ms: i64,
    ) -> Result<Session> {
        let project = self.get_project(project_id)?;
        let role = if supervisor_api {
            SessionRole::Supervisor
        } else {
            SessionRole::Worker
        };
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO sessions (project_id, agent, agent_source, state, task_title, task_prompt, permission_mode, worker_id, cwd, items_api, supervisor_api, role, spawned_by_session_id, created_at_unix_ms, model_profile_id, model_profile_source, goal, desired_running)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, 1)",
            params![
                project_id as i64,
                agent.as_str(),
                agent_source.as_str(),
                SessionState::Starting.as_str(),
                task_title,
                task_prompt,
                permission_mode.as_str(),
                worker_id as i64,
                project.path,
                items_api || role == SessionRole::Supervisor,
                supervisor_api || role == SessionRole::Supervisor,
                role.as_str(),
                spawned_by_session_id.map(|v| v as i64),
                created_at_unix_ms,
                model_profile.map(|(id, _)| id as i64),
                model_profile.map(|(_, source)| source.as_str()),
                seed_goal(task_title, task_prompt)
            ],
        )?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute(
            "INSERT INTO terminals (session_id, kind, title, cwd, created_at_unix_ms, desired_running) VALUES (?1, 'agent', 'Agent', ?2, ?3, 1)",
            params![id as i64, project.path, created_at_unix_ms],
        )?;
        let terminal_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO terminal_runs (terminal_id, generation, state, started_at_unix_ms) VALUES (?1, 1, 'starting', ?2)",
            params![terminal_id, created_at_unix_ms],
        )?;
        tx.commit()?;
        drop(conn);
        self.get_session(id)
    }

    pub fn set_session_cwd(&self, id: u64, cwd: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE sessions SET cwd = ?2 WHERE id = ?1",
            params![id as i64, cwd],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        conn.execute(
            "UPDATE terminals SET cwd = ?2 WHERE session_id = ?1 AND kind = 'agent'",
            params![id as i64, cwd],
        )?;
        Ok(())
    }

    pub fn agent_terminal(&self, session_id: u64) -> Result<Terminal> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{TERMINAL_SELECT} WHERE t.session_id = ?1 AND t.kind = 'agent'"),
                params![session_id as i64],
                row_to_terminal,
            )
            .optional()?
            .ok_or(StorageError::NotFound(
                "agent terminal for session",
                session_id,
            ))
    }

    pub fn get_terminal(&self, terminal_id: u64) -> Result<Terminal> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{TERMINAL_SELECT} WHERE t.id = ?1"),
                params![terminal_id as i64],
                row_to_terminal,
            )
            .optional()?
            .ok_or(StorageError::NotFound("terminal", terminal_id))
    }

    pub fn terminals_for_session(&self, session_id: u64) -> Result<Vec<Terminal>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(&format!(
                "{TERMINAL_SELECT} WHERE t.session_id = ?1 ORDER BY t.id"
            ))?
            .query_map(params![session_id as i64], row_to_terminal)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn create_shell(&self, session_id: u64, title: &str, now: i64) -> Result<Terminal> {
        let session = self.get_session(session_id)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("INSERT INTO terminals (session_id, kind, title, cwd, created_at_unix_ms, desired_running) VALUES (?1, 'shell', ?2, ?3, ?4, 1)", params![session_id as i64, title, session.cwd, now])?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute("INSERT INTO terminal_runs (terminal_id, generation, state, started_at_unix_ms) VALUES (?1, 1, 'starting', ?2)", params![id as i64, now])?;
        tx.commit()?;
        drop(conn);
        self.get_terminal(id)
    }

    pub fn restart_terminal(&self, terminal_id: u64, now: i64) -> Result<Terminal> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let generation: i64 = tx.query_row(
            "SELECT COALESCE(MAX(generation), 0) + 1 FROM terminal_runs WHERE terminal_id = ?1",
            params![terminal_id as i64],
            |r| r.get(0),
        )?;
        if generation == 1 {
            return Err(StorageError::NotFound("terminal", terminal_id));
        }
        tx.execute(
            "UPDATE terminal_runs SET state = 'exited', ended_at_unix_ms = ?2 \
             WHERE terminal_id = ?1 AND generation = ?3 AND state IN ('starting', 'running')",
            params![terminal_id as i64, now, generation - 1],
        )?;
        tx.execute("INSERT INTO terminal_runs (terminal_id, generation, state, started_at_unix_ms) VALUES (?1, ?2, 'starting', ?3)", params![terminal_id as i64, generation, now])?;
        tx.execute(
            "UPDATE terminals SET desired_running = 1 WHERE id = ?1",
            params![terminal_id as i64],
        )?;
        tx.commit()?;
        drop(conn);
        self.get_terminal(terminal_id)
    }

    pub fn update_terminal_run(
        &self,
        terminal_id: u64,
        generation: u64,
        state: TerminalRunState,
        exit_code: Option<i32>,
        scrollback_available: bool,
        now: i64,
    ) -> Result<Terminal> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE terminal_runs SET state = ?3, ended_at_unix_ms = CASE WHEN ?3 IN ('exited','failed') THEN ?4 ELSE NULL END, exit_code = ?5, scrollback_available = ?6 WHERE terminal_id = ?1 AND generation = ?2",
            params![terminal_id as i64, generation as i64, state.as_str(), now, exit_code, scrollback_available],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("terminal run", terminal_id));
        }
        self.get_terminal(terminal_id)
    }

    pub fn set_terminal_scrollback_available(
        &self,
        terminal_id: u64,
        generation: u64,
    ) -> Result<Terminal> {
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE terminal_runs SET scrollback_available = 1 WHERE terminal_id = ?1 AND generation = ?2",
            params![terminal_id as i64, generation as i64],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound("terminal run", terminal_id));
        }
        self.get_terminal(terminal_id)
    }

    pub fn delete_terminal(&self, terminal_id: u64) -> Result<()> {
        let terminal = self.get_terminal(terminal_id)?;
        if terminal.kind == TerminalKind::Agent {
            return Err(StorageError::Conflict(
                "the agent terminal cannot be closed".into(),
            ));
        }
        self.conn.lock().unwrap().execute(
            "DELETE FROM terminals WHERE id = ?1",
            params![terminal_id as i64],
        )?;
        Ok(())
    }

    pub fn set_terminal_desired_running(&self, terminal_id: u64, desired: bool) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE terminals SET desired_running = ?2 WHERE id = ?1",
            params![terminal_id as i64, desired],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("terminal", terminal_id));
        }
        Ok(())
    }

    pub fn terminal_desired_running(&self, terminal_id: u64) -> Result<bool> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT desired_running FROM terminals WHERE id = ?1",
                params![terminal_id as i64],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StorageError::NotFound("terminal", terminal_id))
    }

    pub fn desired_terminals_on_worker(&self, worker_id: u64) -> Result<Vec<Terminal>> {
        let ids: Vec<u64> = self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT t.id FROM terminals t JOIN sessions s ON s.id = t.session_id \
                 WHERE t.desired_running = 1 AND s.worker_id = ?1 ORDER BY t.id",
            )?
            .query_map(params![worker_id as i64], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|id| id as u64)
            .collect();
        ids.into_iter().map(|id| self.get_terminal(id)).collect()
    }

    /// Agent terminals of live sessions on a worker that nothing wants
    /// running, such as a session killed while its worker was away.
    pub fn undesired_live_agent_terminals_on_worker(
        &self,
        worker_id: u64,
    ) -> Result<Vec<Terminal>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{TERMINAL_SELECT} JOIN sessions s ON s.id = t.session_id \
             WHERE t.kind = 'agent' AND t.desired_running = 0 AND s.worker_id = ?1 \
             AND s.state NOT IN ('exited', 'failed') ORDER BY t.id"
        ))?;
        let rows = stmt
            .query_map(params![worker_id as i64], row_to_terminal)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_workers(&self) -> Result<Vec<Worker>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(&format!("SELECT {WORKER_COLUMNS} FROM workers ORDER BY id"))?
            .query_map([], row_to_worker)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_worker(&self, id: u64) -> Result<Worker> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("SELECT {WORKER_COLUMNS} FROM workers WHERE id = ?1"),
                params![id as i64],
                row_to_worker,
            )
            .optional()?
            .ok_or(StorageError::NotFound("worker", id))
    }

    /// Records a pending worker enrollment as a one-time token hash. A
    /// `worker_id` re-enrolls that existing host rather than adding one.
    #[allow(clippy::too_many_arguments)]
    pub fn create_worker_enrollment(
        &self,
        token_hash: &str,
        token_ciphertext: &str,
        label: &str,
        worker_id: Option<u64>,
        connect_mode: ConnectMode,
        endpoint: &str,
        created_at_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        if let Some(worker_id) = worker_id {
            // A replacement token is the only one that should still be able
            // to rotate this worker, and the accept-mode dialer must not keep
            // retrying an older enrollment for the same endpoint.
            tx.execute(
                "UPDATE worker_enrollments SET used_at_unix_ms = ?2 \
                 WHERE worker_id = ?1 AND used_at_unix_ms IS NULL",
                params![worker_id as i64, created_at_unix_ms],
            )?;
        }
        tx.execute(
            "INSERT INTO worker_enrollments (token_hash, token_ciphertext, label, worker_id, \
             connect_mode, endpoint, created_at_unix_ms, expires_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                token_hash,
                token_ciphertext,
                label,
                worker_id.map(|id| id as i64),
                connect_mode.as_str(),
                endpoint,
                created_at_unix_ms,
                expires_at_unix_ms
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically creates the offline worker shown while its first
    /// enrollment is pending, and binds that enrollment to the new id.
    #[allow(clippy::too_many_arguments)]
    pub fn create_pending_worker_enrollment(
        &self,
        token_hash: &str,
        token_ciphertext: &str,
        name: &str,
        label: &str,
        connect_mode: ConnectMode,
        endpoint: &str,
        created_at_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<Worker> {
        self.create_pending_worker_enrollment_with_buckets(
            token_hash,
            token_ciphertext,
            name,
            label,
            connect_mode,
            endpoint,
            created_at_unix_ms,
            expires_at_unix_ms,
            &[],
        )
        .map(|(worker, _, _)| worker)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_pending_worker_enrollment_with_buckets(
        &self,
        token_hash: &str,
        token_ciphertext: &str,
        name: &str,
        label: &str,
        connect_mode: ConnectMode,
        endpoint: &str,
        created_at_unix_ms: i64,
        expires_at_unix_ms: i64,
        bucket_ids: &[u64],
    ) -> Result<(Worker, Vec<Bucket>, Vec<Project>)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO workers (name, connect_mode, endpoint, created_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4)",
            params![name, connect_mode.as_str(), endpoint, created_at_unix_ms],
        )?;
        let worker_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO worker_enrollments (token_hash, token_ciphertext, label, worker_id, \
             connect_mode, endpoint, created_at_unix_ms, expires_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                token_hash,
                token_ciphertext,
                label,
                worker_id,
                connect_mode.as_str(),
                endpoint,
                created_at_unix_ms,
                expires_at_unix_ms
            ],
        )?;
        for bucket_id in bucket_ids {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM buckets WHERE id=?1)",
                params![*bucket_id as i64],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StorageError::NotFound("bucket", *bucket_id));
            }
            tx.execute(
                "INSERT OR IGNORE INTO bucket_workers(bucket_id,worker_id) VALUES(?1,?2)",
                params![*bucket_id as i64, worker_id],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO project_workers(project_id,worker_id) \
                 SELECT id, ?2 FROM projects WHERE bucket_id=?1",
                params![*bucket_id as i64, worker_id],
            )?;
        }
        let project_ids = {
            let mut stmt =
                tx.prepare("SELECT project_id FROM project_workers WHERE worker_id=?1")?;
            let rows = stmt.query_map(params![worker_id], |row| row.get::<_, i64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.commit()?;
        drop(conn);
        let buckets = bucket_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|id| self.get_bucket(id))
            .collect::<Result<Vec<_>>>()?;
        let projects = project_ids
            .into_iter()
            .map(|id| self.get_project(id as u64))
            .collect::<Result<Vec<_>>>()?;
        Ok((self.get_worker(worker_id as u64)?, buckets, projects))
    }

    /// What the controller should be dialing: hosts already enrolled in
    /// accept mode, plus enrollments that will become one. A pending
    /// enrollment is included because the controller has to reach the host
    /// before it can register at all, and it carries the sealed token
    /// because the host will not answer until the controller proves it.
    pub fn accept_mode_dial_targets(&self, now_unix_ms: i64) -> Result<Vec<DialTarget>> {
        let conn = self.conn.lock().unwrap();
        // A live enrollment outranks the host's old pinned key. That is what
        // lets an operator re-enroll an accept-mode host whose key changed.
        // Newest first also makes a freshly rotated token supersede an older
        // one that has not expired yet.
        let mut targets: Vec<DialTarget> = conn
            .prepare(
                "SELECT endpoint, token_ciphertext FROM worker_enrollments \
                 WHERE connect_mode = 'accept' AND endpoint <> '' \
                 AND used_at_unix_ms IS NULL AND expires_at_unix_ms >= ?1 \
                 ORDER BY created_at_unix_ms DESC",
            )?
            .query_map(params![now_unix_ms], |row| {
                Ok(DialTarget {
                    endpoint: row.get(0)?,
                    peer_key_hash: None,
                    token_ciphertext: Some(row.get(1)?),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut seen: HashSet<String> = HashSet::new();
        targets.retain(|target| seen.insert(target.endpoint.clone()));
        let enrolled = conn
            .prepare(
                "SELECT endpoint, peer_key_hash FROM workers \
                 WHERE connect_mode = 'accept' AND endpoint <> '' \
                 AND peer_key_hash IS NOT NULL",
            )?
            .query_map([], |row| {
                Ok(DialTarget {
                    endpoint: row.get(0)?,
                    peer_key_hash: row.get(1)?,
                    token_ciphertext: None,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        targets.extend(
            enrolled
                .into_iter()
                .filter(|target| seen.insert(target.endpoint.clone())),
        );
        Ok(targets)
    }

    /// Enrollments that can still be completed. The controller has to prove
    /// it holds the token before the host will trust it, which means opening
    /// each live token and testing it against the proof the host presented.
    pub fn live_worker_enrollments(&self, now_unix_ms: i64) -> Result<Vec<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT token_ciphertext FROM worker_enrollments \
                 WHERE used_at_unix_ms IS NULL AND expires_at_unix_ms >= ?1 \
                 AND token_ciphertext <> '' ORDER BY created_at_unix_ms",
            )?
            .query_map(params![now_unix_ms], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Consumes a pending enrollment. Fails if the token is unknown, already
    /// used, or expired, and burns it on success so it cannot enroll a second
    /// worker.
    pub fn consume_worker_enrollment(
        &self,
        token_hash: &str,
        now_unix_ms: i64,
    ) -> Result<ConsumedEnrollment> {
        let conn = self.conn.lock().unwrap();
        struct PendingEnrollment {
            label: String,
            worker_id: Option<i64>,
            connect_mode: String,
            endpoint: String,
            used_at_unix_ms: Option<i64>,
            expires_at_unix_ms: i64,
        }
        let row = conn
            .query_row(
                "SELECT label, worker_id, connect_mode, endpoint, used_at_unix_ms, \
                 expires_at_unix_ms FROM worker_enrollments WHERE token_hash = ?1",
                params![token_hash],
                |r| {
                    Ok(PendingEnrollment {
                        label: r.get(0)?,
                        worker_id: r.get(1)?,
                        connect_mode: r.get(2)?,
                        endpoint: r.get(3)?,
                        used_at_unix_ms: r.get(4)?,
                        expires_at_unix_ms: r.get(5)?,
                    })
                },
            )
            .optional()?;
        let PendingEnrollment {
            label,
            worker_id,
            connect_mode,
            endpoint,
            used_at_unix_ms: used,
            expires_at_unix_ms: expires,
        } = row.ok_or_else(|| StorageError::Conflict("unknown enrollment token".into()))?;
        if used.is_some() {
            return Err(StorageError::Conflict(
                "enrollment token already used".into(),
            ));
        }
        if now_unix_ms > expires {
            return Err(StorageError::Conflict("enrollment token expired".into()));
        }
        conn.execute(
            "UPDATE worker_enrollments SET used_at_unix_ms = ?2 WHERE token_hash = ?1",
            params![token_hash, now_unix_ms],
        )?;
        Ok(ConsumedEnrollment {
            label,
            worker_id: worker_id.map(|id| id as u64),
            connect_mode: ConnectMode::parse(&connect_mode).unwrap_or_default(),
            endpoint,
        })
    }

    /// Creates a worker row for a freshly enrolled machine and returns it.
    #[allow(clippy::too_many_arguments)]
    pub fn register_worker(
        &self,
        name: &str,
        hostname: &str,
        platform: &str,
        pm_version: &str,
        default_project_root: &str,
        credential_hash: &str,
        peer_key_hash: &str,
        created_at_unix_ms: i64,
    ) -> Result<Worker> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workers (name, hostname, platform, pm_version, default_project_root, \
             credential_hash, peer_key_hash, created_at_unix_ms, last_seen_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                name,
                hostname,
                platform,
                pm_version,
                default_project_root,
                credential_hash,
                peer_key_hash,
                created_at_unix_ms
            ],
        )?;
        let id = conn.last_insert_rowid() as u64;
        drop(conn);
        self.get_worker(id)
    }

    /// How a host connects. Written at enrollment, since that is when the
    /// operator states it, and read by the dialer.
    pub fn set_worker_connection(
        &self,
        id: u64,
        connect_mode: ConnectMode,
        endpoint: &str,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE workers SET connect_mode = ?2, endpoint = ?3 WHERE id = ?1",
            params![id as i64, connect_mode.as_str(), endpoint],
        )?;
        Ok(())
    }

    /// Re-enrolls an existing host: rotates its credential and pinned key and
    /// refreshes what it reported, leaving its id and therefore its bucket
    /// defaults, project overrides, and session history attached.
    #[allow(clippy::too_many_arguments)]
    pub fn rebind_worker(
        &self,
        id: u64,
        name: &str,
        hostname: &str,
        platform: &str,
        pm_version: &str,
        default_project_root: &str,
        credential_hash: &str,
        peer_key_hash: &str,
        now_unix_ms: i64,
    ) -> Result<Worker> {
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE workers SET name = ?2, hostname = ?3, platform = ?4, pm_version = ?5, \
             default_project_root = ?6, credential_hash = ?7, peer_key_hash = ?8, \
             last_seen_at_unix_ms = ?9 WHERE id = ?1",
            params![
                id as i64,
                name,
                hostname,
                platform,
                pm_version,
                default_project_root,
                credential_hash,
                peer_key_hash,
                now_unix_ms
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound("worker", id));
        }
        self.get_worker(id)
    }

    /// Resolves a reconnecting worker by the public key the handshake proved
    /// it holds.
    pub fn worker_by_key_hash(&self, peer_key_hash: &str) -> Result<Option<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM workers WHERE peer_key_hash = ?1",
                params![peer_key_hash],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|id| id as u64))
    }

    /// Whether a host row has a key pinned to it yet.
    ///
    /// A host added through the UI exists before any machine has claimed it, so
    /// its first registration is a machine arriving rather than one being
    /// replaced. Telling those apart is what keeps the replacement notice
    /// meaningful instead of firing on every ordinary join.
    pub fn worker_has_pinned_key(&self, id: u64) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT peer_key_hash FROM workers WHERE id = ?1",
                params![id as i64],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .is_some_and(|hash| !hash.is_empty()))
    }

    pub fn get_worker_credential_hash(&self, id: u64) -> Result<Option<String>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT credential_hash FROM workers WHERE id = ?1",
                params![id as i64],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or(StorageError::NotFound("worker", id))
    }

    /// All sessions assigned to a worker, for reconciling which survived
    /// a reconnect.
    pub fn sessions_in_bucket(&self, bucket_id: u64) -> Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{SESSION_SELECT} WHERE project_id IN \
             (SELECT id FROM projects WHERE bucket_id = ?1) ORDER BY id DESC"
        ))?;
        let sessions = stmt
            .query_map(params![bucket_id as i64], row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        Ok(sessions)
    }

    pub fn sessions_on_worker(&self, worker_id: u64) -> Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("{SESSION_SELECT} WHERE worker_id = ?1"))?;
        let rows = stmt
            .query_map(params![worker_id as i64], row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    /// Live session ids running on a worker, used to fail them when the
    /// worker disconnects.
    pub fn live_sessions_on_worker(&self, worker_id: u64) -> Result<Vec<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id FROM sessions WHERE worker_id = ?1 \
                 AND state NOT IN ('exited', 'failed')",
            )?
            .query_map(params![worker_id as i64], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|id| id as u64)
            .collect())
    }

    /// A project's path on a specific worker, if one has been recorded.
    pub fn project_path_for_worker(
        &self,
        project_id: u64,
        worker_id: u64,
    ) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT path FROM project_paths WHERE project_id = ?1 AND worker_id = ?2",
                params![project_id as i64, worker_id as i64],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Sets or clears a project's launch path on one worker and returns
    /// the updated project. A cleared worker falls back to the project's
    /// configured path.
    pub fn set_project_worker_path(
        &self,
        project_id: u64,
        worker_id: u64,
        path: Option<&str>,
    ) -> Result<Project> {
        {
            let conn = self.conn.lock().unwrap();
            match path {
                Some(path) => {
                    conn.execute(
                        "INSERT INTO project_paths (project_id, worker_id, path) VALUES (?1, ?2, ?3) \
                         ON CONFLICT(project_id, worker_id) DO UPDATE SET path = excluded.path",
                        params![project_id as i64, worker_id as i64, path],
                    )?;
                }
                None => {
                    conn.execute(
                        "DELETE FROM project_paths WHERE project_id = ?1 AND worker_id = ?2",
                        params![project_id as i64, worker_id as i64],
                    )?;
                }
            }
        }
        self.get_project(project_id)
    }

    pub fn touch_worker(&self, id: u64, now_unix_ms: i64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE workers SET last_seen_at_unix_ms = ?2 WHERE id = ?1",
            params![id as i64, now_unix_ms],
        )?;
        Ok(())
    }

    /// Records the pm build a worker reported at registration. Set on
    /// every reconnect, including back to empty, so the stored build
    /// always reflects the binary currently connected.
    /// Records what a worker reported about itself at registration. All
    /// three move together: a host that was rebuilt, moved into a
    /// container, or had its container renamed reports a different set,
    /// and a build that reports none must read as none again.
    pub fn set_worker_report(
        &self,
        id: u64,
        pm_version: &str,
        runtime: &str,
        container: &str,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE workers SET pm_version = ?2, runtime = ?3, container = ?4 WHERE id = ?1",
            params![id as i64, pm_version, runtime, container],
        )?;
        Ok(())
    }

    pub fn delete_worker(&self, id: u64, now_unix_ms: i64) -> Result<()> {
        if id == LOCAL_WORKER_ID {
            return Err(StorageError::Conflict(
                "the local worker cannot be removed".into(),
            ));
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE terminals SET desired_running = 0 WHERE session_id IN \
             (SELECT id FROM sessions WHERE worker_id = ?1)",
            params![id as i64],
        )?;
        tx.execute(
            "UPDATE terminal_runs SET state = 'failed', ended_at_unix_ms = ?2 \
             WHERE terminal_id IN (SELECT t.id FROM terminals t JOIN sessions s ON s.id = t.session_id WHERE s.worker_id = ?1) \
             AND generation = (SELECT MAX(r2.generation) FROM terminal_runs r2 WHERE r2.terminal_id = terminal_runs.terminal_id) \
             AND state IN ('starting', 'running')",
            params![id as i64, now_unix_ms],
        )?;
        tx.execute(
            "UPDATE sessions SET desired_running = 0, state = 'failed', state_detail = 'worker removed', \
             ended_at_unix_ms = ?2 WHERE worker_id = ?1 AND desired_running = 1",
            params![id as i64, now_unix_ms],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO bucket_workers(bucket_id,worker_id) SELECT bucket_id,0 FROM bucket_workers WHERE worker_id=?1",
            params![id as i64],
        )?;
        tx.execute(
            "UPDATE buckets SET default_worker_id=0 WHERE default_worker_id=?1",
            params![id as i64],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO project_workers(project_id,worker_id) SELECT pw.project_id,b.default_worker_id FROM project_workers pw JOIN projects p ON p.id=pw.project_id JOIN buckets b ON b.id=p.bucket_id WHERE pw.worker_id=?1",
            params![id as i64],
        )?;
        tx.execute(
            "UPDATE projects SET worker_id=NULL WHERE worker_id=?1",
            params![id as i64],
        )?;
        tx.execute(
            "DELETE FROM project_workers WHERE worker_id=?1",
            params![id as i64],
        )?;
        tx.execute(
            "DELETE FROM project_paths WHERE worker_id=?1",
            params![id as i64],
        )?;
        tx.execute(
            "DELETE FROM bucket_workers WHERE worker_id=?1",
            params![id as i64],
        )?;
        tx.execute(
            "DELETE FROM worker_enrollments WHERE worker_id=?1",
            params![id as i64],
        )?;
        let n = tx.execute("DELETE FROM workers WHERE id = ?1", params![id as i64])?;
        if n == 0 {
            return Err(StorageError::NotFound("worker", id));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_session(&self, id: u64) -> Result<Session> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{SESSION_SELECT} WHERE id = ?1"),
                params![id as i64],
                row_to_session,
            )
            .optional()?
            .ok_or(StorageError::NotFound("session", id))?
    }

    pub fn list_ended_sessions(&self, cursor: &str, limit: u32) -> Result<SessionPage> {
        let limit = limit.clamp(1, SESSION_PAGE_LIMIT_MAX);
        let (cursor_ended, cursor_id) = parse_ended_cursor(cursor)?;
        let conn = self.conn.lock().unwrap();
        let total = conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE state IN ('exited','failed')",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let sql = format!(
            "{SESSION_SELECT} WHERE state IN ('exited','failed') AND \
             (?1 IS NULL OR ended_at_unix_ms < ?1 OR (ended_at_unix_ms = ?1 AND id < ?2)) \
             ORDER BY ended_at_unix_ms DESC, id DESC LIMIT ?3"
        );
        let rows = conn
            .prepare(&sql)?
            .query_map(
                params![
                    cursor_ended,
                    cursor_id.map(|id| id as i64),
                    limit as i64 + 1
                ],
                row_to_session,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut sessions = rows.into_iter().collect::<Result<Vec<_>>>()?;
        let has_more = sessions.len() > limit as usize;
        sessions.truncate(limit as usize);
        let next_cursor = if has_more {
            sessions
                .last()
                .and_then(|s| s.ended_at_unix_ms.map(|ended| format!("{ended}:{}", s.id)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        Ok(SessionPage {
            sessions,
            next_cursor,
            total,
        })
    }

    pub fn search_sessions(&self, query: &str, cursor: &str, limit: u32) -> Result<SessionPage> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(SessionPage {
                sessions: Vec::new(),
                next_cursor: String::new(),
                total: 0,
            });
        }
        let limit = limit.clamp(1, SESSION_PAGE_LIMIT_MAX);
        let (cursor_rank, cursor_activity, cursor_id) = parse_search_cursor(cursor)?;
        let exact_id = query.parse::<u64>().ok();
        let fts_query = fts_query(query);
        let conn = self.conn.lock().unwrap();
        let candidate = "(sessions.id = ?1 OR (?2 <> '' AND sessions.id IN \
            (SELECT rowid FROM session_search WHERE session_search MATCH ?2)))";
        let total = conn.query_row(
            &format!("SELECT COUNT(*) FROM sessions WHERE {candidate}"),
            params![exact_id.map(|id| id as i64), fts_query],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let rank = "CASE WHEN sessions.id = ?1 THEN 0 \
            WHEN lower(task_title)=lower(?3) OR lower(goal)=lower(?3) OR lower(headline)=lower(?3) THEN 1 \
            WHEN lower(task_title) LIKE lower(?3)||'%' OR lower(goal) LIKE lower(?3)||'%' \
                OR lower(headline) LIKE lower(?3)||'%' THEN 2 \
            ELSE 3 END";
        let sql = format!(
            "{SESSION_SELECT} WHERE {candidate} AND \
             (?4 IS NULL OR ({rank}) > ?4 OR (({rank}) = ?4 AND \
               (last_activity_at_unix_ms < ?5 OR \
                (last_activity_at_unix_ms = ?5 AND sessions.id < ?6)))) \
             ORDER BY ({rank}), last_activity_at_unix_ms DESC, sessions.id DESC LIMIT ?7"
        );
        let rows = conn
            .prepare(&sql)?
            .query_map(
                params![
                    exact_id.map(|id| id as i64),
                    fts_query,
                    query,
                    cursor_rank,
                    cursor_activity,
                    cursor_id.map(|id| id as i64),
                    limit as i64 + 1,
                ],
                row_to_session,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut sessions = rows.into_iter().collect::<Result<Vec<_>>>()?;
        let has_more = sessions.len() > limit as usize;
        sessions.truncate(limit as usize);
        let next_cursor = if has_more {
            sessions
                .last()
                .map(|s| {
                    let rank = search_rank(s, query, exact_id);
                    format!("{rank}:{}:{}", s.last_activity_at_unix_ms, s.id)
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        Ok(SessionPage {
            sessions,
            next_cursor,
            total,
        })
    }

    pub fn set_session_desired_running(&self, id: u64, desired: bool) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET desired_running = ?2 WHERE id = ?1",
            params![id as i64, desired],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    pub fn set_agent_desired_running(
        &self,
        session_id: u64,
        terminal_id: u64,
        desired: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let sessions = tx.execute(
            "UPDATE sessions SET desired_running = ?2 WHERE id = ?1",
            params![session_id as i64, desired],
        )?;
        let terminals = tx.execute(
            "UPDATE terminals SET desired_running = ?2 WHERE id = ?1 AND session_id = ?3 AND kind = 'agent'",
            params![terminal_id as i64, desired, session_id as i64],
        )?;
        if sessions == 0 || terminals == 0 {
            return Err(StorageError::NotFound(
                "agent terminal for session",
                session_id,
            ));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn session_desired_running(&self, id: u64) -> Result<bool> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT desired_running FROM sessions WHERE id = ?1",
                params![id as i64],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StorageError::NotFound("session", id))
    }

    pub fn auto_resume_sessions_on_worker(&self, worker_id: u64) -> Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{SESSION_SELECT} WHERE desired_running = 1 AND worker_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt
            .query_map(params![worker_id as i64], row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    pub fn update_session_state(
        &self,
        id: u64,
        state: SessionState,
        detail: &str,
    ) -> Result<Session> {
        {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            let previous: Option<String> = tx
                .query_row(
                    "SELECT state FROM sessions WHERE id = ?1",
                    params![id as i64],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(previous) = previous else {
                return Err(StorageError::NotFound("session", id));
            };
            // Stamp the end of a session that reports a terminal state, as
            // the exit path does. A snapshot keeps a failed session only
            // while that stamp is recent, so leaving it unset drops the
            // session out of view the moment it fails.
            tx.execute(
                "UPDATE sessions SET state_revision = state_revision + (state != ?2), \
                 working_since_unix_ms = CASE WHEN ?2 = 'working' AND state != 'working' \
                 THEN ?4 ELSE working_since_unix_ms END, \
                 ended_at_unix_ms = CASE WHEN ?2 IN ('exited','failed') \
                 AND state NOT IN ('exited','failed') THEN ?4 ELSE ended_at_unix_ms END, \
                 state = ?2, state_detail = ?3 WHERE id = ?1",
                params![id as i64, state.as_str(), detail, now_unix_ms()],
            )?;
            record_state_attention(&tx, id, &previous, state)?;
            if state.is_live() {
                tx.execute(
                "UPDATE terminal_runs SET state = 'running' WHERE terminal_id = (SELECT id FROM terminals WHERE session_id = ?1 AND kind = 'agent') AND generation = (SELECT MAX(generation) FROM terminal_runs WHERE terminal_id = (SELECT id FROM terminals WHERE session_id = ?1 AND kind = 'agent'))",
                params![id as i64],
                )?;
            }
            tx.commit()?;
        }
        self.get_session(id)
    }

    /// Marks attention seen without acknowledging the NeedsInput lifecycle.
    pub fn mark_session_seen(&self, id: u64, read_at_unix_ms: i64) -> Result<Session> {
        {
            let conn = self.conn.lock().unwrap();
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                params![id as i64],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StorageError::NotFound("session", id));
            }
            conn.execute(
                "UPDATE notifications SET read_at_unix_ms = ?2 \
                 WHERE session_id = ?1 AND kind IN ('needs-input', 'turn-ended') \
                 AND read_at_unix_ms IS NULL",
                params![id as i64, read_at_unix_ms],
            )?;
        }
        self.get_session(id)
    }

    /// Records that a turn finished while the session was working, so the
    /// idle state it is about to enter reads as unseen until viewed.
    /// Records that a lifecycle hook was observed on this terminal
    /// generation. The stale-turn watchdog needs that fact to tell an agent
    /// whose hooks work from one whose hooks never fired, and it has to
    /// outlive the daemon process that saw the hook.
    pub fn record_generation_hook_seen(
        &self,
        terminal_id: u64,
        generation: u64,
        ts_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE terminal_runs SET hook_seen_at_unix_ms = ?3 \
             WHERE terminal_id = ?1 AND generation = ?2",
            params![terminal_id as i64, generation as i64, ts_unix_ms],
        )?;
        Ok(())
    }

    /// When a terminal generation started, and when a lifecycle hook was
    /// last seen on it. `None` for a generation that has no run row.
    pub fn generation_hook_mark(
        &self,
        terminal_id: u64,
        generation: u64,
    ) -> Result<Option<(i64, Option<i64>)>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COALESCE(started_at_unix_ms, 0), hook_seen_at_unix_ms \
                 FROM terminal_runs WHERE terminal_id = ?1 AND generation = ?2",
                params![terminal_id as i64, generation as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn snooze_supervision(&self, id: u64, generation: u64, until: i64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO supervisor_snoozes (session_id, generation, until_unix_ms, suppress_completion) \
             VALUES (?1, ?2, ?3, 1) ON CONFLICT(session_id) DO UPDATE SET \
             generation = excluded.generation, until_unix_ms = excluded.until_unix_ms, \
             suppress_completion = 1, silent_revision = NULL",
            params![id as i64, generation as i64, until],
        )?;
        Ok(())
    }

    pub fn supervision_snoozed_until(&self, id: u64, generation: u64) -> Result<i64> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT until_unix_ms FROM supervisor_snoozes WHERE session_id = ?1 AND generation = ?2",
            params![id as i64, generation as i64],
            |row| row.get(0),
        ).optional()?.unwrap_or_default())
    }

    pub fn clear_supervision_completion(&self, id: u64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE supervisor_snoozes SET suppress_completion = 0 WHERE session_id = ?1",
            params![id as i64],
        )?;
        Ok(())
    }

    pub fn finish_supervision_turn(
        &self,
        id: u64,
        generation: u64,
        revision: Option<u64>,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let suppressed = conn.query_row(
            "SELECT suppress_completion FROM supervisor_snoozes WHERE session_id = ?1 AND generation = ?2",
            params![id as i64, generation as i64],
            |row| row.get::<_, bool>(0),
        ).optional()?.unwrap_or(false);
        conn.execute(
            "UPDATE supervisor_snoozes SET suppress_completion = 0, silent_revision = ?3 \
             WHERE session_id = ?1 AND generation = ?2",
            params![
                id as i64,
                generation as i64,
                if suppressed {
                    revision.map(|v| v as i64)
                } else {
                    None
                }
            ],
        )?;
        Ok(suppressed)
    }

    pub fn supervision_completion_silent(
        &self,
        id: u64,
        generation: u64,
        revision: u64,
    ) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT silent_revision = ?3 FROM supervisor_snoozes \
             WHERE session_id = ?1 AND generation = ?2 AND silent_revision IS NOT NULL",
                params![id as i64, generation as i64, revision as i64],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn record_turn_finished(&self, id: u64, ts_unix_ms: i64) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "INSERT INTO notifications (session_id, ts_unix_ms, kind, read_at_unix_ms) \
             SELECT id, ?2, 'turn-ended', NULL FROM sessions WHERE id = ?1",
            params![id as i64, ts_unix_ms],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    pub fn set_session_ended(
        &self,
        id: u64,
        state: SessionState,
        detail: &str,
        exit_code: Option<i32>,
        ended_at_unix_ms: i64,
    ) -> Result<Session> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET state_revision = state_revision + (state != ?2), \
             state = ?2, state_detail = ?3, exit_code = ?4, ended_at_unix_ms = ?5 WHERE id = ?1",
            params![
                id as i64,
                state.as_str(),
                detail,
                exit_code,
                ended_at_unix_ms
            ],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        self.conn.lock().unwrap().execute(
            "UPDATE notifications SET read_at_unix_ms = ?2 \
             WHERE session_id = ?1 AND kind = 'turn-ended' AND read_at_unix_ms IS NULL",
            params![id as i64, ended_at_unix_ms],
        )?;
        self.conn.lock().unwrap().execute(
            "UPDATE terminal_runs SET state = ?2, ended_at_unix_ms = ?3, exit_code = ?4 WHERE terminal_id = (SELECT id FROM terminals WHERE session_id = ?1 AND kind = 'agent') AND generation = (SELECT MAX(generation) FROM terminal_runs WHERE terminal_id = (SELECT id FROM terminals WHERE session_id = ?1 AND kind = 'agent'))",
            params![id as i64, if state == SessionState::Failed { "failed" } else { "exited" }, ended_at_unix_ms, exit_code],
        )?;
        self.get_session(id)
    }

    /// Resets an ended session's row so its own entry can host a fresh
    /// agent process, clearing the end-of-run fields and stale live
    /// state. Keeps the agent session id and transcript path so the
    /// resume can target the same conversation.
    pub fn reactivate_session(&self, id: u64, state: SessionState) -> Result<Session> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET state_revision = state_revision + (state != ?2), \
             working_since_unix_ms = CASE WHEN ?2 = 'working' AND state != 'working' \
             THEN ?3 ELSE working_since_unix_ms END, \
             state = ?2, state_detail = '', exit_code = NULL, \
             ended_at_unix_ms = NULL, activity = '', progress_percent = NULL, desired_running = 1 WHERE id = ?1",
            params![id as i64, state.as_str(), now_unix_ms()],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        self.get_session(id)
    }

    pub fn set_session_token(&self, id: u64, token: &str) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET session_token = ?2 WHERE id = ?1",
            params![id as i64, token],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    /// Applies an agent activity report: updates the live fields and
    /// appends to the activity_reports history.
    pub fn record_activity(&self, id: u64, update: &ActivityUpdate) -> Result<Session> {
        {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            let previous: Option<String> = tx
                .query_row(
                    "SELECT state FROM sessions WHERE id = ?1",
                    params![id as i64],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(previous) = previous else {
                return Err(StorageError::NotFound("session", id));
            };
            tx.execute(
                "UPDATE sessions SET state_revision = state_revision + (state != ?2), \
                 working_since_unix_ms = CASE WHEN ?2 = 'working' AND state != 'working' \
                 THEN ?6 ELSE working_since_unix_ms END, \
                 state = ?2, state_detail = ?3, activity = ?4, progress_percent = ?5 WHERE id = ?1",
                params![
                    id as i64,
                    update.state.as_str(),
                    update.state_detail,
                    update.activity,
                    update.progress_percent.map(|v| v as i64),
                    now_unix_ms()
                ],
            )?;
            record_state_attention(&tx, id, &previous, update.state)?;
            tx.execute(
                "INSERT INTO activity_reports (session_id, ts_unix_ms, kind, payload) VALUES (?1, ?2, ?3, ?4)",
                params![id as i64, update.ts_unix_ms, update.kind, update.payload],
            )?;
            tx.commit()?;
        }
        self.get_session(id)
    }

    /// Persists a batch of debounced terminal activity timestamps in one
    /// transaction and returns the refreshed sessions for publication.
    pub fn checkpoint_session_activity(
        &self,
        updates: &[SessionActivityUpdate],
    ) -> Result<Vec<Session>> {
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            for update in updates {
                tx.execute(
                    "INSERT INTO session_activity (session_id, last_agent_activity_at_unix_ms, last_user_interaction_at_unix_ms, last_user_submit_at_unix_ms) \
                     VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT(session_id) DO UPDATE SET \
                       last_agent_activity_at_unix_ms = MAX(last_agent_activity_at_unix_ms, excluded.last_agent_activity_at_unix_ms), \
                       last_user_interaction_at_unix_ms = MAX(last_user_interaction_at_unix_ms, excluded.last_user_interaction_at_unix_ms), \
                       last_user_submit_at_unix_ms = MAX(last_user_submit_at_unix_ms, excluded.last_user_submit_at_unix_ms)",
                    params![
                        update.session_id as i64,
                        update.last_agent_activity_at_unix_ms.unwrap_or(0),
                        update.last_user_interaction_at_unix_ms.unwrap_or(0),
                        update.last_user_submit_at_unix_ms.unwrap_or(0),
                    ],
                )?;
            }
            tx.commit()?;
        }
        updates
            .iter()
            .map(|update| self.get_session(update.session_id))
            .collect()
    }

    pub fn activity_reports(&self, session_id: u64) -> Result<Vec<ActivityReport>> {
        let conn = self.conn.lock().unwrap();
        let reports = conn
            .prepare(
                "SELECT ts_unix_ms, kind, payload FROM activity_reports \
                 WHERE session_id = ?1 ORDER BY id",
            )?
            .query_map(params![session_id as i64], |r| {
                Ok(ActivityReport {
                    ts_unix_ms: r.get(0)?,
                    kind: r.get(1)?,
                    payload: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(reports)
    }

    /// Replaces a session's goal. Callers skip blank goals so a report
    /// that omits one keeps the current value.
    pub fn set_goal(&self, id: u64, goal: &str) -> Result<Session> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET goal = ?2 WHERE id = ?1",
            params![id as i64, goal],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        self.get_session(id)
    }

    /// Sets a session's agent-authored rolling summary. Empty strings
    /// are written verbatim so the agent can clear either field.
    pub fn set_headline_summary(&self, id: u64, headline: &str, summary: &str) -> Result<Session> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET headline = ?2, summary = ?3 WHERE id = ?1",
            params![id as i64, headline, summary],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        self.get_session(id)
    }

    /// Points a session at a worker.
    pub fn set_session_worker(&self, id: u64, worker_id: u64) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET worker_id = ?2 WHERE id = ?1",
            params![id as i64, worker_id as i64],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    /// Updates a session's agent-reported git location. Each `None`
    /// field keeps its stored value, so an agent that reports only a
    /// branch does not blank the worktree it reported earlier. Returns
    /// the refreshed session for publication.
    pub fn set_session_git(&self, id: u64, git: &SessionGitUpdate) -> Result<Session> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET
             git_branch = COALESCE(?2, git_branch),
             git_worktree = COALESCE(?3, git_worktree),
             git_repo_root = COALESCE(?4, git_repo_root),
             git_commit = COALESCE(?5, git_commit),
             git_upstream = COALESCE(?6, git_upstream),
             git_dirty = COALESCE(?7, git_dirty)
             WHERE id = ?1",
            params![
                id as i64,
                git.branch,
                git.worktree,
                git.repo_root,
                git.commit,
                git.upstream,
                git.dirty.map(i64::from),
            ],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        self.get_session(id)
    }

    /// Updates a session's MCP tool grants; `None` fields keep the
    /// stored value. Returns the refreshed session for publication.
    pub fn set_session_apis(
        &self,
        id: u64,
        items_api: Option<bool>,
        supervisor_api: Option<bool>,
        role: Option<SessionRole>,
    ) -> Result<Session> {
        {
            let conn = self.conn.lock().unwrap();
            let n = conn.execute(
                "UPDATE sessions SET
                 role = COALESCE(?4, role),
                 items_api = CASE WHEN COALESCE(?4, role) = 'supervisor' THEN 1 ELSE COALESCE(?2, items_api) END,
                 supervisor_api = CASE WHEN COALESCE(?4, role) = 'supervisor' THEN 1 ELSE COALESCE(?3, 0) END
                 WHERE id = ?1",
                params![id as i64, items_api, supervisor_api, role.map(SessionRole::as_str)],
            )?;
            if n == 0 {
                return Err(StorageError::NotFound("session", id));
            }
        }
        self.get_session(id)
    }

    /// Appends a timestamped checkpoint entry to the session timeline,
    /// reusing `activity_reports` with the checkpoint kind. `payload` is
    /// the JSON timeline entry.
    pub fn append_checkpoint(&self, id: u64, ts_unix_ms: i64, payload: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO activity_reports (session_id, ts_unix_ms, kind, payload) VALUES (?1, ?2, ?3, ?4)",
            params![id as i64, ts_unix_ms, CHECKPOINT_KIND, payload],
        )?;
        Ok(())
    }

    /// Appends a timeline note about something the user did to the session.
    pub fn append_user_note(&self, id: u64, ts_unix_ms: i64, payload: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO activity_reports (session_id, ts_unix_ms, kind, payload) VALUES (?1, ?2, ?3, ?4)",
            params![id as i64, ts_unix_ms, USER_NOTE_KIND, payload],
        )?;
        Ok(())
    }

    /// Records that the agent finished, failed, or paused a turn, which is
    /// the hook-driven half of the session's activity clock.
    pub fn record_agent_turn(&self, id: u64, ts_unix_ms: i64) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "INSERT INTO session_activity (session_id, last_agent_turn_at_unix_ms) \
             SELECT id, ?2 FROM sessions WHERE id = ?1 \
             ON CONFLICT(session_id) DO UPDATE SET \
               last_agent_turn_at_unix_ms = MAX(last_agent_turn_at_unix_ms, excluded.last_agent_turn_at_unix_ms)",
            params![id as i64, ts_unix_ms],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    /// Replaces the session's glance bag wholesale, keeping the given
    /// order. Callers bound the set before calling; this writes verbatim.
    pub fn replace_glance(&self, id: u64, fields: &[ContextField]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM session_glance WHERE session_id = ?1",
            params![id as i64],
        )?;
        for (position, f) in fields.iter().enumerate() {
            tx.execute(
                "INSERT INTO session_glance (session_id, position, key, label, value, kind, severity) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id as i64,
                    position as i64,
                    f.key,
                    f.label,
                    f.value,
                    f.kind.as_str(),
                    f.severity.as_str()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Upserts detail-context fields by key, leaving other keys intact.
    /// Updates to existing keys always apply; a new key is dropped once
    /// the session already holds `CONTEXT_MAX_FIELDS`, so the bag can
    /// never grow past its cap. Returns the count of new keys dropped.
    pub fn upsert_context(
        &self,
        id: u64,
        fields: &[ContextField],
        updated_at: i64,
    ) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut count: usize = tx.query_row(
            "SELECT COUNT(*) FROM session_context WHERE session_id = ?1",
            params![id as i64],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let mut dropped = 0usize;
        for f in fields {
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM session_context WHERE session_id = ?1 AND key = ?2",
                    params![id as i64, f.key],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !exists && count >= CONTEXT_MAX_FIELDS {
                dropped += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO session_context (session_id, key, label, value, kind, severity, updated_at_unix_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(session_id, key) DO UPDATE SET \
                 label = excluded.label, value = excluded.value, kind = excluded.kind, \
                 severity = excluded.severity, updated_at_unix_ms = excluded.updated_at_unix_ms",
                params![
                    id as i64,
                    f.key,
                    f.label,
                    f.value,
                    f.kind.as_str(),
                    f.severity.as_str(),
                    updated_at
                ],
            )?;
            if !exists {
                count += 1;
            }
        }
        tx.commit()?;
        Ok(dropped)
    }

    /// Removes detail-context fields by key. Missing keys are ignored.
    pub fn clear_context(&self, id: u64, keys: &[String]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for key in keys {
            tx.execute(
                "DELETE FROM session_context WHERE session_id = ?1 AND key = ?2",
                params![id as i64, key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The session's current glance and detail bags. Detail is ordered
    /// by most-recently-updated first so the panel leads with fresh facts.
    pub fn session_context(&self, id: u64) -> Result<SessionContext> {
        let conn = self.conn.lock().unwrap();
        let glance = conn
            .prepare(
                "SELECT key, label, value, kind, severity FROM session_glance \
                 WHERE session_id = ?1 ORDER BY position",
            )?
            .query_map(params![id as i64], row_to_context_field)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let detail = conn
            .prepare(
                "SELECT key, label, value, kind, severity FROM session_context \
                 WHERE session_id = ?1 ORDER BY updated_at_unix_ms, key",
            )?
            .query_map(params![id as i64], row_to_context_field)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(SessionContext {
            session_id: id,
            glance,
            detail,
        })
    }

    pub fn get_session_token(&self, id: u64) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT session_token FROM sessions WHERE id = ?1",
                params![id as i64],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Resolves a per-session hook/report token to its session id.
    pub fn get_session_id_by_token(&self, token: &str) -> Result<Option<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM sessions WHERE session_token = ?1",
                params![token],
                |r| Ok(r.get::<_, i64>(0)? as u64),
            )
            .optional()?)
    }

    /// Records a message handed to a session, and mints its one-time
    /// reply capability when the sender asked for an answer.
    pub fn record_agent_message(
        &self,
        from_session_id: u64,
        to_session_id: u64,
        body: &str,
        transport: &str,
        reply: Option<(&str, i64)>,
    ) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_messages \
             (from_session_id, to_session_id, body, transport, reply_token, \
              reply_expires_at_unix_ms, created_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                from_session_id as i64,
                to_session_id as i64,
                body,
                transport,
                reply.map(|(token, _)| token),
                reply.map(|(_, expires)| expires),
                now_unix_ms(),
            ],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    pub fn agent_message(&self, id: u64) -> Result<AgentMessage> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, from_session_id, to_session_id, body, transport, reply_token, \
                 reply_expires_at_unix_ms, reply_used_at_unix_ms, reply_body, replied_at_unix_ms \
                 FROM agent_messages WHERE id = ?1",
                params![id as i64],
                row_to_agent_message,
            )
            .optional()?
            .ok_or(StorageError::NotFound("agent message", id))
    }

    /// Burns the reply capability and records the answer, in one step so
    /// a token can never be spent without an answer landing with it.
    ///
    /// Returns false when the capability was already spent, which is how
    /// a duplicate reply is refused rather than overwriting the first.
    pub fn record_agent_reply(&self, id: u64, body: &str) -> Result<bool> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE agent_messages SET reply_body = ?2, replied_at_unix_ms = ?3, \
             reply_used_at_unix_ms = ?3 \
             WHERE id = ?1 AND reply_token IS NOT NULL AND reply_used_at_unix_ms IS NULL",
            params![id as i64, body, now_unix_ms()],
        )?;
        Ok(n > 0)
    }

    /// Records the loopback port an agent was launched on, so its
    /// inbound channel survives a daemon restart.
    pub fn set_agent_port(&self, id: u64, port: u16) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET agent_port = ?2 WHERE id = ?1",
            params![id as i64, port as i64],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    pub fn agent_port(&self, id: u64) -> Result<Option<u16>> {
        let port: Option<i64> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT agent_port FROM sessions WHERE id = ?1",
                params![id as i64],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(port.and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0))
    }

    pub fn set_agent_session_id(&self, id: u64, agent_session_id: &str) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET agent_session_id = ?2 WHERE id = ?1",
            params![id as i64, agent_session_id],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    /// Records the agent's own session id and transcript path, learned
    /// from a hook payload; ignores empty values so a later hook cannot
    /// clobber an id an earlier one already captured.
    pub fn set_agent_identity(
        &self,
        id: u64,
        agent_session_id: &str,
        transcript_path: &str,
    ) -> Result<()> {
        if agent_session_id.is_empty() {
            return Ok(());
        }
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET agent_session_id = ?2, transcript_path = ?3, agent_resumable = 1 WHERE id = ?1",
            params![id as i64, agent_session_id, transcript_path],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    pub fn get_transcript_path(&self, id: u64) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT transcript_path FROM sessions WHERE id = ?1",
                params![id as i64],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn set_agent_resumable(&self, id: u64, resumable: bool) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE sessions SET agent_resumable = ?2 WHERE id = ?1",
            params![id as i64, resumable],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("session", id));
        }
        Ok(())
    }

    pub fn live_session_count(&self) -> Result<usize> {
        let n: i64 = self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM sessions WHERE desired_running = 1 OR state NOT IN ('exited', 'failed')",
            [],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    pub fn live_spawned_by_count(&self, supervisor_id: u64) -> Result<usize> {
        let n: i64 = self.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM sessions WHERE spawned_by_session_id = ?1 \
             AND (desired_running = 1 OR state NOT IN ('exited', 'failed'))",
            params![supervisor_id as i64],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Every session a supervisor spawned, newest first.
    /// Sessions the daemon believes are mid-turn, for the stale-turn
    /// watchdog. Ordered oldest first so the longest-running suspect is
    /// reported first.
    pub fn working_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{SESSION_SELECT} WHERE state = ?1 ORDER BY id ASC"
        ))?;
        let sessions = stmt
            .query_map(params![SessionState::Working.as_str()], row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        Ok(sessions)
    }

    /// Records the child transition its supervisor has been told about,
    /// so a restart does not re-announce it.
    pub fn set_announced_wake_key(&self, id: u64, key: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE sessions SET announced_wake_key = ?2 WHERE id = ?1",
            params![id as i64, key],
        )?;
        Ok(())
    }

    pub fn announced_wake_key(&self, id: u64) -> Result<Option<String>> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT announced_wake_key FROM sessions WHERE id = ?1",
            params![id as i64],
            |row| row.get(0),
        )?)
    }

    pub fn spawned_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{SESSION_SELECT} WHERE spawned_by_session_id IS NOT NULL ORDER BY id DESC"
        ))?;
        let sessions = stmt
            .query_map([], row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        Ok(sessions)
    }

    /// Public item identities linked to a session, oldest link first.
    pub fn item_refs_for_session(&self, session_id: u64) -> Result<Vec<ItemRef>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT item.bucket_id, item.item_number FROM item_sessions linked \
             JOIN items item ON item.id = linked.item_id \
             WHERE linked.session_id = ?1 ORDER BY linked.item_id",
        )?;
        let refs = stmt
            .query_map(params![session_id as i64], |row| {
                Ok(ItemRef {
                    bucket_id: row.get::<_, i64>(0)? as u64,
                    item_id: row.get::<_, i64>(1)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(refs)
    }

    pub fn user_count(&self) -> Result<u64> {
        let n: i64 =
            self.conn
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    pub fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        created_at_unix_ms: i64,
    ) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash, created_at_unix_ms) VALUES (?1, ?2, ?3)",
            params![username, password_hash, created_at_unix_ms],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StorageError::Conflict(format!("user {username:?} already exists"))
            }
            e => e.into(),
        })?;
        Ok(conn.last_insert_rowid() as u64)
    }

    /// Returns (user id, password hash).
    pub fn get_user_by_name(&self, username: &str) -> Result<Option<(u64, String)>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, password_hash FROM users WHERE username = ?1",
                params![username],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?)),
            )
            .optional()?)
    }

    pub fn get_username(&self, user_id: u64) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT username FROM users WHERE id = ?1",
                params![user_id as i64],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The stored password hash for a user id, for re-authenticating
    /// someone already holding a session rather than a username.
    pub fn get_user_hash(&self, user_id: u64) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT password_hash FROM users WHERE id = ?1",
                params![user_id as i64],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_user_password(&self, user_id: u64, password_hash: &str) -> Result<()> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE users SET password_hash = ?2 WHERE id = ?1",
            params![user_id as i64, password_hash],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("user", user_id));
        }
        Ok(())
    }

    /// Drops every session a user holds apart from the one named, which
    /// is how a password change signs out the other browsers and devices
    /// without signing out the person making it.
    pub fn delete_other_auth_sessions(&self, user_id: u64, keep_token_hash: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM auth_sessions WHERE user_id = ?1 AND token_hash != ?2",
            params![user_id as i64, keep_token_hash],
        )?;
        Ok(())
    }

    pub fn create_auth_session(
        &self,
        token_hash: &str,
        user_id: u64,
        created_at_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO auth_sessions (token_hash, user_id, created_at_unix_ms, expires_at_unix_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, user_id as i64, created_at_unix_ms, expires_at_unix_ms],
        )?;
        Ok(())
    }

    /// Resolves a token hash to its user id, treating expired rows as
    /// absent and deleting them.
    pub fn lookup_auth_session(&self, token_hash: &str, now_unix_ms: i64) -> Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM auth_sessions WHERE expires_at_unix_ms <= ?1",
            params![now_unix_ms],
        )?;
        Ok(conn
            .query_row(
                "SELECT user_id FROM auth_sessions WHERE token_hash = ?1",
                params![token_hash],
                |r| Ok(r.get::<_, i64>(0)? as u64),
            )
            .optional()?)
    }

    pub fn delete_auth_session(&self, token_hash: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM auth_sessions WHERE token_hash = ?1",
            params![token_hash],
        )?;
        Ok(())
    }

    /// Records a pending mobile enrollment as a one-time token hash.
    pub fn create_mobile_enrollment_token(
        &self,
        token_hash: &str,
        user_id: u64,
        created_at_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM mobile_enrollment_tokens WHERE expires_at_unix_ms <= ?1",
            params![created_at_unix_ms],
        )?;
        conn.execute(
            "INSERT INTO mobile_enrollment_tokens \
             (token_hash, user_id, created_at_unix_ms, expires_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                token_hash,
                user_id as i64,
                created_at_unix_ms,
                expires_at_unix_ms
            ],
        )?;
        Ok(())
    }

    /// Consumes a pending mobile enrollment token, returning the user it
    /// enrolls for. Fails if the token is unknown, already used, or
    /// expired, and burns it on success.
    pub fn consume_mobile_enrollment_token(
        &self,
        token_hash: &str,
        now_unix_ms: i64,
    ) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(i64, Option<i64>, i64)> = conn
            .query_row(
                "SELECT user_id, used_at_unix_ms, expires_at_unix_ms \
                 FROM mobile_enrollment_tokens WHERE token_hash = ?1",
                params![token_hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (user_id, used, expires) =
            row.ok_or_else(|| StorageError::Conflict("unknown enrollment token".into()))?;
        if used.is_some() {
            return Err(StorageError::Conflict(
                "enrollment token already used".into(),
            ));
        }
        if now_unix_ms > expires {
            return Err(StorageError::Conflict("enrollment token expired".into()));
        }
        conn.execute(
            "UPDATE mobile_enrollment_tokens SET used_at_unix_ms = ?2 WHERE token_hash = ?1",
            params![token_hash, now_unix_ms],
        )?;
        Ok(user_id as u64)
    }

    /// Registers an enrollment for one app installation, returning the
    /// device row and whether it reused the installation's existing one.
    /// A second enrollment of a live installation re-points that row and
    /// drops the previous login's credentials rather than stacking a new
    /// device, so one phone stays one row and one push endpoint. Revoked
    /// rows are not reused: a revoked installation comes back as new.
    #[allow(clippy::too_many_arguments)]
    pub fn enroll_mobile_device(
        &self,
        user_id: u64,
        name: &str,
        platform: &str,
        app_installation_id: &str,
        refresh_family_id: &str,
        created_at_unix_ms: i64,
    ) -> Result<(MobileDevice, bool)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existing: Option<u64> = tx
            .query_row(
                "SELECT id FROM mobile_devices WHERE user_id = ?1 AND app_installation_id = ?2 \
                 AND revoked_at_unix_ms IS NULL",
                params![user_id as i64, app_installation_id],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE mobile_devices SET name = ?2, platform = ?3, refresh_family_id = ?4 \
                     WHERE id = ?1",
                    params![id as i64, name, platform, refresh_family_id],
                )?;
                for table in [
                    "mobile_access_tokens",
                    "mobile_refresh_tokens",
                    "socket_tickets",
                ] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE device_id = ?1"),
                        params![id as i64],
                    )?;
                }
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO mobile_devices (user_id, name, platform, app_installation_id, \
                     refresh_family_id, created_at_unix_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        user_id as i64,
                        name,
                        platform,
                        app_installation_id,
                        refresh_family_id,
                        created_at_unix_ms
                    ],
                )?;
                tx.last_insert_rowid() as u64
            }
        };
        let device = tx.query_row(
            &format!("{MOBILE_DEVICE_SELECT} WHERE id = ?1"),
            params![id as i64],
            row_to_mobile_device,
        )?;
        tx.commit()?;
        Ok((device, existing.is_some()))
    }

    pub fn get_mobile_device(&self, id: u64) -> Result<MobileDevice> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{MOBILE_DEVICE_SELECT} WHERE id = ?1"),
                params![id as i64],
                row_to_mobile_device,
            )
            .optional()?
            .ok_or(StorageError::NotFound("mobile device", id))
    }

    /// The user's devices, revoked rows excluded: a revoked device is
    /// gone as far as the app and the management UI are concerned.
    pub fn list_mobile_devices(&self, user_id: u64) -> Result<Vec<MobileDevice>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{MOBILE_DEVICE_SELECT} WHERE user_id = ?1 AND revoked_at_unix_ms IS NULL ORDER BY id"
        ))?;
        let devices = stmt
            .query_map(params![user_id as i64], row_to_mobile_device)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(devices)
    }

    /// Marks a device revoked and deletes its live access tokens, so
    /// bearer auth and refresh both stop working immediately. Refresh
    /// token rows stay for audit; the device check rejects them.
    pub fn revoke_mobile_device(&self, id: u64, now_unix_ms: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "UPDATE mobile_devices SET revoked_at_unix_ms = ?2 \
             WHERE id = ?1 AND revoked_at_unix_ms IS NULL",
            params![id as i64, now_unix_ms],
        )?;
        if changed == 0 {
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM mobile_devices WHERE id = ?1",
                params![id as i64],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Err(StorageError::NotFound("mobile device", id));
            }
        }
        conn.execute(
            "DELETE FROM mobile_access_tokens WHERE device_id = ?1",
            params![id as i64],
        )?;
        Ok(())
    }

    pub fn create_mobile_refresh_token(
        &self,
        device_id: u64,
        token_hash: &str,
        created_at_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO mobile_refresh_tokens \
             (device_id, token_hash, created_at_unix_ms, expires_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                device_id as i64,
                token_hash,
                created_at_unix_ms,
                expires_at_unix_ms
            ],
        )?;
        Ok(())
    }

    pub fn lookup_mobile_refresh_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<MobileRefreshToken>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, device_id, expires_at_unix_ms, used_at_unix_ms, \
                 successor_refresh_hash, successor_access_hash, retry_at_unix_ms \
                 FROM mobile_refresh_tokens WHERE token_hash = ?1",
                params![token_hash],
                |r| {
                    Ok(MobileRefreshToken {
                        id: r.get::<_, i64>(0)? as u64,
                        device_id: r.get::<_, i64>(1)? as u64,
                        expires_at_unix_ms: r.get(2)?,
                        used_at_unix_ms: r.get(3)?,
                        successor_refresh_hash: r.get(4)?,
                        successor_access_hash: r.get(5)?,
                        retry_at_unix_ms: r.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn mark_mobile_refresh_token_used(&self, id: u64, now_unix_ms: i64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE mobile_refresh_tokens SET used_at_unix_ms = ?2 WHERE id = ?1",
            params![id as i64, now_unix_ms],
        )?;
        Ok(())
    }

    /// Records the successor hashes on a rotated refresh token so the
    /// one-retry rule can tell whether the successor has been spent.
    pub fn set_mobile_refresh_token_successor(
        &self,
        id: u64,
        successor_refresh_hash: &str,
        successor_access_hash: &str,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE mobile_refresh_tokens SET \
             successor_refresh_hash = ?2, successor_access_hash = ?3 \
             WHERE id = ?1",
            params![id as i64, successor_refresh_hash, successor_access_hash],
        )?;
        Ok(())
    }

    /// Deletes the successor refresh and access token rows by hash,
    /// clearing the way for a replacement pair on a valid retry.
    pub fn revoke_mobile_successor_tokens(
        &self,
        successor_refresh_hash: &str,
        successor_access_hash: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM mobile_refresh_tokens WHERE token_hash = ?1",
            params![successor_refresh_hash],
        )?;
        conn.execute(
            "DELETE FROM mobile_access_tokens WHERE token_hash = ?1",
            params![successor_access_hash],
        )?;
        Ok(())
    }

    /// Marks the one retry as consumed.
    pub fn mark_mobile_refresh_token_retry(&self, id: u64, now_unix_ms: i64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE mobile_refresh_tokens SET retry_at_unix_ms = ?2 WHERE id = ?1",
            params![id as i64, now_unix_ms],
        )?;
        Ok(())
    }

    /// Checks whether a successor pair has been spent: the successor
    /// refresh token was rotated, or the successor access token was
    /// seen on an authenticated request. Returns the condition name
    /// for the revoke log when spent.
    pub fn check_mobile_successor_spent(
        &self,
        successor_refresh_hash: &str,
        successor_access_hash: &str,
    ) -> Result<Option<&'static str>> {
        let conn = self.conn.lock().unwrap();
        let refresh_used: Option<Option<i64>> = conn
            .query_row(
                "SELECT used_at_unix_ms FROM mobile_refresh_tokens \
                 WHERE token_hash = ?1",
                params![successor_refresh_hash],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(Some(_)) = refresh_used {
            return Ok(Some("successor rotated"));
        }
        let access_used: Option<Option<i64>> = conn
            .query_row(
                "SELECT first_used_at_unix_ms FROM mobile_access_tokens \
                 WHERE token_hash = ?1",
                params![successor_access_hash],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(Some(_)) = access_used {
            return Ok(Some("successor access token used"));
        }
        Ok(None)
    }

    /// Stamps the first authenticated use of an access token so the
    /// one-retry rule can tell whether a successor has been spent.
    pub fn mark_mobile_access_token_first_use(
        &self,
        token_hash: &str,
        now_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE mobile_access_tokens SET first_used_at_unix_ms = ?2 \
             WHERE token_hash = ?1 AND first_used_at_unix_ms IS NULL",
            params![token_hash, now_unix_ms],
        )?;
        Ok(())
    }

    /// Registers or rotates a device's push endpoint. Re-registration
    /// replaces the stored token and re-enables a disabled endpoint.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_push_endpoint(
        &self,
        device_id: u64,
        token_ciphertext: &str,
        environment: &str,
        locale: &str,
        previews_enabled: bool,
        event_mask: u32,
        public_key: &str,
        now_unix_ms: i64,
    ) -> Result<PushEndpoint> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO mobile_push_endpoints \
             (device_id, token_ciphertext, environment, locale, previews_enabled, \
              event_mask, public_key, created_at_unix_ms, updated_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
             ON CONFLICT(device_id) DO UPDATE SET \
               token_ciphertext = excluded.token_ciphertext, \
               environment = excluded.environment, \
               locale = excluded.locale, \
               previews_enabled = excluded.previews_enabled, \
               event_mask = excluded.event_mask, \
               push_counter = CASE WHEN public_key != excluded.public_key THEN 0 ELSE push_counter END, \
               public_key = excluded.public_key, \
               updated_at_unix_ms = excluded.updated_at_unix_ms, \
               disabled_at_unix_ms = NULL, \
               disabled_reason = ''",
            params![
                device_id as i64,
                token_ciphertext,
                environment,
                locale,
                previews_enabled,
                event_mask as i64,
                public_key,
                now_unix_ms
            ],
        )?;
        self.get_push_endpoint(device_id)?
            .ok_or(StorageError::NotFound("push endpoint", device_id))
    }

    pub fn get_push_endpoint(&self, device_id: u64) -> Result<Option<PushEndpoint>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                &format!(
                    "SELECT {PUSH_ENDPOINT_COLUMNS} FROM mobile_push_endpoints \
                     WHERE device_id = ?1"
                ),
                params![device_id as i64],
                row_to_push_endpoint,
            )
            .optional()?)
    }

    /// Atomically increments and returns the push counter for sealed
    /// notification replay rejection.
    pub fn increment_push_counter(&self, device_id: u64) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mobile_push_endpoints SET push_counter = push_counter + 1 WHERE device_id = ?1",
            params![device_id as i64],
        )?;
        let counter: i64 = conn.query_row(
            "SELECT push_counter FROM mobile_push_endpoints WHERE device_id = ?1",
            params![device_id as i64],
            |row| row.get(0),
        )?;
        Ok(counter as u64)
    }

    pub fn delete_push_endpoint(&self, device_id: u64) -> Result<bool> {
        let n = self.conn.lock().unwrap().execute(
            "DELETE FROM mobile_push_endpoints WHERE device_id = ?1",
            params![device_id as i64],
        )?;
        Ok(n > 0)
    }

    /// Turns an endpoint off in place, keeping the row so a later
    /// re-registration can tell rotation from first registration.
    pub fn disable_push_endpoint(
        &self,
        device_id: u64,
        reason: &str,
        now_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE mobile_push_endpoints SET disabled_at_unix_ms = ?2, disabled_reason = ?3 \
             WHERE device_id = ?1 AND disabled_at_unix_ms IS NULL",
            params![device_id as i64, now_unix_ms, reason],
        )?;
        Ok(())
    }

    /// Endpoints eligible for delivery: enabled, on a non-revoked
    /// device. Each is paired with the user that device belongs to, so
    /// per-user delivery rules need no second lookup.
    pub fn active_push_endpoints(&self) -> Result<Vec<(PushEndpoint, u64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PUSH_ENDPOINT_COLUMNS}, d.user_id FROM mobile_push_endpoints \
             JOIN mobile_devices d ON d.id = mobile_push_endpoints.device_id \
             WHERE disabled_at_unix_ms IS NULL AND d.revoked_at_unix_ms IS NULL \
             ORDER BY device_id"
        ))?;
        let endpoints = stmt
            .query_map([], |row| {
                Ok((
                    row_to_push_endpoint(row)?,
                    row.get::<_, i64>("user_id")? as u64,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(endpoints)
    }

    /// Queues one delivery, returning false when the (event, device)
    /// pair was already recorded so replays never alert twice.
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue_notification_delivery(
        &self,
        event_id: &str,
        device_id: u64,
        session_id: u64,
        state: &str,
        collapse_id: &str,
        status: &str,
        next_attempt_at_unix_ms: i64,
        now_unix_ms: i64,
    ) -> Result<bool> {
        let n = self.conn.lock().unwrap().execute(
            "INSERT OR IGNORE INTO notification_deliveries \
             (event_id, device_id, session_id, state, collapse_id, status, \
              attempt_count, next_attempt_at_unix_ms, created_at_unix_ms, updated_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?8)",
            params![
                event_id,
                device_id as i64,
                session_id as i64,
                state,
                collapse_id,
                status,
                next_attempt_at_unix_ms,
                now_unix_ms
            ],
        )?;
        Ok(n > 0)
    }

    pub fn due_notification_deliveries(
        &self,
        now_unix_ms: i64,
        limit: usize,
    ) -> Result<Vec<NotificationDelivery>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "{NOTIFICATION_DELIVERY_SELECT} WHERE status = 'pending' \
             AND next_attempt_at_unix_ms <= ?1 ORDER BY next_attempt_at_unix_ms LIMIT ?2"
        ))?;
        let deliveries = stmt
            .query_map(
                params![now_unix_ms, limit as i64],
                row_to_notification_delivery,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(deliveries)
    }

    pub fn lookup_notification_delivery(
        &self,
        event_id: &str,
        device_id: u64,
    ) -> Result<Option<NotificationDelivery>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{NOTIFICATION_DELIVERY_SELECT} WHERE event_id = ?1 AND device_id = ?2"),
                params![event_id, device_id as i64],
                row_to_notification_delivery,
            )
            .optional()?)
    }

    pub fn mark_delivery_sent(
        &self,
        id: u64,
        provider_message_id: Option<&str>,
        now_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE notification_deliveries SET status = 'sent', provider_message_id = ?2, \
             attempt_count = attempt_count + 1, last_error = '', updated_at_unix_ms = ?3 \
             WHERE id = ?1",
            params![id as i64, provider_message_id, now_unix_ms],
        )?;
        Ok(())
    }

    pub fn mark_delivery_retry(
        &self,
        id: u64,
        next_attempt_at_unix_ms: i64,
        error: &str,
        now_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE notification_deliveries SET attempt_count = attempt_count + 1, \
             next_attempt_at_unix_ms = ?2, last_error = ?3, updated_at_unix_ms = ?4 \
             WHERE id = ?1",
            params![id as i64, next_attempt_at_unix_ms, error, now_unix_ms],
        )?;
        Ok(())
    }

    /// Settles a delivery in a terminal status without another attempt.
    pub fn mark_delivery_settled(
        &self,
        id: u64,
        status: &str,
        error: &str,
        now_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE notification_deliveries SET status = ?2, attempt_count = attempt_count + 1, \
             last_error = ?3, updated_at_unix_ms = ?4 WHERE id = ?1",
            params![id as i64, status, error, now_unix_ms],
        )?;
        Ok(())
    }

    /// Receipt cleanup: settled rows older than the dedupe window are
    /// deleted; pending rows always survive.
    pub fn prune_notification_deliveries(&self, cutoff_unix_ms: i64) -> Result<usize> {
        let n = self.conn.lock().unwrap().execute(
            "DELETE FROM notification_deliveries \
             WHERE status != 'pending' AND updated_at_unix_ms < ?1",
            params![cutoff_unix_ms],
        )?;
        Ok(n)
    }

    pub fn set_push_policy(
        &self,
        bucket_id: u64,
        role: &str,
        events: &str,
        scope: &str,
        now_unix_ms: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO push_policies (bucket_id, role, events, scope, updated_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(bucket_id, role) DO UPDATE SET \
               events = excluded.events, scope = excluded.scope, \
               updated_at_unix_ms = excluded.updated_at_unix_ms",
            params![bucket_id as i64, role, events, scope, now_unix_ms],
        )?;
        Ok(())
    }

    pub fn delete_push_policy(&self, bucket_id: u64, role: &str) -> Result<bool> {
        let n = self.conn.lock().unwrap().execute(
            "DELETE FROM push_policies WHERE bucket_id = ?1 AND role = ?2",
            params![bucket_id as i64, role],
        )?;
        Ok(n > 0)
    }

    pub fn list_push_policies(&self) -> Result<Vec<PushPolicy>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT bucket_id, role, events, scope, updated_at_unix_ms \
             FROM push_policies ORDER BY bucket_id, role",
        )?;
        let policies = stmt
            .query_map([], |r| {
                Ok(PushPolicy {
                    bucket_id: r.get::<_, i64>(0)? as u64,
                    role: r.get(1)?,
                    events: r.get(2)?,
                    scope: r.get(3)?,
                    updated_at_unix_ms: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(policies)
    }

    /// The committed transition counter and the time the session last
    /// entered `working`, for event ids and completed detection.
    pub fn session_transition_marks(&self, id: u64) -> Result<(u64, Option<i64>)> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT state_revision, working_since_unix_ms FROM sessions WHERE id = ?1",
                params![id as i64],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
            )
            .optional()?
            .ok_or(StorageError::NotFound("session", id))
    }

    /// Identifies the newest question an agent asked with flag_blocked.
    /// A session already parked keeps its state and state revision when it
    /// asks, so this is what distinguishes a fresh question from the park
    /// its supervisor was already told about. rowid rather than a
    /// timestamp: two questions in the same millisecond are still two.
    pub fn latest_blocked_report(&self, session_id: u64) -> Result<Option<i64>> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT MAX(rowid) FROM activity_reports WHERE session_id = ?1 AND kind = ?2",
            params![session_id as i64, BLOCKED_KIND],
            |row| row.get::<_, Option<i64>>(0),
        )?)
    }

    /// Records an access token as its hash. `device_id` is `None` for the
    /// dashboard, which holds a token without enrolling anything.
    /// Drops every access token a cookie session minted. A logout has to end
    /// the authority it handed out, not just the session row.
    pub fn delete_access_tokens_of_session(&self, session_token_hash: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM mobile_access_tokens WHERE session_token_hash = ?1",
            params![session_token_hash],
        )?;
        Ok(())
    }

    pub fn create_access_token(
        &self,
        device_id: Option<u64>,
        user_id: u64,
        session_token_hash: Option<&str>,
        token_hash: &str,
        created_at_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM mobile_access_tokens WHERE expires_at_unix_ms <= ?1",
            params![created_at_unix_ms],
        )?;
        conn.execute(
            "INSERT INTO mobile_access_tokens \
             (token_hash, device_id, user_id, session_token_hash, created_at_unix_ms, \
              expires_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                token_hash,
                device_id.map(|id| id as i64),
                user_id as i64,
                session_token_hash,
                created_at_unix_ms,
                expires_at_unix_ms
            ],
        )?;
        Ok(())
    }

    /// Resolves a live access token hash to the device that holds it, if any,
    /// the user it speaks for, and whether its first use is already recorded.
    /// Expired tokens and tokens of revoked devices are absent.
    ///
    /// The last of those is returned rather than left to the caller to discover
    /// by writing: marking first use is a write, and a request that only needs
    /// to know who is asking should not open a write transaction to find out.
    pub fn lookup_access_token(
        &self,
        token_hash: &str,
        now_unix_ms: i64,
    ) -> Result<Option<AccessTokenHolder>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT t.device_id, t.user_id, t.first_used_at_unix_ms \
                 FROM mobile_access_tokens t \
                 LEFT JOIN mobile_devices d ON d.id = t.device_id \
                 WHERE t.token_hash = ?1 AND t.expires_at_unix_ms > ?2 \
                 AND (t.device_id IS NULL OR d.revoked_at_unix_ms IS NULL)",
                params![token_hash, now_unix_ms],
                |r| {
                    Ok(AccessTokenHolder {
                        device_id: r.get::<_, Option<i64>>(0)?.map(|id| id as u64),
                        user_id: r.get::<_, i64>(1)? as u64,
                        first_used: r.get::<_, Option<i64>>(2)?.is_some(),
                    })
                },
            )
            .optional()?)
    }

    /// Stamps device activity, at most once per minute to bound writes
    /// on the bearer-auth hot path.
    pub fn touch_mobile_device(&self, device_id: u64, now_unix_ms: i64) -> Result<()> {
        const LAST_SEEN_MIN_INTERVAL_MS: i64 = 60_000;
        self.conn.lock().unwrap().execute(
            "UPDATE mobile_devices SET last_seen_at_unix_ms = ?2 \
             WHERE id = ?1 AND COALESCE(last_seen_at_unix_ms, 0) <= ?2 - ?3",
            params![device_id as i64, now_unix_ms, LAST_SEEN_MIN_INTERVAL_MS],
        )?;
        Ok(())
    }

    /// Records a one-use socket ticket as its hash. Expired rows are
    /// cleaned opportunistically because tickets live only for seconds.
    pub fn create_socket_ticket(
        &self,
        token_hash: &str,
        user_id: u64,
        device_id: Option<u64>,
        terminal: Option<TerminalTicketBinding>,
        created_at_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM socket_tickets WHERE expires_at_unix_ms <= ?1",
            params![created_at_unix_ms],
        )?;
        conn.execute(
            "INSERT INTO socket_tickets \
             (token_hash, user_id, device_id, terminal_id, generation, replay_bytes, \
              created_at_unix_ms, expires_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                token_hash,
                user_id as i64,
                device_id.map(|id| id as i64),
                terminal.map(|t| t.terminal_id as i64),
                terminal.map(|t| t.generation as i64),
                terminal.map(|t| t.replay_bytes as i64),
                created_at_unix_ms,
                expires_at_unix_ms
            ],
        )?;
        Ok(())
    }

    /// Burns a socket ticket on its first presentation and returns the
    /// identity and bindings it was minted with. Unknown, reused, and
    /// expired tickets and tickets of revoked devices fail with the
    /// reason; a burned ticket stays burned even when the caller's
    /// binding check later fails.
    pub fn consume_socket_ticket(
        &self,
        token_hash: &str,
        now_unix_ms: i64,
    ) -> Result<SocketTicket> {
        let conn = self.conn.lock().unwrap();
        type Row = (
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            i64,
            Option<i64>,
            Option<i64>,
        );
        let row: Option<Row> = conn
            .query_row(
                "SELECT t.user_id, t.device_id, t.terminal_id, t.generation, t.replay_bytes, \
                 t.expires_at_unix_ms, t.consumed_at_unix_ms, d.revoked_at_unix_ms \
                 FROM socket_tickets t \
                 LEFT JOIN mobile_devices d ON d.id = t.device_id \
                 WHERE t.token_hash = ?1",
                params![token_hash],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            user_id,
            device_id,
            terminal_id,
            generation,
            replay_bytes,
            expires,
            consumed,
            revoked,
        )) = row
        else {
            return Err(StorageError::Conflict("unknown socket ticket".into()));
        };
        if consumed.is_some() {
            return Err(StorageError::Conflict("socket ticket already used".into()));
        }
        conn.execute(
            "UPDATE socket_tickets SET consumed_at_unix_ms = ?2 WHERE token_hash = ?1",
            params![token_hash, now_unix_ms],
        )?;
        if now_unix_ms > expires {
            return Err(StorageError::Conflict("socket ticket expired".into()));
        }
        if revoked.is_some() {
            return Err(StorageError::Conflict(
                "socket ticket device revoked".into(),
            ));
        }
        let terminal = match (terminal_id, generation, replay_bytes) {
            (Some(terminal_id), Some(generation), Some(replay_bytes)) => {
                Some(TerminalTicketBinding {
                    terminal_id: terminal_id as u64,
                    generation: generation as u64,
                    replay_bytes: replay_bytes as u64,
                })
            }
            _ => None,
        };
        Ok(SocketTicket {
            user_id: user_id as u64,
            device_id: device_id.map(|id| id as u64),
            terminal,
        })
    }

    pub fn get_user_setting(&self, user_id: u64, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM user_settings WHERE user_id = ?1 AND key = ?2",
                params![user_id as i64, key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn list_user_settings(&self, user_id: u64) -> Result<Vec<(String, String)>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare("SELECT key, value FROM user_settings WHERE user_id = ?1 ORDER BY key")?
            .query_map(params![user_id as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Writes one user preference; `None` deletes the row so the built-in
    /// user-level default applies again.
    pub fn set_user_setting(&self, user_id: u64, key: &str, value: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        match value {
            Some(value) => {
                conn.execute(
                    "INSERT INTO user_settings (user_id, key, value) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(user_id, key) DO UPDATE SET value = excluded.value",
                    params![user_id as i64, key, value],
                )?;
            }
            None => {
                conn.execute(
                    "DELETE FROM user_settings WHERE user_id = ?1 AND key = ?2",
                    params![user_id as i64, key],
                )?;
            }
        }
        Ok(())
    }

    pub fn list_workspaces(&self, user_id: u64) -> Result<Vec<Workspace>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id, name, layout_json, created_at_unix_ms, updated_at_unix_ms, position \
                 FROM workspaces WHERE user_id = ?1 ORDER BY position, id",
            )?
            .query_map(params![user_id as i64], row_to_workspace)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every user's workspaces, for the owner-trusted unix socket where
    /// no user identity exists.
    pub fn list_all_workspaces(&self) -> Result<Vec<Workspace>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id, name, layout_json, created_at_unix_ms, updated_at_unix_ms, position \
                 FROM workspaces ORDER BY position, id",
            )?
            .query_map([], row_to_workspace)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn create_workspace(
        &self,
        user_id: u64,
        name: &str,
        layout_json: &str,
        now_unix_ms: i64,
    ) -> Result<Workspace> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workspaces (user_id, name, layout_json, created_at_unix_ms, updated_at_unix_ms, position) \
             VALUES (?1, ?2, ?3, ?4, ?4, COALESCE((SELECT MAX(position) + 1 FROM workspaces WHERE user_id = ?1), 0))",
            params![user_id as i64, name, layout_json, now_unix_ms],
        )?;
        let id = conn.last_insert_rowid() as u64;
        drop(conn);
        self.get_workspace(user_id, id)
    }

    pub fn get_workspace(&self, user_id: u64, id: u64) -> Result<Workspace> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, name, layout_json, created_at_unix_ms, updated_at_unix_ms, position \
                 FROM workspaces WHERE user_id = ?1 AND id = ?2",
                params![user_id as i64, id as i64],
                row_to_workspace,
            )
            .optional()?
            .ok_or(StorageError::NotFound("workspace", id))
    }

    pub fn update_workspace(
        &self,
        user_id: u64,
        id: u64,
        name: &str,
        layout_json: &str,
        now_unix_ms: i64,
    ) -> Result<Workspace> {
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE workspaces SET name = ?3, layout_json = ?4, updated_at_unix_ms = ?5 \
             WHERE user_id = ?1 AND id = ?2",
            params![user_id as i64, id as i64, name, layout_json, now_unix_ms],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound("workspace", id));
        }
        self.get_workspace(user_id, id)
    }

    pub fn delete_workspace(&self, user_id: u64, id: u64) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let position = tx
            .query_row(
                "SELECT position FROM workspaces WHERE user_id = ?1 AND id = ?2",
                params![user_id as i64, id as i64],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(StorageError::NotFound("workspace", id))?;
        tx.execute(
            "DELETE FROM workspaces WHERE user_id = ?1 AND id = ?2",
            params![user_id as i64, id as i64],
        )?;
        tx.execute(
            "UPDATE workspaces SET position = position - 1 WHERE user_id = ?1 AND position > ?2",
            params![user_id as i64, position],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn reorder_workspaces(&self, user_id: u64, workspace_ids: &[u64]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existing = tx
            .prepare("SELECT id FROM workspaces WHERE user_id = ?1 ORDER BY id")?
            .query_map(params![user_id as i64], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let requested = workspace_ids
            .iter()
            .map(|id| *id as i64)
            .collect::<Vec<_>>();
        let mut sorted_requested = requested.clone();
        sorted_requested.sort_unstable();
        if existing != sorted_requested {
            return Err(StorageError::Conflict(
                "workspace order must include every workspace once".into(),
            ));
        }
        for (position, id) in requested.into_iter().enumerate() {
            tx.execute(
                "UPDATE workspaces SET position = ?3 WHERE user_id = ?1 AND id = ?2",
                params![user_id as i64, id, position as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_session_forward(
        &self,
        session_id: u64,
        worker_port: u16,
        listener_port: u16,
        slug: &str,
        label: &str,
        scheme: &str,
        now_unix_ms: i64,
    ) -> Result<SessionForward> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO session_forwards (session_id, worker_port, listener_port, slug, label, scheme, created_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id as i64,
                worker_port as i64,
                listener_port as i64,
                slug,
                label,
                scheme,
                now_unix_ms
            ],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StorageError::Conflict(format!(
                    "session {session_id} already publishes port {worker_port}, \
                     or slug {slug} is taken"
                ))
            }
            e => e.into(),
        })?;
        let id = conn.last_insert_rowid() as u64;
        Ok(conn
            .query_row(
                &format!("{FORWARD_SELECT} WHERE f.id = ?1"),
                params![id as i64],
                row_to_session_forward,
            )
            .optional()?
            .expect("just-inserted forward row"))
    }

    pub fn get_session_forward(
        &self,
        session_id: u64,
        worker_port: u16,
    ) -> Result<Option<SessionForward>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{FORWARD_SELECT} WHERE f.session_id = ?1 AND f.worker_port = ?2"),
                params![session_id as i64, worker_port as i64],
                row_to_session_forward,
            )
            .optional()?)
    }

    /// The forward holding a slug, anywhere on the controller. Slugs are
    /// unique across every session because each one is a hostname.
    pub fn session_forward_by_slug(&self, slug: &str) -> Result<Option<SessionForward>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{FORWARD_SELECT} WHERE f.slug = ?1 AND f.slug != ''"),
                params![slug],
                row_to_session_forward,
            )
            .optional()?)
    }

    /// A slug's holder, counting only sessions the user still wants
    /// running. One that was killed or that failed keeps its row for a
    /// resume but no longer reserves the hostname.
    pub fn desired_session_forward_by_slug(&self, slug: &str) -> Result<Option<SessionForward>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                &format!(
                    "{FORWARD_SELECT} WHERE f.slug = ?1 AND f.slug != '' AND f.session_id IN \
                     (SELECT id FROM sessions WHERE desired_running = 1)"
                ),
                params![slug],
                row_to_session_forward,
            )
            .optional()?)
    }

    /// Gives up a forward's name while leaving the forward itself. The
    /// unique index over slugs is partial, so an emptied one stops
    /// colliding with the next session to ask for that name.
    pub fn clear_session_forward_slug(&self, id: u64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE session_forwards SET slug = '' WHERE id = ?1",
            params![id as i64],
        )?;
        Ok(())
    }

    /// Names a forward that has no slug, which is how a row published
    /// before slugs existed takes one when its port is republished.
    pub fn set_session_forward_slug(&self, id: u64, slug: &str) -> Result<()> {
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE session_forwards SET slug = ?2 WHERE id = ?1 AND slug = ''",
            params![id as i64, slug],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound("forward", id));
        }
        Ok(())
    }

    pub fn list_session_forwards(&self, session_id: u64) -> Result<Vec<SessionForward>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(&format!(
                "{FORWARD_SELECT} WHERE f.session_id = ?1 ORDER BY f.id"
            ))?
            .query_map(params![session_id as i64], row_to_session_forward)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn all_session_forwards(&self) -> Result<Vec<SessionForward>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(&format!("{FORWARD_SELECT} ORDER BY f.id"))?
            .query_map([], row_to_session_forward)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Forwards whose session the user wants alive, i.e. the set whose
    /// listeners must be bound after a controller restart.
    pub fn desired_session_forwards(&self) -> Result<Vec<SessionForward>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(&format!(
                "{FORWARD_SELECT} WHERE f.session_id IN \
                 (SELECT id FROM sessions WHERE desired_running = 1) ORDER BY f.id"
            ))?
            .query_map([], row_to_session_forward)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn set_session_forward_listener_port(&self, id: u64, listener_port: u16) -> Result<()> {
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE session_forwards SET listener_port = ?2 WHERE id = ?1",
            params![id as i64, listener_port as i64],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound("forward", id));
        }
        Ok(())
    }

    pub fn session_forward(&self, id: u64) -> Result<SessionForward> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{FORWARD_SELECT} WHERE f.id = ?1"),
                params![id as i64],
                row_to_session_forward,
            )
            .optional()?
            .ok_or(StorageError::NotFound("forward", id))
    }

    pub fn delete_session_forward_by_id(&self, id: u64) -> Result<SessionForward> {
        let conn = self.conn.lock().unwrap();
        let forward = conn
            .query_row(
                &format!("{FORWARD_SELECT} WHERE f.id = ?1"),
                params![id as i64],
                row_to_session_forward,
            )
            .optional()?
            .ok_or(StorageError::NotFound("forward", id))?;
        conn.execute(
            "DELETE FROM session_forwards WHERE id = ?1",
            params![id as i64],
        )?;
        Ok(forward)
    }

    /// Creates a share and the forward that carries it. The forward's
    /// port stays zero until a worker binds a server for the share, and
    /// the slug is taken here so a published URL can be returned before
    /// any server exists.
    pub fn create_session_dir_share(
        &self,
        session_id: u64,
        path: &str,
        slug: &str,
        label: &str,
        now_unix_ms: i64,
    ) -> Result<(SessionDirShare, SessionForward)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO session_dir_shares (session_id, path, slug, label, created_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id as i64, path, slug, label, now_unix_ms],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StorageError::Conflict(format!("session {session_id} already shares {path}"))
            }
            e => e.into(),
        })?;
        let share_id = tx.last_insert_rowid() as u64;
        tx.execute(
            "INSERT INTO session_forwards \
             (session_id, worker_port, listener_port, slug, label, scheme, created_at_unix_ms, dir_share_id) \
             VALUES (?1, 0, 0, ?2, ?3, 'http', ?4, ?5)",
            params![session_id as i64, slug, label, now_unix_ms, share_id as i64],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _)
                if f.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                StorageError::Conflict(format!("slug {slug} is taken"))
            }
            e => e.into(),
        })?;
        let forward_id = tx.last_insert_rowid() as u64;
        tx.commit()?;
        drop(conn);
        Ok((
            self.session_dir_share(share_id)?,
            self.session_forward(forward_id)?,
        ))
    }

    pub fn session_dir_share(&self, id: u64) -> Result<SessionDirShare> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{DIR_SHARE_SELECT} WHERE id = ?1"),
                params![id as i64],
                row_to_dir_share,
            )
            .optional()?
            .ok_or(StorageError::NotFound("directory share", id))
    }

    pub fn list_session_dir_shares(&self, session_id: u64) -> Result<Vec<SessionDirShare>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(&format!(
                "{DIR_SHARE_SELECT} WHERE session_id = ?1 ORDER BY id"
            ))?
            .query_map(params![session_id as i64], row_to_dir_share)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every share the controller wants served on one worker: the shares
    /// of that worker's sessions the user wants running. This is the
    /// desired state a reconnecting worker is reconciled against.
    pub fn desired_dir_shares_on_worker(&self, worker_id: u64) -> Result<Vec<SessionDirShare>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(&format!(
                "{DIR_SHARE_SELECT} WHERE session_id IN \
                 (SELECT id FROM sessions WHERE worker_id = ?1 AND desired_running = 1) \
                 ORDER BY id"
            ))?
            .query_map(params![worker_id as i64], row_to_dir_share)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn session_dir_share_by_slug(&self, slug: &str) -> Result<Option<SessionDirShare>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{DIR_SHARE_SELECT} WHERE slug = ?1"),
                params![slug],
                row_to_dir_share,
            )
            .optional()?)
    }

    /// The share a forward carries, if it carries one. A forward with
    /// no share is an ordinary published port.
    pub fn dir_share_of_forward(&self, forward_id: u64) -> Result<Option<SessionDirShare>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                &format!(
                    "{DIR_SHARE_SELECT} WHERE id = \
                     (SELECT dir_share_id FROM session_forwards WHERE id = ?1)"
                ),
                params![forward_id as i64],
                row_to_dir_share,
            )
            .optional()?)
    }

    /// The forward carrying a share, which is where its id, slug and
    /// public URL live.
    pub fn dir_share_forward(&self, share_id: u64) -> Result<SessionForward> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!("{FORWARD_SELECT} WHERE f.dir_share_id = ?1"),
                params![share_id as i64],
                row_to_session_forward,
            )
            .optional()?
            .ok_or(StorageError::NotFound("directory share forward", share_id))
    }

    /// Moves a share's forward to the port its server currently answers
    /// on. The forward id is in URLs already handed out, so a rebind
    /// updates the row rather than replacing it.
    pub fn set_session_forward_worker_port(&self, id: u64, worker_port: u16) -> Result<()> {
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE session_forwards SET worker_port = ?2 WHERE id = ?1",
            params![id as i64, worker_port as i64],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound("forward", id));
        }
        Ok(())
    }

    /// Deletes a share, and with it the forward that carried it.
    pub fn delete_session_dir_share(&self, id: u64) -> Result<SessionDirShare> {
        let conn = self.conn.lock().unwrap();
        let share = conn
            .query_row(
                &format!("{DIR_SHARE_SELECT} WHERE id = ?1"),
                params![id as i64],
                row_to_dir_share,
            )
            .optional()?
            .ok_or(StorageError::NotFound("directory share", id))?;
        conn.execute(
            "DELETE FROM session_dir_shares WHERE id = ?1",
            params![id as i64],
        )?;
        Ok(share)
    }

    pub fn delete_session_forward(&self, session_id: u64, worker_port: u16) -> Result<()> {
        let changed = self.conn.lock().unwrap().execute(
            "DELETE FROM session_forwards WHERE session_id = ?1 AND worker_port = ?2",
            params![session_id as i64, worker_port as i64],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound("forward", session_id));
        }
        Ok(())
    }

    /// Creates or updates one item. `actor_session_id` is `Some` for
    /// agent writes, which are subject to the sticky rules: a closed
    /// (done/dropped) item's status can only change with a note saying
    /// why, and snooze is untouchable. Human writes (`None`) bypass
    /// them — it is the user's own board.
    pub fn upsert_item(
        &self,
        bucket_id: u64,
        up: &ItemUpsert,
        actor_session_id: Option<u64>,
        now: i64,
    ) -> Result<(Item, ItemOutcome)> {
        self.upsert_item_with_project_default(bucket_id, up, actor_session_id, None, now)
    }

    pub(crate) fn upsert_item_with_project_default(
        &self,
        bucket_id: u64,
        up: &ItemUpsert,
        actor_session_id: Option<u64>,
        create_project_id: Option<u64>,
        now: i64,
    ) -> Result<(Item, ItemOutcome)> {
        validate_item_body(up.body.as_deref())?;
        let (item_number, outcome): (u64, ItemOutcome) = {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            tx.query_row(
                "SELECT 1 FROM buckets WHERE id = ?1",
                params![bucket_id as i64],
                |_| Ok(()),
            )
            .optional()?
            .ok_or(StorageError::NotFound("bucket", bucket_id))?;

            if let Some(session_id) = actor_session_id {
                ensure_session_in_bucket(&tx, bucket_id, session_id)?;
            }

            if let Some(project_id) = up.project_id {
                let in_bucket: Option<i64> = tx
                    .query_row(
                        "SELECT bucket_id FROM projects WHERE id = ?1",
                        params![project_id as i64],
                        |r| r.get(0),
                    )
                    .optional()?;
                if in_bucket != Some(bucket_id as i64) {
                    return Err(StorageError::Conflict(format!(
                        "project {project_id} is not in this bucket"
                    )));
                }
            }

            let existing = match (up.id, &up.external_key) {
                (Some(id), _) => {
                    let internal_id = internal_item_id(&tx, bucket_id, id)?;
                    Some((
                        internal_id,
                        load_item(&tx, bucket_id, id)?.ok_or(StorageError::NotFound("item", id))?,
                    ))
                }
                (None, Some(key)) => {
                    let id: Option<i64> = tx
                        .query_row(
                            "SELECT id FROM items WHERE bucket_id = ?1 AND external_key = ?2",
                            params![bucket_id as i64, key],
                            |r| r.get(0),
                        )
                        .optional()?;
                    match id {
                        Some(id) => {
                            load_item_by_internal_id(&tx, id as u64)?.map(|item| (id as u64, item))
                        }
                        None => None,
                    }
                }
                (None, None) => None,
            };

            let note = up.note.as_deref().map(str::trim).unwrap_or_default();
            let result = match existing {
                None => {
                    let title = up
                        .title
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .ok_or_else(|| {
                            StorageError::Conflict("title is required to create an item".into())
                        })?;
                    let status = up.status.unwrap_or(ItemStatus::Inbox);
                    let source_kind = up.source_kind.unwrap_or(if actor_session_id.is_some() {
                        ItemSourceKind::Agent
                    } else {
                        ItemSourceKind::Human
                    });
                    let project_id = up.project_id.or(create_project_id);
                    let item_number: u64 = tx.query_row(
                        "INSERT INTO bucket_item_sequences (bucket_id, next_item_number) VALUES (?1, 2) \
                         ON CONFLICT(bucket_id) DO UPDATE SET next_item_number = next_item_number + 1 \
                         RETURNING next_item_number - 1",
                        params![bucket_id as i64],
                        |row| Ok(row.get::<_, i64>(0)? as u64),
                    )?;
                    tx.execute(
                        "INSERT INTO items (bucket_id, item_number, project_id, external_key, title, body, \
                         status, priority, source_kind, source_detail, url, due_at_unix_ms, \
                         created_by_session_id, created_at_unix_ms, updated_at_unix_ms, \
                         done_at_unix_ms, question) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15, ?16)",
                        params![
                            bucket_id as i64,
                            item_number as i64,
                            project_id.map(|p| p as i64),
                            up.external_key,
                            title,
                            up.body.as_deref().unwrap_or_default(),
                            status.as_str(),
                            up.priority.unwrap_or(ItemPriority::Normal).as_str(),
                            source_kind.as_str(),
                            up.source_detail.as_deref().unwrap_or_default(),
                            up.url.as_deref().unwrap_or_default(),
                            up.due_at_unix_ms,
                            actor_session_id.map(|s| s as i64),
                            now,
                            (status == ItemStatus::Done).then_some(now),
                            up.question.as_deref().unwrap_or_default(),
                        ],
                    )
                    .map_err(conflict_on_constraint(|| {
                        format!(
                            "external key {:?} already exists in this bucket",
                            up.external_key.as_deref().unwrap_or_default()
                        )
                    }))?;
                    let internal_id = tx.last_insert_rowid() as u64;
                    replace_item_deps(
                        &tx,
                        bucket_id,
                        internal_id,
                        item_number,
                        up.blocked_by.as_deref().unwrap_or(&[]),
                    )?;
                    add_item_note(&tx, internal_id, actor_session_id, now, "created", title)?;
                    if up.question.as_deref().is_some_and(|q| !q.is_empty()) {
                        add_item_note(
                            &tx,
                            internal_id,
                            actor_session_id,
                            now,
                            "note",
                            up.question.as_deref().unwrap_or_default(),
                        )?;
                    }
                    if !note.is_empty() {
                        add_item_note(&tx, internal_id, actor_session_id, now, "note", note)?;
                    }
                    (internal_id, item_number, ItemOutcome::Created)
                }
                Some((internal_id, old)) => {
                    let agent_actor = actor_session_id.is_some();
                    let status = up.status.unwrap_or(old.status);
                    if agent_actor
                        && old.status.is_closed()
                        && status != old.status
                        && note.is_empty()
                    {
                        return Err(StorageError::Conflict(format!(
                            "item {} is {}; include a note explaining why its status changes",
                            old.id,
                            old.status.as_str()
                        )));
                    }
                    let title = up
                        .title
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .unwrap_or(&old.title);
                    let body = up.body.as_deref().unwrap_or(&old.body);
                    let question = up.question.as_deref().unwrap_or(&old.question);
                    let priority = up.priority.unwrap_or(old.priority);
                    let source_kind = up.source_kind.unwrap_or(old.source_kind);
                    let source_detail = up.source_detail.as_deref().unwrap_or(&old.source_detail);
                    let url = up.url.as_deref().unwrap_or(&old.url);
                    let external_key = up.external_key.as_deref().or(old.external_key.as_deref());
                    let project_id = if up.clear_project {
                        None
                    } else {
                        up.project_id.or(old.project_id)
                    };
                    let due = if up.clear_due {
                        None
                    } else {
                        up.due_at_unix_ms.or(old.due_at_unix_ms)
                    };
                    let done_at = match (old.status, status) {
                        (a, b) if a == b => old.done_at_unix_ms,
                        (_, ItemStatus::Done) => Some(now),
                        (ItemStatus::Done, _) => None,
                        _ => old.done_at_unix_ms,
                    };

                    let deps_changed = up
                        .blocked_by
                        .as_ref()
                        .is_some_and(|deps| sorted(deps) != sorted(&old.blocked_by));
                    let fields_changed = title != old.title
                        || body != old.body
                        || question != old.question
                        || status != old.status
                        || priority != old.priority
                        || source_kind != old.source_kind
                        || source_detail != old.source_detail
                        || url != old.url
                        || external_key != old.external_key.as_deref()
                        || project_id != old.project_id
                        || due != old.due_at_unix_ms;
                    let link_changed = up
                        .link_session_id
                        .is_some_and(|s| !old.session_ids.contains(&s));

                    if fields_changed {
                        tx.execute(
                            "UPDATE items SET project_id = ?2, external_key = ?3, title = ?4, \
                             body = ?5, status = ?6, priority = ?7, source_kind = ?8, \
                             source_detail = ?9, url = ?10, due_at_unix_ms = ?11, \
                             updated_at_unix_ms = ?12, done_at_unix_ms = ?13, \
                             question = ?14 WHERE id = ?1",
                            params![
                                internal_id as i64,
                                project_id.map(|p| p as i64),
                                external_key,
                                title,
                                body,
                                status.as_str(),
                                priority.as_str(),
                                source_kind.as_str(),
                                source_detail,
                                url,
                                due,
                                now,
                                done_at,
                                question,
                            ],
                        )
                        .map_err(conflict_on_constraint(|| {
                            format!("external key {external_key:?} already exists in this bucket")
                        }))?;
                    } else if deps_changed || !note.is_empty() || link_changed {
                        tx.execute(
                            "UPDATE items SET updated_at_unix_ms = ?2 WHERE id = ?1",
                            params![internal_id as i64, now],
                        )?;
                    }
                    if deps_changed {
                        replace_item_deps(
                            &tx,
                            bucket_id,
                            internal_id,
                            old.id,
                            up.blocked_by.as_deref().unwrap_or(&[]),
                        )?;
                    }
                    if status != old.status {
                        add_item_note(
                            &tx,
                            internal_id,
                            actor_session_id,
                            now,
                            "status",
                            &format!("{} -> {}", old.status.as_str(), status.as_str()),
                        )?;
                    }
                    if question != old.question && !question.is_empty() {
                        add_item_note(&tx, internal_id, actor_session_id, now, "note", question)?;
                    }
                    if !note.is_empty() {
                        add_item_note(&tx, internal_id, actor_session_id, now, "note", note)?;
                    }
                    if fields_changed || deps_changed || !note.is_empty() || link_changed {
                        (internal_id, old.id, ItemOutcome::Updated)
                    } else {
                        (internal_id, old.id, ItemOutcome::Unchanged)
                    }
                }
            };
            if let Some(session_id) = up.link_session_id {
                link_item_session(&tx, bucket_id, result.0, session_id)?;
            }
            tx.commit()?;
            (result.1, result.2)
        };
        Ok((self.get_item(bucket_id, item_number)?, outcome))
    }

    pub fn respond_to_item(
        &self,
        bucket_id: u64,
        item_id: u64,
        text: &str,
        routed_to_session: bool,
        now: i64,
    ) -> Result<Item> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let internal_id = internal_item_id(&tx, bucket_id, item_id)?;
        let old =
            load_item(&tx, bucket_id, item_id)?.ok_or(StorageError::NotFound("item", item_id))?;
        let status = if routed_to_session && old.status == ItemStatus::Blocked {
            ItemStatus::InProgress
        } else {
            old.status
        };
        tx.execute(
            "UPDATE items SET question = '', status = ?2, updated_at_unix_ms = ?3 \
             WHERE id = ?1",
            params![internal_id as i64, status.as_str(), now],
        )?;
        add_item_note(&tx, internal_id, None, now, "user_reply", text)?;
        if status != old.status {
            add_item_note(
                &tx,
                internal_id,
                None,
                now,
                "status",
                &format!("{} -> {}", old.status.as_str(), status.as_str()),
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.get_item(bucket_id, item_id)
    }

    pub fn get_item(&self, bucket_id: u64, id: u64) -> Result<Item> {
        let conn = self.conn.lock().unwrap();
        load_item(&conn, bucket_id, id)?.ok_or(StorageError::NotFound("item", id))
    }

    pub fn item_id_by_external_key(&self, bucket_id: u64, key: &str) -> Result<Option<u64>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT item_number FROM items WHERE bucket_id = ?1 AND external_key = ?2",
                params![bucket_id as i64, key],
                |r| Ok(r.get::<_, i64>(0)? as u64),
            )
            .optional()?)
    }

    pub fn project_by_name(&self, bucket_id: u64, name: &str) -> Result<Option<Project>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id, bucket_id, name, path, permission_mode, worker_id, default_agent, model_profile_id FROM projects \
                 WHERE bucket_id = ?1 AND name = ?2",
                params![bucket_id as i64, name],
                row_to_project,
            )
            .optional()?)
    }

    pub fn project_names(&self, bucket_id: u64) -> Result<Vec<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare("SELECT name FROM projects WHERE bucket_id = ?1 ORDER BY name")?
            .query_map(params![bucket_id as i64], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Records that a session works an item, without any other change.
    pub fn link_item_session(&self, bucket_id: u64, item_id: u64, session_id: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let internal_id = internal_item_id(&conn, bucket_id, item_id)?;
        link_item_session(&conn, bucket_id, internal_id, session_id)
    }

    pub fn delete_item(&self, bucket_id: u64, id: u64) -> Result<Item> {
        let item = self.get_item(bucket_id, id)?;
        let conn = self.conn.lock().unwrap();
        let internal_id = internal_item_id(&conn, bucket_id, id)?;
        conn.execute(
            "DELETE FROM items WHERE id = ?1",
            params![internal_id as i64],
        )?;
        Ok(item)
    }

    /// Stores one independently referenced attachment and its activity note in one
    /// transaction. Equal digests are intentionally allowed: uploads are historical item
    /// actions, while the digest index makes duplicate detection cheap for callers.
    #[allow(clippy::too_many_arguments)]
    pub fn create_item_attachment(
        &self,
        bucket_id: u64,
        item_id: u64,
        filename: &str,
        media_type: &str,
        content: &[u8],
        sha256: &[u8; 32],
        actor_session_id: Option<u64>,
        now: i64,
    ) -> Result<ItemAttachment> {
        let filename_chars = filename.chars().count();
        if filename.trim().is_empty() || filename_chars > ITEM_ATTACHMENT_FILENAME_MAX {
            return Err(StorageError::Validation {
                field: "filename",
                limit: ITEM_ATTACHMENT_FILENAME_MAX,
                actual: filename_chars,
                unit: "Unicode scalar values",
            });
        }
        if content.len() > ITEM_ATTACHMENT_FILE_MAX {
            return Err(StorageError::Validation {
                field: "attachment",
                limit: ITEM_ATTACHMENT_FILE_MAX,
                actual: content.len(),
                unit: "bytes",
            });
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let internal_id = internal_item_id(&tx, bucket_id, item_id)?;
        if let Some(session_id) = actor_session_id {
            ensure_session_in_bucket(&tx, bucket_id, session_id)?;
        }
        let existing: i64 = tx.query_row(
            "SELECT COALESCE(SUM(byte_length), 0) FROM item_attachments WHERE item_id = ?1",
            params![internal_id as i64],
            |row| row.get(0),
        )?;
        checked_attachment_total(existing as usize, content.len())?;
        tx.execute(
            "INSERT INTO item_attachments (item_id, filename, media_type, byte_length, sha256, content, created_at_unix_ms, created_by_session_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                internal_id as i64,
                filename,
                media_type,
                content.len() as i64,
                sha256.as_slice(),
                content,
                now,
                actor_session_id.map(|id| id as i64),
            ],
        )?;
        let id = tx.last_insert_rowid() as u64;
        add_item_note(
            &tx,
            internal_id,
            actor_session_id,
            now,
            "note",
            &format!("attached {filename} ({} bytes)", content.len()),
        )?;
        tx.execute(
            "UPDATE items SET updated_at_unix_ms = ?2 WHERE id = ?1",
            params![internal_id as i64, now],
        )?;
        tx.commit()?;
        Ok(ItemAttachment {
            id,
            bucket_id,
            item_id,
            filename: filename.to_owned(),
            media_type: media_type.to_owned(),
            byte_length: content.len() as u64,
            sha256: hex::encode(sha256),
            created_at_unix_ms: now,
            created_by_session_id: actor_session_id,
        })
    }

    pub fn list_item_attachments(
        &self,
        bucket_id: u64,
        item_id: u64,
    ) -> Result<Vec<ItemAttachment>> {
        let conn = self.conn.lock().unwrap();
        let internal_id = internal_item_id(&conn, bucket_id, item_id)?;
        let mut statement = conn.prepare(
            "SELECT id, filename, media_type, byte_length, sha256, created_at_unix_ms, created_by_session_id \
             FROM item_attachments WHERE item_id = ?1 ORDER BY created_at_unix_ms, id",
        )?;
        let attachments = statement
            .query_map(params![internal_id as i64], |row| {
                let digest: Vec<u8> = row.get(4)?;
                Ok(ItemAttachment {
                    id: row.get::<_, i64>(0)? as u64,
                    bucket_id,
                    item_id,
                    filename: row.get(1)?,
                    media_type: row.get(2)?,
                    byte_length: row.get::<_, i64>(3)? as u64,
                    sha256: hex::encode(digest),
                    created_at_unix_ms: row.get(5)?,
                    created_by_session_id: row.get::<_, Option<i64>>(6)?.map(|id| id as u64),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(attachments)
    }

    pub fn get_item_attachment(&self, attachment_id: u64) -> Result<ItemAttachmentContent> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT item.bucket_id, item.item_number, attachment.filename, attachment.media_type, \
                        attachment.byte_length, attachment.sha256, attachment.content, \
                        attachment.created_at_unix_ms, attachment.created_by_session_id \
                 FROM item_attachments attachment JOIN items item ON item.id = attachment.item_id \
                 WHERE attachment.id = ?1",
                params![attachment_id as i64],
                |row| {
                    let bucket_id = row.get::<_, i64>(0)? as u64;
                    let item_id = row.get::<_, i64>(1)? as u64;
                    let digest: Vec<u8> = row.get(5)?;
                    Ok(ItemAttachmentContent {
                        metadata: ItemAttachment {
                            id: attachment_id,
                            bucket_id,
                            item_id,
                            filename: row.get(2)?,
                            media_type: row.get(3)?,
                            byte_length: row.get::<_, i64>(4)? as u64,
                            sha256: hex::encode(digest),
                            created_at_unix_ms: row.get(7)?,
                            created_by_session_id: row
                                .get::<_, Option<i64>>(8)?
                                .map(|id| id as u64),
                        },
                        content: row.get(6)?,
                    })
                },
            )
            .optional()?
            .ok_or(StorageError::NotFound("attachment", attachment_id))
    }

    pub fn delete_item_attachment(
        &self,
        bucket_id: u64,
        item_id: u64,
        attachment_id: u64,
        actor_session_id: Option<u64>,
        now: i64,
    ) -> Result<ItemAttachment> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let internal_id = internal_item_id(&tx, bucket_id, item_id)?;
        let content = tx
            .query_row(
                "SELECT filename, media_type, byte_length, sha256, created_at_unix_ms, created_by_session_id \
                 FROM item_attachments WHERE id = ?1 AND item_id = ?2",
                params![attachment_id as i64, internal_id as i64],
                |row| {
                    let digest: Vec<u8> = row.get(3)?;
                    Ok(ItemAttachment {
                        id: attachment_id,
                        bucket_id,
                        item_id,
                        filename: row.get(0)?,
                        media_type: row.get(1)?,
                        byte_length: row.get::<_, i64>(2)? as u64,
                        sha256: hex::encode(digest),
                        created_at_unix_ms: row.get(4)?,
                        created_by_session_id: row
                            .get::<_, Option<i64>>(5)?
                            .map(|id| id as u64),
                    })
                },
            )
            .optional()?
            .ok_or(StorageError::NotFound("attachment", attachment_id))?;
        tx.execute(
            "DELETE FROM item_attachments WHERE id = ?1 AND item_id = ?2",
            params![attachment_id as i64, internal_id as i64],
        )?;
        add_item_note(
            &tx,
            internal_id,
            actor_session_id,
            now,
            "note",
            &format!("removed attachment {}", content.filename),
        )?;
        tx.execute(
            "UPDATE items SET updated_at_unix_ms = ?2 WHERE id = ?1",
            params![internal_id as i64, now],
        )?;
        tx.commit()?;
        Ok(content)
    }

    /// Parks an item until a time (`None` clears). Human-only by
    /// construction: nothing on the MCP path reaches this.
    pub fn snooze_item(
        &self,
        bucket_id: u64,
        id: u64,
        until_unix_ms: Option<i64>,
        now: i64,
    ) -> Result<Item> {
        let conn = self.conn.lock().unwrap();
        let internal_id = internal_item_id(&conn, bucket_id, id)?;
        let n = conn.execute(
            "UPDATE items SET snoozed_until_unix_ms = ?2, updated_at_unix_ms = ?3 WHERE id = ?1",
            params![internal_id as i64, until_unix_ms, now],
        )?;
        if n == 0 {
            return Err(StorageError::NotFound("item", id));
        }
        drop(conn);
        self.get_item(bucket_id, id)
    }

    fn append_item_query_filters(
        sql: &mut String,
        args: &mut Vec<rusqlite::types::Value>,
        q: &ItemQuery,
        now: i64,
    ) -> Option<usize> {
        if !q.statuses.is_empty() {
            let list = q
                .statuses
                .iter()
                .map(|s| format!("'{}'", s.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND status IN ({list})"));
        } else if !q.include_closed && q.summary_filter != Some(ItemSummaryFilter::DoneRecently) {
            sql.push_str(" AND status NOT IN ('done','dropped')");
        }
        match q.summary_filter {
            Some(ItemSummaryFilter::NeedsYou) => {
                sql.push_str(" AND (question <> '' OR status IN ('inbox','blocked'))");
            }
            Some(ItemSummaryFilter::InProgress) => sql.push_str(" AND status = 'in_progress'"),
            Some(ItemSummaryFilter::Planned) => sql.push_str(" AND status = 'planned'"),
            Some(ItemSummaryFilter::BlockedExternal) => {
                sql.push_str(" AND status = 'blocked_external'");
            }
            Some(ItemSummaryFilter::DoneRecently) => {
                args.push((now - ITEM_SNAPSHOT_CLOSED_WINDOW_MS).into());
                sql.push_str(&format!(
                    " AND status = 'done' AND updated_at_unix_ms >= ?{}",
                    args.len()
                ));
            }
            Some(ItemSummaryFilter::LiveLinked) => {
                sql.push_str(
                    " AND EXISTS (SELECT 1 FROM item_sessions linked \
                     JOIN sessions live ON live.id = linked.session_id \
                     WHERE linked.item_id = items.id \
                     AND (live.desired_running = 1 OR live.state NOT IN ('exited','failed')))",
                );
            }
            None => {}
        }
        if let Some(project_id) = q.project_id {
            args.push((project_id as i64).into());
            sql.push_str(&format!(" AND project_id = ?{}", args.len()));
        }
        if !q.priorities.is_empty() {
            let list = q
                .priorities
                .iter()
                .map(|p| format!("'{}'", p.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND priority IN ({list})"));
        }
        if !q.source_kinds.is_empty() {
            let list = q
                .source_kinds
                .iter()
                .map(|s| format!("'{}'", s.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND source_kind IN ({list})"));
        }
        if let Some(since) = q.updated_since_unix_ms {
            args.push(since.into());
            sql.push_str(&format!(" AND updated_at_unix_ms >= ?{}", args.len()));
        }
        if !q.include_snoozed {
            args.push(now.into());
            sql.push_str(&format!(
                " AND (snoozed_until_unix_ms IS NULL OR snoozed_until_unix_ms <= ?{})",
                args.len()
            ));
        }
        let search_index = q.search.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(|search| {
            let search = search.chars().take(200).collect::<String>().to_lowercase();
            args.push(search.into());
            let index = args.len();
            sql.push_str(&format!(
                " AND (instr(lower(title), ?{index}) > 0 OR instr(lower(body), ?{index}) > 0 \
                 OR instr(lower(question), ?{index}) > 0 OR instr(lower(COALESCE(external_key, '')), ?{index}) > 0 \
                 OR instr(lower(source_detail), ?{index}) > 0 OR instr(lower(url), ?{index}) > 0)"
            ));
            index
        });
        search_index
    }

    pub fn list_items(&self, q: &ItemQuery, now: i64) -> Result<Vec<Item>> {
        let conn = self.conn.lock().unwrap();
        Self::list_items_on(&conn, q, now)
    }

    fn list_items_on(conn: &Connection, q: &ItemQuery, now: i64) -> Result<Vec<Item>> {
        let mut sql = format!("{ITEM_SELECT} WHERE bucket_id = ?1");
        let mut args: Vec<rusqlite::types::Value> = vec![(q.bucket_id as i64).into()];
        let search_index = Self::append_item_query_filters(&mut sql, &mut args, q, now);
        let closed_last = q.include_closed
            && q.statuses.is_empty()
            && search_index.is_none()
            && q.summary_filter != Some(ItemSummaryFilter::DoneRecently);
        if let Some(index) = search_index {
            sql.push_str(&format!(
                " ORDER BY CASE WHEN lower(title) = ?{index} THEN 0 WHEN instr(lower(title), ?{index}) = 1 THEN 1 \
                 WHEN instr(lower(title), ?{index}) > 0 THEN 2 WHEN instr(lower(question), ?{index}) > 0 THEN 3 \
                 WHEN instr(lower(body), ?{index}) > 0 THEN 4 WHEN instr(lower(COALESCE(external_key, '')), ?{index}) > 0 THEN 5 \
                 WHEN instr(lower(source_detail), ?{index}) > 0 THEN 6 ELSE 7 END, \
                 CASE priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END, \
                 due_at_unix_ms IS NULL, due_at_unix_ms, updated_at_unix_ms DESC, id DESC"
            ));
        } else {
            sql.push_str(" ORDER BY ");
            if closed_last {
                // Ordinary Board pages keep actionable rows ahead of collapsed
                // history. Counts still cover the full bounded query, and an
                // explicit completed/search view retains its natural ranking.
                sql.push_str("CASE WHEN status IN ('done','dropped') THEN 1 ELSE 0 END, ");
            }
            sql.push_str(
                "CASE priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END, \
                 due_at_unix_ms IS NULL, due_at_unix_ms, updated_at_unix_ms DESC, id DESC",
            );
        }
        if let Some(limit) = q.limit {
            let limit = limit.clamp(1, ITEM_QUERY_LIMIT_MAX);
            args.push(i64::from(limit).into());
            sql.push_str(&format!(" LIMIT ?{}", args.len()));
            args.push(i64::from(q.offset).into());
            sql.push_str(&format!(" OFFSET ?{}", args.len()));
        }
        let mut items = conn
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(args), row_to_item)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        attach_item_links(conn, &mut items)?;
        Ok(items)
    }

    pub fn item_query_counts(&self, q: &ItemQuery, now: i64) -> Result<ItemQueryCounts> {
        let conn = self.conn.lock().unwrap();
        Self::item_query_counts_on(&conn, q, now)
    }

    fn item_query_counts_on(conn: &Connection, q: &ItemQuery, now: i64) -> Result<ItemQueryCounts> {
        let bucket_total = conn.query_row(
            "SELECT COUNT(*) FROM items WHERE bucket_id = ?1",
            params![q.bucket_id as i64],
            |row| row.get::<_, i64>(0),
        )? as u64;

        let mut sql = "SELECT status, COUNT(*) FROM items WHERE bucket_id = ?1".to_owned();
        let mut args: Vec<rusqlite::types::Value> = vec![(q.bucket_id as i64).into()];
        Self::append_item_query_filters(&mut sql, &mut args, q, now);
        sql.push_str(" GROUP BY status");
        let status_counts = conn
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(args), |row| {
                let status = row.get::<_, String>(0)?;
                let count = row.get::<_, i64>(1)? as u64;
                Ok((
                    ItemStatus::parse(&status).expect("stored item status is valid"),
                    count,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let matching_total = status_counts.iter().map(|(_, count)| count).sum();
        Ok(ItemQueryCounts {
            bucket_total,
            matching_total,
            status_counts,
        })
    }

    /// Returns one bounded page and its facets from the same database view.
    /// This prevents a concurrent status mutation from pairing stale rows with
    /// fresh completed counts (or vice versa) in the Board HTTP response.
    pub fn list_items_with_counts(
        &self,
        q: &ItemQuery,
        now: i64,
    ) -> Result<(Vec<Item>, ItemQueryCounts)> {
        let conn = self.conn.lock().unwrap();
        let items = Self::list_items_on(&conn, q, now)?;
        let counts = Self::item_query_counts_on(&conn, q, now)?;
        Ok((items, counts))
    }

    pub fn item_notes(&self, bucket_id: u64, item_id: u64) -> Result<Vec<ItemNote>> {
        let conn = self.conn.lock().unwrap();
        let internal_id = internal_item_id(&conn, bucket_id, item_id)?;
        let notes = conn
            .prepare(
                "SELECT id, session_id, ts_unix_ms, kind, text FROM item_notes \
                 WHERE item_id = ?1 ORDER BY id",
            )?
            .query_map(params![internal_id as i64], |r| {
                Ok(ItemNote {
                    id: r.get::<_, i64>(0)? as u64,
                    session_id: r.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                    ts_unix_ms: r.get(2)?,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(notes)
    }

    pub fn create_briefing(
        &self,
        bucket_id: u64,
        session_id: Option<u64>,
        markdown: &str,
        now: i64,
    ) -> Result<BucketBriefing> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT 1 FROM buckets WHERE id = ?1",
            params![bucket_id as i64],
            |_| Ok(()),
        )
        .optional()?
        .ok_or(StorageError::NotFound("bucket", bucket_id))?;
        if let Some(session_id) = session_id {
            ensure_session_in_bucket(&conn, bucket_id, session_id)?;
        }
        conn.execute(
            "INSERT INTO bucket_briefings (bucket_id, session_id, ts_unix_ms, markdown) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                bucket_id as i64,
                session_id.map(|s| s as i64),
                now,
                markdown
            ],
        )?;
        Ok(BucketBriefing {
            id: conn.last_insert_rowid() as u64,
            bucket_id,
            session_id,
            ts_unix_ms: now,
            markdown: markdown.to_string(),
        })
    }

    /// A bucket's briefings, newest first.
    pub fn list_briefings(&self, bucket_id: u64, limit: usize) -> Result<Vec<BucketBriefing>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .prepare(
                "SELECT id, bucket_id, session_id, ts_unix_ms, markdown FROM bucket_briefings \
                 WHERE bucket_id = ?1 ORDER BY id DESC LIMIT ?2",
            )?
            .query_map(params![bucket_id as i64, limit as i64], row_to_briefing)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Stores `value` only if the key is absent, then returns the
    /// stored value. One mutex-held statement pair, so concurrent
    /// callers agree on a single winner.
    pub fn ensure_setting(&self, key: &str, value: &str) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO NOTHING",
            params![key, value],
        )?;
        Ok(conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )?)
    }

    /// Writes one setting; `None` deletes the row so the built-in
    /// default applies again.
    /// Rebuilds the database so no freed page keeps what was deleted from it,
    /// and then checkpoints so the rebuilt pages reach the database file.
    ///
    /// The checkpoint is not optional. Under WAL journaling a VACUUM writes the
    /// rebuilt database into the write-ahead log, and the main file keeps the
    /// pages it had until a checkpoint copies over them, so a VACUUM on its own
    /// leaves the old bytes exactly where somebody copying the database would
    /// find them.
    pub fn rebuild_erasing_freed_pages(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("VACUUM;")?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Bytes the database occupies on disk, for saying how long a rebuild of it
    /// is likely to take before starting one.
    pub fn size_on_disk(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let pages: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        Ok((pages.max(0) as u64) * (page_size.max(0) as u64))
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        match value {
            Some(value) => {
                conn.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, value],
                )?;
            }
            None => {
                conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
            }
        }
        Ok(())
    }

    pub fn instruction_layers(
        &self,
        bucket_id: u64,
        project_id: Option<u64>,
    ) -> Result<Vec<InstructionLayer>> {
        let conn = self.conn.lock().unwrap();
        let sql = if project_id.is_some() {
            "SELECT id,bucket_id,project_id,target,markdown,revision,updated_at_unix_ms,updated_by_session_id FROM instruction_layers WHERE bucket_id=?1 AND (project_id IS NULL OR project_id=?2) ORDER BY CASE WHEN project_id IS NULL THEN 0 ELSE 1 END, CASE target WHEN 'all' THEN 0 WHEN 'worker' THEN 1 ELSE 2 END"
        } else {
            "SELECT id,bucket_id,project_id,target,markdown,revision,updated_at_unix_ms,updated_by_session_id FROM instruction_layers WHERE bucket_id=?1 AND project_id IS NULL ORDER BY CASE target WHEN 'all' THEN 0 WHEN 'worker' THEN 1 ELSE 2 END"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = match project_id {
            Some(project_id) => stmt
                .query_map(
                    params![bucket_id as i64, project_id as i64],
                    row_to_instruction_layer,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt
                .query_map(params![bucket_id as i64], row_to_instruction_layer)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    pub fn all_instruction_layers(&self) -> Result<Vec<InstructionLayer>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id,bucket_id,project_id,target,markdown,revision,updated_at_unix_ms,updated_by_session_id FROM instruction_layers ORDER BY bucket_id, COALESCE(project_id,0), target")?;
        let rows = stmt
            .query_map([], row_to_instruction_layer)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_instruction_layer(
        &self,
        bucket_id: u64,
        project_id: Option<u64>,
        target: InstructionTarget,
        markdown: &str,
        expected_revision: u64,
        note: &str,
        updated_by_session_id: Option<u64>,
        now: i64,
    ) -> Result<InstructionLayer> {
        if markdown.chars().count() > INSTRUCTION_MARKDOWN_MAX {
            return Err(StorageError::Conflict(format!(
                "instruction markdown exceeds {INSTRUCTION_MARKDOWN_MAX} characters"
            )));
        }
        if let Some(project_id) = project_id {
            let project = self.get_project(project_id)?;
            if project.bucket_id != bucket_id {
                return Err(StorageError::Conflict(format!(
                    "project {project_id} is not in bucket {bucket_id}"
                )));
            }
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existing: Option<(u64,u64)> = tx.query_row("SELECT id,revision FROM instruction_layers WHERE bucket_id=?1 AND project_id IS ?2 AND target=?3", params![bucket_id as i64, project_id.map(|v|v as i64), target.as_str()], |r| Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64))).optional()?;
        let (id, revision) = match existing {
            Some((id, current)) => {
                if current != expected_revision {
                    return Err(StorageError::Conflict(format!("instruction revision conflict: expected {expected_revision}, current {current}")));
                }
                let next = current + 1;
                tx.execute("UPDATE instruction_layers SET markdown=?2,revision=?3,updated_at_unix_ms=?4,updated_by_session_id=?5 WHERE id=?1", params![id as i64,markdown,next as i64,now,updated_by_session_id.map(|v|v as i64)])?;
                (id, next)
            }
            None => {
                if expected_revision != 0 {
                    return Err(StorageError::Conflict(format!(
                        "instruction revision conflict: expected {expected_revision}, current 0"
                    )));
                }
                tx.execute("INSERT INTO instruction_layers(bucket_id,project_id,target,markdown,revision,updated_at_unix_ms,updated_by_session_id) VALUES(?1,?2,?3,?4,1,?5,?6)", params![bucket_id as i64,project_id.map(|v|v as i64),target.as_str(),markdown,now,updated_by_session_id.map(|v|v as i64)])?;
                (tx.last_insert_rowid() as u64, 1)
            }
        };
        tx.execute("INSERT INTO instruction_revisions(layer_id,revision,markdown,note,updated_at_unix_ms,updated_by_session_id) VALUES(?1,?2,?3,?4,?5,?6)", params![id as i64,revision as i64,markdown,note,now,updated_by_session_id.map(|v|v as i64)])?;
        tx.commit()?;
        drop(conn);
        self.get_instruction_layer(id)
    }

    pub fn get_instruction_layer(&self, id: u64) -> Result<InstructionLayer> {
        self.conn.lock().unwrap().query_row("SELECT id,bucket_id,project_id,target,markdown,revision,updated_at_unix_ms,updated_by_session_id FROM instruction_layers WHERE id=?1", params![id as i64], row_to_instruction_layer).optional()?.ok_or(StorageError::NotFound("instruction layer",id))
    }

    pub fn instruction_history(&self, layer_id: u64) -> Result<Vec<InstructionRevision>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt=conn.prepare("SELECT revision,markdown,note,updated_at_unix_ms,updated_by_session_id FROM instruction_revisions WHERE layer_id=?1 ORDER BY revision DESC")?;
        let rows = stmt
            .query_map(params![layer_id as i64], |r| {
                Ok(InstructionRevision {
                    revision: r.get::<_, i64>(0)? as u64,
                    markdown: r.get(1)?,
                    note: r.get(2)?,
                    updated_at_unix_ms: r.get(3)?,
                    updated_by_session_id: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn revert_instruction_layer(
        &self,
        layer_id: u64,
        revision: u64,
        expected_revision: u64,
        note: &str,
        updated_by_session_id: Option<u64>,
        now: i64,
    ) -> Result<InstructionLayer> {
        let layer = self.get_instruction_layer(layer_id)?;
        let markdown = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT markdown FROM instruction_revisions WHERE layer_id=?1 AND revision=?2",
                params![layer_id as i64, revision as i64],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StorageError::NotFound("instruction revision", revision))?;
        self.set_instruction_layer(
            layer.bucket_id,
            layer.project_id,
            layer.target,
            &markdown,
            expected_revision,
            note,
            updated_by_session_id,
            now,
        )
    }

    pub fn save_instruction_snapshot(
        &self,
        session_id: u64,
        generation: u64,
        compiled: &str,
        sources_json: &str,
        hash: &str,
        now: i64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute("INSERT OR REPLACE INTO session_instruction_snapshots(session_id,terminal_generation,compiled_markdown,source_revisions_json,content_hash,created_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6)",params![session_id as i64,generation as i64,compiled,sources_json,hash,now])?;
        Ok(())
    }
    pub fn instruction_snapshot(
        &self,
        session_id: u64,
        generation: u64,
    ) -> Result<(String, String, String)> {
        self.conn.lock().unwrap().query_row("SELECT compiled_markdown,source_revisions_json,content_hash FROM session_instruction_snapshots WHERE session_id=?1 AND terminal_generation=?2",params![session_id as i64,generation as i64],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(StorageError::NotFound("session instruction snapshot",session_id))
    }

    pub fn snapshot(&self, now_unix_ms: i64) -> Result<Snapshot> {
        let conn = self.conn.lock().unwrap();
        let mut buckets = conn
            .prepare(
                "SELECT id, name, position, permission_mode, default_worker_id, is_default, default_agent, model_profile_id FROM buckets ORDER BY position, id",
            )?
            .query_map([], row_to_bucket)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for bucket in &mut buckets {
            bucket.allowed_worker_ids =
                worker_ids_for(&conn, "bucket_workers", "bucket_id", bucket.id)?;
        }
        let mut projects = conn
            .prepare("SELECT id, bucket_id, name, path, permission_mode, worker_id, default_agent, model_profile_id FROM projects ORDER BY id")?
            .query_map([], row_to_project)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for project in &mut projects {
            project.allowed_worker_ids =
                worker_ids_for(&conn, "project_workers", "project_id", project.id)?;
            project.worker_paths = project_paths_for(&conn, project.id)?;
        }
        let sessions = conn
            .prepare(&format!(
                "{SESSION_SELECT} WHERE state NOT IN ('exited','failed') \
                OR ended_at_unix_ms >= ?1 ORDER BY id"
            ))?
            .query_map(
                params![now_unix_ms - SESSION_RECENT_GRACE_MS],
                row_to_session,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        let instruction_layers = conn.prepare("SELECT id,bucket_id,project_id,target,markdown,revision,updated_at_unix_ms,updated_by_session_id FROM instruction_layers ORDER BY bucket_id,COALESCE(project_id,0),target")?
            .query_map([], row_to_instruction_layer)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let workers = conn
            .prepare(&format!("SELECT {WORKER_COLUMNS} FROM workers ORDER BY id"))?
            .query_map([], row_to_worker)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let session_ids = sessions
            .iter()
            .map(|session| session.id)
            .collect::<std::collections::HashSet<_>>();
        let terminals = conn
            .prepare(&format!("{TERMINAL_SELECT} WHERE t.session_id IN \
                (SELECT id FROM sessions WHERE state NOT IN ('exited','failed') OR ended_at_unix_ms >= ?1) ORDER BY t.id"))?
            .query_map(params![now_unix_ms - SESSION_RECENT_GRACE_MS], row_to_terminal)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|terminal| session_ids.contains(&terminal.session_id))
            .collect();
        let forwards = conn
            .prepare(&format!("{FORWARD_SELECT} WHERE f.session_id IN \
                (SELECT id FROM sessions WHERE state NOT IN ('exited','failed') OR ended_at_unix_ms >= ?1) ORDER BY f.id"))?
            .query_map(params![now_unix_ms - SESSION_RECENT_GRACE_MS], row_to_session_forward)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|forward| session_ids.contains(&forward.session_id))
            .collect();
        let mut contexts: std::collections::BTreeMap<u64, SessionContext> =
            std::collections::BTreeMap::new();
        let mut glance_stmt = conn.prepare(
            "SELECT session_id, key, label, value, kind, severity FROM session_glance \
             WHERE session_id IN (SELECT id FROM sessions WHERE state NOT IN ('exited','failed') \
                OR ended_at_unix_ms >= ?1) ORDER BY session_id, position",
        )?;
        for row in glance_stmt.query_map(params![now_unix_ms - SESSION_RECENT_GRACE_MS], |r| {
            Ok((r.get::<_, i64>(0)? as u64, context_field_from(r)?))
        })? {
            let (sid, field) = row?;
            if session_ids.contains(&sid) {
                contexts.entry(sid).or_default().glance.push(field);
            }
        }
        let mut detail_stmt = conn.prepare(
            "SELECT session_id, key, label, value, kind, severity FROM session_context \
             WHERE session_id IN (SELECT id FROM sessions WHERE state NOT IN ('exited','failed') \
                OR ended_at_unix_ms >= ?1) ORDER BY session_id, updated_at_unix_ms, key",
        )?;
        for row in detail_stmt.query_map(params![now_unix_ms - SESSION_RECENT_GRACE_MS], |r| {
            Ok((r.get::<_, i64>(0)? as u64, context_field_from(r)?))
        })? {
            let (sid, field) = row?;
            if session_ids.contains(&sid) {
                contexts.entry(sid).or_default().detail.push(field);
            }
        }
        let contexts = contexts
            .into_iter()
            .map(|(session_id, mut c)| {
                c.session_id = session_id;
                c
            })
            .collect();
        let mut items = conn
            .prepare(&format!(
                "{ITEM_SELECT} WHERE status NOT IN ('done','dropped') \
                 OR updated_at_unix_ms >= ?1 ORDER BY id"
            ))?
            .query_map(
                params![now_unix_ms - ITEM_SNAPSHOT_CLOSED_WINDOW_MS],
                row_to_item,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        attach_item_links(&conn, &mut items)?;
        let briefings = conn
            .prepare(
                "SELECT id, bucket_id, session_id, ts_unix_ms, markdown FROM bucket_briefings b \
                 WHERE id = (SELECT MAX(id) FROM bucket_briefings WHERE bucket_id = b.bucket_id) \
                 ORDER BY bucket_id",
            )?
            .query_map([], row_to_briefing)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let reviews = conn
            .prepare(&format!(
                "{} WHERE state = 'open' ORDER BY id",
                crate::review_store::REVIEW_SELECT
            ))?
            .query_map([], crate::review_store::row_to_review)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let plans = crate::plan_store::plans_from_conn(&conn, None, false)?;
        Ok(Snapshot {
            review_viewer_states: Vec::new(),
            reviews,
            plans,
            buckets,
            projects,
            sessions,
            workers,
            terminals,
            contexts,
            forwards,
            items,
            briefings,
            user_settings: Vec::new(),
            instruction_layers,
            model_profiles: Vec::new(),
            agent_dialects: Vec::new(),
        })
    }
}

/// Seeds the one useful first-run bucket while the database is still known to
/// be brand new. This helper is intentionally never called during migrations
/// or ordinary reopen, so an intentionally emptied installation stays empty.
/// What a brand-new database starts with: one bucket and one project,
/// both usable before anything is configured.
fn seed_fresh_default_bucket(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let bucket_count: i64 = tx.query_row("SELECT COUNT(*) FROM buckets", [], |row| row.get(0))?;
    if bucket_count == 0 {
        tx.execute(
            "INSERT INTO buckets (name, position, permission_mode, default_worker_id, is_default, default_agent) \
             VALUES ('Default', 0, 'default', ?1, 1, 'claude')",
            params![LOCAL_WORKER_ID as i64],
        )?;
        let bucket_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT OR IGNORE INTO bucket_workers (bucket_id, worker_id) VALUES (?1, ?2)",
            params![bucket_id, LOCAL_WORKER_ID as i64],
        )?;
        // A project with no path runs in the home directory of whatever
        // worker it spawns on, so a fresh install can start a session
        // without configuring anything first.
        tx.execute(
            "INSERT INTO projects (bucket_id, name, path, permission_mode) \
             VALUES (?1, 'Default', '', 'inherit')",
            params![bucket_id],
        )?;
        let project_id = tx.last_insert_rowid();
        // Without this the project allows no worker at all, and a fresh
        // install cannot spawn on the one thing it seeded.
        tx.execute(
            "INSERT OR IGNORE INTO project_workers (project_id, worker_id) VALUES (?1, ?2)",
            params![project_id, LOCAL_WORKER_ID as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Projects whose allowlist is empty, which a spawn refuses outright. The
/// fresh-install seed omitted the row, so a database made by it needs the same
/// backfill the table's own migration gave older projects.
const REPAIR_EMPTY_PROJECT_WORKERS: &str = "
INSERT OR IGNORE INTO project_workers (project_id, worker_id)
    SELECT p.id, bw.worker_id FROM projects p
    JOIN bucket_workers bw ON bw.bucket_id = p.bucket_id
    WHERE NOT EXISTS (SELECT 1 FROM project_workers pw WHERE pw.project_id = p.id);
";

const ITEM_SELECT: &str =
    "SELECT item_number, bucket_id, project_id, external_key, title, body, status, \
     priority, source_kind, source_detail, url, due_at_unix_ms, snoozed_until_unix_ms, \
     created_by_session_id, created_at_unix_ms, updated_at_unix_ms, done_at_unix_ms, \
     question FROM items";

fn rewrite_durable_legacy_item_links(conn: &Connection) -> Result<()> {
    let refs = conn
        .prepare("SELECT id, bucket_id, item_number FROM items")?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                (row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)? as u64),
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;

    let columns = [
        ("SELECT rowid, bucket_id, title FROM items", "UPDATE items SET title = ?2 WHERE rowid = ?1"),
        ("SELECT rowid, bucket_id, body FROM items", "UPDATE items SET body = ?2 WHERE rowid = ?1"),
        ("SELECT rowid, bucket_id, question FROM items", "UPDATE items SET question = ?2 WHERE rowid = ?1"),
        ("SELECT rowid, bucket_id, source_detail FROM items", "UPDATE items SET source_detail = ?2 WHERE rowid = ?1"),
        ("SELECT rowid, bucket_id, url FROM items", "UPDATE items SET url = ?2 WHERE rowid = ?1"),
        ("SELECT note.rowid, item.bucket_id, note.text FROM item_notes note JOIN items item ON item.id = note.item_id", "UPDATE item_notes SET text = ?2 WHERE rowid = ?1"),
        ("SELECT rowid, bucket_id, markdown FROM bucket_briefings", "UPDATE bucket_briefings SET markdown = ?2 WHERE rowid = ?1"),
        ("SELECT session.rowid, project.bucket_id, session.task_title FROM sessions session JOIN projects project ON project.id = session.project_id", "UPDATE sessions SET task_title = ?2 WHERE rowid = ?1"),
        ("SELECT session.rowid, project.bucket_id, session.task_prompt FROM sessions session JOIN projects project ON project.id = session.project_id", "UPDATE sessions SET task_prompt = ?2 WHERE rowid = ?1"),
        ("SELECT session.rowid, project.bucket_id, session.state_detail FROM sessions session JOIN projects project ON project.id = session.project_id", "UPDATE sessions SET state_detail = ?2 WHERE rowid = ?1"),
        ("SELECT session.rowid, project.bucket_id, session.activity FROM sessions session JOIN projects project ON project.id = session.project_id", "UPDATE sessions SET activity = ?2 WHERE rowid = ?1"),
        ("SELECT session.rowid, project.bucket_id, session.headline FROM sessions session JOIN projects project ON project.id = session.project_id", "UPDATE sessions SET headline = ?2 WHERE rowid = ?1"),
        ("SELECT session.rowid, project.bucket_id, session.summary FROM sessions session JOIN projects project ON project.id = session.project_id", "UPDATE sessions SET summary = ?2 WHERE rowid = ?1"),
        ("SELECT report.rowid, project.bucket_id, report.payload FROM activity_reports report JOIN sessions session ON session.id = report.session_id JOIN projects project ON project.id = session.project_id", "UPDATE activity_reports SET payload = ?2 WHERE rowid = ?1"),
        ("SELECT glance.rowid, project.bucket_id, glance.value FROM session_glance glance JOIN sessions session ON session.id = glance.session_id JOIN projects project ON project.id = session.project_id", "UPDATE session_glance SET value = ?2 WHERE rowid = ?1"),
        ("SELECT context.rowid, project.bucket_id, context.value FROM session_context context JOIN sessions session ON session.id = context.session_id JOIN projects project ON project.id = session.project_id", "UPDATE session_context SET value = ?2 WHERE rowid = ?1"),
        ("SELECT rowid, bucket_id, markdown FROM instruction_layers", "UPDATE instruction_layers SET markdown = ?2 WHERE rowid = ?1"),
        ("SELECT revision.rowid, layer.bucket_id, revision.markdown FROM instruction_revisions revision JOIN instruction_layers layer ON layer.id = revision.layer_id", "UPDATE instruction_revisions SET markdown = ?2 WHERE rowid = ?1"),
        ("SELECT revision.rowid, layer.bucket_id, revision.note FROM instruction_revisions revision JOIN instruction_layers layer ON layer.id = revision.layer_id", "UPDATE instruction_revisions SET note = ?2 WHERE rowid = ?1"),
        ("SELECT snapshot.rowid, project.bucket_id, snapshot.compiled_markdown FROM session_instruction_snapshots snapshot JOIN sessions session ON session.id = snapshot.session_id JOIN projects project ON project.id = session.project_id", "UPDATE session_instruction_snapshots SET compiled_markdown = ?2 WHERE rowid = ?1"),
    ];
    for (select, update) in columns {
        let rows = conn
            .prepare(select)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (rowid, bucket_id, text) in rows {
            let rewritten = rewrite_legacy_item_links(&text, bucket_id, &refs);
            if rewritten != text {
                conn.execute(update, params![rowid, rewritten])?;
            }
        }
    }
    Ok(())
}

fn rewrite_legacy_item_links(
    text: &str,
    owner_bucket_id: u64,
    refs: &HashMap<u64, (u64, u64)>,
) -> String {
    const PREFIX: &str = "pm:item/";
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(offset) = rest.find(PREFIX) {
        let start = offset + PREFIX.len();
        output.push_str(&rest[..start]);
        let digits = rest[start..].bytes().take_while(u8::is_ascii_digit).count();
        let identifier_adjacent_before = rest[..offset]
            .bytes()
            .next_back()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        let identifier_adjacent_after = rest[start + digits..]
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if digits == 0
            || identifier_adjacent_before
            || identifier_adjacent_after
            || rest[start + digits..].starts_with('/')
        {
            rest = &rest[start..];
            continue;
        }
        let legacy_id = rest[start..start + digits].parse::<u64>().ok();
        if let Some((bucket_id, item_number)) = legacy_id
            .and_then(|id| refs.get(&id).copied())
            .filter(|(bucket_id, _)| *bucket_id == owner_bucket_id)
        {
            output.push_str(&format!("{bucket_id}/{item_number}"));
        } else {
            output.push_str(&rest[start..start + digits]);
        }
        rest = &rest[start + digits..];
    }
    output.push_str(rest);
    output
}

/// Loads one item with its dependency and session links attached.
fn load_item(conn: &Connection, bucket_id: u64, item_number: u64) -> Result<Option<Item>> {
    let item = conn
        .query_row(
            &format!("{ITEM_SELECT} WHERE bucket_id = ?1 AND item_number = ?2"),
            params![bucket_id as i64, item_number as i64],
            row_to_item,
        )
        .optional()?
        .transpose()?;
    match item {
        Some(mut item) => {
            attach_item_links(conn, std::slice::from_mut(&mut item))?;
            Ok(Some(item))
        }
        None => Ok(None),
    }
}

fn load_item_by_internal_id(conn: &Connection, internal_id: u64) -> Result<Option<Item>> {
    let item = conn
        .query_row(
            &format!("{ITEM_SELECT} WHERE id = ?1"),
            params![internal_id as i64],
            row_to_item,
        )
        .optional()?
        .transpose()?;
    match item {
        Some(mut item) => {
            attach_item_links(conn, std::slice::from_mut(&mut item))?;
            Ok(Some(item))
        }
        None => Ok(None),
    }
}

fn internal_item_id(conn: &Connection, bucket_id: u64, item_number: u64) -> Result<u64> {
    conn.query_row(
        "SELECT id FROM items WHERE bucket_id = ?1 AND item_number = ?2",
        params![bucket_id as i64, item_number as i64],
        |row| Ok(row.get::<_, i64>(0)? as u64),
    )
    .optional()?
    .ok_or(StorageError::NotFound("item", item_number))
}

fn attach_item_links(conn: &Connection, items: &mut [Item]) -> Result<()> {
    let mut deps = conn.prepare(
        "SELECT dependency.item_number FROM item_deps edge \
         JOIN items dependency ON dependency.id = edge.depends_on_item_id \
         WHERE edge.item_id = ?1 ORDER BY dependency.item_number",
    )?;
    let mut sessions = conn
        .prepare("SELECT session_id FROM item_sessions WHERE item_id = ?1 ORDER BY session_id")?;
    for item in items {
        let internal_id = internal_item_id(conn, item.bucket_id, item.id)?;
        item.blocked_by = deps
            .query_map(params![internal_id as i64], |r| {
                Ok(r.get::<_, i64>(0)? as u64)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        item.session_ids = sessions
            .query_map(params![internal_id as i64], |r| {
                Ok(r.get::<_, i64>(0)? as u64)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }
    Ok(())
}

/// Replaces an item's "blocked by" edges after validating every
/// reference stays inside the bucket.
fn replace_item_deps(
    conn: &Connection,
    bucket_id: u64,
    internal_id: u64,
    item_number: u64,
    deps: &[u64],
) -> Result<()> {
    let deps = sorted(deps);
    if deps.len() > ITEM_DEPS_MAX {
        return Err(StorageError::Conflict(format!(
            "an item can be blocked by at most {ITEM_DEPS_MAX} items"
        )));
    }
    conn.execute(
        "DELETE FROM item_deps WHERE item_id = ?1",
        params![internal_id as i64],
    )?;
    for dep in deps {
        if dep == item_number {
            return Err(StorageError::Conflict(
                "an item cannot be blocked by itself".into(),
            ));
        }
        let dependency_id = internal_item_id(conn, bucket_id, dep).map_err(|_| {
            StorageError::Conflict(format!("blocked_by item {dep} not found in this bucket"))
        })?;
        conn.execute(
            "INSERT INTO item_deps (item_id, depends_on_item_id) VALUES (?1, ?2)",
            params![internal_id as i64, dependency_id as i64],
        )?;
    }
    Ok(())
}

fn add_item_note(
    conn: &Connection,
    item_id: u64,
    session_id: Option<u64>,
    ts_unix_ms: i64,
    kind: &str,
    text: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO item_notes (item_id, session_id, ts_unix_ms, kind, text) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            item_id as i64,
            session_id.map(|s| s as i64),
            ts_unix_ms,
            kind,
            text
        ],
    )?;
    Ok(())
}

fn link_item_session(
    conn: &Connection,
    bucket_id: u64,
    item_id: u64,
    session_id: u64,
) -> Result<()> {
    ensure_session_in_bucket(conn, bucket_id, session_id)?;
    conn.execute(
        "INSERT OR IGNORE INTO item_sessions (item_id, session_id) VALUES (?1, ?2)",
        params![item_id as i64, session_id as i64],
    )
    .map_err(conflict_on_constraint(|| {
        format!("cannot link item {item_id} to session {session_id}")
    }))?;
    Ok(())
}

/// The attention a state carries, owed by every path that writes one:
/// entering the blocked state asks for the user, and leaving idle retires the
/// finished-turn alert.
fn record_state_attention(
    conn: &Connection,
    id: u64,
    previous: &str,
    state: SessionState,
) -> Result<()> {
    // Repeated signals within one blocked stretch must not re-alert, so only
    // the transition into it counts.
    if state == SessionState::NeedsInput && previous != SessionState::NeedsInput.as_str() {
        conn.execute(
            "INSERT INTO notifications (session_id, ts_unix_ms, kind, read_at_unix_ms) \
             VALUES (?1, ?2, 'needs-input', NULL)",
            params![id as i64, now_unix_ms()],
        )?;
    }
    if state != SessionState::Idle {
        conn.execute(
            "UPDATE notifications SET read_at_unix_ms = ?2 \
             WHERE session_id = ?1 AND kind = 'turn-ended' AND read_at_unix_ms IS NULL",
            params![id as i64, now_unix_ms()],
        )?;
    }
    Ok(())
}

fn ensure_session_in_bucket(conn: &Connection, bucket_id: u64, session_id: u64) -> Result<()> {
    let session_bucket = conn
        .query_row(
            "SELECT project.bucket_id FROM sessions session \
             JOIN projects project ON project.id = session.project_id \
             WHERE session.id = ?1",
            params![session_id as i64],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .optional()?;
    if session_bucket != Some(bucket_id) {
        return Err(StorageError::Conflict(format!(
            "session {session_id} is not in this bucket"
        )));
    }
    Ok(())
}

fn sorted(v: &[u64]) -> Vec<u64> {
    let mut s = v.to_vec();
    s.sort_unstable();
    s.dedup();
    s
}

/// Maps a constraint violation to a domain Conflict, passing other
/// sqlite errors through.
fn conflict_on_constraint<F: Fn() -> String>(msg: F) -> impl Fn(rusqlite::Error) -> StorageError {
    move |e| match e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            StorageError::Conflict(msg())
        }
        e => e.into(),
    }
}

fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<Result<Item>> {
    let status_str: String = row.get(6)?;
    let priority_str: String = row.get(7)?;
    let kind_str: String = row.get(8)?;
    let (status, priority, source_kind) = match (
        ItemStatus::parse(&status_str),
        ItemPriority::parse(&priority_str),
        ItemSourceKind::parse(&kind_str),
    ) {
        (Some(s), Some(p), Some(k)) => (s, p, k),
        _ => {
            return Ok(Err(StorageError::Conflict(format!(
                "unreadable item row: status={status_str:?} priority={priority_str:?} \
                 source={kind_str:?}"
            ))))
        }
    };
    Ok(Ok(Item {
        id: row.get::<_, i64>(0)? as u64,
        bucket_id: row.get::<_, i64>(1)? as u64,
        project_id: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        external_key: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        question: row.get(17)?,
        status,
        priority,
        source_kind,
        source_detail: row.get(9)?,
        url: row.get(10)?,
        due_at_unix_ms: row.get(11)?,
        snoozed_until_unix_ms: row.get(12)?,
        created_by_session_id: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
        created_at_unix_ms: row.get(14)?,
        updated_at_unix_ms: row.get(15)?,
        done_at_unix_ms: row.get(16)?,
        blocked_by: Vec::new(),
        session_ids: Vec::new(),
    }))
}

fn row_to_briefing(row: &rusqlite::Row) -> rusqlite::Result<BucketBriefing> {
    Ok(BucketBriefing {
        id: row.get::<_, i64>(0)? as u64,
        bucket_id: row.get::<_, i64>(1)? as u64,
        session_id: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        ts_unix_ms: row.get(3)?,
        markdown: row.get(4)?,
    })
}

const SESSION_SELECT: &str = "SELECT id, project_id, agent, state, task_title, task_prompt, \
     agent_session_id, created_at_unix_ms, ended_at_unix_ms, exit_code, state_detail, \
     activity, progress_percent, transcript_path, permission_mode, worker_id, cwd, agent_resumable, \
     headline, summary, items_api, \
     MAX(created_at_unix_ms, COALESCE(ended_at_unix_ms, 0), \
         COALESCE((SELECT last_user_submit_at_unix_ms FROM session_activity WHERE session_id = sessions.id), 0), \
         COALESCE((SELECT last_agent_turn_at_unix_ms FROM session_activity WHERE session_id = sessions.id), 0), \
         COALESCE((SELECT MAX(ts_unix_ms) FROM activity_reports WHERE session_id = sessions.id AND kind != 'user-note'), 0)) AS last_activity_at_unix_ms, \
     MAX(COALESCE((SELECT last_agent_activity_at_unix_ms FROM session_activity WHERE session_id = sessions.id), 0), \
         COALESCE((SELECT MAX(ts_unix_ms) FROM activity_reports WHERE session_id = sessions.id), 0)) AS last_agent_activity_at_unix_ms, \
     COALESCE((SELECT last_user_interaction_at_unix_ms FROM session_activity WHERE session_id = sessions.id), 0) AS last_user_interaction_at_unix_ms, \
     supervisor_api, spawned_by_session_id, role, agent_source, \
     CASE WHEN state = 'needs-input' AND EXISTS ( \
         SELECT 1 FROM notifications \
         WHERE session_id = sessions.id AND kind = 'needs-input' AND read_at_unix_ms IS NULL \
     ) THEN 1 ELSE 0 END AS needs_input_unseen, \
     model_profile_id, model_profile_source, \
     CASE WHEN state = 'idle' AND EXISTS ( \
         SELECT 1 FROM notifications \
         WHERE session_id = sessions.id AND kind = 'turn-ended' AND read_at_unix_ms IS NULL \
     ) THEN 1 ELSE 0 END AS idle_unseen, \
     git_branch, git_worktree, git_repo_root, git_commit, git_upstream, git_dirty, goal \
     FROM sessions";

fn parse_ended_cursor(cursor: &str) -> Result<(Option<i64>, Option<u64>)> {
    if cursor.is_empty() {
        return Ok((None, None));
    }
    let (ended, id) = cursor
        .split_once(':')
        .ok_or_else(|| StorageError::Conflict("invalid ended-session cursor".into()))?;
    Ok((
        Some(
            ended
                .parse()
                .map_err(|_| StorageError::Conflict("invalid ended-session cursor".into()))?,
        ),
        Some(
            id.parse()
                .map_err(|_| StorageError::Conflict("invalid ended-session cursor".into()))?,
        ),
    ))
}

fn parse_search_cursor(cursor: &str) -> Result<(Option<i64>, Option<i64>, Option<u64>)> {
    if cursor.is_empty() {
        return Ok((None, None, None));
    }
    let mut parts = cursor.split(':');
    let invalid = || StorageError::Conflict("invalid session-search cursor".into());
    let rank = parts
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let activity = parts
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let id = parts
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    if parts.next().is_some() {
        return Err(invalid());
    }
    Ok((Some(rank), Some(activity), Some(id)))
}

fn fts_query(query: &str) -> String {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| format!("\"{}\"*", part.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn search_rank(session: &Session, query: &str, exact_id: Option<u64>) -> i64 {
    if exact_id == Some(session.id) {
        return 0;
    }
    let query = query.to_lowercase();
    let title = session.task_title.to_lowercase();
    let goal = session.goal.to_lowercase();
    let headline = session.headline.to_lowercase();
    if title == query || goal == query || headline == query {
        1
    } else if title.starts_with(&query) || goal.starts_with(&query) || headline.starts_with(&query)
    {
        2
    } else {
        3
    }
}

const FORWARD_SELECT: &str = "SELECT f.id, f.session_id, f.worker_port, f.listener_port, f.slug, \
     f.label, f.scheme, f.created_at_unix_ms, COALESCE(d.path, '') \
     FROM session_forwards f LEFT JOIN session_dir_shares d ON d.id = f.dir_share_id";

const DIR_SHARE_SELECT: &str = "SELECT id, session_id, path, slug, label, created_at_unix_ms \
     FROM session_dir_shares";

const TERMINAL_SELECT: &str = "SELECT t.id, t.session_id, t.kind, t.title, t.cwd, t.created_at_unix_ms, \
    r.generation, r.state, r.started_at_unix_ms, r.ended_at_unix_ms, r.exit_code, r.scrollback_available \
    FROM terminals t JOIN terminal_runs r ON r.terminal_id = t.id \
    AND r.generation = (SELECT MAX(r2.generation) FROM terminal_runs r2 WHERE r2.terminal_id = t.id)";

fn row_to_model_profile(row: &rusqlite::Row) -> rusqlite::Result<ModelProfile> {
    Ok(ModelProfile {
        id: row.get::<_, i64>(0)? as u64,
        name: row.get(1)?,
        key_set: row.get::<_, Option<String>>(2)?.is_some(),
        endpoints: Vec::new(),
        created_at_unix_ms: row.get(3)?,
        updated_at_unix_ms: row.get(4)?,
    })
}

fn model_profile_endpoints(
    conn: &Connection,
    profile_id: u64,
) -> Result<Vec<ModelProfileEndpoint>> {
    Ok(conn
        .prepare(
            "SELECT dialect, model, base_url, background_model FROM model_profile_endpoints \
             WHERE profile_id = ?1 ORDER BY dialect",
        )?
        .query_map(params![profile_id as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|(dialect, model, base_url, background_model)| {
            Some(ModelProfileEndpoint {
                profile_id,
                dialect: ModelDialect::parse(&dialect)?,
                model,
                base_url,
                background_model,
            })
        })
        .collect())
}

fn row_to_bucket(row: &rusqlite::Row) -> rusqlite::Result<Bucket> {
    let mode: String = row.get(3)?;
    Ok(Bucket {
        id: row.get::<_, i64>(0)? as u64,
        name: row.get(1)?,
        position: row.get::<_, i64>(2)? as u32,
        permission_mode: PermissionMode::parse(&mode).unwrap_or(PermissionMode::Default),
        default_agent: row
            .get::<_, Option<String>>(6)?
            .and_then(|agent| AgentKind::parse(&agent)),
        model_profile_id: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        default_worker_id: row.get::<_, i64>(4)? as u64,
        allowed_worker_ids: Vec::new(),
        is_default: row.get(5)?,
    })
}

fn row_to_dir_share(row: &rusqlite::Row) -> rusqlite::Result<SessionDirShare> {
    Ok(SessionDirShare {
        id: row.get::<_, i64>(0)? as u64,
        session_id: row.get::<_, i64>(1)? as u64,
        path: row.get(2)?,
        slug: row.get(3)?,
        label: row.get(4)?,
        created_at_unix_ms: row.get(5)?,
    })
}

fn row_to_session_forward(row: &rusqlite::Row) -> rusqlite::Result<SessionForward> {
    Ok(SessionForward {
        id: row.get::<_, i64>(0)? as u64,
        session_id: row.get::<_, i64>(1)? as u64,
        worker_port: row.get::<_, i64>(2)? as u16,
        listener_port: row.get::<_, i64>(3)? as u16,
        slug: row.get(4)?,
        label: row.get(5)?,
        scheme: row.get(6)?,
        created_at_unix_ms: row.get(7)?,
        source_path: row.get(8)?,
        // Live overlays owned by the daemon, not persisted.
        url: String::new(),
        target_reachable: None,
    })
}

fn row_to_workspace(row: &rusqlite::Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get::<_, i64>(0)? as u64,
        name: row.get(1)?,
        layout_json: row.get(2)?,
        created_at_unix_ms: row.get(3)?,
        updated_at_unix_ms: row.get(4)?,
        position: row.get::<_, i64>(5)? as u32,
    })
}

fn row_to_project(row: &rusqlite::Row) -> rusqlite::Result<Project> {
    let mode: String = row.get(4)?;
    Ok(Project {
        id: row.get::<_, i64>(0)? as u64,
        bucket_id: row.get::<_, i64>(1)? as u64,
        name: row.get(2)?,
        path: row.get(3)?,
        permission_mode: PermissionMode::parse(&mode).unwrap_or(PermissionMode::Inherit),
        default_agent: row
            .get::<_, Option<String>>(6)?
            .and_then(|agent| AgentKind::parse(&agent)),
        model_profile_id: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        worker_id: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        allowed_worker_ids: Vec::new(),
        worker_paths: Vec::new(),
    })
}

fn project_paths_for(conn: &Connection, project_id: u64) -> Result<Vec<ProjectPath>> {
    Ok(conn
        .prepare(
            "SELECT worker_id, path FROM project_paths WHERE project_id = ?1 ORDER BY worker_id",
        )?
        .query_map(params![project_id as i64], |row| {
            Ok(ProjectPath {
                worker_id: row.get::<_, i64>(0)? as u64,
                path: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn worker_ids_for(
    conn: &Connection,
    table: &str,
    parent_column: &str,
    parent_id: u64,
) -> Result<Vec<u64>> {
    debug_assert!(matches!(table, "bucket_workers" | "project_workers"));
    debug_assert!(matches!(parent_column, "bucket_id" | "project_id"));
    let mut stmt = conn.prepare(&format!(
        "SELECT worker_id FROM {table} WHERE {parent_column} = ?1 ORDER BY worker_id"
    ))?;
    let rows = stmt
        .query_map(
            params![parent_id as i64],
            |r| Ok(r.get::<_, i64>(0)? as u64),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn row_to_mobile_device(row: &rusqlite::Row) -> rusqlite::Result<MobileDevice> {
    Ok(MobileDevice {
        id: row.get::<_, i64>(0)? as u64,
        user_id: row.get::<_, i64>(1)? as u64,
        name: row.get(2)?,
        platform: row.get(3)?,
        app_installation_id: row.get(4)?,
        refresh_family_id: row.get(5)?,
        created_at_unix_ms: row.get(6)?,
        last_seen_at_unix_ms: row.get(7)?,
        revoked_at_unix_ms: row.get(8)?,
    })
}

const MOBILE_DEVICE_SELECT: &str = "SELECT id, user_id, name, platform, \
    app_installation_id, refresh_family_id, created_at_unix_ms, \
    last_seen_at_unix_ms, revoked_at_unix_ms FROM mobile_devices";

/// Columns feeding `row_to_push_endpoint`, in order; a caller that
/// selects more appends its own columns after these.
const PUSH_ENDPOINT_COLUMNS: &str = "device_id, token_ciphertext, environment, \
    locale, previews_enabled, event_mask, public_key, push_counter, \
    mobile_push_endpoints.created_at_unix_ms, \
    updated_at_unix_ms, disabled_at_unix_ms, disabled_reason";

fn row_to_push_endpoint(row: &rusqlite::Row) -> rusqlite::Result<PushEndpoint> {
    Ok(PushEndpoint {
        device_id: row.get::<_, i64>(0)? as u64,
        token_ciphertext: row.get(1)?,
        environment: row.get(2)?,
        locale: row.get(3)?,
        previews_enabled: row.get(4)?,
        event_mask: row.get::<_, i64>(5)? as u32,
        public_key: row.get(6)?,
        push_counter: row.get::<_, i64>(7)? as u64,
        created_at_unix_ms: row.get(8)?,
        updated_at_unix_ms: row.get(9)?,
        disabled_at_unix_ms: row.get(10)?,
        disabled_reason: row.get(11)?,
    })
}

const NOTIFICATION_DELIVERY_SELECT: &str = "SELECT id, event_id, device_id, session_id, state, \
    collapse_id, status, provider_message_id, attempt_count, next_attempt_at_unix_ms, \
    last_error, created_at_unix_ms FROM notification_deliveries";

fn row_to_notification_delivery(row: &rusqlite::Row) -> rusqlite::Result<NotificationDelivery> {
    Ok(NotificationDelivery {
        id: row.get::<_, i64>(0)? as u64,
        event_id: row.get(1)?,
        device_id: row.get::<_, i64>(2)? as u64,
        session_id: row.get::<_, i64>(3)? as u64,
        state: row.get(4)?,
        collapse_id: row.get(5)?,
        status: row.get(6)?,
        provider_message_id: row.get(7)?,
        attempt_count: row.get::<_, i64>(8)? as u32,
        next_attempt_at_unix_ms: row.get(9)?,
        last_error: row.get(10)?,
        created_at_unix_ms: row.get(11)?,
    })
}

/// The `workers` columns [`row_to_worker`] reads, in the order it reads
/// them. Every query that maps a worker row selects exactly this.
const WORKER_COLUMNS: &str = "id, name, hostname, platform, default_project_root, \
                              last_seen_at_unix_ms, pm_version, connect_mode, endpoint, \
                              runtime, container";

fn row_to_worker(row: &rusqlite::Row) -> rusqlite::Result<Worker> {
    let id = row.get::<_, i64>(0)? as u64;
    Ok(Worker {
        id,
        name: row.get(1)?,
        hostname: row.get(2)?,
        platform: row.get(3)?,
        // The local worker is always reachable; a remote worker's live
        // reachability is layered on by the daemon from its connections.
        online: id == LOCAL_WORKER_ID,
        default_project_root: row.get(4)?,
        last_seen_at_unix_ms: row.get(5)?,
        pm_version: row.get(6)?,
        runtime: row.get(9)?,
        container: row.get(10)?,
        connect_mode: row
            .get::<_, String>(7)
            .ok()
            .and_then(|mode| ConnectMode::parse(&mode))
            .unwrap_or_default(),
        endpoint: row.get(8)?,
    })
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Result<Session>> {
    let agent_str: String = row.get(2)?;
    let state_str: String = row.get(3)?;
    let mode_str: String = row.get(14)?;
    let role_str: String = row.get(26)?;
    let agent_source_str: String = row.get(27)?;
    let (agent, state) = match (
        AgentKind::parse(&agent_str),
        SessionState::parse(&state_str),
    ) {
        (Some(a), Some(s)) => (a, s),
        _ => {
            return Ok(Err(StorageError::Conflict(format!(
                "unreadable session row: agent={agent_str:?} state={state_str:?}"
            ))))
        }
    };
    Ok(Ok(Session {
        id: row.get::<_, i64>(0)? as u64,
        project_id: row.get::<_, i64>(1)? as u64,
        agent,
        agent_source: AgentSelectionSource::parse(&agent_source_str)
            .unwrap_or(AgentSelectionSource::Explicit),
        state,
        task_title: row.get(4)?,
        task_prompt: row.get(5)?,
        agent_session_id: row.get(6)?,
        created_at_unix_ms: row.get(7)?,
        ended_at_unix_ms: row.get(8)?,
        exit_code: row.get(9)?,
        state_detail: row.get(10)?,
        activity: row.get(11)?,
        progress_percent: row.get::<_, Option<i64>>(12)?.map(|v| v as u32),
        resumable: row.get::<_, i64>(17)? != 0,
        permission_mode: PermissionMode::parse(&mode_str).unwrap_or(PermissionMode::Default),
        worker_id: row.get::<_, i64>(15)? as u64,
        cwd: row.get(16)?,
        goal: row.get(38)?,
        headline: row.get(18)?,
        summary: row.get(19)?,
        git: session_git_from_row(row)?,
        items_api: row.get::<_, i64>(20)? != 0,
        supervisor_api: row.get::<_, i64>(24)? != 0,
        spawned_by_session_id: row.get::<_, Option<i64>>(25)?.map(|v| v as u64),
        role: SessionRole::parse(&role_str).unwrap_or(SessionRole::Worker),
        last_activity_at_unix_ms: row.get(21)?,
        last_agent_activity_at_unix_ms: row.get(22)?,
        last_user_interaction_at_unix_ms: row.get(23)?,
        needs_input_unseen: row.get::<_, i64>(28)? != 0,
        idle_unseen: row.get::<_, i64>(31)? != 0,
        model_profile_id: row.get::<_, Option<i64>>(29)?.map(|v| v as u64),
        model_profile_source: row
            .get::<_, Option<String>>(30)?
            .and_then(|source| ModelProfileSource::parse(&source)),
    }))
}

/// The goal a session starts with: its task title, or else the first
/// sentence of its prompt cut to GOAL_SEED_MAX characters.
pub fn seed_goal(task_title: &str, task_prompt: &str) -> String {
    let title = task_title.trim();
    if !title.is_empty() {
        return truncate_chars(title, GOAL_MAX);
    }
    let sentence = task_prompt
        .lines()
        .map(|line| line.trim().trim_start_matches(['#', '-', '*', '>']).trim())
        .find(|line| !line.is_empty())
        .map(first_sentence)
        .unwrap_or_default();
    truncate_at_word(sentence, GOAL_SEED_MAX)
}

fn first_sentence(line: &str) -> &str {
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '.' | '?' | '!') && chars.peek().is_none_or(|(_, next)| next.is_whitespace())
        {
            return &line[..i];
        }
    }
    line
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Cuts at the last word boundary within `max` characters and marks the
/// cut with an ellipsis, which counts toward `max`.
fn truncate_at_word(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head = truncate_chars(text, max - 1);
    let cut = head
        .rfind(char::is_whitespace)
        .filter(|&i| i > 0)
        .map_or(head.as_str(), |i| &head[..i]);
    format!("{}…", cut.trim_end())
}

/// Reads the six git columns into a `SessionGit`, collapsing an
/// all-empty row to `None` so a session no agent has reported on
/// publishes no git object at all.
fn session_git_from_row(row: &rusqlite::Row) -> rusqlite::Result<Option<SessionGit>> {
    let git = SessionGit {
        branch: row.get(32)?,
        worktree: row.get(33)?,
        repo_root: row.get(34)?,
        commit: row.get(35)?,
        upstream: row.get(36)?,
        dirty: row.get::<_, Option<i64>>(37)?.map(|v| v != 0),
    };
    Ok((!git.is_empty()).then_some(git))
}

fn row_to_instruction_layer(row: &rusqlite::Row) -> rusqlite::Result<InstructionLayer> {
    let target: String = row.get(3)?;
    Ok(InstructionLayer {
        id: row.get::<_, i64>(0)? as u64,
        bucket_id: row.get::<_, i64>(1)? as u64,
        project_id: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
        target: InstructionTarget::parse(&target).unwrap_or(InstructionTarget::All),
        markdown: row.get(4)?,
        revision: row.get::<_, i64>(5)? as u64,
        updated_at_unix_ms: row.get(6)?,
        updated_by_session_id: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
    })
}

/// Maps a `(key, label, value, kind, severity)` row into a field,
/// falling unknown kinds/severities back to text/neutral.
fn row_to_context_field(row: &rusqlite::Row) -> rusqlite::Result<ContextField> {
    context_field_at(row, 0)
}

/// Same, for a row whose field columns start after a leading
/// `session_id` (the grouped snapshot queries).
fn context_field_from(row: &rusqlite::Row) -> rusqlite::Result<ContextField> {
    context_field_at(row, 1)
}

fn context_field_at(row: &rusqlite::Row, base: usize) -> rusqlite::Result<ContextField> {
    let kind: String = row.get(base + 3)?;
    let severity: String = row.get(base + 4)?;
    Ok(ContextField {
        key: row.get(base)?,
        label: row.get(base + 1)?,
        value: row.get(base + 2)?,
        kind: ContextKind::parse_or_text(&kind),
        severity: ContextSeverity::parse_or_neutral(&severity),
    })
}

fn row_to_terminal(row: &rusqlite::Row) -> rusqlite::Result<Terminal> {
    let kind = row.get::<_, String>(2)?;
    let state = row.get::<_, String>(7)?;
    Ok(Terminal {
        id: row.get::<_, i64>(0)? as u64,
        session_id: row.get::<_, i64>(1)? as u64,
        kind: TerminalKind::parse(&kind).ok_or(rusqlite::Error::InvalidQuery)?,
        title: row.get(3)?,
        cwd: row.get(4)?,
        created_at_unix_ms: row.get(5)?,
        generation: row.get::<_, i64>(6)? as u64,
        state: TerminalRunState::parse(&state).ok_or(rusqlite::Error::InvalidQuery)?,
        started_at_unix_ms: row.get(8)?,
        ended_at_unix_ms: row.get(9)?,
        exit_code: row.get(10)?,
        scrollback_available: row.get::<_, i64>(11)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Storage {
        Storage::open_in_memory().unwrap()
    }

    fn store_with_project() -> (Storage, u64) {
        let s = store();
        let b = s.create_bucket("work").unwrap();
        let p = s.create_project(b.id, "api", "/tmp/api").unwrap();
        (s, p.id)
    }

    #[test]
    fn bucket_crud_and_duplicate_name() {
        let s = store();
        let b = s.create_bucket("work").unwrap();
        assert_eq!(b.name, "work");
        assert!(matches!(
            s.create_bucket("work"),
            Err(StorageError::Conflict(_))
        ));
        s.delete_bucket(b.id).unwrap();
        assert!(matches!(
            s.get_bucket(b.id),
            Err(StorageError::NotFound("bucket", _))
        ));
    }

    #[test]
    fn bucket_with_projects_refuses_delete() {
        let s = store();
        let b = s.create_bucket("work").unwrap();
        s.create_project(b.id, "api", "/tmp/api").unwrap();
        assert!(matches!(
            s.delete_bucket(b.id),
            Err(StorageError::Conflict(_))
        ));
    }

    #[test]
    fn project_requires_existing_bucket() {
        let s = store();
        assert!(matches!(
            s.create_project(42, "api", "/tmp/api"),
            Err(StorageError::NotFound("bucket", 42))
        ));
    }

    #[test]
    fn project_updates_preserve_absent_fields() {
        let (s, project_id) = store_with_project();
        let updated = s
            .update_project(
                project_id,
                Some("/tmp/api-v2"),
                Some(PermissionMode::Auto),
                Some(Some(LOCAL_WORKER_ID)),
            )
            .unwrap();
        assert_eq!(updated.path, "/tmp/api-v2");
        assert_eq!(updated.permission_mode, PermissionMode::Auto);
        assert_eq!(updated.worker_id, Some(LOCAL_WORKER_ID));

        let updated = s
            .update_project(project_id, Some("/tmp/api-v3"), None, None)
            .unwrap();
        assert_eq!(updated.path, "/tmp/api-v3");
        assert_eq!(updated.permission_mode, PermissionMode::Auto);
        assert_eq!(updated.worker_id, Some(LOCAL_WORKER_ID));

        let updated = s
            .update_project(project_id, None, None, Some(None))
            .unwrap();
        assert_eq!(updated.path, "/tmp/api-v3");
        assert_eq!(updated.permission_mode, PermissionMode::Auto);
        assert_eq!(updated.worker_id, None);
    }

    #[test]
    fn project_worker_override_is_stored_at_creation() {
        let s = store();
        let bucket = s.create_bucket("work").unwrap();
        let project = s
            .create_project_with_worker(bucket.id, "api", "/srv/api", Some(LOCAL_WORKER_ID))
            .unwrap();

        assert_eq!(project.worker_id, Some(LOCAL_WORKER_ID));
    }

    #[test]
    fn worker_allowlists_are_hierarchical_and_non_empty() {
        let s = store();
        s.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO workers(id,name,created_at_unix_ms) VALUES(7,'remote',0)",
                [],
            )
            .unwrap();
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7], 7, false)
            .unwrap();
        assert_eq!(bucket.allowed_worker_ids, vec![0, 7]);
        assert!(bucket.is_default, "the first bucket is always default");
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", Some(7), &[7])
            .unwrap();
        assert_eq!(project.allowed_worker_ids, vec![7]);
        assert!(matches!(
            s.set_project_workers(project.id, &[0, 9], Some(9)),
            Err(StorageError::Conflict(_))
        ));
        assert!(matches!(
            s.set_project_workers(project.id, &[], None),
            Err(StorageError::Conflict(_))
        ));
    }

    fn add_workers(s: &Storage) {
        s.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO workers(id,name,created_at_unix_ms) VALUES(7,'builder',0),(9,'spare',0)",
                [],
            )
            .unwrap();
    }

    fn conflict<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(StorageError::Conflict(message)) => message,
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_launch_path_survives_its_host_leaving_the_allow_list() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7], 0, false)
            .unwrap();
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", None, &[0, 7])
            .unwrap();
        s.set_project_worker_path(project.id, 7, Some("/home/dev/api"))
            .unwrap();

        // Saving the project's hosts without 7 used to delete its path,
        // so an ordinary settings edit discarded a directory someone had
        // configured by hand and nothing said so.
        s.set_project_workers(project.id, &[0], None).unwrap();
        assert_eq!(
            s.project_path_for_worker(project.id, 7).unwrap(),
            Some("/home/dev/api".to_string())
        );

        s.set_project_workers(project.id, &[0, 7], None).unwrap();
        let project = s.get_project(project.id).unwrap();
        assert_eq!(
            project
                .worker_paths
                .iter()
                .find(|m| m.worker_id == 7)
                .map(|m| m.path.as_str()),
            Some("/home/dev/api"),
            "re-allowing the host should bring its path back"
        );
    }

    #[test]
    fn a_kept_path_does_not_put_a_disallowed_host_back_in_the_allow_list() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7], 0, false)
            .unwrap();
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", None, &[0, 7])
            .unwrap();
        s.set_project_worker_path(project.id, 7, Some("/home/dev/api"))
            .unwrap();

        s.set_project_workers(project.id, &[0], None).unwrap();

        // Keeping the path is only safe because selection is gated on the
        // allow list; the path must stay dormant, not re-admit the host.
        let project = s.get_project(project.id).unwrap();
        assert!(!project.allowed_worker_ids.contains(&7));
    }

    #[test]
    fn deleting_a_host_still_takes_its_launch_paths_with_it() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7], 0, false)
            .unwrap();
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", None, &[0, 7])
            .unwrap();
        s.set_project_worker_path(project.id, 7, Some("/home/dev/api"))
            .unwrap();

        // A deleted host's id may later be reused by a different machine,
        // so its paths must not outlive it.
        s.delete_worker(7, 3000).unwrap();
        assert_eq!(s.project_path_for_worker(project.id, 7).unwrap(), None);
    }

    #[test]
    fn project_worker_conflicts_name_the_host_at_fault() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7], 0, false)
            .unwrap();
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", None, &[0, 7])
            .unwrap();

        assert!(conflict(s.set_project_workers(project.id, &[], None))
            .contains("at least one allowed Host"));
        assert!(conflict(s.set_project_workers(project.id, &[0, 9], None)).contains("\"spare\""));
        assert!(conflict(s.set_project_workers(project.id, &[7], Some(0)))
            .contains("the project's default the local Host"));
        assert!(conflict(s.set_project_workers(project.id, &[7], None)).contains("inherits"));
    }

    #[test]
    fn removing_a_bucket_host_moves_pinned_projects_to_the_replacement() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7, 9], 0, false)
            .unwrap();
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", Some(7), &[7])
            .unwrap();
        s.set_project_worker_path(project.id, 7, Some("/builder/api"))
            .unwrap();

        let (bucket, changed) = s
            .set_bucket_workers(bucket.id, &[0, 9], 0, Some(9))
            .unwrap();

        assert_eq!(bucket.allowed_worker_ids, vec![0, 9]);
        let project = s.get_project(project.id).unwrap();
        assert_eq!(project.worker_id, Some(9));
        assert_eq!(project.allowed_worker_ids, vec![9]);
        assert_eq!(
            s.project_path_for_worker(project.id, 7).unwrap().as_deref(),
            Some("/builder/api"),
            "leaving the allow list is reversible, so the configured path is kept"
        );
        assert_eq!(
            changed.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![project.id]
        );
    }

    /// Removing a Host from a bucket's allow list is ordinary, reversible
    /// configuration, so it must not destroy directories the operator set by
    /// hand. Only deleting the Host outright takes its paths, because its id
    /// can then be reused by a different machine.
    #[test]
    fn a_host_leaving_and_rejoining_a_bucket_keeps_its_project_paths() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7], 0, false)
            .unwrap();
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", None, &[0, 7])
            .unwrap();
        s.set_project_worker_path(project.id, 7, Some("/builder/api"))
            .unwrap();

        s.set_bucket_workers(bucket.id, &[0], 0, None).unwrap();
        assert_eq!(
            s.project_path_for_worker(project.id, 7).unwrap().as_deref(),
            Some("/builder/api")
        );

        s.set_bucket_workers(bucket.id, &[0, 7], 0, None).unwrap();
        assert_eq!(
            s.get_project(project.id).unwrap().worker_paths,
            vec![ProjectPath {
                worker_id: 7,
                path: "/builder/api".into()
            }],
            "re-adding the Host brings its configured path back"
        );
    }

    #[test]
    fn removing_a_bucket_host_without_a_replacement_uses_the_new_default() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7, 9], 0, false)
            .unwrap();
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", Some(7), &[7])
            .unwrap();

        s.set_bucket_workers(bucket.id, &[0, 9], 9, None).unwrap();

        let project = s.get_project(project.id).unwrap();
        assert_eq!(project.worker_id, Some(9));
        assert_eq!(project.allowed_worker_ids, vec![9]);
    }

    #[test]
    fn removing_a_bucket_default_leaves_inheriting_projects_inheriting() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7], 0, false)
            .unwrap();
        let project = s
            .create_project_with_workers(bucket.id, "api", "/srv/api", None, &[0])
            .unwrap();

        let (bucket, changed) = s.set_bucket_workers(bucket.id, &[7], 7, None).unwrap();

        assert_eq!(bucket.default_worker_id, 7);
        let project = s.get_project(project.id).unwrap();
        assert_eq!(project.worker_id, None, "the project still inherits");
        assert_eq!(project.allowed_worker_ids, vec![7]);
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn a_replacement_host_must_be_one_the_bucket_still_allows() {
        let s = store();
        add_workers(&s);
        let bucket = s
            .create_bucket_with_workers("work", &[0, 7], 0, false)
            .unwrap();

        assert!(conflict(s.set_bucket_workers(bucket.id, &[0], 0, Some(7))).contains("replacement"));
        assert!(conflict(s.set_bucket_workers(bucket.id, &[], 0, None))
            .contains("at least one allowed Host"));
        assert!(conflict(s.set_bucket_workers(bucket.id, &[0], 7, None)).contains("\"builder\""));
    }

    #[test]
    fn the_seeded_default_project_allows_the_local_worker() {
        // A project with an empty allowlist cannot be spawned on, so a fresh
        // install that seeds one is an install that cannot start a session.
        let s = Storage::init(Connection::open_in_memory().unwrap(), true).unwrap();
        let snapshot = s.snapshot(0).unwrap();
        let project = snapshot
            .projects
            .first()
            .expect("the fresh install seeds a project");
        assert_eq!(project.allowed_worker_ids, vec![LOCAL_WORKER_ID]);
    }

    #[test]
    fn a_project_left_without_an_allowed_worker_is_repaired_on_open() {
        // The seed shipped without the row, so a database it made opens with a
        // project nothing may run on until this backfills it from the bucket.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pm.db");
        let project_id;
        {
            let storage = Storage::open(&path).unwrap();
            project_id = storage.snapshot(0).unwrap().projects[0].id;
            storage
                .conn
                .lock()
                .unwrap()
                .execute(
                    "DELETE FROM project_workers WHERE project_id = ?1",
                    params![project_id as i64],
                )
                .unwrap();
            assert!(storage
                .get_project(project_id)
                .unwrap()
                .allowed_worker_ids
                .is_empty());
        }

        let reopened = Storage::open(&path).unwrap();
        assert_eq!(
            reopened.get_project(project_id).unwrap().allowed_worker_ids,
            vec![LOCAL_WORKER_ID]
        );
    }

    #[test]
    fn exactly_one_bucket_is_default() {
        let s = store();
        let first = s.create_bucket("first").unwrap();
        let second = s.create_bucket("second").unwrap();
        assert!(first.is_default);
        assert!(!second.is_default);
        let changed = s.set_default_bucket(second.id).unwrap();
        assert_eq!(changed.iter().filter(|bucket| bucket.is_default).count(), 1);
        assert!(
            changed
                .iter()
                .find(|bucket| bucket.id == second.id)
                .unwrap()
                .is_default
        );
    }

    /// `working_since_unix_ms` marks when the turn began, and the
    /// completed-notification rule compares an agent report's checkpoint
    /// against it. An agent whose activity signal repeats within a turn
    /// re-enters `working` several times, so moving the mark on a repeat
    /// would push it past a checkpoint already recorded and silently
    /// disqualify the turn from notifying.
    #[test]
    fn re_entering_working_keeps_the_mark_the_turn_began_at() {
        let (s, pid) = store_with_project();
        let sess = s
            .create_session(
                pid,
                AgentKind::ClaudeCode,
                "fix",
                "fix the bug",
                PermissionMode::Bypass,
                0,
                true,
                false,
                None,
                1000,
            )
            .unwrap();

        s.update_session_state(sess.id, SessionState::Working, "")
            .unwrap();
        let (revision, began) = s.session_transition_marks(sess.id).unwrap();
        let began = began.expect("entering working marks the turn");

        std::thread::sleep(std::time::Duration::from_millis(5));
        s.update_session_state(sess.id, SessionState::Working, "")
            .unwrap();

        let (repeated_revision, repeated) = s.session_transition_marks(sess.id).unwrap();
        assert_eq!(repeated, Some(began), "a repeat must not restart the turn");
        assert_eq!(repeated_revision, revision, "a repeat is not a transition");

        // The next turn does move it, or a completed report from this
        // one would keep qualifying forever.
        s.update_session_state(sess.id, SessionState::Idle, "")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.update_session_state(sess.id, SessionState::Working, "")
            .unwrap();
        let (_, next) = s.session_transition_marks(sess.id).unwrap();
        assert!(next.unwrap() > began, "a new turn marks its own start");
    }

    #[test]
    fn session_lifecycle_updates_persist() {
        let (s, pid) = store_with_project();
        let sess = s
            .create_session(
                pid,
                AgentKind::ClaudeCode,
                "fix",
                "fix the bug",
                PermissionMode::Bypass,
                0,
                true,
                false,
                None,
                1000,
            )
            .unwrap();
        assert_eq!(sess.state, SessionState::Starting);
        assert_eq!(sess.permission_mode, PermissionMode::Bypass);

        let sess = s
            .update_session_state(sess.id, SessionState::Working, "")
            .unwrap();
        assert_eq!(sess.state, SessionState::Working);

        let blocked = s
            .update_session_state(sess.id, SessionState::NeedsInput, "choose")
            .unwrap();
        assert!(blocked.needs_input_unseen);
        let seen = s.mark_session_seen(sess.id, 1_500).unwrap();
        assert_eq!(seen.state, SessionState::NeedsInput);
        assert!(!seen.needs_input_unseen);
        assert!(
            !s.update_session_state(sess.id, SessionState::NeedsInput, "still choose")
                .unwrap()
                .needs_input_unseen,
            "repeated signals in one lifecycle must not re-alert"
        );
        s.update_session_state(sess.id, SessionState::Working, "")
            .unwrap();
        assert!(
            s.update_session_state(sess.id, SessionState::NeedsInput, "choose again")
                .unwrap()
                .needs_input_unseen,
            "a later transition resets attention"
        );

        let sess = s
            .set_session_ended(sess.id, SessionState::Exited, "", Some(0), 2000)
            .unwrap();
        assert_eq!(sess.state, SessionState::Exited);
        assert_eq!(sess.exit_code, Some(0));
        assert_eq!(sess.ended_at_unix_ms, Some(2000));
    }

    fn ended_session(s: &Storage, project_id: u64, title: &str, ended_at: i64) -> Session {
        let session = s
            .create_session(
                project_id,
                AgentKind::Test,
                title,
                "prompt must not be searched",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                ended_at - 1_000,
            )
            .unwrap();
        s.set_session_ended(session.id, SessionState::Exited, "", Some(0), ended_at)
            .unwrap()
    }

    #[test]
    fn snapshot_bounds_sessions_and_related_payloads_at_sixty_seconds() {
        let (s, project_id) = store_with_project();
        let now = 100_000;
        let boundary = ended_session(&s, project_id, "boundary", now - SESSION_RECENT_GRACE_MS);
        let old = ended_session(&s, project_id, "old", now - SESSION_RECENT_GRACE_MS - 1);
        let live = s
            .create_session(
                project_id,
                AgentKind::Test,
                "live",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                now,
            )
            .unwrap();
        s.upsert_context(
            old.id,
            &[ContextField {
                key: "large".into(),
                label: "Large".into(),
                value: "x".repeat(48),
                kind: ContextKind::Text,
                severity: ContextSeverity::Neutral,
            }],
            now,
        )
        .unwrap();

        let snapshot = s.snapshot(now).unwrap();
        let ids = snapshot
            .sessions
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![boundary.id, live.id]);
        assert!(snapshot
            .terminals
            .iter()
            .all(|terminal| terminal.session_id != old.id));
        assert!(snapshot
            .contexts
            .iter()
            .all(|context| context.session_id != old.id));
    }

    #[test]
    fn snapshot_payload_is_independent_of_large_ended_history() {
        let (s, project_id) = store_with_project();
        let now = 1_000_000;
        s.create_session(
            project_id,
            AgentKind::Test,
            "live",
            "small",
            PermissionMode::Default,
            0,
            true,
            false,
            None,
            now,
        )
        .unwrap();
        let before = pm_protocol::domain::ServerMsg::Snapshot(s.snapshot(now).unwrap())
            .encode_to_vec()
            .len();
        let large_prompt = "P".repeat(64 * 1024);
        for i in 0..80 {
            let session = s
                .create_session(
                    project_id,
                    AgentKind::Test,
                    &format!("old {i}"),
                    &large_prompt,
                    PermissionMode::Default,
                    0,
                    true,
                    false,
                    None,
                    i,
                )
                .unwrap();
            s.set_session_ended(session.id, SessionState::Exited, "", Some(0), i)
                .unwrap();
        }
        let snapshot = s.snapshot(now).unwrap();
        let after = pm_protocol::domain::ServerMsg::Snapshot(snapshot.clone())
            .encode_to_vec()
            .len();
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(after, before);
    }

    #[test]
    fn ended_history_is_stable_and_capped_at_fifty() {
        let (s, project_id) = store_with_project();
        for i in 0..105 {
            ended_session(&s, project_id, &format!("ended {i}"), 10_000 + i);
        }
        let first = s.list_ended_sessions("", 500).unwrap();
        assert_eq!(first.sessions.len(), 50);
        assert_eq!(first.total, 105);
        assert_eq!(first.sessions[0].task_title, "ended 104");
        // A newer row arriving after page one cannot shift the keyset continuation.
        ended_session(&s, project_id, "new arrival", 20_000);
        let second = s.list_ended_sessions(&first.next_cursor, 50).unwrap();
        assert_eq!(second.sessions.len(), 50);
        assert_eq!(second.sessions[0].task_title, "ended 54");
        let first_ids = first
            .sessions
            .iter()
            .map(|session| session.id)
            .collect::<std::collections::HashSet<_>>();
        assert!(second
            .sessions
            .iter()
            .all(|session| !first_ids.contains(&session.id)));
        let third = s.list_ended_sessions(&second.next_cursor, 50).unwrap();
        assert_eq!(third.sessions.len(), 5);
        assert!(third.next_cursor.is_empty());
    }

    #[test]
    fn metadata_search_ranks_and_excludes_payload_fields() {
        let (s, project_id) = store_with_project();
        let exact = ended_session(&s, project_id, "Needle", 5_000);
        let other = ended_session(&s, project_id, "other", 6_000);
        s.conn.lock().unwrap().execute(
            "UPDATE sessions SET headline='Needle heading', summary='searchable summary', activity='indexing metadata', agent_session_id='conv-needle', cwd='/srv/needle' WHERE id=?1",
            params![other.id as i64],
        ).unwrap();
        s.conn.lock().unwrap().execute(
            "INSERT INTO activity_reports(session_id,ts_unix_ms,kind,payload) VALUES(?1,?2,'report','payload-only-secret')",
            params![other.id as i64, 7_000],
        ).unwrap();

        let title = s.search_sessions("Needle", "", 50).unwrap();
        assert_eq!(
            title.sessions[0].id, exact.id,
            "exact title ranks before headline prefix"
        );
        assert!(title.sessions.iter().any(|session| session.id == other.id));
        assert_eq!(
            s.search_sessions("conv-needle", "", 50).unwrap().sessions[0].id,
            other.id
        );
        assert!(s
            .search_sessions("payload-only-secret", "", 50)
            .unwrap()
            .sessions
            .is_empty());
        assert!(s
            .search_sessions("prompt must not be searched", "", 50)
            .unwrap()
            .sessions
            .is_empty());
        assert!(s.search_sessions("' OR 1=1 --", "", 50).is_ok());

        let by_id = s.search_sessions(&other.id.to_string(), "", 50).unwrap();
        assert_eq!(by_id.sessions[0].id, other.id);
    }

    #[test]
    fn project_with_live_session_refuses_delete_until_ended() {
        let (s, pid) = store_with_project();
        let sess = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1000,
            )
            .unwrap();
        assert!(matches!(
            s.delete_project(pid),
            Err(StorageError::Conflict(_))
        ));
        s.set_session_ended(sess.id, SessionState::Exited, "", Some(0), 2000)
            .unwrap();
        s.delete_project(pid).unwrap();
    }

    #[test]
    fn snapshot_returns_everything_ordered() {
        let (s, pid) = store_with_project();
        s.create_session(
            pid,
            AgentKind::Codex,
            "a",
            "b",
            PermissionMode::Auto,
            0,
            true,
            false,
            None,
            1,
        )
        .unwrap();
        let snap = s.snapshot(0).unwrap();
        assert_eq!(snap.buckets.len(), 1);
        assert_eq!(snap.projects.len(), 1);
        assert_eq!(snap.sessions.len(), 1);
        assert_eq!(snap.terminals.len(), 1);
        assert_eq!(snap.terminals[0].kind, TerminalKind::Agent);
    }

    #[test]
    fn shell_restart_keeps_identity_and_advances_generation() {
        let (s, pid) = store_with_project();
        let session = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        s.set_session_cwd(session.id, "/worker/project").unwrap();
        let shell = s.create_shell(session.id, "debug", 2).unwrap();
        assert_eq!(shell.cwd, "/worker/project");
        s.update_terminal_run(
            shell.id,
            shell.generation,
            TerminalRunState::Exited,
            Some(0),
            false,
            3,
        )
        .unwrap();
        let with_scrollback = s
            .set_terminal_scrollback_available(shell.id, shell.generation)
            .unwrap();
        assert!(with_scrollback.scrollback_available);
        assert_eq!(with_scrollback.ended_at_unix_ms, Some(3));
        let restarted = s.restart_terminal(shell.id, 4).unwrap();
        assert_eq!(restarted.id, shell.id);
        assert_eq!(restarted.generation, shell.generation + 1);
        assert!(!restarted.scrollback_available);
    }

    #[test]
    fn fresh_database_seeds_the_local_worker() {
        let s = store();
        let workers = s.list_workers().unwrap();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].id, LOCAL_WORKER_ID);
        assert_eq!(workers[0].name, "local");
        assert!(workers[0].online, "the local worker is always online");
        assert_eq!(s.snapshot(0).unwrap().workers, workers);
    }

    #[test]
    fn migrating_a_v6_database_seeds_workers_and_keeps_the_project_path() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE buckets (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, \
                position INTEGER NOT NULL DEFAULT 0, permission_mode TEXT NOT NULL DEFAULT 'default');
             CREATE TABLE projects (id INTEGER PRIMARY KEY, bucket_id INTEGER NOT NULL REFERENCES buckets(id), \
                name TEXT NOT NULL, path TEXT NOT NULL, permission_mode TEXT NOT NULL DEFAULT 'inherit', \
                UNIQUE(bucket_id, name));
             CREATE TABLE sessions (id INTEGER PRIMARY KEY, \
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE, \
                agent TEXT NOT NULL, state TEXT NOT NULL, task_title TEXT NOT NULL, task_prompt TEXT NOT NULL, \
                agent_session_id TEXT, created_at_unix_ms INTEGER NOT NULL, ended_at_unix_ms INTEGER, \
                exit_code INTEGER, state_detail TEXT NOT NULL DEFAULT '', session_token TEXT, \
                activity TEXT NOT NULL DEFAULT '', progress_percent INTEGER, transcript_path TEXT, \
                permission_mode TEXT NOT NULL DEFAULT 'default');
             CREATE TABLE activity_reports (id INTEGER PRIMARY KEY, \
                session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, \
                ts_unix_ms INTEGER NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL);
             INSERT INTO buckets (id, name) VALUES (1, 'work');
             INSERT INTO projects (id, bucket_id, name, path) VALUES (5, 1, 'api', '/srv/api');
             INSERT INTO sessions (id, project_id, agent, state, task_title, task_prompt, created_at_unix_ms) \
                VALUES (9, 5, 'claude', 'exited', 't', 'p', 1);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 6i64).unwrap();

        let s = Storage::init(conn, false).unwrap();

        assert_eq!(
            s.list_workers()
                .unwrap()
                .iter()
                .map(|w| w.id)
                .collect::<Vec<_>>(),
            vec![LOCAL_WORKER_ID]
        );
        assert_eq!(
            s.get_session(9).unwrap().worker_id,
            LOCAL_WORKER_ID,
            "existing sessions default to the local worker"
        );
        assert!(
            s.get_session(9).unwrap().items_api,
            "migrated sessions get the Items API by default"
        );
        assert!(
            !s.get_session(9).unwrap().supervisor_api,
            "migrated sessions never get the supervisor tools"
        );
        assert_eq!(
            s.get_session(9).unwrap().spawned_by_session_id,
            None,
            "migrated sessions have no spawning supervisor"
        );
        s.upsert_item(
            1,
            &ItemUpsert {
                title: Some("works after migration".into()),
                ..ItemUpsert::default()
            },
            None,
            10,
        )
        .unwrap();
        assert_eq!(s.get_project(5).unwrap().path, "/srv/api");
        assert_eq!(
            s.project_path_for_worker(5, LOCAL_WORKER_ID).unwrap(),
            None,
            "the upgrade leaves no per-worker path to outrank the project's own"
        );
    }

    #[test]
    fn worker_enrollment_is_single_use_and_expiring() {
        let s = store();
        s.create_worker_enrollment(
            "hash-a",
            "sealed-a",
            "laptop",
            None,
            ConnectMode::Dial,
            "",
            1000,
            5000,
        )
        .unwrap();

        // Expired before use.
        assert!(matches!(
            s.consume_worker_enrollment("hash-a", 6000),
            Err(StorageError::Conflict(_))
        ));

        s.create_worker_enrollment(
            "hash-b",
            "sealed-b",
            "server",
            None,
            ConnectMode::Accept,
            "server.internal:7678",
            1000,
            5000,
        )
        .unwrap();
        let consumed = s.consume_worker_enrollment("hash-b", 2000).unwrap();
        assert_eq!(consumed.label, "server");
        assert_eq!(consumed.worker_id, None);
        assert_eq!(consumed.connect_mode, ConnectMode::Accept);
        assert_eq!(consumed.endpoint, "server.internal:7678");
        // Second use is refused.
        assert!(matches!(
            s.consume_worker_enrollment("hash-b", 2500),
            Err(StorageError::Conflict(_))
        ));
        // Unknown token is refused.
        assert!(matches!(
            s.consume_worker_enrollment("nope", 2000),
            Err(StorageError::Conflict(_))
        ));
    }

    #[test]
    fn worker_enrollment_grants_selected_buckets_and_all_their_projects() {
        let s = store();
        add_workers(&s);
        let first = s
            .create_bucket_with_workers("first", &[0, 7], 0, false)
            .unwrap();
        let second = s.create_bucket("second").unwrap();
        let untouched = s.create_bucket("untouched").unwrap();
        let restricted = s
            .create_project_with_workers(first.id, "restricted", "/restricted", Some(7), &[7])
            .unwrap();
        let inherited = s
            .create_project(second.id, "inherited", "/inherited")
            .unwrap();
        let other = s.create_project(untouched.id, "other", "/other").unwrap();
        let (worker, buckets, projects) = s
            .create_pending_worker_enrollment_with_buckets(
                "hash",
                "sealed",
                "new-worker",
                "new-worker",
                ConnectMode::Dial,
                "",
                1000,
                5000,
                &[first.id, second.id, first.id],
            )
            .unwrap();
        assert_eq!(buckets.len(), 2);
        assert_eq!(projects.len(), 2);
        for bucket_id in [first.id, second.id] {
            assert!(s
                .get_bucket(bucket_id)
                .unwrap()
                .allowed_worker_ids
                .contains(&worker.id));
            assert_eq!(s.get_bucket(bucket_id).unwrap().default_worker_id, 0);
        }
        let restricted = s.get_project(restricted.id).unwrap();
        assert!(restricted.allowed_worker_ids.contains(&worker.id));
        assert_eq!(restricted.worker_id, Some(7));
        let inherited = s.get_project(inherited.id).unwrap();
        assert!(inherited.allowed_worker_ids.contains(&worker.id));
        assert_eq!(inherited.worker_id, None);
        assert_eq!(s.get_project(other.id).unwrap(), other);
        assert_eq!(s.get_bucket(untouched.id).unwrap(), untouched);
        let future = s.create_project(first.id, "future", "/future").unwrap();
        assert!(future.allowed_worker_ids.contains(&worker.id));
    }

    #[test]
    fn worker_enrollment_rejects_missing_buckets_without_partial_creation() {
        let s = store();
        let bucket = s.create_bucket("work").unwrap();
        let project = s.create_project(bucket.id, "project", "/project").unwrap();
        let missing_bucket = bucket.id + 1;
        let workers = s.list_workers().unwrap();
        assert!(matches!(s.create_pending_worker_enrollment_with_buckets(
            "hash", "sealed", "new-worker", "new-worker", ConnectMode::Dial, "",
            1000, 5000, &[bucket.id, missing_bucket],
        ), Err(StorageError::NotFound("bucket", id)) if id == missing_bucket));
        assert_eq!(s.list_workers().unwrap(), workers);
        assert_eq!(s.get_bucket(bucket.id).unwrap(), bucket);
        assert_eq!(s.get_project(project.id).unwrap(), project);
        assert!(s.live_worker_enrollments(1000).unwrap().is_empty());
    }

    #[test]
    fn pending_worker_is_visible_and_its_newest_enrollment_drives_the_dialer() {
        let s = store();
        let pending = s
            .create_pending_worker_enrollment(
                "hash-old",
                "sealed-old",
                "garage-box",
                "garage-box",
                ConnectMode::Accept,
                "garage.internal:7677",
                1000,
                5000,
            )
            .unwrap();
        assert_eq!(pending.name, "garage-box");
        assert_eq!(pending.hostname, "");
        assert_eq!(pending.last_seen_at_unix_ms, None);
        assert_eq!(pending.connect_mode, ConnectMode::Accept);
        assert_eq!(pending.endpoint, "garage.internal:7677");
        assert!(s.list_workers().unwrap().contains(&pending));

        s.create_worker_enrollment(
            "hash-new",
            "sealed-new",
            "garage-box",
            Some(pending.id),
            ConnectMode::Accept,
            "garage.internal:7677",
            2000,
            6000,
        )
        .unwrap();
        assert!(matches!(
            s.consume_worker_enrollment("hash-old", 2500),
            Err(StorageError::Conflict(_))
        ));
        assert_eq!(
            s.accept_mode_dial_targets(2500).unwrap(),
            vec![DialTarget {
                endpoint: "garage.internal:7677".into(),
                peer_key_hash: None,
                token_ciphertext: Some("sealed-new".into()),
            }]
        );

        let enrollment = s.consume_worker_enrollment("hash-new", 2500).unwrap();
        assert_eq!(enrollment.worker_id, Some(pending.id));
        s.rebind_worker(
            pending.id,
            "garage-box",
            "garage.local",
            "linux",
            "0.1.0+test",
            "/srv",
            "credential",
            "host-key",
            2500,
        )
        .unwrap();
        assert_eq!(
            s.accept_mode_dial_targets(2500).unwrap(),
            vec![DialTarget {
                endpoint: "garage.internal:7677".into(),
                peer_key_hash: Some("host-key".into()),
                token_ciphertext: None,
            }]
        );

        s.delete_worker(pending.id, 3000).unwrap();
        assert!(s.accept_mode_dial_targets(3000).unwrap().is_empty());
    }

    #[test]
    fn registering_a_worker_is_resolvable_by_its_pinned_key() {
        let s = store();
        let w = s
            .register_worker(
                "laptop",
                "host.local",
                "linux",
                "0.1.0+abc1234",
                "/home/dev",
                "cred-hash",
                "key-hash",
                1000,
            )
            .unwrap();
        assert_ne!(w.id, LOCAL_WORKER_ID);
        assert_eq!(w.name, "laptop");
        assert_eq!(w.pm_version, "0.1.0+abc1234");
        assert!(!w.online, "storage does not mark remote workers online");
        assert_eq!(s.worker_by_key_hash("key-hash").unwrap(), Some(w.id));
        assert_eq!(s.worker_by_key_hash("other").unwrap(), None);
        assert_eq!(
            s.get_worker_credential_hash(w.id).unwrap().as_deref(),
            Some("cred-hash")
        );

        s.delete_worker(w.id, 3000).unwrap();
        assert_eq!(s.worker_by_key_hash("key-hash").unwrap(), None);
        assert!(
            matches!(
                s.delete_worker(LOCAL_WORKER_ID, 3000),
                Err(StorageError::Conflict(_))
            ),
            "the local worker cannot be removed"
        );
    }

    #[test]
    fn auto_resume_query_and_worker_removal_share_desired_running_authority() {
        let (s, project_id) = store_with_project();
        let worker = s
            .register_worker(
                "remote",
                "host.local",
                "linux",
                "",
                "/home/dev",
                "credential",
                "key-remote",
                1000,
            )
            .unwrap();
        let eligible = s
            .create_session(
                project_id,
                AgentKind::Test,
                "eligible",
                "prompt",
                PermissionMode::Default,
                worker.id,
                true,
                false,
                None,
                1000,
            )
            .unwrap();
        let killed = s
            .create_session(
                project_id,
                AgentKind::Test,
                "killed",
                "prompt",
                PermissionMode::Default,
                worker.id,
                true,
                false,
                None,
                1000,
            )
            .unwrap();
        let killed_terminal = s.agent_terminal(killed.id).unwrap();
        s.set_agent_desired_running(killed.id, killed_terminal.id, false)
            .unwrap();
        s.set_session_ended(killed.id, SessionState::Exited, "", Some(0), 2000)
            .unwrap();

        assert_eq!(
            s.auto_resume_sessions_on_worker(worker.id)
                .unwrap()
                .into_iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            vec![eligible.id]
        );

        s.delete_worker(worker.id, 3000).unwrap();
        assert!(s
            .auto_resume_sessions_on_worker(worker.id)
            .unwrap()
            .is_empty());
        let removed = s.get_session(eligible.id).unwrap();
        assert_eq!(removed.state, SessionState::Failed);
        assert_eq!(removed.state_detail, "worker removed");
        assert!(!s.session_desired_running(eligible.id).unwrap());
        assert_eq!(
            s.get_session(killed.id).unwrap().state,
            SessionState::Exited
        );
    }

    #[test]
    fn sessions_and_projects_default_to_the_local_worker() {
        let (s, pid) = store_with_project();
        let sess = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        assert_eq!(sess.worker_id, LOCAL_WORKER_ID);
        assert_eq!(s.get_session(sess.id).unwrap().worker_id, LOCAL_WORKER_ID);
        let project = s.get_project(pid).unwrap();
        assert_eq!(project.worker_id, None, "no worker override by default");
        let bucket = s.get_bucket(project.bucket_id).unwrap();
        assert_eq!(bucket.default_worker_id, LOCAL_WORKER_ID);
    }

    #[test]
    fn resumable_reflects_worker_reported_availability() {
        let (s, pid) = store_with_project();
        let sess = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        assert!(
            !s.get_session(sess.id).unwrap().resumable,
            "no transcript yet"
        );

        s.set_agent_identity(sess.id, "aid", "/worker/agent.jsonl")
            .unwrap();
        assert!(
            s.get_session(sess.id).unwrap().resumable,
            "worker reported the conversation available"
        );

        s.set_agent_resumable(sess.id, false).unwrap();
        assert!(
            !s.get_session(sess.id).unwrap().resumable,
            "worker reported the conversation unavailable"
        );
    }

    #[test]
    fn session_tokens_round_trip_and_reject_unknown() {
        let (s, pid) = store_with_project();
        let sess = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        assert_eq!(s.get_session_token(sess.id).unwrap(), None);

        s.set_session_token(sess.id, "tok-1").unwrap();
        assert_eq!(s.get_session_token(sess.id).unwrap(), Some("tok-1".into()));
        assert_eq!(s.get_session_id_by_token("tok-1").unwrap(), Some(sess.id));
        assert_eq!(s.get_session_id_by_token("other").unwrap(), None);
    }

    #[test]
    fn v1_database_migrates_to_current_schema() {
        // The v1 sessions DDL, frozen as the migration's contract.
        const V1_DDL: &str = "
            CREATE TABLE buckets (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, position INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE projects (id INTEGER PRIMARY KEY, bucket_id INTEGER NOT NULL REFERENCES buckets(id), name TEXT NOT NULL, path TEXT NOT NULL, UNIQUE(bucket_id, name));
            CREATE TABLE sessions (
                id INTEGER PRIMARY KEY,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                agent TEXT NOT NULL, state TEXT NOT NULL,
                task_title TEXT NOT NULL, task_prompt TEXT NOT NULL,
                agent_session_id TEXT, created_at_unix_ms INTEGER NOT NULL,
                ended_at_unix_ms INTEGER, exit_code INTEGER,
                state_detail TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE activity_reports (id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, ts_unix_ms INTEGER NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL);
            INSERT INTO buckets (id, name, position) VALUES (1, 'b', 0);
            INSERT INTO projects (id, bucket_id, name, path) VALUES (1, 1, 'p', '/tmp');
            INSERT INTO sessions (project_id, agent, state, task_title, task_prompt, created_at_unix_ms)
                VALUES (1, 'test', 'exited', 't', 'p', 1000);
            INSERT INTO sessions (project_id, agent, state, task_title, task_prompt, created_at_unix_ms)
                VALUES (1, 'test', 'working', 'live', 'p', 1001);
            INSERT INTO activity_reports (session_id, ts_unix_ms, kind, payload)
                VALUES (1, 2000, 'status', '{}');
            PRAGMA user_version = 1;
        ";
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("old.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(V1_DDL).unwrap();
        }

        let s = Storage::open(&db).unwrap();
        s.set_session_token(1, "tok-after-migration").unwrap();
        assert_eq!(
            s.get_session_id_by_token("tok-after-migration").unwrap(),
            Some(1)
        );
        let migrated = s.get_session(1).unwrap();
        assert_eq!(migrated.activity, "");
        assert_eq!(migrated.progress_percent, None);
        assert_eq!(migrated.permission_mode, PermissionMode::Default);
        assert!(!s.session_desired_running(1).unwrap());
        assert!(s.session_desired_running(2).unwrap());
        assert!(!s
            .terminal_desired_running(s.agent_terminal(1).unwrap().id)
            .unwrap());
        assert!(s
            .terminal_desired_running(s.agent_terminal(2).unwrap().id)
            .unwrap());
        assert_eq!(migrated.last_agent_activity_at_unix_ms, 2000);
        assert_eq!(migrated.last_user_interaction_at_unix_ms, 0);
        assert_eq!(migrated.last_activity_at_unix_ms, 2000);

        let (item, _) = s
            .upsert_item(
                1,
                &ItemUpsert {
                    title: Some("pick storage".into()),
                    question: Some("Which database?".into()),
                    ..ItemUpsert::default()
                },
                None,
                3000,
            )
            .unwrap();
        assert_eq!(item.question, "Which database?");
        let item = s
            .respond_to_item(1, item.id, "SQLite.", false, 4000)
            .unwrap();
        assert!(item.question.is_empty());
        assert!(s
            .item_notes(1, item.id)
            .unwrap()
            .iter()
            .any(|note| note.kind == "user_reply" && note.text == "SQLite."));
    }

    #[test]
    fn record_activity_updates_live_fields_and_appends_history() {
        let (s, pid) = store_with_project();
        let sess = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();

        let updated = s
            .record_activity(
                sess.id,
                &ActivityUpdate {
                    state: SessionState::Working,
                    state_detail: "",
                    activity: "compiling",
                    progress_percent: Some(25),
                    kind: "status",
                    payload: "{}",
                    ts_unix_ms: 1000,
                },
            )
            .unwrap();
        assert_eq!(updated.activity, "compiling");
        assert_eq!(updated.progress_percent, Some(25));

        let count: i64 = s
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM activity_reports", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    /// An agent that blocks through its own report path asks for the user
    /// just as loudly as one whose adapter hook reports it, or the dashboard
    /// shows a blocked session with nothing calling attention to it.
    #[test]
    fn a_reported_block_raises_unseen_attention() {
        let (s, pid) = store_with_project();
        let sess = s
            .create_session(
                pid,
                AgentKind::ClaudeCode,
                "fix",
                "fix the bug",
                PermissionMode::Bypass,
                0,
                true,
                false,
                None,
                1000,
            )
            .unwrap();
        fn blocked(detail: &str) -> ActivityUpdate<'_> {
            ActivityUpdate {
                state: SessionState::NeedsInput,
                state_detail: detail,
                activity: "",
                progress_percent: None,
                kind: "blocked",
                payload: "{}",
                ts_unix_ms: 1000,
            }
        }

        let updated = s.record_activity(sess.id, &blocked("which port?")).unwrap();
        assert_eq!(updated.state, SessionState::NeedsInput);
        assert!(updated.needs_input_unseen);

        s.mark_session_seen(sess.id, 1_500).unwrap();
        assert!(
            !s.record_activity(sess.id, &blocked("still which port?"))
                .unwrap()
                .needs_input_unseen,
            "repeated signals in one lifecycle must not re-alert"
        );

        s.update_session_state(sess.id, SessionState::Working, "")
            .unwrap();
        assert!(
            s.record_activity(sess.id, &blocked("which port now?"))
                .unwrap()
                .needs_input_unseen,
            "a later transition resets attention"
        );
    }

    /// The watchdog that ends an unreported turn has to tell an agent whose
    /// hooks work from one whose hooks never fired, and it has to still know
    /// which is which after the daemon that saw the hook is gone.
    #[test]
    fn a_generations_hook_mark_outlives_the_process_that_recorded_it() {
        let (s, pid) = store_with_project();
        let session = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                100,
            )
            .unwrap();
        let terminal = s.agent_terminal(session.id).unwrap();

        let (started_at, seen) = s
            .generation_hook_mark(terminal.id, terminal.generation)
            .unwrap()
            .expect("the run row exists from the spawn");
        assert_eq!(started_at, 100);
        assert_eq!(seen, None, "no hook has arrived yet");

        s.record_generation_hook_seen(terminal.id, terminal.generation, 4_000)
            .unwrap();
        assert_eq!(
            s.generation_hook_mark(terminal.id, terminal.generation)
                .unwrap(),
            Some((100, Some(4_000)))
        );

        // A relaunch is a new generation, and starts over with no evidence
        // that its hooks work.
        let restarted = s.restart_terminal(terminal.id, 5_000).unwrap();
        assert!(restarted.generation > terminal.generation);
        let previous: (String, Option<i64>) = s
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT state, ended_at_unix_ms FROM terminal_runs \
                 WHERE terminal_id = ?1 AND generation = ?2",
                params![terminal.id as i64, terminal.generation as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(previous, ("exited".into(), Some(5_000)));
        assert_eq!(
            s.generation_hook_mark(terminal.id, restarted.generation)
                .unwrap()
                .map(|(_, seen)| seen),
            Some(None),
            "a fresh generation inherits nothing from the last one"
        );
    }

    #[test]
    fn terminal_activity_checkpoints_track_both_clocks_and_derive_recency() {
        let (s, pid) = store_with_project();
        let session = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                100,
            )
            .unwrap();

        let updated = s
            .checkpoint_session_activity(&[SessionActivityUpdate {
                session_id: session.id,
                last_agent_activity_at_unix_ms: Some(200),
                last_user_interaction_at_unix_ms: Some(300),
                last_user_submit_at_unix_ms: None,
            }])
            .unwrap();
        assert_eq!(updated.len(), 1);
        let current = s.get_session(session.id).unwrap();
        assert_eq!(current.last_agent_activity_at_unix_ms, 200);
        assert_eq!(current.last_user_interaction_at_unix_ms, 300);
        assert_eq!(
            current.last_activity_at_unix_ms, 100,
            "output and unsubmitted typing leave the shown clock at creation"
        );

        s.checkpoint_session_activity(&[SessionActivityUpdate {
            session_id: session.id,
            last_agent_activity_at_unix_ms: Some(150),
            last_user_interaction_at_unix_ms: Some(250),
            last_user_submit_at_unix_ms: Some(250),
        }])
        .unwrap();
        let current = s.get_session(session.id).unwrap();
        assert_eq!(current.last_agent_activity_at_unix_ms, 200);
        assert_eq!(current.last_user_interaction_at_unix_ms, 300);
        assert_eq!(current.last_activity_at_unix_ms, 250);

        s.record_agent_turn(session.id, 400).unwrap();
        assert_eq!(
            s.get_session(session.id).unwrap().last_activity_at_unix_ms,
            400
        );
        s.record_agent_turn(session.id, 350).unwrap();
        assert_eq!(
            s.get_session(session.id).unwrap().last_activity_at_unix_ms,
            400
        );

        s.append_user_note(session.id, 900, "{}").unwrap();
        assert_eq!(
            s.get_session(session.id).unwrap().last_activity_at_unix_ms,
            400,
            "a note about the user's own action is not agent activity"
        );
        s.append_checkpoint(session.id, 500, "{}").unwrap();
        assert_eq!(
            s.get_session(session.id).unwrap().last_activity_at_unix_ms,
            500
        );
        assert!(s.record_agent_turn(9999, 1).is_err());
    }

    #[test]
    fn migrating_a_v52_database_adds_the_submit_and_turn_clocks() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v52.db");
        let old_id = {
            let s = Storage::open(&db).unwrap();
            let bucket = s.create_bucket("work").unwrap();
            let pid = s.create_project(bucket.id, "api", "/tmp/api").unwrap().id;
            let id = session_for_context(&s, pid);
            s.checkpoint_session_activity(&[SessionActivityUpdate {
                session_id: id,
                last_agent_activity_at_unix_ms: Some(200),
                last_user_interaction_at_unix_ms: Some(300),
                last_user_submit_at_unix_ms: None,
            }])
            .unwrap();
            let conn = s.conn.lock().unwrap();
            conn.execute_batch(
                "ALTER TABLE session_activity DROP COLUMN last_user_submit_at_unix_ms;
                 ALTER TABLE session_activity DROP COLUMN last_agent_turn_at_unix_ms;",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 52).unwrap();
            id
        };

        let s = Storage::open(&db).unwrap();
        let old = s.get_session(old_id).unwrap();
        assert_eq!(old.last_agent_activity_at_unix_ms, 200);
        assert_eq!(old.last_user_interaction_at_unix_ms, 300);
        s.record_agent_turn(old_id, 700).unwrap();
        assert_eq!(s.get_session(old_id).unwrap().last_activity_at_unix_ms, 700);
    }

    fn session_for_context(s: &Storage, pid: u64) -> u64 {
        s.create_session(
            pid,
            AgentKind::Test,
            "t",
            "p",
            PermissionMode::Default,
            0,
            true,
            false,
            None,
            1,
        )
        .unwrap()
        .id
    }

    fn field(key: &str, value: &str) -> ContextField {
        ContextField {
            key: key.into(),
            label: key.into(),
            value: value.into(),
            kind: ContextKind::Text,
            severity: ContextSeverity::Neutral,
        }
    }

    #[test]
    fn headline_summary_and_checkpoint_persist() {
        let (s, pid) = store_with_project();
        let id = session_for_context(&s, pid);
        let sess = s
            .set_headline_summary(id, "migrating auth", "swap to jwt")
            .unwrap();
        assert_eq!(sess.headline, "migrating auth");
        assert_eq!(sess.summary, "swap to jwt");
        s.append_checkpoint(id, 1000, "{\"note\":\"green\"}")
            .unwrap();
        let reports = s.activity_reports(id).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].kind, CHECKPOINT_KIND);
    }

    #[test]
    fn create_session_seeds_the_goal_from_the_title() {
        let (s, pid) = store_with_project();
        let id = session_for_context(&s, pid);
        assert_eq!(s.get_session(id).unwrap().goal, "t");
    }

    #[test]
    fn set_goal_persists_and_is_searchable() {
        let (s, pid) = store_with_project();
        let id = session_for_context(&s, pid);
        let sess = s.set_goal(id, "Optimizing femtocell software").unwrap();
        assert_eq!(sess.goal, "Optimizing femtocell software");
        let page = s.search_sessions("femtocell", "", 50).unwrap();
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].id, id);
    }

    #[test]
    fn seed_goal_prefers_the_trimmed_title() {
        assert_eq!(seed_goal("  Fix auth  ", "ignored prompt"), "Fix auth");
        let long_title = "x".repeat(GOAL_MAX + 5);
        assert_eq!(seed_goal(&long_title, "").chars().count(), GOAL_MAX);
    }

    #[test]
    fn seed_goal_takes_the_first_sentence_of_the_prompt() {
        assert_eq!(
            seed_goal("", "Optimize the femtocell scheduler. Then profile it."),
            "Optimize the femtocell scheduler"
        );
        assert_eq!(
            seed_goal("", "\n\n# Rename the Run button\n\nDetails follow."),
            "Rename the Run button"
        );
        assert_eq!(
            seed_goal("", "Bump v1.2 to v1.3? please"),
            "Bump v1.2 to v1.3"
        );
        assert_eq!(seed_goal("", "   "), "");
    }

    #[test]
    fn seed_goal_cuts_a_long_sentence_at_a_word_boundary() {
        let prompt =
            "Investigate why the worker link drops every few minutes on the remote host and fix it";
        let goal = seed_goal("", prompt);
        assert!(goal.chars().count() <= GOAL_SEED_MAX, "{goal}");
        assert!(goal.ends_with('…'));
        assert!(prompt.starts_with(goal.trim_end_matches('…')));
        assert!(!goal.trim_end_matches('…').ends_with(' '));
    }

    #[test]
    fn migrating_a_v51_database_adds_the_goal_and_indexes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v51.db");
        let old_id = {
            let s = Storage::open(&db).unwrap();
            let bucket = s.create_bucket("work").unwrap();
            let pid = s.create_project(bucket.id, "api", "/tmp/api").unwrap().id;
            let id = session_for_context(&s, pid);
            let conn = s.conn.lock().unwrap();
            conn.execute_batch(
                "DROP TRIGGER session_search_insert;
                 DROP TRIGGER session_search_delete;
                 DROP TRIGGER session_search_update;
                 DROP TRIGGER session_search_project_name;
                 DROP TRIGGER session_search_bucket_name;
                 DROP TABLE session_search;
                 DROP INDEX idx_sessions_ended_page;
                 ALTER TABLE sessions DROP COLUMN goal;",
            )
            .unwrap();
            conn.execute_batch(MIGRATE_V25_TO_V26).unwrap();
            conn.pragma_update(None, "user_version", 51).unwrap();
            id
        };

        let s = Storage::open(&db).unwrap();
        let old = s.get_session(old_id).unwrap();
        assert_eq!(old.goal, "", "existing sessions are not backfilled");
        assert_eq!(s.search_sessions("t", "", 50).unwrap().sessions.len(), 1);
        s.set_goal(old_id, "Needle goal").unwrap();
        assert_eq!(
            s.search_sessions("Needle", "", 50).unwrap().sessions[0].id,
            old_id
        );
    }

    #[test]
    fn glance_replaces_wholesale_and_keeps_order() {
        let (s, pid) = store_with_project();
        let id = session_for_context(&s, pid);
        s.replace_glance(id, &[field("a", "1"), field("b", "2")])
            .unwrap();
        assert_eq!(s.session_context(id).unwrap().glance.len(), 2);
        s.replace_glance(id, &[field("c", "3")]).unwrap();
        let glance = s.session_context(id).unwrap().glance;
        assert_eq!(glance.len(), 1);
        assert_eq!(glance[0].key, "c");
    }

    #[test]
    fn context_upserts_by_key_and_clears() {
        let (s, pid) = store_with_project();
        let id = session_for_context(&s, pid);
        assert_eq!(
            s.upsert_context(id, &[field("branch", "main")], 1).unwrap(),
            0
        );
        s.upsert_context(id, &[field("branch", "dev"), field("url", "x")], 2)
            .unwrap();
        let detail = s.session_context(id).unwrap().detail;
        assert_eq!(detail.len(), 2);
        assert_eq!(
            detail.iter().find(|f| f.key == "branch").unwrap().value,
            "dev"
        );
        s.clear_context(id, &["branch".into()]).unwrap();
        let detail = s.session_context(id).unwrap().detail;
        assert_eq!(detail.len(), 1);
        assert_eq!(detail[0].key, "url");
    }

    #[test]
    fn context_is_capped_and_reports_drops() {
        let (s, pid) = store_with_project();
        let id = session_for_context(&s, pid);
        let full: Vec<ContextField> = (0..CONTEXT_MAX_FIELDS)
            .map(|i| field(&format!("k{i}"), "v"))
            .collect();
        assert_eq!(s.upsert_context(id, &full, 1).unwrap(), 0);
        // Updates to existing keys still apply past the cap; new keys drop.
        let dropped = s
            .upsert_context(id, &[field("k0", "updated"), field("new", "v")], 2)
            .unwrap();
        assert_eq!(dropped, 1);
        let detail = s.session_context(id).unwrap().detail;
        assert_eq!(detail.len(), CONTEXT_MAX_FIELDS);
        assert_eq!(
            detail.iter().find(|f| f.key == "k0").unwrap().value,
            "updated"
        );
    }

    #[test]
    fn snapshot_carries_context_bags() {
        let (s, pid) = store_with_project();
        let id = session_for_context(&s, pid);
        s.replace_glance(id, &[field("status", "building")])
            .unwrap();
        s.upsert_context(id, &[field("branch", "main")], 1).unwrap();
        let contexts = s.snapshot(0).unwrap().contexts;
        let ctx = contexts.iter().find(|c| c.session_id == id).unwrap();
        assert_eq!(ctx.glance.len(), 1);
        assert_eq!(ctx.detail.len(), 1);
    }

    #[test]
    fn newer_schema_versions_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("future.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }
        assert!(matches!(
            Storage::open(&db),
            Err(StorageError::SchemaTooNew(_))
        ));
    }

    #[test]
    fn previous_schema_version_is_migrated() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("previous.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
                .unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        let conn = storage.conn.lock().unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let has_session_search: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_search'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(has_session_search, 1);
    }

    /// Before enrollment correlated on the installation id, every login
    /// added a device row and each one kept its own push endpoint, so one
    /// phone received a copy of every notification per login. The
    /// migration has to leave one live row per installation behind.
    #[test]
    fn migrating_collapses_duplicate_rows_for_one_installation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("duplicate-devices.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(
                "DROP INDEX idx_mobile_devices_installation;
                 INSERT INTO users (id, username, password_hash, created_at_unix_ms)
                    VALUES (1, 'testuser', 'hash', 1), (2, 'sam', 'hash', 1);
                 INSERT INTO mobile_devices
                    (id, user_id, app_installation_id, refresh_family_id, created_at_unix_ms)
                    VALUES (1, 1, 'phone-a', 'family-1', 1),
                           (2, 1, 'phone-a', 'family-2', 2),
                           (3, 1, 'phone-a', 'family-3', 3),
                           (4, 1, 'phone-b', 'family-4', 4),
                           (5, 2, 'phone-a', 'family-5', 5);
                 INSERT INTO mobile_access_tokens
                    (token_hash, device_id, created_at_unix_ms, expires_at_unix_ms)
                    VALUES ('hash-1', 1, 1, 9), ('hash-3', 3, 3, 9);
                 INSERT INTO mobile_push_endpoints
                    (device_id, token_ciphertext, environment, event_mask,
                     created_at_unix_ms, updated_at_unix_ms)
                    VALUES (1, 'sealed-1', 'sandbox', 7, 1, 1),
                           (3, 'sealed-3', 'sandbox', 7, 3, 3);",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
                .unwrap();
        }

        let storage = Storage::open(&db).unwrap();

        let live = storage.list_mobile_devices(1).unwrap();
        assert_eq!(
            live.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![3, 4],
            "the newest row per installation survives, one per installation"
        );
        assert_eq!(
            storage.list_mobile_devices(2).unwrap()[0].id,
            5,
            "the same installation under another user is a separate device"
        );
        assert!(storage
            .get_mobile_device(1)
            .unwrap()
            .revoked_at_unix_ms
            .is_some());
        assert!(storage.get_push_endpoint(1).unwrap().is_none());
        assert!(
            storage.get_push_endpoint(3).unwrap().is_some(),
            "the surviving row keeps its push endpoint"
        );
        assert!(storage.lookup_access_token("hash-1", 5).unwrap().is_none());
        assert!(storage.lookup_access_token("hash-3", 5).unwrap().is_some());

        let conn = Connection::open(&db).unwrap();
        assert!(conn
            .execute(
                "INSERT INTO mobile_devices
                    (user_id, app_installation_id, refresh_family_id, created_at_unix_ms)
                    VALUES (1, 'phone-b', 'family-6', 6)",
                [],
            )
            .is_err());
    }

    /// A database that predates gateway-only delivery carries a provider
    /// column. Dropping it is a table rebuild, so the rows have to survive.
    #[test]
    fn dropping_the_provider_column_keeps_the_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("push-provider.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(
                "INSERT INTO users (id, username, password_hash, created_at_unix_ms)
                    VALUES (1, 'testuser', 'hash', 1);
                 INSERT INTO mobile_devices
                    (id, user_id, app_installation_id, refresh_family_id, created_at_unix_ms)
                    VALUES (7, 1, 'install-1', 'family-1', 1);
                 DROP TABLE mobile_push_endpoints;
                 CREATE TABLE mobile_push_endpoints (
                    device_id INTEGER PRIMARY KEY REFERENCES mobile_devices(id) ON DELETE CASCADE,
                    provider TEXT NOT NULL CHECK(provider IN ('apns','fcm','gateway')),
                    token_ciphertext TEXT NOT NULL,
                    environment TEXT NOT NULL CHECK(environment IN ('production','sandbox')),
                    locale TEXT NOT NULL DEFAULT '',
                    previews_enabled INTEGER NOT NULL DEFAULT 0,
                    event_mask INTEGER NOT NULL,
                    public_key TEXT NOT NULL DEFAULT '',
                    push_counter INTEGER NOT NULL DEFAULT 0,
                    created_at_unix_ms INTEGER NOT NULL,
                    updated_at_unix_ms INTEGER NOT NULL,
                    disabled_at_unix_ms INTEGER,
                    disabled_reason TEXT NOT NULL DEFAULT ''
                 );
                 INSERT INTO mobile_push_endpoints
                    (device_id, provider, token_ciphertext, environment, locale,
                     previews_enabled, event_mask, public_key, push_counter,
                     created_at_unix_ms, updated_at_unix_ms)
                    VALUES (7, 'apns', 'sealed-token', 'sandbox', 'en-US', 1, 7, 'pk', 4, 1, 1);",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 42i64).unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        let endpoint = storage.get_push_endpoint(7).unwrap().unwrap();
        assert_eq!(endpoint.token_ciphertext, "sealed-token");
        assert_eq!(endpoint.environment, "sandbox");
        assert_eq!(endpoint.locale, "en-US");
        assert!(endpoint.previews_enabled);
        assert_eq!(endpoint.event_mask, 7);
        assert_eq!(endpoint.public_key, "pk");
        assert_eq!(
            endpoint.push_counter, 4,
            "the replay counter must not restart, or the device drops the next push"
        );

        let conn = Connection::open(&db).unwrap();
        let has_provider: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('mobile_push_endpoints') WHERE name='provider'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_provider, 0, "the provider column should be gone");
    }

    #[test]
    fn v28_migration_adds_the_notifications_read_model() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("notifications-v28.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch("DROP TABLE notifications;").unwrap();
            conn.pragma_update(None, "user_version", 28).unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        let bucket = storage.create_bucket("work").unwrap();
        let project = storage
            .create_project(bucket.id, "api", "/tmp/api")
            .unwrap();
        let session = storage
            .create_session(
                project.id,
                AgentKind::Test,
                "blocked",
                "prompt",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        assert!(
            storage
                .update_session_state(session.id, SessionState::NeedsInput, "question")
                .unwrap()
                .needs_input_unseen
        );
    }

    #[test]
    fn default_agent_migration_preserves_existing_catalogs_and_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("agent-defaults-v27.db");
        {
            let conn = Connection::open(&db).unwrap();
            let old_schema = SCHEMA
                .replace("    default_agent TEXT,\n", "")
                .replace("    agent_source TEXT NOT NULL DEFAULT 'explicit',\n", "");
            conn.execute_batch(&old_schema).unwrap();
            conn.execute("INSERT INTO buckets (id,name,position,permission_mode,default_worker_id,is_default) VALUES (7,'kept',0,'default',0,1)", []).unwrap();
            conn.execute(
                "INSERT INTO bucket_workers (bucket_id,worker_id) VALUES (7,0)",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO projects (id,bucket_id,name,path,permission_mode,worker_id) VALUES (9,7,'api','/srv/api','inherit',NULL)", []).unwrap();
            conn.execute(
                "INSERT INTO project_workers (project_id,worker_id) VALUES (9,0)",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO sessions (id,project_id,agent,state,task_title,task_prompt,created_at_unix_ms,desired_running) VALUES (11,9,'codex','exited','kept session','',1,0)", []).unwrap();
            conn.pragma_update(None, "user_version", 27).unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        let bucket = storage.get_bucket(7).unwrap();
        let project = storage.get_project(9).unwrap();
        let session = storage.get_session(11).unwrap();
        assert_eq!(bucket.name, "kept");
        assert_eq!(bucket.allowed_worker_ids, vec![0]);
        assert_eq!(bucket.default_agent, None);
        assert_eq!(project.path, "/srv/api");
        assert_eq!(project.allowed_worker_ids, vec![0]);
        assert_eq!(project.default_agent, None);
        assert_eq!(session.agent, AgentKind::Codex);
        assert_eq!(session.agent_source, AgentSelectionSource::Explicit);
    }

    /// The schema as it stood before model profiles, so a migration
    /// test starts from a database that never had them.
    fn schema_without_model_profiles() -> String {
        const TABLES_END: &str = "    PRIMARY KEY (profile_id, dialect)\n);\n";
        let start = SCHEMA.find("CREATE TABLE model_profiles (").unwrap();
        let end = start + SCHEMA[start..].find(TABLES_END).unwrap() + TABLES_END.len();
        let mut schema = SCHEMA.to_string();
        schema.replace_range(start..end, "");
        schema
            .replace(
                "    is_default INTEGER NOT NULL DEFAULT 0,\n    model_profile_id INTEGER REFERENCES model_profiles(id)\n",
                "    is_default INTEGER NOT NULL DEFAULT 0\n",
            )
            .replace(
                "    model_profile_id INTEGER REFERENCES model_profiles(id),\n    UNIQUE(bucket_id, name)\n",
                "    UNIQUE(bucket_id, name)\n",
            )
            .replace(
                "    ,model_profile_id INTEGER REFERENCES model_profiles(id) ON DELETE SET NULL\n    ,model_profile_source TEXT\n",
                "",
            )
    }

    #[test]
    fn model_profile_migration_preserves_catalogs_and_leaves_sessions_unattached() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("model-profiles-v32.db");
        {
            let conn = Connection::open(&db).unwrap();
            let old_schema = schema_without_model_profiles();
            assert!(!old_schema.contains("model_profile"), "{old_schema}");
            conn.execute_batch(&old_schema).unwrap();
            conn.execute("INSERT INTO buckets (id,name,position,permission_mode,default_worker_id,is_default) VALUES (7,'kept',0,'default',0,1)", []).unwrap();
            conn.execute(
                "INSERT INTO bucket_workers (bucket_id,worker_id) VALUES (7,0)",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO projects (id,bucket_id,name,path,permission_mode,worker_id) VALUES (9,7,'api','/srv/api','inherit',NULL)", []).unwrap();
            conn.execute(
                "INSERT INTO project_workers (project_id,worker_id) VALUES (9,0)",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO sessions (id,project_id,agent,state,task_title,task_prompt,created_at_unix_ms,desired_running) VALUES (11,9,'codex','exited','kept session','',1,0)", []).unwrap();
            conn.pragma_update(None, "user_version", 32).unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        let version: i64 = storage
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let bucket = storage.get_bucket(7).unwrap();
        let project = storage.get_project(9).unwrap();
        let session = storage.get_session(11).unwrap();
        assert_eq!(bucket.name, "kept");
        assert_eq!(bucket.allowed_worker_ids, vec![0]);
        assert_eq!(bucket.model_profile_id, None);
        assert_eq!(project.path, "/srv/api");
        assert_eq!(project.model_profile_id, None);
        assert_eq!(session.agent, AgentKind::Codex);
        assert_eq!(session.model_profile_id, None);
        assert_eq!(session.model_profile_source, None);
        assert!(storage.list_model_profiles().unwrap().is_empty());

        let profile = storage
            .create_model_profile("gateway", Some("v1:ciphertext"), 1000)
            .unwrap();
        storage
            .set_bucket_model_profile(7, Some(profile.id))
            .unwrap();
        assert_eq!(
            storage.get_bucket(7).unwrap().model_profile_id,
            Some(profile.id)
        );
    }

    #[test]
    fn upgrade_drops_per_worker_project_paths_recorded_by_earlier_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("project-paths-v33.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO workers (id,name,created_at_unix_ms) VALUES (4,'remote',0)",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO buckets (id,name) VALUES (1,'bucket')", [])
                .unwrap();
            conn.execute(
                "INSERT INTO projects (id,bucket_id,name,path) VALUES (2,1,'api','/srv/api')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO project_paths (project_id,worker_id,path) VALUES (2,0,'/legacy/api'),(2,4,'/home/dev/api')",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 33).unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        let project = storage.get_project(2).unwrap();
        assert_eq!(project.path, "/srv/api");
        assert!(project.worker_paths.is_empty());
        assert_eq!(storage.project_path_for_worker(2, 4).unwrap(), None);
    }

    /// Upgrading the schema must never disturb a path an operator configured.
    /// The v33 cleanup ran on any database older than the current version, so
    /// each release that bumped the schema silently cleared every per-worker
    /// project path, which is what made them look like they reset themselves.
    #[test]
    fn upgrading_the_schema_keeps_configured_per_worker_project_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("configured-paths.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(
                "INSERT INTO workers (id,name,peer_key_hash,created_at_unix_ms) \
                     VALUES (4,'mac-vm','key-a',0);
                 INSERT INTO buckets (id,name) VALUES (1,'bucket');
                 INSERT INTO projects (id,bucket_id,name,path) VALUES (2,1,'api','/srv/api');
                 INSERT INTO project_paths (project_id,worker_id,path) \
                     VALUES (2,4,'/Users/admin/api');",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
                .unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        assert_eq!(
            storage.project_path_for_worker(2, 4).unwrap().as_deref(),
            Some("/Users/admin/api"),
            "an upgrade must not clear a path the operator configured"
        );
    }

    /// The same cleanup still has to run for the databases it was written
    /// for, where the rows are copies a spawn froze rather than configuration.
    #[test]
    fn upgrading_from_the_writeback_era_still_drops_its_frozen_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("writeback-paths.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(
                "INSERT INTO workers (id,name,peer_key_hash,created_at_unix_ms) \
                     VALUES (4,'mac-vm','key-a',0);
                 INSERT INTO buckets (id,name) VALUES (1,'bucket');
                 INSERT INTO projects (id,bucket_id,name,path) VALUES (2,1,'api','/srv/api');
                 INSERT INTO project_paths (project_id,worker_id,path) \
                     VALUES (2,4,'/frozen/api');",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 33).unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        assert_eq!(storage.project_path_for_worker(2, 4).unwrap(), None);
    }

    /// A pinned key is a machine's identity, so the database itself has to
    /// refuse a second row holding one. Without that, enrolling an already
    /// enrolled machine as a new Host left its per-worker project paths on
    /// an id nothing connects as any more, which reads as the path clearing
    /// itself.
    #[test]
    fn a_second_host_cannot_hold_a_key_another_host_is_enrolled_with() {
        let s = store();
        s.register_worker(
            "laptop",
            "host.local",
            "linux",
            "0.1.0",
            "/home/dev",
            "cred-a",
            "key-a",
            1,
        )
        .unwrap();

        let duplicate = s.register_worker(
            "laptop-again",
            "host.local",
            "linux",
            "0.1.0",
            "/home/dev",
            "cred-b",
            "key-a",
            2,
        );
        assert!(
            duplicate.is_err(),
            "a key already enrolled must not open a second Host row"
        );

        // A Host that has not enrolled yet holds no key, and any number of
        // those may be pending at once.
        s.register_worker("a", "a.local", "linux", "", "", "cred-c", "", 3)
            .unwrap();
        s.register_worker("b", "b.local", "linux", "", "", "cred-d", "", 4)
            .unwrap();
    }

    /// Databases written before the unique index could already hold two rows
    /// for one machine. The upgrade has to merge them rather than refuse to
    /// open, and the surviving row is the one configuration already points
    /// at, carrying what the newest registration reported.
    #[test]
    fn upgrade_merges_hosts_that_share_a_pinned_key_and_keeps_their_project_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("duplicate-hosts-v43.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch("DROP INDEX workers_peer_key_hash;")
                .unwrap();
            conn.execute_batch(
                "INSERT INTO workers (id,name,hostname,platform,pm_version,default_project_root,\
                     credential_hash,peer_key_hash,created_at_unix_ms,last_seen_at_unix_ms)
                     VALUES (4,'mac-vm','mac.local','macos','0.1.0','/Users/admin','cred-old','key-a',1,10),
                            (5,'mac-vm','mac.local','macos','0.3.9','/Users/admin','cred-new','key-a',2,20);
                 INSERT INTO buckets (id,name) VALUES (1,'bucket');
                 INSERT INTO projects (id,bucket_id,name,path) VALUES (2,1,'api','/srv/api');
                 INSERT INTO project_paths (project_id,worker_id,path) VALUES (2,4,'/Users/admin/api');
                 INSERT INTO bucket_workers (bucket_id,worker_id) VALUES (1,4);
                 INSERT INTO project_workers (project_id,worker_id) VALUES (2,5);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id,project_id,worker_id,agent,state,task_title,\
                 task_prompt,created_at_unix_ms) \
                 VALUES (8,2,5,'claude','running','s','',3)",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 43).unwrap();
        }

        let storage = Storage::open(&db).unwrap();

        let workers = storage.list_workers().unwrap();
        assert_eq!(
            workers.iter().filter(|w| w.name == "mac-vm").count(),
            1,
            "the two rows for one machine merge into one Host"
        );
        assert!(storage.get_worker(5).is_err(), "the later row is gone");

        let kept = storage.get_worker(4).unwrap();
        assert_eq!(
            kept.pm_version, "0.3.9",
            "the surviving row carries what the newest registration reported"
        );
        assert_eq!(
            storage.get_worker_credential_hash(4).unwrap().as_deref(),
            Some("cred-new"),
            "the credential the live host holds must keep working"
        );

        assert_eq!(
            storage.project_path_for_worker(2, 4).unwrap().as_deref(),
            Some("/Users/admin/api"),
            "the configured path survives the merge"
        );
        let session_worker: i64 = storage
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT worker_id FROM sessions WHERE id = 8", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            session_worker, 4,
            "history on the discarded row moves to the surviving Host"
        );
        assert_eq!(storage.get_project(2).unwrap().allowed_worker_ids, vec![4]);
    }

    #[test]
    fn upgrade_adds_worker_pm_version_and_reads_existing_rows_as_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("workers-v34.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(
                "DROP TABLE workers;
                 CREATE TABLE workers (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     hostname TEXT NOT NULL DEFAULT '',
                     platform TEXT NOT NULL DEFAULT '',
                     default_project_root TEXT NOT NULL DEFAULT '',
                     credential_hash TEXT,
                     created_at_unix_ms INTEGER NOT NULL,
                     last_seen_at_unix_ms INTEGER
                 );
                 INSERT INTO workers (id, name, created_at_unix_ms) VALUES (0, 'local', 0);
                 INSERT INTO workers (id, name, hostname, platform, credential_hash, created_at_unix_ms) \
                     VALUES (3, 'laptop', 'host.local', 'linux', 'cred', 1);",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 34).unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        let version: i64 = storage
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let migrated = storage.get_worker(3).unwrap();
        assert_eq!(
            migrated.pm_version, "",
            "a pre-upgrade worker has no reported build"
        );
        storage
            .set_worker_report(3, "0.1.0+abc1234", "", "")
            .unwrap();
        assert_eq!(storage.get_worker(3).unwrap().pm_version, "0.1.0+abc1234");
        storage.set_worker_report(3, "", "", "").unwrap();
        assert_eq!(
            storage.get_worker(3).unwrap().pm_version,
            "",
            "a reconnect without a version clears the stale build"
        );
    }

    #[test]
    fn model_profile_entries_are_unique_per_dialect_and_upsert_in_place() {
        let s = store();
        let profile = s.create_model_profile("gateway", None, 1000).unwrap();
        assert!(!profile.key_set);
        assert!(matches!(
            s.create_model_profile("gateway", None, 1000),
            Err(StorageError::Conflict(_))
        ));

        s.set_model_profile_endpoint(
            profile.id,
            ModelDialect::AnthropicMessages,
            "gw/big",
            "https://gw.example/v1",
            "gw/small",
            1001,
        )
        .unwrap();
        let updated = s
            .set_model_profile_endpoint(
                profile.id,
                ModelDialect::AnthropicMessages,
                "gw/bigger",
                "https://gw.example/v1",
                "",
                1002,
            )
            .unwrap();
        assert_eq!(updated.endpoints.len(), 1);
        assert_eq!(updated.endpoints[0].model, "gw/bigger");
        assert_eq!(updated.endpoints[0].background_model, "");

        s.set_model_profile_endpoint(
            profile.id,
            ModelDialect::OpenaiResponses,
            "gw/o",
            "",
            "",
            1003,
        )
        .unwrap();
        assert_eq!(s.get_model_profile(profile.id).unwrap().endpoints.len(), 2);

        assert!(matches!(
            s.set_model_profile_endpoint(
                profile.id + 99,
                ModelDialect::AnthropicMessages,
                "m",
                "",
                "",
                1004
            ),
            Err(StorageError::NotFound("model profile", _))
        ));

        s.delete_model_profile_endpoint(profile.id, ModelDialect::OpenaiResponses)
            .unwrap();
        assert_eq!(s.get_model_profile(profile.id).unwrap().endpoints.len(), 1);
        // Deleting the profile takes its entries with it.
        s.delete_model_profile(profile.id).unwrap();
        assert!(matches!(
            s.get_model_profile(profile.id),
            Err(StorageError::NotFound("model profile", _))
        ));
    }

    #[test]
    fn users_are_unique_and_lookup_returns_hash() {
        let s = store();
        assert_eq!(s.user_count().unwrap(), 0);
        let uid = s.create_user("testuser", "hash123", 1000).unwrap();
        assert_eq!(s.user_count().unwrap(), 1);
        assert!(matches!(
            s.create_user("testuser", "other", 1000),
            Err(StorageError::Conflict(_))
        ));
        assert_eq!(
            s.get_user_by_name("testuser").unwrap(),
            Some((uid, "hash123".into()))
        );
        assert_eq!(s.get_user_by_name("nobody").unwrap(), None);
        assert_eq!(s.get_username(uid).unwrap(), Some("testuser".into()));
    }

    #[test]
    fn user_settings_are_isolated_reset_and_cascade_with_the_user() {
        let s = store();
        let first = s.create_user("first", "h", 1000).unwrap();
        let second = s.create_user("second", "h", 1000).unwrap();
        s.set_user_setting(first, "terminal.theme", Some("first-theme"))
            .unwrap();
        s.set_user_setting(second, "terminal.theme", Some("second-theme"))
            .unwrap();

        assert_eq!(
            s.get_user_setting(first, "terminal.theme").unwrap(),
            Some("first-theme".into())
        );
        assert_eq!(
            s.get_user_setting(second, "terminal.theme").unwrap(),
            Some("second-theme".into())
        );
        s.set_user_setting(first, "terminal.theme", None).unwrap();
        assert_eq!(s.get_user_setting(first, "terminal.theme").unwrap(), None);
        assert_eq!(s.list_user_settings(second).unwrap().len(), 1);

        s.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM users WHERE id = ?1", params![second as i64])
            .unwrap();
        assert!(s.list_user_settings(second).unwrap().is_empty());
    }

    #[test]
    fn migrating_a_v18_database_adds_user_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v18.db");
        let seeded = Storage::open(&db).unwrap();
        seeded.create_user("testuser", "h", 1).unwrap();
        drop(seeded);

        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("DROP TABLE user_settings;").unwrap();
        conn.pragma_update(None, "user_version", 18i64).unwrap();
        drop(conn);

        let migrated = Storage::open(&db).unwrap();
        migrated
            .set_user_setting(1, "terminal.theme", Some("theme"))
            .unwrap();
        assert_eq!(
            migrated.get_user_setting(1, "terminal.theme").unwrap(),
            Some("theme".into())
        );
    }

    #[test]
    fn migrating_v19_maps_supervisor_grants_to_first_class_roles() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v19.db");
        let seeded = Storage::open(&db).unwrap();
        let b = seeded.create_bucket("b").unwrap();
        let p = seeded.create_project(b.id, "p", "/p").unwrap();
        let worker = seeded
            .create_session(
                p.id,
                AgentKind::Codex,
                "w",
                "",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let supervisor = seeded
            .create_session(
                p.id,
                AgentKind::Codex,
                "s",
                "",
                PermissionMode::Default,
                0,
                true,
                true,
                None,
                2,
            )
            .unwrap();
        drop(seeded);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("DROP TABLE session_instruction_snapshots; DROP TABLE instruction_revisions; DROP TABLE instruction_layers; ALTER TABLE sessions DROP COLUMN role;").unwrap();
        conn.pragma_update(None, "user_version", 19).unwrap();
        drop(conn);
        let migrated = Storage::open(&db).unwrap();
        assert_eq!(
            migrated.get_session(worker.id).unwrap().role,
            SessionRole::Worker
        );
        let sup = migrated.get_session(supervisor.id).unwrap();
        assert_eq!(sup.role, SessionRole::Supervisor);
        assert!(sup.items_api && sup.supervisor_api);
    }

    #[test]
    fn instruction_layers_are_optimistic_versioned_and_revertible() {
        let s = store();
        let b = s.create_bucket("b").unwrap();
        let p = s.create_project(b.id, "p", "/p").unwrap();
        let first = s
            .set_instruction_layer(
                b.id,
                None,
                InstructionTarget::Worker,
                "one",
                0,
                "create",
                None,
                1,
            )
            .unwrap();
        assert_eq!(first.revision, 1);
        assert!(matches!(
            s.set_instruction_layer(
                b.id,
                None,
                InstructionTarget::Worker,
                "lost",
                0,
                "",
                None,
                2
            ),
            Err(StorageError::Conflict(_))
        ));
        let second = s
            .set_instruction_layer(
                b.id,
                None,
                InstructionTarget::Worker,
                "two",
                1,
                "update",
                None,
                3,
            )
            .unwrap();
        assert_eq!(second.revision, 2);
        let project = s
            .set_instruction_layer(
                b.id,
                Some(p.id),
                InstructionTarget::All,
                "project",
                0,
                "",
                None,
                4,
            )
            .unwrap();
        let layers = s.instruction_layers(b.id, Some(p.id)).unwrap();
        assert_eq!(
            layers.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![first.id, project.id]
        );
        let bucket_layers = s.instruction_layers(b.id, None).unwrap();
        assert_eq!(
            bucket_layers.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![first.id]
        );
        let reverted = s
            .revert_instruction_layer(first.id, 1, 2, "revert", None, 5)
            .unwrap();
        assert_eq!(reverted.markdown, "one");
        assert_eq!(reverted.revision, 3);
        assert_eq!(s.instruction_history(first.id).unwrap().len(), 3);
    }

    #[test]
    fn supervisor_role_enforces_both_effective_grants_and_worker_demotion() {
        let s = store();
        let b = s.create_bucket("b").unwrap();
        let p = s.create_project(b.id, "p", "/p").unwrap();
        let session = s
            .create_session(
                p.id,
                AgentKind::Codex,
                "s",
                "",
                PermissionMode::Default,
                0,
                false,
                false,
                None,
                1,
            )
            .unwrap();
        let promoted = s
            .set_session_apis(
                session.id,
                Some(false),
                Some(false),
                Some(SessionRole::Supervisor),
            )
            .unwrap();
        assert!(promoted.items_api && promoted.supervisor_api);
        assert_eq!(promoted.role, SessionRole::Supervisor);
        let demoted = s
            .set_session_apis(session.id, Some(false), None, Some(SessionRole::Worker))
            .unwrap();
        assert!(!demoted.items_api && !demoted.supervisor_api);
        assert_eq!(demoted.role, SessionRole::Worker);
    }

    #[test]
    fn auth_sessions_resolve_until_expiry() {
        let s = store();
        let uid = s.create_user("testuser", "h", 1000).unwrap();
        s.create_auth_session("tokhash", uid, 1000, 5000).unwrap();

        assert_eq!(s.lookup_auth_session("tokhash", 2000).unwrap(), Some(uid));
        assert_eq!(s.lookup_auth_session("wrong", 2000).unwrap(), None);
        assert_eq!(s.lookup_auth_session("tokhash", 5000).unwrap(), None);
        assert_eq!(
            s.lookup_auth_session("tokhash", 2000).unwrap(),
            None,
            "expired row must be gone even when time rolls back"
        );
    }

    #[test]
    fn deleted_auth_session_stops_resolving() {
        let s = store();
        let uid = s.create_user("testuser", "h", 1000).unwrap();
        s.create_auth_session("tokhash", uid, 1000, i64::MAX)
            .unwrap();
        s.delete_auth_session("tokhash").unwrap();
        assert_eq!(s.lookup_auth_session("tokhash", 2000).unwrap(), None);
    }

    #[test]
    fn workspaces_are_scoped_to_their_user_and_persist_layouts() {
        let s = store();
        let first = s.create_user("first", "h", 1000).unwrap();
        let second = s.create_user("second", "h", 1000).unwrap();
        let created = s
            .create_workspace(
                first,
                "release watch",
                r#"{"kind":"pane","terminalId":"7"}"#,
                2000,
            )
            .unwrap();

        assert_eq!(s.list_workspaces(first).unwrap(), vec![created.clone()]);
        assert!(s.list_workspaces(second).unwrap().is_empty());
        let updated = s
            .update_workspace(
                first,
                created.id,
                "release",
                r#"{"kind":"pane","terminalId":"8"}"#,
                3000,
            )
            .unwrap();
        assert_eq!(updated.name, "release");
        assert_eq!(updated.updated_at_unix_ms, 3000);
        assert!(matches!(
            s.update_workspace(second, created.id, "stolen", "{}", 4000),
            Err(StorageError::NotFound("workspace", _))
        ));
        s.delete_workspace(first, created.id).unwrap();
        assert!(s.list_workspaces(first).unwrap().is_empty());
    }

    #[test]
    fn workspace_tab_order_persists() {
        let s = store();
        let user = s.create_user("first", "h", 1000).unwrap();
        let first = s.create_workspace(user, "first", "{}", 2000).unwrap();
        let second = s.create_workspace(user, "second", "{}", 3000).unwrap();
        s.reorder_workspaces(user, &[second.id, first.id]).unwrap();
        assert_eq!(
            s.list_workspaces(user)
                .unwrap()
                .into_iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>(),
            vec![second.id, first.id]
        );
        assert!(matches!(
            s.reorder_workspaces(user, &[first.id]),
            Err(StorageError::Conflict(_))
        ));
        s.delete_workspace(user, second.id).unwrap();
        assert_eq!(s.list_workspaces(user).unwrap()[0].position, 0);
    }

    #[test]
    fn migrating_a_v10_database_adds_workspaces() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v8.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL, created_at_unix_ms INTEGER NOT NULL);
             CREATE TABLE sessions (id INTEGER PRIMARY KEY, desired_running INTEGER NOT NULL DEFAULT 0);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 10i64).unwrap();
        drop(conn);

        let migrated = Storage::open(&db).unwrap();
        let version: i64 = migrated
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(migrated.list_workspaces(1).unwrap().is_empty());
    }

    #[test]
    fn migrating_a_v11_database_adds_workspace_tab_order() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v11.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL, created_at_unix_ms INTEGER NOT NULL);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, name TEXT NOT NULL, layout_json TEXT NOT NULL, created_at_unix_ms INTEGER NOT NULL, updated_at_unix_ms INTEGER NOT NULL);
             CREATE TABLE sessions (id INTEGER PRIMARY KEY, desired_running INTEGER NOT NULL DEFAULT 0);
             INSERT INTO users VALUES (1, 'testuser', 'h', 1);
             INSERT INTO workspaces VALUES (7, 1, 'release', '{}', 1, 1);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 11i64).unwrap();
        drop(conn);

        let migrated = Storage::open(&db).unwrap();
        assert_eq!(migrated.list_workspaces(1).unwrap()[0].position, 7);
        let obsolete_columns: i64 = migrated
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name='workspace_home_position'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(obsolete_columns, 0);
    }

    #[test]
    fn migrating_a_v21_database_removes_session_tab_position() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v21.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            "ALTER TABLE users ADD COLUMN workspace_home_position INTEGER NOT NULL DEFAULT 0;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 21i64).unwrap();
        drop(conn);

        let migrated = Storage::open(&db).unwrap();
        let obsolete_columns: i64 = migrated
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name='workspace_home_position'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(obsolete_columns, 0);
    }

    #[test]
    fn migrating_a_v22_database_adds_worker_assignments() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v22.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO buckets (id, name, position, default_worker_id)
                 VALUES (1, 'work', 0, 0);
             INSERT INTO projects (id, bucket_id, name, path)
                 VALUES (2, 1, 'api', '/srv/api');
             DROP INDEX idx_one_default_bucket;
             DROP TABLE project_workers;
             DROP TABLE bucket_workers;
             ALTER TABLE buckets DROP COLUMN is_default;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 22i64).unwrap();
        drop(conn);

        let migrated = Storage::open(&db).unwrap();
        let conn = migrated.conn.lock().unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let added_schema_objects: i64 = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM pragma_table_info('buckets') WHERE name='is_default') +
                    (SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='bucket_workers') +
                    (SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='project_workers')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let migrated_data: (i64, i64, i64) = conn
            .query_row(
                "SELECT
                    (SELECT is_default FROM buckets WHERE id = 1),
                    (SELECT COUNT(*) FROM bucket_workers WHERE bucket_id = 1 AND worker_id = 0),
                    (SELECT COUNT(*) FROM project_workers WHERE project_id = 2 AND worker_id = 0)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(added_schema_objects, 3);
        assert_eq!(migrated_data, (1, 1, 1));
    }

    #[test]
    fn a_slug_is_reserved_only_while_its_session_is_wanted_running() {
        let (s, pid) = store_with_project();
        let session = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let fwd = s
            .create_session_forward(session.id, 5173, 0, "preview", "preview", "http", 1000)
            .unwrap();
        assert_eq!(
            s.desired_session_forward_by_slug("preview").unwrap(),
            Some(fwd.clone())
        );

        // Shutting the session down gives up the claim on the name while
        // the row stays, so a resume still knows what it published.
        s.set_session_desired_running(session.id, false).unwrap();
        assert_eq!(s.desired_session_forward_by_slug("preview").unwrap(), None);
        assert_eq!(
            s.list_session_forwards(session.id).unwrap(),
            vec![fwd.clone()]
        );

        // The unique index only stops colliding once the name is actually
        // given up, which is what lets the next session take it.
        s.clear_session_forward_slug(fwd.id).unwrap();
        assert_eq!(s.session_forward_by_slug("preview").unwrap(), None);
        let other = s
            .create_session(
                pid,
                AgentKind::Test,
                "t2",
                "",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let taken = s
            .create_session_forward(other.id, 5174, 0, "preview", "preview", "http", 2000)
            .unwrap();
        assert_eq!(taken.slug, "preview");
        assert_eq!(
            s.desired_session_forward_by_slug("preview").unwrap(),
            Some(taken)
        );
    }

    #[test]
    fn session_forwards_persist_and_follow_desired_running() {
        let (s, pid) = store_with_project();
        let session = s
            .create_session(
                pid,
                AgentKind::Test,
                "t",
                "",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let fwd = s
            .create_session_forward(session.id, 5173, 41000, "vite", "vite dev", "http", 1000)
            .unwrap();
        assert_eq!(fwd.worker_port, 5173);
        assert_eq!(fwd.listener_port, 41000);
        assert_eq!(fwd.slug, "vite");
        assert_eq!(fwd.label, "vite dev");
        assert_eq!(fwd.scheme, "http");
        assert_eq!(
            s.get_session_forward(session.id, 5173).unwrap(),
            Some(fwd.clone())
        );
        assert_eq!(
            s.list_session_forwards(session.id).unwrap(),
            vec![fwd.clone()]
        );
        assert_eq!(s.desired_session_forwards().unwrap(), vec![fwd.clone()]);
        assert!(matches!(
            s.create_session_forward(session.id, 5173, 41001, "again", "again", "http", 2000),
            Err(StorageError::Conflict(_))
        ));
        // A slug is one forward's hostname, so a second session cannot
        // take it even on a port of its own.
        let other = s
            .create_session(
                pid,
                AgentKind::Test,
                "t2",
                "",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        assert!(matches!(
            s.create_session_forward(other.id, 5174, 0, "vite", "vite", "http", 2000),
            Err(StorageError::Conflict(_))
        ));
        assert_eq!(
            s.session_forward_by_slug("vite").unwrap(),
            Some(fwd.clone())
        );
        assert_eq!(s.session_forward_by_slug("absent").unwrap(), None);

        s.set_session_forward_listener_port(fwd.id, 41500).unwrap();
        assert_eq!(
            s.get_session_forward(session.id, 5173)
                .unwrap()
                .unwrap()
                .listener_port,
            41500
        );

        s.set_session_desired_running(session.id, false).unwrap();
        assert!(s.desired_session_forwards().unwrap().is_empty());
        assert_eq!(s.all_session_forwards().unwrap().len(), 1);

        s.delete_session_forward(session.id, 5173).unwrap();
        assert!(matches!(
            s.delete_session_forward(session.id, 5173),
            Err(StorageError::NotFound("forward", _))
        ));
        assert!(s.list_session_forwards(session.id).unwrap().is_empty());
    }

    #[test]
    fn migrating_a_v12_database_adds_session_forwards() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("v12.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id INTEGER PRIMARY KEY, desired_running INTEGER NOT NULL DEFAULT 0);
             INSERT INTO sessions (id, desired_running) VALUES (7, 1);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 12i64).unwrap();
        drop(conn);

        let migrated = Storage::open(&db).unwrap();
        let version: i64 = migrated
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let created = migrated
            .create_session_forward(7, 5173, 41000, "vite", "vite", "http", 1000)
            .unwrap();
        assert_eq!(migrated.desired_session_forwards().unwrap(), vec![created]);
    }

    /// A forward that predates slugs keeps its row and takes no name,
    /// which is what leaves it reachable at its id under a share domain.
    #[test]
    fn migrating_a_pre_slug_database_keeps_its_forwards_unnamed() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("pre-slug.db");
        let (session_id, forward_id) = {
            let s = Storage::open(&db).unwrap();
            let bucket = s.create_bucket("work").unwrap();
            let project = s.create_project(bucket.id, "api", "/tmp/api").unwrap();
            let session = s
                .create_session(
                    project.id,
                    AgentKind::Test,
                    "t",
                    "",
                    PermissionMode::Default,
                    0,
                    true,
                    false,
                    None,
                    1,
                )
                .unwrap();
            let forward = s
                .create_session_forward(session.id, 5173, 41000, "vite", "vite", "http", 1000)
                .unwrap();
            (session.id, forward.id)
        };

        // Rewind the one thing this change adds, so what reopens is the
        // shape a controller wrote before slugs existed rather than a
        // hand-built approximation of it.
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP INDEX idx_session_forwards_slug;
             ALTER TABLE session_forwards DROP COLUMN slug;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
            .unwrap();
        drop(conn);

        let migrated = Storage::open(&db).unwrap();
        let version: i64 = migrated
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let kept = migrated.session_forward(forward_id).unwrap();
        assert_eq!(kept.slug, "", "an existing forward was given a name");
        assert_eq!(kept.label, "vite");
        assert_eq!(kept.worker_port, 5173);
        assert_eq!(kept.listener_port, 41000);
        assert_eq!(kept.scheme, "http");
        assert_eq!(kept.created_at_unix_ms, 1000);
        assert_eq!(migrated.desired_session_forwards().unwrap(), vec![kept]);
        // Nothing holds the empty slug, and more than one row may leave
        // it empty, because the uniqueness index skips those.
        assert_eq!(migrated.session_forward_by_slug("").unwrap(), None);
        migrated
            .create_session_forward(session_id, 5174, 0, "", "other", "http", 2000)
            .unwrap();
        assert_eq!(migrated.all_session_forwards().unwrap().len(), 2);
        // The index the migration adds is live on the upgraded database.
        migrated
            .create_session_forward(session_id, 5175, 0, "docs-preview", "", "http", 3000)
            .unwrap();
        assert!(matches!(
            migrated.create_session_forward(session_id, 5176, 0, "docs-preview", "", "http", 4000),
            Err(StorageError::Conflict(_))
        ));
    }

    fn blank_field(key: &str, value: &str) -> ContextField {
        ContextField {
            key: key.into(),
            label: key.into(),
            value: value.into(),
            kind: ContextKind::Text,
            severity: ContextSeverity::Neutral,
        }
    }

    fn store_with_bucket() -> (Storage, u64) {
        let s = store();
        let b = s.create_bucket("work").unwrap();
        (s, b.id)
    }

    fn titled(title: &str) -> ItemUpsert {
        ItemUpsert {
            title: Some(title.into()),
            ..ItemUpsert::default()
        }
    }

    fn keyed(key: &str, title: &str) -> ItemUpsert {
        ItemUpsert {
            external_key: Some(key.into()),
            title: Some(title.into()),
            ..ItemUpsert::default()
        }
    }

    fn schema_v24() -> String {
        SCHEMA
            .replacen(
                "    item_number INTEGER NOT NULL CHECK(item_number > 0),\n",
                "",
                1,
            )
            .replacen(
                "    UNIQUE (bucket_id, external_key),\n    UNIQUE (bucket_id, item_number)\n",
                "    UNIQUE (bucket_id, external_key)\n",
                1,
            )
            .replacen(
                "CREATE TABLE bucket_item_sequences (\n    bucket_id INTEGER PRIMARY KEY REFERENCES buckets(id) ON DELETE CASCADE,\n    next_item_number INTEGER NOT NULL CHECK(next_item_number > 0)\n);\n",
                "",
                1,
            )
    }

    #[test]
    fn v24_migration_preserves_surrogate_relationships_and_rewrites_owned_links() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v24.db");
        let conn = Connection::open(&path).unwrap();
        let old_schema = schema_v24();
        assert!(!old_schema.contains("item_number INTEGER"));
        assert!(!old_schema.contains("bucket_item_sequences"));
        conn.execute_batch(&old_schema).unwrap();
        conn.execute_batch(
            "INSERT INTO buckets (id, name, position, is_default) VALUES
                 (1, 'one', 0, 1), (2, 'two', 1, 0);
             INSERT INTO projects (id, bucket_id, name, path) VALUES
                 (10, 1, 'one-project', '/one'), (20, 2, 'two-project', '/two');
             INSERT INTO sessions
                 (id, project_id, agent, state, task_title, task_prompt, created_at_unix_ms)
                 VALUES (100, 10, 'test', 'idle',
                    'work pm:item/41; foreign pm:item/55', 'open pm:item/70', 1);
             INSERT INTO items
                 (id, bucket_id, project_id, external_key, title, body, status, priority,
                  source_kind, created_at_unix_ms, updated_at_unix_ms)
                 VALUES
                 (41, 1, 10, 'same-key', 'first pm:item/41', '', 'planned', 'normal', 'human', 1, 1),
                 (55, 2, 20, 'same-key', 'other pm:item/55', '', 'planned', 'normal', 'human', 1, 1),
                 (70, 1, 10, NULL, 'second',
                  'owned pm:item/41; self pm:item/70; foreign pm:item/55; canonical pm:item/1/1; xpm:item/41; pm:item/41tail',
                  'blocked', 'high', 'agent', 2, 2);
             INSERT INTO item_deps (item_id, depends_on_item_id) VALUES (70, 41);
             INSERT INTO item_sessions (item_id, session_id) VALUES (70, 100);
             INSERT INTO item_notes (id, item_id, ts_unix_ms, kind, text)
                 VALUES (500, 70, 3, 'note', 'see pm:item/41 and pm:item/55');
             INSERT INTO item_attachments
                 (id, item_id, filename, media_type, byte_length, sha256, content, created_at_unix_ms)
                 VALUES (600, 70, 'proof.txt', 'text/plain', 1, zeroblob(32), X'78', 4);
             INSERT INTO bucket_briefings (id, bucket_id, ts_unix_ms, markdown)
                 VALUES (700, 1, 5, '[owned](pm:item/41) [foreign](pm:item/55)');
             INSERT INTO session_context
                 (session_id, key, label, value, kind, severity, updated_at_unix_ms)
                 VALUES (100, 'item', 'Item', 'pm:item/70', 'url', 'neutral', 6);
             INSERT INTO session_glance
                 (session_id, position, key, label, value, kind, severity)
                 VALUES (100, 0, 'item', 'Item', 'pm:item/41', 'url', 'neutral');
             INSERT INTO instruction_layers
                 (id, bucket_id, target, markdown, updated_at_unix_ms)
                 VALUES (800, 1, 'all', 'read pm:item/41', 7);
             INSERT INTO instruction_revisions
                 (layer_id, revision, markdown, note, updated_at_unix_ms)
                 VALUES (800, 1, 'read pm:item/70', 'from pm:item/41', 7);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 24).unwrap();
        drop(conn);

        let migrated = Storage::open(&path).unwrap();
        let first = migrated.get_item(1, 1).unwrap();
        let second = migrated.get_item(1, 2).unwrap();
        let other = migrated.get_item(2, 1).unwrap();
        assert_eq!(first.title, "first pm:item/1/1");
        assert_eq!(other.title, "other pm:item/2/1");
        assert_eq!(second.blocked_by, vec![1]);
        assert_eq!(second.session_ids, vec![100]);
        assert_eq!(
            second.body,
            "owned pm:item/1/1; self pm:item/1/2; foreign pm:item/55; canonical pm:item/1/1; xpm:item/41; pm:item/41tail"
        );
        assert!(matches!(
            migrated.get_item(1, 55),
            Err(StorageError::NotFound("item", 55))
        ));
        assert_eq!(
            migrated.item_notes(1, 2).unwrap()[0].text,
            "see pm:item/1/1 and pm:item/55"
        );
        let attachment = migrated.get_item_attachment(600).unwrap();
        assert_eq!(
            (attachment.metadata.bucket_id, attachment.metadata.item_id),
            (1, 2)
        );
        assert_eq!(attachment.content, b"x");
        assert_eq!(
            migrated.list_briefings(1, 10).unwrap()[0].markdown,
            "[owned](pm:item/1/1) [foreign](pm:item/55)"
        );
        assert_eq!(
            migrated.get_session(100).unwrap().task_title,
            "work pm:item/1/1; foreign pm:item/55"
        );
        assert_eq!(
            migrated.session_context(100).unwrap().detail[0].value,
            "pm:item/1/2"
        );
        assert_eq!(
            migrated.session_context(100).unwrap().glance[0].value,
            "pm:item/1/1"
        );
        assert_eq!(
            migrated.instruction_layers(1, None).unwrap()[0].markdown,
            "read pm:item/1/1"
        );

        let (next_one, _) = migrated
            .upsert_item(1, &titled("next one"), None, 10)
            .unwrap();
        let (next_two, _) = migrated
            .upsert_item(2, &titled("next two"), None, 10)
            .unwrap();
        assert_eq!((next_one.id, next_two.id), (3, 2));
        let internal_edges: (i64, i64, i64, i64) = migrated
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM item_deps WHERE item_id=70 AND depends_on_item_id=41),
                    (SELECT COUNT(*) FROM item_sessions WHERE item_id=70 AND session_id=100),
                    (SELECT item_id FROM item_notes WHERE id=500),
                    (SELECT item_id FROM item_attachments WHERE id=600)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(internal_edges, (1, 1, 70, 70));
    }

    #[test]
    fn bucket_local_numbers_isolate_reads_writes_links_and_external_keys() {
        let s = store();
        let one = s.create_bucket("one").unwrap();
        let two = s.create_bucket("two").unwrap();
        let one_project = s.create_project(one.id, "one", "/one").unwrap();
        let two_project = s.create_project(two.id, "two", "/two").unwrap();
        let two_session = s
            .create_session(
                two_project.id,
                AgentKind::Test,
                "two",
                "",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let (one_item, _) = s
            .upsert_item(one.id, &keyed("shared", "one"), None, 1)
            .unwrap();
        let (two_item, _) = s
            .upsert_item(two.id, &keyed("shared", "two"), None, 1)
            .unwrap();
        assert_eq!((one_item.id, two_item.id), (1, 1));
        assert_eq!(
            s.item_id_by_external_key(one.id, "shared").unwrap(),
            Some(1)
        );
        assert_eq!(
            s.item_id_by_external_key(two.id, "shared").unwrap(),
            Some(1)
        );

        let update = ItemUpsert {
            id: Some(1),
            title: Some("one updated".into()),
            ..Default::default()
        };
        s.upsert_item(one.id, &update, None, 2).unwrap();
        assert_eq!(s.get_item(one.id, 1).unwrap().title, "one updated");
        assert_eq!(s.get_item(two.id, 1).unwrap().title, "two");
        assert!(matches!(
            s.link_item_session(one.id, 1, two_session.id),
            Err(StorageError::Conflict(_))
        ));
        assert!(matches!(
            s.upsert_item(one.id, &titled("foreign actor"), Some(two_session.id), 2),
            Err(StorageError::Conflict(_))
        ));
        assert!(matches!(
            s.create_item_attachment(
                one.id,
                1,
                "foreign.txt",
                "text/plain",
                b"x",
                &[0; 32],
                Some(two_session.id),
                2,
            ),
            Err(StorageError::Conflict(_))
        ));
        assert!(matches!(
            s.create_briefing(one.id, Some(two_session.id), "foreign", 2),
            Err(StorageError::Conflict(_))
        ));

        let one_session = s
            .create_session(
                one_project.id,
                AgentKind::Test,
                "one",
                "",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                2,
            )
            .unwrap();
        s.link_item_session(one.id, 1, one_session.id).unwrap();
        assert_eq!(
            s.item_refs_for_session(one_session.id).unwrap(),
            vec![ItemRef {
                bucket_id: one.id,
                item_id: 1
            }]
        );
        assert!(s.item_refs_for_session(two_session.id).unwrap().is_empty());
    }

    #[test]
    fn concurrent_item_allocation_is_unique_and_monotonic_per_bucket() {
        use std::sync::{Arc, Barrier};

        let storage = Arc::new(store());
        let bucket = storage.create_bucket("work").unwrap();
        let barrier = Arc::new(Barrier::new(12));
        let mut threads = Vec::new();
        for index in 0..12 {
            let storage = Arc::clone(&storage);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                storage
                    .upsert_item(bucket.id, &titled(&format!("item {index}")), None, index)
                    .unwrap()
                    .0
                    .id
            }));
        }
        let mut ids = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, (1..=12).collect::<Vec<_>>());
    }

    #[test]
    fn item_create_takes_defaults_and_records_creation() {
        let (s, b) = store_with_bucket();
        let (item, outcome) = s
            .upsert_item(b, &titled("reply to alice"), None, 100)
            .unwrap();
        assert_eq!(outcome, ItemOutcome::Created);
        assert_eq!(item.status, ItemStatus::Inbox);
        assert_eq!(item.priority, ItemPriority::Normal);
        assert_eq!(item.source_kind, ItemSourceKind::Human);
        assert_eq!(item.created_at_unix_ms, 100);
        let notes = s.item_notes(b, item.id).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].kind, "created");

        // An agent-created item defaults its source to agent.
        let (s2, b2) = store_with_bucket();
        let p = s2.create_project(b2, "api", "/tmp/api").unwrap();
        let sess = s2
            .create_session(
                p.id,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let (item, _) = s2
            .upsert_item(b2, &titled("found flaky test"), Some(sess.id), 100)
            .unwrap();
        assert_eq!(item.source_kind, ItemSourceKind::Agent);
        assert_eq!(item.created_by_session_id, Some(sess.id));
    }

    #[test]
    fn item_question_set_clear_and_history_follow_upsert_semantics() {
        let (s, b) = store_with_bucket();
        let mut create = titled("choose a database");
        create.question = Some("Postgres or SQLite?".into());
        let (item, outcome) = s.upsert_item(b, &create, None, 100).unwrap();
        assert_eq!(outcome, ItemOutcome::Created);
        assert_eq!(item.question, "Postgres or SQLite?");

        let absent = ItemUpsert {
            id: Some(item.id),
            body: Some("The deployment is single-node.".into()),
            ..ItemUpsert::default()
        };
        let (item, _) = s.upsert_item(b, &absent, None, 200).unwrap();
        assert_eq!(item.question, "Postgres or SQLite?");

        let clear = ItemUpsert {
            id: Some(item.id),
            question: Some(String::new()),
            ..ItemUpsert::default()
        };
        let (item, outcome) = s.upsert_item(b, &clear, None, 300).unwrap();
        assert_eq!(outcome, ItemOutcome::Updated);
        assert!(item.question.is_empty());

        let ask_again = ItemUpsert {
            id: Some(item.id),
            question: Some("Postgres 17?".into()),
            ..ItemUpsert::default()
        };
        let (item, _) = s.upsert_item(b, &ask_again, None, 400).unwrap();
        assert_eq!(item.question, "Postgres 17?");
        let question_notes: Vec<String> = s
            .item_notes(b, item.id)
            .unwrap()
            .into_iter()
            .filter(|note| note.kind == "note")
            .map(|note| note.text)
            .collect();
        assert_eq!(question_notes, vec!["Postgres or SQLite?", "Postgres 17?"]);
    }

    #[test]
    fn item_create_requires_a_title() {
        let (s, b) = store_with_bucket();
        assert!(matches!(
            s.upsert_item(b, &ItemUpsert::default(), None, 100),
            Err(StorageError::Conflict(_))
        ));
    }

    #[test]
    fn item_resweep_by_external_key_updates_instead_of_duplicating() {
        let (s, b) = store_with_bucket();
        let (first, _) = s
            .upsert_item(b, &keyed("github:pr:1", "Review PR #1"), None, 100)
            .unwrap();

        // Identical sweep: unchanged, no updated_at bump.
        let (again, outcome) = s
            .upsert_item(b, &keyed("github:pr:1", "Review PR #1"), None, 200)
            .unwrap();
        assert_eq!(outcome, ItemOutcome::Unchanged);
        assert_eq!(again.id, first.id);
        assert_eq!(again.updated_at_unix_ms, 100);

        // A real change updates the same row.
        let mut up = keyed("github:pr:1", "Review PR #1");
        up.priority = Some(ItemPriority::Urgent);
        let (updated, outcome) = s.upsert_item(b, &up, None, 300).unwrap();
        assert_eq!(outcome, ItemOutcome::Updated);
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.priority, ItemPriority::Urgent);
        assert_eq!(updated.updated_at_unix_ms, 300);
        assert_eq!(
            s.list_items(
                &ItemQuery {
                    bucket_id: b,
                    ..Default::default()
                },
                300
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn duplicate_external_key_is_a_conflict() {
        let (s, b) = store_with_bucket();
        s.upsert_item(b, &keyed("k", "one"), None, 100).unwrap();
        let (other, _) = s.upsert_item(b, &titled("two"), None, 100).unwrap();
        let up = ItemUpsert {
            id: Some(other.id),
            external_key: Some("k".into()),
            ..ItemUpsert::default()
        };
        assert!(matches!(
            s.upsert_item(b, &up, None, 200),
            Err(StorageError::Conflict(_))
        ));
    }

    #[test]
    fn agent_cannot_reopen_closed_items_without_a_note() {
        let (s, b) = store_with_bucket();
        let p = s.create_project(b, "api", "/tmp/api").unwrap();
        let sess = s
            .create_session(
                p.id,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let mut up = keyed("jira:1", "ship it");
        up.status = Some(ItemStatus::Done);
        let (item, _) = s.upsert_item(b, &up, Some(sess.id), 100).unwrap();
        assert_eq!(item.done_at_unix_ms, Some(100));

        // Agent re-sweep trying to reopen silently: refused.
        let mut reopen = keyed("jira:1", "ship it");
        reopen.status = Some(ItemStatus::Inbox);
        assert!(matches!(
            s.upsert_item(b, &reopen, Some(sess.id), 200),
            Err(StorageError::Conflict(_))
        ));

        // Same change with a note explaining why: accepted, and the
        // done timestamp clears.
        reopen.note = Some("ticket reopened upstream".into());
        let (reopened, outcome) = s.upsert_item(b, &reopen, Some(sess.id), 300).unwrap();
        assert_eq!(outcome, ItemOutcome::Updated);
        assert_eq!(reopened.status, ItemStatus::Inbox);
        assert_eq!(reopened.done_at_unix_ms, None);

        // A human needs no note.
        let mut done = keyed("jira:1", "ship it");
        done.status = Some(ItemStatus::Dropped);
        s.upsert_item(b, &done, None, 400).unwrap();
        let mut human_reopen = keyed("jira:1", "ship it");
        human_reopen.status = Some(ItemStatus::Planned);
        let (item, _) = s.upsert_item(b, &human_reopen, None, 500).unwrap();
        assert_eq!(item.status, ItemStatus::Planned);
    }

    #[test]
    fn agent_upsert_without_status_never_changes_status() {
        let (s, b) = store_with_bucket();
        let p = s.create_project(b, "api", "/tmp/api").unwrap();
        let sess = s
            .create_session(
                p.id,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let mut up = keyed("e:1", "reply");
        up.status = Some(ItemStatus::Dropped);
        s.upsert_item(b, &up, None, 100).unwrap();

        // Re-sweep sees the email again and refiles it with fresh body
        // but no status: dropped stays dropped.
        let mut sweep = keyed("e:1", "reply");
        sweep.body = Some("they pinged again".into());
        let (item, _) = s.upsert_item(b, &sweep, Some(sess.id), 200).unwrap();
        assert_eq!(item.status, ItemStatus::Dropped);
    }

    #[test]
    fn snooze_is_set_and_cleared_and_filters_listings() {
        let (s, b) = store_with_bucket();
        let (item, _) = s.upsert_item(b, &titled("later"), None, 100).unwrap();
        let snoozed = s.snooze_item(b, item.id, Some(5_000), 200).unwrap();
        assert_eq!(snoozed.snoozed_until_unix_ms, Some(5_000));

        let open = ItemQuery {
            bucket_id: b,
            ..Default::default()
        };
        assert!(
            s.list_items(&open, 1_000).unwrap().is_empty(),
            "snoozed hidden"
        );
        assert_eq!(
            s.list_items(&open, 6_000).unwrap().len(),
            1,
            "snooze expired"
        );
        let include = ItemQuery {
            bucket_id: b,
            include_snoozed: true,
            ..Default::default()
        };
        assert_eq!(s.list_items(&include, 1_000).unwrap().len(), 1);

        let cleared = s.snooze_item(b, item.id, None, 300).unwrap();
        assert_eq!(cleared.snoozed_until_unix_ms, None);
    }

    #[test]
    fn deps_validate_and_replace() {
        let (s, b) = store_with_bucket();
        let (blocker, _) = s.upsert_item(b, &titled("land PR"), None, 100).unwrap();
        let mut up = titled("deploy");
        up.blocked_by = Some(vec![blocker.id]);
        let (item, _) = s.upsert_item(b, &up, None, 100).unwrap();
        assert_eq!(item.blocked_by, vec![blocker.id]);

        // Unknown reference is refused.
        let mut bad = ItemUpsert {
            id: Some(item.id),
            blocked_by: Some(vec![999]),
            ..ItemUpsert::default()
        };
        assert!(matches!(
            s.upsert_item(b, &bad, None, 200),
            Err(StorageError::Conflict(_))
        ));

        // Self-reference is refused.
        bad.blocked_by = Some(vec![item.id]);
        assert!(matches!(
            s.upsert_item(b, &bad, None, 200),
            Err(StorageError::Conflict(_))
        ));

        // The same public number in another bucket still resolves only in the
        // write's bucket; there is no way to address the foreign namespace.
        let other = s.create_bucket("other").unwrap();
        let (foreign, _) = s
            .upsert_item(other.id, &titled("elsewhere"), None, 100)
            .unwrap();
        bad.blocked_by = Some(vec![foreign.id]);
        assert_eq!(foreign.id, blocker.id);
        let (updated, _) = s.upsert_item(b, &bad, None, 200).unwrap();
        assert_eq!(updated.blocked_by, vec![blocker.id]);
        assert_eq!(s.get_item(other.id, foreign.id).unwrap().title, "elsewhere");

        // Some(empty) clears the edges.
        let clear = ItemUpsert {
            id: Some(item.id),
            blocked_by: Some(Vec::new()),
            ..ItemUpsert::default()
        };
        let (item, outcome) = s.upsert_item(b, &clear, None, 300).unwrap();
        assert_eq!(outcome, ItemOutcome::Updated);
        assert!(item.blocked_by.is_empty());
    }

    #[test]
    fn status_flips_and_notes_build_the_timeline() {
        let (s, b) = store_with_bucket();
        let (item, _) = s.upsert_item(b, &titled("fix bug"), None, 100).unwrap();
        let mut up = ItemUpsert {
            id: Some(item.id),
            status: Some(ItemStatus::InProgress),
            ..ItemUpsert::default()
        };
        s.upsert_item(b, &up, None, 200).unwrap();
        up.status = Some(ItemStatus::Done);
        up.note = Some("landed in 3766580".into());
        s.upsert_item(b, &up, None, 300).unwrap();

        let kinds: Vec<(String, String)> = s
            .item_notes(b, item.id)
            .unwrap()
            .into_iter()
            .map(|n| (n.kind, n.text))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("created".into(), "fix bug".into()),
                ("status".into(), "inbox -> in_progress".into()),
                ("status".into(), "in_progress -> done".into()),
                ("note".into(), "landed in 3766580".into()),
            ]
        );
    }

    #[test]
    fn list_items_filters_by_status_and_project() {
        let (s, b) = store_with_bucket();
        let p = s.create_project(b, "api", "/tmp/api").unwrap();
        let mut planned = titled("queued");
        planned.status = Some(ItemStatus::Planned);
        planned.project_id = Some(p.id);
        s.upsert_item(b, &planned, None, 100).unwrap();
        let mut done = titled("finished");
        done.status = Some(ItemStatus::Done);
        s.upsert_item(b, &done, None, 100).unwrap();

        let default = ItemQuery {
            bucket_id: b,
            ..Default::default()
        };
        assert_eq!(
            s.list_items(&default, 100).unwrap().len(),
            1,
            "closed hidden"
        );

        let closed_too = ItemQuery {
            bucket_id: b,
            include_closed: true,
            ..Default::default()
        };
        let all_items = s.list_items(&closed_too, 100).unwrap();
        assert_eq!(all_items.len(), 2);
        assert_eq!(
            all_items
                .iter()
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            vec!["queued", "finished"],
            "ordinary bounded pages rank open work before collapsed history"
        );
        let first_page = s
            .list_items(
                &ItemQuery {
                    limit: Some(1),
                    ..closed_too.clone()
                },
                100,
            )
            .unwrap();
        let second_page = s
            .list_items(
                &ItemQuery {
                    limit: Some(1),
                    offset: 1,
                    ..closed_too.clone()
                },
                100,
            )
            .unwrap();
        assert_eq!(first_page[0].title, "queued");
        assert_eq!(second_page[0].title, "finished");
        assert_eq!(
            s.item_query_counts(&default, 100).unwrap(),
            ItemQueryCounts {
                bucket_total: 2,
                matching_total: 1,
                status_counts: vec![(ItemStatus::Planned, 1)],
            }
        );
        let all_counts = s.item_query_counts(&closed_too, 100).unwrap();
        assert_eq!(all_counts.bucket_total, 2);
        assert_eq!(all_counts.matching_total, 2);
        assert!(all_counts.status_counts.contains(&(ItemStatus::Planned, 1)));
        assert!(all_counts.status_counts.contains(&(ItemStatus::Done, 1)));

        let by_status = ItemQuery {
            bucket_id: b,
            statuses: vec![ItemStatus::Done],
            ..Default::default()
        };
        let done_items = s.list_items(&by_status, 100).unwrap();
        assert_eq!(done_items.len(), 1);
        assert_eq!(done_items[0].title, "finished");

        let by_project = ItemQuery {
            bucket_id: b,
            project_id: Some(p.id),
            ..Default::default()
        };
        assert_eq!(s.list_items(&by_project, 100).unwrap().len(), 1);

        // A project in another bucket cannot be referenced.
        let other = s.create_bucket("other").unwrap();
        let mut misfiled = titled("wrong bucket");
        misfiled.project_id = Some(p.id);
        assert!(matches!(
            s.upsert_item(other.id, &misfiled, None, 100),
            Err(StorageError::Conflict(_))
        ));
    }

    #[test]
    fn list_items_summary_filters_are_server_ranked_and_composable() {
        let (s, b) = store_with_bucket();
        let p = s.create_project(b, "api", "/tmp/api").unwrap();
        let live = s
            .create_session(
                p.id,
                AgentKind::Test,
                "live",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let exited = s
            .create_session(
                p.id,
                AgentKind::Test,
                "exited",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        s.set_session_ended(exited.id, SessionState::Exited, "", Some(0), 2)
            .unwrap();

        let mut needs_you = titled("question");
        needs_you.status = Some(ItemStatus::Planned);
        needs_you.question = Some("choose".into());
        needs_you.priority = Some(ItemPriority::High);
        let (needs_you, _) = s.upsert_item(b, &needs_you, None, 100).unwrap();
        let mut linked = titled("linked");
        linked.status = Some(ItemStatus::InProgress);
        linked.priority = Some(ItemPriority::High);
        linked.link_session_id = Some(live.id);
        let (linked, _) = s.upsert_item(b, &linked, None, 100).unwrap();
        let mut dead_link = titled("dead link");
        dead_link.link_session_id = Some(exited.id);
        s.upsert_item(b, &dead_link, None, 100).unwrap();
        let mut done = titled("recent done");
        done.status = Some(ItemStatus::Done);
        let (done, _) = s.upsert_item(b, &done, None, 100).unwrap();

        let ids = |summary_filter| {
            s.list_items(
                &ItemQuery {
                    bucket_id: b,
                    summary_filter: Some(summary_filter),
                    priorities: vec![ItemPriority::High],
                    ..Default::default()
                },
                100,
            )
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>()
        };
        assert_eq!(ids(ItemSummaryFilter::NeedsYou), vec![needs_you.id]);
        assert_eq!(ids(ItemSummaryFilter::InProgress), vec![linked.id]);
        assert_eq!(ids(ItemSummaryFilter::LiveLinked), vec![linked.id]);
        assert!(ids(ItemSummaryFilter::DoneRecently).is_empty());

        let recent_done = s
            .list_items(
                &ItemQuery {
                    bucket_id: b,
                    summary_filter: Some(ItemSummaryFilter::DoneRecently),
                    ..Default::default()
                },
                100,
            )
            .unwrap();
        assert_eq!(recent_done[0].id, done.id);
    }

    #[test]
    fn item_search_matches_every_field_case_insensitively_and_stays_in_bucket() {
        let (s, b) = store_with_bucket();
        let other = s.create_bucket("other").unwrap();
        let cases = [
            ("Needle title", "", "", None, "", ""),
            ("body", "NEEDLE in body", "", None, "", ""),
            ("question", "", "needle decision", None, "", ""),
            ("key", "", "", Some("jira:NEEDLE-4"), "", ""),
            ("detail", "", "", None, "mailbox/Needle", ""),
            ("url", "", "", None, "", "https://example.test/NEEDLE"),
        ];
        for (title, body, question, key, detail, url) in cases {
            let mut write = titled(title);
            write.body = Some(body.into());
            write.question = Some(question.into());
            write.external_key = key.map(str::to_owned);
            write.source_detail = Some(detail.into());
            write.url = Some(url.into());
            s.upsert_item(b, &write, None, 100).unwrap();
        }
        let mut foreign = titled("needle foreign");
        foreign.body = Some("needle".into());
        s.upsert_item(other.id, &foreign, None, 100).unwrap();

        let found = s
            .list_items(
                &ItemQuery {
                    bucket_id: b,
                    search: Some("nEeDlE".into()),
                    include_closed: true,
                    include_snoozed: true,
                    ..Default::default()
                },
                100,
            )
            .unwrap();
        assert_eq!(found.len(), 6);
        assert!(found.iter().all(|item| item.bucket_id == b));
        assert_eq!(
            found[0].title, "Needle title",
            "exact/prefix title ranks first"
        );

        let literal = s
            .list_items(
                &ItemQuery {
                    bucket_id: b,
                    search: Some("%_'".into()),
                    include_closed: true,
                    include_snoozed: true,
                    ..Default::default()
                },
                100,
            )
            .unwrap();
        assert!(
            literal.is_empty(),
            "SQL wildcard and quote characters are literal"
        );
    }

    #[test]
    fn item_search_composes_filters_and_pages_in_stable_order() {
        let (s, b) = store_with_bucket();
        let project = s.create_project(b, "api", "/tmp/api").unwrap();
        for index in 0..8 {
            let mut write = titled(&format!("search result {index}"));
            write.project_id = Some(project.id);
            write.priority = Some(if index < 6 {
                ItemPriority::High
            } else {
                ItemPriority::Low
            });
            write.source_kind = Some(if index < 6 {
                ItemSourceKind::Github
            } else {
                ItemSourceKind::Human
            });
            write.status = Some(if index == 5 {
                ItemStatus::Done
            } else {
                ItemStatus::Planned
            });
            s.upsert_item(b, &write, None, 100 + index).unwrap();
        }
        let base = ItemQuery {
            bucket_id: b,
            search: Some("search".into()),
            project_id: Some(project.id),
            priorities: vec![ItemPriority::High],
            source_kinds: vec![ItemSourceKind::Github],
            statuses: vec![ItemStatus::Planned],
            include_closed: true,
            include_snoozed: true,
            limit: Some(2),
            ..Default::default()
        };
        let first = s.list_items(&base, 1_000).unwrap();
        let second = s
            .list_items(
                &ItemQuery {
                    offset: 2,
                    ..base.clone()
                },
                1_000,
            )
            .unwrap();
        let third = s
            .list_items(&ItemQuery { offset: 4, ..base }, 1_000)
            .unwrap();
        let ids = first
            .into_iter()
            .chain(second)
            .chain(third)
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 5);
        assert!(
            ids.windows(2).all(|pair| pair[0] > pair[1]),
            "updated/id order is stable"
        );

        let done = s
            .list_items(
                &ItemQuery {
                    bucket_id: b,
                    search: Some("search".into()),
                    statuses: vec![ItemStatus::Done],
                    priorities: vec![ItemPriority::High],
                    source_kinds: vec![ItemSourceKind::Github],
                    ..Default::default()
                },
                1_000,
            )
            .unwrap();
        assert_eq!(done.len(), 1, "explicit done filter composes with search");

        let (snoozed, _) = s
            .upsert_item(b, &titled("search snoozed"), None, 200)
            .unwrap();
        s.snooze_item(b, snoozed.id, Some(5_000), 201).unwrap();
        let hidden = s
            .list_items(
                &ItemQuery {
                    bucket_id: b,
                    search: Some("snoozed".into()),
                    ..Default::default()
                },
                1_000,
            )
            .unwrap();
        let included = s
            .list_items(
                &ItemQuery {
                    bucket_id: b,
                    search: Some("snoozed".into()),
                    include_snoozed: true,
                    ..Default::default()
                },
                1_000,
            )
            .unwrap();
        assert!(hidden.is_empty());
        assert_eq!(included.len(), 1, "snoozed filter composes with search");
    }

    #[test]
    fn human_item_edit_can_clear_project_and_due_date() {
        let (s, b) = store_with_bucket();
        let project = s.create_project(b, "api", "/tmp/api").unwrap();
        let mut write = titled("editable");
        write.project_id = Some(project.id);
        write.due_at_unix_ms = Some(1_700_000_000_000);
        let (item, _) = s.upsert_item(b, &write, None, 100).unwrap();
        let (cleared, _) = s
            .upsert_item(
                b,
                &ItemUpsert {
                    id: Some(item.id),
                    clear_project: true,
                    clear_due: true,
                    ..Default::default()
                },
                None,
                200,
            )
            .unwrap();
        assert_eq!(cleared.project_id, None);
        assert_eq!(cleared.due_at_unix_ms, None);
    }

    #[test]
    fn snapshot_carries_open_and_recent_items_and_latest_briefings() {
        let (s, b) = store_with_bucket();
        s.upsert_item(b, &titled("open"), None, 100).unwrap();
        let mut old_done = titled("ancient");
        old_done.status = Some(ItemStatus::Done);
        s.upsert_item(b, &old_done, None, 100).unwrap();

        let now = 100 + ITEM_SNAPSHOT_CLOSED_WINDOW_MS + 1;
        let titles: Vec<String> = s
            .snapshot(now)
            .unwrap()
            .items
            .into_iter()
            .map(|i| i.title)
            .collect();
        assert_eq!(titles, vec!["open"], "stale closed items age out");

        s.create_briefing(b, None, "first", 100).unwrap();
        s.create_briefing(b, None, "second", 200).unwrap();
        let briefings = s.snapshot(now).unwrap().briefings;
        assert_eq!(briefings.len(), 1);
        assert_eq!(briefings[0].markdown, "second");
        assert_eq!(
            s.list_briefings(b, 10)
                .unwrap()
                .iter()
                .map(|x| x.markdown.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
    }

    #[test]
    fn deleting_an_item_cascades_and_bucket_delete_requires_empty_projects_only() {
        let (s, b) = store_with_bucket();
        let (blocker, _) = s.upsert_item(b, &titled("a"), None, 100).unwrap();
        let mut up = titled("b");
        up.blocked_by = Some(vec![blocker.id]);
        let (item, _) = s.upsert_item(b, &up, None, 100).unwrap();

        s.delete_item(b, blocker.id).unwrap();
        assert!(matches!(
            s.get_item(b, blocker.id),
            Err(StorageError::NotFound("item", _))
        ));
        assert!(s.get_item(b, item.id).unwrap().blocked_by.is_empty());
    }

    #[test]
    fn item_attachments_are_bounded_indexed_independent_and_cascade() {
        let (s, bucket_id) = store_with_bucket();
        let (item, _) = s
            .upsert_item(bucket_id, &titled("attachments"), None, 100)
            .unwrap();
        let digest = [7_u8; 32];
        let first = s
            .create_item_attachment(
                bucket_id,
                item.id,
                "résumé.txt",
                "text/plain",
                b"same bytes",
                &digest,
                None,
                200,
            )
            .unwrap();
        let second = s
            .create_item_attachment(
                bucket_id,
                item.id,
                "résumé.txt",
                "text/plain",
                b"same bytes",
                &digest,
                None,
                201,
            )
            .unwrap();
        assert_ne!(first.id, second.id, "duplicates are independent references");
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(
            s.list_item_attachments(bucket_id, item.id).unwrap().len(),
            2
        );
        assert_eq!(
            s.get_item_attachment(first.id).unwrap().content,
            b"same bytes"
        );
        assert!(s
            .item_notes(bucket_id, item.id)
            .unwrap()
            .iter()
            .any(|note| note.text.contains("résumé.txt")));

        s.delete_item_attachment(bucket_id, item.id, first.id, None, 300)
            .unwrap();
        assert!(matches!(
            s.get_item_attachment(first.id),
            Err(StorageError::NotFound("attachment", _))
        ));
        assert_eq!(
            s.list_item_attachments(bucket_id, item.id).unwrap().len(),
            1
        );

        assert!(matches!(
            s.create_item_attachment(
                bucket_id,
                item.id,
                "large.bin",
                "application/octet-stream",
                &vec![0; ITEM_ATTACHMENT_FILE_MAX + 1],
                &[0; 32],
                None,
                400,
            ),
            Err(StorageError::Validation {
                field: "attachment",
                ..
            })
        ));
        assert_eq!(
            s.list_item_attachments(bucket_id, item.id).unwrap().len(),
            1
        );

        s.delete_item(bucket_id, item.id).unwrap();
        assert!(matches!(
            s.get_item_attachment(second.id),
            Err(StorageError::NotFound("attachment", _))
        ));
    }

    #[test]
    fn item_attachment_total_limit_accepts_the_boundary_and_rejects_one_more_byte() {
        assert_eq!(
            checked_attachment_total(ITEM_ATTACHMENT_TOTAL_MAX - 1, 1).unwrap(),
            ITEM_ATTACHMENT_TOTAL_MAX
        );
        assert!(matches!(
            checked_attachment_total(ITEM_ATTACHMENT_TOTAL_MAX, 1),
            Err(StorageError::Validation {
                field: "item attachments",
                limit: ITEM_ATTACHMENT_TOTAL_MAX,
                ..
            })
        ));
    }

    #[test]
    fn empty_attachment_persists_across_restart_and_v23_migrates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("items.db");
        let attachment_id;
        {
            let storage = Storage::open(&path).unwrap();
            let bucket_id = storage.create_bucket("work").unwrap().id;
            let (item, _) = storage
                .upsert_item(bucket_id, &titled("empty"), None, 100)
                .unwrap();
            attachment_id = storage
                .create_item_attachment(
                    bucket_id,
                    item.id,
                    "empty.bin",
                    "application/octet-stream",
                    b"",
                    &[0; 32],
                    None,
                    200,
                )
                .unwrap()
                .id;
            storage
                .conn
                .lock()
                .unwrap()
                .pragma_update(None, "user_version", 23)
                .unwrap();
        }
        let reopened = Storage::open(&path).unwrap();
        let attachment = reopened.get_item_attachment(attachment_id).unwrap();
        assert!(attachment.content.is_empty());
        assert_eq!(attachment.metadata.byte_length, 0);
        let version: i64 = reopened
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn fresh_persistent_database_seeds_one_local_default_bucket_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("fresh.db");

        let storage = Storage::open(&db).unwrap();
        let buckets = storage.snapshot(0).unwrap().buckets;
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].name, "Default");
        assert!(buckets[0].is_default);
        assert_eq!(buckets[0].default_worker_id, LOCAL_WORKER_ID);
        assert_eq!(buckets[0].default_agent, Some(AgentKind::ClaudeCode));
        assert_eq!(buckets[0].allowed_worker_ids, vec![LOCAL_WORKER_ID]);
        drop(storage);

        let reopened = Storage::open(&db).unwrap();
        let buckets = reopened.snapshot(0).unwrap().buckets;
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].name, "Default");
    }

    #[test]
    fn migration_never_seeds_or_resurrects_an_empty_existing_database() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("existing-empty.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
                .unwrap();
        }

        let migrated = Storage::open(&db).unwrap();
        assert!(migrated.snapshot(0).unwrap().buckets.is_empty());
        drop(migrated);

        let reopened = Storage::open(&db).unwrap();
        assert!(reopened.snapshot(0).unwrap().buckets.is_empty());
    }

    #[test]
    fn migration_preserves_existing_buckets_without_adding_default() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("existing-data.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO buckets (name, position, default_worker_id, is_default) \
                 VALUES ('Existing', 0, 0, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO bucket_workers (bucket_id, worker_id) VALUES (1, 0)",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
                .unwrap();
        }

        let migrated = Storage::open(&db).unwrap();
        let buckets = migrated.snapshot(0).unwrap().buckets;
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].name, "Existing");
    }

    /// A fresh install can start a session before anything is
    /// configured, which needs a project as well as a bucket.
    #[test]
    fn a_fresh_database_seeds_a_bucket_and_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("pm.db")).unwrap();
        let snapshot = storage.snapshot(0).unwrap();
        assert_eq!(snapshot.buckets.len(), 1, "one bucket");
        assert_eq!(snapshot.projects.len(), 1, "one project");
        assert_eq!(snapshot.projects[0].bucket_id, snapshot.buckets[0].id);
        assert_eq!(
            snapshot.projects[0].path, "",
            "no path, so it runs in the home directory of whatever worker it spawns on"
        );
    }

    #[test]
    fn reopening_sweeps_blank_context_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("pm.db");
        let sess = {
            let s = Storage::open(&db).unwrap();
            let b = s.create_bucket("b").unwrap();
            let p = s.create_project(b.id, "api", "/tmp/api").unwrap();
            let sess = s
                .create_session(
                    p.id,
                    AgentKind::Test,
                    "t",
                    "p",
                    PermissionMode::Default,
                    0,
                    true,
                    false,
                    None,
                    1,
                )
                .unwrap();
            // The storage layer stores what it is given; blank fields written
            // by a daemon predating ingestion-side dropping must not survive
            // the next open.
            s.replace_glance(
                sess.id,
                &[
                    blank_field("", ""),
                    blank_field("tests", "passing"),
                    blank_field("status", "  "),
                ],
            )
            .unwrap();
            s.upsert_context(sess.id, &[blank_field("", "x")], 1)
                .unwrap();
            sess.id
        };

        let s = Storage::open(&db).unwrap();
        let ctx = s.session_context(sess).unwrap();
        assert_eq!(
            ctx.glance
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            ["tests"],
            "blank glance rows are swept on open"
        );
        assert!(
            ctx.detail.is_empty(),
            "blank context rows are swept on open"
        );
    }

    #[test]
    fn item_session_links_survive_upserts() {
        let (s, b) = store_with_bucket();
        let p = s.create_project(b, "api", "/tmp/api").unwrap();
        let sess = s
            .create_session(
                p.id,
                AgentKind::Test,
                "t",
                "p",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let mut up = titled("work this");
        up.link_session_id = Some(sess.id);
        let (item, _) = s.upsert_item(b, &up, None, 100).unwrap();
        assert_eq!(item.session_ids, vec![sess.id]);

        // Linking again is idempotent; other upserts keep the link.
        let again = ItemUpsert {
            id: Some(item.id),
            link_session_id: Some(sess.id),
            ..ItemUpsert::default()
        };
        let (item, outcome) = s.upsert_item(b, &again, None, 200).unwrap();
        assert_eq!(outcome, ItemOutcome::Unchanged);
        assert_eq!(item.session_ids, vec![sess.id]);
    }

    #[test]
    fn item_body_boundary_is_unicode_safe_and_round_trips_without_loss() {
        let (s, bucket_id) = store_with_bucket();
        let markdown_prefix = "# Release notes\n\n- café\n- 😀\n\n";
        let body = format!(
            "{markdown_prefix}{}",
            "界".repeat(ITEM_BODY_MAX - markdown_prefix.chars().count())
        );
        assert_eq!(body.chars().count(), ITEM_BODY_MAX);

        let mut create = titled("large markdown");
        create.body = Some(body.clone());
        let (created, _) = s.upsert_item(bucket_id, &create, None, 100).unwrap();
        assert_eq!(created.body, body);
        assert_eq!(s.get_item(bucket_id, created.id).unwrap().body, body);

        let oversized = format!("{}😀", "x".repeat(ITEM_BODY_MAX));
        let update = ItemUpsert {
            id: Some(created.id),
            body: Some(oversized),
            ..ItemUpsert::default()
        };
        assert!(matches!(
            s.upsert_item(bucket_id, &update, None, 200),
            Err(StorageError::Validation {
                field: "body",
                limit: ITEM_BODY_MAX,
                actual,
                unit: "Unicode scalar values",
            }) if actual == ITEM_BODY_MAX + 1
        ));
        assert_eq!(s.get_item(bucket_id, created.id).unwrap().body, body);

        let persistence_error = s.conn.lock().unwrap().execute(
            "UPDATE items SET body = ?2 WHERE id = ?1",
            params![created.id as i64, "z".repeat(ITEM_BODY_MAX + 1)],
        );
        assert!(persistence_error.is_err());
        let persistence_error = s.conn.lock().unwrap().execute(
            "INSERT INTO items (
                bucket_id, title, body, status, priority, source_kind,
                created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, 'oversized', ?2, 'planned', 'normal', 'human', 300, 300)",
            params![bucket_id as i64, "😀".repeat(ITEM_BODY_MAX + 1)],
        );
        assert!(persistence_error.is_err());
        assert_eq!(s.get_item(bucket_id, created.id).unwrap().body, body);
    }

    #[test]
    fn v20_body_limit_migration_preserves_existing_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("items.db");
        let storage = Storage::open(&path).unwrap();
        let bucket_id = storage.create_bucket("work").unwrap().id;
        let mut write = titled("existing");
        write.body = Some("# Existing\n\ncafé 😀".into());
        let (item, _) = storage.upsert_item(bucket_id, &write, None, 100).unwrap();
        {
            let conn = storage.conn.lock().unwrap();
            conn.execute_batch(
                "DROP TRIGGER items_body_length_insert;
                 DROP TRIGGER items_body_length_update;
                 PRAGMA user_version = 20;",
            )
            .unwrap();
        }
        drop(storage);

        let migrated = Storage::open(&path).unwrap();
        assert_eq!(
            migrated.get_item(bucket_id, item.id).unwrap().body,
            "# Existing\n\ncafé 😀"
        );
        let version: i64 = migrated
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
    #[test]
    fn supervisor_snooze_migrates_and_preserves_state_across_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("snooze.db");
        let storage = Storage::open(&db).unwrap();
        storage
            .conn
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE supervisor_snoozes; PRAGMA user_version = 55;")
            .unwrap();
        drop(storage);
        let storage = Storage::open(&db).unwrap();
        assert_eq!(storage.supervision_snoozed_until(1, 1).unwrap(), 0);
        let (memory, project) = store_with_project();
        let session = memory
            .create_session(
                project,
                AgentKind::ClaudeCode,
                "snooze",
                "wait",
                PermissionMode::Default,
                0,
                true,
                true,
                None,
                1000,
            )
            .unwrap();
        memory.snooze_supervision(session.id, 1, 5000).unwrap();
        assert_eq!(
            memory.supervision_snoozed_until(session.id, 1).unwrap(),
            5000
        );
        assert!(!memory
            .finish_supervision_turn(session.id, 2, Some(2))
            .unwrap());
        assert!(memory
            .finish_supervision_turn(session.id, 1, Some(2))
            .unwrap());
        assert!(memory
            .supervision_completion_silent(session.id, 1, 2)
            .unwrap());
        assert!(!memory
            .supervision_completion_silent(session.id, 1, 3)
            .unwrap());
        assert!(!memory
            .finish_supervision_turn(session.id, 1, Some(3))
            .unwrap());
    }
}
