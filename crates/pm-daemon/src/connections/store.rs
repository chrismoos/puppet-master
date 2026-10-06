use anyhow::{bail, Context, Result};
use rand::RngCore;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use super::{Call, Config, Connection, Tool};
use crate::storage::{now_unix_ms, Storage};

const RECENT_CALL_LIMIT: usize = 200;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS connections (
 id INTEGER PRIMARY KEY,
 config TEXT NOT NULL,
 project_id INTEGER NOT NULL REFERENCES projects(id),
 revision INTEGER NOT NULL DEFAULT 1,
 active INTEGER NOT NULL DEFAULT 0,
 created_by_session INTEGER REFERENCES sessions(id),
 credential TEXT,
 tested_revision INTEGER,
 tested_at INTEGER,
 tools TEXT NOT NULL DEFAULT '[]',
 policy_proposal TEXT
);
CREATE TABLE IF NOT EXISTS connection_calls (
 id TEXT PRIMARY KEY,
 session_id INTEGER NOT NULL REFERENCES sessions(id),
 request_id TEXT NOT NULL,
 connection_id INTEGER NOT NULL REFERENCES connections(id),
 status TEXT NOT NULL,
 expires_at INTEGER NOT NULL,
 record TEXT NOT NULL,
 UNIQUE(session_id, request_id)
);
CREATE INDEX IF NOT EXISTS connection_calls_status ON connection_calls(status, expires_at);
CREATE TABLE IF NOT EXISTS connection_oauth (
 state TEXT PRIMARY KEY,
 connection_id INTEGER NOT NULL REFERENCES connections(id),
 revision INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 secret TEXT NOT NULL
);
";

/// A database created before `tested_at` or `deleted_at` existed gains
/// the columns here.
pub(crate) fn migrate(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    for column in ["tested_at", "deleted_at"] {
        let present: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('connections') WHERE name=?1",
            [column],
            |row| row.get(0),
        )?;
        if present == 0 {
            conn.execute_batch(&format!(
                "ALTER TABLE connections ADD COLUMN {column} INTEGER;"
            ))?;
        }
    }
    Ok(())
}

pub(super) fn random_id() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn read_connection(row: &rusqlite::Row<'_>) -> rusqlite::Result<Connection> {
    let decode = |index| -> rusqlite::Result<Value> {
        let raw: String = row.get(index)?;
        serde_json::from_str(&raw).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })
    };
    Ok(Connection {
        id: row.get(0)?,
        config: serde_json::from_value(decode(1)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        revision: row.get(2)?,
        active: row.get(3)?,
        created_by_session: row.get(4)?,
        credential_set: row.get(5)?,
        tested_revision: row.get(6)?,
        tested_at: row.get(10)?,
        deleted_at: row.get(11)?,
        policy_proposal: row
            .get::<_, Option<String>>(8)?
            .map(|raw| {
                serde_json::from_str(&raw).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        tool_count: row.get(9)?,
        tools: serde_json::from_value(decode(7)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

const CONNECTION_COLUMNS: &str = "id, config, revision, active, created_by_session, credential IS NOT NULL, tested_revision, tools, policy_proposal, json_array_length(tools), tested_at, deleted_at";

impl Storage {
    pub(crate) fn connection_summaries(&self) -> Result<Vec<Connection>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, json_remove(config, '$.schema', '$.rules'), revision, active, created_by_session, credential IS NOT NULL, tested_revision, '[]', json_remove(policy_proposal, '$.setup.schema'), json_array_length(tools), tested_at, deleted_at FROM connections WHERE deleted_at IS NULL ORDER BY id")?;
        let rows = stmt
            .query_map([], read_connection)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub(crate) fn connection(&self, id: u64) -> Result<Connection> {
        Ok(self.conn.lock().unwrap().query_row(
            &format!("SELECT {CONNECTION_COLUMNS} FROM connections WHERE id=?1"),
            [id],
            read_connection,
        )?)
    }

    pub(crate) fn create_connection(
        &self,
        config: Config,
        session: Option<u64>,
    ) -> Result<Connection> {
        let id = {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO connections(config,project_id,created_by_session) VALUES(?1,?2,?3)",
                params![serde_json::to_string(&config)?, config.project_id, session],
            )?;
            conn.last_insert_rowid() as u64
        };
        if config.kind == super::Kind::Openapi {
            let tools = super::openapi::import(
                config.schema.as_ref().context("Missing OpenAPI document")?,
            )?;
            self.conn.lock().unwrap().execute(
                "UPDATE connections SET tools=?1 WHERE id=?2",
                params![serde_json::to_string(&tools)?, id],
            )?;
        }
        self.connection(id)
    }

    pub(crate) fn update_connection(
        &self,
        id: u64,
        revision: u64,
        config: &Config,
        credential: Option<Option<String>>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let raw: String =
            tx.query_row("SELECT config FROM connections WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        let old: Config = serde_json::from_str(&raw)?;
        let mut config = config.clone();
        if old.kind != config.kind || old.endpoint != config.endpoint || old.schema != config.schema
        {
            config.rules.clear();
        }
        let preserve = credential.is_none()
            && old.kind == config.kind
            && old.endpoint == config.endpoint
            && old.project_id == config.project_id
            && old.schema == config.schema
            && old.oauth == config.oauth;
        let changed = tx.execute("UPDATE connections SET config=?1,project_id=?2,revision=revision+1,active=CASE WHEN ?5 THEN active ELSE 0 END,tested_revision=CASE WHEN ?5 AND tested_revision=revision THEN revision+1 ELSE NULL END,policy_proposal=NULL WHERE id=?3 AND revision=?4 AND deleted_at IS NULL", params![serde_json::to_string(&config)?, config.project_id, id, revision, preserve])?;
        if changed != 1 {
            bail!("Connection changed. Reload before saving");
        }
        if let Some(credential) = credential {
            tx.execute(
                "UPDATE connections SET credential=?1 WHERE id=?2",
                params![credential, id],
            )?;
        }
        tx.execute("UPDATE connection_calls SET status='canceled' WHERE connection_id=?1 AND status IN ('pending','authorized')", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn connection_secret(&self, id: u64) -> Result<Option<String>> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT credential FROM connections WHERE id=?1",
            [id],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn record_connection_test(
        &self,
        id: u64,
        revision: u64,
        tools: Vec<Tool>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let old = tx.query_row(
            &format!("SELECT {CONNECTION_COLUMNS} FROM connections WHERE id=?1"),
            [id],
            read_connection,
        )?;
        if old.deleted_at.is_some() {
            bail!("Connection was removed");
        }
        if old.revision != revision {
            bail!("Connection changed during testing. Test again");
        }
        let changed = !old.tools.is_empty() && old.tools != tools;
        if changed {
            let mut config = old.config;
            config.rules.clear();
            tx.execute("UPDATE connections SET config=?1,revision=revision+1,active=0,tested_revision=revision+1,tested_at=?4,tools=?2,policy_proposal=NULL WHERE id=?3",params![serde_json::to_string(&config)?,serde_json::to_string(&tools)?,id,now_unix_ms()])?;
            tx.execute("UPDATE connection_calls SET status='canceled' WHERE connection_id=?1 AND status IN ('pending','authorized')",[id])?;
        } else {
            tx.execute(
                "UPDATE connections SET tools=?1,tested_revision=?2,tested_at=?4 WHERE id=?3",
                params![serde_json::to_string(&tools)?, revision, id, now_unix_ms()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn activate_connection(&self, id: u64, revision: u64, active: bool) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let changed = tx.execute("UPDATE connections SET active=?1 WHERE id=?2 AND revision=?3 AND deleted_at IS NULL AND (?1=0 OR tested_revision=revision)", params![active,id,revision])?;
        if changed != 1 {
            bail!("Connection changed or needs a successful test before activation");
        }
        if !active {
            tx.execute("UPDATE connection_calls SET status='canceled' WHERE connection_id=?1 AND status IN ('pending','authorized')", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// A removed connection keeps its row and its call history, leaves
    /// every list, and can never be used or activated again.
    pub(crate) fn delete_connection(&self, id: u64, revision: u64) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE connections SET deleted_at=?1, active=0 WHERE id=?2 AND revision=?3 AND deleted_at IS NULL",
            params![now_unix_ms(), id, revision],
        )?;
        if changed != 1 {
            bail!("Connection changed or was already removed");
        }
        tx.execute("UPDATE connection_calls SET status='canceled' WHERE connection_id=?1 AND status IN ('pending','authorized')", [id])?;
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn create_connection_call(&self, call: Call) -> Result<(Call, bool)> {
        self.persist_connection_call(call, false)
    }

    pub(crate) fn create_live_connection_call(&self, call: Call) -> Result<(Call, bool)> {
        self.persist_connection_call(call, true)
    }

    fn persist_connection_call(&self, call: Call, require_active: bool) -> Result<(Call, bool)> {
        let inserted = {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            if require_active {
                let valid: bool = tx.query_row("SELECT active AND revision=?2 AND tested_revision=revision FROM connections WHERE id=?1", params![call.connection_id, call.connection_revision], |row|row.get(0))?;
                if !valid {
                    bail!("Connection changed before the request was saved. Inspect it and submit a new request");
                }
            }
            let inserted = tx.execute("INSERT OR IGNORE INTO connection_calls(id,session_id,request_id,connection_id,status,expires_at,record) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![call.id,call.session_id,call.request_id,call.connection_id,call.status,call.expires_at,serde_json::to_string(&call)?])?;
            tx.commit()?;
            inserted
        };
        let stored = self
            .connection_call_by_request(call.session_id, &call.request_id)?
            .context("Call was not saved")?;
        if stored.connection_id != call.connection_id
            || stored.tool != call.tool
            || stored.arguments != call.arguments
        {
            bail!("request_id already identifies a different call");
        }
        Ok((stored, inserted == 1))
    }

    pub(crate) fn connection_call_by_request(
        &self,
        session: u64,
        request_id: &str,
    ) -> Result<Option<Call>> {
        let id: Option<String> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM connection_calls WHERE session_id=?1 AND request_id=?2",
                params![session, request_id],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|id| self.connection_call(&id)).transpose()
    }

    pub(crate) fn connection_call(&self, id: &str) -> Result<Call> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE connection_calls SET status='expired' WHERE id=?1 AND status IN ('pending','authorized') AND expires_at<=?2", params![id,now_unix_ms()])?;
        let (raw, status): (String, String) = conn.query_row(
            "SELECT record,status FROM connection_calls WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut call: Call = serde_json::from_str(&raw)?;
        call.status = status;
        Ok(call)
    }

    #[cfg(test)]
    fn connection_calls_for_session(&self, session: Option<u64>) -> Result<Vec<Call>> {
        let ids = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare("SELECT id FROM connection_calls WHERE (?1 IS NULL OR session_id=?1) ORDER BY rowid DESC LIMIT ?2")?;
            let rows = stmt
                .query_map(params![session, RECENT_CALL_LIMIT], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        ids.iter().map(|id| self.connection_call(id)).collect()
    }

    pub(crate) fn admin_connection_call_summaries(
        &self,
        session: Option<u64>,
    ) -> Result<Vec<Value>> {
        self.connection_call_summaries_filtered(session, None)
    }

    pub(super) fn connection_call_summaries_in_scope(
        &self,
        scope: &super::scope::ConnectionScope,
    ) -> Result<Vec<Value>> {
        self.connection_call_summaries_filtered(
            (!scope.supervisor).then_some(scope.session_id),
            Some(scope),
        )
    }

    fn connection_call_summaries_filtered(
        &self,
        session: Option<u64>,
        scope: Option<&super::scope::ConnectionScope>,
    ) -> Result<Vec<Value>> {
        let projects = scope
            .map(|scope| serde_json::to_string(&scope.projects))
            .transpose()?;
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE connection_calls SET status='expired' WHERE status IN ('pending','authorized') AND expires_at<=?1", [now_unix_ms()])?;
        let mut statement = conn.prepare("SELECT json_set(call.record, '$.arguments', NULL, '$.result', NULL), call.status, json_type(call.record, '$.result') FROM connection_calls call JOIN sessions owner ON owner.id = call.session_id JOIN connections connection ON connection.id = call.connection_id WHERE (?1 IS NULL OR call.session_id=?1) AND (?3 IS NULL OR (owner.project_id IN (SELECT value FROM json_each(?3)) AND connection.project_id IN (SELECT value FROM json_each(?3)) AND json_extract(call.record, '$.project_id') IN (SELECT value FROM json_each(?3)))) ORDER BY call.rowid DESC LIMIT ?2")?;
        let records = statement
            .query_map(params![session, RECENT_CALL_LIMIT, projects], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        records
            .into_iter()
            .map(|(raw, status, result_type)| {
                let mut summary: Value = serde_json::from_str(&raw)?;
                summary["status"] = Value::String(status);
                summary["has_result"] = Value::Bool(result_type.is_some_and(|kind| kind != "null"));
                Ok(summary)
            })
            .collect()
    }

    /// Calls that waited on a decision, newest request first, without arguments or results.
    pub(crate) fn connection_approval_summaries(&self) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE connection_calls SET status='expired' WHERE status IN ('pending','authorized') AND expires_at<=?1", [now_unix_ms()])?;
        let mut statement = conn.prepare(
            "SELECT json_set(record, '$.arguments', NULL, '$.result', NULL), status, json_type(record, '$.result') \
             FROM connection_calls \
             WHERE json_extract(record, '$.requires_approval') = 1 OR status = 'pending' \
                OR json_extract(record, '$.decided_by') IS NOT NULL \
             ORDER BY json_extract(record, '$.created_at') DESC, rowid DESC LIMIT ?1",
        )?;
        let records = statement
            .query_map(params![RECENT_CALL_LIMIT], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        records
            .into_iter()
            .map(|(raw, status, result_type)| {
                let mut summary: Value = serde_json::from_str(&raw)?;
                summary["status"] = Value::String(status);
                summary["has_result"] = Value::Bool(result_type.is_some_and(|kind| kind != "null"));
                summary["requires_approval"] = Value::Bool(true);
                Ok(summary)
            })
            .collect()
    }

    pub(crate) fn decide_connection_call(
        &self,
        id: &str,
        approve: bool,
        username: &str,
    ) -> Result<()> {
        let mut call = self.connection_call(id)?;
        call.decided_by = Some(username.into());
        call.decided_at = Some(now_unix_ms());
        let status = if approve { "authorized" } else { "denied" };
        self.conn.lock().unwrap().execute("UPDATE connection_calls SET status=?1,record=?2 WHERE id=?3 AND status='pending' AND expires_at>?4", params![status,serde_json::to_string(&call)?,id,now_unix_ms()])?;
        Ok(())
    }

    pub(crate) fn claim_connection_call(&self, id: &str) -> Result<Option<Call>> {
        let changed = self.conn.lock().unwrap().execute("UPDATE connection_calls SET status='executing' WHERE id=?1 AND status='authorized' AND expires_at>?2", params![id,now_unix_ms()])?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(self.connection_call(id)?))
    }

    pub(crate) fn finish_connection_call(
        &self,
        id: &str,
        status: &str,
        result: Option<Value>,
        error: Option<String>,
    ) -> Result<()> {
        let mut call = self.connection_call(id)?;
        call.result = result;
        call.error = error;
        call.finished_at = Some(now_unix_ms());
        self.conn.lock().unwrap().execute(
            "UPDATE connection_calls SET status=?1,record=?2 WHERE id=?3 AND status='executing'",
            params![status, serde_json::to_string(&call)?, id],
        )?;
        Ok(())
    }

    pub(crate) fn recover_connection_calls(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE connection_calls SET status='outcome_unknown' WHERE status='executing'",
            [],
        )?;
        let mut stmt = conn.prepare("SELECT id FROM connection_calls WHERE status='authorized'")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub(crate) fn propose_connection_policy(
        &self,
        id: u64,
        revision: u64,
        proposal: &Value,
    ) -> Result<()> {
        if self.conn.lock().unwrap().execute(
            "UPDATE connections SET policy_proposal=?1 WHERE id=?2 AND revision=?3 AND deleted_at IS NULL",
            params![serde_json::to_string(proposal)?, id, revision],
        )? != 1
        {
            bail!("Connection changed. Prepare a new proposal");
        }
        Ok(())
    }

    pub(crate) fn apply_connection_policy(
        &self,
        id: u64,
        revision: u64,
        proposal_id: &str,
        accept: bool,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let (raw, proposal, current_revision, tools): (String, Option<String>, u64, String) = tx
            .query_row(
                "SELECT config,policy_proposal,revision,tools FROM connections WHERE id=?1 AND deleted_at IS NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        let proposal: Value =
            serde_json::from_str(&proposal.context("No policy proposal is pending")?)?;
        if revision != current_revision
            || proposal["revision"].as_u64() != Some(revision)
            || proposal["id"].as_str() != Some(proposal_id)
        {
            bail!("The proposal changed. Review the latest proposal");
        }
        if accept {
            let policy: super::PolicyDraft = serde_json::from_value(proposal["policy"].clone())?;
            let old: Config = serde_json::from_str(&raw)?;
            if proposal["setup"].is_object() {
                let setup: super::update::Setup =
                    serde_json::from_value(proposal["setup"].clone())?;
                let mut config = setup.with_policy(policy);
                if super::update::resets_rules(&old, &config) {
                    config.rules.clear();
                }
                let discovered: Vec<Tool> = serde_json::from_str(&tools)?;
                let tools = super::update::tools_for(&config, &discovered)?;
                config
                    .rules
                    .retain(|name, _| tools.iter().any(|tool| &tool.name == name));
                let reactivate = super::update::requires_activation(&old, &config);
                let clears_credential = old.endpoint != config.endpoint
                    || old.project_id != config.project_id
                    || old.oauth != config.oauth;
                tx.execute("UPDATE connections SET config=?1,project_id=?2,tools=?3,revision=revision+1,active=CASE WHEN ?4 THEN 0 ELSE active END,tested_revision=CASE WHEN NOT ?4 AND tested_revision=revision THEN revision+1 ELSE NULL END,credential=CASE WHEN ?5 THEN NULL ELSE credential END,policy_proposal=NULL WHERE id=?6",params![serde_json::to_string(&config)?,config.project_id,serde_json::to_string(&tools)?,reactivate,clears_credential,id])?;
            } else {
                let mut config = old;
                config.read_policy = policy.read_policy;
                config.write_policy = policy.write_policy;
                config.unknown_policy = policy.unknown_policy;
                config.rules = policy.rules;
                tx.execute("UPDATE connections SET config=?1,revision=revision+1,tested_revision=CASE WHEN tested_revision=revision THEN revision+1 ELSE NULL END,policy_proposal=NULL WHERE id=?2",params![serde_json::to_string(&config)?,id])?;
            }
            tx.execute("UPDATE connection_calls SET status='canceled' WHERE connection_id=?1 AND status IN ('pending','authorized')",[id])?;
        } else {
            tx.execute(
                "UPDATE connections SET policy_proposal=NULL WHERE id=?1",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn connection_policy_proposal(&self, id: u64) -> Result<Option<Value>> {
        let raw: Option<String> = self.conn.lock().unwrap().query_row(
            "SELECT policy_proposal FROM connections WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        raw.map(|raw| Ok(serde_json::from_str(&raw)?)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_protocol::domain::{AgentKind, PermissionMode};
    use serde_json::json;

    fn fixture() -> (Storage, Connection, u64) {
        let storage = Storage::open_in_memory().unwrap();
        let bucket = storage.create_bucket("connections").unwrap();
        let project = storage
            .create_project(bucket.id, "project", "/tmp/connections-test")
            .unwrap();
        let session = storage
            .create_session(
                project.id,
                AgentKind::ClaudeCode,
                "task",
                "prompt",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                now_unix_ms(),
            )
            .unwrap();
        let config:Config=serde_json::from_value(json!({"name":"test","project_id":project.id,"endpoint":format!("http://{}",std::net::Ipv4Addr::LOCALHOST)})).unwrap();
        let connection = storage.create_connection(config, Some(session.id)).unwrap();
        (storage, connection, session.id)
    }

    fn call(connection: &Connection, session: u64, status: &str) -> Call {
        Call {
            id: random_id(),
            request_id: "request".into(),
            session_id: session,
            project_id: connection.config.project_id,
            connection_id: connection.id,
            connection_revision: connection.revision,
            tool: "update".into(),
            arguments: json!({"name":"recorded"}),
            justification: "test update".into(),
            status: status.into(),
            created_at: now_unix_ms(),
            expires_at: now_unix_ms() + super::super::APPROVAL_TTL_MS,
            decided_by: None,
            decided_at: None,
            finished_at: None,
            result: None,
            error: None,
            requires_approval: status == "pending",
        }
    }

    #[test]
    fn scoped_history_filters_foreign_calls_before_applying_the_recent_limit() {
        let (storage, own, session_id) = fixture();
        let mut visible = call(&own, session_id, "succeeded");
        visible.request_id = "own".into();
        storage.create_connection_call(visible.clone()).unwrap();
        let foreign_bucket = storage.create_bucket("foreign").unwrap();
        let foreign_project = storage
            .create_project(foreign_bucket.id, "foreign", "/tmp/connections-test")
            .unwrap();
        let mut config = own.config.clone();
        config.project_id = foreign_project.id;
        let foreign = storage.create_connection(config, None).unwrap();
        for index in 0..RECENT_CALL_LIMIT {
            let mut hidden = call(&foreign, session_id, "succeeded");
            hidden.request_id = format!("foreign-{index}");
            storage.create_connection_call(hidden).unwrap();
        }
        let session = storage.get_session(session_id).unwrap();
        for supervisor in [false, true] {
            storage
                .set_session_apis(
                    session_id,
                    Some(true),
                    Some(supervisor),
                    Some(if supervisor {
                        pm_protocol::domain::SessionRole::Supervisor
                    } else {
                        pm_protocol::domain::SessionRole::Worker
                    }),
                )
                .unwrap();
            let scope = storage.connection_scope(&session).unwrap();
            let history = storage.connection_call_summaries_in_scope(&scope).unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0]["id"], visible.id);
        }
        assert_eq!(
            storage.admin_connection_call_summaries(None).unwrap().len(),
            RECENT_CALL_LIMIT
        );
    }

    #[test]
    fn a_database_from_before_test_and_removal_times_gains_the_columns_once() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE connections (id INTEGER PRIMARY KEY, config TEXT NOT NULL, tested_revision INTEGER);
             INSERT INTO connections (id, config, tested_revision) VALUES (1, '{}', 4);",
        )
        .unwrap();
        super::migrate(&conn).unwrap();
        super::migrate(&conn).unwrap();
        let (tested_at, deleted_at): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT tested_at, deleted_at FROM connections WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(tested_at, None);
        assert_eq!(deleted_at, None);
    }

    /// Removal keeps the row for the history that points at it and takes
    /// the connection out of every other path: lists, activation, edits
    /// and tests. A second removal, or one against a stale revision, is
    /// refused rather than repeated.
    #[test]
    fn a_removed_connection_leaves_the_list_keeps_its_calls_and_cannot_come_back() {
        let (store, c, session) = fixture();
        store
            .record_connection_test(c.id, c.revision, Vec::new())
            .unwrap();
        store.activate_connection(c.id, c.revision, true).unwrap();
        let (pending, _) = store
            .create_connection_call(call(&c, session, "pending"))
            .unwrap();
        let (done, _) = store
            .create_connection_call(Call {
                request_id: "earlier".into(),
                ..call(&c, session, "succeeded")
            })
            .unwrap();

        assert!(store.delete_connection(c.id, c.revision + 1).is_err());
        store.delete_connection(c.id, c.revision).unwrap();
        assert!(store.delete_connection(c.id, c.revision).is_err());

        let removed = store.connection(c.id).unwrap();
        assert!(removed.deleted_at.is_some());
        assert!(!removed.active);
        assert!(store.connection_summaries().unwrap().is_empty());
        assert_eq!(
            store.connection_call(&pending.id).unwrap().status,
            "canceled"
        );
        assert_eq!(store.connection_call(&done.id).unwrap().status, "succeeded");
        assert!(store.activate_connection(c.id, c.revision, true).is_err());
        assert!(store
            .update_connection(c.id, c.revision, &c.config, None)
            .is_err());
        assert!(store
            .record_connection_test(c.id, c.revision, Vec::new())
            .is_err());
    }

    /// Ordering is by request time, and a decided record from before `requires_approval` still counts.
    #[test]
    fn approval_summaries_are_newest_first_and_only_approvals() {
        let (store, c, session) = fixture();
        let mut allowed = call(&c, session, "authorized");
        allowed.request_id = "allowed".into();
        let mut older = call(&c, session, "pending");
        older.request_id = "older".into();
        older.created_at -= 2_000;
        let mut newer = call(&c, session, "pending");
        newer.request_id = "newer".into();
        let mut legacy = call(&c, session, "denied");
        legacy.request_id = "legacy".into();
        legacy.created_at -= 1_000;
        legacy.requires_approval = false;
        legacy.decided_by = Some("owner".into());
        for call in [newer.clone(), allowed, legacy.clone(), older.clone()] {
            store.create_connection_call(call).unwrap();
        }
        let listed = store.connection_approval_summaries().unwrap();
        let ids: Vec<&str> = listed
            .iter()
            .map(|call| call["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![newer.id.as_str(), legacy.id.as_str(), older.id.as_str()]
        );
        assert!(listed.iter().all(|call| call["arguments"].is_null()));
        assert!(listed.iter().all(|call| call["requires_approval"] == true));
    }

    #[test]
    fn approval_is_claimed_once_and_returns_recorded_result() {
        let (store, c, session) = fixture();
        let (call, inserted) = store
            .create_connection_call(call(&c, session, "pending"))
            .unwrap();
        assert!(inserted);
        store
            .decide_connection_call(&call.id, true, "owner")
            .unwrap();
        assert_eq!(
            store.connection_call(&call.id).unwrap().status,
            "authorized"
        );
        assert_eq!(
            store
                .claim_connection_call(&call.id)
                .unwrap()
                .unwrap()
                .arguments,
            json!({"name":"recorded"})
        );
        assert!(store.claim_connection_call(&call.id).unwrap().is_none());
        store
            .finish_connection_call(&call.id, "succeeded", Some(json!({"updated":true})), None)
            .unwrap();
        let recorded = store.connection_call(&call.id).unwrap();
        assert_eq!(recorded.status, "succeeded");
        assert_eq!(recorded.result, Some(json!({"updated":true})));
        store
            .decide_connection_call(&call.id, false, "other")
            .unwrap();
        assert_eq!(
            store
                .connection_call(&call.id)
                .unwrap()
                .decided_by
                .as_deref(),
            Some("owner")
        );
    }

    #[test]
    fn repeated_request_recovers_call_but_rejects_changed_arguments() {
        let (store, c, session) = fixture();
        let original = call(&c, session, "pending");
        let (first, _) = store.create_connection_call(original.clone()).unwrap();
        let (second, inserted) = store.create_connection_call(original.clone()).unwrap();
        assert!(!inserted);
        assert_eq!(first.id, second.id);
        let mut altered = original;
        altered.arguments = json!({"name":"different"});
        assert!(store.create_connection_call(altered).is_err());
    }

    #[test]
    fn call_summaries_omit_payloads_and_preserve_result_availability() {
        let (store, connection, session) = fixture();
        let (request, _) = store
            .create_connection_call(call(&connection, session, "authorized"))
            .unwrap();
        store.claim_connection_call(&request.id).unwrap().unwrap();
        store
            .finish_connection_call(
                &request.id,
                "succeeded",
                Some(json!({"record":"complete"})),
                None,
            )
            .unwrap();
        let summaries = store
            .admin_connection_call_summaries(Some(session))
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0]["status"], "succeeded");
        assert_eq!(summaries[0]["arguments"], Value::Null);
        assert_eq!(summaries[0]["result"], Value::Null);
        assert_eq!(summaries[0]["has_result"], true);
        let full = store.connection_call(&request.id).unwrap();
        assert_eq!(full.arguments, request.arguments);
        assert_eq!(full.result.unwrap(), json!({"record":"complete"}));
    }

    #[test]
    fn worker_history_is_scoped_before_applying_the_result_limit() {
        let (store, connection, session) = fixture();
        let (original, _) = store
            .create_connection_call(call(&connection, session, "pending"))
            .unwrap();
        let other = store
            .create_session(
                connection.config.project_id,
                AgentKind::ClaudeCode,
                "other",
                "prompt",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                now_unix_ms(),
            )
            .unwrap();
        for _ in 0..201 {
            let mut recent = call(&connection, other.id, "denied");
            recent.request_id = random_id();
            store.create_connection_call(recent).unwrap();
        }
        let history = store.connection_calls_for_session(Some(session)).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, original.id);
        assert_eq!(store.connection_calls_for_session(None).unwrap().len(), 200);
    }

    #[test]
    fn changed_catalog_revokes_tool_classifications_and_pending_approvals() {
        let (store, connection, session) = fixture();
        let tool = Tool {
            name: "read".into(),
            description: "Read".into(),
            input_schema: serde_json::json!({"type":"object"}),
            suggested_access: super::super::Access::Read,
            operation: None,
        };
        store
            .record_connection_test(connection.id, connection.revision, vec![tool.clone()])
            .unwrap();
        let mut config = connection.config;
        config.rules.insert(
            tool.name.clone(),
            super::super::ToolRule {
                access: super::super::Access::Read,
                policy: None,
            },
        );
        store
            .update_connection(connection.id, connection.revision, &config, None)
            .unwrap();
        let revision = store.connection(connection.id).unwrap().revision;
        store
            .activate_connection(connection.id, revision, true)
            .unwrap();
        let current = store.connection(connection.id).unwrap();
        let (pending, _) = store
            .create_connection_call(call(&current, session, "pending"))
            .unwrap();
        let mut changed = tool;
        changed.input_schema = serde_json::json!({"type":"object","required":["new_argument"]});
        store
            .record_connection_test(current.id, revision, vec![changed])
            .unwrap();
        let current = store.connection(current.id).unwrap();
        assert!(!current.active);
        assert!(current.config.rules.is_empty());
        assert!(current.revision > revision);
        assert_eq!(
            store.connection_call(&pending.id).unwrap().status,
            "canceled"
        );
    }

    #[test]
    fn a_configuration_change_cannot_be_followed_by_inserting_a_stale_request() {
        let (store, connection, session) = fixture();
        store
            .record_connection_test(connection.id, connection.revision, Vec::new())
            .unwrap();
        store
            .activate_connection(connection.id, connection.revision, true)
            .unwrap();
        let stale = call(&connection, session, "pending");
        store
            .update_connection(connection.id, connection.revision, &connection.config, None)
            .unwrap();
        assert!(store.create_live_connection_call(stale).is_err());
        assert!(store
            .connection_calls_for_session(Some(session))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn expired_request_cannot_be_approved_without_a_sweep() {
        let (store, c, session) = fixture();
        let mut call = call(&c, session, "pending");
        call.expires_at = now_unix_ms() - 1;
        let (call, _) = store.create_connection_call(call).unwrap();
        store
            .decide_connection_call(&call.id, true, "owner")
            .unwrap();
        assert_eq!(store.connection_call(&call.id).unwrap().status, "expired");
        assert!(store.claim_connection_call(&call.id).unwrap().is_none());
    }

    #[test]
    fn persisted_approval_and_execution_state_survives_reopening_the_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("connections.db");
        let store = Storage::open(&path).unwrap();
        let bucket = store.create_bucket("connections").unwrap();
        let project = store
            .create_project(bucket.id, "project", "/tmp/connections-test")
            .unwrap();
        let session = store
            .create_session(
                project.id,
                AgentKind::ClaudeCode,
                "task",
                "prompt",
                PermissionMode::Default,
                0,
                true,
                false,
                None,
                now_unix_ms(),
            )
            .unwrap();
        let config:Config = serde_json::from_value(json!({"name":"persisted", "project_id":project.id, "endpoint":format!("http://{}",std::net::Ipv4Addr::LOCALHOST)})).unwrap();
        let connection = store.create_connection(config, Some(session.id)).unwrap();
        let (pending, _) = store
            .create_connection_call(call(&connection, session.id, "pending"))
            .unwrap();
        let mut executing = call(&connection, session.id, "authorized");
        executing.request_id = "executing-request".into();
        let (executing, _) = store.create_connection_call(executing).unwrap();
        store.claim_connection_call(&executing.id).unwrap().unwrap();
        drop(store);
        let store = Storage::open(&path).unwrap();
        assert_eq!(
            store.connection(connection.id).unwrap().config.name,
            "persisted"
        );
        assert!(store.recover_connection_calls().unwrap().is_empty());
        assert_eq!(
            store.connection_call(&pending.id).unwrap().status,
            "pending"
        );
        assert_eq!(
            store.connection_call(&executing.id).unwrap().status,
            "outcome_unknown"
        );
        store
            .decide_connection_call(&pending.id, false, "owner")
            .unwrap();
        assert_eq!(store.connection_call(&pending.id).unwrap().status, "denied");
        assert!(store.claim_connection_call(&pending.id).unwrap().is_none());
    }

    #[test]
    fn restart_never_retries_a_call_already_sent() {
        let (store, c, session) = fixture();
        let (call, _) = store
            .create_connection_call(call(&c, session, "authorized"))
            .unwrap();
        store.claim_connection_call(&call.id).unwrap().unwrap();
        assert!(store.recover_connection_calls().unwrap().is_empty());
        assert_eq!(
            store.connection_call(&call.id).unwrap().status,
            "outcome_unknown"
        );
    }

    #[test]
    fn activation_requires_current_test_and_edits_cancel_pending_calls() {
        let (store, c, session) = fixture();
        assert!(store.activate_connection(c.id, c.revision, true).is_err());
        store
            .record_connection_test(c.id, c.revision, Vec::new())
            .unwrap();
        store.activate_connection(c.id, c.revision, true).unwrap();
        let (call, _) = store
            .create_connection_call(call(&c, session, "pending"))
            .unwrap();
        let mut changed = c.config.clone();
        changed.endpoint.push_str("/changed");
        store
            .update_connection(c.id, c.revision, &changed, None)
            .unwrap();
        assert!(!store.connection(c.id).unwrap().active);
        assert_eq!(store.connection_call(&call.id).unwrap().status, "canceled");
        assert!(store
            .update_connection(c.id, c.revision, &c.config, None)
            .is_err());
        assert!(store
            .record_connection_test(c.id, c.revision, Vec::new())
            .is_err());
    }

    #[test]
    fn confirmation_binds_exact_policy_proposal_and_keeps_valid_connection_active() {
        let (store, c, session) = fixture();
        store
            .record_connection_test(c.id, c.revision, Vec::new())
            .unwrap();
        store.activate_connection(c.id, c.revision, true).unwrap();
        let pending = store
            .create_connection_call(call(&c, session, "pending"))
            .unwrap()
            .0;
        let proposal = json!({"id":"first","revision":c.revision,"policy":{"read_policy":"allow","write_policy":"approve","unknown_policy":"deny","rules":{}}});
        store
            .propose_connection_policy(c.id, c.revision, &proposal)
            .unwrap();
        let replacement = json!({"id":"second","revision":c.revision,"policy":proposal["policy"]});
        store
            .propose_connection_policy(c.id, c.revision, &replacement)
            .unwrap();
        assert!(store
            .apply_connection_policy(c.id, c.revision, "first", true)
            .is_err());
        assert_eq!(store.connection(c.id).unwrap().revision, c.revision);
        store
            .apply_connection_policy(c.id, c.revision, "second", true)
            .unwrap();
        let updated = store.connection(c.id).unwrap();
        assert!(updated.active);
        assert_eq!(updated.tested_revision, Some(updated.revision));
        assert_eq!(updated.config.unknown_policy, super::super::Policy::Deny);
        assert_eq!(
            store.connection_call(&pending.id).unwrap().status,
            "canceled"
        );
    }
}
