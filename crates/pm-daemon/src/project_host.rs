//! Whether a project can actually run on one host, and if not, why.
//!
//! A reachable host holding an unusable project path is not the same
//! condition as a host that is off the network, and the two need
//! different fixes: one is a config field, the other is connectivity.

use pm_protocol::domain::PathCheck;

/// Probes a path the way a session's working directory is used: it must
/// exist, be a directory, and be readable by this process.
///
/// Symlinks are followed, so a link to a directory is usable and a
/// dangling one reads as missing.
pub fn check_path(path: &str) -> (PathCheck, String) {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (PathCheck::Missing, String::new()),
        Err(e) => (PathCheck::Unreadable, e.to_string()),
        Ok(meta) if !meta.is_dir() => (PathCheck::NotADirectory, String::new()),
        Ok(_) => match std::fs::read_dir(path) {
            Ok(_) => (PathCheck::Ok, String::new()),
            Err(e) => (PathCheck::Unreadable, e.to_string()),
        },
    }
}

/// How a project stands on one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectHostState {
    /// The host is reachable and the path is a readable directory there.
    Ready {
        path: String,
    },
    /// The host itself cannot be reached.
    HostOffline,
    /// The host is reachable but no path resolves for this project.
    PathUnset,
    PathMissing {
        path: String,
    },
    PathNotADirectory {
        path: String,
    },
    PathUnreadable {
        path: String,
        detail: String,
    },
    /// The host is reachable and holds a configured path, but it did
    /// not say whether that path is usable.
    PathUnchecked {
        path: String,
        reason: String,
    },
}

impl ProjectHostState {
    /// Classifies a resolved path against a host's answer about it.
    pub fn from_check(path: &str, check: PathCheck, detail: String) -> Self {
        let path = path.to_string();
        match check {
            PathCheck::Ok => ProjectHostState::Ready { path },
            PathCheck::Missing => ProjectHostState::PathMissing { path },
            PathCheck::NotADirectory => ProjectHostState::PathNotADirectory { path },
            PathCheck::Unreadable => ProjectHostState::PathUnreadable { path, detail },
        }
    }

    /// Stable slug for the MCP and HTTP surfaces.
    pub fn status(&self) -> &'static str {
        match self {
            ProjectHostState::Ready { .. } => "ready",
            ProjectHostState::HostOffline => "host-offline",
            ProjectHostState::PathUnset => "path-unset",
            ProjectHostState::PathMissing { .. } => "path-missing",
            ProjectHostState::PathNotADirectory { .. } => "path-not-a-directory",
            ProjectHostState::PathUnreadable { .. } => "path-unreadable",
            ProjectHostState::PathUnchecked { .. } => "path-unchecked",
        }
    }

    /// The resolved path this verdict is about, empty when none resolved.
    pub fn path(&self) -> &str {
        match self {
            ProjectHostState::Ready { path }
            | ProjectHostState::PathMissing { path }
            | ProjectHostState::PathNotADirectory { path }
            | ProjectHostState::PathUnreadable { path, .. }
            | ProjectHostState::PathUnchecked { path, .. } => path,
            ProjectHostState::HostOffline | ProjectHostState::PathUnset => "",
        }
    }

    /// Whether a spawn may go ahead. An unchecked path does not block:
    /// the controller has no evidence against it, and refusing would
    /// break every project on a host running an older pm.
    pub fn can_launch(&self) -> bool {
        matches!(
            self,
            ProjectHostState::Ready { .. } | ProjectHostState::PathUnchecked { .. }
        )
    }

    /// One sentence naming the project, the worker, and the path, so the
    /// field to change is clear without opening the config.
    pub fn message(&self, project: &str, host: &str, worker_id: u64) -> String {
        let host = format!("worker {host:?} (id {worker_id})");
        match self {
            ProjectHostState::Ready { path } => {
                format!("project {project:?} runs at {path:?} on {host}")
            }
            ProjectHostState::HostOffline => format!("{host} is offline"),
            ProjectHostState::PathUnset => format!(
                "project {project:?} has no path configured for {host}: set the \
                 project's path, or a path for that worker"
            ),
            ProjectHostState::PathMissing { path } => {
                format!("project {project:?} path {path:?} does not exist on {host}")
            }
            ProjectHostState::PathNotADirectory { path } => {
                format!("project {project:?} path {path:?} on {host} is not a directory")
            }
            ProjectHostState::PathUnreadable { path, detail } => {
                let detail = if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                };
                format!("project {project:?} path {path:?} on {host} is not readable{detail}")
            }
            ProjectHostState::PathUnchecked { path, reason } => format!(
                "project {project:?} path {path:?} on {host} could not be checked: {reason}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_readable_directory_is_usable() {
        let dir = tempfile::tempdir().unwrap();
        let (check, detail) = check_path(dir.path().to_str().unwrap());
        assert_eq!(check, PathCheck::Ok);
        assert!(detail.is_empty());
    }

    #[test]
    fn a_path_that_does_not_exist_reads_as_missing_not_as_a_permission_problem() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("nothing-here");
        let (check, _) = check_path(absent.to_str().unwrap());
        assert_eq!(check, PathCheck::Missing);
    }

    #[test]
    fn a_file_is_distinguished_from_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let (check, _) = check_path(file.to_str().unwrap());
        assert_eq!(check, PathCheck::NotADirectory);
    }

    #[test]
    fn an_unreadable_directory_reports_the_operating_system_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let closed = dir.path().join("closed");
        std::fs::create_dir(&closed).unwrap();
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (check, detail) = check_path(closed.to_str().unwrap());
        // Root ignores the mode, so the check honestly reports it usable.
        if check == PathCheck::Ok {
            return;
        }
        assert_eq!(check, PathCheck::Unreadable);
        assert!(!detail.is_empty(), "the OS error is what makes it fixable");
    }

    #[test]
    fn a_dangling_symlink_reads_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path().join("gone"), &link).unwrap();
        let (check, _) = check_path(link.to_str().unwrap());
        assert_eq!(check, PathCheck::Missing);
    }

    #[test]
    fn every_message_names_the_project_the_host_and_the_path() {
        let cases = [
            ProjectHostState::PathUnset,
            ProjectHostState::PathMissing {
                path: "/srv/acme".into(),
            },
            ProjectHostState::PathNotADirectory {
                path: "/srv/acme".into(),
            },
            ProjectHostState::PathUnreadable {
                path: "/srv/acme".into(),
                detail: "Permission denied (os error 13)".into(),
            },
        ];
        for state in cases {
            let message = state.message("acme", "lima", 1);
            assert!(message.contains("acme"), "{message}");
            assert!(message.contains("lima"), "{message}");
            assert!(message.contains("id 1"), "{message}");
            assert!(
                !message.contains("offline"),
                "a reachable host must not read as offline: {message}"
            );
            if !matches!(state, ProjectHostState::PathUnset) {
                assert!(message.contains("/srv/acme"), "{message}");
            }
        }
    }

    #[test]
    fn only_an_offline_host_is_described_as_offline() {
        assert!(ProjectHostState::HostOffline
            .message("acme", "lima", 1)
            .contains("offline"));
    }

    #[test]
    fn an_unchecked_path_does_not_block_a_spawn() {
        assert!(ProjectHostState::PathUnchecked {
            path: "/srv/acme".into(),
            reason: "that host runs a pm too old to answer".into(),
        }
        .can_launch());
        assert!(ProjectHostState::Ready {
            path: "/srv/acme".into()
        }
        .can_launch());
        assert!(!ProjectHostState::PathUnset.can_launch());
        assert!(!ProjectHostState::HostOffline.can_launch());
    }
}
