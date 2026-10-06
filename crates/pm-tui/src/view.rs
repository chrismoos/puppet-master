//! Pure view-model over the reducer state: row ordering, labels, and
//! time formatting. Rendering stays a thin map over these.

use pm_protocol::domain::{Session, SessionRole, SessionState};

use crate::app::World;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    NeedsInputHeader,
    Bucket(u64),
    Supervisors,
    Project(u64),
    Session { id: u64, pinned: bool },
}

/// Needs-input sessions are pinned in their own section at the top and
/// omitted from the tree below, so every session occupies exactly one
/// selectable row.
pub fn rows(world: &World) -> Vec<Row> {
    let mut out = Vec::new();
    let pinned: Vec<u64> = world
        .sessions
        .iter()
        .filter(|s| s.state == SessionState::NeedsInput)
        .map(|s| s.id)
        .collect();
    if !pinned.is_empty() {
        out.push(Row::NeedsInputHeader);
        out.extend(pinned.iter().map(|&id| Row::Session { id, pinned: true }));
    }
    for b in &world.buckets {
        out.push(Row::Bucket(b.id));
        let supervisors: Vec<_> = world
            .sessions
            .iter()
            .filter(|s| {
                s.role == SessionRole::Supervisor
                    && s.state != SessionState::NeedsInput
                    && world
                        .project(s.project_id)
                        .is_some_and(|p| p.bucket_id == b.id)
            })
            .collect();
        if !supervisors.is_empty() {
            out.push(Row::Supervisors);
            out.extend(supervisors.into_iter().map(|s| Row::Session {
                id: s.id,
                pinned: false,
            }));
        }
        for p in world.projects.iter().filter(|p| p.bucket_id == b.id) {
            out.push(Row::Project(p.id));
            for s in world.sessions.iter().filter(|s| {
                s.role == SessionRole::Worker
                    && s.project_id == p.id
                    && s.state != SessionState::NeedsInput
            }) {
                out.push(Row::Session {
                    id: s.id,
                    pinned: false,
                });
            }
        }
    }
    out
}

/// Session ids in visual row order; the selection cursor moves over
/// this list.
pub fn session_row_ids(world: &World) -> Vec<u64> {
    rows(world)
        .into_iter()
        .filter_map(|row| match row {
            Row::Session { id, .. } => Some(id),
            Row::NeedsInputHeader | Row::Bucket(_) | Row::Supervisors | Row::Project(_) => None,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectChoice {
    pub id: u64,
    pub label: String,
    pub worker_id: u64,
}

pub fn project_choices(world: &World) -> Vec<ProjectChoice> {
    let mut out = Vec::new();
    for b in &world.buckets {
        for p in world.projects.iter().filter(|p| p.bucket_id == b.id) {
            out.push(ProjectChoice {
                id: p.id,
                label: format!("{}/{}", b.name, p.name),
                worker_id: p.worker_id.unwrap_or(b.default_worker_id),
            });
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerChoice {
    pub id: u64,
    pub label: String,
}

pub fn worker_choices(world: &World) -> Vec<WorkerChoice> {
    let mut workers: Vec<_> = world.workers.iter().collect();
    workers.sort_by_key(|worker| (worker.id != pm_protocol::domain::LOCAL_WORKER_ID, worker.id));
    workers
        .into_iter()
        .map(|worker| WorkerChoice {
            id: worker.id,
            label: format!(
                "{}{}{}",
                worker.name,
                if worker.id == pm_protocol::domain::LOCAL_WORKER_ID {
                    " (local)"
                } else {
                    ""
                },
                if worker.online { "" } else { " (offline)" }
            ),
        })
        .collect()
}

pub fn project_label(world: &World, project_id: u64) -> String {
    match world.project(project_id) {
        Some(p) => match world.bucket(p.bucket_id) {
            Some(b) => format!("{}/{}", b.name, p.name),
            None => p.name.clone(),
        },
        None => String::new(),
    }
}

/// Live sessions measure against now, ended ones against their end
/// time, so finished sessions stop ticking.
/// Time since the session last did something, a single coarse unit,
/// e.g. "10s ago" or "2d ago".
pub fn last_active_ago(session: &Session, now_ms: i64) -> String {
    let secs = (now_ms - session.last_activity_at_unix_ms).max(0) / 1000;
    let unit = if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    };
    format!("{unit} ago")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, World};
    use crate::testutil::{bucket, project, session, snapshot};

    fn world() -> World {
        let mut app = App::new();
        app.apply_snapshot(snapshot(
            vec![bucket(2, "oss"), bucket(1, "work")],
            vec![project(1, 1, "api"), project(2, 2, "cli")],
            vec![
                session(1, 1, SessionState::Working),
                session(2, 2, SessionState::NeedsInput),
                session(3, 1, SessionState::Idle),
            ],
        ));
        app.world
    }

    #[test]
    fn needs_input_sessions_are_pinned_first_and_not_duplicated() {
        let world = world();
        assert_eq!(
            rows(&world),
            vec![
                Row::NeedsInputHeader,
                Row::Session {
                    id: 2,
                    pinned: true
                },
                Row::Bucket(1),
                Row::Project(1),
                Row::Session {
                    id: 1,
                    pinned: false
                },
                Row::Session {
                    id: 3,
                    pinned: false
                },
                Row::Bucket(2),
                Row::Project(2),
            ]
        );
        assert_eq!(session_row_ids(&world), vec![2, 1, 3]);
    }

    #[test]
    fn no_pinned_section_without_needs_input() {
        let mut app = App::new();
        app.apply_snapshot(snapshot(
            vec![bucket(1, "work")],
            vec![project(1, 1, "api")],
            vec![session(1, 1, SessionState::Working)],
        ));
        assert_eq!(
            rows(&app.world),
            vec![
                Row::Bucket(1),
                Row::Project(1),
                Row::Session {
                    id: 1,
                    pinned: false
                },
            ]
        );
    }

    #[test]
    fn buckets_order_by_position() {
        let world = world();
        let bucket_rows: Vec<u64> = rows(&world)
            .into_iter()
            .filter_map(|r| match r {
                Row::Bucket(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(bucket_rows, vec![1, 2]);
    }

    #[test]
    fn project_choices_carry_bucket_labels() {
        let world = world();
        let choices = project_choices(&world);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].label, "work/api");
        assert_eq!(choices[1].label, "oss/cli");
    }

    #[test]
    fn project_label_falls_back_gracefully() {
        let world = world();
        assert_eq!(project_label(&world, 1), "work/api");
        assert_eq!(project_label(&world, 99), "");
    }

    #[test]
    fn last_active_ago_uses_one_coarse_unit() {
        let mut s = session(1, 1, SessionState::Working);
        s.last_activity_at_unix_ms = 100_000;
        assert_eq!(last_active_ago(&s, 110_000), "10s ago");
        assert_eq!(last_active_ago(&s, 100_000 + 5 * 60_000), "5m ago");
        assert_eq!(last_active_ago(&s, 100_000 + 3 * 3_600_000), "3h ago");
        assert_eq!(last_active_ago(&s, 100_000 + 2 * 86_400_000), "2d ago");
        // A clock skew into the past never shows a negative age.
        assert_eq!(last_active_ago(&s, 0), "0s ago");
    }
}
