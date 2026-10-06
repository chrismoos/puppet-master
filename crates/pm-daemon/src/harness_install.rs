use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use pm_protocol::domain::{AgentKind, HarnessStatus};
use tokio::io::{AsyncRead, AsyncReadExt};

const OUTPUT_LIMIT: usize = 32 * 1024;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const READ_BUFFER_SIZE: usize = 4096;
const EXECUTABLE_BITS: u32 = 0o111;
type Installation = Arc<Mutex<HarnessStatus>>;
static INSTALLATIONS: OnceLock<Mutex<HashMap<AgentKind, Installation>>> = OnceLock::new();

pub fn install_command(agent: AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::ClaudeCode => Some("curl -fsSL https://claude.ai/install.sh | bash"),
        AgentKind::Codex => Some("curl -fsSL https://chatgpt.com/codex/install.sh | sh"),
        AgentKind::Gemini => Some("npm install -g @google/gemini-cli"),
        AgentKind::OpenCode => Some("curl -fsSL https://opencode.ai/install | bash"),
        AgentKind::Antigravity => {
            Some("curl -fsSL https://antigravity.google/cli/install.sh | bash")
        }
        AgentKind::Test => None,
    }
}

pub fn resolve_program(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_in(program, &path, home.as_deref())
}

pub(crate) fn resolve_in(
    program: &str,
    path: &std::ffi::OsStr,
    home: Option<&Path>,
) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path)
        .filter(|p| p.is_absolute())
        .collect();
    if let Some(home) = home {
        dirs.extend([home.join(".local/bin"), home.join(".opencode/bin")]);
    }
    dirs.into_iter()
        .map(|dir| dir.join(program))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & EXECUTABLE_BITS != 0)
        })
}

pub fn request(agent: AgentKind, install: bool) -> HarnessStatus {
    let mut installations = INSTALLATIONS.get_or_init(Mutex::default).lock().unwrap();
    request_with(
        &mut installations,
        agent,
        install,
        resolve_program(agent.program()).is_some(),
    )
}

fn request_with(
    installations: &mut HashMap<AgentKind, Installation>,
    agent: AgentKind,
    install: bool,
    available: bool,
) -> HarnessStatus {
    if let Some(previous) = installations.get(&agent) {
        let status = previous.lock().unwrap().clone();
        if status.state == "installing" || (!install && status.state == "failed") {
            return status;
        }
    }
    let mut status = HarnessStatus {
        state: "missing".into(),
        command: install_command(agent).unwrap_or_default().into(),
        output: String::new(),
        error: String::new(),
    };
    if agent == AgentKind::Test || available {
        status.state = "ready".into();
    } else if install {
        status.state = "installing".into();
        let shared = Arc::new(Mutex::new(status.clone()));
        installations.insert(agent, shared.clone());
        tokio::spawn(async move {
            let command = shared.lock().unwrap().command.clone();
            let result = run_installer(&command, &shared, INSTALL_TIMEOUT).await.and_then(|()| {
                resolve_program(agent.program()).map(|_| ()).ok_or_else(|| format!(
                    "The installer finished, but {} was not found on the worker's PATH or in its user install directory.", agent.program()
                ))
            });
            let mut status = shared.lock().unwrap();
            match result {
                Ok(()) => status.state = "ready".into(),
                Err(error) => {
                    status.state = "failed".into();
                    status.error = error;
                }
            }
        });
    }
    status
}

async fn capture(reader: impl AsyncRead + Unpin, status: Installation) {
    let mut reader = reader;
    let mut buffer = [0; READ_BUFFER_SIZE];
    while let Ok(count) = reader.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        let mut status = status.lock().unwrap();
        status
            .output
            .push_str(&String::from_utf8_lossy(&buffer[..count]));
        if status.output.len() > OUTPUT_LIMIT {
            let mut start = status.output.len() - OUTPUT_LIMIT;
            while !status.output.is_char_boundary(start) {
                start += 1;
            }
            status.output.drain(..start);
        }
    }
}

async fn run_installer(
    command: &str,
    status: &Installation,
    timeout: Duration,
) -> Result<(), String> {
    let mut child = tokio::process::Command::new("bash")
        .args(["-o", "pipefail", "-c", command])
        .current_dir(
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/")),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start the installer: {e}"))?;
    let pid = child.id().expect("spawned installer has a process id");
    let stdout = capture(child.stdout.take().unwrap(), status.clone());
    let stderr = capture(child.stderr.take().unwrap(), status.clone());
    let outcome = tokio::time::timeout(timeout, async {
        let (result, (), ()) = tokio::join!(child.wait(), stdout, stderr);
        result
    })
    .await;
    match outcome {
        Ok(Ok(exit)) if exit.success() => Ok(()),
        Ok(Ok(exit)) => Err(format!("Installation failed ({exit}).")),
        Ok(Err(e)) => Err(format!("Could not wait for the installer: {e}")),
        Err(_) => {
            // The installer pipeline and its downloads share this process group
            // when the installer leads one. When it does not, negating the pid
            // would signal whichever group holds that id, which is somebody
            // else's process tree, so this asks first.
            crate::mux::signal_child_group(pid, libc::SIGKILL);
            let _ = child.wait().await;
            Err(format!(
                "Installation timed out after {} seconds.",
                timeout.as_secs()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn status() -> Installation {
        Arc::new(Mutex::new(HarnessStatus {
            state: "installing".into(),
            command: String::new(),
            output: String::new(),
            error: String::new(),
        }))
    }

    #[test]
    fn resolves_existing_executables_before_user_install_directories() {
        const EXECUTABLE_MODE: u32 = 0o755;
        const READ_WRITE_MODE: u32 = 0o644;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin");
        let local = dir.path().join(".local/bin");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        for parent in [&path, &local] {
            let file = parent.join("claude");
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(EXECUTABLE_MODE))
                .unwrap();
        }
        assert_eq!(
            resolve_in("claude", path.as_os_str(), Some(dir.path())),
            Some(path.join("claude"))
        );
        std::fs::remove_file(path.join("claude")).unwrap();
        assert_eq!(
            resolve_in("claude", path.as_os_str(), Some(dir.path())),
            Some(local.join("claude"))
        );
        std::fs::set_permissions(
            local.join("claude"),
            std::fs::Permissions::from_mode(READ_WRITE_MODE),
        )
        .unwrap();
        assert_eq!(
            resolve_in("claude", path.as_os_str(), Some(dir.path())),
            None
        );
    }

    #[test]
    fn checks_never_install_and_existing_executables_are_preserved() {
        let mut jobs = HashMap::new();
        assert_eq!(
            request_with(&mut jobs, AgentKind::ClaudeCode, false, false).state,
            "missing"
        );
        assert_eq!(
            request_with(&mut jobs, AgentKind::ClaudeCode, true, true).state,
            "ready"
        );
        assert!(jobs.is_empty());
    }

    #[test]
    fn every_selectable_harness_has_an_installer() {
        for agent in AgentKind::SELECTABLE {
            assert!(install_command(*agent).is_some());
        }
        assert!(install_command(AgentKind::Test).is_none());
    }

    #[tokio::test]
    async fn preserves_stdout_stderr_and_failure_status() {
        let status = status();
        let error = run_installer(
            "echo downloading; echo 'permission denied' >&2; exit 7",
            &status,
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert!(error.contains('7'));
        let output = &status.lock().unwrap().output;
        assert!(output.contains("downloading"));
        assert!(output.contains("permission denied"));
    }

    #[tokio::test]
    async fn failed_download_in_a_pipeline_is_a_failure() {
        assert!(
            run_installer("false | bash", &status(), Duration::from_secs(5))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn reports_output_before_installation_finishes() {
        let status = status();
        let shared = status.clone();
        let task = tokio::spawn(async move {
            run_installer("echo downloading; sleep 1", &shared, Duration::from_secs(5)).await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !status.lock().unwrap().output.contains("downloading") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!task.is_finished());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn times_out_and_keeps_the_last_output() {
        const TIMEOUT: Duration = Duration::from_secs(30);
        const OUTPUT_WAIT: Duration = Duration::from_secs(5);
        let status = status();
        let shared = status.clone();
        let task =
            tokio::spawn(
                async move { run_installer("echo waiting; sleep 60", &shared, TIMEOUT).await },
            );
        tokio::time::timeout(OUTPUT_WAIT, async {
            while !status.lock().unwrap().output.contains("waiting") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!task.is_finished());
        tokio::time::pause();
        tokio::time::advance(TIMEOUT).await;
        let error = task.await.unwrap().unwrap_err();
        assert!(error.contains("timed out"));
        assert!(status.lock().unwrap().output.contains("waiting"));
    }

    #[tokio::test]
    async fn caps_output_without_stopping_the_installer() {
        let status = status();
        run_installer(
            "yes progress | head -c 100000; echo done",
            &status,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let output = &status.lock().unwrap().output;
        assert!(output.len() <= OUTPUT_LIMIT);
        assert!(output.ends_with("done\n"));
    }

    #[test]
    fn concurrent_requests_share_installation_and_failed_polls_keep_the_error() {
        let status = status();
        let mut jobs = HashMap::from([(AgentKind::ClaudeCode, status.clone())]);
        assert_eq!(
            request_with(&mut jobs, AgentKind::ClaudeCode, true, false).state,
            "installing"
        );
        assert!(Arc::ptr_eq(
            jobs.get(&AgentKind::ClaudeCode).unwrap(),
            &status
        ));
        status.lock().unwrap().state = "failed".into();
        status.lock().unwrap().error = "download failed".into();
        assert_eq!(
            request_with(&mut jobs, AgentKind::ClaudeCode, false, false).error,
            "download failed"
        );
    }
}
