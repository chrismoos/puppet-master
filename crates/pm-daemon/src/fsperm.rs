//! Owner-only permissions for the state the daemon and the worker write.
//! The socket
//! directory is locked down where it is bound; the database, the sealing
//! secret, and the scrollback hold credentials and terminal output, so
//! they get the same treatment.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub const PRIVATE_DIR_MODE: u32 = 0o700;
pub const PRIVATE_FILE_MODE: u32 = 0o600;

/// Creates `path` and any missing parent, then restricts it to its owner.
/// An existing directory is restricted too, so an installation made
/// before this got the same protection as a fresh one.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE))
}

pub fn restrict_file(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))
}

/// Narrows a SQLite database and the two files it keeps beside it.
///
/// SQLite creates the write-ahead log and the shared-memory file itself, with
/// default permissions, and the log holds rows recently written: session token
/// hashes, the password hash, sealed provider keys. The owner-only parent
/// directory is the control that makes them unreachable, so these are narrowed
/// against the directory later being widened or the state being copied out by
/// something that preserves modes. They may not exist yet, which is not an
/// error.
pub fn restrict_database(db_path: &Path) -> io::Result<()> {
    restrict_file(db_path)?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = db_path.as_os_str().to_owned();
        sidecar.push(suffix);
        match restrict_file(Path::new(&sidecar)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_created_directory_is_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("state").join("scrollback");
        create_private_dir(&nested).unwrap();
        assert_eq!(mode_of(&nested), PRIVATE_DIR_MODE);
    }

    /// Installations predate this, so a directory that already exists with
    /// wider permissions has to be narrowed rather than left alone.
    #[test]
    fn an_existing_world_readable_directory_is_narrowed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("state");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&dir).unwrap();
        assert_eq!(mode_of(&dir), PRIVATE_DIR_MODE);
    }

    /// SQLite makes these itself, so nothing narrows them unless this does,
    /// and the write-ahead log holds rows as sensitive as the database's.
    #[test]
    fn a_database_narrows_its_write_ahead_log_and_shared_memory() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("pm.db");
        for path in [
            &db,
            &root.path().join("pm.db-wal"),
            &root.path().join("pm.db-shm"),
        ] {
            std::fs::write(path, b"x").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        restrict_database(&db).unwrap();
        assert_eq!(mode_of(&db), PRIVATE_FILE_MODE);
        assert_eq!(mode_of(&root.path().join("pm.db-wal")), PRIVATE_FILE_MODE);
        assert_eq!(mode_of(&root.path().join("pm.db-shm")), PRIVATE_FILE_MODE);
    }

    /// A fresh database has neither sidecar yet, and that is not a failure.
    #[test]
    fn a_database_without_sidecars_is_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("pm.db");
        std::fs::write(&db, b"x").unwrap();
        restrict_database(&db).unwrap();
        assert_eq!(mode_of(&db), PRIVATE_FILE_MODE);
    }

    #[test]
    fn a_restricted_file_is_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("pm.db");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        restrict_file(&file).unwrap();
        assert_eq!(mode_of(&file), PRIVATE_FILE_MODE);
    }
}
