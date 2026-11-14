//! The notify_user tool: an agent tells the user something they asked to
//! hear about, through the same alert and push paths as a state change.

use pm_protocol::domain::{
    AgentNotification, AgentNotificationLink, AgentNotificationLinkKind, SessionAlert,
    SessionAlertKind,
};
use rusqlite::{params, OptionalExtension};
use tracing::info;

use crate::daemon::{Daemon, DaemonError};
use crate::storage::now_unix_ms;

/// Ids are never reused, because a push delivery is deduplicated by an event id built from one.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agent_notifications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_notifications_session
    ON agent_notifications(session_id, created_at_unix_ms);";

/// The shortest gap between two notifications from one session.
pub const NOTIFY_MIN_INTERVAL_MS: i64 = 10_000;
pub const TITLE_MAX_CHARS: usize = 80;
pub const BODY_MAX_CHARS: usize = 300;
/// Messages are kept only long enough for their push deliveries to settle.
const MESSAGE_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MS_PER_SECOND: i64 = 1000;

/// A message delivery's push event id is this prefix and the message id.
pub(crate) const AGENT_NOTIFICATION_EVENT_PREFIX: &str = "agent-notification:";

const ITEM_REF_PREFIX: &str = "pm:item/";

#[derive(Debug, PartialEq, Eq)]
pub enum RecordRefusal {
    /// Carries the ms left before the session may send again.
    TooSoon(i64),
}

impl crate::storage::Storage {
    /// Records a message unless the session sent one within `min_interval_ms`,
    /// checking and inserting under one lock so two calls cannot both pass.
    pub fn record_agent_notification(
        &self,
        session_id: u64,
        title: &str,
        body: &str,
        now: i64,
        min_interval_ms: i64,
    ) -> rusqlite::Result<Result<u64, RecordRefusal>> {
        let conn = self.conn.lock().unwrap();
        let last: Option<i64> = conn.query_row(
            "SELECT MAX(created_at_unix_ms) FROM agent_notifications WHERE session_id = ?1",
            params![session_id as i64],
            |row| row.get(0),
        )?;
        if let Some(last) = last {
            let elapsed = now - last;
            if elapsed < min_interval_ms {
                return Ok(Err(RecordRefusal::TooSoon(min_interval_ms - elapsed)));
            }
        }
        conn.execute(
            "DELETE FROM agent_notifications WHERE created_at_unix_ms < ?1",
            params![now - MESSAGE_RETENTION_MS],
        )?;
        conn.execute(
            "INSERT INTO agent_notifications (session_id, title, body, created_at_unix_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id as i64, title, body, now],
        )?;
        Ok(Ok(conn.last_insert_rowid() as u64))
    }

    pub fn agent_notification(&self, id: u64) -> rusqlite::Result<Option<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT title, body FROM agent_notifications WHERE id = ?1",
            params![id as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
    }
}

/// Refuses text over `max` chars rather than cutting it, so the agent learns its message was too long.
fn bounded(field: &str, value: &str, max: usize) -> Result<String, DaemonError> {
    let value = crate::text::unescape_html_entities(value.trim());
    let count = value.chars().count();
    if count > max {
        return Err(DaemonError::Rejected(format!(
            "{field} is {count} characters; keep it to {max}"
        )));
    }
    Ok(value)
}

fn same_url(a: &str, b: &str) -> bool {
    !a.is_empty() && a.trim_end_matches('/') == b.trim_end_matches('/')
}

impl Daemon {
    /// Tells the user something through an alert and a push, at most once
    /// per session every [`NOTIFY_MIN_INTERVAL_MS`].
    pub fn notify_user(
        &self,
        session_token: &str,
        title: &str,
        body: &str,
        link: &str,
    ) -> Result<AgentNotification, DaemonError> {
        let session_id = self.resolve_live_session(session_token)?;
        let title = bounded("title", title, TITLE_MAX_CHARS)?;
        if title.is_empty() {
            return Err(DaemonError::Rejected(
                "notify_user needs a title: say in a few words what the user should know".into(),
            ));
        }
        let body = bounded("body", body, BODY_MAX_CHARS)?;
        let link = match link.trim() {
            "" => None,
            link => Some(self.resolve_message_link(session_id, link)?),
        };
        let message_id = match self
            .storage()
            .record_agent_notification(
                session_id,
                &title,
                &body,
                now_unix_ms(),
                NOTIFY_MIN_INTERVAL_MS,
            )
            .map_err(crate::storage::StorageError::from)?
        {
            Ok(id) => id,
            Err(RecordRefusal::TooSoon(remaining_ms)) => {
                let seconds = (remaining_ms + MS_PER_SECOND - 1) / MS_PER_SECOND;
                return Err(DaemonError::Rejected(format!(
                    "notify_user allows one notification every {} seconds per session; \
                     nothing was sent. Wait {seconds}s, and send it then only if it is \
                     still worth interrupting the user for",
                    NOTIFY_MIN_INTERVAL_MS / MS_PER_SECOND
                )));
            }
        };
        let message = AgentNotification { title, body, link };
        info!(
            session = session_id,
            message = message_id,
            link = message
                .link
                .as_ref()
                .map(|l| l.kind.as_str())
                .unwrap_or("none"),
            "agent notified the user"
        );
        self.send_session_alert(SessionAlert {
            session_id,
            kind: SessionAlertKind::Message,
            notification: Some(message.clone()),
        });
        self.queue_agent_notification_push(session_id, message_id);
        Ok(message)
    }

    /// Resolves an agent-supplied link to something this controller serves:
    /// a forward the session published, by URL or slug, or a board item in
    /// the session's bucket.
    fn resolve_message_link(
        &self,
        session_id: u64,
        link: &str,
    ) -> Result<AgentNotificationLink, DaemonError> {
        if link.starts_with(ITEM_REF_PREFIX) {
            let session = self.storage().get_session(session_id)?;
            let bucket_id = self.storage().get_project(session.project_id)?.bucket_id;
            let id = self.resolve_item_ref(bucket_id, &serde_json::Value::from(link))?;
            let item = self.storage().get_item(bucket_id, id)?;
            return Ok(AgentNotificationLink {
                kind: AgentNotificationLinkKind::Item,
                title: item.title,
                url: format!("/bucket/{bucket_id}/item/{id}"),
            });
        }
        for forward in self.storage().list_session_forwards(session_id)? {
            let view = self.forward_view(forward);
            if view.slug != link && !same_url(&view.url, link) {
                continue;
            }
            if view.url.is_empty() {
                return Err(DaemonError::Rejected(format!(
                    "{link} is not being served right now, so there is nothing to open"
                )));
            }
            let kind = if view.source_path.is_empty() {
                AgentNotificationLinkKind::Preview
            } else {
                AgentNotificationLinkKind::Page
            };
            let title = if view.label.trim().is_empty() {
                view.slug.clone()
            } else {
                view.label.trim().to_string()
            };
            return Ok(AgentNotificationLink {
                kind,
                title,
                url: view.url,
            });
        }
        Err(DaemonError::Rejected(format!(
            "link {link:?} is not something this session can link to: use the URL or slug of \
             a directory or port this session published, or a board item reference like \
             pm:item/<bucket-id>/<item-number>"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use pm_protocol::domain::{AgentKind, PermissionMode};

    fn storage_with_session() -> (Storage, u64) {
        let storage = Storage::open_in_memory().unwrap();
        let bucket = storage.create_bucket("notify").unwrap();
        let project = storage
            .create_project(bucket.id, "project", "/tmp/notify-test")
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
        (storage, session.id)
    }

    #[test]
    fn a_second_message_inside_the_interval_is_refused_with_the_time_left() {
        let (storage, session) = storage_with_session();
        let first = storage
            .record_agent_notification(session, "Tests passed", "", 1_000, NOTIFY_MIN_INTERVAL_MS)
            .unwrap();
        assert!(first.is_ok());
        let second = storage
            .record_agent_notification(session, "Again", "", 4_000, NOTIFY_MIN_INTERVAL_MS)
            .unwrap();
        assert_eq!(second, Err(RecordRefusal::TooSoon(7_000)));
        let later = storage
            .record_agent_notification(session, "Later", "", 11_000, NOTIFY_MIN_INTERVAL_MS)
            .unwrap();
        assert!(later.is_ok());
    }

    #[test]
    fn a_recorded_message_reads_back_and_old_ones_are_pruned() {
        let (storage, session) = storage_with_session();
        let old = storage
            .record_agent_notification(session, "Old", "", 0, NOTIFY_MIN_INTERVAL_MS)
            .unwrap()
            .unwrap();
        let now = MESSAGE_RETENTION_MS + 1;
        let fresh = storage
            .record_agent_notification(session, "Fresh", "body", now, NOTIFY_MIN_INTERVAL_MS)
            .unwrap()
            .unwrap();
        assert_eq!(storage.agent_notification(old).unwrap(), None);
        assert_eq!(
            storage.agent_notification(fresh).unwrap(),
            Some(("Fresh".to_string(), "body".to_string()))
        );
    }

    #[test]
    fn text_over_the_limit_is_refused_rather_than_cut() {
        assert_eq!(bounded("title", "  ok  ", TITLE_MAX_CHARS).unwrap(), "ok");
        let long = "x".repeat(TITLE_MAX_CHARS + 1);
        let error = bounded("title", &long, TITLE_MAX_CHARS).unwrap_err();
        assert!(error.to_string().contains("keep it to 80"), "{error}");
    }

    #[test]
    fn urls_match_with_or_without_a_trailing_slash() {
        assert!(same_url("https://a.example/", "https://a.example"));
        assert!(!same_url("", ""));
        assert!(!same_url("https://a.example/", "https://b.example/"));
    }
}
