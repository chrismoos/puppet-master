use std::collections::BTreeSet;

use anyhow::{bail, Result};
use pm_protocol::domain::{Session, SessionRole};
use rusqlite::params;

use crate::storage::Storage;

pub(super) struct ConnectionScope {
    pub(super) projects: BTreeSet<u64>,
    pub(super) session_id: u64,
    pub(super) supervisor: bool,
}

impl ConnectionScope {
    pub(super) fn contains_project(&self, project_id: u64) -> bool {
        self.projects.contains(&project_id)
    }

    pub(super) fn require_project(&self, project_id: u64) -> Result<()> {
        if !self.contains_project(project_id) {
            bail!("Connection project is outside this session's scope");
        }
        Ok(())
    }

    pub(super) fn require_call(
        &self,
        session_id: u64,
        project_id: u64,
        owner_project: u64,
        connection_project: u64,
    ) -> Result<()> {
        if !self.supervisor && session_id != self.session_id {
            bail!("Call belongs to another session");
        }
        for project in [project_id, owner_project, connection_project] {
            self.require_project(project)?;
        }
        Ok(())
    }
}

impl Storage {
    pub(super) fn connection_scope(&self, session: &Session) -> Result<ConnectionScope> {
        let session = self.get_session(session.id)?;
        self.get_project(session.project_id)?;
        let supervisor = session.role == SessionRole::Supervisor;
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare("SELECT target.id FROM projects target JOIN projects source ON source.id = ?1 WHERE target.id = source.id OR (?2 AND target.bucket_id = source.bucket_id)")?;
        let projects = statement
            .query_map(params![session.project_id, supervisor], |row| row.get(0))?
            .collect::<rusqlite::Result<BTreeSet<u64>>>()?;
        Ok(ConnectionScope {
            projects,
            session_id: session.id,
            supervisor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_require_the_origin_owner_and_connection_to_remain_in_scope() {
        let scope = ConnectionScope {
            projects: [1, 2].into(),
            session_id: 10,
            supervisor: true,
        };
        assert!(scope.require_call(11, 1, 2, 2).is_ok());
        for projects in [[3, 1, 1], [1, 3, 1], [1, 1, 3]] {
            assert!(scope
                .require_call(11, projects[0], projects[1], projects[2])
                .is_err());
        }
        let worker = ConnectionScope {
            projects: [1].into(),
            session_id: 10,
            supervisor: false,
        };
        assert!(worker.require_call(10, 1, 1, 1).is_ok());
        assert!(worker.require_call(11, 1, 1, 1).is_err());
        for projects in [[2, 1, 1], [1, 2, 1], [1, 1, 2]] {
            assert!(worker
                .require_call(10, projects[0], projects[1], projects[2])
                .is_err());
        }
    }
}
