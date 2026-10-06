//! Container-executing sandbox tests, opt-in via
//! `make sandbox-container-test`. Each test skips cleanly when the
//! opt-in gate, a container runtime, or the local image is missing, so
//! the default lane never touches a container.

use std::process::Command;

const IMAGE: &str = "puppet-master-worker:local";
const CONTAINER_BIN: &str = "/opt/pm/bin/pm";

fn runtime() -> Option<&'static str> {
    ["docker", "podman"].into_iter().find(|r| {
        Command::new(r)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn container_gate() -> Option<&'static str> {
    if std::env::var_os("PM_SANDBOX_CONTAINER_TESTS").is_none() {
        eprintln!("skipping: PM_SANDBOX_CONTAINER_TESTS is not set");
        return None;
    }
    let Some(rt) = runtime() else {
        eprintln!("skipping: no container runtime available");
        return None;
    };
    let have_image = Command::new(rt)
        .args(["image", "inspect", IMAGE])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_image {
        eprintln!("skipping: image {IMAGE} is missing; build it with `make sandbox-image`");
        return None;
    }
    Some(rt)
}

#[test]
fn image_reports_the_launcher_version() {
    let Some(rt) = container_gate() else {
        return;
    };
    let host = Command::new(env!("CARGO_BIN_EXE_pm"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(host.status.success());
    let inner = Command::new(rt)
        .args(["run", "--rm", IMAGE, CONTAINER_BIN, "--version"])
        .output()
        .unwrap();
    assert!(
        inner.status.success(),
        "pm failed to run inside {IMAGE}: {}",
        String::from_utf8_lossy(&inner.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&host.stdout).trim(),
        String::from_utf8_lossy(&inner.stdout).trim()
    );
}

#[test]
fn inner_worker_rejects_re_sandbox() {
    let exe = env!("CARGO_BIN_EXE_pm");
    let out = Command::new(exe)
        .env("PM_SANDBOX_INNER", "1")
        .args(["worker", "--sandbox"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--sandbox cannot nest"),
        "unexpected stderr: {stderr}"
    );
}
