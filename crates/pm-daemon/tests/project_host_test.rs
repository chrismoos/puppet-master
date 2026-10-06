//! Telling three conditions apart: a host that is off the network, a
//! reachable host a project has no path on, and a reachable host whose
//! configured path cannot be used. Only the first is an outage, and
//! every surface that reports one has to say which it is.

mod support;

use pm_daemon::daemon::DaemonError;
use pm_daemon::project_host::ProjectHostState;
use pm_protocol::domain::{
    AgentKind, ControllerMsg, PathCheck, PermissionMode, WorkerMsg, LOCAL_WORKER_ID,
};
use support::*;

fn enroll(
    env: &TestEnv,
    name: &str,
    protocol_version: u32,
) -> pm_daemon::daemon::WorkerRegistration {
    let (token, _expires) = env.daemon.create_worker_enrollment(name).unwrap();
    env.daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: &format!("key-{name}"),
            hostname: &format!("{name}.local"),
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version,
        })
        .unwrap()
}

/// The bucket the test env's project lives in.
fn bucket_of(env: &TestEnv) -> u64 {
    env.daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == env.project_id)
        .unwrap()
        .bucket_id
}

/// A project whose path is whatever the caller says, including nothing,
/// which is the state a blanked configuration leaves behind.
fn project_with_path(env: &TestEnv, name: &str, path: &str) -> u64 {
    env.daemon
        .create_project(bucket_of(env), name, path)
        .unwrap()
}

/// Lets a project run on a remote host and makes that host its default,
/// which is what a project configured for one Host looks like.
fn allow_remote(env: &TestEnv, project_id: u64, worker_id: u64) {
    let snapshot = env.daemon.subscribe().0;
    let project = snapshot
        .projects
        .iter()
        .find(|p| p.id == project_id)
        .unwrap()
        .clone();
    let bucket = snapshot
        .buckets
        .into_iter()
        .find(|b| b.id == project.bucket_id)
        .unwrap();
    let mut bucket_allowed = bucket.allowed_worker_ids;
    if !bucket_allowed.contains(&worker_id) {
        bucket_allowed.push(worker_id);
    }
    env.daemon
        .set_bucket_workers(
            project.bucket_id,
            &bucket_allowed,
            bucket.default_worker_id,
            None,
        )
        .unwrap();
    let mut allowed = project.allowed_worker_ids;
    if !allowed.contains(&worker_id) {
        allowed.push(worker_id);
    }
    env.daemon
        .set_project_workers(project_id, &allowed, Some(worker_id))
        .unwrap();
}

/// Answers one path check the way a host running the current pm would.
async fn answer_path_check(
    env: &TestEnv,
    worker_id: u64,
    rx: &mut tokio::sync::mpsc::Receiver<ControllerMsg>,
    status: PathCheck,
    detail: &str,
) -> String {
    let (req_id, path) = match tokio::time::timeout(TEST_TIMEOUT, rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ControllerMsg::PathCheck { req_id, path } => (req_id, path),
        other => panic!("expected a path check, got {other:?}"),
    };
    env.daemon.apply_worker_message(
        worker_id,
        WorkerMsg::PathChecked {
            req_id,
            status,
            detail: detail.to_string(),
        },
    );
    path
}

fn spawn_error(env: &TestEnv, project_id: u64, worker_id: Option<u64>) -> DaemonError {
    env.daemon
        .spawn_session_with_agent_override(
            project_id,
            Some(AgentKind::Test),
            "t",
            "do the work",
            None,
            PermissionMode::Inherit,
            worker_id,
            true,
            false,
            None,
            None,
            None,
        )
        .expect_err("the spawn should have been refused")
}

#[tokio::test]
async fn an_unreachable_host_is_the_only_thing_reported_as_offline() {
    let env = daemon_env();
    let reg = enroll(&env, "lima", pm_protocol::WORKER_PROTOCOL_VERSION);
    let worker_id = reg.worker_id;
    allow_remote(&env, env.project_id, worker_id);
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/srv/acme"))
        .unwrap();
    env.daemon.disconnect_worker(&reg.link);

    let state = env
        .daemon
        .project_host_state(env.project_id, worker_id, None)
        .await
        .unwrap();
    assert_eq!(state, ProjectHostState::HostOffline);
    assert_eq!(state.status(), "host-offline");

    let message = spawn_error(&env, env.project_id, Some(worker_id)).to_string();
    assert!(message.contains("offline"), "{message}");
    assert!(message.contains("lima"), "{message}");
}

/// A project with no path of its own runs in the home directory the
/// host reported, so a project can be used before anyone configures a
/// path for every machine it might run on.
#[tokio::test]
async fn a_project_with_no_path_runs_in_the_hosts_home_directory() {
    let env = daemon_env();
    let reg = enroll(&env, "lima", pm_protocol::WORKER_PROTOCOL_VERSION);
    let worker_id = reg.worker_id;
    // No mapping for this host, and no project path either.
    let project_id = project_with_path(&env, "blanked", "");
    allow_remote(&env, project_id, worker_id);

    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == project_id)
        .unwrap();
    assert_eq!(
        env.daemon
            .effective_project_path(&project, worker_id, None)
            .unwrap(),
        "/home/dev",
        "the home directory the host reported at registration"
    );
}

/// A host that reported no home leaves nothing to fall back to, and the
/// refusal still has to name the host and the project rather than read
/// as an outage.
#[tokio::test]
async fn a_host_that_reported_no_home_still_says_the_path_is_unset() {
    let env = daemon_env();
    let (token, _expires) = env.daemon.create_worker_enrollment("rootless").unwrap();
    let reg = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-rootless",
            hostname: "rootless.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    let worker_id = reg.worker_id;
    let project_id = project_with_path(&env, "blanked", "");
    allow_remote(&env, project_id, worker_id);

    let state = env
        .daemon
        .project_host_state(project_id, worker_id, None)
        .await
        .unwrap();
    assert_eq!(state, ProjectHostState::PathUnset);

    let message = spawn_error(&env, project_id, Some(worker_id)).to_string();
    assert!(
        !message.contains("offline"),
        "a reachable host must not read as an outage: {message}"
    );
    assert!(message.contains("no path configured"), "{message}");
    assert!(message.contains("blanked"), "{message}");
    assert!(message.contains("rootless"), "{message}");
}

#[tokio::test]
async fn a_configured_path_that_is_missing_on_the_host_names_the_path() {
    let env = daemon_env();
    let reg = enroll(&env, "lima", pm_protocol::WORKER_PROTOCOL_VERSION);
    let worker_id = reg.worker_id;
    let mut rx = reg.rx;
    allow_remote(&env, env.project_id, worker_id);
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/srv/acme"))
        .unwrap();

    let daemon = env.daemon.clone();
    let project_id = env.project_id;
    let task =
        tokio::spawn(async move { daemon.project_host_state(project_id, worker_id, None).await });
    let asked = answer_path_check(&env, worker_id, &mut rx, PathCheck::Missing, "").await;
    assert_eq!(asked, "/srv/acme", "the host is asked about its own path");

    let state = task.await.unwrap().unwrap();
    assert_eq!(
        state,
        ProjectHostState::PathMissing {
            path: "/srv/acme".into()
        }
    );
    let message = state.message("acme", "lima", worker_id);
    assert!(message.contains("/srv/acme"), "{message}");
    assert!(message.contains("does not exist"), "{message}");
    assert!(!message.contains("offline"), "{message}");
}

#[tokio::test]
async fn a_path_that_is_a_file_and_a_path_that_cannot_be_read_are_told_apart() {
    let env = daemon_env();
    let reg = enroll(&env, "lima", pm_protocol::WORKER_PROTOCOL_VERSION);
    let worker_id = reg.worker_id;
    let mut rx = reg.rx;
    allow_remote(&env, env.project_id, worker_id);
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/srv/acme"))
        .unwrap();

    let daemon = env.daemon.clone();
    let project_id = env.project_id;
    let task =
        tokio::spawn(async move { daemon.project_host_state(project_id, worker_id, None).await });
    answer_path_check(&env, worker_id, &mut rx, PathCheck::NotADirectory, "").await;
    assert_eq!(
        task.await.unwrap().unwrap(),
        ProjectHostState::PathNotADirectory {
            path: "/srv/acme".into()
        }
    );

    let daemon = env.daemon.clone();
    let task =
        tokio::spawn(async move { daemon.project_host_state(project_id, worker_id, None).await });
    answer_path_check(
        &env,
        worker_id,
        &mut rx,
        PathCheck::Unreadable,
        "Permission denied (os error 13)",
    )
    .await;
    let state = task.await.unwrap().unwrap();
    assert_eq!(
        state,
        ProjectHostState::PathUnreadable {
            path: "/srv/acme".into(),
            detail: "Permission denied (os error 13)".into(),
        }
    );
    assert!(
        state
            .message("acme", "lima", worker_id)
            .contains("os error 13"),
        "the operating system's reason is what makes it fixable"
    );
}

#[tokio::test]
async fn a_usable_path_lets_the_spawn_through() {
    let env = daemon_env();
    let reg = enroll(&env, "lima", pm_protocol::WORKER_PROTOCOL_VERSION);
    let worker_id = reg.worker_id;
    let mut rx = reg.rx;
    allow_remote(&env, env.project_id, worker_id);
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/srv/acme"))
        .unwrap();

    let daemon = env.daemon.clone();
    let project_id = env.project_id;
    let task = tokio::spawn(async move { daemon.ensure_spawnable(project_id, None, None).await });
    answer_path_check(&env, worker_id, &mut rx, PathCheck::Ok, "").await;
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn an_explicit_working_directory_is_what_gets_checked() {
    let env = daemon_env();
    let reg = enroll(&env, "lima", pm_protocol::WORKER_PROTOCOL_VERSION);
    let worker_id = reg.worker_id;
    let mut rx = reg.rx;
    let project_id = project_with_path(&env, "blanked", "");
    allow_remote(&env, project_id, worker_id);

    let daemon = env.daemon.clone();
    let task = tokio::spawn(async move {
        daemon
            .ensure_spawnable(project_id, None, Some("/srv/override"))
            .await
    });
    let asked = answer_path_check(&env, worker_id, &mut rx, PathCheck::Ok, "").await;
    assert_eq!(asked, "/srv/override");
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_host_too_old_to_answer_is_reported_as_unchecked_and_still_spawns() {
    let env = daemon_env();
    let reg = enroll(&env, "lima", pm_protocol::WORKER_PROTOCOL_PATH_CHECK - 1);
    let worker_id = reg.worker_id;
    allow_remote(&env, env.project_id, worker_id);
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some("/srv/acme"))
        .unwrap();

    let state = env
        .daemon
        .project_host_state(env.project_id, worker_id, None)
        .await
        .unwrap();
    assert_eq!(state.status(), "path-unchecked");
    assert!(state.can_launch(), "no evidence against the path");
    let message = state.message("acme", "lima", worker_id);
    assert!(message.contains("/srv/acme"), "{message}");
    assert!(!message.contains("offline"), "{message}");
}

#[tokio::test]
async fn the_local_host_is_checked_by_the_controller_itself() {
    let env = daemon_env();

    let state = env
        .daemon
        .project_host_state(env.project_id, LOCAL_WORKER_ID, None)
        .await
        .unwrap();
    assert_eq!(
        state,
        ProjectHostState::Ready {
            path: env.project_root().to_string_lossy().into_owned()
        }
    );

    let gone = env.project_root().join("removed");
    let missing = project_with_path(&env, "gone", gone.to_str().unwrap());
    let state = env
        .daemon
        .project_host_state(missing, LOCAL_WORKER_ID, None)
        .await
        .unwrap();
    assert_eq!(state.status(), "path-missing");
    assert_eq!(state.path(), gone.to_string_lossy());
}

/// The local worker is this process, so a project with no path runs in
/// this process's own home directory. A session still never starts in
/// nowhere: without a home there is nothing to fall back to and the
/// spawn is refused, which the rootless host above covers.
#[tokio::test]
async fn a_project_with_no_path_runs_in_this_processs_home() {
    let env = daemon_env();
    let project_id = project_with_path(&env, "blanked", "");
    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == project_id)
        .unwrap();

    assert_eq!(
        env.daemon
            .effective_project_path(&project, LOCAL_WORKER_ID, None)
            .unwrap(),
        std::env::var("HOME").unwrap_or_default()
    );
}
