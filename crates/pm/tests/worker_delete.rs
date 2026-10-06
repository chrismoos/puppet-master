#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fixture {
    root: tempfile::TempDir,
    configs: Vec<PathBuf>,
}

impl Fixture {
    fn new(fail_cleanup: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let configs = [
            root.path().join("config/puppet-master/worker.toml"),
            root.path()
                .join("Library/Application Support/puppet-master/worker.toml"),
        ]
        .to_vec();
        for config in &configs {
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(config, "[profiles.build.sandbox]\nruntime = \"docker\"\n").unwrap();
        }
        let script = root.path().join("docker");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$DELETE_TEST_LOG\"\ncase \"$1 $2\" in\n 'inspect --format') printf 'false\\n' ;;\n 'volume ls') printf 'pm-worker-build-home\\n' ;;\n 'rm --force') exit {} ;;\nesac\n",
                if fail_cleanup { "1" } else { "0" }
            ),
        )
        .unwrap();
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, configs }
    }

    fn run(&self, flags: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pm"))
            .args(["worker", "delete", "--name", "build"])
            .args(flags)
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_RUNTIME_DIR", self.root.path())
            .env("PM_SOCKET", self.root.path().join("unused.sock"))
            .env("PATH", self.root.path())
            .env("DELETE_TEST_LOG", self.root.path().join("calls"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn profile_exists(&self) -> bool {
        self.configs.iter().all(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .contains("profiles.build")
        })
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.root.path().join("calls")).unwrap_or_default()
    }
}

#[test]
fn headless_deletion_without_acceptance_keeps_configuration_and_resources() {
    let fixture = Fixture::new(false);
    let output = fixture.run(&[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--accept-delete"));
    assert!(fixture.profile_exists());
    assert!(fixture.calls().is_empty());
}

#[test]
fn accepted_deletion_removes_the_container_volume_and_local_profile() {
    let fixture = Fixture::new(false);
    let output = fixture.run(&["--accept-delete"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.profile_exists());
    let calls = fixture.calls();
    assert!(calls
        .lines()
        .any(|line| line == "rm --force pm-worker-build"));
    assert!(calls
        .lines()
        .any(|line| line == "volume rm pm-worker-build-home"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("deleted worker build"));
}

#[test]
fn keeping_resources_only_removes_the_local_profile() {
    let fixture = Fixture::new(false);
    let output = fixture.run(&["--accept-delete", "--no-delete-resources"]);
    assert!(output.status.success());
    assert!(!fixture.profile_exists());
    assert!(fixture.calls().is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("were kept"));
}

#[test]
fn cleanup_failure_keeps_the_profile_for_retry() {
    let fixture = Fixture::new(true);
    let output = fixture.run(&["--accept-delete"]);
    assert!(!output.status.success());
    assert!(fixture.profile_exists());
    assert!(!fixture.calls().contains("volume rm"));
}

#[test]
fn incus_deletion_uses_the_attached_pool_and_removes_the_home_volume() {
    let fixture = Fixture::new(false);
    for config in &fixture.configs {
        std::fs::write(config, "[profiles.build.sandbox]\nruntime = \"incus\"\n").unwrap();
    }
    let script = fixture.root.path().join("incus");
    std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$DELETE_TEST_LOG\"\ncase \"$1 $2\" in\n 'info pm-worker-build') printf 'Status: STOPPED\\n' ;;\n 'config device') printf 'attached-pool\\n' ;;\n 'storage volume') if [ \"$3\" = list ]; then printf 'pm-worker-build-home\\n'; fi ;;\nesac\n").unwrap();
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = fixture.run(&["--accept-delete"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.profile_exists());
    let calls = fixture.calls();
    let lines: Vec<_> = calls.lines().collect();
    assert_eq!(
        lines,
        vec![
            "info pm-worker-build",
            "config device get pm-worker-build home pool",
            "delete --force pm-worker-build",
            "storage volume list attached-pool --format=csv --columns=n",
            "storage volume delete attached-pool pm-worker-build-home",
        ]
    );
}
