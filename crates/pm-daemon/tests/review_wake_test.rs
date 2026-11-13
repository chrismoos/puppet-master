mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use pm_protocol::domain::{HookKind, ReviewSide, ReviewState};
use support::{agent_inbox, await_output, daemon_env, hook, spawn_test_session, TestEnv};

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
    hook(&env, session, HookKind::TurnEnded, "");
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
    hook(&env, session, HookKind::TurnEnded, "");
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
    hook(&env, session, HookKind::TurnEnded, "");
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

async fn comment(env: &TestEnv, review_id: u64, line: u32, excerpt: &str, send: bool) -> u64 {
    env.daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id,
            path: "src/lib.rs".into(),
            line,
            side: ReviewSide::Right,
            excerpt: excerpt.into(),
            body: format!("look at {excerpt}"),
            send,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap()
}

async fn no_notice(inbox: &mut support::AgentInbox, why: &str) {
    let got = tokio::time::timeout(Duration::from_millis(400), inbox.lines.recv()).await;
    assert!(got.is_err(), "{why}: {got:?}");
}

#[tokio::test]
async fn comments_sent_during_a_turn_are_announced_once_when_it_ends() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "held");
    let mut inbox = agent_inbox(&env, session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nFOUR\n");
    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();

    hook(&env, session, HookKind::PromptSubmitted, "");
    comment(&env, review.id, 2, "two", true).await;
    comment(&env, review.id, 4, "four", true).await;
    no_notice(
        &mut inbox,
        "a working agent's inbox must not be handed a notice",
    )
    .await;

    env.daemon.process_review_wakes().await;
    no_notice(&mut inbox, "the turn has not ended").await;

    hook(&env, session, HookKind::TurnEnded, "");
    env.daemon.process_review_wakes().await;
    let notice = tokio::time::timeout(Duration::from_secs(5), inbox.lines.recv())
        .await
        .expect("a notice once the turn ended")
        .unwrap();
    assert!(
        notice.contains(&format!("review {} (2 threads)", review.id)),
        "{notice}"
    );

    env.daemon.process_review_wakes().await;
    no_notice(&mut inbox, "a held notice is delivered once").await;
}

#[tokio::test]
async fn a_review_finished_during_the_turn_sends_nothing_afterwards() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "finished-while-busy");
    let mut inbox = agent_inbox(&env, session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();

    hook(&env, session, HookKind::PromptSubmitted, "");
    comment(&env, review.id, 2, "two", true).await;
    env.daemon.finish_review(review.id).await.unwrap();
    hook(&env, session, HookKind::TurnEnded, "");
    env.daemon.process_review_wakes().await;

    no_notice(
        &mut inbox,
        "a finished review must not be announced afterwards",
    )
    .await;
}

#[tokio::test]
async fn comments_resolved_before_the_turn_ends_are_not_announced() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "resolved-while-busy");
    let mut inbox = agent_inbox(&env, session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();

    hook(&env, session, HookKind::PromptSubmitted, "");
    let thread = comment(&env, review.id, 2, "two", true).await;
    env.daemon.resolve_review_thread(thread, true).unwrap();
    hook(&env, session, HookKind::TurnEnded, "");
    env.daemon.process_review_wakes().await;

    no_notice(&mut inbox, "nothing is waiting on the agent").await;
}

#[tokio::test]
async fn a_review_notice_never_falls_back_to_the_terminal_when_an_inbox_exists() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "inbox-unreachable");
    let dir = tempfile::tempdir().unwrap();
    // An inbox that is addressable but takes nothing, as a timed-out
    // delivery looks from the daemon's side.
    support::agent_inbox_path(&env, dir.path(), session);
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &review_ctx(&root), false)
        .await
        .unwrap();
    let (replay, mut rx, _guard) = env.daemon.attach(session).await.unwrap();
    hook(&env, session, HookKind::TurnEnded, "");

    comment(&env, review.id, 2, "two", true).await;
    // A marker typed afterwards bounds the wait: had the notice reached
    // the terminal, the PTY echo would show it before the marker.
    let terminal = env.daemon.agent_terminal(session).unwrap();
    env.daemon.terminal_input(
        terminal.id,
        bytes::Bytes::from_static(b"after-notice-marker"),
    );
    let seen = await_output(&mut rx, replay.to_vec(), b"after-notice-marker").await;
    std::env::remove_var(pm_adapters::TEST_INBOX_DIR_ENV);
    assert!(
        !String::from_utf8_lossy(&seen).contains("Review notice"),
        "the notice was typed into the terminal"
    );
}

#[tokio::test]
async fn open_review_does_not_silently_reopen_a_review_the_user_finished() {
    let _lock = ENV_MUTEX.lock().await;
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "no-silent-reopen");
    write_file(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");
    let ctx = review_ctx(&root);
    let review = env.daemon.open_review(session, &ctx, false).await.unwrap();
    env.daemon.finish_review(review.id).await.unwrap();

    let refused = env
        .daemon
        .open_review_with(session, &ctx, Default::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("finished by the user"), "{refused}");
    assert!(refused.contains("reopen"), "{refused}");
    assert_eq!(
        env.daemon.get_review_for_test(review.id).state,
        ReviewState::Finished
    );

    let reopened = env
        .daemon
        .open_review_with(
            session,
            &ctx,
            pm_daemon::review::OpenReviewOptions {
                reopen_finished: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(reopened.id, review.id);
    assert_eq!(reopened.state, ReviewState::Open);
}
