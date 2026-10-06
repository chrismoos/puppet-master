mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use pm_protocol::domain::ReviewSide;
use support::{agent_inbox, daemon_env, spawn_test_session, TestEnv};

fn git(args: &[&str], cwd: &Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn write_file(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn head_sha(root: &Path) -> String {
    String::from_utf8_lossy(
        &Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string()
}

fn repo_env() -> (TestEnv, PathBuf) {
    let env = daemon_env();
    let root = env.project_root();
    git(&["init", "-q", "-b", "master"], &root);
    git(&["config", "user.email", "review@test"], &root);
    git(&["config", "user.name", "review"], &root);
    git(&["config", "commit.gpgsign", "false"], &root);
    write_file(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    git(&["add", "-A"], &root);
    git(&["commit", "-qm", "base"], &root);
    (env, root)
}

fn review_ctx(root: &Path) -> pm_daemon::review::ReviewContext {
    pm_daemon::review::ReviewContext {
        worktree: root.display().to_string(),
        base: head_sha(root),
        label: "test review".into(),
        ..Default::default()
    }
}

static ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn an_idle_session_receives_inbox_notice_when_comments_sent() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "idle");
    let mut inbox = agent_inbox(&env, session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();

    assert!(!env.daemon.is_session_awaiting_review(session));

    let _thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "please check line two".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    env.daemon
        .send_review_threads(review.id, &[])
        .await
        .unwrap();

    let received = tokio::time::timeout(Duration::from_secs(5), inbox.lines.recv())
        .await
        .expect("inbox received message")
        .expect("channel not closed");

    let json: serde_json::Value = serde_json::from_str(&received).unwrap();
    assert_eq!(json["type"], "user");
    let content = json["message"]["content"].as_str().unwrap();
    assert!(content.contains("[puppet-master] Review notice:"));
    assert!(content.contains("New review comments were sent on review"));
    assert!(content.contains("Call next_review_event"));
}

#[tokio::test]
async fn a_polling_session_does_not_receive_wake_notice() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "polling");
    let mut inbox = agent_inbox(&env, session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();

    let daemon = env.daemon.clone();
    let poll_handle = tokio::spawn(async move {
        daemon
            .await_review_event(session, Duration::from_secs(5))
            .await
    });

    for _ in 0..50 {
        if env.daemon.is_session_awaiting_review(session) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(env.daemon.is_session_awaiting_review(session));

    let _thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "polling check".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    env.daemon
        .send_review_threads(review.id, &[])
        .await
        .unwrap();

    let wait_outcome = poll_handle.await.unwrap().unwrap();
    match wait_outcome {
        pm_daemon::review::ReviewWait::Event(event) => {
            assert_eq!(event.review.id, review.id);
        }
        other => panic!("expected ReviewWait::Event, got {other:?}"),
    }

    let wake = tokio::time::timeout(Duration::from_millis(300), inbox.lines.recv()).await;
    assert!(
        wake.is_err(),
        "polling session must not receive a wake notice"
    );
}

#[tokio::test]
async fn review_wake_notices_are_throttled_across_rapid_sends() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "throttle");
    let mut inbox = agent_inbox(&env, session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nFOUR\n");

    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();

    let _t1 = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "first comment".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    let first_notice = tokio::time::timeout(Duration::from_secs(5), inbox.lines.recv())
        .await
        .expect("first notice received")
        .expect("channel open");
    assert!(first_notice.contains("[puppet-master] Review notice:"));

    let _t2 = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 4,
            side: ReviewSide::Right,
            excerpt: "four".into(),
            body: "second comment".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    let second_notice = tokio::time::timeout(Duration::from_millis(400), inbox.lines.recv()).await;
    assert!(
        second_notice.is_err(),
        "rapid second send must be throttled"
    );
}

#[tokio::test]
async fn an_idle_session_receives_inbox_notice_when_review_finished() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "idle-finish");
    let mut inbox = agent_inbox(&env, session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();

    assert!(!env.daemon.is_session_awaiting_review(session));

    env.daemon.finish_review(review.id).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(5), inbox.lines.recv())
        .await
        .expect("inbox received message")
        .expect("channel not closed");

    let json: serde_json::Value = serde_json::from_str(&received).unwrap();
    assert_eq!(json["type"], "user");
    let content = json["message"]["content"].as_str().unwrap();
    assert!(content.contains("[puppet-master] Review notice:"));
    assert!(content.contains(&format!("Review {} was finished.", review.id)));
    assert!(content.contains("No further comments are pending."));
}

#[tokio::test]
async fn a_polling_session_does_not_receive_wake_notice_when_review_finished() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "polling-finish");
    let mut inbox = agent_inbox(&env, session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();

    let daemon = env.daemon.clone();
    let poll_handle = tokio::spawn(async move {
        daemon
            .await_review_event(session, Duration::from_secs(5))
            .await
    });

    for _ in 0..50 {
        if env.daemon.is_session_awaiting_review(session) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(env.daemon.is_session_awaiting_review(session));

    env.daemon.finish_review(review.id).await.unwrap();

    let wait_outcome = poll_handle.await.unwrap().unwrap();
    assert!(matches!(
        wait_outcome,
        pm_daemon::review::ReviewWait::Finished
    ));

    let wake = tokio::time::timeout(Duration::from_millis(300), inbox.lines.recv()).await;
    assert!(
        wake.is_err(),
        "polling session must not receive a wake notice on finish"
    );
}
