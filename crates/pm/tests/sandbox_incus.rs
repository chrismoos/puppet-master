//! Incus-executing sandbox tests, opt-in via `make sandbox-incus-test`.
//! They check the argument mapping against a real daemon, which is the
//! seam a unit test cannot cover. Each test skips cleanly when the opt-in
//! gate or a usable Incus is missing, so the default lane never touches a
//! container.

use std::process::Command;

const IMAGE: &str = "images:ubuntu/26.04";
const INSTANCE: &str = "pm-worker-incus-arg-check";
const VOLUME: &str = "pm-worker-incus-arg-check-home";

fn incus(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("incus").args(args).output()
}

fn ok(args: &[&str]) -> bool {
    incus(args).map(|o| o.status.success()).unwrap_or(false)
}

fn incus_gate() -> bool {
    if std::env::var_os("PM_SANDBOX_INCUS_TESTS").is_none() {
        eprintln!("skipping: PM_SANDBOX_INCUS_TESTS is not set");
        return false;
    }
    // Presence of the binary is not enough: the daemon socket is
    // group-restricted, and a launcher that cannot reach it can do nothing.
    if !ok(&["storage", "list"]) {
        eprintln!("skipping: no reachable incus daemon");
        return false;
    }
    true
}

fn pool() -> String {
    let out = incus(&["profile", "device", "get", "default", "root", "pool"]).unwrap();
    let pool = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if pool.is_empty() {
        "default".to_string()
    } else {
        pool
    }
}

fn cleanup() {
    let _ = incus(&["delete", "--force", INSTANCE]);
    let _ = incus(&["storage", "volume", "delete", &pool(), VOLUME]);
}

/// Every property the launcher sets has to be one Incus accepts, on the
/// version being run against. A rejected key fails the launch after a
/// full provision, so it is worth catching here.
#[test]
fn incus_accepts_the_launcher_properties_and_devices() {
    if !incus_gate() {
        return;
    }
    cleanup();
    let pool = pool();
    let mounts = tempfile::tempdir().unwrap();
    let source = mounts.path().to_str().unwrap().to_string();

    let created = incus(&[
        "create",
        IMAGE,
        INSTANCE,
        "-c",
        "security.nesting=true",
        "-c",
        "security.syscalls.intercept.mknod=true",
        "-c",
        "security.syscalls.intercept.setxattr=true",
        "-c",
        "boot.autostart=false",
        "-c",
        "environment.PM_SANDBOX_INNER=1",
        "-c",
        "limits.cpu=1",
        "-c",
        "limits.memory=512MiB",
        "-c",
        "limits.memory.swap=false",
        "-c",
        "limits.cpu.allowance=50%",
        "-c",
        "limits.memory.enforce=hard",
    ])
    .unwrap();
    assert!(
        created.status.success(),
        "incus rejected the launcher's configuration: {}",
        String::from_utf8_lossy(&created.stderr)
    );

    let volume = incus(&[
        "storage",
        "volume",
        "create",
        &pool,
        VOLUME,
        "security.shifted=false",
    ])
    .unwrap();
    assert!(
        volume.status.success(),
        "incus rejected the home volume: {}",
        String::from_utf8_lossy(&volume.stderr)
    );

    for device in [
        vec![
            "config",
            "device",
            "add",
            INSTANCE,
            "home",
            "disk",
            &format!("pool={pool}"),
            &format!("source={VOLUME}"),
            "path=/home/worker",
        ],
        vec![
            "config",
            "device",
            "add",
            INSTANCE,
            "dir0",
            "disk",
            &format!("source={source}"),
            "path=/srv/repos",
            "shift=true",
            "readonly=true",
        ],
        vec![
            "config",
            "device",
            "add",
            INSTANCE,
            "pm-listen",
            "proxy",
            "listen=tcp:127.0.0.1:47678",
            "connect=tcp:127.0.0.1:47678",
        ],
        vec![
            "config",
            "device",
            "override",
            INSTANCE,
            "root",
            "size=20GiB",
        ],
    ] {
        let out = incus(&device).unwrap();
        assert!(
            out.status.success(),
            "incus rejected {device:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let info = incus(&["info", INSTANCE]).unwrap();
    assert!(info.status.success());
    let info = String::from_utf8_lossy(&info.stdout);
    assert!(
        info.lines().any(|l| l.starts_with("Status:")),
        "the running-state probe reads a Status line: {info}"
    );
    assert!(
        info.contains("STOPPED"),
        "a created-but-unstarted instance is not running: {info}"
    );

    let started = incus(&["start", INSTANCE]).unwrap();
    assert!(
        started.status.success(),
        "starting with an unshifted home failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert!(ok(&[
        "exec",
        INSTANCE,
        "--",
        "sh",
        "-c",
        "printf persistent > /home/worker/shift-check"
    ]));
    assert!(ok(&["stop", INSTANCE, "--force"]));
    assert!(ok(&[
        "storage",
        "volume",
        "set",
        &pool,
        VOLUME,
        "security.shifted=true"
    ]));
    assert!(ok(&[
        "storage",
        "volume",
        "set",
        &pool,
        VOLUME,
        "security.shifted=false"
    ]));
    assert!(ok(&["start", INSTANCE]));
    let contents = incus(&["exec", INSTANCE, "--", "cat", "/home/worker/shift-check"]).unwrap();
    assert!(contents.status.success());
    assert_eq!(String::from_utf8_lossy(&contents.stdout), "persistent");
    cleanup();
}

/// The children probe has to find the worker by name, because systemd is
/// PID 1 in a system container.
#[test]
fn the_terminal_probe_reports_nothing_when_no_worker_runs() {
    if !incus_gate() {
        return;
    }
    let out = incus(&[
        "exec",
        "pm-worker-does-not-exist",
        "--",
        "sh",
        "-c",
        "p=$(pgrep -x pm | head -1); cat /proc/$p/task/$p/children",
    ])
    .unwrap();
    assert!(
        !out.status.success(),
        "an unreachable container must leave the count indeterminate, which refuses to replace"
    );
}
