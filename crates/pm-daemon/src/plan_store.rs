use std::collections::BTreeMap;

use pm_protocol::domain::{Plan, PlanDecisionMode, PlanState};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::storage::{Result, Storage, StorageError};

pub const PLAN_MARKDOWN_MAX: usize = 2 * 1024 * 1024;
pub const PLAN_MESSAGE_MAX: usize = 32_000;

pub const PLAN_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS plans (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    owning_session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    creating_session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('active','accepted','archived')),
    markdown_path TEXT NOT NULL DEFAULT '',
    markdown_snapshot TEXT NOT NULL DEFAULT '',
    revision INTEGER NOT NULL DEFAULT 0,
    active_decision_id INTEGER,
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plans_project ON plans(project_id, state, id);
CREATE INDEX IF NOT EXISTS idx_plans_session ON plans(owning_session_id, state, id);
CREATE TABLE IF NOT EXISTS plan_items (
    plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    PRIMARY KEY(plan_id, item_id)
);
CREATE TABLE IF NOT EXISTS plan_decisions (
    id INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    decision_key TEXT NOT NULL,
    title TEXT NOT NULL,
    prompt_markdown TEXT NOT NULL DEFAULT '',
    detail_markdown TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL CHECK(mode IN ('single','multiple','dialogue')),
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','waiting','resolved')),
    allow_custom INTEGER NOT NULL DEFAULT 0,
    require_selection INTEGER NOT NULL DEFAULT 1,
    batch_key TEXT,
    batch_position INTEGER,
    resolution_markdown TEXT NOT NULL DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    updated_at_unix_ms INTEGER NOT NULL,
    UNIQUE(plan_id, decision_key)
);
CREATE INDEX IF NOT EXISTS idx_plan_decisions_plan ON plan_decisions(plan_id, id);
CREATE TABLE IF NOT EXISTS plan_options (
    id INTEGER PRIMARY KEY,
    decision_id INTEGER NOT NULL REFERENCES plan_decisions(id) ON DELETE CASCADE,
    option_key TEXT NOT NULL,
    label TEXT NOT NULL,
    detail_markdown TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL,
    is_recommended INTEGER NOT NULL DEFAULT 0,
    UNIQUE(decision_id, option_key)
);
CREATE TABLE IF NOT EXISTS plan_responses (
    decision_id INTEGER PRIMARY KEY REFERENCES plan_decisions(id) ON DELETE CASCADE,
    selected_option_keys TEXT NOT NULL DEFAULT '[]',
    custom_label TEXT NOT NULL DEFAULT '',
    custom_detail_markdown TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT '{}',
    submitted_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS plan_decision_drafts (
    decision_id INTEGER PRIMARY KEY REFERENCES plan_decisions(id) ON DELETE CASCADE,
    selected_option_keys TEXT NOT NULL DEFAULT '[]',
    custom_label TEXT NOT NULL DEFAULT '',
    custom_detail_markdown TEXT NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT '{}',
    updated_at_unix_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS plan_messages (
    id INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    decision_id INTEGER REFERENCES plan_decisions(id) ON DELETE CASCADE,
    author TEXT NOT NULL CHECK(author IN ('user','session')),
    session_id INTEGER,
    body TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plan_messages_plan ON plan_messages(plan_id, id);
";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOption {
    pub id: u64,
    pub key: String,
    pub label: String,
    pub detail_markdown: String,
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOptionDraft {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub detail_markdown: String,
    #[serde(default)]
    pub recommended: bool,
}

impl PlanOptionDraft {
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        detail_markdown: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            detail_markdown: detail_markdown.into(),
            recommended: false,
        }
    }

    pub fn recommended(
        key: impl Into<String>,
        label: impl Into<String>,
        detail_markdown: impl Into<String>,
        recommended: bool,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            detail_markdown: detail_markdown.into(),
            recommended,
        }
    }
}

impl From<(String, String, String)> for PlanOptionDraft {
    fn from((key, label, detail_markdown): (String, String, String)) -> Self {
        Self {
            key,
            label,
            detail_markdown,
            recommended: false,
        }
    }
}

impl From<(&str, &str, &str)> for PlanOptionDraft {
    fn from((key, label, detail_markdown): (&str, &str, &str)) -> Self {
        Self {
            key: key.to_string(),
            label: label.to_string(),
            detail_markdown: detail_markdown.to_string(),
            recommended: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanResponse {
    pub selected_option_keys: Vec<String>,
    pub custom_label: String,
    pub custom_detail_markdown: String,
    pub notes: BTreeMap<String, String>,
    pub submitted_at_unix_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDecisionDraftState {
    pub selected_option_keys: Vec<String>,
    pub custom_label: String,
    pub custom_detail_markdown: String,
    pub notes: BTreeMap<String, String>,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDecision {
    pub id: u64,
    pub key: String,
    pub title: String,
    pub prompt_markdown: String,
    pub detail_markdown: String,
    pub mode: String,
    pub state: String,
    pub allow_custom: bool,
    pub require_selection: bool,
    pub batch_key: Option<String>,
    pub batch_position: Option<u32>,
    pub resolution_markdown: String,
    pub options: Vec<PlanOption>,
    pub response: Option<PlanResponse>,
    pub draft: Option<PlanDecisionDraftState>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanMessage {
    pub id: u64,
    pub decision_id: Option<u64>,
    pub author: String,
    pub session_id: Option<u64>,
    pub body: String,
    pub created_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDetail {
    pub plan: serde_json::Value,
    pub markdown: String,
    pub decisions: Vec<PlanDecision>,
    pub messages: Vec<PlanMessage>,
}

pub struct PlanDecisionDraft<'a> {
    pub key: &'a str,
    pub title: &'a str,
    pub prompt_markdown: &'a str,
    pub detail_markdown: &'a str,
    pub mode: PlanDecisionMode,
    pub allow_custom: bool,
    pub require_selection: bool,
    pub options: &'a [PlanOptionDraft],
}

pub struct PlanResponseDraft<'a> {
    pub decision_id: u64,
    pub selected_option_keys: &'a [String],
    pub custom_label: &'a str,
    pub custom_detail_markdown: &'a str,
    pub notes: &'a BTreeMap<String, String>,
}

pub struct PlanDraftUpdate<'a> {
    pub decision_id: u64,
    pub selected_option_keys: &'a [String],
    pub custom_label: &'a str,
    pub custom_detail_markdown: &'a str,
    pub notes: &'a BTreeMap<String, String>,
}

fn parse_plan_state(value: String) -> PlanState {
    PlanState::parse(&value).unwrap_or_default()
}

fn row_plan(row: &rusqlite::Row<'_>) -> rusqlite::Result<Plan> {
    Ok(Plan {
        id: row.get::<_, i64>(0)? as u64,
        project_id: row.get::<_, i64>(1)? as u64,
        bucket_id: row.get::<_, i64>(2)? as u64,
        owning_session_id: row.get::<_, i64>(3)? as u64,
        creating_session_id: row.get::<_, i64>(4)? as u64,
        name: row.get(5)?,
        summary: row.get(6)?,
        state: parse_plan_state(row.get(7)?),
        markdown_path: row.get(8)?,
        revision: row.get::<_, i64>(9)? as u64,
        active_decision_id: row.get::<_, Option<i64>>(10)?.map(|id| id as u64),
        linked_item_ids: Vec::new(),
        created_at_unix_ms: row.get(11)?,
        updated_at_unix_ms: row.get(12)?,
    })
}

const PLAN_SELECT: &str = "SELECT p.id,p.project_id,pr.bucket_id,p.owning_session_id,\
    p.creating_session_id,p.name,p.summary,p.state,p.markdown_path,p.revision,\
    p.active_decision_id,p.created_at_unix_ms,p.updated_at_unix_ms \
    FROM plans p JOIN projects pr ON pr.id=p.project_id";

fn attach_items(conn: &Connection, plan: &mut Plan) -> rusqlite::Result<()> {
    plan.linked_item_ids = conn
        .prepare("SELECT i.item_number FROM plan_items pi JOIN items i ON i.id=pi.item_id WHERE pi.plan_id=?1 ORDER BY i.item_number")?
        .query_map(params![plan.id as i64], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|id| id as u64)
        .collect();
    Ok(())
}

pub(crate) fn plans_from_conn(
    conn: &Connection,
    project_id: Option<u64>,
    include_archived: bool,
) -> Result<Vec<Plan>> {
    let sql = match (project_id, include_archived) {
        (Some(_), true) => format!("{PLAN_SELECT} WHERE p.project_id=?1 ORDER BY p.id"),
        (Some(_), false) => {
            format!("{PLAN_SELECT} WHERE p.project_id=?1 AND p.state!='archived' ORDER BY p.id")
        }
        (None, true) => format!("{PLAN_SELECT} ORDER BY p.id"),
        (None, false) => format!("{PLAN_SELECT} WHERE p.state!='archived' ORDER BY p.id"),
    };
    let mut plans = if let Some(project_id) = project_id {
        conn.prepare(&sql)?
            .query_map(params![project_id as i64], row_plan)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        conn.prepare(&sql)?
            .query_map([], row_plan)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for plan in &mut plans {
        attach_items(conn, plan)?;
    }
    Ok(plans)
}

impl Storage {
    #[allow(clippy::too_many_arguments)]
    pub fn create_plan(
        &self,
        project_id: u64,
        session_id: u64,
        name: &str,
        summary: &str,
        markdown_path: &str,
        linked_item_ids: &[u64],
        now: i64,
    ) -> Result<Plan> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO plans(project_id,owning_session_id,creating_session_id,name,summary,\
             markdown_path,created_at_unix_ms,updated_at_unix_ms) VALUES(?1,?2,?2,?3,?4,?5,?6,?6)",
            params![
                project_id as i64,
                session_id as i64,
                name,
                summary,
                markdown_path,
                now
            ],
        )?;
        let id = tx.last_insert_rowid() as u64;
        for item_id in linked_item_ids {
            let linked = tx.execute(
                "INSERT INTO plan_items(plan_id,item_id) SELECT ?1,id FROM items WHERE item_number=?2 AND bucket_id=(SELECT bucket_id FROM projects WHERE id=?3)",
                params![id as i64, *item_id as i64, project_id as i64],
            )?;
            if linked == 0 {
                return Err(StorageError::Conflict(format!(
                    "item {item_id} is not in the plan's bucket"
                )));
            }
        }
        tx.commit()?;
        drop(conn);
        self.get_plan(id)
    }

    pub fn get_plan(&self, id: u64) -> Result<Plan> {
        let conn = self.conn.lock().unwrap();
        let mut plan = conn
            .query_row(
                &format!("{PLAN_SELECT} WHERE p.id=?1"),
                params![id as i64],
                row_plan,
            )
            .optional()?
            .ok_or(StorageError::NotFound("plan", id))?;
        attach_items(&conn, &mut plan)?;
        Ok(plan)
    }

    pub fn list_plans(&self, project_id: Option<u64>, include_archived: bool) -> Result<Vec<Plan>> {
        let conn = self.conn.lock().unwrap();
        plans_from_conn(&conn, project_id, include_archived)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_plan(
        &self,
        id: u64,
        name: Option<&str>,
        summary: Option<&str>,
        state: Option<PlanState>,
        markdown_path: Option<&str>,
        linked_item_ids: Option<&[u64]>,
        now: i64,
    ) -> Result<Plan> {
        let current = self.get_plan(id)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE plans SET name=?2,summary=?3,state=?4,markdown_path=?5,updated_at_unix_ms=MAX(updated_at_unix_ms+1,?6) WHERE id=?1",
            params![id as i64, name.unwrap_or(&current.name), summary.unwrap_or(&current.summary), state.unwrap_or(current.state).as_str(), markdown_path.unwrap_or(&current.markdown_path), now],
        )?;
        if let Some(item_ids) = linked_item_ids {
            tx.execute(
                "DELETE FROM plan_items WHERE plan_id=?1",
                params![id as i64],
            )?;
            for item_id in item_ids {
                let linked = tx.execute(
                    "INSERT INTO plan_items(plan_id,item_id) SELECT ?1,id FROM items WHERE item_number=?2 AND bucket_id=(SELECT bucket_id FROM projects WHERE id=?3)",
                    params![id as i64, *item_id as i64, current.project_id as i64],
                )?;
                if linked == 0 {
                    return Err(StorageError::Conflict(format!(
                        "item {item_id} is not in the plan's bucket"
                    )));
                }
            }
        }
        tx.commit()?;
        drop(conn);
        self.get_plan(id)
    }

    pub fn sync_plan_markdown(
        &self,
        id: u64,
        path: &str,
        markdown: &str,
        now: i64,
    ) -> Result<Plan> {
        if markdown.chars().count() > PLAN_MARKDOWN_MAX {
            return Err(StorageError::Validation {
                field: "plan markdown",
                limit: PLAN_MARKDOWN_MAX,
                actual: markdown.chars().count(),
                unit: "characters",
            });
        }
        self.conn.lock().unwrap().execute(
            "UPDATE plans SET markdown_path=?2,markdown_snapshot=?3,revision=revision+1,updated_at_unix_ms=MAX(updated_at_unix_ms+1,?4) WHERE id=?1",
            params![id as i64, path, markdown, now],
        )?;
        self.get_plan(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_plan_decision(
        &self,
        plan_id: u64,
        key: &str,
        title: &str,
        prompt_markdown: &str,
        detail_markdown: &str,
        mode: PlanDecisionMode,
        allow_custom: bool,
        require_selection: bool,
        options: &[PlanOptionDraft],
        now: i64,
    ) -> Result<(Plan, PlanDecision)> {
        let draft = PlanDecisionDraft {
            key,
            title,
            prompt_markdown,
            detail_markdown,
            mode,
            allow_custom,
            require_selection,
            options,
        };
        let (plan, mut decisions) =
            self.upsert_plan_decision_batch(plan_id, None, &[draft], now)?;
        Ok((plan, decisions.remove(0)))
    }

    pub fn upsert_plan_decision_batch(
        &self,
        plan_id: u64,
        batch_key: Option<&str>,
        drafts: &[PlanDecisionDraft<'_>],
        now: i64,
    ) -> Result<(Plan, Vec<PlanDecision>)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut decision_ids = Vec::with_capacity(drafts.len());
        for (batch_position, draft) in drafts.iter().enumerate() {
            tx.execute(
                "INSERT INTO plan_decisions(plan_id,decision_key,title,prompt_markdown,detail_markdown,mode,state,allow_custom,require_selection,batch_key,batch_position,created_at_unix_ms,updated_at_unix_ms) \
                 VALUES(?1,?2,?3,?4,?5,?6,'open',?7,?8,?9,?10,?11,?11) ON CONFLICT(plan_id,decision_key) DO UPDATE SET \
                 title=excluded.title,prompt_markdown=excluded.prompt_markdown,detail_markdown=excluded.detail_markdown,mode=excluded.mode,state='open',allow_custom=excluded.allow_custom,require_selection=excluded.require_selection,batch_key=excluded.batch_key,batch_position=excluded.batch_position,resolution_markdown='',updated_at_unix_ms=excluded.updated_at_unix_ms",
                params![plan_id as i64, draft.key, draft.title, draft.prompt_markdown, draft.detail_markdown, draft.mode.as_str(), draft.allow_custom, draft.require_selection, batch_key, batch_position as i64, now],
            )?;
            let decision_id: i64 = tx.query_row(
                "SELECT id FROM plan_decisions WHERE plan_id=?1 AND decision_key=?2",
                params![plan_id as i64, draft.key],
                |row| row.get(0),
            )?;
            decision_ids.push(decision_id);
            tx.execute(
                "DELETE FROM plan_responses WHERE decision_id=?1",
                params![decision_id],
            )?;
            tx.execute(
                "DELETE FROM plan_options WHERE decision_id=?1",
                params![decision_id],
            )?;
            for (position, opt) in draft.options.iter().enumerate() {
                let is_rec = if draft.mode == PlanDecisionMode::Single {
                    opt.recommended
                } else {
                    false
                };
                tx.execute(
                    "INSERT INTO plan_options(decision_id,option_key,label,detail_markdown,position,is_recommended) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![decision_id, opt.key, opt.label, opt.detail_markdown, position as i64, is_rec],
                )?;
            }
        }
        let active_decision_id = decision_ids
            .first()
            .copied()
            .ok_or_else(|| StorageError::Conflict("a decision batch must not be empty".into()))?;
        tx.execute(
            "UPDATE plans SET active_decision_id=?2,updated_at_unix_ms=MAX(updated_at_unix_ms+1,?3) WHERE id=?1",
            params![plan_id as i64, active_decision_id, now],
        )?;
        tx.commit()?;
        drop(conn);
        let plan = self.get_plan(plan_id)?;
        let decisions = self
            .plan_decisions(plan_id)?
            .into_iter()
            .filter(|decision| decision_ids.contains(&(decision.id as i64)))
            .collect();
        Ok((plan, decisions))
    }

    pub fn resolve_plan_decision(
        &self,
        plan_id: u64,
        key: &str,
        resolution: &str,
        now: i64,
    ) -> Result<Plan> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let (decision_id, batch_key): (i64, Option<String>) = tx
            .query_row(
                "SELECT id,batch_key FROM plan_decisions WHERE plan_id=?1 AND decision_key=?2",
                params![plan_id as i64, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StorageError::NotFound("plan decision", plan_id))?;
        tx.execute("UPDATE plan_decisions SET state='resolved',resolution_markdown=?2,updated_at_unix_ms=?3 WHERE id=?1", params![decision_id,resolution,now])?;
        tx.execute(
            "DELETE FROM plan_decision_drafts WHERE decision_id=?1",
            params![decision_id],
        )?;
        let active_id: Option<i64> = tx.query_row(
            "SELECT active_decision_id FROM plans WHERE id=?1",
            params![plan_id as i64],
            |row| row.get(0),
        )?;
        let active_in_batch = match (active_id, batch_key.as_deref()) {
            (Some(active_id), Some(batch_key)) => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_decisions WHERE id=?1 AND batch_key=?2)",
                params![active_id, batch_key],
                |row| row.get(0),
            )?,
            (Some(active_id), None) => active_id == decision_id,
            (None, _) => false,
        };
        if active_in_batch {
            let next_id: Option<i64> = if let Some(batch_key) = batch_key {
                tx.query_row(
                    "SELECT id FROM plan_decisions WHERE plan_id=?1 AND batch_key=?2 AND state!='resolved' ORDER BY batch_position,id LIMIT 1",
                    params![plan_id as i64, batch_key],
                    |row| row.get(0),
                )
                .optional()?
            } else {
                None
            };
            tx.execute(
                "UPDATE plans SET active_decision_id=?2,updated_at_unix_ms=MAX(updated_at_unix_ms+1,?3) WHERE id=?1",
                params![plan_id as i64, next_id, now],
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.get_plan(plan_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn submit_plan_response(
        &self,
        plan_id: u64,
        decision_id: u64,
        selected: &[String],
        custom_label: &str,
        custom_detail: &str,
        notes: &BTreeMap<String, String>,
        now: i64,
    ) -> Result<Plan> {
        self.submit_plan_responses(
            plan_id,
            &[PlanResponseDraft {
                decision_id,
                selected_option_keys: selected,
                custom_label,
                custom_detail_markdown: custom_detail,
                notes,
            }],
            now,
        )
    }

    pub fn submit_plan_responses(
        &self,
        plan_id: u64,
        responses: &[PlanResponseDraft<'_>],
        now: i64,
    ) -> Result<Plan> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for response in responses {
            let belongs: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_decisions WHERE id=?1 AND plan_id=?2)",
                params![response.decision_id as i64, plan_id as i64],
                |row| row.get(0),
            )?;
            if !belongs {
                return Err(StorageError::NotFound(
                    "plan decision",
                    response.decision_id,
                ));
            }
            tx.execute("INSERT INTO plan_responses(decision_id,selected_option_keys,custom_label,custom_detail_markdown,notes,submitted_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(decision_id) DO UPDATE SET selected_option_keys=excluded.selected_option_keys,custom_label=excluded.custom_label,custom_detail_markdown=excluded.custom_detail_markdown,notes=excluded.notes,submitted_at_unix_ms=excluded.submitted_at_unix_ms",params![response.decision_id as i64,serde_json::to_string(response.selected_option_keys).unwrap_or_else(|_|"[]".into()),response.custom_label,response.custom_detail_markdown,serde_json::to_string(response.notes).unwrap_or_else(|_|"{}".into()),now])?;
            let accepted = tx.execute(
                "UPDATE plan_decisions SET state='waiting',updated_at_unix_ms=?2 WHERE id=?1 AND state='open'",
                params![response.decision_id as i64, now],
            )?;
            if accepted != 1 {
                return Err(StorageError::Conflict(
                    "the planning response was already submitted".into(),
                ));
            }
            tx.execute(
                "DELETE FROM plan_decision_drafts WHERE decision_id=?1",
                params![response.decision_id as i64],
            )?;
        }
        tx.execute(
            "UPDATE plans SET updated_at_unix_ms=MAX(updated_at_unix_ms+1,?2) WHERE id=?1",
            params![plan_id as i64, now],
        )?;
        tx.commit()?;
        drop(conn);
        self.get_plan(plan_id)
    }

    pub fn save_plan_decision_drafts(
        &self,
        plan_id: u64,
        drafts: &[PlanDraftUpdate<'_>],
        now: i64,
    ) -> Result<Plan> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for draft in drafts {
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM plan_decisions WHERE id=?1 AND plan_id=?2",
                    params![draft.decision_id as i64, plan_id as i64],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(state) = state else {
                return Err(StorageError::NotFound("plan decision", draft.decision_id));
            };
            if state != "open" {
                return Err(StorageError::Conflict(
                    "cannot save draft for non-open decision".into(),
                ));
            }
            tx.execute(
                "INSERT INTO plan_decision_drafts(decision_id,selected_option_keys,custom_label,custom_detail_markdown,notes,updated_at_unix_ms) \
                 VALUES(?1,?2,?3,?4,?5,?6) \
                 ON CONFLICT(decision_id) DO UPDATE SET \
                 selected_option_keys=excluded.selected_option_keys,\
                 custom_label=excluded.custom_label,\
                 custom_detail_markdown=excluded.custom_detail_markdown,\
                 notes=excluded.notes,\
                 updated_at_unix_ms=excluded.updated_at_unix_ms",
                params![
                    draft.decision_id as i64,
                    serde_json::to_string(draft.selected_option_keys).unwrap_or_else(|_| "[]".into()),
                    draft.custom_label,
                    draft.custom_detail_markdown,
                    serde_json::to_string(draft.notes).unwrap_or_else(|_| "{}".into()),
                    now,
                ],
            )?;
        }
        tx.execute(
            "UPDATE plans SET updated_at_unix_ms=MAX(updated_at_unix_ms+1,?2) WHERE id=?1",
            params![plan_id as i64, now],
        )?;
        tx.commit()?;
        drop(conn);
        self.get_plan(plan_id)
    }

    pub fn add_plan_message(
        &self,
        plan_id: u64,
        decision_id: Option<u64>,
        author: &str,
        session_id: Option<u64>,
        body: &str,
        now: i64,
    ) -> Result<PlanMessage> {
        if body.trim().is_empty() || body.chars().count() > PLAN_MESSAGE_MAX {
            return Err(StorageError::Validation {
                field: "plan message",
                limit: PLAN_MESSAGE_MAX,
                actual: body.chars().count(),
                unit: "characters",
            });
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        if let Some(decision_id) = decision_id {
            let belongs: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_decisions WHERE id=?1 AND plan_id=?2)",
                params![decision_id as i64, plan_id as i64],
                |row| row.get(0),
            )?;
            if !belongs {
                return Err(StorageError::NotFound("plan decision", decision_id));
            }
        }
        tx.execute("INSERT INTO plan_messages(plan_id,decision_id,author,session_id,body,created_at_unix_ms) VALUES(?1,?2,?3,?4,?5,?6)",params![plan_id as i64,decision_id.map(|id|id as i64),author,session_id.map(|id|id as i64),body,now])?;
        let id = tx.last_insert_rowid() as u64;
        tx.execute(
            "UPDATE plans SET updated_at_unix_ms=MAX(updated_at_unix_ms+1,?2) WHERE id=?1",
            params![plan_id as i64, now],
        )?;
        tx.commit()?;
        Ok(PlanMessage {
            id,
            decision_id,
            author: author.into(),
            session_id,
            body: body.into(),
            created_at_unix_ms: now,
        })
    }

    pub fn plan_markdown(&self, plan_id: u64) -> Result<String> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT markdown_snapshot FROM plans WHERE id=?1",
                params![plan_id as i64],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StorageError::NotFound("plan", plan_id))
    }

    pub fn plan_decisions(&self, plan_id: u64) -> Result<Vec<PlanDecision>> {
        let conn = self.conn.lock().unwrap();
        let mut decisions = conn.prepare("SELECT id,decision_key,title,prompt_markdown,detail_markdown,mode,state,allow_custom,require_selection,batch_key,batch_position,resolution_markdown,created_at_unix_ms,updated_at_unix_ms FROM plan_decisions WHERE plan_id=?1 ORDER BY id")?.query_map(params![plan_id as i64],|row| {
            Ok(PlanDecision { id: row.get::<_,i64>(0)? as u64,key:row.get(1)?,title:row.get(2)?,prompt_markdown:row.get(3)?,detail_markdown:row.get(4)?,mode:row.get(5)?,state:row.get(6)?,allow_custom:row.get(7)?,require_selection:row.get(8)?,batch_key:row.get(9)?,batch_position:row.get::<_,Option<i64>>(10)?.map(|value| value as u32),resolution_markdown:row.get(11)?,options:Vec::new(),response:None,draft:None,created_at_unix_ms:row.get(12)?,updated_at_unix_ms:row.get(13)? })
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        for decision in &mut decisions {
            decision.options=conn.prepare("SELECT id,option_key,label,detail_markdown,is_recommended FROM plan_options WHERE decision_id=?1 ORDER BY position,id")?.query_map(params![decision.id as i64],|row|Ok(PlanOption{id:row.get::<_,i64>(0)? as u64,key:row.get(1)?,label:row.get(2)?,detail_markdown:row.get(3)?,recommended:row.get::<_,i64>(4)?!=0}))?.collect::<rusqlite::Result<Vec<_>>>()?;
            decision.response=conn.query_row("SELECT selected_option_keys,custom_label,custom_detail_markdown,notes,submitted_at_unix_ms FROM plan_responses WHERE decision_id=?1",params![decision.id as i64],|row|Ok(PlanResponse{selected_option_keys:serde_json::from_str(&row.get::<_,String>(0)?).unwrap_or_default(),custom_label:row.get(1)?,custom_detail_markdown:row.get(2)?,notes:serde_json::from_str(&row.get::<_,String>(3)?).unwrap_or_default(),submitted_at_unix_ms:row.get(4)?})).optional()?;
            decision.draft=conn.query_row("SELECT selected_option_keys,custom_label,custom_detail_markdown,notes,updated_at_unix_ms FROM plan_decision_drafts WHERE decision_id=?1",params![decision.id as i64],|row|Ok(PlanDecisionDraftState{selected_option_keys:serde_json::from_str(&row.get::<_,String>(0)?).unwrap_or_default(),custom_label:row.get(1)?,custom_detail_markdown:row.get(2)?,notes:serde_json::from_str(&row.get::<_,String>(3)?).unwrap_or_default(),updated_at_unix_ms:row.get(4)?})).optional()?;
        }
        Ok(decisions)
    }

    pub fn plan_messages(&self, plan_id: u64) -> Result<Vec<PlanMessage>> {
        self.conn.lock().unwrap().prepare("SELECT id,decision_id,author,session_id,body,created_at_unix_ms FROM plan_messages WHERE plan_id=?1 ORDER BY id")?.query_map(params![plan_id as i64],|row|Ok(PlanMessage{id:row.get::<_,i64>(0)? as u64,decision_id:row.get::<_,Option<i64>>(1)?.map(|id|id as u64),author:row.get(2)?,session_id:row.get::<_,Option<i64>>(3)?.map(|id|id as u64),body:row.get(4)?,created_at_unix_ms:row.get(5)?}))?.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_protocol::domain::{AgentKind, PermissionMode};

    fn store_with_plan() -> (Storage, u64) {
        let store = Storage::open_in_memory().unwrap();
        let bucket = store.create_bucket("work").unwrap();
        let project = store.create_project(bucket.id, "api", "/tmp/api").unwrap();
        let session = store
            .create_session(
                project.id,
                AgentKind::ClaudeCode,
                "plan",
                "plan the work",
                PermissionMode::Bypass,
                0,
                true,
                false,
                None,
                1,
            )
            .unwrap();
        let plan = store
            .create_plan(project.id, session.id, "Plan", "", "plan.md", &[], 2)
            .unwrap();
        (store, plan.id)
    }

    #[test]
    fn batch_submission_and_resolution_are_atomic() {
        let (store, plan_id) = store_with_plan();
        let options = vec![PlanOptionDraft::new("yes", "Yes", "")];
        let drafts = [
            PlanDecisionDraft {
                key: "first",
                title: "First",
                prompt_markdown: "",
                detail_markdown: "",
                mode: PlanDecisionMode::Single,
                allow_custom: false,
                require_selection: true,
                options: &options,
            },
            PlanDecisionDraft {
                key: "second",
                title: "Second",
                prompt_markdown: "",
                detail_markdown: "",
                mode: PlanDecisionMode::Single,
                allow_custom: false,
                require_selection: true,
                options: &options,
            },
        ];
        let (_, decisions) = store
            .upsert_plan_decision_batch(plan_id, Some("setup"), &drafts, 3)
            .unwrap();
        let selected = vec!["yes".into()];
        let notes = BTreeMap::new();
        let responses = decisions
            .iter()
            .map(|decision| PlanResponseDraft {
                decision_id: decision.id,
                selected_option_keys: &selected,
                custom_label: "",
                custom_detail_markdown: "",
                notes: &notes,
            })
            .collect::<Vec<_>>();

        store.submit_plan_responses(plan_id, &responses, 4).unwrap();
        assert!(matches!(
            store.submit_plan_responses(plan_id, &responses, 5),
            Err(StorageError::Conflict(_))
        ));
        store
            .resolve_plan_decision(plan_id, "first", "Accepted", 6)
            .unwrap();
        assert_eq!(
            store.get_plan(plan_id).unwrap().active_decision_id,
            Some(decisions[1].id)
        );
        store
            .resolve_plan_decision(plan_id, "second", "Accepted", 7)
            .unwrap();
        assert_eq!(store.get_plan(plan_id).unwrap().active_decision_id, None);
    }
}
