//! The review round trip against a real git repository: opening,
//! anchoring, dispatch, replies, and the revision views.

mod support;

use pm_daemon::review::ReviewContext;
use pm_protocol::domain::{
    ReviewAnchorStatus, ReviewChoiceAnswer, ReviewChoiceSelect, ReviewSide, ReviewThreadState,
    ReviewViewerStateUpdate,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{daemon_env, spawn_test_session, TestEnv, TEST_HOST, TEST_ORIGIN};

fn git(args: &[&str], cwd: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(root: &Path, rel: &str, body: &str) {
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

/// The context a caller states outright. Reviews never infer this, so
/// every test spells out the tree and the resolved base it means.
fn ctx(root: &Path) -> ReviewContext {
    ReviewContext {
        worktree: root.display().to_string(),
        base: head_sha(root),
        label: "test review".into(),
        ..Default::default()
    }
}

/// A daemon whose project directory is a real repository with one
/// commit, which is what a review needs to resolve a range at all.
fn repo_env() -> (TestEnv, PathBuf) {
    let env = daemon_env();
    let root = env.project_root();
    git(&["init", "-q", "-b", "master"], &root);
    git(&["config", "user.email", "review@test"], &root);
    git(&["config", "user.name", "review"], &root);
    git(&["config", "commit.gpgsign", "false"], &root);
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    git(&["add", "-A"], &root);
    git(&["commit", "-qm", "base"], &root);
    (env, root)
}

#[tokio::test]
async fn opening_a_review_resolves_the_range_and_snapshots_immediately() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .expect("review opens");

    // A single ref means "everything since it", so the head side is
    // dropped and the diff runs against the working tree.
    assert!(review.head.is_empty());
    assert!(!review.base.is_empty());
    assert!(review.range_key.ends_with("..WORKING"));
    // Pinned from the first frame, so an agent already mid-task cannot
    // move the page under the reader.
    assert_eq!(review.revision, 1);

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    assert_eq!(detail.files, vec!["src/lib.rs".to_string()]);
}

#[tokio::test]
async fn reopening_the_same_range_attaches_rather_than_duplicating() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let first = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let second = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    assert_eq!(first.id, second.id);

    // A different pathspec is a different review, with its own threads.
    let filtered = env
        .daemon
        .open_review(
            session,
            &ReviewContext {
                pathspec: vec!["src".into()],
                ..ctx(&root)
            },
            false,
        )
        .await
        .unwrap();
    assert_ne!(first.id, filtered.id);
}

#[tokio::test]
async fn a_thread_follows_its_line_when_the_agent_edits_above_it() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 5,
            side: ReviewSide::Right,
            excerpt: "five".into(),
            body: "why five?".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    // The agent inserts two lines above the commented one.
    write(
        &root,
        "src/lib.rs",
        "one\nINSERTED\nALSO\ntwo\nthree\nfour\nfive\n",
    );

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.anchor_status, ReviewAnchorStatus::Moved);
    assert_eq!(t.current_line, 7);
    // The excerpt shows today's code so the agent is not reading a
    // stale line number.
    assert!(t.current_excerpt.contains("five"));
}

#[tokio::test]
async fn a_comment_on_a_line_that_was_rewritten_is_flagged_as_changed() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    // The work under review: a line added since the base commit.
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "rename this".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    write(&root, "src/lib.rs", "one\nRENAMED\nthree\nfour\nfive\n");

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.anchor_status, ReviewAnchorStatus::Changed);
}

#[tokio::test]
async fn a_left_side_comment_never_moves() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Left,
            excerpt: "two".into(),
            body: "this was wrong before too".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    write(&root, "src/lib.rs", "PREPENDED\none\ntwo\nthree\nfour\n");

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.current_line, 2);
    assert_eq!(t.anchor_status, ReviewAnchorStatus::Same);
}

#[tokio::test]
async fn an_unsent_comment_stays_a_draft_until_it_is_sent() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    env.daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 1,
            side: ReviewSide::Right,
            excerpt: "one".into(),
            body: "draft".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    // A draft is the reviewer's, not the agent's: nothing is queued.
    assert!(env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .is_none());
    assert_eq!(env.daemon.get_review_for_test(review.id).draft_count, 1);

    env.daemon
        .send_review_threads(review.id, &[])
        .await
        .unwrap();
    let event = env.daemon.next_review_event(session).await.unwrap();
    assert!(event.is_some());
    assert_eq!(event.unwrap().thread.state, ReviewThreadState::Sent);
}

#[tokio::test]
async fn a_claim_nobody_answered_is_handed_out_again() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    git(&["add", "-A"], &root.clone());
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    for line in [1, 3] {
        env.daemon
            .add_review_comment(&pm_daemon::review::NewComment {
                review_id: review.id,
                path: "src/lib.rs".into(),
                line,
                side: ReviewSide::Right,
                excerpt: "x".into(),
                body: "look at this".into(),
                send: false,
                anchor_snapshot_id: None,
                choice: None,
            })
            .await
            .unwrap();
    }
    env.daemon
        .send_review_threads(review.id, &[])
        .await
        .unwrap();

    // The first is claimed, and in the failure this covers its response never
    // reaches the agent: it cannot reply, so nothing clears the claim.
    let stranded = env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .expect("the first thread dispatches");
    assert!(env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .is_none());

    // Inside the window the claim still holds the file.
    assert_eq!(
        env.daemon
            .reclaim_stale_review_claims(
                pm_daemon::daemon::now_unix_ms() - pm_daemon::review_store::REVIEW_CLAIM_STALE_MS
            )
            .unwrap(),
        0
    );

    // Past it the file is dispatchable again rather than stranded for the life
    // of the review.
    assert_eq!(
        env.daemon
            .reclaim_stale_review_claims(pm_daemon::daemon::now_unix_ms())
            .unwrap(),
        1
    );
    let again = env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .expect("the stranded thread is handed out again");
    assert_eq!(again.thread.id, stranded.thread.id);
}

#[tokio::test]
async fn two_threads_on_one_file_dispatch_one_at_a_time() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    write(&root, "src/other.rs", "alpha\nbeta\n");
    let root_ref = root.clone();
    git(&["add", "-A"], &root_ref);
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    for (path, line) in [("src/lib.rs", 1), ("src/lib.rs", 3), ("src/other.rs", 1)] {
        env.daemon
            .add_review_comment(&pm_daemon::review::NewComment {
                review_id: review.id,
                path: path.into(),
                line,
                side: ReviewSide::Right,
                excerpt: "x".into(),
                body: "look at this".into(),
                send: false,
                anchor_snapshot_id: None,
                choice: None,
            })
            .await
            .unwrap();
    }
    env.daemon
        .send_review_threads(review.id, &[])
        .await
        .unwrap();

    let first = env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    let second = env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    // Both in flight, but never both on the same file.
    assert_ne!(first.thread.path, second.thread.path);
    // The third shares a file with one of them, so it waits.
    assert!(env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_reply_marks_the_thread_answered_and_names_the_files_it_changed() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "rename it".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    let event = env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.thread.id, thread);

    // The agent edits, then replies.
    write(&root, "src/lib.rs", "one\nRENAMED\nthree\nfour\n");
    env.daemon
        .post_review_reply(session, thread, "renamed it", true)
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.state, ReviewThreadState::Answered);
    let reply = t.messages.last().unwrap();
    assert!(reply.addressed);
    assert_eq!(reply.session_id, session);
    // The link a reply carries points at the file this thread touched.
    assert_eq!(reply.changed_files, vec!["src/lib.rs".to_string()]);
}

/// The reported failure: comment on the only change, ask for it to be
/// removed, and the file stops differing from the base at all. Nothing
/// is left for the diff to render, so the conversation went with it.
#[tokio::test]
async fn a_file_whose_change_was_undone_still_shows_its_comment() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nEXTRA\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 3,
            side: ReviewSide::Right,
            excerpt: "EXTRA".into(),
            body: "remove this line".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();

    // The agent does what was asked, which leaves the file identical to
    // the base.
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    env.daemon
        .post_review_reply(session, thread, "removed", true)
        .await
        .unwrap();

    for view in ["cum:1", "live"] {
        let diff = env
            .daemon
            .review_diff(review.id, view, 3, None)
            .await
            .unwrap()
            .content;
        assert!(
            diff.contains("diff --git a/src/lib.rs"),
            "the commented file is still rendered in {view}: {diff}"
        );
        // Context only: the change it was about is genuinely gone.
        assert!(!diff.contains("+EXTRA"), "{view}: {diff}");
        assert!(!diff.contains("-EXTRA"), "{view}: {diff}");
        // With the code around where it stood, so the comment lands on
        // something a reader can read.
        assert!(diff.contains(" two"), "{view}: {diff}");
        assert!(diff.contains(" three"), "{view}: {diff}");
    }

    // And the thread itself is still there to be rendered.
    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    assert!(detail.files.contains(&"src/lib.rs".to_string()));
    assert_eq!(detail.threads.len(), 1);
}

#[tokio::test]
async fn a_brand_new_file_deleted_after_comment_is_rendered_as_deleted_across_revisions() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/brand_new.rs", "fn brand_new() {}\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/brand_new.rs".into(),
            line: 1,
            side: ReviewSide::Right,
            excerpt: "fn brand_new() {}".into(),
            body: "please delete this file".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();

    std::fs::remove_file(root.join("src/brand_new.rs")).unwrap();
    env.daemon
        .post_review_reply(session, thread, "deleted the file", true)
        .await
        .unwrap();

    for view in ["cum:1", "live"] {
        let diff = env
            .daemon
            .review_diff(review.id, view, 3, None)
            .await
            .unwrap()
            .content;
        assert!(
            diff.contains("diff --git a/src/brand_new.rs b/src/brand_new.rs"),
            "the deleted brand new file is rendered in {view}: {diff}"
        );
        assert!(
            diff.contains("deleted file"),
            "marked as deleted file in {view}: {diff}"
        );
    }

    let round_diff = env
        .daemon
        .review_diff(review.id, "round:1", 3, None)
        .await
        .unwrap()
        .content;
    assert!(
        round_diff.contains("diff --git a/src/brand_new.rs b/src/brand_new.rs"),
        "the round diff contains the file: {round_diff}"
    );
    assert!(
        round_diff.contains("-fn brand_new()"),
        "the round diff shows lines removed: {round_diff}"
    );

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    assert!(
        detail.files.contains(&"src/brand_new.rs".to_string()),
        "detail files contains brand_new.rs: {:?}",
        detail.files
    );
    assert_eq!(detail.threads.len(), 1);
    assert_eq!(detail.threads[0].path, "src/brand_new.rs");
    assert_eq!(detail.threads[0].excerpt, "fn brand_new() {}");
}

/// The same failure one level down: the file still has changes, so it
/// renders, but the commented line is no longer inside any of them.
#[tokio::test]
async fn a_comment_outside_every_remaining_hunk_still_has_code_under_it() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    // A base long enough that one region can settle while another goes
    // on changing.
    let base: String = (1..=12).map(|n| format!("l{n:02}\n")).collect();
    write(&root, "src/lib.rs", &base);
    git(&["add", "-A"], &root);
    git(&["commit", "-qm", "long"], &root);

    let mut working: Vec<String> = (1..=12).map(|n| format!("l{n:02}")).collect();
    working[11] = "CHANGED".into();
    working.insert(2, "EXTRA".into());
    let joined = |v: &[String]| format!("{}\n", v.join("\n"));
    write(&root, "src/lib.rs", &joined(&working));

    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 3,
            side: ReviewSide::Right,
            excerpt: "EXTRA".into(),
            body: "remove this line".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();

    // EXTRA goes, the edit at the far end stays, so the file still
    // differs — nowhere near where the comment sits.
    working.remove(2);
    write(&root, "src/lib.rs", &joined(&working));
    env.daemon
        .post_review_reply(session, thread, "removed", true)
        .await
        .unwrap();

    let diff = env
        .daemon
        .review_diff(review.id, "live", 3, None)
        .await
        .unwrap()
        .content;
    assert!(
        diff.contains("+CHANGED"),
        "the change that is left still renders: {diff}"
    );
    // The commented region comes back as its own section rather than
    // being dropped for having stopped changing.
    assert!(diff.contains(" l03"), "{diff}");
    assert_eq!(diff.matches("@@ -").count(), 2, "two sections: {diff}");
}

/// A reviewer's reply belongs in the thread it answers. Opening a second
/// thread on the same line instead left one conversation stacked up the
/// page in pieces, each resolvable on its own.
#[tokio::test]
async fn a_reviewer_reply_stays_in_its_thread_and_goes_back_to_the_agent() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "TWO".into(),
            body: "why upper case?".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    env.daemon
        .post_review_reply(session, thread, "it names a constant", true)
        .await
        .unwrap();

    env.daemon
        .reply_review_thread(thread, "then say so in a comment")
        .await
        .unwrap();

    // One conversation, in order, rather than a second thread on the
    // same line.
    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    assert_eq!(detail.threads.len(), 1, "{:?}", detail.threads);
    let bodies: Vec<&str> = detail.threads[0]
        .messages
        .iter()
        .map(|m| m.body.as_str())
        .collect();
    assert_eq!(
        bodies,
        vec![
            "why upper case?",
            "it names a constant",
            "then say so in a comment"
        ]
    );

    // And the agent is asked again, or the follow-up is one nobody answers.
    let event = env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .expect("the reply is queued back to the agent");
    assert_eq!(event.thread.id, thread);
}

#[tokio::test]
async fn a_reply_reopens_a_thread_the_reviewer_had_resolved() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "TWO".into(),
            body: "why upper case?".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon.resolve_review_thread(thread, true).unwrap();

    env.daemon
        .reply_review_thread(thread, "actually, one more thing")
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    assert_eq!(detail.threads[0].state, ReviewThreadState::Sent);

    // An empty reply is refused rather than stored as a blank turn.
    assert!(env.daemon.reply_review_thread(thread, "   ").await.is_err());
}

#[tokio::test]
async fn the_round_view_shows_only_what_the_agent_changed() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    // The reviewer's own work in progress, which is what they opened
    // the review to read.
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nREVIEWER\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "rename it".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    write(&root, "src/lib.rs", "one\nRENAMED\nthree\nfour\nREVIEWER\n");
    env.daemon
        .post_review_reply(session, thread, "done", true)
        .await
        .unwrap();

    let round = env
        .daemon
        .review_diff(review.id, "round:1", 3, None)
        .await
        .unwrap()
        .content;
    assert!(round.contains("-two"), "{round}");
    assert!(round.contains("+RENAMED"), "{round}");
    // The reviewer's own pre-existing edit was already there when the
    // round started, so the round does not claim the agent made it.
    assert!(!round.contains("+REVIEWER"), "{round}");

    // The cumulative view runs from the base and does include it.
    let cumulative = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap()
        .content;
    assert!(cumulative.contains("+RENAMED"), "{cumulative}");
}

/// The reported shape of a review drawing a one-line edit as a whole
/// new file: the base snapshot is captured once at open, so a file the
/// agent only starts changing afterwards is not in it.
#[tokio::test]
async fn a_file_first_changed_after_the_review_opened_keeps_its_base_side() {
    let (env, root) = repo_env();
    write(
        &root,
        "src/layout.rs",
        "alpha
beta
gamma
",
    );
    git(&["add", "-A"], &root);
    git(&["commit", "-qm", "second file"], &root);
    let session = spawn_test_session(&env, "p");
    // Only one of the two files differs when the review opens.
    write(
        &root,
        "src/lib.rs",
        "one
TWO
three
four
",
    );
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    // The agent then edits one line of a file that was untouched at open.
    write(
        &root,
        "src/layout.rs",
        "alpha
BETA
gamma
",
    );

    let diff = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap()
        .content;
    assert!(diff.contains("-beta"), "the base side was lost: {diff}");
    assert!(diff.contains("+BETA"), "{diff}");
    // The two lines it did not touch are context, not additions.
    assert!(!diff.contains("+alpha"), "drawn as a new file: {diff}");
    assert!(!diff.contains("+gamma"), "drawn as a new file: {diff}");

    // And the document view of that file reads its base side too.
    let base_side = env
        .daemon
        .review_file(review.id, "sent:1", "src/layout.rs", "new")
        .await
        .unwrap()
        .content;
    assert_eq!(base_side.as_deref(), Some(&b"alpha\nbeta\ngamma\n"[..]));
}

/// The mirror of the same gap: a file that stops differing drops out of
/// the head capture, and an absent right-hand side would draw it as
/// deleted.
#[tokio::test]
async fn a_file_whose_edit_is_reverted_is_not_drawn_as_deleted() {
    let (env, root) = repo_env();
    write(
        &root,
        "src/layout.rs",
        "alpha
beta
gamma
",
    );
    git(&["add", "-A"], &root);
    git(&["commit", "-qm", "second file"], &root);
    let session = spawn_test_session(&env, "p");
    write(
        &root,
        "src/lib.rs",
        "one
TWO
three
four
",
    );
    write(
        &root,
        "src/layout.rs",
        "alpha
BETA
gamma
",
    );
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    write(
        &root,
        "src/layout.rs",
        "alpha
beta
gamma
",
    );

    let diff = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap()
        .content;
    assert!(!diff.contains("-alpha"), "drawn as deleted: {diff}");
    assert!(!diff.contains("src/layout.rs"), "still in the diff: {diff}");
    assert!(diff.contains("+TWO"), "{diff}");
}

/// A base side that cannot be read is the one case the fill above
/// cannot answer, and it must not come out looking like a new file.
#[tokio::test]
async fn a_base_side_that_cannot_be_read_is_reported_rather_than_drawn_as_new() {
    let (env, root) = repo_env();
    write(&root, "src/layout.rs", "alpha\nbeta\ngamma\n");
    git(&["add", "-A"], &root);
    git(&["commit", "-qm", "second file"], &root);
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    // The file enters the diff after open, and its base content is no
    // longer in the object store.
    let sha = String::from_utf8_lossy(
        &Command::new("git")
            .args(["rev-parse", &format!("{}:src/layout.rs", review.base)])
            .current_dir(&root)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    let (dir, rest) = sha.split_at(2);
    std::fs::remove_file(root.join(".git/objects").join(dir).join(rest)).unwrap();
    write(&root, "src/layout.rs", "alpha\nBETA\ngamma\n");

    let diff = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap()
        .content;
    assert!(diff.contains("Unreadable: base side"), "{diff}");
    assert!(!diff.contains("+alpha"), "invented a new file: {diff}");

    // And the page is told which file it is not showing, and why.
    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let named = detail
        .skipped
        .iter()
        .find(|(path, _)| path == "src/layout.rs")
        .unwrap_or_else(|| panic!("not named: {:?}", detail.skipped));
    assert!(named.1.contains("base"), "{}", named.1);
}

#[tokio::test]
async fn untracked_files_are_reviewed_but_oversized_ones_are_skipped_with_a_reason() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/new_file.rs", "brand new\n");
    write(
        &root,
        "huge.bin",
        &"x".repeat(pm_daemon::review_store::UNTRACKED_MAX_BYTES as usize + 1),
    );
    write(&root, "binary.dat", "before\u{0}after");

    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();

    // New work is visible rather than silently missing.
    assert!(detail.files.contains(&"src/new_file.rs".to_string()));
    // The two that would flood the review are named, with the reason.
    let skipped: Vec<&str> = detail.skipped.iter().map(|(p, _)| p.as_str()).collect();
    assert!(skipped.contains(&"huge.bin"), "{skipped:?}");
    assert!(skipped.contains(&"binary.dat"), "{skipped:?}");
    let huge = detail
        .skipped
        .iter()
        .find(|(p, _)| p == "huge.bin")
        .unwrap();
    assert!(huge.1.contains("exceeds"), "{}", huge.1);
}

#[tokio::test]
async fn a_file_outside_any_repository_is_reviewed_against_an_empty_baseline() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "p");
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("plan.md");
    std::fs::write(&doc, "# Plan\n\nFirst draft.\n").unwrap();

    let review = env
        .daemon
        .open_review(
            session,
            &ReviewContext {
                source_file: doc.display().to_string(),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(review.mode, pm_protocol::domain::ReviewMode::File);
    assert!(review.range_key.starts_with("file:"));

    // The baseline is empty, so the whole document reads as added.
    let diff = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap()
        .content;
    assert!(diff.contains("+# Plan"), "{diff}");

    // Edits to the real path show up without any repository involved.
    std::fs::write(&doc, "# Plan\n\nSecond draft.\n").unwrap();
    let diff = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap()
        .content;
    assert!(diff.contains("+Second draft."), "{diff}");
}

#[tokio::test]
async fn viewer_state_persists_and_a_partial_update_keeps_what_it_omits() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    env.daemon
        .set_review_viewer_state(
            7,
            &ReviewViewerStateUpdate {
                review_id: review.id,
                viewed_files: Some(vec!["src/lib.rs".into()]),
                layout: Some("unified".into()),
                scroll_key: Some("::10".into()),
                scroll_top: Some(420),
                ..Default::default()
            },
        )
        .unwrap();

    // A later update that mentions only the context must not blank the
    // reader's viewed set or their scroll position.
    let state = env
        .daemon
        .set_review_viewer_state(
            7,
            &ReviewViewerStateUpdate {
                review_id: review.id,
                context: Some(20),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(state.context, 20);
    assert_eq!(state.layout, "unified");
    assert_eq!(state.viewed_files, vec!["src/lib.rs".to_string()]);
    assert_eq!(state.scroll.get("::10"), Some(&420));

    // And it is per user, so another reader starts clean.
    let other = env.daemon.review_detail(review.id, 8).await.unwrap();
    assert!(other.viewer.viewed_files.is_empty());
}

#[tokio::test]
async fn a_reader_stays_pinned_while_the_agent_works_and_is_told_what_is_waiting() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "fix it".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    write(&root, "src/lib.rs", "one\nFIXED\nthree\nfour\nfive\n");
    env.daemon
        .post_review_reply(session, thread, "fixed", true)
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    // The reader has not advanced, so a revision is waiting and the
    // page can say which files it touched.
    assert_eq!(detail.latest_rev, 1);
    assert_eq!(detail.pending_files, vec!["src/lib.rs".to_string()]);

    env.daemon.advance_review(review.id, 1, 0).await.unwrap();
    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    assert_eq!(detail.viewer.pinned_rev, 1);
}

#[tokio::test]
async fn finishing_a_review_closes_it_and_releases_its_claims() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    env.daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 1,
            side: ReviewSide::Right,
            excerpt: "one".into(),
            body: "look".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    let finished = env.daemon.finish_review(review.id).await.unwrap();
    assert_eq!(finished.state, pm_protocol::domain::ReviewState::Finished);
    // A finished review is not offered to the agent any more.
    assert!(env.daemon.list_reviews(session, false).unwrap().is_empty());
    assert_eq!(env.daemon.list_reviews(session, true).unwrap().len(), 1);
}

// ---- the surfaces an agent and a browser actually call ----

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::util::ServiceExt;

async fn rpc(app: &axum::Router, token: &str, body: Value) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn call(name: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})
}

/// The text an agent reads back out of a tool result.
fn tool_text(body: &Value) -> String {
    body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn an_agent_drives_a_whole_review_over_mcp() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");

    let (status, body) = rpc(
        &app,
        &token,
        call(
            "open_review",
            json!({"worktree": root.display().to_string(), "base": head_sha(&root)}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = tool_text(&body);
    // The agent is told to hand the user a URL, not an id.
    assert!(text.contains("/#/review/"), "{text}");

    // Nothing is waiting yet, so the call holds rather than reporting
    // an empty queue the agent would wander off from. A short wait
    // keeps the test quick; the agent is told to call straight back.
    let (_, body) = rpc(
        &app,
        &token,
        call("next_review_event", json!({"wait_seconds": 1})),
    )
    .await;
    assert_eq!(body["result"]["isError"], false);
    assert!(
        tool_text(&body).contains("call \n                         next_review_event again")
            || tool_text(&body).contains("again"),
        "{body}"
    );

    let review = env.daemon.list_reviews(session, false).unwrap().remove(0);
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "rename this".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    let (_, body) = rpc(&app, &token, call("next_review_event", json!({}))).await;
    let text = tool_text(&body);
    assert!(text.contains("rename this"), "{text}");
    assert!(text.contains("src/lib.rs"), "{text}");
    // The agent is pointed at today's code and told where to reply.
    assert!(text.contains("current code:"), "{text}");
    assert!(text.contains(&format!("thread_id {thread}")), "{text}");

    write(&root, "src/lib.rs", "one\nRENAMED\nthree\nfour\nfive\n");
    let (_, body) = rpc(
        &app,
        &token,
        call(
            "post_review_reply",
            json!({"thread_id": thread, "body": "renamed", "addressed": true}),
        ),
    )
    .await;
    assert_eq!(body["result"]["isError"], false, "{body}");

    let (_, body) = rpc(&app, &token, call("review_status", json!({}))).await;
    assert!(tool_text(&body).contains("1 answered"), "{body}");
}

#[tokio::test]
async fn an_agent_cannot_freeze_a_review_at_a_head_sha() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    // A frozen head reads blobs at that commit, so every edit the agent
    // made answering a comment would land somewhere the review never
    // looks and no reply could ever record a revision.
    let (_, body) = rpc(
        &app,
        &token,
        call(
            "open_review",
            json!({
                "worktree": root.display().to_string(),
                "base": head_sha(&root),
                "head": head_sha(&root),
            }),
        ),
    )
    .await;
    assert_eq!(body["result"]["isError"], true, "{body}");
    let text = tool_text(&body);
    assert!(text.contains("`head` must be empty here"), "{text}");
    assert!(text.contains("pm review open --head"), "{text}");

    // Refused before anything was created, so the agent is not left
    // with a review it cannot use.
    assert!(env.daemon.list_reviews(session, true).unwrap().is_empty());
}

#[tokio::test]
async fn an_agent_cannot_open_a_review_on_an_unresolved_base() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let (_, body) = rpc(
        &app,
        &token,
        call(
            "open_review",
            json!({"worktree": root.display().to_string(), "base": "HEAD~1"}),
        ),
    )
    .await;
    assert_eq!(body["result"]["isError"], true, "{body}");
    // The agent is handed the command that resolves it.
    assert!(tool_text(&body).contains("rev-parse HEAD~1"), "{body}");
    assert!(env.daemon.list_reviews(session, true).unwrap().is_empty());
}

#[tokio::test]
async fn an_agent_cannot_reply_to_a_thread_that_does_not_exist() {
    let (env, _root) = repo_env();
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let (_, body) = rpc(
        &app,
        &token,
        call("post_review_reply", json!({"thread_id": 9999, "body": "x"})),
    )
    .await;
    assert_eq!(body["result"]["isError"], true, "{body}");

    // And an empty reply is refused rather than stored as a blank one.
    let (_, body) = rpc(
        &app,
        &token,
        call("post_review_reply", json!({"thread_id": 1, "body": "   "})),
    )
    .await;
    assert_eq!(body["result"]["isError"], true, "{body}");
}

#[tokio::test]
async fn the_browser_reads_a_review_only_when_signed_in() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let unauthenticated = app
        .clone()
        .oneshot(
            Request::get(format!("/api/reviews/{}", review.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let signed_in = env.daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = support::dashboard_bearer(&env.daemon, &signed_in);

    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/api/reviews/{}", review.id))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    let detail: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(detail["review"]["id"], review.id);
    assert_eq!(detail["files"][0], "src/lib.rs");

    // The diff is plain text, because the client parses it as a diff.
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/api/reviews/{}/diff?context=3", review.id))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // The snapshot this render came from rides in a header, so a
    // comment written against it can name what the reader was shown
    // without the body changing shape.
    let diff_snapshot = res
        .headers()
        .get("x-review-snapshot")
        .expect("the diff names its snapshot")
        .to_str()
        .unwrap()
        .to_string();
    assert!(diff_snapshot.parse::<u64>().unwrap() > 0);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    let diff = String::from_utf8_lossy(&bytes);
    assert!(diff.contains("+five"), "{diff}");

    // A whole file is served for the markdown preview.
    let res = app
        .oneshot(
            Request::get(format!("/api/reviews/{}/file?file=src/lib.rs", review.id))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()
            .get("x-review-snapshot")
            .expect("the file names its snapshot")
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 0
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("five"));
}

/// A review on a remote session reads that worker's tree through the
/// same encoded op the controller uses for its own, so the two cannot
/// behave differently. Here the worker is not connected, which must be
/// reported plainly rather than silently falling back to a local read
/// of whatever the controller has at that path.
#[tokio::test]
async fn a_review_on_an_unreachable_worker_reports_it_rather_than_reading_locally() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    env.daemon
        .set_session_worker_for_test(session, 42)
        .expect("session moves to a worker");

    let err = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .expect_err("an unreachable worker cannot serve a review");
    let message = err.to_string();
    // The controller has this exact path and it is a real repository,
    // so a local fallback would have returned a convincing diff of the
    // wrong machine's work.
    assert!(!message.is_empty(), "{message}");
}

// ---- the same review, served by a worker ----

/// Stands in for a remote worker: takes the encoded ops the controller
/// sends, runs the very same reader a worker would, and answers.
///
/// The point of the test is that this loop contains no review logic at
/// all. If a remote review behaved differently from a local one, it
/// would have to be because the two took different code paths, and they
/// do not.
fn serve_worker(
    daemon: std::sync::Arc<pm_daemon::Daemon>,
    worker_id: u64,
    mut rx: tokio::sync::mpsc::Receiver<pm_protocol::domain::ControllerMsg>,
) -> (
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&served);
    let handle = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let pm_protocol::domain::ControllerMsg::RepoRequest { req_id, op } = msg {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let answered = match pm_daemon::review_repo::handle_encoded(&op) {
                    Ok(answer) => pm_protocol::domain::WorkerMsg::RepoResponse {
                        req_id,
                        ok: true,
                        error: String::new(),
                        answer,
                    },
                    Err(error) => pm_protocol::domain::WorkerMsg::RepoResponse {
                        req_id,
                        ok: false,
                        error,
                        answer: Vec::new(),
                    },
                };
                daemon.apply_worker_message(worker_id, answered);
            }
        }
    });
    (handle, served)
}

#[tokio::test]
async fn a_review_on_a_worker_reads_that_worker_and_matches_a_local_one() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    write(&root, "src/new.rs", "untracked\n");

    // The same review, read locally.
    let local = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let local_detail = env.daemon.review_detail(local.id, 1).await.unwrap();
    let local_diff = env
        .daemon
        .review_diff(local.id, "", 3, None)
        .await
        .unwrap()
        .content;

    // Now move the session onto a worker and connect one that serves
    // the reader. Nothing else about the review changes.
    let (token, _) = env.daemon.create_worker_enrollment("remote").unwrap();
    let registration = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-remote",
            hostname: "remote",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    let worker_id = registration.worker_id;
    let (pump, served) = serve_worker(
        std::sync::Arc::clone(&env.daemon),
        worker_id,
        registration.rx,
    );
    env.daemon
        .set_session_worker_for_test(session, worker_id)
        .unwrap();

    let remote = env
        .daemon
        .open_review(
            session,
            &ReviewContext {
                // A different range key, so this is its own review
                // rather than the local one reattached.
                label: "remote".into(),
                pathspec: vec!["src".into()],
                ..ctx(&root)
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(remote.worker_id, worker_id);

    let remote_detail = env.daemon.review_detail(remote.id, 1).await.unwrap();
    let remote_diff = env
        .daemon
        .review_diff(remote.id, "", 3, None)
        .await
        .unwrap()
        .content;

    // The worker actually served the reads, rather than the controller
    // quietly falling back to its own filesystem.
    pump.abort();
    assert!(
        served.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "the worker was never asked to read anything"
    );

    // Same files, same diff, from the same reader.
    assert_eq!(local_detail.files, remote_detail.files);
    assert_eq!(local_diff, remote_diff);
    assert!(local_diff.contains("+five"), "{local_diff}");
    assert!(local_diff.contains("+untracked"), "{local_diff}");
}

#[tokio::test]
async fn a_worker_that_cannot_read_the_tree_reports_it_rather_than_answering_emptily() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");

    let (token, _) = env.daemon.create_worker_enrollment("remote").unwrap();
    let registration = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-remote",
            hostname: "remote",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    let worker_id = registration.worker_id;
    let (_pump, _served) = serve_worker(
        std::sync::Arc::clone(&env.daemon),
        worker_id,
        registration.rx,
    );
    env.daemon
        .set_session_worker_for_test(session, worker_id)
        .unwrap();

    let err = env
        .daemon
        .open_review(
            session,
            &ReviewContext {
                worktree: "/no/such/tree".into(),
                ..ctx(&root)
            },
            false,
        )
        .await
        .expect_err("a tree the worker cannot open is not a review");
    assert!(err.to_string().contains("/no/such/tree"), "{err}");
}

/// A worker on an older build would never understand the read, so the
/// refusal names the version to update to instead of leaving the
/// reviewer waiting out a timeout.
#[tokio::test]
async fn a_worker_that_predates_the_reader_is_refused_with_the_version_it_needs() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");

    let (token, _) = env.daemon.create_worker_enrollment("old").unwrap();
    let registration = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-old",
            hostname: "old",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_REVIEW_REPO - 1,
        })
        .unwrap();
    let worker_id = registration.worker_id;
    // Deliberately no pump: an older worker would not answer anyway.
    env.daemon
        .set_session_worker_for_test(session, worker_id)
        .unwrap();

    let err = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .expect_err("an older worker cannot serve a review");
    let message = err.to_string();
    assert!(message.contains("older Puppet Master"), "{message}");
    assert!(
        message.contains(&pm_protocol::WORKER_PROTOCOL_REVIEW_REPO.to_string()),
        "{message}"
    );
}

#[tokio::test]
async fn a_file_review_derives_its_worktree_from_the_document() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "p");
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("plan.md");
    std::fs::write(&doc, "# Plan\n").unwrap();

    // No worktree given: the only sensible one is the directory holding
    // the document, and making the caller know that produced empty
    // reviews when they passed a parent instead.
    let review = env
        .daemon
        .open_review(
            session,
            &ReviewContext {
                source_file: doc.display().to_string(),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(review.worktree, dir.path().display().to_string());
    let diff = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap()
        .content;
    assert!(diff.contains("+# Plan"), "{diff}");
}

#[tokio::test]
async fn a_file_review_pointed_at_the_wrong_directory_is_refused_not_left_blank() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "p");
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("docs")).unwrap();
    let doc = dir.path().join("docs/plan.md");
    std::fs::write(&doc, "# Plan\n").unwrap();

    // The parent of the document's directory. A file review reads
    // <worktree>/<file name>, so this looks for <dir>/plan.md, which is
    // not there.
    let err = env
        .daemon
        .open_review(
            session,
            &ReviewContext {
                worktree: dir.path().display().to_string(),
                source_file: doc.display().to_string(),
                ..Default::default()
            },
            false,
        )
        .await
        .expect_err("a document that is not where the review reads is not a review");
    let message = err.to_string();
    assert!(message.contains("not found"), "{message}");
    // The message names where it actually looked, which is the thing
    // the caller got wrong.
    assert!(message.contains("plan.md"), "{message}");
}

#[tokio::test]
async fn waiting_returns_the_moment_a_comment_is_sent() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    // The agent parks here first, before any comment exists.
    let daemon = std::sync::Arc::clone(&env.daemon);
    let waiting = tokio::spawn(async move {
        daemon
            .await_review_event(session, std::time::Duration::from_secs(10))
            .await
    });

    // Then the reviewer sends one.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    env.daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "look at this".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), waiting)
        .await
        .expect("the wait returned rather than hanging")
        .unwrap()
        .unwrap();
    match outcome {
        pm_daemon::review::ReviewWait::Event(event) => {
            assert_eq!(event.thread.path, "src/lib.rs");
            assert_eq!(event.thread.messages[0].body, "look at this");
        }
        other => panic!("expected the comment, got {other:?}"),
    }
}

#[tokio::test]
async fn waiting_ends_when_the_review_is_finished_rather_than_hanging() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    env.daemon.finish_review(review.id).await.unwrap();

    // Nothing is ever coming, so the agent is released instead of
    // parked on a review that is over.
    let outcome = env
        .daemon
        .await_review_event(session, std::time::Duration::from_secs(10))
        .await
        .unwrap();
    assert!(matches!(outcome, pm_daemon::review::ReviewWait::Finished));
}

#[tokio::test]
async fn waiting_times_out_while_the_review_is_still_open() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    env.daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    // An open review with nothing waiting reports a timeout, which the
    // agent is told to answer by calling straight back.
    let outcome = env
        .daemon
        .await_review_event(session, std::time::Duration::from_millis(400))
        .await
        .unwrap();
    assert!(matches!(outcome, pm_daemon::review::ReviewWait::TimedOut));
}

#[tokio::test]
async fn an_unsent_draft_survives_leaving_the_page() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    let key = "line:src/lib.rs:2:right";
    env.daemon
        .set_review_viewer_state(
            7,
            &ReviewViewerStateUpdate {
                review_id: review.id,
                draft_key: Some(key.into()),
                draft_body: Some("half-written thought".into()),
                ..Default::default()
            },
        )
        .unwrap();

    // Coming back finds the text where it was left, rather than an
    // empty box.
    let detail = env.daemon.review_detail(review.id, 7).await.unwrap();
    assert_eq!(
        detail.viewer.drafts.get(key).map(String::as_str),
        Some("half-written thought")
    );

    // Another update must not disturb it.
    env.daemon
        .set_review_viewer_state(
            7,
            &ReviewViewerStateUpdate {
                review_id: review.id,
                context: Some(20),
                ..Default::default()
            },
        )
        .unwrap();
    let detail = env.daemon.review_detail(review.id, 7).await.unwrap();
    assert_eq!(detail.viewer.drafts.len(), 1);

    // Submitting clears the slot rather than leaving a stale copy.
    let state = env
        .daemon
        .set_review_viewer_state(
            7,
            &ReviewViewerStateUpdate {
                review_id: review.id,
                draft_key: Some(key.into()),
                draft_body: Some(String::new()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(state.drafts.is_empty());
}

#[tokio::test]
async fn a_reply_that_changed_nothing_does_not_advertise_an_empty_revision() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "why is this here?".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();

    // Answering a question edits nothing.
    env.daemon
        .post_review_reply(session, thread, "it is the second line", true)
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    // No revision to advance to, so the page offers none.
    assert_eq!(detail.latest_rev, 0, "{:?}", detail.revisions);
    assert!(detail.pending_files.is_empty());
    // And the reply carries no changes link that would open an empty diff.
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    let reply = t.messages.last().unwrap();
    assert_eq!(reply.changes_rev, 0);
    assert!(reply.changed_files.is_empty());
}

#[tokio::test]
async fn finishing_a_review_takes_it_out_of_the_live_list_not_just_its_state() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    let mut events = env.daemon.subscribe().1;
    env.daemon.finish_review(review.id).await.unwrap();

    // A finished review must leave every connected client, not merely
    // change state: the snapshot carries open reviews only, so a
    // change event would leave the tab up until the page reloaded.
    let mut removed = false;
    while let Ok(event) = events.try_recv() {
        if matches!(event, pm_protocol::domain::Event::ReviewRemoved(id) if id == review.id) {
            removed = true;
        }
        assert!(
            !matches!(&event, pm_protocol::domain::Event::ReviewChanged(r) if r.id == review.id),
            "a finished review was published as a change, so clients would keep it"
        );
    }
    assert!(removed, "finishing published no removal");
    assert!(env.daemon.list_reviews(session, false).unwrap().is_empty());
}

#[tokio::test]
async fn seen_marks_only_ever_move_forward() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    let mark = |thread: u64, message: u64| ReviewViewerStateUpdate {
        review_id: review.id,
        seen_thread: Some(thread),
        seen_message: Some(message),
        ..Default::default()
    };
    env.daemon.set_review_viewer_state(7, &mark(1, 9)).unwrap();
    // A thread scrolling back into view must not un-see a newer reply
    // that arrived while the reader was further down.
    let state = env.daemon.set_review_viewer_state(7, &mark(1, 4)).unwrap();
    assert_eq!(state.seen.get(&1), Some(&9));

    let state = env.daemon.set_review_viewer_state(7, &mark(1, 12)).unwrap();
    assert_eq!(state.seen.get(&1), Some(&12));
    // And it is per reader.
    let other = env.daemon.review_detail(review.id, 8).await.unwrap();
    assert!(other.viewer.seen.is_empty());
}

#[tokio::test]
async fn a_review_reports_the_newest_message_on_each_thread() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\nfive\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 2,
            side: ReviewSide::Right,
            excerpt: "two".into(),
            body: "look".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    // This is what lets a client tell unseen from seen without pulling
    // the whole review, and it says what exists rather than who read it.
    let opened = env.daemon.get_review_for_test(review.id);
    let first = *opened.thread_latest_message.get(&thread).unwrap();
    assert!(first > 0);

    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    env.daemon
        .post_review_reply(session, thread, "done", true)
        .await
        .unwrap();
    let answered = env.daemon.get_review_for_test(review.id);
    assert!(
        *answered.thread_latest_message.get(&thread).unwrap() > first,
        "a reply did not move the thread's newest message"
    );
}

/// Two rounds, one sent straight from the comment and one drafted first,
/// because those are separate paths into `send_review_threads` and only
/// the second captures the round's Sent snapshot there. A reader flipping
/// between revisions has to see each round's own work.
#[tokio::test]
async fn each_round_view_shows_only_that_round_across_two_rounds() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    // Round 1: the comment carries send: true, so it dispatches directly.
    let first = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 1,
            side: ReviewSide::Right,
            excerpt: "one".into(),
            body: "rename the first line".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    write(&root, "src/lib.rs", "ROUND_ONE\ntwo\nthree\nfour\n");
    env.daemon
        .post_review_reply(session, first, "renamed the first line", true)
        .await
        .unwrap();

    // Round 2: drafted, then dispatched by sending the drafts.
    let second = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 3,
            side: ReviewSide::Right,
            excerpt: "three".into(),
            body: "rename the third line".into(),
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
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .unwrap();
    write(&root, "src/lib.rs", "ROUND_ONE\ntwo\nROUND_TWO\nfour\n");
    env.daemon
        .post_review_reply(session, second, "renamed the third line", true)
        .await
        .unwrap();

    let diff = |view: &'static str| {
        let daemon = env.daemon.clone();
        let id = review.id;
        async move { daemon.review_diff(id, view, 3, None).await.unwrap().content }
    };

    // Each round names its own edit and not the other's, which is the
    // whole point of being able to switch between them.
    let round1 = diff("round:1").await;
    assert!(
        round1.contains("+ROUND_ONE"),
        "round:1 missing its own edit: {round1}"
    );
    assert!(
        !round1.contains("+ROUND_TWO"),
        "round:1 claims round 2's edit: {round1}"
    );

    let round2 = diff("round:2").await;
    assert!(
        round2.contains("+ROUND_TWO"),
        "round:2 missing its own edit: {round2}"
    );
    assert!(
        !round2.contains("+ROUND_ONE"),
        "round:2 claims round 1's edit: {round2}"
    );

    // Switching revisions has to actually change what is rendered.
    assert_ne!(round1, round2, "the two rounds render identically");

    // Round to round, and from the base.
    let delta2 = diff("delta:2").await;
    assert!(
        delta2.contains("+ROUND_TWO"),
        "delta:2 missing round 2: {delta2}"
    );
    assert!(
        !delta2.contains("+ROUND_ONE"),
        "delta:2 reaches back past round 1: {delta2}"
    );

    let cum2 = diff("cum:2").await;
    assert!(cum2.contains("+ROUND_ONE"), "cum:2 missing round 1: {cum2}");
    assert!(cum2.contains("+ROUND_TWO"), "cum:2 missing round 2: {cum2}");
}

/// The reader's own pass down a file: several comments written against the
/// tree as it stands, then released one at a time, with the agent editing
/// between them. Each edit shifts the lines the later comments name, so the
/// question is whether a comment still points at the code it was written
/// about by the time the agent claims it.
#[tokio::test]
async fn a_comment_still_names_its_own_code_after_earlier_rounds_shifted_the_file() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(
        &root,
        "src/lib.rs",
        "alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\n",
    );
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    // Read the file top to bottom and comment as you go. All three are
    // written against the same tree, before the agent has touched anything.
    let mut threads = Vec::new();
    for (line, excerpt, body) in [
        (2u32, "bravo", "rename bravo"),
        (4, "delta", "rename delta"),
        (6, "foxtrot", "rename foxtrot"),
    ] {
        threads.push(
            env.daemon
                .add_review_comment(&pm_daemon::review::NewComment {
                    review_id: review.id,
                    path: "src/lib.rs".into(),
                    line,
                    side: ReviewSide::Right,
                    excerpt: excerpt.into(),
                    body: body.into(),
                    send: false,
                    anchor_snapshot_id: None,
                    choice: None,
                })
                .await
                .unwrap(),
        );
    }
    env.daemon
        .send_review_threads(review.id, &[])
        .await
        .unwrap();

    // The agent works them one at a time. Each fix inserts a line, so every
    // later comment's line number is stale by one more than the last.
    let expected = [("bravo", 2u32), ("delta", 5), ("foxtrot", 8)];
    let mut body = vec!["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
    for (i, (word, want_line)) in expected.iter().enumerate() {
        let event = env
            .daemon
            .next_review_event(session)
            .await
            .unwrap()
            .expect("an event is queued");

        // The line the agent is handed has to be where that word actually
        // lives now, not where it lived when the comment was written.
        let at = body
            .iter()
            .position(|w| w == word)
            .expect("the commented word is still in the file") as u32
            + 1;
        assert_eq!(
            event.thread.current_line,
            at,
            "comment on {word} pointed at line {} but {word} is on line {at}\nfile now:\n{}",
            event.thread.current_line,
            body.join("\n"),
        );
        assert_eq!(event.thread.current_line, *want_line, "on {word}");
        assert!(
            event.thread.current_excerpt.contains(word),
            "excerpt for {word} does not contain it: {}",
            event.thread.current_excerpt,
        );

        // Fix it the way the comment asked, and add a line above while we
        // are in there, which is what shifts everything below.
        let at_idx = at as usize - 1;
        body[at_idx] = match *word {
            "bravo" => "BRAVO",
            "delta" => "DELTA",
            _ => "FOXTROT",
        };
        body.insert(at_idx, "// note");
        write(&root, "src/lib.rs", &format!("{}\n", body.join("\n")));
        env.daemon
            .post_review_reply(session, threads[i], "done", true)
            .await
            .unwrap();
    }
}

/// The bug this covers: the reviewer's page renders one revision, the
/// agent edits underneath it, and the comment that follows names a line
/// counted against the render the reader still has on screen. Anchoring
/// that comment to the tree as it stands when it arrives points it at
/// whatever moved into that line since.
#[tokio::test]
async fn a_comment_anchors_to_the_render_the_reader_saw_rather_than_the_tree_it_arrives_at() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "alpha\nbravo\ncharlie\ndelta\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    // What the reader is looking at, and the snapshot it was rendered
    // from. On their page `delta` is line 4.
    let rendered = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap();
    assert!(rendered.content.contains("+delta"), "{}", rendered.content);
    let seen = rendered
        .snapshot_id
        .expect("the live view names its snapshot");

    // The agent inserts a line while the reader is still reading, so
    // `delta` is now line 5 and line 4 holds `charlie`.
    write(
        &root,
        "src/lib.rs",
        "alpha\n// note\nbravo\ncharlie\ndelta\n",
    );

    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 4,
            side: ReviewSide::Right,
            excerpt: "delta".into(),
            body: "rename delta".into(),
            send: true,
            anchor_snapshot_id: Some(seen),
            choice: None,
        })
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.anchor_snapshot_id, seen);
    assert_eq!(
        t.current_line, 5,
        "the comment on delta points at line {} instead: {}",
        t.current_line, t.current_excerpt,
    );
    assert!(t.current_excerpt.contains("delta"), "{}", t.current_excerpt);
    // The line moved under the reader, and the agent is told so rather
    // than being handed a line that merely looks settled.
    assert_eq!(t.anchor_status, ReviewAnchorStatus::Moved);

    let event = env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .expect("an event is queued");
    assert_eq!(event.thread.current_line, 5);
    assert!(event.thread.current_excerpt.contains("delta"));
}

/// An agent, or a client too old to send one, supplies no snapshot. Its
/// line counts against the tree as it stands, so that is what the
/// comment anchors to, exactly as before.
#[tokio::test]
async fn a_comment_without_a_snapshot_still_anchors_to_the_tree_it_arrives_at() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "alpha\nbravo\ncharlie\ndelta\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    write(
        &root,
        "src/lib.rs",
        "alpha\n// note\nbravo\ncharlie\ndelta\n",
    );

    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 4,
            side: ReviewSide::Right,
            excerpt: "charlie".into(),
            body: "rename charlie".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.current_line, 4);
    assert!(
        t.current_excerpt.contains("charlie"),
        "{}",
        t.current_excerpt
    );
    assert_eq!(t.anchor_status, ReviewAnchorStatus::Same);
}

/// A supplied id is a claim, not a fact. One from another review, one
/// that never existed, and one whose snapshot does not hold the
/// commented file are all refused, and the comment anchors to a fresh
/// capture instead of into a snapshot describing something else.
#[tokio::test]
async fn an_anchor_snapshot_from_elsewhere_is_refused_rather_than_trusted() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "alpha\nbravo\ncharlie\ndelta\n");
    write(&root, "src/other.rs", "one\ntwo\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    // A second review over a narrower scope, whose snapshots are no
    // business of the first one's comments.
    let other = env
        .daemon
        .open_review(
            session,
            &ReviewContext {
                pathspec: vec!["src/other.rs".into()],
                ..ctx(&root)
            },
            false,
        )
        .await
        .unwrap();
    let foreign = env
        .daemon
        .review_diff(other.id, "", 3, None)
        .await
        .unwrap()
        .snapshot_id
        .expect("the live view names its snapshot");
    let own = env
        .daemon
        .review_diff(review.id, "", 3, None)
        .await
        .unwrap()
        .snapshot_id
        .expect("the live view names its snapshot");

    for supplied in [Some(foreign), Some(u64::MAX)] {
        let thread = env
            .daemon
            .add_review_comment(&pm_daemon::review::NewComment {
                review_id: review.id,
                path: "src/lib.rs".into(),
                line: 2,
                side: ReviewSide::Right,
                excerpt: "bravo".into(),
                body: "rename bravo".into(),
                send: false,
                anchor_snapshot_id: supplied,
                choice: None,
            })
            .await
            .unwrap();
        let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
        let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
        assert_ne!(Some(t.anchor_snapshot_id), supplied);
        assert!(t.anchor_snapshot_id > own, "expected a fresh capture");
        assert_eq!(t.current_line, 2);
        assert!(t.current_excerpt.contains("bravo"), "{}", t.current_excerpt);
    }

    // A snapshot of this review that does not hold the commented file
    // is no anchor either: it can say nothing about where that file's
    // lines have moved.
    let narrow = env
        .daemon
        .review_diff(other.id, "", 3, None)
        .await
        .unwrap()
        .snapshot_id
        .unwrap();
    assert!(env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: other.id,
            path: "src/lib.rs".into(),
            line: 1,
            side: ReviewSide::Right,
            excerpt: "alpha".into(),
            body: "out of scope".into(),
            send: false,
            anchor_snapshot_id: Some(narrow),
            choice: None,
        })
        .await
        .is_ok());
    let detail = env.daemon.review_detail(other.id, 1).await.unwrap();
    let t = detail.threads.last().unwrap();
    assert_ne!(t.anchor_snapshot_id, narrow);
}

/// A reader can be looking at an earlier round rather than the working
/// tree. That view has its own stored snapshot, and a comment written
/// against it anchors there, so its line still maps forward through
/// everything the agent has done since.
#[tokio::test]
async fn a_comment_on_a_round_view_anchors_to_that_round() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "alpha\nbravo\ncharlie\ndelta\n");
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    let first = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 1,
            side: ReviewSide::Right,
            excerpt: "alpha".into(),
            body: "rename alpha".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: None,
        })
        .await
        .unwrap();
    env.daemon
        .next_review_event(session)
        .await
        .unwrap()
        .expect("an event is queued");
    // The agent's round: it renames alpha and adds a line above it.
    write(
        &root,
        "src/lib.rs",
        "// head\nALPHA\nbravo\ncharlie\ndelta\n",
    );
    env.daemon
        .post_review_reply(session, first, "renamed", true)
        .await
        .unwrap();

    // The reader opens what that round produced. On this page `delta`
    // is line 5.
    let round = env
        .daemon
        .review_diff(review.id, "cum:1", 3, None)
        .await
        .unwrap();
    assert!(round.content.contains("+ALPHA"), "{}", round.content);
    let seen = round.snapshot_id.expect("a round view names its snapshot");

    // The agent keeps working while they read it.
    write(
        &root,
        "src/lib.rs",
        "// head\n// more\nALPHA\nbravo\ncharlie\ndelta\n",
    );

    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "src/lib.rs".into(),
            line: 5,
            side: ReviewSide::Right,
            excerpt: "delta".into(),
            body: "rename delta".into(),
            send: false,
            anchor_snapshot_id: Some(seen),
            choice: None,
        })
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.anchor_snapshot_id, seen);
    assert_eq!(t.current_line, 6, "excerpt: {}", t.current_excerpt);
    assert!(t.current_excerpt.contains("delta"), "{}", t.current_excerpt);
}

/// An answer to a marked option list is data, not prose: it survives
/// the round trip as fields, so the agent reads the decision instead of
/// recovering it from a sentence.
#[tokio::test]
async fn an_answer_to_a_choice_list_reaches_the_agent_as_fields() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(
        &root,
        "plan.md",
        "# Auth\n\n<!-- pm-choice id=auth-approach select=one -->\n- [ ] JWT\n- [ ] Sessions\n",
    );
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    let answer = ReviewChoiceAnswer {
        choice_id: "auth-approach".into(),
        select: ReviewChoiceSelect::One,
        option_ids: vec!["sessions".into()],
        option_labels: vec!["Sessions".into()],
        other_text: String::new(),
        notes: "revocation matters more than statelessness".into(),
    };
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "plan.md".into(),
            line: 3,
            side: ReviewSide::Right,
            excerpt: "<!-- pm-choice id=auth-approach select=one -->".into(),
            body: "auth-approach: Sessions".into(),
            send: true,
            anchor_snapshot_id: None,
            choice: Some(answer.clone()),
        })
        .await
        .unwrap();

    let event = env
        .daemon
        .next_review_event(session)
        .await
        .unwrap()
        .expect("the answer is dispatched like any other comment");
    assert_eq!(event.thread.id, thread);
    let carried = event.thread.messages[0]
        .choice
        .as_ref()
        .expect("the answer travels with the message");
    assert_eq!(carried, &answer);
}

/// A reader changing their mind before sending leaves one answer, not
/// two for the agent to reconcile.
#[tokio::test]
async fn editing_an_unsent_answer_replaces_it() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(
        &root,
        "plan.md",
        "<!-- pm-choice id=a select=one -->\n- [ ] one\n- [ ] two\n",
    );
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();

    let first = ReviewChoiceAnswer {
        choice_id: "a".into(),
        select: ReviewChoiceSelect::One,
        option_ids: vec!["one".into()],
        option_labels: vec!["one".into()],
        other_text: String::new(),
        notes: String::new(),
    };
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "plan.md".into(),
            line: 1,
            side: ReviewSide::Right,
            excerpt: "choice".into(),
            body: "a: one".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: Some(first),
        })
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let message = detail
        .threads
        .iter()
        .find(|t| t.id == thread)
        .unwrap()
        .messages[0]
        .id;

    let second = ReviewChoiceAnswer {
        choice_id: "a".into(),
        select: ReviewChoiceSelect::One,
        option_ids: vec!["two".into()],
        option_labels: vec!["two".into()],
        other_text: String::new(),
        notes: "on reflection".into(),
    };
    env.daemon
        .edit_review_comment(message, "a: two", Some(&second))
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.messages.len(), 1);
    assert_eq!(t.messages[0].choice.as_ref(), Some(&second));
}

/// Clearing the answer on an edit leaves prose behind, not a decision
/// the reader has taken back.
#[tokio::test]
async fn editing_a_comment_without_an_answer_clears_the_one_it_had() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(
        &root,
        "plan.md",
        "<!-- pm-choice id=a select=one -->\n- [ ] one\n",
    );
    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .unwrap();
    let thread = env
        .daemon
        .add_review_comment(&pm_daemon::review::NewComment {
            review_id: review.id,
            path: "plan.md".into(),
            line: 1,
            side: ReviewSide::Right,
            excerpt: "choice".into(),
            body: "a: one".into(),
            send: false,
            anchor_snapshot_id: None,
            choice: Some(ReviewChoiceAnswer {
                choice_id: "a".into(),
                select: ReviewChoiceSelect::One,
                option_ids: vec!["one".into()],
                option_labels: vec!["one".into()],
                other_text: String::new(),
                notes: String::new(),
            }),
        })
        .await
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let message = detail
        .threads
        .iter()
        .find(|t| t.id == thread)
        .unwrap()
        .messages[0]
        .id;
    env.daemon
        .edit_review_comment(message, "never mind", None)
        .unwrap();

    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();
    let t = detail.threads.iter().find(|t| t.id == thread).unwrap();
    assert_eq!(t.messages[0].body, "never mind");
    assert!(t.messages[0].choice.is_none());
}

/// A review outlives the worktree it was opened against: the branch is
/// merged and the worktree removed while the review is still open in a
/// tab. Everything the reader needs was captured at open.
#[tokio::test]
async fn a_review_whose_worktree_is_gone_still_reads() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .expect("review opens");
    let before = env.daemon.review_detail(review.id, 1).await.unwrap();
    assert_eq!(before.files, vec!["src/lib.rs".to_string()]);
    assert!(!before.detached, "a live worktree is not detached");

    std::fs::remove_dir_all(&root).unwrap();

    let detail = env
        .daemon
        .review_detail(review.id, 1)
        .await
        .expect("a review with no worktree still reads");
    assert_eq!(detail.files, vec!["src/lib.rs".to_string()]);
    assert!(
        detail.detached,
        "the reader is told it is not the live tree"
    );
    assert_eq!(detail.threads.len(), before.threads.len());
}

#[tokio::test]
async fn review_serves_image_files_with_correct_content_type_and_sides() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let signed_in = env.daemon.auth_setup("testuser", "hunter2hunter2").unwrap();
    let auth = support::dashboard_bearer(&env.daemon, &signed_in);

    let png_bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";
    let svg_bytes = b"<svg width=\"10\" height=\"10\"><circle cx=\"5\" cy=\"5\" r=\"5\"/></svg>";
    std::fs::write(root.join("icon.png"), png_bytes).unwrap();
    std::fs::write(root.join("logo.svg"), svg_bytes).unwrap();

    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .expect("review opens");
    let detail = env.daemon.review_detail(review.id, 1).await.unwrap();

    assert!(detail.files.contains(&"icon.png".to_string()));
    assert!(detail.files.contains(&"logo.svg".to_string()));
    assert!(!detail
        .skipped
        .iter()
        .any(|(p, _)| p == "icon.png" || p == "logo.svg"));

    let res = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/reviews/{}/file?file=icon.png&side=new",
                review.id
            ))
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    // The bytes are a file somebody committed, so on any branch an agent
    // fetched or a contributor pushed they are attacker-influenced. An SVG
    // served as image/svg+xml runs its own script on top-level navigation,
    // which is how a reviewer looks at an image diff closely: open the image
    // in its own tab. A disposition makes that a download instead, and a
    // browser ignores it for the diff view's own <img>, so the view still
    // works. nosniff stops the same thing arriving by a guessed type.
    for header_name in [header::CONTENT_DISPOSITION, header::X_CONTENT_TYPE_OPTIONS] {
        assert!(
            res.headers().get(&header_name).is_some(),
            "a reviewed file is served without {header_name}"
        );
    }
    assert_eq!(
        res.headers().get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
        "nosniff"
    );
    assert!(res
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("attachment"));
    let body = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    assert_eq!(&body[..], &png_bytes[..]);

    let res = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/reviews/{}/file?file=icon.png&side=old",
                review.id
            ))
            .header(header::AUTHORIZATION, &auth)
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, TEST_ORIGIN)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let res = app
        .oneshot(
            Request::get(format!("/api/reviews/{}/file?file=logo.svg", review.id))
                .header(header::AUTHORIZATION, &auth)
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, TEST_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/svg+xml"
    );
    let body = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .unwrap();
    assert_eq!(&body[..], &svg_bytes[..]);
}

#[tokio::test]
async fn the_tree_check_names_what_moved_since_a_render_and_nothing_when_it_has_not() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .expect("review opens");
    env.daemon
        .review_diff(review.id, "", 3, None)
        .await
        .expect("the working tree renders");
    let rendered = env
        .daemon
        .latest_review_snapshot(review.id)
        .unwrap()
        .expect("a render captures the tree it drew");

    // A tree nobody touched has not moved, and saying otherwise would
    // ask every reader to refresh onto the diff they already have.
    assert!(env
        .daemon
        .review_tree_changes(review.id, rendered)
        .await
        .unwrap()
        .is_empty());

    // An edit and an untracked addition, which is what an agent working
    // alongside the reader leaves behind. Neither advances a revision.
    write(&root, "src/lib.rs", "one\nTWO\nTHREE\nfour\n");
    write(&root, "notes.md", "written after the render\n");
    assert_eq!(
        env.daemon
            .review_tree_changes(review.id, rendered)
            .await
            .unwrap(),
        vec!["notes.md".to_string(), "src/lib.rs".to_string()],
    );

    // A file that leaves the review is a change too: reverting the edit
    // drops src/lib.rs out of the diff entirely.
    std::fs::remove_file(root.join("notes.md")).unwrap();
    write(&root, "src/lib.rs", "one\ntwo\nthree\nfour\n");
    assert_eq!(
        env.daemon
            .review_tree_changes(review.id, rendered)
            .await
            .unwrap(),
        vec!["src/lib.rs".to_string()],
    );

    // Rendering again measures against the tree that render drew, so
    // what was reported once is not reported forever.
    env.daemon
        .review_diff(review.id, "", 3, None)
        .await
        .expect("the working tree renders");
    let latest = env
        .daemon
        .latest_review_snapshot(review.id)
        .unwrap()
        .expect("the second render captures too");
    assert_ne!(latest, rendered);
    assert!(env
        .daemon
        .review_tree_changes(review.id, latest)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_review_whose_worktree_is_gone_is_never_reported_as_having_moved() {
    let (env, root) = repo_env();
    let session = spawn_test_session(&env, "p");
    write(&root, "src/lib.rs", "one\nTWO\nthree\nfour\n");

    let review = env
        .daemon
        .open_review(session, &ctx(&root), false)
        .await
        .expect("review opens");
    env.daemon
        .review_diff(review.id, "", 3, None)
        .await
        .expect("the working tree renders");
    let rendered = env
        .daemon
        .latest_review_snapshot(review.id)
        .unwrap()
        .unwrap();

    // The branch merged and its worktree was removed. The review still
    // serves its last capture, so there is nothing newer to offer.
    std::fs::remove_dir_all(&root).unwrap();
    assert!(env
        .daemon
        .review_tree_changes(review.id, rendered)
        .await
        .unwrap()
        .is_empty());
}

/// A capture that fails, here because the tree is not a repository, used
/// to leave the review row open with nothing in it, and every later wait
/// on this session then reported a review still open after the user had
/// finished the real one.
#[tokio::test]
async fn a_review_whose_first_capture_fails_leaves_nothing_open() {
    let (env, _root) = repo_env();
    let session = spawn_test_session(&env, "p");
    let not_a_repo = tempfile::tempdir().unwrap();
    let context = ReviewContext {
        worktree: not_a_repo.path().display().to_string(),
        base: "0123456789abcdef0123456789abcdef01234567".into(),
        label: "doomed".into(),
        ..Default::default()
    };
    assert!(env
        .daemon
        .open_review(session, &context, false)
        .await
        .is_err());
    let waited = env
        .daemon
        .await_review_event(session, std::time::Duration::from_millis(50))
        .await
        .unwrap();
    assert!(
        matches!(waited, pm_daemon::review::ReviewWait::Finished),
        "{waited:?}"
    );
}
