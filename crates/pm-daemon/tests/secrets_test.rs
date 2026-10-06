//! The at-rest sealing secret lives beside the database rather than in
//! it, so reading the database is not by itself enough to open the
//! credentials it holds.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use pm_daemon::storage::Storage;

const INSTALLATION_SECRET_KEY: &str = "installation.secret";

struct Layout {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    secret: PathBuf,
    scrollback: PathBuf,
    socket: PathBuf,
}

fn layout() -> Layout {
    let tmp = tempfile::tempdir().unwrap();
    Layout {
        db: tmp.path().join("pm.db"),
        secret: tmp.path().join("pm.db.secret"),
        scrollback: tmp.path().join("scrollback"),
        socket: tmp.path().join("unused.sock"),
        _tmp: tmp,
    }
}

/// Starts a daemon on this layout and keeps the handle, for tests that ask
/// it something rather than only inspecting what it wrote.
fn daemon(layout: &Layout) -> pm_daemon::Daemon {
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        db_path: Some(layout.db.clone()),
        socket_path: layout.socket.clone(),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: layout.scrollback.clone(),
        registry: support::test_registry(),
        local_worker_enabled: true,
        release_channel: None,
        forward: Default::default(),
    };
    pm_daemon::Daemon::new(config).unwrap().0
}

/// A controller's identity is what every enrolled host pins, so it has to
/// hold still. A database carried away from its secret file cannot open the
/// key it stores, and minting one per call would present a different
/// controller to every host on every connection.
#[test]
fn a_worker_key_that_cannot_be_opened_is_replaced_once_and_then_holds() {
    let layout = layout();
    let controller = daemon(&layout);
    let original = controller.worker_identity().unwrap().key_hash();
    assert_eq!(controller.worker_identity().unwrap().key_hash(), original);
    drop(controller);

    std::fs::remove_file(&layout.secret).unwrap();
    let controller = daemon(&layout);
    let replaced = controller.worker_identity().unwrap().key_hash();
    assert_ne!(
        replaced, original,
        "a key sealed with a lost secret cannot be recovered"
    );
    assert_eq!(
        controller.worker_identity().unwrap().key_hash(),
        replaced,
        "the replacement is stored, not minted again per call"
    );
    drop(controller);

    let controller = daemon(&layout);
    assert_eq!(
        controller.worker_identity().unwrap().key_hash(),
        replaced,
        "and it survives a restart, so a host can pin it"
    );
}

fn start(layout: &Layout) {
    let config = pm_daemon::DaemonConfig {
        stale_turn_quiet_ms: None,
        hook_silence_grace_ms: None,
        db_path: Some(layout.db.clone()),
        socket_path: layout.socket.clone(),
        http_addr: None,
        http_tls: None,
        worker_addr: None,
        public_url: None,
        scrollback_dir: layout.scrollback.clone(),
        registry: support::test_registry(),
        local_worker_enabled: true,
        release_channel: None,
        forward: Default::default(),
    };
    let (_daemon, _channels) = pm_daemon::Daemon::new(config).unwrap();
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn stored_secret(db: &Path) -> Option<String> {
    Storage::open(db)
        .unwrap()
        .get_setting(INSTALLATION_SECRET_KEY)
        .unwrap()
}

#[test]
fn a_new_installation_keeps_its_sealing_secret_out_of_the_database() {
    let layout = layout();
    start(&layout);

    let secret = std::fs::read_to_string(&layout.secret).unwrap();
    assert!(!secret.trim().is_empty(), "secret file should hold a value");
    assert_eq!(mode_of(&layout.secret), 0o600);
    assert_eq!(
        stored_secret(&layout.db),
        None,
        "the database must not carry the secret that opens what it stores"
    );
}

/// The secret must survive a restart unchanged, or every credential
/// already sealed under it stops opening.
#[test]
fn the_secret_is_generated_once_and_reused() {
    let layout = layout();
    start(&layout);
    let first = std::fs::read_to_string(&layout.secret).unwrap();
    start(&layout);
    let second = std::fs::read_to_string(&layout.secret).unwrap();
    assert_eq!(first, second);
}

/// Installations created before the secret moved out still have it in
/// their settings table. Carrying the same value across is what keeps
/// their stored API keys and push tokens readable.
#[test]
fn a_secret_left_in_the_database_moves_to_the_file_intact() {
    let layout = layout();
    let existing = "5ff6e2a1c0de4b7a9d3f8e1b2c4a6d8f0e1a3b5c7d9f1e3a5b7c9d1f3e5a7b9c";
    Storage::open(&layout.db)
        .unwrap()
        .ensure_setting(INSTALLATION_SECRET_KEY, existing)
        .unwrap();

    start(&layout);

    assert_eq!(
        std::fs::read_to_string(&layout.secret).unwrap().trim(),
        existing,
        "the moved secret must keep its value"
    );
    assert_eq!(mode_of(&layout.secret), 0o600);
    assert_eq!(
        stored_secret(&layout.db),
        None,
        "the database copy should be dropped once the file is durable"
    );
}

#[test]
fn the_database_and_scrollback_are_owner_only() {
    let layout = layout();
    start(&layout);

    assert_eq!(mode_of(&layout.db), 0o600);
    assert_eq!(mode_of(&layout.scrollback), 0o700);
    assert_eq!(mode_of(&layout.scrollback.join("session-files")), 0o700);
}

/// Constructing a daemon must not require a Tokio reactor. Plenty of
/// synchronous tests build one, and background work that needs a runtime
/// belongs to the async entry point rather than the constructor.
#[test]
fn a_daemon_is_constructible_without_a_reactor() {
    let layout = layout();
    start(&layout);
}

/// Whether the bytes are findable in the files an operator would copy, which
/// is the whole question: the documented promise is that a copy of the
/// database alone does not yield the secret.
fn secret_is_legible_on_disk(db: &Path, secret: &str) -> Vec<String> {
    let mut found = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", db.display()));
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes
            .windows(secret.len())
            .any(|window| window == secret.as_bytes())
        {
            found.push(if suffix.is_empty() {
                "pm.db".to_string()
            } else {
                format!("pm.db{suffix}")
            });
        }
    }
    found
}

/// Moving the secret to its own file drops its row, and dropping a row does
/// not erase it: SQLite frees the space and, with secure_delete off as it is
/// in the bundled build, leaves the bytes there. The settings table is small
/// and rarely rewritten, so nothing overwrites them on its own and the
/// documented promise stays false for that installation until the database is
/// rebuilt.
#[test]
fn a_migrated_installation_stops_carrying_the_secret_it_moved_out() {
    let layout = layout();
    let existing = "5ff6e2a1c0de4b7a9d3f8e1b2c4a6d8f0e1a3b5c7d9f1e3a5b7c9d1f3e5a7b9c";
    {
        let storage = Storage::open(&layout.db).unwrap();
        storage
            .ensure_setting(INSTALLATION_SECRET_KEY, existing)
            .unwrap();
        // Other settings beside it, so dropping its row does not empty the
        // page and hand it back to be written over by accident.
        for i in 0..40 {
            storage
                .ensure_setting(&format!("probe.setting.{i}"), &"x".repeat(60))
                .unwrap();
        }
    }
    assert_eq!(
        secret_is_legible_on_disk(&layout.db, existing),
        vec!["pm.db".to_string()],
        "the fixture should start with the secret in the database"
    );

    // Held open, because that is when a database gets copied: the operator
    // attaches it to a bug report from a controller that is still running. A
    // closed connection checkpoints on its way out and would hide whether the
    // rebuild reached the file.
    let running = daemon(&layout);

    assert_eq!(
        std::fs::read_to_string(&layout.secret).unwrap().trim(),
        existing,
        "the secret must still open what was sealed under it"
    );
    let still_there = secret_is_legible_on_disk(&layout.db, existing);
    assert!(
        still_there.is_empty(),
        "a copy of {still_there:?} taken from a running controller would still \
         yield the sealing secret"
    );
    drop(running);
}

/// Rebuilding a database costs time in proportion to its size, so it happens
/// once rather than on every start.
#[test]
fn the_rebuild_does_not_happen_again_on_the_next_start() {
    let layout = layout();
    let existing = "5ff6e2a1c0de4b7a9d3f8e1b2c4a6d8f0e1a3b5c7d9f1e3a5b7c9d1f3e5a7b9c";
    Storage::open(&layout.db)
        .unwrap()
        .ensure_setting(INSTALLATION_SECRET_KEY, existing)
        .unwrap();
    start(&layout);
    assert_eq!(
        Storage::open(&layout.db)
            .unwrap()
            .get_setting("installation.secret_pages_pending")
            .unwrap(),
        None,
        "a completed rebuild must not leave itself marked as pending"
    );

    // A fresh installation never marks it at all, so it never rebuilds.
    let fresh = self::layout();
    start(&fresh);
    assert_eq!(
        Storage::open(&fresh.db)
            .unwrap()
            .get_setting("installation.secret_pages_pending")
            .unwrap(),
        None
    );
}
