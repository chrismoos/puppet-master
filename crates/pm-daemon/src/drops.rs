//! Files dropped onto a session from the dashboard, stored on whichever
//! host runs that session so the agent can open them by path.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::daemon::{Daemon, DaemonError};

pub const SESSION_DROP_FILE_MAX: usize = 10 * 1024 * 1024;

/// Longest stored name, counted in bytes of the sanitized form.
const DROP_NAME_MAX: usize = 96;

/// How long a stored drop survives. The path is typed into a composer the
/// moment it is written, but an agent may open it again later in the same
/// conversation, so this outlives a working day rather than a turn.
pub const DROP_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How many names are tried before a drop is refused. Collisions come
/// from dropping the same file twice, not from an adversary.
const DROP_NAME_TRIES: u32 = 64;

const DROP_DIR_PREFIX: &str = "session-";

/// A sanitized drop name: the original reduced to characters that need no
/// quoting in a shell or a composer, with its extension kept because an
/// agent reads the file type from it.
///
/// Returns `None` when nothing usable is left, which is what makes the
/// typed path safe: a name carrying a newline would otherwise submit the
/// composer it was typed into.
pub fn drop_filename(value: &str) -> Option<String> {
    let trimmed = value.trim();
    // The base name comes off before the extension is split, or a dot in
    // a leading `../` is read as the extension separator.
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let (stem, extension) = match base.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => (stem, extension),
        _ => (base, ""),
    };
    let stem = slug(stem);
    let extension = slug(extension);
    let stem = if stem.is_empty() {
        "file".to_owned()
    } else {
        stem
    };
    let extension: String = extension.chars().take(16).collect();
    let keep = DROP_NAME_MAX.saturating_sub(extension.len() + 1);
    let stem: String = stem.chars().take(keep.max(1)).collect();
    if extension.is_empty() {
        Some(stem)
    } else {
        Some(format!("{stem}.{extension}"))
    }
}

/// Characters a path can carry through a composer, a shell word, and a
/// file name on every host a worker runs on.
fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        let keep = character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.');
        let next = if keep { character } else { '-' };
        if next == '-' && out.ends_with('-') {
            continue;
        }
        out.push(next);
    }
    out.trim_matches(['-', '.']).to_owned()
}

fn session_dir(root: &Path, session_id: u64) -> PathBuf {
    root.join(format!("{DROP_DIR_PREFIX}{session_id}"))
}

/// Writes one dropped file and returns the absolute path it took. Runs on
/// the controller for a local session and on the worker for a remote one,
/// so the two hosts cannot disagree about naming or permissions.
pub fn store_drop(
    root: &Path,
    session_id: u64,
    filename: &str,
    content: &[u8],
) -> io::Result<PathBuf> {
    let dir = session_dir(root, session_id);
    crate::fsperm::create_private_dir(&dir)?;
    let (stem, extension) = match filename.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (filename, String::new()),
    };
    for attempt in 0..DROP_NAME_TRIES {
        let candidate = if attempt == 0 {
            dir.join(filename)
        } else {
            dir.join(format!("{stem}-{}{extension}", attempt + 1))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                crate::fsperm::restrict_file(&candidate)?;
                io::Write::write_all(&mut file, content)?;
                io::Write::flush(&mut file)?;
                return std::fs::canonicalize(&candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many dropped files share this name",
    ))
}

/// Removes drop directories nothing has written to within `retention`.
/// Called at startup and before each write, because a worker that runs
/// for weeks never passes through startup again.
pub fn sweep(root: &Path, now: SystemTime, retention: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(DROP_DIR_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age > retention)
            })
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Drops the whole directory for one session, for when it ends.
pub fn remove_session_drops(root: &Path, session_id: u64) {
    let _ = std::fs::remove_dir_all(session_dir(root, session_id));
}

/// What a drop did: where the bytes landed, and why the path did not
/// reach the composer when it did not.
pub struct SessionDrop {
    pub path: String,
    pub typed: bool,
    pub detail: String,
}

impl Daemon {
    /// Stores a dropped file on the host running `session_id` and types
    /// its path into a terminal of that session, as plain bytes and with
    /// no Enter, so it reads as though the viewer typed it.
    pub async fn drop_session_file(
        &self,
        session_id: u64,
        terminal_id: Option<u64>,
        filename: &str,
        content: &[u8],
    ) -> Result<SessionDrop, DaemonError> {
        if content.is_empty() {
            return Err(DaemonError::Rejected("the dropped file is empty".into()));
        }
        if content.len() > SESSION_DROP_FILE_MAX {
            return Err(DaemonError::Rejected(format!(
                "the dropped file exceeds the {SESSION_DROP_FILE_MAX}-byte limit"
            )));
        }
        let filename = drop_filename(filename)
            .ok_or_else(|| DaemonError::Rejected("the dropped file has no usable name".into()))?;
        let session = self.storage().get_session(session_id)?;
        let terminal = match terminal_id {
            Some(id) => {
                let terminal = self.storage().get_terminal(id)?;
                if terminal.session_id != session_id {
                    return Err(DaemonError::Rejected(
                        "that terminal belongs to another session".into(),
                    ));
                }
                terminal
            }
            None => self.storage().agent_terminal(session_id)?,
        };
        let path = if session.worker_id == pm_protocol::domain::LOCAL_WORKER_ID {
            self.store_local_drop(session_id, &filename, content)?
        } else {
            self.store_remote_drop(session.worker_id, session_id, filename, content)
                .await?
        };
        match self
            .type_into_terminal(&terminal, &format!("{path} "))
            .await
        {
            Ok(()) => Ok(SessionDrop {
                path,
                typed: true,
                detail: String::new(),
            }),
            Err(failure) => Ok(SessionDrop {
                path,
                typed: false,
                detail: failure.detail().into(),
            }),
        }
    }

    fn store_local_drop(
        &self,
        session_id: u64,
        filename: &str,
        content: &[u8],
    ) -> Result<String, DaemonError> {
        let root = self.session_drops_dir();
        sweep(root, SystemTime::now(), DROP_RETENTION);
        store_drop(root, session_id, filename, content)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|error| {
                DaemonError::Rejected(format!("cannot store the dropped file: {error}"))
            })
    }

    async fn store_remote_drop(
        &self,
        worker_id: u64,
        session_id: u64,
        filename: String,
        content: &[u8],
    ) -> Result<String, DaemonError> {
        let link = self.worker_link(worker_id)?;
        if link.protocol_version() < pm_protocol::WORKER_PROTOCOL_SESSION_DROP {
            return Err(DaemonError::Rejected(
                "this session's host runs a pm too old to accept dropped files; update it".into(),
            ));
        }
        let result = self
            .worker_session_drop(worker_id, session_id, filename, content.to_vec())
            .await?;
        if !result.ok {
            return Err(DaemonError::Rejected(result.error));
        }
        Ok(result.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_keeps_its_extension_and_loses_everything_unquotable() {
        assert_eq!(drop_filename("Screen Shot.png").unwrap(), "Screen-Shot.png");
        assert_eq!(drop_filename("a'b\"c;d.png").unwrap(), "a-b-c-d.png");
        assert_eq!(drop_filename("notes").unwrap(), "notes");
    }

    #[test]
    fn a_name_cannot_carry_a_newline_into_the_composer() {
        let name = drop_filename("shot\n\rrm -rf.png").unwrap();
        assert!(!name.contains('\n') && !name.contains('\r'), "{name}");
        assert_eq!(name, "shot-rm-rf.png");
    }

    #[test]
    fn a_name_cannot_escape_its_directory() {
        assert_eq!(drop_filename("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(drop_filename("/etc/shadow").unwrap(), "shadow");
        assert_eq!(drop_filename("...").unwrap(), "file");
        assert_eq!(drop_filename("").unwrap(), "file");
    }

    #[test]
    fn a_long_name_stays_within_the_limit_and_keeps_its_extension() {
        let name = drop_filename(&format!("{}.png", "a".repeat(400))).unwrap();
        assert!(name.len() <= DROP_NAME_MAX, "{} bytes", name.len());
        assert!(name.ends_with(".png"), "{name}");
    }

    #[test]
    fn two_drops_of_one_name_get_separate_files() {
        let root = tempfile::tempdir().unwrap();
        let first = store_drop(root.path(), 7, "shot.png", b"one").unwrap();
        let second = store_drop(root.path(), 7, "shot.png", b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
        assert_eq!(second.file_name().unwrap(), "shot-2.png");
    }

    #[test]
    fn a_stored_drop_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = store_drop(root.path(), 1, "shot.png", b"x").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, crate::fsperm::PRIVATE_FILE_MODE);
        let dir = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir, crate::fsperm::PRIVATE_DIR_MODE);
    }

    #[test]
    fn the_sweep_takes_old_sessions_and_leaves_fresh_ones() {
        let root = tempfile::tempdir().unwrap();
        let old = store_drop(root.path(), 1, "a.png", b"x").unwrap();
        let fresh = store_drop(root.path(), 2, "b.png", b"x").unwrap();
        let unrelated = root.path().join("scrollback.bin");
        std::fs::write(&unrelated, b"x").unwrap();
        sweep(
            root.path(),
            SystemTime::now() + Duration::from_secs(60),
            Duration::from_secs(30),
        );
        assert!(!old.exists(), "an old session's drops survived the sweep");
        assert!(!fresh.exists(), "both are older than the retention here");
        assert!(
            unrelated.exists(),
            "the sweep touched a file it does not own"
        );

        let kept = store_drop(root.path(), 3, "c.png", b"x").unwrap();
        sweep(root.path(), SystemTime::now(), DROP_RETENTION);
        assert!(kept.exists(), "a fresh session's drops were swept");
    }

    #[test]
    fn one_session_can_be_removed_without_touching_another() {
        let root = tempfile::tempdir().unwrap();
        let mine = store_drop(root.path(), 1, "a.png", b"x").unwrap();
        let theirs = store_drop(root.path(), 2, "a.png", b"x").unwrap();
        remove_session_drops(root.path(), 1);
        assert!(!mine.exists());
        assert!(theirs.exists());
    }
}
