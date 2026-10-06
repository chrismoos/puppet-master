//! MCP reporting: the JSON-RPC endpoint and the report semantics it
//! drives on sessions.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use pm_daemon::daemon::AgentReport;
use pm_daemon::storage::ITEM_BODY_MAX;
use pm_protocol::domain::{
    AgentKind, ContextField, ContextKind, ContextSeverity, ControllerMsg, HookKind, ItemWrite,
    PermissionMode, SessionContext, SessionRole, SessionState, WorkerMsg,
    ITEM_STATUS_CREATE_GUIDANCE, LOCAL_WORKER_ID,
};
use serde_json::{json, Value};
use support::*;
use tower::util::ServiceExt;

fn session_of(env: &TestEnv, id: u64) -> pm_protocol::domain::Session {
    env.daemon
        .subscribe()
        .0
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
}

fn context_of(env: &TestEnv, id: u64) -> SessionContext {
    env.daemon
        .subscribe()
        .0
        .contexts
        .into_iter()
        .find(|c| c.session_id == id)
        .unwrap_or_default()
}

fn field(key: &str, value: &str, kind: ContextKind) -> ContextField {
    ContextField {
        key: key.into(),
        label: key.into(),
        value: value.into(),
        kind,
        severity: ContextSeverity::Neutral,
    }
}

/// A report that only sets a headline, leaving every other bag untouched.
fn headline(text: &str) -> AgentReport {
    AgentReport::Report {
        goal: String::new(),
        headline: text.into(),
        summary: None,
        note: String::new(),
        glance: None,
        context: None,
        clear: Vec::new(),
        git: Default::default(),
    }
}

/// A report that sets only the git bag, leaving the headline empty so
/// the stored name is kept.
fn git_report(git: pm_daemon::storage::SessionGitUpdate) -> AgentReport {
    let git = Box::new(git);
    AgentReport::Report {
        goal: String::new(),
        headline: String::new(),
        summary: None,
        note: String::new(),
        glance: None,
        context: None,
        clear: Vec::new(),
        git,
    }
}

#[tokio::test]
async fn report_records_the_session_git_location() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    assert!(session_of(&env, id).git.is_none());

    env.daemon
        .handle_agent_report(
            &token,
            git_report(pm_daemon::storage::SessionGitUpdate {
                branch: Some("pm/session-branch".into()),
                worktree: Some("/repo/.worktrees/session-branch".into()),
                repo_root: Some("/repo".into()),
                commit: Some("4f2a91c".into()),
                upstream: Some("origin/master".into()),
                dirty: Some(true),
            }),
        )
        .unwrap();

    let git = session_of(&env, id).git.expect("git reported");
    assert_eq!(git.branch, "pm/session-branch");
    assert_eq!(git.worktree, "/repo/.worktrees/session-branch");
    assert_eq!(git.repo_root, "/repo");
    assert_eq!(git.commit, "4f2a91c");
    assert_eq!(git.upstream, "origin/master");
    assert_eq!(git.dirty, Some(true));
}

/// A checkout reports only the fields that moved, so the worktree and
/// repository reported earlier must survive it.
#[tokio::test]
async fn a_partial_git_report_keeps_the_fields_it_omits() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_agent_report(
            &token,
            git_report(pm_daemon::storage::SessionGitUpdate {
                branch: Some("master".into()),
                worktree: Some("/repo".into()),
                dirty: Some(false),
                ..Default::default()
            }),
        )
        .unwrap();
    env.daemon
        .handle_agent_report(
            &token,
            git_report(pm_daemon::storage::SessionGitUpdate {
                branch: Some("pm/next".into()),
                ..Default::default()
            }),
        )
        .unwrap();

    let git = session_of(&env, id).git.expect("git reported");
    assert_eq!(git.branch, "pm/next");
    assert_eq!(git.worktree, "/repo");
    assert_eq!(git.dirty, Some(false));
}

/// A report that mentions no git at all must not disturb what an
/// earlier report stored.
#[tokio::test]
async fn a_report_without_git_leaves_the_stored_location() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_agent_report(
            &token,
            git_report(pm_daemon::storage::SessionGitUpdate {
                branch: Some("pm/keep".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    env.daemon
        .handle_agent_report(&token, headline("still working"))
        .unwrap();

    let s = session_of(&env, id);
    assert_eq!(s.headline, "still working");
    assert_eq!(s.git.expect("git kept").branch, "pm/keep");
}

#[tokio::test]
async fn report_sets_the_headline_and_timeline() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    let note = env
        .daemon
        .handle_agent_report(
            &token,
            AgentReport::Report {
                goal: String::new(),
                headline: "migrating auth to JWT — 3/5 files".into(),
                summary: Some("swapping cookie sessions for signed tokens".into()),
                note: "tests passing".into(),
                glance: None,
                context: None,
                clear: Vec::new(),
                git: Default::default(),
            },
        )
        .unwrap();
    assert_eq!(note, None);
    let s = session_of(&env, id);
    assert_eq!(s.headline, "migrating auth to JWT — 3/5 files");
    assert_eq!(s.summary, "swapping cookie sessions for signed tokens");
    let reports = env.daemon.activity_reports(id).unwrap();
    assert!(reports
        .iter()
        .any(|r| r.kind == "checkpoint" && r.payload.contains("tests passing")));
}

#[tokio::test]
async fn report_unescapes_html_entities_in_all_reported_fields() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    let note = env
        .daemon
        .handle_agent_report(
            &token,
            AgentReport::Report {
                goal: "Auth &amp; Permissions".into(),
                headline: "Building &amp; testing &lt;div&gt; &apos;done&apos;".into(),
                summary: Some("Reviewed &quot;core&quot; &amp; &quot;cli&quot;".into()),
                note: "tests passing &amp; green".into(),
                glance: Some(vec![ContextField {
                    key: "build".into(),
                    label: "build &amp; test".into(),
                    value: "fast &#x26; clean".into(),
                    kind: ContextKind::Text,
                    severity: ContextSeverity::Neutral,
                }]),
                context: Some(vec![ContextField {
                    key: "coverage".into(),
                    label: "code &amp; tests".into(),
                    value: "95% &#38; rising".into(),
                    kind: ContextKind::Text,
                    severity: ContextSeverity::Neutral,
                }]),
                clear: Vec::new(),
                git: Default::default(),
            },
        )
        .unwrap();
    assert_eq!(note, None);

    let s = session_of(&env, id);
    assert_eq!(s.goal, "Auth & Permissions");
    assert_eq!(s.headline, "Building & testing <div> 'done'");
    assert_eq!(s.summary, "Reviewed \"core\" & \"cli\"");

    let reports = env.daemon.activity_reports(id).unwrap();
    assert!(reports
        .iter()
        .any(|r| r.kind == "checkpoint" && r.payload.contains("tests passing & green")));

    let ctx = context_of(&env, id);
    assert_eq!(ctx.glance[0].label, "build & test");
    assert_eq!(ctx.glance[0].value, "fast & clean");
    assert_eq!(ctx.detail[0].label, "code & tests");
    assert_eq!(ctx.detail[0].value, "95% & rising");
}

#[tokio::test]
async fn blocked_unescapes_html_entities_in_question() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_agent_report(
            &token,
            AgentReport::Blocked {
                question: "Should we merge &lt;feature&gt; &amp; &apos;fix&apos;?".into(),
            },
        )
        .unwrap();

    let s = session_of(&env, id);
    assert_eq!(s.state, SessionState::NeedsInput);
    assert_eq!(s.state_detail, "Should we merge <feature> & 'fix'?");
}

#[tokio::test]
async fn item_attachment_tools_scope_local_files_and_return_bounded_resources() {
    let env = daemon_env();
    let session_id = spawn_test_session(&env, "p");
    let session = session_of(&env, session_id);
    std::fs::write(
        std::path::Path::new(&session.cwd).join("résumé.txt"),
        b"hello",
    )
    .unwrap();
    let token = env.daemon.session_token(session_id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:attachment", "attachment");

    let attached = call(
        &app,
        &token,
        "attach_item_file",
        json!({"item": item_id, "path": "résumé.txt", "media_type": "text/plain"}),
    )
    .await;
    assert_eq!(attached["isError"], false, "{attached}");
    let metadata: Value = serde_json::from_str(call_text(&attached)).unwrap();
    assert_eq!(metadata["filename"], "résumé.txt");
    assert_eq!(metadata["byte_length"], 5);

    let listed = call(
        &app,
        &token,
        "list_item_attachments",
        json!({"item": item_id}),
    )
    .await;
    let list: Value = serde_json::from_str(call_text(&listed)).unwrap();
    assert_eq!(list["attachments"].as_array().unwrap().len(), 1);

    let fetched = call(
        &app,
        &token,
        "get_item_attachment",
        json!({"item": item_id, "attachment_id": metadata["id"]}),
    )
    .await;
    assert_eq!(fetched["isError"], false, "{fetched}");
    assert_eq!(fetched["content"][0]["type"], "resource");
    assert_eq!(fetched["content"][0]["resource"]["blob"], "aGVsbG8=");

    std::fs::write(
        std::path::Path::new(&session.cwd).join("large.bin"),
        vec![0_u8; pm_daemon::attachments::MCP_ATTACHMENT_FETCH_MAX + 1],
    )
    .unwrap();
    let large = call(
        &app,
        &token,
        "attach_item_file",
        json!({"item": item_id, "path": "large.bin"}),
    )
    .await;
    let large_metadata: Value = serde_json::from_str(call_text(&large)).unwrap();
    let bounded = call(
        &app,
        &token,
        "get_item_attachment",
        json!({"item": item_id, "attachment_id": large_metadata["id"]}),
    )
    .await;
    assert_eq!(bounded["isError"], true);
    assert!(call_text(&bounded).contains("agent retrieval limit"));

    let escaped = call(
        &app,
        &token,
        "attach_item_file",
        json!({"item": item_id, "path": "../secret"}),
    )
    .await;
    assert_eq!(escaped["isError"], true);
    assert!(call_text(&escaped).contains("traversal") || call_text(&escaped).contains("outside"));
    assert_eq!(
        env.daemon
            .list_item_attachments(bucket_id, item_id)
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn list_items_pages_summary_rows_and_get_items_reads_the_full_text() {
    let env = daemon_env();
    let session_id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session_id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket = bucket_of(&env);

    let body = "push notifications ".repeat(40);
    let filed = call(
        &app,
        &token,
        "upsert_items",
        json!({"items": [
            {"external_key": "push:1", "title": "push one", "body": body},
            {"external_key": "push:2", "title": "push two", "body": body},
            {"external_key": "push:3", "title": "push three", "body": body},
            {"external_key": "other:1", "title": "unrelated", "body": "nothing to see"},
        ]}),
    )
    .await;
    assert_eq!(filed["isError"], false, "{filed}");

    let page = call(
        &app,
        &token,
        "list_items",
        json!({"search": "push", "limit": 2}),
    )
    .await;
    assert_eq!(page["isError"], false, "{page}");
    let page: Value = serde_json::from_str(call_text(&page)).unwrap();
    assert_eq!(page["matching_total"], 3);
    assert_eq!(page["next_offset"], 2);
    let rows = page["items"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert!(
            row.get("body").is_none(),
            "summary rows omit the body: {row}"
        );
        assert!(row.get("question").is_none(), "{row}");
        assert!(row.get("url").is_none(), "empty fields are dropped: {row}");
        assert!(row.get("due_at_unix_ms").is_none(), "{row}");
        assert_eq!(row["body_chars"], body.chars().count());
        assert_eq!(row["bucket_id"], bucket);
        assert_eq!(row["ref"], format!("pm:item/{bucket}/{}", row["id"]));
        assert_eq!(row["status"], "inbox");
    }

    let last = call(
        &app,
        &token,
        "list_items",
        json!({"search": "push", "limit": 2, "offset": 2}),
    )
    .await;
    let last: Value = serde_json::from_str(call_text(&last)).unwrap();
    assert_eq!(last["items"].as_array().unwrap().len(), 1);
    assert_eq!(last["matching_total"], 3);
    assert!(last["next_offset"].is_null(), "{last}");

    let exact = call(&app, &token, "list_items", json!({"limit": 4})).await;
    let exact: Value = serde_json::from_str(call_text(&exact)).unwrap();
    assert_eq!(exact["items"].as_array().unwrap().len(), 4);
    assert!(
        exact["next_offset"].is_null(),
        "a page that exhausts the total has no cursor: {exact}"
    );

    let full = call(
        &app,
        &token,
        "get_items",
        json!({"items": [1, format!("pm:item/{bucket}/2"), "push:3"]}),
    )
    .await;
    assert_eq!(full["isError"], false, "{full}");
    let full: Value = serde_json::from_str(call_text(&full)).unwrap();
    let items = full["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["id"], 1);
    assert_eq!(items[1]["id"], 2);
    assert_eq!(items[2]["external_key"], "push:3");
    for item in items {
        assert_eq!(item["body"], body);
        assert_eq!(item["question"], "");
    }

    let missing = call(&app, &token, "get_items", json!({"items": [1, 99]})).await;
    assert_eq!(missing["isError"], true, "{missing}");
    assert!(call_text(&missing).contains("99"), "{missing}");

    let foreign_bucket = env.daemon.create_bucket("foreign").unwrap();
    seed_item(&env, foreign_bucket, "same", "foreign");
    let crossed = call(
        &app,
        &token,
        "get_items",
        json!({"items": [format!("pm:item/{foreign_bucket}/1")]}),
    )
    .await;
    assert_eq!(crossed["isError"], true, "{crossed}");
    assert!(call_text(&crossed).contains("outside this session's bucket"));

    let too_many: Vec<u64> = (1..=21).collect();
    let bounded = call(&app, &token, "get_items", json!({"items": too_many})).await;
    assert_eq!(bounded["isError"], true, "{bounded}");
    assert!(call_text(&bounded).contains("at most 20"), "{bounded}");

    let empty = call(&app, &token, "get_items", json!({"items": []})).await;
    assert_eq!(empty["isError"], true, "{empty}");
}

#[tokio::test]
async fn a_report_moves_the_shown_activity_clock() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let before = session_of(&env, id).last_activity_at_unix_ms;

    std::thread::sleep(std::time::Duration::from_millis(5));
    let reported = call(&app, &token, "report", json!({"headline": "building"})).await;
    assert_eq!(reported["isError"], false, "{reported}");
    let after = session_of(&env, id).last_activity_at_unix_ms;
    assert!(after > before, "a report is agent activity");

    std::thread::sleep(std::time::Duration::from_millis(5));
    let blocked = call(
        &app,
        &token,
        "flag_blocked",
        json!({"question": "which db?"}),
    )
    .await;
    assert_eq!(blocked["isError"], false, "{blocked}");
    assert!(
        session_of(&env, id).last_activity_at_unix_ms > after,
        "flag_blocked is agent activity"
    );
}

#[tokio::test]
async fn item_tools_cannot_cross_bucket_or_resolve_legacy_surrogates() {
    let env = daemon_env();
    let session_id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session_id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let local_bucket = bucket_of(&env);
    let local_item = seed_item(&env, local_bucket, "same", "local");
    let foreign_bucket = env.daemon.create_bucket("foreign").unwrap();
    let foreign_item = seed_item(&env, foreign_bucket, "same", "foreign");
    assert_eq!((local_item, foreign_item), (1, 1));

    let updated = call(
        &app,
        &token,
        "upsert_items",
        json!({"items": [{"id": 1, "title": "local updated"}]}),
    )
    .await;
    assert_eq!(updated["isError"], false, "{updated}");
    assert_eq!(
        env.daemon.get_item(local_bucket, 1).unwrap().title,
        "local updated"
    );
    assert_eq!(
        env.daemon.get_item(foreign_bucket, 1).unwrap().title,
        "foreign"
    );

    // Internal row 2 is the foreign item, but public number 2 does not exist
    // in the caller's bucket and therefore cannot update or read it.
    let surrogate = call(
        &app,
        &token,
        "upsert_items",
        json!({"items": [{"id": 2, "title": "stolen"}]}),
    )
    .await;
    assert_eq!(surrogate["isError"], true, "{surrogate}");
    assert_eq!(
        env.daemon.get_item(foreign_bucket, 1).unwrap().title,
        "foreign"
    );

    let legacy_link = call(
        &app,
        &token,
        "list_item_attachments",
        json!({"item": "pm:item/2"}),
    )
    .await;
    assert_eq!(legacy_link["isError"], true, "{legacy_link}");
    assert!(call_text(&legacy_link).contains("unqualified and unsupported"));

    let canonical = call(
        &app,
        &token,
        "list_item_attachments",
        json!({"item": format!("pm:item/{local_bucket}/1")}),
    )
    .await;
    assert_eq!(canonical["isError"], false, "{canonical}");
    let crossed = call(
        &app,
        &token,
        "list_item_attachments",
        json!({"item": format!("pm:item/{foreign_bucket}/1")}),
    )
    .await;
    assert_eq!(crossed["isError"], true, "{crossed}");
    assert!(call_text(&crossed).contains("outside this session's bucket"));

    let listed = call(&app, &token, "list_items", json!({"include_done": true})).await;
    let listed: Value = serde_json::from_str(call_text(&listed)).unwrap();
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed["items"][0]["bucket_id"], local_bucket);
    assert_eq!(
        listed["items"][0]["ref"],
        format!("pm:item/{local_bucket}/1")
    );
}

/// A report built by guessing the tool's shape used to be applied as an
/// all-empty update and answered "recorded", so nothing on the dashboard
/// changed and nothing said so.
#[tokio::test]
async fn guessed_or_hollow_reports_are_rejected_instead_of_recorded() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let baseline = session_of(&env, id);
    let reports_before = env.daemon.activity_reports(id).unwrap().len();

    let guessed = call(
        &app,
        &token,
        "report",
        json!({"raw": "{\"headline\":\"building\"}"}),
    )
    .await;
    assert_eq!(guessed["isError"], true, "{guessed}");
    assert!(
        call_text(&guessed).contains("unknown field \"raw\""),
        "{guessed}"
    );
    assert!(call_text(&guessed).contains("headline"), "{guessed}");

    let hollow = call(&app, &token, "report", json!({})).await;
    assert_eq!(hollow["isError"], true, "{hollow}");
    assert!(call_text(&hollow).contains("needs a headline"), "{hollow}");

    let blank = call(&app, &token, "report", json!({"headline": "   "})).await;
    assert_eq!(blank["isError"], true, "{blank}");

    let after = session_of(&env, id);
    assert_eq!(after.headline, baseline.headline);
    assert_eq!(
        after.last_activity_at_unix_ms, baseline.last_activity_at_unix_ms,
        "a rejected report is not activity"
    );
    assert_eq!(
        env.daemon.activity_reports(id).unwrap().len(),
        reports_before
    );

    let note_only = call(&app, &token, "report", json!({"note": "tests passing"})).await;
    assert_eq!(
        note_only["isError"], false,
        "a blank headline with a real note still applies: {note_only}"
    );
    assert_eq!(
        env.daemon.activity_reports(id).unwrap().len(),
        reports_before + 1
    );

    let real = call(&app, &token, "report", json!({"headline": "building"})).await;
    assert_eq!(real["isError"], false, "{real}");
    assert_eq!(session_of(&env, id).headline, "building");

    let guessed_block = call(&app, &token, "flag_blocked", json!({"q": "which?"})).await;
    assert_eq!(guessed_block["isError"], true, "{guessed_block}");
    assert!(call_text(&guessed_block).contains("unknown field \"q\""));
    let empty_block = call(&app, &token, "flag_blocked", json!({"question": ""})).await;
    assert_eq!(empty_block["isError"], true, "{empty_block}");
    assert!(call_text(&empty_block).contains("needs a question"));
    assert_eq!(session_of(&env, id).state, baseline.state);
}

#[tokio::test]
async fn report_metadata_does_not_change_hook_owned_lifecycle_state() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    env.daemon
        .handle_hook_event(
            &token,
            pm_protocol::domain::HookKind::TurnEnded,
            "",
            "",
            "",
            false,
        )
        .unwrap();

    env.daemon
        .handle_agent_report(&token, headline("documenting the result"))
        .unwrap();

    let session = session_of(&env, id);
    assert_eq!(session.state, SessionState::Idle);
    assert_eq!(session.headline, "documenting the result");
}

#[tokio::test]
async fn blank_fields_never_reach_the_context_bags() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_agent_report(
            &token,
            AgentReport::Report {
                goal: String::new(),
                headline: "working".into(),
                summary: None,
                note: String::new(),
                glance: Some(vec![
                    field("", "", ContextKind::Text),
                    field("tests", "passing", ContextKind::Badge),
                    field("status", "   ", ContextKind::Text),
                ]),
                context: Some(vec![
                    field("   ", "orphan", ContextKind::Text),
                    field("branch", "main", ContextKind::Code),
                ]),
                clear: Vec::new(),
                git: Default::default(),
            },
        )
        .unwrap();
    let ctx = context_of(&env, id);
    assert_eq!(
        ctx.glance
            .iter()
            .map(|f| f.key.as_str())
            .collect::<Vec<_>>(),
        ["tests"],
        "blank glance fields are dropped, not stored as empty chips"
    );
    assert_eq!(
        ctx.detail
            .iter()
            .map(|f| f.key.as_str())
            .collect::<Vec<_>>(),
        ["branch"],
        "blank context fields are dropped"
    );
}

#[tokio::test]
async fn headline_only_report_leaves_context_untouched() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_agent_report(
            &token,
            AgentReport::Report {
                goal: String::new(),
                headline: "starting".into(),
                summary: None,
                note: String::new(),
                glance: Some(vec![field("tests", "passing", ContextKind::Badge)]),
                context: Some(vec![field("branch", "main", ContextKind::Code)]),
                clear: Vec::new(),
                git: Default::default(),
            },
        )
        .unwrap();
    // A later bare-headline report must not wipe the glance/context bags.
    env.daemon
        .handle_agent_report(&token, headline("next step"))
        .unwrap();
    let ctx = context_of(&env, id);
    assert_eq!(
        ctx.glance.len(),
        1,
        "glance survives a headline-only report"
    );
    assert_eq!(
        ctx.detail.len(),
        1,
        "context survives a headline-only report"
    );
    // An empty headline keeps the current name rather than blanking it.
    env.daemon
        .handle_agent_report(&token, headline("   "))
        .unwrap();
    assert_eq!(session_of(&env, id).headline, "next step");
}

#[tokio::test]
async fn report_glance_replaces_and_bounds_to_three() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    let note = env
        .daemon
        .handle_agent_report(
            &token,
            AgentReport::Report {
                goal: String::new(),
                headline: "working".into(),
                summary: None,
                note: String::new(),
                glance: Some(vec![
                    field("a", "1", ContextKind::Text),
                    field("b", "2", ContextKind::Text),
                    field("c", "3", ContextKind::Text),
                    field("d", "4", ContextKind::Text),
                ]),
                context: None,
                clear: Vec::new(),
                git: Default::default(),
            },
        )
        .unwrap();
    let glance = context_of(&env, id).glance;
    assert_eq!(glance.len(), 3, "kept only the first three");
    assert_eq!(
        glance.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert!(note.unwrap().contains("glance"));
}

#[tokio::test]
async fn report_context_upserts_clears_and_caps() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    let mut report = headline("x");
    if let AgentReport::Report { context, .. } = &mut report {
        *context = Some(vec![
            field("branch", "feat/jwt", ContextKind::Code),
            field("preview_url", "http://localhost:3000", ContextKind::Url),
        ]);
    }
    env.daemon.handle_agent_report(&token, report).unwrap();

    // Upsert by key: branch is replaced, preview_url stays.
    let mut report = headline("x");
    if let AgentReport::Report { context, clear, .. } = &mut report {
        *context = Some(vec![field("branch", "feat/jwt-2", ContextKind::Code)]);
        *clear = Vec::new();
    }
    env.daemon.handle_agent_report(&token, report).unwrap();
    let detail = context_of(&env, id).detail;
    assert_eq!(detail.len(), 2);
    assert_eq!(
        detail.iter().find(|f| f.key == "branch").unwrap().value,
        "feat/jwt-2"
    );

    // Clear one key.
    let mut report = headline("x");
    if let AgentReport::Report { clear, .. } = &mut report {
        *clear = vec!["branch".into()];
    }
    env.daemon.handle_agent_report(&token, report).unwrap();
    let detail = context_of(&env, id).detail;
    assert_eq!(detail.len(), 1);
    assert_eq!(detail[0].key, "preview_url");

    // Fill to the cap, then overflow returns a note and drops extras.
    let bulk: Vec<ContextField> = (0..25)
        .map(|i| field(&format!("k{i}"), "v", ContextKind::Text))
        .collect();
    let mut report = headline("x");
    if let AgentReport::Report { context, .. } = &mut report {
        *context = Some(bulk);
    }
    let note = env.daemon.handle_agent_report(&token, report).unwrap();
    assert_eq!(context_of(&env, id).detail.len(), 20);
    assert!(note.unwrap().contains("full"));
}

#[tokio::test]
async fn blocked_moves_session_to_needs_input() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_agent_report(
            &token,
            AgentReport::Blocked {
                question: "merge or rebase?".into(),
            },
        )
        .unwrap();
    let s = session_of(&env, id);
    assert_eq!(s.state, SessionState::NeedsInput);
    assert_eq!(s.state_detail, "merge or rebase?");
}

#[tokio::test]
async fn reports_with_bad_tokens_are_rejected() {
    let env = daemon_env();
    spawn_test_session(&env, "p");
    let err = env
        .daemon
        .handle_agent_report("bogus", headline("hi"))
        .unwrap_err();
    assert!(err.to_string().contains("unknown session token"));
}

async fn rpc(app: &axum::Router, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut builder = Request::post("/mcp").header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let res = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn mcp_endpoint_speaks_the_protocol_subset() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let (status, _) = rpc(&app, None, json!({"jsonrpc":"2.0","id":1,"method":"ping"})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["serverInfo"]["name"], "puppet-master");

    let (status, _) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "report",
            "flag_blocked",
            "reply_message",
            "publish_port",
            "publish_dir",
            "list_dirs",
            "unpublish_dir",
            "unpublish_port",
            "upsert_plan",
            "sync_plan",
            "present_plan_decision",
            "present_plan_decision_batch",
            "resolve_plan_decision",
            "post_plan_message",
            "get_plan",
            "list_plans",
            "list_connections",
            "get_connection",
            "list_connection_tools",
            "describe_connection_tool",
            "seed_connection",
            "update_connection_draft",
            "propose_connection_policy",
            "propose_connection_update",
            "call_connection_tool",
            "get_connection_call",
            "list_connection_calls",
            "open_review",
            "next_review_event",
            "post_review_reply",
            "review_status",
            "upsert_items",
            "list_items",
            "get_items",
            "attach_item_file",
            "list_item_attachments",
            "get_item_attachment",
            "post_briefing"
        ]
    );
    let publish = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "publish_port")
        .unwrap();
    assert_eq!(
        publish["inputSchema"]["required"],
        json!(["port", "slug"]),
        "a forward cannot be published unnamed"
    );
    // The rules are in the schema so an agent gets a usable slug on the
    // first call rather than after a rejection.
    let slug_guidance = publish["inputSchema"]["properties"]["slug"]["description"]
        .as_str()
        .unwrap();
    for rule in [
        "3 to 40",
        "lowercase a-z, 0-9 and hyphen",
        "starting and ending alphanumeric",
        "no consecutive hyphens",
        "Uppercase is rejected",
        "'www', 'api', 'app', 'admin', 'share', 'pm' and 'mail' are reserved",
        "the letter f followed by digits",
        "unique across every live forward",
    ] {
        assert!(slug_guidance.contains(rule), "slug schema omits {rule:?}");
    }
    let description = publish["description"].as_str().unwrap();
    for phrase in ["share domain", "hostname", "docs-preview"] {
        assert!(
            description.contains(phrase),
            "the tool description omits {phrase:?}"
        );
    }
    let upsert = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "upsert_items")
        .unwrap();
    let status_guidance = upsert["inputSchema"]["properties"]["items"]["items"]["properties"]
        ["status"]["description"]
        .as_str()
        .unwrap();
    assert!(status_guidance.contains(ITEM_STATUS_CREATE_GUIDANCE));
    assert!(status_guidance.contains("no additional user information"));
    assert!(status_guidance.contains("genuinely requires user triage"));
    assert!(status_guidance.contains("Dispatching a worker is orthogonal"));
    assert!(status_guidance
        .contains("\"Create a task to rename the Run button to Dispatch\" -> `planned`"));
    assert!(status_guidance.contains("\"Maybe improve the dashboard navigation\" -> `inbox`"));
    assert!(status_guidance.contains(
        "\"Create a task to rename the Run button to Dispatch, but do not dispatch it\" -> `planned`"
    ));
    let item_properties = &upsert["inputSchema"]["properties"]["items"]["items"]["properties"];
    assert_eq!(item_properties["body"]["maxLength"], ITEM_BODY_MAX);
    assert!(item_properties["body"]["description"]
        .as_str()
        .unwrap()
        .contains("Unicode scalar values"));
    let due_guidance = item_properties["due"]["description"].as_str().unwrap();
    assert!(due_guidance.contains("RFC 3339/ISO 8601"));
    assert!(due_guidance.contains("Z or a UTC offset"));
    assert!(due_guidance.contains("'YYYY-MM-DD'"));
    assert!(due_guidance.contains("unix milliseconds"));

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"report","arguments":{
                "headline":"building the thing",
                "glance":[{"key":"status","label":"status","value":"building","kind":"badge","severity":"info"}]}}}),
    )
    .await;
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(session_of(&env, id).headline, "building the thing");
    assert_eq!(context_of(&env, id).glance[0].value, "building");

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
            "name":"flag_blocked","arguments":{"question":"which port?"}}}),
    )
    .await;
    assert_eq!(body["result"]["isError"], false);
    let s = session_of(&env, id);
    assert_eq!(s.state, SessionState::NeedsInput);
    assert_eq!(s.state_detail, "which port?");
    assert!(
        s.needs_input_unseen,
        "a blocked agent must ask for the user, not sit there already seen"
    );

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
            "name":"no_such_tool","arguments":{}}}),
    )
    .await;
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":6,"method":"resources/list"}),
    )
    .await;
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not supported"));
}

#[tokio::test]
async fn report_tool_schema_keeps_live_status_compact_and_current() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let report = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "report")
        .unwrap();
    let description = report["description"].as_str().unwrap();
    assert!(description.contains("Keep headlines plain text (`&`, not `&amp;`)"));
    assert!(description.contains("Keep `glance` and `context` small, current, and high-signal"));
    assert!(description.contains("Treat 20 context fields as a ceiling, not a target"));
    assert!(description.contains("use `clear` to drop stale keys"));

    assert!(description.contains("Never put the step in the goal or the goal in the headline"));

    let properties = &report["inputSchema"]["properties"];
    assert_eq!(properties["goal"]["type"], "string");
    assert!(properties["goal"]["description"]
        .as_str()
        .unwrap()
        .contains("constantly updated to reflect the current goal"));
    assert_eq!(
        report["inputSchema"]["required"],
        json!(["goal", "headline"])
    );
    assert!(properties["headline"]["description"]
        .as_str()
        .unwrap()
        .contains("use '&', not '&amp;'"));
    assert!(properties["glance"]["description"]
        .as_str()
        .unwrap()
        .contains("highest-signal current chips"));
    assert!(properties["context"]["description"]
        .as_str()
        .unwrap()
        .contains("20 is a ceiling, not a target"));
    assert!(properties["clear"]["description"]
        .as_str()
        .unwrap()
        .contains("Stale context keys"));
}

#[tokio::test]
async fn oversized_item_body_returns_structured_error_without_mutation() {
    let env = daemon_env();
    let session_id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session_id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let prefix = "# large Markdown\n\n";
    let exact = format!(
        "{prefix}{}😀",
        "界".repeat(ITEM_BODY_MAX - prefix.chars().count() - 1)
    );
    assert_eq!(exact.chars().count(), ITEM_BODY_MAX);

    let created = call(
        &app,
        &token,
        "upsert_items",
        json!({"items": [{"external_key": "body:boundary", "title": "Boundary", "body": exact}]}),
    )
    .await;
    assert_eq!(created["isError"], false, "{}", call_text(&created));

    let oversized = format!("{}😀", "x".repeat(ITEM_BODY_MAX));
    let rejected = call(
        &app,
        &token,
        "upsert_items",
        json!({"items": [
            {"external_key": "body:boundary", "body": oversized},
            {"external_key": "body:must-not-create", "title": "Must not create"}
        ]}),
    )
    .await;
    assert_eq!(rejected["isError"], true);
    let error: Value = serde_json::from_str(call_text(&rejected)).unwrap();
    assert_eq!(error["error"], "validation_failed");
    assert_eq!(error["details"][0]["field"], "body");
    assert_eq!(error["details"][0]["code"], "max_length");
    assert_eq!(error["details"][0]["limit"], ITEM_BODY_MAX);
    assert_eq!(error["details"][0]["actual"], ITEM_BODY_MAX + 1);
    assert_eq!(error["details"][0]["unit"], "Unicode scalar values");

    let items = env.daemon.subscribe().0.items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].body.chars().count(), ITEM_BODY_MAX);
    assert_eq!(items[0].body.chars().last(), Some('😀'));
}

fn spawn_without_items_api(env: &TestEnv) -> u64 {
    env.daemon
        .spawn_session(
            env.project_id,
            pm_protocol::domain::AgentKind::Test,
            "no items",
            "p",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            false,
            false,
            None,
        )
        .unwrap()
}

#[tokio::test]
async fn item_tools_are_gated_by_the_spawn_toggle() {
    let env = daemon_env();
    let id = spawn_without_items_api(&env);
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "report",
            "flag_blocked",
            "reply_message",
            "publish_port",
            "publish_dir",
            "list_dirs",
            "unpublish_dir",
            "unpublish_port",
            "upsert_plan",
            "sync_plan",
            "present_plan_decision",
            "present_plan_decision_batch",
            "resolve_plan_decision",
            "post_plan_message",
            "get_plan",
            "list_plans",
            "list_connections",
            "get_connection",
            "list_connection_tools",
            "describe_connection_tool",
            "seed_connection",
            "update_connection_draft",
            "propose_connection_policy",
            "propose_connection_update",
            "call_connection_tool",
            "get_connection_call",
            "list_connection_calls",
            "open_review",
            "next_review_event",
            "post_review_reply",
            "review_status"
        ],
        "disabled sessions do not see the item tools"
    );

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"upsert_items","arguments":{"items":[{"title":"x"}]}}}),
    )
    .await;
    assert_eq!(body["result"]["isError"], true);
    assert!(body["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("disabled"));
}

#[tokio::test]
async fn item_tools_sweep_reconcile_and_brief_over_mcp() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    // First sweep: a blocker and an item depending on it by key.
    let sweep = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
    "name":"upsert_items","arguments":{"items":[
        {"external_key":"pr:1","title":"Land the auth PR","status":"blocked_external",
         "priority":"high","source_kind":"github","source_detail":"acme/api",
         "project":"test-project","due":"2026-07-25","question":"Approve the merge?"},
        {"external_key":"deploy:1","title":"Deploy once the PR lands",
         "blocked_by":["pr:1"]}
    ]}}});
    let (_, body) = rpc(&app, Some(&token), sweep.clone()).await;
    assert_eq!(body["result"]["isError"], false);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let results: Value = serde_json::from_str(text).unwrap();
    assert_eq!(results["results"][0]["outcome"], "created");
    assert_eq!(results["results"][1]["outcome"], "created");
    let blocker_id = results["results"][0]["id"].as_u64().unwrap();

    // Snapshot carries them; the dependency edge resolved by key.
    let items = env.daemon.subscribe().0.items;
    assert_eq!(items.len(), 2);
    let deploy = items
        .iter()
        .find(|i| i.title.starts_with("Deploy"))
        .unwrap();
    assert_eq!(deploy.blocked_by, vec![blocker_id]);
    let pr = items.iter().find(|i| i.title.contains("auth PR")).unwrap();
    assert!(pr.due_at_unix_ms.is_some());
    assert_eq!(pr.created_by_session_id, Some(id));
    assert_eq!(pr.question, "Approve the merge?");

    // The identical re-sweep changes nothing and duplicates nothing.
    let (_, body) = rpc(&app, Some(&token), sweep).await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let results: Value = serde_json::from_str(text).unwrap();
    assert_eq!(results["results"][0]["outcome"], "unchanged");
    assert_eq!(env.daemon.subscribe().0.items.len(), 2);

    // list_items returns the open board as JSON.
    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"list_items","arguments":{}}}),
    )
    .await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let listed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(listed["items"].as_array().unwrap().len(), 2);

    // An unknown status fails the whole batch and writes nothing.
    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"upsert_items","arguments":{"items":[
                {"external_key":"x:1","title":"a","status":"wip"}]}}}),
    )
    .await;
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("blocked_external"),
        "error names valid values"
    );
    assert_eq!(env.daemon.subscribe().0.items.len(), 2, "nothing written");

    // A briefing lands as the bucket's latest.
    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
            "name":"post_briefing","arguments":{
                "markdown":format!("## Needs you\n- [Land the auth PR](pm:item/{}/1)", bucket_of(&env))}}}),
    )
    .await;
    assert_eq!(body["result"]["isError"], false);
    let briefings = env.daemon.subscribe().0.briefings;
    assert_eq!(briefings.len(), 1);
    assert!(briefings[0]
        .markdown
        .contains(&format!("pm:item/{}/1", bucket_of(&env))));
    assert_eq!(briefings[0].session_id, Some(id));
}

#[tokio::test]
async fn item_due_accepts_rfc3339_dates_and_unix_milliseconds() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let unix_milliseconds = 1_784_393_092_414_i64;
    let accepted = [
        ("date-only", json!("2026-07-18"), 1_784_332_800_000_i64),
        ("unix-ms", json!(unix_milliseconds), unix_milliseconds),
        ("utc", json!("2026-07-18T21:59:00Z"), 1_784_411_940_000_i64),
        (
            "reported-positive-offset",
            json!("2026-07-18T23:59:00+02:00"),
            1_784_411_940_000_i64,
        ),
        (
            "negative-offset",
            json!("2026-07-18T23:59:00-04:30"),
            1_784_435_340_000_i64,
        ),
        (
            "fractional-seconds",
            json!("2026-07-18T21:59:00.123Z"),
            1_784_411_940_123_i64,
        ),
    ];
    let items: Vec<Value> = accepted
        .iter()
        .map(|(key, due, _)| {
            json!({
                "external_key": format!("due:{key}"),
                "title": format!("Due {key}"),
                "due": due,
            })
        })
        .collect();

    let result = call(&app, &token, "upsert_items", json!({"items": items})).await;
    assert_eq!(result["isError"], false, "{}", call_text(&result));

    let stored = env.daemon.subscribe().0.items;
    for (key, _, expected) in accepted {
        let item = stored
            .iter()
            .find(|item| item.external_key.as_deref() == Some(&format!("due:{key}")))
            .unwrap();
        assert_eq!(item.due_at_unix_ms, Some(expected), "{key}");
    }
}

#[tokio::test]
async fn item_due_rejects_malformed_timestamp_without_writing() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let result = call(
        &app,
        &token,
        "upsert_items",
        json!({"items": [{
            "external_key": "due:malformed",
            "title": "Malformed due",
            "due": "2026-07-18T23:59:00+2:00"
        }]}),
    )
    .await;

    assert_eq!(result["isError"], true);
    let error = call_text(&result);
    assert!(error.contains("RFC 3339/ISO 8601"), "{error}");
    assert!(error.contains("Z or a UTC offset"), "{error}");
    assert!(error.contains("'YYYY-MM-DD'"), "{error}");
    assert!(error.contains("unix milliseconds"), "{error}");
    assert!(env.daemon.subscribe().0.items.is_empty());
}

#[tokio::test]
async fn item_creates_default_to_the_filing_sessions_project() {
    let env = daemon_env();
    let bucket_id = bucket_of(&env);
    let other_root = tempfile::tempdir().unwrap();
    let other_project_id = env
        .daemon
        .create_project(
            bucket_id,
            "other-project",
            other_root.path().to_str().unwrap(),
        )
        .unwrap();
    let first_session_id = spawn_test_session(&env, "first");
    let other_session_id = env
        .daemon
        .spawn_session(
            other_project_id,
            AgentKind::Test,
            "other project",
            "second",
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let first_token = env.daemon.session_token(first_session_id).unwrap().unwrap();
    let other_token = env.daemon.session_token(other_session_id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let result = call(
        &app,
        &first_token,
        "upsert_items",
        json!({"items": [
            {"external_key": "default:first", "title": "Default to first project"},
            {"external_key": "override:first", "title": "Override the default",
             "project": other_project_id}
        ]}),
    )
    .await;
    assert_eq!(result["isError"], false, "{}", call_text(&result));

    let result = call(
        &app,
        &first_token,
        "upsert_items",
        json!({"items": [
            {"external_key": "override:first", "body": "Updated without a project"}
        ]}),
    )
    .await;
    assert_eq!(result["isError"], false, "{}", call_text(&result));
    let response: Value = serde_json::from_str(call_text(&result)).unwrap();
    assert_eq!(response["results"][0]["outcome"], "updated");

    let result = call(
        &app,
        &other_token,
        "upsert_items",
        json!({"items": [
            {"external_key": "default:other", "title": "Default to other project"}
        ]}),
    )
    .await;
    assert_eq!(result["isError"], false, "{}", call_text(&result));

    let items = env.daemon.subscribe().0.items;
    let project_of = |key: &str| {
        items
            .iter()
            .find(|item| item.external_key.as_deref() == Some(key))
            .unwrap()
            .project_id
    };
    assert_eq!(project_of("default:first"), Some(env.project_id));
    assert_eq!(project_of("override:first"), Some(other_project_id));
    assert_eq!(project_of("default:other"), Some(other_project_id));
}

#[tokio::test]
async fn tool_call_with_dead_session_reports_error_content() {
    let mut env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    env.daemon.kill_session(id).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"report","arguments":{"headline":"late"}}}),
    )
    .await;
    assert_eq!(body["result"]["isError"], true);
}

fn bucket_of(env: &TestEnv) -> u64 {
    env.daemon.subscribe().0.buckets[0].id
}

fn spawn_supervisor(env: &TestEnv) -> u64 {
    env.daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "supervisor",
            "sup",
            None,
            PermissionMode::Inherit,
            None,
            true,
            true,
            None,
        )
        .unwrap()
}

fn seed_item(env: &TestEnv, bucket_id: u64, key: &str, title: &str) -> u64 {
    let write = ItemWrite {
        bucket_id,
        external_key: Some(key.into()),
        title: Some(title.into()),
        ..ItemWrite::default()
    };
    env.daemon
        .upsert_item(bucket_id, &write, None)
        .unwrap()
        .0
        .id
}

fn spawn_supervised_child(env: &TestEnv, supervisor_id: u64, prompt: &str) -> u64 {
    env.daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "supervised child",
            prompt,
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            Some(supervisor_id),
        )
        .unwrap()
}

async fn call(app: &axum::Router, token: &str, name: &str, arguments: Value) -> Value {
    let (_, body) = rpc(
        app,
        Some(token),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name": name, "arguments": arguments}}),
    )
    .await;
    body["result"].clone()
}

fn call_text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

#[tokio::test]
async fn supervisor_tools_are_gated_by_the_spawn_toggle() {
    let env = daemon_env();
    let plain = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(plain).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"spawn_session"),
        "plain sessions must not see the supervisor tools: {names:?}"
    );

    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
        "project": "test-project", "agent": "test", "prompt": "x", "item": 1}),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert!(call_text(&result).contains("disabled"));

    let sup = spawn_supervisor(&env);
    let sup_token = env.daemon.session_token(sup).unwrap().unwrap();
    let (_, body) = rpc(
        &app,
        Some(&sup_token),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    let names: Vec<String> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for tool in [
        "spawn_session",
        "session_status",
        "wait_sessions",
        "snooze_supervision",
        "read_terminal",
        "send_input",
        "interrupt_session",
        "resume_session",
        "kill_session",
        "list_sessions",
        "list_instructions",
        "get_effective_instructions",
        "set_instructions",
    ] {
        assert!(names.iter().any(|n| n == tool), "missing {tool}: {names:?}");
    }
    let send_input = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "send_input")
        .unwrap();
    assert_eq!(
        send_input["inputSchema"]["properties"]["submit"]["default"],
        false
    );
    let description = send_input["description"].as_str().unwrap();
    assert!(description.contains("exactly one Enter"));
    assert!(description.contains("submission_unconfirmed"));

    let spawn = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "spawn_session")
        .unwrap();
    assert!(
        spawn["inputSchema"]["properties"]["host"].is_object(),
        "spawn_session offers the host argument"
    );
    assert!(
        !spawn["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("host")),
        "host stays optional"
    );
    let spawn_description = spawn["description"].as_str().unwrap();
    assert!(spawn_description.contains("host"));
    assert!(spawn_description.contains("permission mode always apply"));

    // The agent choices are generated from the adapter registry, so the
    // schema can never advertise an agent the daemon would reject.
    let offered: Vec<String> = spawn["inputSchema"]["properties"]["agent"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    assert_eq!(offered, env.daemon.spawnable_agents());
    assert!(!offered.is_empty(), "spawn_session must offer some agent");
}

#[tokio::test]
async fn supervisor_instruction_tools_enforce_bucket_scope_revisions_and_live_demotion() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let set=call(&app,&token,"set_instructions",json!({"scope":"project","project":env.project_id,"role":"worker","markdown":"project policy: omit tracker references","expected_revision":0,"note":"user requested"})).await;
    assert_eq!(set["isError"], false, "{set}");
    assert_eq!(
        call(
            &app,
            &token,
            "set_instructions",
            json!({"scope":"bucket","role":"all","markdown":"bucket all","expected_revision":0})
        )
        .await["isError"],
        false
    );
    let conflict=call(&app,&token,"set_instructions",json!({"scope":"project","project":env.project_id,"role":"worker","markdown":"lost write","expected_revision":0})).await;
    assert_eq!(conflict["isError"], true);
    assert!(call_text(&conflict).contains("revision conflict"));
    let effective = call(
        &app,
        &token,
        "get_effective_instructions",
        json!({"project":env.project_id,"role":"worker"}),
    )
    .await;
    assert!(call_text(&effective).contains("Puppet Master role: Worker"));
    assert!(call_text(&effective).contains("project policy: omit tracker references"));
    let text = call_text(&effective);
    assert!(text.contains("Use `#<item-id>` for Puppet Master items"));
    assert!(text.contains("OPS-1234"));
    assert!(text.contains("acme/widgets#482"));
    assert!(text.contains("Never rewrite shared or published history"));
    assert!(
        text.find("Puppet Master contract").unwrap() < text.find("Built-in worker role").unwrap()
    );
    assert!(text.find("bucket all").unwrap() < text.find("project policy").unwrap());
    let listed = call(
        &app,
        &token,
        "list_instructions",
        json!({"project":env.project_id}),
    )
    .await;
    assert!(call_text(&listed).contains("user requested"));
    let item = seed_item(&env, bucket_of(&env), "instruction:spawn", "spawn");
    let spawned = call(
        &app,
        &token,
        "spawn_session",
        json!({"project":env.project_id,"agent":"test","prompt":"task","item":item}),
    )
    .await;
    let child: Value = serde_json::from_str(call_text(&spawned)).unwrap();
    let child = child["session_id"].as_u64().unwrap();
    let snap = env.daemon.instruction_snapshot(child, 1).unwrap();
    assert!(snap.0.contains("project policy: omit tracker references"));
    assert!(snap.0.contains("Use `#<item-id>` for Puppet Master items"));
    assert!(snap.1.contains("layer_id"));
    assert_eq!(snap.2.len(), 64);
    env.daemon
        .update_session_apis(sup, None, Some(false))
        .unwrap();
    let denied = call(&app, &token, "list_instructions", json!({})).await;
    assert_eq!(denied["isError"], true);
}

#[tokio::test]
async fn spawned_workers_receive_tracker_agnostic_commit_traceability_instructions() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);

    let cases = [
        ("pm:item:4294967296", "large PM item #4294967296"),
        ("jira:OPS-1234", "external ticket OPS-1234"),
        ("github:acme/widgets#482", "upstream issue acme/widgets#482"),
    ];
    for (external_key, title) in cases {
        let item = seed_item(&env, bucket_id, external_key, title);
        let spawned = call(
            &app,
            &token,
            "spawn_session",
            json!({"project":env.project_id,"agent":"test","prompt":title,"item":item}),
        )
        .await;
        let child: Value = serde_json::from_str(call_text(&spawned)).unwrap();
        let snapshot = env
            .daemon
            .instruction_snapshot(child["session_id"].as_u64().unwrap(), 1)
            .unwrap()
            .0;
        assert!(snapshot.contains("every task commit"));
        assert!(!snapshot.contains("human-authored"));
        assert!(snapshot.contains("Use `#<item-id>` for Puppet Master items"));
        assert!(snapshot.contains("preserve the upstream system's normal reference"));
        assert!(snapshot.contains("every relevant canonical reference"));
        assert!(snapshot.contains("Do not invent a reference"));
    }

    let supervisor = env
        .daemon
        .effective_instructions(bucket_id, None, SessionRole::Supervisor)
        .unwrap();
    assert!(!supervisor.contains("Commit traceability"));
}

#[test]
fn compiled_default_instructions_define_needs_input_escalation_contracts() {
    let env = daemon_env();
    let bucket_id = bucket_of(&env);
    let worker = env
        .daemon
        .effective_instructions(bucket_id, Some(env.project_id), SessionRole::Worker)
        .unwrap();
    let supervisor = env
        .daemon
        .effective_instructions(bucket_id, None, SessionRole::Supervisor)
        .unwrap();

    for compiled in [&worker, &supervisor] {
        assert!(compiled.contains("When picking up an item or receiving a new task"));
        assert!(compiled.contains(
            "only when a missing architectural or implementation-specific detail is critical"
        ));
        assert!(compiled.contains("ask the user before proceeding"));
        assert!(compiled.contains("NeedsInput/question card, item question, or direct question"));
        assert!(
            compiled.contains("Continue making reasonable assumptions for non-critical details")
        );
        assert!(compiled.contains("Before ending a turn that cannot continue"));
        assert!(compiled.contains("call `flag_blocked` with one concrete question"));
        assert!(compiled.contains("Never merely print a question and then stop or finish"));
        assert!(
            compiled.contains("Idle means the turn is complete and no input is currently required")
        );
        assert!(compiled.contains("never use Idle as an implicit waiting state"));
    }

    for guidance in [
        "When a child becomes NeedsInput",
        "inspect its `state_detail`, recent reports, linked item context, and terminal as needed",
        "answer or steer the child directly",
        "call your own `flag_blocked` with one concise user-facing question",
        "identifies the child or item",
        "Do not automatically mark yourself NeedsInput before triage",
        "do not infer blocking from question marks or terminal text",
        "When a child becomes Idle, its turn is complete and it requires no input",
    ] {
        assert!(
            supervisor.contains(guidance),
            "supervisor contract omits guidance: {guidance}"
        );
    }
}

#[test]
fn compiled_default_instructions_keep_live_status_compact_and_current() {
    let env = daemon_env();
    let bucket_id = bucket_of(&env);
    for role in [SessionRole::Worker, SessionRole::Supervisor] {
        let compiled = env
            .daemon
            .effective_instructions(bucket_id, Some(env.project_id), role)
            .unwrap();
        for guidance in [
            "Keep headlines plain text (`&`, not `&amp;`)",
            "Keep `glance` and `context` small, current, and high-signal",
            "Treat 20 context fields as a ceiling, not a target",
            "use `clear` to drop stale keys",
        ] {
            assert!(
                compiled.contains(guidance),
                "compiled {role:?} instructions omit guidance: {guidance}"
            );
        }
    }
}

#[tokio::test]
async fn supervisor_spawn_ignores_worker_cwd_and_permission_overrides() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:flash", "fix the flash");

    // Hostile extras ride along; the handler must not forward any of
    // them into the spawn.
    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
        "project": "test-project", "agent": "test", "prompt": "do the fix",
        "item": "it:flash",
        "cwd": "/evil", "path": "/evil", "worker": 99, "worker_id": 99,
        "permission_mode": "bypass"}),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let reply: Value = serde_json::from_str(call_text(&result)).unwrap();
    let child = reply["session_id"].as_u64().unwrap();
    assert_eq!(reply["item_id"].as_u64(), Some(item_id));

    let s = session_of(&env, child);
    let project_path = env
        .daemon
        .subscribe()
        .0
        .projects
        .iter()
        .find(|p| p.id == env.project_id)
        .unwrap()
        .path
        .clone();
    assert_eq!(s.cwd, project_path, "cwd always comes from the project");
    assert_eq!(s.worker_id, LOCAL_WORKER_ID, "worker always cascades");
    assert_eq!(
        s.permission_mode,
        PermissionMode::Default,
        "permission mode always cascades from the project"
    );
    assert_eq!(s.spawned_by_session_id, Some(sup));
    assert!(s.items_api, "children keep the item tools");
    assert!(
        !s.supervisor_api,
        "children never inherit the supervisor tools"
    );

    let item = env
        .daemon
        .subscribe()
        .0
        .items
        .into_iter()
        .find(|i| i.id == item_id)
        .unwrap();
    assert!(
        item.session_ids.contains(&child),
        "session linked to the item"
    );
    let notes = env.daemon.item_notes(bucket_of(&env), item_id).unwrap();
    assert!(
        notes
            .iter()
            .any(|n| n.text.contains(&format!("spawned session {child}"))),
        "spawn audited on the item timeline: {notes:?}"
    );
}

#[tokio::test]
async fn supervisor_spawn_agent_is_optional_and_explicit_values_remain_authoritative() {
    let env = codex_tui_daemon_env();
    env.daemon
        .set_project_default_agent(env.project_id, Some(AgentKind::Codex))
        .unwrap();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let inherited_item = seed_item(&env, bucket_id, "it:inherited-agent", "inherit");

    let inherited = call(
        &app,
        &token,
        "spawn_session",
        json!({"project": env.project_id, "prompt": "inherit agent", "item": inherited_item}),
    )
    .await;
    assert_eq!(inherited["isError"], false, "{inherited}");
    let inherited_reply: Value = serde_json::from_str(call_text(&inherited)).unwrap();
    assert_eq!(inherited_reply["agent"], "codex");
    assert_eq!(inherited_reply["agent_source"], "project");

    let explicit_item = seed_item(&env, bucket_id, "it:explicit-agent", "explicit");
    let explicit = call(
        &app,
        &token,
        "spawn_session",
        json!({"project": env.project_id, "agent": "codex", "prompt": "explicit agent", "item": explicit_item}),
    )
    .await;
    assert_eq!(explicit["isError"], false, "{explicit}");
    let explicit_reply: Value = serde_json::from_str(call_text(&explicit)).unwrap();
    assert_eq!(explicit_reply["agent"], "codex");
    assert_eq!(explicit_reply["agent_source"], "explicit");

    let notes = env.daemon.item_notes(bucket_id, inherited_item).unwrap();
    assert!(notes
        .iter()
        .any(|note| note.text.contains("source=project")));
}

#[tokio::test]
async fn supervisor_ops_only_touch_its_own_children() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let stranger = spawn_test_session(&env, "p");

    // Observing and messaging follow the bucket, which is what
    // list_sessions has always shown, so a peer's session is reachable.
    for (tool, args) in [
        ("session_status", json!({"session": stranger})),
        ("read_terminal", json!({"session": stranger})),
        (
            "send_input",
            json!({"session": stranger, "text": "echo hi", "submit": true}),
        ),
    ] {
        let result = call(&app, &token, tool, args).await;
        assert_eq!(
            result["isError"], false,
            "{tool} must reach a session in the same bucket: {result}"
        );
    }

    // Acting on it does not: those destroy work, and the session
    // belongs to whoever spawned it.
    for (tool, args) in [
        ("interrupt_session", json!({"session": stranger})),
        ("resume_session", json!({"session": stranger})),
        ("kill_session", json!({"session": stranger})),
    ] {
        let result = call(&app, &token, tool, args).await;
        assert_eq!(result["isError"], true, "{tool} must refuse");
        assert!(
            call_text(&result).contains("was not spawned by this supervisor"),
            "{tool}: {result}"
        );
    }
    assert!(
        session_of(&env, stranger).state.is_live(),
        "the stranger session is untouched"
    );
}

#[tokio::test]
/// The bucket is the boundary, not merely a wider one: a session
/// outside it stays unreachable by every tool, read included.
async fn supervisor_reads_stop_at_the_bucket_boundary() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let other_bucket = env.daemon.create_bucket("other-bucket").unwrap();
    let other_project = env
        .daemon
        .create_project(other_bucket, "other", env.project_root().to_str().unwrap())
        .unwrap();
    let outsider = env
        .daemon
        .spawn_session(
            other_project,
            pm_protocol::domain::AgentKind::Test,
            "test task",
            "outsider",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();

    for (tool, args) in [
        ("session_status", json!({"session": outsider})),
        ("read_terminal", json!({"session": outsider})),
        (
            "send_input",
            json!({"session": outsider, "text": "echo hi", "submit": true}),
        ),
    ] {
        let result = call(&app, &token, tool, args).await;
        assert_eq!(result["isError"], true, "{tool} must refuse");
        assert!(
            call_text(&result).contains("not in this supervisor's bucket"),
            "{tool}: {result}"
        );
    }
}

#[tokio::test]
async fn supervisor_resumes_an_ended_child_and_audits_the_item() {
    let mut env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:resume", "resume me");

    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "test",
            "prompt": "keep the conversation",
            "item": item_id
        }),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let reply: Value = serde_json::from_str(call_text(&result)).unwrap();
    let child = reply["session_id"].as_u64().unwrap();
    let baseline = call(&app, &token, "wait_sessions", json!({"sessions": [child]})).await;
    let baseline: Value = serde_json::from_str(call_text(&baseline)).unwrap();
    let before_exit_cursor = baseline["cursor"].as_u64().unwrap();

    env.daemon.kill_session(child).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);
    assert!(!session_of(&env, child).state.is_live());
    let exited = call(
        &app,
        &token,
        "wait_sessions",
        json!({"sessions": [child], "after_cursor": before_exit_cursor}),
    )
    .await;
    let exited: Value = serde_json::from_str(call_text(&exited)).unwrap();
    assert_eq!(exited["changes"][0]["to"], "exited");
    assert_eq!(exited["changes"][0]["generation"], 1);
    let exited_cursor = exited["cursor"].as_u64().unwrap();
    env.daemon
        .set_instructions(
            bucket_id,
            Some(env.project_id),
            pm_protocol::domain::InstructionTarget::Worker,
            "resume policy",
            0,
            "",
            None,
        )
        .unwrap();

    let result = call(&app, &token, "resume_session", json!({"session": child})).await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(call_text(&result), format!("session {child} resumed"));
    // A resume submits no prompt, so the child is starting up rather
    // than working on anything.
    assert_eq!(session_of(&env, child).state, SessionState::Starting);
    let resumed = call(
        &app,
        &token,
        "wait_sessions",
        json!({"sessions": [child], "after_cursor": exited_cursor}),
    )
    .await;
    let resumed: Value = serde_json::from_str(call_text(&resumed)).unwrap();
    let resumed_changes = resumed["changes"].as_array().unwrap();
    assert_eq!(resumed_changes.last().unwrap()["to"], "starting");
    assert!(
        resumed_changes
            .iter()
            .all(|change| change["generation"] == 2),
        "{resumed}"
    );
    assert_eq!(resumed["sessions"][0]["generation"], 2);
    assert!(env
        .daemon
        .instruction_snapshot(child, 2)
        .unwrap()
        .0
        .contains("resume policy"));

    let (replay, mut rx, _guard) = env.daemon.attach(child).await.unwrap();
    let expected = format!("READY resumed:testsess-{child}");
    await_output(&mut rx, replay.to_vec(), expected.as_bytes()).await;

    let notes = env.daemon.item_notes(bucket_of(&env), item_id).unwrap();
    assert!(
        notes
            .iter()
            .any(|n| n.text.contains(&format!("resumed session {child}"))),
        "resume audited: {notes:?}"
    );
}

#[tokio::test]
async fn supervisor_resume_rejects_a_live_child() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:live-resume", "still live");

    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "test",
            "prompt": "still working",
            "item": item_id
        }),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let reply: Value = serde_json::from_str(call_text(&result)).unwrap();
    let child = reply["session_id"].as_u64().unwrap();

    let result = call(&app, &token, "resume_session", json!({"session": child})).await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(call_text(&result).contains("still live"), "{result}");
    assert!(session_of(&env, child).state.is_live());
}

#[tokio::test]
async fn supervisor_send_input_distinguishes_busy_and_transport_failure() {
    let env = codex_tui_daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let item_id = seed_item(&env, bucket_of(&env), "it:input-failure", "input failure");
    let (enrollment, _) = env.daemon.create_worker_enrollment("remote").unwrap();
    let worker = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enrollment,
            credential: "",
            peer_key_hash: "key-remote",
            hostname: "remote",
            platform: "test",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/tmp",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    let mut rx = answering_host_requests(env.daemon.clone(), worker.worker_id, worker.rx);
    env.daemon
        .set_project_worker(env.project_id, Some(worker.worker_id))
        .unwrap();
    let spawned = call(
        &app,
        &token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "codex",
            "prompt": "remote input",
            "item": item_id
        }),
    )
    .await;
    assert_eq!(spawned["isError"], false, "{spawned}");
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();
    let (terminal_id, generation) = match rx.recv().await.unwrap() {
        ControllerMsg::Spawn {
            terminal_id,
            generation,
            ..
        } => (terminal_id, generation),
        message => panic!("unexpected worker command: {message:?}"),
    };
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(1);
    worker
        .link
        .connect_terminal_stream(terminal_id, generation, input_tx);

    // The channel holds one frame and nobody drains it: the paste of the
    // first call is accepted, and its deferred Enter hits backpressure.
    let partial = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "first\r\n", "submit": true}),
    )
    .await;
    assert_eq!(partial["isError"], true, "{partial}");
    assert_eq!(partial["structuredContent"]["delivery"], "partial");
    assert_eq!(
        partial["structuredContent"]["input_state"],
        "submit_undelivered"
    );
    assert_eq!(partial["structuredContent"]["reason"], "busy");
    assert_eq!(partial["structuredContent"]["retryable"], true);
    assert_eq!(partial["structuredContent"]["submitted"], false);
    assert_eq!(partial["structuredContent"]["bytes_queued"], 6);
    assert_eq!(partial["structuredContent"]["bytes_sent"], 0);

    let busy = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "second", "submit": true}),
    )
    .await;
    assert_eq!(busy["isError"], true, "{busy}");
    assert_eq!(busy["structuredContent"]["delivery"], "not_delivered");
    assert_eq!(busy["structuredContent"]["reason"], "busy");
    assert_eq!(busy["structuredContent"]["retryable"], true);
    let frame = input_rx.try_recv().unwrap();
    assert!(matches!(
        pm_protocol::terminal_frame::decode(&frame),
        Some(pm_protocol::terminal_frame::TerminalFrame::Input {
            generation: frame_generation,
            submitted: false,
            data: b"\x1b[200~first\x1b[201~",
        }) if frame_generation == generation
    ));
    assert!(input_rx.try_recv().is_err());

    drop(input_rx);
    let failed = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "third", "submit": true}),
    )
    .await;
    assert_eq!(failed["isError"], true, "{failed}");
    assert_eq!(failed["structuredContent"]["reason"], "transport_failure");
    assert_eq!(failed["structuredContent"]["retryable"], true);
}

/// The remote-worker fixture shared by the submission-confirmation tests:
/// a codex child spawned on a fake worker whose input frames the test
/// drains, driven to idle so a submitted input must prove a turn began.
async fn idle_codex_child_with_input_stream(
    env: &TestEnv,
    app: &axum::Router,
    token: &str,
) -> (u64, tokio::sync::mpsc::Receiver<bytes::Bytes>, u64) {
    let item_id = seed_item(env, bucket_of(env), "it:input-confirm", "input confirm");
    let (enrollment, _) = env.daemon.create_worker_enrollment("remote").unwrap();
    let worker = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enrollment,
            credential: "",
            peer_key_hash: "key-remote",
            hostname: "remote",
            platform: "test",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/tmp",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    let mut rx = answering_host_requests(env.daemon.clone(), worker.worker_id, worker.rx);
    env.daemon
        .set_project_worker(env.project_id, Some(worker.worker_id))
        .unwrap();
    let spawned = call(
        app,
        token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "codex",
            "prompt": "confirm input",
            "item": item_id
        }),
    )
    .await;
    assert_eq!(spawned["isError"], false, "{spawned}");
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();
    let (terminal_id, generation) = match rx.recv().await.unwrap() {
        ControllerMsg::Spawn {
            terminal_id,
            generation,
            ..
        } => (terminal_id, generation),
        message => panic!("unexpected worker command: {message:?}"),
    };
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(4);
    worker
        .link
        .connect_terminal_stream(terminal_id, generation, input_tx);
    let child_token = env.daemon.session_token(child).unwrap().unwrap();
    env.daemon
        .handle_hook_event(
            &child_token,
            HookKind::Started,
            "",
            "agent-confirm",
            "",
            false,
        )
        .unwrap();
    env.daemon
        .handle_hook_event(
            &child_token,
            HookKind::TurnEnded,
            "",
            "agent-confirm",
            "",
            false,
        )
        .unwrap();
    assert_eq!(session_of(env, child).state, SessionState::Idle);
    (child, input_rx, generation)
}

fn input_frame_data(frame: &bytes::Bytes) -> Vec<u8> {
    match pm_protocol::terminal_frame::decode(frame) {
        Some(pm_protocol::terminal_frame::TerminalFrame::Input { data, .. }) => data.to_vec(),
        other => panic!("unexpected terminal frame: {other:?}"),
    }
}

#[tokio::test]
async fn send_input_reports_submitted_once_a_turn_begins() {
    let env = codex_tui_daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let (child, mut input_rx, _) = idle_codex_child_with_input_stream(&env, &app, &token).await;
    let child_token = env.daemon.session_token(child).unwrap().unwrap();

    let daemon = Arc::clone(&env.daemon);
    let driver = async move {
        let paste = input_rx.recv().await.unwrap();
        assert_eq!(
            input_frame_data(&paste),
            b"\x1b[200~fix the failing test\x1b[201~"
        );
        let enter = input_rx.recv().await.unwrap();
        assert_eq!(input_frame_data(&enter), b"\r");
        daemon
            .handle_hook_event(
                &child_token,
                HookKind::PromptSubmitted,
                "",
                "agent-confirm",
                "",
                false,
            )
            .unwrap();
        input_rx
    };
    let (result, mut input_rx) = tokio::join!(
        call(
            &app,
            &token,
            "send_input",
            json!({"session": child, "text": "fix the failing test", "submit": true}),
        ),
        driver
    );
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["delivery"], "delivered");
    assert_eq!(result["structuredContent"]["input_state"], "submitted");
    assert_eq!(result["structuredContent"]["submitted"], true);
    assert!(
        input_rx.try_recv().is_err(),
        "no extra Enter after a confirmed turn"
    );
    assert_eq!(session_of(&env, child).state, SessionState::Working);
}

#[tokio::test]
async fn send_input_retries_enter_and_reports_unconfirmed_without_a_turn() {
    let env = codex_tui_daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let (child, mut input_rx, _) = idle_codex_child_with_input_stream(&env, &app, &token).await;

    let driver = async move {
        let paste = input_rx.recv().await.unwrap();
        assert_eq!(
            input_frame_data(&paste),
            b"\x1b[200~stalled message\x1b[201~"
        );
        let enter = input_rx.recv().await.unwrap();
        assert_eq!(input_frame_data(&enter), b"\r");
        // No prompt-submitted hook ever fires: the daemon re-sends the
        // Enter once, then reports the submission unconfirmed.
        let retry = input_rx.recv().await.unwrap();
        assert_eq!(input_frame_data(&retry), b"\r");
        input_rx
    };
    let (result, mut input_rx) = tokio::join!(
        call(
            &app,
            &token,
            "send_input",
            json!({"session": child, "text": "stalled message", "submit": true}),
        ),
        driver
    );
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["delivery"], "queued");
    assert_eq!(
        result["structuredContent"]["input_state"],
        "submission_unconfirmed"
    );
    assert_eq!(result["structuredContent"]["submitted"], false);
    assert_eq!(result["structuredContent"]["retryable"], true);
    assert_eq!(
        result["structuredContent"]["reason"],
        "submission_unconfirmed"
    );
    assert!(input_rx.try_recv().is_err(), "exactly one Enter retry");
}

#[tokio::test]
async fn supervisor_input_connects_a_terminal_stream_nobody_has_opened() {
    let env = codex_tui_daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let item_id = seed_item(&env, bucket_of(&env), "it:input-attach", "input attach");
    let (enrollment, _) = env.daemon.create_worker_enrollment("remote").unwrap();
    let worker = env
        .daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &enrollment,
            credential: "",
            peer_key_hash: "key-remote",
            hostname: "remote",
            platform: "test",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/tmp",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap();
    let worker_rx = answering_host_requests(env.daemon.clone(), worker.worker_id, worker.rx);
    env.daemon
        .set_project_worker(env.project_id, Some(worker.worker_id))
        .unwrap();
    let spawned = call(
        &app,
        &token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "codex",
            "prompt": "unopened session",
            "item": item_id
        }),
    )
    .await;
    assert_eq!(spawned["isError"], false, "{spawned}");
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();

    let mut worker_rx = worker_rx;
    let worker_link = worker.link;
    match worker_rx.recv().await.unwrap() {
        ControllerMsg::Spawn { .. } => {}
        message => panic!("unexpected worker command: {message:?}"),
    }

    // Deliberately never connect a terminal stream: this is a session the
    // supervisor spawned and nobody has opened, which is the ordinary case.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(2);
    let deliver = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "answer", "submit": true}),
    );
    let attach = async {
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), worker_rx.recv())
            .await
            .expect("input to an unopened session must request a terminal stream")
            .unwrap();
        match request {
            ControllerMsg::TerminalAttach {
                terminal_id,
                generation,
                ..
            } => {
                worker_link.connect_terminal_stream(terminal_id, generation, input_tx);
                worker_link.feed_terminal_output(
                    terminal_id,
                    generation,
                    pm_protocol::terminal_frame::FLAG_REPLAY
                        | pm_protocol::terminal_frame::FLAG_REPLAY_START
                        | pm_protocol::terminal_frame::FLAG_REPLAY_END,
                    bytes::Bytes::new(),
                );
                generation
            }
            message => panic!("expected the daemon to ask the worker for a stream: {message:?}"),
        }
    };
    let (delivered, generation) = tokio::join!(deliver, attach);

    assert_eq!(delivered["isError"], false, "{delivered}");
    assert_eq!(delivered["structuredContent"]["delivery"], "queued");
    let frame = input_rx.try_recv().expect("input reached the worker");
    assert!(matches!(
        pm_protocol::terminal_frame::decode(&frame),
        Some(pm_protocol::terminal_frame::TerminalFrame::Input {
            generation: frame_generation,
            submitted: false,
            data: b"\x1b[200~answer\x1b[201~",
        }) if frame_generation == generation
    ));
    let enter = input_rx
        .try_recv()
        .expect("the deferred Enter reached the worker");
    assert!(matches!(
        pm_protocol::terminal_frame::decode(&enter),
        Some(pm_protocol::terminal_frame::TerminalFrame::Input {
            generation: frame_generation,
            submitted: false,
            data: b"\r",
        }) if frame_generation == generation
    ));
}

#[tokio::test]
async fn supervisor_long_prompt_submits_through_a_real_codex_like_pty_tui() {
    let env = codex_tui_daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let item_id = seed_item(&env, bucket_of(&env), "it:tui-input", "TUI input");
    let spawned = call(
        &app,
        &token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "codex",
            "prompt": "real PTY composer",
            "item": item_id
        }),
    )
    .await;
    assert_eq!(spawned["isError"], false, "{spawned}");
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();
    let (replay, mut rx, _guard) = env.daemon.attach(child).await.unwrap();
    let acc = await_output(&mut rx, replay.to_vec(), b"COMPOSER READY").await;

    let prompt = format!("LONG-PROMPT-MARKER {}", "x".repeat(683));
    assert_eq!(prompt.len(), 702);
    let child_token = env.daemon.session_token(child).unwrap().unwrap();
    env.daemon
        .handle_hook_event(&child_token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    assert_eq!(session_of(&env, child).state, SessionState::Idle);

    // The composer folds an Enter that shares a read chunk with the
    // paste-end marker, so only the deferred Enter can submit; the hook a
    // real agent fires on submission is mirrored here so the daemon can
    // confirm the turn.
    let daemon = Arc::clone(&env.daemon);
    let hook_token = child_token.clone();
    let driver = async {
        let acc = await_output(&mut rx, acc, b"SUBMITTED 702 LONG-PROMPT-MARKER").await;
        daemon
            .handle_hook_event(&hook_token, HookKind::PromptSubmitted, "", "", "", false)
            .unwrap();
        acc
    };
    let (submitted, mut acc) = tokio::join!(
        call(
            &app,
            &token,
            "send_input",
            json!({"session": child, "text": prompt, "submit": true}),
        ),
        driver
    );
    assert_eq!(submitted["isError"], false, "{submitted}");
    assert_eq!(submitted["structuredContent"]["delivery"], "delivered");
    assert_eq!(submitted["structuredContent"]["input_state"], "submitted");
    assert_eq!(submitted["structuredContent"]["submitted"], true);
    assert_eq!(submitted["structuredContent"]["bytes_queued"], 703);
    assert_eq!(submitted["structuredContent"]["bytes_sent"], 0);
    assert!(!String::from_utf8_lossy(&acc).contains("STAGED 703"));
    assert_eq!(session_of(&env, child).state, SessionState::Working);

    let reserved = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "unsafe\u{1b}[201~tail", "submit": true}),
    )
    .await;
    assert_eq!(reserved["isError"], true, "{reserved}");
    assert_eq!(reserved["structuredContent"]["reason"], "invalid_input");
    assert_eq!(reserved["structuredContent"]["bytes_queued"], 0);

    env.daemon
        .handle_hook_event(
            &child_token,
            HookKind::NeedsInput,
            "approval",
            "",
            "",
            false,
        )
        .unwrap();
    assert_eq!(session_of(&env, child).state, SessionState::NeedsInput);
    let needs_input = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "approve", "submit": true}),
    )
    .await;
    assert_eq!(needs_input["isError"], false, "{needs_input}");
    acc = await_output(&mut rx, acc, b"SUBMITTED 7 approve").await;

    let multiline = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "line one\nline two\r\n", "submit": true}),
    )
    .await;
    assert_eq!(multiline["isError"], false, "{multiline}");
    assert_eq!(multiline["structuredContent"]["bytes_queued"], 18);
    acc = await_output(&mut rx, acc, b"SUBMITTED 17 line one\nline two").await;

    let empty_submit = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "", "submit": true}),
    )
    .await;
    assert_eq!(empty_submit["isError"], false, "{empty_submit}");
    let _ = await_output(&mut rx, acc, b"SUBMITTED 0 ").await;
}

/// The inbox directory the scripted adapter resolves against is named by
/// a process-wide environment variable, so the tests that point it
/// somewhere take turns.
static INBOX_ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Waits briefly for a frame containing `needle`. The socket is read by
/// a task of its own, so a write that has returned has not necessarily
/// been collected yet.
async fn await_frame(frames: &Arc<std::sync::Mutex<Vec<String>>>, needle: &str) -> bool {
    for _ in 0..100 {
        if frames.lock().unwrap().iter().any(|f| f.contains(needle)) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
/// A blocked worker's question goes to its supervisor as something they
/// can answer, the answer comes back to the worker, and the state change
/// it also caused does not arrive as a second, redundant notice.
async fn a_blocked_worker_asks_its_supervisor_and_gets_an_answer() {
    let _lock = INBOX_ENV_MUTEX.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let frames = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let sup_token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let item_id = seed_item(&env, bucket_of(&env), "it:blocked", "blocked worker");
    let spawned = call(
        &app,
        &sup_token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "test",
            "prompt": "blocked worker",
            "item": item_id
        }),
    )
    .await;
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();
    let (replay, mut rx, _guard) = env.daemon.attach(child).await.unwrap();
    await_output(&mut rx, replay.to_vec(), b"READY").await;
    let child_token = env.daemon.session_token(child).unwrap().unwrap();

    // The question goes to the supervisor and the answer comes back to
    // the worker, so each end holds its own inbox, bound where that
    // session resolves it. One shared socket would collect a frame that
    // went to the wrong agent.
    for session in [sup, child] {
        let listener =
            tokio::net::UnixListener::bind(agent_inbox_path(&env, dir.path(), session)).unwrap();
        let collected = Arc::clone(&frames);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = Vec::new();
                if tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
                    .await
                    .is_ok()
                {
                    collected
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf).to_string());
                }
            }
        });
    }

    let blocked = call(
        &app,
        &child_token,
        "flag_blocked",
        json!({ "question": "which database should I migrate first?" }),
    )
    .await;
    assert_eq!(blocked["isError"], false, "{blocked}");
    let text = call_text(&blocked);
    assert!(
        text.contains("put to your supervisor as message"),
        "the worker must be told its question was asked: {text}"
    );
    let message_id = text
        .split("message ")
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .and_then(|id| id.trim().parse::<u64>().ok())
        .expect("a message id in the response");

    let message = env.daemon.agent_message(message_id).unwrap();
    assert_eq!(message.from_session_id, child);
    assert_eq!(message.to_session_id, sup);
    assert!(
        await_frame(&frames, "which database should I migrate first?").await,
        "the supervisor must receive the question: {:?}",
        frames.lock().unwrap()
    );

    // One event, one notice: the NeedsInput transition the block caused
    // must not also arrive through the reconciliation pass.
    let wakes = env
        .daemon
        .process_supervisor_wakes_at(pm_daemon::daemon::now_unix_ms() + 60_000)
        .await;
    assert!(
        wakes.iter().all(|w| !w.sessions.contains(&child)),
        "the question already reached the supervisor: {wakes:?}"
    );

    let answered = call(
        &app,
        &sup_token,
        "reply_message",
        json!({
            "message_id": message_id,
            "reply_token": message.reply_token.unwrap(),
            "body": "start with the billing database"
        }),
    )
    .await;
    assert_eq!(answered["isError"], false, "{answered}");
    assert!(
        await_frame(&frames, "start with the billing database").await,
        "the answer must reach the blocked worker: {:?}",
        frames.lock().unwrap()
    );
    std::env::remove_var(pm_adapters::TEST_INBOX_DIR_ENV);
}

#[tokio::test]
/// The whole round trip: a supervisor asks for an answer, the worker
/// spends its one capability to give one, and the answer reaches the
/// supervisor both as a message and through the wait.
async fn a_message_asking_for_a_reply_gets_exactly_one_answer_back() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let sup_token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let item_id = seed_item(&env, bucket_of(&env), "it:reply", "reply routing");
    let spawned = call(
        &app,
        &sup_token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "test",
            "prompt": "reply routing",
            "item": item_id
        }),
    )
    .await;
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();
    let (replay, mut rx, _guard) = env.daemon.attach(child).await.unwrap();
    await_output(&mut rx, replay.to_vec(), b"READY").await;

    let sent = call(
        &app,
        &sup_token,
        "send_input",
        json!({ "session": child, "text": "how far along are you?", "submit": true, "reply": true }),
    )
    .await;
    assert_eq!(sent["isError"], false, "{sent}");
    let message_id = sent["structuredContent"]["message_id"].as_u64().unwrap();

    // The worker has to be told how to answer, in the message itself.
    let message = env.daemon.agent_message(message_id).unwrap();
    let token = message.reply_token.clone().unwrap();
    assert_eq!(message.to_session_id, child);
    assert_eq!(message.from_session_id, sup);

    let child_token = env.daemon.session_token(child).unwrap().unwrap();
    // A token that is real but presented by the wrong session is not a
    // capability: it would let any transcript reader answer for a worker.
    let stolen = call(
        &app,
        &sup_token,
        "reply_message",
        json!({ "message_id": message_id, "reply_token": token, "body": "not mine to send" }),
    )
    .await;
    assert_eq!(stolen["isError"], true, "{stolen}");

    let answered = call(
        &app,
        &child_token,
        "reply_message",
        json!({ "message_id": message_id, "reply_token": token, "body": "two of three files" }),
    )
    .await;
    assert_eq!(answered["isError"], false, "{answered}");

    // One capability, one answer: a second attempt must not overwrite it.
    let again = call(
        &app,
        &child_token,
        "reply_message",
        json!({ "message_id": message_id, "reply_token": token, "body": "actually all three" }),
    )
    .await;
    assert_eq!(again["isError"], true, "{again}");

    let waited = call(
        &app,
        &sup_token,
        "await_reply",
        json!({ "message_id": message_id, "wait_seconds": 5 }),
    )
    .await;
    assert_eq!(waited["isError"], false, "{waited}");
    assert!(
        call_text(&waited).contains("two of three files"),
        "{}",
        call_text(&waited)
    );

    // Reading is not consuming: a sender that lost the first response
    // must be able to ask again and still get the answer.
    let waited_again = call(
        &app,
        &sup_token,
        "await_reply",
        json!({ "message_id": message_id, "wait_seconds": 5 }),
    )
    .await;
    assert!(
        call_text(&waited_again).contains("two of three files"),
        "{}",
        call_text(&waited_again)
    );
}

#[tokio::test]
/// A reply carries no capability of its own, so an exchange is one round
/// trip and two sessions cannot talk each other in circles.
async fn a_reply_cannot_itself_be_replied_to() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let sup_token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let item_id = seed_item(&env, bucket_of(&env), "it:loop", "loop guard");
    let spawned = call(
        &app,
        &sup_token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "test",
            "prompt": "loop guard",
            "item": item_id
        }),
    )
    .await;
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();
    let (replay, mut rx, _guard) = env.daemon.attach(child).await.unwrap();
    await_output(&mut rx, replay.to_vec(), b"READY").await;

    let sent = call(
        &app,
        &sup_token,
        "send_input",
        json!({ "session": child, "text": "status?", "submit": true, "reply": true }),
    )
    .await;
    let message_id = sent["structuredContent"]["message_id"].as_u64().unwrap();
    let token = env
        .daemon
        .agent_message(message_id)
        .unwrap()
        .reply_token
        .unwrap();
    let child_token = env.daemon.session_token(child).unwrap().unwrap();
    call(
        &app,
        &child_token,
        "reply_message",
        json!({ "message_id": message_id, "reply_token": token, "body": "fine" }),
    )
    .await;

    // The answer went back to the supervisor as a message of its own,
    // and that message must not be answerable.
    let delivered = env.daemon.agent_message(message_id + 1);
    assert!(
        delivered.is_err(),
        "a reply must not create a message that asks for another reply"
    );
}

#[tokio::test]
/// A supervisor's message must take the agent's own channel when it has
/// one, and must not fall back to typing at the terminal after it did.
/// Two deliveries of one message is the failure this guards.
async fn send_input_prefers_the_agents_own_inbox_over_its_terminal() {
    let _lock = INBOX_ENV_MUTEX.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let item_id = seed_item(&env, bucket_of(&env), "it:inbox", "inbox routing");
    let spawned = call(
        &app,
        &token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "test",
            "prompt": "inbox routing",
            "item": item_id
        }),
    )
    .await;
    assert_eq!(spawned["isError"], false, "{spawned}");
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();
    let (replay, mut rx, _guard) = env.daemon.attach(child).await.unwrap();
    let before = await_output(&mut rx, replay.to_vec(), b"READY").await;

    let listener =
        tokio::net::UnixListener::bind(agent_inbox_path(&env, dir.path(), child)).unwrap();
    let received = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buf)
            .await
            .unwrap();
        String::from_utf8(buf).unwrap()
    });

    let sent = call(
        &app,
        &token,
        "send_input",
        json!({ "session": child, "text": "take the inbox", "submit": true }),
    )
    .await;
    assert_eq!(sent["isError"], false, "{sent}");
    assert_eq!(sent["structuredContent"]["delivery"], "delivered", "{sent}");
    assert_eq!(sent["structuredContent"]["submitted"], true, "{sent}");
    assert!(
        call_text(&sent).contains("claude-socket"),
        "the outcome must name the channel that carried it: {}",
        call_text(&sent)
    );

    let frames = received.await.unwrap();
    let message: Value = serde_json::from_str(frames.lines().next().unwrap()).unwrap();
    assert_eq!(message["type"], "user");
    assert_eq!(message["message"]["content"], "take the inbox");

    // Nothing may reach the terminal: a message delivered twice is a
    // worker that acts on it twice.
    let mut tail = before;
    while let Ok(Ok(chunk)) =
        tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await
    {
        tail.extend_from_slice(&chunk);
    }
    let tail = String::from_utf8_lossy(&tail);
    assert!(
        !tail.contains("take the inbox"),
        "the terminal saw the message too: {tail}"
    );
    std::env::remove_var(pm_adapters::TEST_INBOX_DIR_ENV);
}

#[tokio::test]
async fn supervisor_send_input_preserves_legacy_and_submits_exactly_once() {
    let mut env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let item_id = seed_item(&env, bucket_of(&env), "it:input", "input semantics");
    let spawned = call(
        &app,
        &token,
        "spawn_session",
        json!({
            "project": env.project_id,
            "agent": "test",
            "prompt": "input semantics",
            "item": item_id
        }),
    )
    .await;
    let child = serde_json::from_str::<Value>(call_text(&spawned)).unwrap()["session_id"]
        .as_u64()
        .unwrap();
    let (replay, mut rx, _guard) = env.daemon.attach(child).await.unwrap();
    let mut acc = await_output(&mut rx, replay.to_vec(), b"READY").await;
    assert_eq!(session_of(&env, child).state, SessionState::Working);

    let empty = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": ""}),
    )
    .await;
    assert_eq!(empty["isError"], true, "{empty}");
    assert_eq!(empty["structuredContent"]["reason"], "invalid_input");

    let staged = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "echo staged"}),
    )
    .await;
    assert_eq!(staged["isError"], false, "{staged}");
    assert_eq!(staged["structuredContent"]["input_state"], "input_queued");
    assert_eq!(staged["structuredContent"]["submitted"], false);
    assert_eq!(staged["structuredContent"]["bytes_queued"], 11);
    assert_eq!(staged["structuredContent"]["bytes_sent"], 0);
    assert!(!staged.to_string().contains("echo staged"));

    let enter = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "", "submit": true}),
    )
    .await;
    assert_eq!(enter["isError"], false, "{enter}");
    assert_eq!(
        enter["structuredContent"]["input_state"],
        "queued_behind_turn"
    );
    assert_eq!(enter["structuredContent"]["reason"], "agent_mid_turn");
    assert_eq!(enter["structuredContent"]["submitted"], false);
    assert_eq!(enter["structuredContent"]["bytes_queued"], 1);
    acc = await_output(&mut rx, acc, b"OUT staged").await;

    for (text, marker) in [
        ("echo legacy-cr\r", "OUT legacy-cr"),
        ("echo legacy-lf\n", "OUT legacy-lf"),
    ] {
        let result = call(
            &app,
            &token,
            "send_input",
            json!({"session": child, "text": text, "submit": false}),
        )
        .await;
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(result["structuredContent"]["submit_requested"], false);
        acc = await_output(&mut rx, acc, marker.as_bytes()).await;
    }

    let normalized = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "echo normalized\r\n\r", "submit": true}),
    )
    .await;
    assert_eq!(normalized["isError"], false, "{normalized}");
    assert_eq!(normalized["structuredContent"]["bytes_queued"], 16);
    acc = await_output(&mut rx, acc, b"OUT normalized").await;

    let multiline = call(
        &app,
        &token,
        "send_input",
        json!({
            "session": child,
            "text": "echo first\necho second",
            "submit": true
        }),
    )
    .await;
    assert_eq!(multiline["isError"], false, "{multiline}");
    acc = await_output(&mut rx, acc, b"OUT second").await;
    assert!(String::from_utf8_lossy(&acc).contains("OUT first"));

    let unicode = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "echo szív", "submit": true}),
    )
    .await;
    assert_eq!(unicode["structuredContent"]["bytes_queued"], 11);
    acc = await_output(&mut rx, acc, "OUT szív".as_bytes()).await;

    let too_large = call(
        &app,
        &token,
        "send_input",
        json!({
            "session": child,
            "text": "x".repeat(pm_daemon::daemon::SUPERVISOR_INPUT_MAX),
            "submit": true
        }),
    )
    .await;
    assert_eq!(too_large["isError"], true, "{too_large}");
    assert_eq!(too_large["structuredContent"]["delivery"], "not_delivered");
    assert_eq!(too_large["structuredContent"]["reason"], "input_too_large");

    let child_token = env.daemon.session_token(child).unwrap().unwrap();
    env.daemon
        .handle_hook_event(
            &child_token,
            pm_protocol::domain::HookKind::TurnEnded,
            "",
            "",
            "",
            false,
        )
        .unwrap();
    assert_eq!(session_of(&env, child).state, SessionState::Idle);
    let idle = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "echo idle", "submit": true}),
    )
    .await;
    assert_eq!(idle["isError"], false, "{idle}");
    acc = await_output(&mut rx, acc, b"OUT idle").await;

    let exit = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "exit 0", "submit": true}),
    )
    .await;
    assert_eq!(exit["isError"], false, "{exit}");
    let exited = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exited);
    let ended = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "after", "submit": true}),
    )
    .await;
    assert_eq!(ended["isError"], true, "{ended}");
    assert_eq!(ended["structuredContent"]["reason"], "session_ended");
    assert_eq!(ended["structuredContent"]["retryable"], false);

    let output = String::from_utf8_lossy(&acc);
    assert_eq!(output.matches("OUT normalized").count(), 1, "{output}");
    let notes = env.daemon.item_notes(bucket_of(&env), item_id).unwrap();
    assert!(notes
        .iter()
        .any(|note| note.text.contains("(input_queued)")));
    assert!(notes
        .iter()
        .any(|note| note.text.contains("(submission_requested)")));
}

#[tokio::test]
async fn supervisor_steers_a_child_end_to_end() {
    let mut env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:steer", "steer me");

    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
        "project": env.project_id, "agent": "test", "prompt": "steer the task",
        "item": item_id}),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let reply: Value = serde_json::from_str(call_text(&result)).unwrap();
    let child = reply["session_id"].as_u64().unwrap();

    let (replay, mut rx, _guard) = env.daemon.attach(child).await.unwrap();
    let acc = await_output(&mut rx, replay.to_vec(), b"READY").await;

    let result = call(&app, &token, "session_status", json!({"session": child})).await;
    assert_eq!(result["isError"], false, "{result}");
    let status: Value = serde_json::from_str(call_text(&result)).unwrap();
    assert_eq!(status["id"].as_u64(), Some(child));
    assert_eq!(status["spawned_by_session_id"].as_u64(), Some(sup));
    assert!(
        status["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["bucket_id"] == bucket_of(&env) && value["id"] == item_id),
        "{status}"
    );

    let result = call(&app, &token, "read_terminal", json!({"session": child})).await;
    assert_eq!(result["isError"], false, "{result}");
    assert!(
        call_text(&result).contains("READY steer the task"),
        "{result}"
    );

    let result = call(
        &app,
        &token,
        "send_input",
        json!({"session": child, "text": "echo steered\r"}),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["delivery"], "queued");
    assert_eq!(
        result["structuredContent"]["input_state"],
        "submission_requested"
    );
    assert_eq!(result["structuredContent"]["submit_requested"], false);
    assert_eq!(result["structuredContent"]["submitted"], false);
    assert_eq!(result["structuredContent"]["bytes_queued"], 13);
    assert_eq!(result["structuredContent"]["bytes_sent"], 0);
    let acc = await_output(&mut rx, acc, b"OUT steered").await;
    drop(acc);

    let result = call(&app, &token, "read_terminal", json!({"session": child})).await;
    assert!(call_text(&result).contains("OUT steered"), "{result}");

    let result = call(&app, &token, "kill_session", json!({"session": child})).await;
    assert_eq!(result["isError"], false, "{result}");
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);
    assert!(!session_of(&env, child).state.is_live());

    let notes = env.daemon.item_notes(bucket_of(&env), item_id).unwrap();
    assert!(
        notes
            .iter()
            .any(|n| n.text.contains("(submission_requested)")),
        "input audited: {notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.text.contains(&format!("killed session {child}"))),
        "kill audited: {notes:?}"
    );
}

#[tokio::test]
async fn supervisor_spawns_are_capped_by_the_setting() {
    let mut env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:cap", "capped");

    let key = pm_daemon::daemon::SETTING_SUPERVISOR_MAX_CHILDREN;
    assert!(env.daemon.set_setting(key, Some("0")).is_err());
    assert!(env.daemon.set_setting(key, Some("many")).is_err());
    env.daemon.set_setting(key, Some("1")).unwrap();

    let spawn_args = json!({
        "project": env.project_id, "agent": "test", "prompt": "work", "item": item_id});
    let result = call(&app, &token, "spawn_session", spawn_args.clone()).await;
    assert_eq!(result["isError"], false, "{result}");
    let reply: Value = serde_json::from_str(call_text(&result)).unwrap();
    let child = reply["session_id"].as_u64().unwrap();

    let result = call(&app, &token, "spawn_session", spawn_args.clone()).await;
    assert_eq!(
        result["isError"], true,
        "the second live child exceeds the cap"
    );
    assert!(call_text(&result).contains("limit 1"), "{result}");

    env.daemon.kill_session(child).unwrap();
    let exit = tokio::time::timeout(TEST_TIMEOUT, env.exit_rx.recv())
        .await
        .unwrap()
        .unwrap();
    env.daemon.handle_session_exit(exit);

    let result = call(&app, &token, "spawn_session", spawn_args).await;
    assert_eq!(
        result["isError"], false,
        "an ended child frees the slot: {result}"
    );
}

#[tokio::test]
async fn supervisor_spawn_requires_bucket_local_identity_and_rejects_legacy_refs() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
        "project": "test-project", "agent": "test", "prompt": "x"}),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert!(call_text(&result).contains("item is required"), "{result}");

    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
        "project": "test-project", "agent": "test", "prompt": "x", "item": "it:nope"}),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert!(
        call_text(&result).contains("does not match any item"),
        "{result}"
    );

    let local_item = seed_item(&env, bucket_of(&env), "it:local", "here");
    let other_bucket = env.daemon.create_bucket("other").unwrap();
    let foreign_item = seed_item(&env, other_bucket, "it:foreign", "elsewhere");
    assert_eq!((local_item, foreign_item), (1, 1));

    // The foreign row's old database-global surrogate would be 2. It must not
    // be accepted as a public number in the supervisor's bucket.
    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
        "project": "test-project", "agent": "test", "prompt": "x", "item": 2}),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert!(
        call_text(&result).contains("does not exist in this session's bucket"),
        "{result}"
    );

    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({
        "project": "test-project", "agent": "test", "prompt": "x", "item": "pm:item/2"}),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert!(
        call_text(&result).contains("unqualified and unsupported"),
        "{result}"
    );

    let qualified_by_scope = call(
        &app,
        &token,
        "spawn_session",
        json!({
        "project": "test-project", "agent": "test", "prompt": "x", "item": local_item}),
    )
    .await;
    assert_eq!(qualified_by_scope["isError"], false, "{qualified_by_scope}");
    let spawned: Value = serde_json::from_str(call_text(&qualified_by_scope)).unwrap();
    assert_eq!(spawned["bucket_id"], bucket_of(&env));
    assert_eq!(
        spawned["item_ref"],
        format!("pm:item/{}/1", bucket_of(&env))
    );
}

#[tokio::test]
async fn supervisor_list_sessions_shows_the_bucket_newest_first() {
    let env = daemon_env();
    let first = spawn_test_session(&env, "p");
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let result = call(&app, &token, "list_sessions", json!({})).await;
    assert_eq!(result["isError"], false, "{result}");
    let reply: Value = serde_json::from_str(call_text(&result)).unwrap();
    assert!(reply["cursor"].is_u64(), "{reply}");
    assert!(reply["sessions"][0]["generation"].is_u64(), "{reply}");
    let ids: Vec<u64> = reply["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, vec![sup, first], "newest first, both sessions listed");
}

#[tokio::test]
async fn wait_sessions_baseline_and_cursor_close_the_status_to_wait_race() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let child = spawn_supervised_child(&env, supervisor, "race child");
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let child_token = env.daemon.session_token(child).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let baseline = call(&app, &token, "wait_sessions", json!({"sessions": [child]})).await;
    assert_eq!(baseline["isError"], false, "{baseline}");
    let baseline: Value = serde_json::from_str(call_text(&baseline)).unwrap();
    assert_eq!(baseline["reason"], "changed");
    assert_eq!(baseline["baseline"], true);
    assert_eq!(baseline["changes"], json!([]));
    assert_eq!(baseline["sessions"][0]["generation"], 1);
    let cursor = baseline["cursor"].as_u64().unwrap();

    let status = call(&app, &token, "session_status", json!({"session": child})).await;
    let status: Value = serde_json::from_str(call_text(&status)).unwrap();
    assert_eq!(status["cursor"].as_u64(), Some(cursor));
    assert_eq!(status["generation"], 1);

    // The lifecycle transition lands after status but before the wait call.
    env.daemon
        .handle_hook_event(&child_token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    let raced = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        call(
            &app,
            &token,
            "wait_sessions",
            json!({"sessions": [child], "after_cursor": cursor, "timeout_ms": 55_000}),
        ),
    )
    .await
    .expect("recorded transition must return immediately");
    let raced: Value = serde_json::from_str(call_text(&raced)).unwrap();
    assert_eq!(raced["reason"], "changed");
    assert_eq!(raced["changes"][0]["session"].as_u64(), Some(child));
    assert_eq!(raced["changes"][0]["from"], "working");
    assert_eq!(raced["changes"][0]["to"], "idle");
    assert_eq!(raced["changes"][0]["generation"], 1);
    assert!(raced["changes"][0]["timestamp_unix_ms"].is_i64());
}

#[tokio::test]
async fn wait_sessions_observes_two_workers_one_completion_at_a_time_end_to_end() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let first_item = seed_item(&env, bucket_id, "wait:first", "first worker");
    let second_item = seed_item(&env, bucket_id, "wait:second", "second worker");
    let mut children = Vec::new();
    for (prompt, item) in [("first worker", first_item), ("second worker", second_item)] {
        let spawned = call(
            &app,
            &token,
            "spawn_session",
            json!({
                "project": env.project_id,
                "agent": "test",
                "prompt": prompt,
                "item": item,
            }),
        )
        .await;
        assert_eq!(spawned["isError"], false, "{spawned}");
        let spawned: Value = serde_json::from_str(call_text(&spawned)).unwrap();
        children.push(spawned["session_id"].as_u64().unwrap());
    }
    let [first, second] = children.as_slice() else {
        unreachable!()
    };
    let (first, second) = (*first, *second);
    let first_token = env.daemon.session_token(first).unwrap().unwrap();
    let second_token = env.daemon.session_token(second).unwrap().unwrap();

    let baseline = call(
        &app,
        &token,
        "wait_sessions",
        json!({"sessions": [first, second]}),
    )
    .await;
    let baseline: Value = serde_json::from_str(call_text(&baseline)).unwrap();
    let cursor = baseline["cursor"].as_u64().unwrap();

    let first_wait = {
        let app = app.clone();
        let token = token.clone();
        tokio::spawn(async move {
            call(
                &app,
                &token,
                "wait_sessions",
                json!({
                    "sessions": [first, second],
                    "after_cursor": cursor,
                    "states": ["idle", "needs-input", "failed", "exited"],
                    "timeout_ms": 55_000,
                }),
            )
            .await
        })
    };
    tokio::task::yield_now().await;
    env.daemon
        .handle_hook_event(&first_token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    let first_result = tokio::time::timeout(std::time::Duration::from_secs(1), first_wait)
        .await
        .unwrap()
        .unwrap();
    let first_result: Value = serde_json::from_str(call_text(&first_result)).unwrap();
    assert_eq!(first_result["changes"].as_array().unwrap().len(), 1);
    assert_eq!(first_result["changes"][0]["session"].as_u64(), Some(first));
    let next_cursor = first_result["cursor"].as_u64().unwrap();

    let second_wait = {
        let app = app.clone();
        let token = token.clone();
        tokio::spawn(async move {
            call(
                &app,
                &token,
                "wait_sessions",
                json!({
                    "sessions": [first, second],
                    "after_cursor": next_cursor,
                    "states": ["idle", "needs-input", "failed", "exited"],
                    "timeout_ms": 55_000,
                }),
            )
            .await
        })
    };
    tokio::task::yield_now().await;
    env.daemon
        .handle_hook_event(&second_token, HookKind::TurnEnded, "", "", "", false)
        .unwrap();
    let second_result = tokio::time::timeout(std::time::Duration::from_secs(1), second_wait)
        .await
        .unwrap()
        .unwrap();
    let second_result: Value = serde_json::from_str(call_text(&second_result)).unwrap();
    assert_eq!(second_result["changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        second_result["changes"][0]["session"].as_u64(),
        Some(second)
    );
}

#[tokio::test]
async fn wait_sessions_coalesces_simultaneous_events_and_deduplicates_provider_hooks() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let first = spawn_supervised_child(&env, supervisor, "first");
    let second = spawn_supervised_child(&env, supervisor, "second");
    let failed = spawn_supervised_child(&env, supervisor, "failed");
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let baseline = call(
        &app,
        &token,
        "wait_sessions",
        json!({"sessions": [first, second, failed]}),
    )
    .await;
    let baseline: Value = serde_json::from_str(call_text(&baseline)).unwrap();
    let cursor = baseline["cursor"].as_u64().unwrap();

    for session_id in [first, second] {
        let child_token = env.daemon.session_token(session_id).unwrap().unwrap();
        env.daemon
            .handle_hook_event(&child_token, HookKind::TurnEnded, "", "", "", false)
            .unwrap();
        // Remote Claude/Codex HookReport delivery can duplicate a local
        // provider hook; the normalized lifecycle event remains singular.
        env.daemon.apply_worker_message(
            LOCAL_WORKER_ID,
            WorkerMsg::HookReport {
                session_token: child_token,
                kind: HookKind::TurnEnded,
                detail: String::new(),
                agent_session_id: String::new(),
                transcript_path: String::new(),
                req_id: 0,
                background_work: false,
            },
        );
    }
    env.daemon.apply_worker_message(
        LOCAL_WORKER_ID,
        WorkerMsg::SessionState {
            session_id: failed,
            state: SessionState::Failed,
            detail: "provider failed".into(),
        },
    );
    let result = call(
        &app,
        &token,
        "wait_sessions",
        json!({"sessions": [first, second, failed], "after_cursor": cursor}),
    )
    .await;
    let result: Value = serde_json::from_str(call_text(&result)).unwrap();
    let changes = result["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 3, "{result}");
    assert_eq!(changes[2]["to"], "failed");
    assert_eq!(result["cursor"].as_u64(), Some(cursor + 3));
}

#[tokio::test]
/// A supervisor watches any session in its bucket, not only its own
/// children, so two supervisors can coordinate without one of them
/// having to poll.
async fn a_supervisor_waits_on_a_peers_session() {
    let env = daemon_env();
    let mine = spawn_supervisor(&env);
    let peer = spawn_supervisor(&env);
    let their_child = spawn_supervised_child(&env, peer, "peer's work");
    let token = env.daemon.session_token(mine).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let waited = call(
        &app,
        &token,
        "wait_sessions",
        json!({ "sessions": [their_child], "timeout_ms": 1_000 }),
    )
    .await;
    assert_eq!(waited["isError"], false, "{waited}");
    let baseline: Value = serde_json::from_str(call_text(&waited)).unwrap();
    assert_ne!(
        baseline["reason"], "unauthorized",
        "a peer's session in the same bucket must be watchable: {baseline}"
    );
    // The first call answers with the baseline, which must carry the
    // peer's session: seeing its state is the point.
    assert!(
        baseline["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["session"].as_u64() == Some(their_child)),
        "the baseline must include the peer's session: {baseline}"
    );

    // From that cursor the call actually waits rather than refusing.
    let cursor = baseline["cursor"].as_u64().unwrap();
    let waited = call(
        &app,
        &token,
        "wait_sessions",
        json!({ "sessions": [their_child], "after_cursor": cursor, "timeout_ms": 1_000 }),
    )
    .await;
    let waited: Value = serde_json::from_str(call_text(&waited)).unwrap();
    assert_eq!(
        waited["reason"], "timeout",
        "the call should have waited and found nothing, not refused: {waited}"
    );
}

#[tokio::test]
/// A hold longer than a client waits comes back as a transport error
/// rather than an answer, so an over-long request is clamped instead of
/// being honoured or refused.
async fn a_wait_longer_than_a_client_allows_is_clamped_not_refused() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let child = spawn_supervised_child(&env, supervisor, "clamped");
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let started = std::time::Instant::now();
    let waited = call(
        &app,
        &token,
        "wait_sessions",
        json!({ "sessions": [child], "timeout_ms": 300_000 }),
    )
    .await;
    assert_eq!(waited["isError"], false, "{waited}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the wait must return inside what a client allows, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn wait_sessions_returns_timeout_cancelled_unauthorized_and_missing_without_activity() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let child = spawn_supervised_child(&env, supervisor, "watched");
    // A session a supervisor may not watch is one outside its bucket:
    // inside it, waiting follows the same boundary as listing.
    let other_bucket = env.daemon.create_bucket("other-bucket").unwrap();
    let other_project = env
        .daemon
        .create_project(other_bucket, "other", env.project_root().to_str().unwrap())
        .unwrap();
    let stranger = env
        .daemon
        .spawn_session(
            other_project,
            pm_protocol::domain::AgentKind::Test,
            "test task",
            "stranger",
            None,
            pm_protocol::domain::PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let supervisor_token = token.clone();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let baseline = call(&app, &token, "wait_sessions", json!({"sessions": [child]})).await;
    let baseline: Value = serde_json::from_str(call_text(&baseline)).unwrap();
    let cursor = baseline["cursor"].as_u64().unwrap();

    let before = session_of(&env, supervisor).last_agent_activity_at_unix_ms;
    let timed_out = call(
        &app,
        &token,
        "wait_sessions",
        json!({"sessions": [child], "after_cursor": cursor, "timeout_ms": 1}),
    )
    .await;
    let timed_out: Value = serde_json::from_str(call_text(&timed_out)).unwrap();
    assert_eq!(timed_out["reason"], "timeout");
    assert_eq!(
        session_of(&env, supervisor).last_agent_activity_at_unix_ms,
        before,
        "waiting must not manufacture agent activity"
    );

    let unauthorized = call(
        &app,
        &token,
        "wait_sessions",
        json!({"sessions": [stranger], "after_cursor": cursor}),
    )
    .await;
    let unauthorized: Value = serde_json::from_str(call_text(&unauthorized)).unwrap();
    assert_eq!(unauthorized["reason"], "unauthorized");
    assert_eq!(unauthorized["session"].as_u64(), Some(stranger));

    let missing = call(
        &app,
        &token,
        "wait_sessions",
        json!({"sessions": [u64::MAX], "after_cursor": cursor}),
    )
    .await;
    let missing: Value = serde_json::from_str(call_text(&missing)).unwrap();
    assert_eq!(missing["reason"], "missing");

    let cancel_wait = {
        let app = app.clone();
        let token = token.clone();
        tokio::spawn(async move {
            call(
                &app,
                &token,
                "wait_sessions",
                json!({"sessions": [child], "after_cursor": cursor, "timeout_ms": 55_000}),
            )
            .await
        })
    };
    tokio::task::yield_now().await;
    env.daemon
        .handle_hook_event(
            &supervisor_token,
            HookKind::PromptSubmitted,
            "",
            "",
            "",
            false,
        )
        .unwrap();
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(1), cancel_wait)
        .await
        .expect("new prompt must cancel promptly")
        .unwrap();
    let cancelled: Value = serde_json::from_str(call_text(&cancelled)).unwrap();
    assert_eq!(cancelled["reason"], "cancelled");
}

#[tokio::test]
async fn wait_sessions_reauthorizes_a_pending_call_after_live_demotion() {
    let env = daemon_env();
    let supervisor = spawn_supervisor(&env);
    let child = spawn_supervised_child(&env, supervisor, "watched");
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let baseline = call(&app, &token, "wait_sessions", json!({"sessions": [child]})).await;
    let baseline: Value = serde_json::from_str(call_text(&baseline)).unwrap();
    let cursor = baseline["cursor"].as_u64().unwrap();

    let pending = {
        let app = app.clone();
        let token = token.clone();
        tokio::spawn(async move {
            call(
                &app,
                &token,
                "wait_sessions",
                json!({"sessions": [child], "after_cursor": cursor, "timeout_ms": 55_000}),
            )
            .await
        })
    };
    tokio::task::yield_now().await;
    env.daemon
        .update_session_apis(supervisor, None, Some(false))
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), pending)
        .await
        .expect("live demotion must terminate the wait")
        .unwrap();
    let result: Value = serde_json::from_str(call_text(&result)).unwrap();
    assert_eq!(result["reason"], "unauthorized");
}

#[tokio::test]
async fn runtime_api_toggle_grants_and_revokes_tools() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:runtime", "runtime grant");

    let spawn_args = json!({
        "project": env.project_id, "agent": "test", "prompt": "work", "item": item_id});
    let result = call(&app, &token, "spawn_session", spawn_args.clone()).await;
    assert_eq!(
        result["isError"], true,
        "no supervisor tools before the grant"
    );

    env.daemon
        .update_session_apis(session, None, Some(true))
        .unwrap();

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
    )
    .await;
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"spawn_session"),
        "granted session lists the supervisor tools: {names:?}"
    );

    let result = call(&app, &token, "spawn_session", spawn_args).await;
    assert_eq!(result["isError"], false, "grant applies live: {result}");
    let reply: Value = serde_json::from_str(call_text(&result)).unwrap();
    let child = reply["session_id"].as_u64().unwrap();
    assert_eq!(session_of(&env, child).spawned_by_session_id, Some(session));

    let reports = env.daemon.activity_reports(session).unwrap();
    assert!(
        reports
            .iter()
            .any(|r| r.payload.contains("supervisor tools enabled")),
        "the grant lands on the session timeline: {reports:?}"
    );

    env.daemon
        .update_session_apis(session, None, Some(false))
        .unwrap();
    let result = call(&app, &token, "session_status", json!({"session": child})).await;
    assert_eq!(
        result["isError"], true,
        "revocation applies on the next call"
    );
    assert!(call_text(&result).contains("disabled"), "{result}");

    env.daemon
        .update_session_apis(session, Some(false), None)
        .unwrap();
    let result = call(&app, &token, "list_items", json!({})).await;
    assert_eq!(
        result["isError"], true,
        "the items toggle flips independently"
    );

    let err = env
        .daemon
        .update_session_apis(9999, Some(true), None)
        .unwrap_err();
    assert!(err.to_string().contains("session"), "{err}");
}

#[tokio::test]
async fn session_apis_cannot_be_toggled_over_mcp() {
    let env = daemon_env();
    let session = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(session).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));

    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"update_session_apis",
            "arguments":{"session": session, "supervisor_api": true}}}),
    )
    .await;
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown tool"),
        "no MCP path can reach the API toggle: {body}"
    );
    assert!(!session_of(&env, session).supervisor_api);
}

fn enroll_host(env: &TestEnv, label: &str) -> pm_daemon::daemon::WorkerRegistration {
    let (token, _expires) = env.daemon.create_worker_enrollment(label).unwrap();
    env.daemon
        .register_worker_connection(pm_daemon::daemon::WorkerHello {
            enrollment_token: &token,
            credential: "",
            peer_key_hash: "key-host.local",
            hostname: "host.local",
            platform: "linux",
            pm_version: "",
            runtime: "",
            container: "",
            default_project_root: "/home/dev",
            live_sessions: &[],
            live_terminals: &[],
            protocol_version: pm_protocol::WORKER_PROTOCOL_VERSION,
        })
        .unwrap()
}

/// Allows the worker for the project and gives it a per-worker path, so
/// it is a selectable host without being the default.
fn configure_host(env: &TestEnv, worker_id: u64, path: &str) {
    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .iter()
        .find(|p| p.id == env.project_id)
        .cloned()
        .unwrap();
    env.daemon
        .set_bucket_workers(project.bucket_id, &[0, worker_id], 0, None)
        .unwrap();
    env.daemon
        .set_project_workers(env.project_id, &[0, worker_id], None)
        .unwrap();
    env.daemon
        .set_project_worker_path(env.project_id, worker_id, Some(path))
        .unwrap();
}

#[tokio::test]
async fn supervisor_spawn_host_dispatches_to_a_configured_host() {
    let env = daemon_env();
    let reg = enroll_host(&env, "mac-vm");
    let worker_id = reg.worker_id;
    let mut rx = answering_host_requests(env.daemon.clone(), worker_id, reg.rx);
    configure_host(&env, worker_id, "/mac/repo");
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:mac", "validate on the mac");

    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({"project": env.project_id, "agent": "test", "prompt": "run it",
            "item": item_id, "host": "mac-vm"}),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let reply: Value = serde_json::from_str(call_text(&result)).unwrap();
    let child = reply["session_id"].as_u64().unwrap();
    assert_eq!(reply["worker_id"].as_u64(), Some(worker_id));
    assert_eq!(reply["host"], "mac-vm");

    let s = session_of(&env, child);
    assert_eq!(s.worker_id, worker_id);
    assert_eq!(s.cwd, "/mac/repo", "the per-worker path applies");
    assert!(matches!(rx.recv().await, Some(ControllerMsg::Spawn { .. })));

    let notes = env.daemon.item_notes(bucket_id, item_id).unwrap();
    assert!(
        notes.iter().any(|n| n.text.contains("host=mac-vm")),
        "the spawn note names the chosen host: {notes:?}"
    );
}

#[tokio::test]
async fn supervisor_spawn_rejects_unconfigured_hosts_with_the_valid_list() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item_id = seed_item(&env, bucket_id, "it:nohost", "wrong host");
    let sessions_before = env.daemon.subscribe().0.sessions.len();

    for host in [json!("mac-vm"), json!(99)] {
        let result = call(
            &app,
            &token,
            "spawn_session",
            json!({"project": env.project_id, "agent": "test", "prompt": "run it",
                "item": item_id, "host": host}),
        )
        .await;
        assert_eq!(result["isError"], true, "{result}");
        let text = call_text(&result);
        assert!(
            text.contains("valid workers: local (id 0)"),
            "rejection lists the valid choices: {text}"
        );
    }
    let result = call(
        &app,
        &token,
        "spawn_session",
        json!({"project": env.project_id, "agent": "test", "prompt": "run it",
            "item": item_id, "host": {"bad": true}}),
    )
    .await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(call_text(&result).contains("worker id or name"));
    assert_eq!(
        env.daemon.subscribe().0.sessions.len(),
        sessions_before,
        "no session is created for a rejected host"
    );

    // The default host stays an accepted explicit choice.
    let ok = call(
        &app,
        &token,
        "spawn_session",
        json!({"project": env.project_id, "agent": "test", "prompt": "run it",
            "item": item_id, "host": "local"}),
    )
    .await;
    assert_eq!(ok["isError"], false, "{ok}");
    let reply: Value = serde_json::from_str(call_text(&ok)).unwrap();
    assert_eq!(reply["worker_id"].as_u64(), Some(LOCAL_WORKER_ID));
}

/// A project with no path of its own runs in the home directory of the
/// host it spawns on, so a spawn goes ahead instead of being refused.
/// The refusal that remains, for a host with no home to fall back to,
/// is covered where the host states are.
#[tokio::test]
async fn spawn_over_mcp_runs_a_pathless_project_in_the_hosts_home() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let blanked = env.daemon.create_project(bucket_id, "blanked", "").unwrap();
    let item = seed_item(&env, bucket_id, "path:unset", "no path");

    let spawned = call(
        &app,
        &token,
        "spawn_session",
        json!({"project":blanked,"agent":"test","prompt":"task","item":item}),
    )
    .await;
    assert_ne!(
        spawned["isError"],
        true,
        "a project with no path runs in the home directory: {}",
        call_text(&spawned)
    );
    let project = env
        .daemon
        .subscribe()
        .0
        .projects
        .into_iter()
        .find(|p| p.id == blanked)
        .unwrap();
    assert_eq!(
        env.daemon
            .effective_project_path(&project, pm_protocol::domain::LOCAL_WORKER_ID, None)
            .unwrap(),
        std::env::var("HOME").unwrap_or_default()
    );
}

#[tokio::test]
async fn session_status_reports_the_host_and_how_the_project_stands_on_it() {
    let env = daemon_env();
    let sup = spawn_supervisor(&env);
    let token = env.daemon.session_token(sup).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let bucket_id = bucket_of(&env);
    let item = seed_item(&env, bucket_id, "status:host", "host block");

    let spawned = call(
        &app,
        &token,
        "spawn_session",
        json!({"project":env.project_id,"agent":"test","prompt":"task","item":item}),
    )
    .await;
    let child: Value = serde_json::from_str(call_text(&spawned)).unwrap();
    let child_id = child["session_id"].as_u64().unwrap();

    let status = call(&app, &token, "session_status", json!({"session":child_id})).await;
    let status: Value = serde_json::from_str(call_text(&status)).unwrap();
    assert_eq!(status["host"]["online"], true);
    assert_eq!(status["project_host"]["status"], "ready");
    assert_eq!(
        status["project_host"]["path"],
        env.project_root().to_string_lossy().into_owned()
    );

    let listed = call(&app, &token, "list_sessions", json!({})).await;
    let listed: Value = serde_json::from_str(call_text(&listed)).unwrap();
    let row = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == child_id)
        .unwrap();
    assert_eq!(row["host"]["online"], true);
    assert_eq!(row["host"]["id"], 0);
}

fn goal_report(goal: &str, headline: &str) -> AgentReport {
    AgentReport::Report {
        goal: goal.into(),
        headline: headline.into(),
        summary: None,
        note: String::new(),
        glance: None,
        context: None,
        clear: Vec::new(),
        git: Default::default(),
    }
}

#[tokio::test]
async fn spawn_seeds_the_goal_from_the_task_title() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    assert_eq!(session_of(&env, id).goal, "test task");
}

#[tokio::test]
async fn spawn_without_a_title_seeds_the_goal_from_the_prompt() {
    let env = daemon_env();
    let id = env
        .daemon
        .spawn_session(
            env.project_id,
            AgentKind::Test,
            "",
            "Optimize the femtocell scheduler. Start with the uplink path.",
            None,
            PermissionMode::Inherit,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    assert_eq!(
        session_of(&env, id).goal,
        "Optimize the femtocell scheduler"
    );
}

#[tokio::test]
async fn report_goal_replaces_the_seed_and_a_blank_goal_keeps_it() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();

    env.daemon
        .handle_agent_report(
            &token,
            goal_report("  Optimizing femtocell software ", "reading"),
        )
        .unwrap();
    let s = session_of(&env, id);
    assert_eq!(s.goal, "Optimizing femtocell software");
    assert_eq!(s.headline, "reading");

    env.daemon
        .handle_agent_report(&token, goal_report("   ", "editing femtocell.rs 2/3"))
        .unwrap();
    let s = session_of(&env, id);
    assert_eq!(s.goal, "Optimizing femtocell software");
    assert_eq!(s.headline, "editing femtocell.rs 2/3");
}

#[tokio::test]
async fn report_goal_is_capped() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let long = "é".repeat(pm_daemon::storage::GOAL_MAX + 20);
    env.daemon
        .handle_agent_report(&token, goal_report(&long, "working"))
        .unwrap();
    assert_eq!(
        session_of(&env, id).goal.chars().count(),
        pm_daemon::storage::GOAL_MAX
    );
}

#[tokio::test]
async fn report_tool_carries_the_goal_and_session_status_returns_it() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let (_, body) = rpc(
        &app,
        Some(&token),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"report","arguments":{"goal":"Moving auth to JWTs","headline":"swapping cookies"}}}),
    )
    .await;
    assert_eq!(body["result"]["isError"], false);
    let s = session_of(&env, id);
    assert_eq!(s.goal, "Moving auth to JWTs");
    assert_eq!(s.headline, "swapping cookies");
    let json = pm_daemon::daemon::session_json(&s);
    assert_eq!(json["goal"], "Moving auth to JWTs");
    assert_eq!(json["headline"], "swapping cookies");
}

#[tokio::test]
async fn snooze_supervision_tool_validates_duration_and_requires_a_working_or_blocked_supervisor() {
    let env = daemon_env();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let plain = spawn_test_session(&env, "plain");
    let plain_token = env.daemon.session_token(plain).unwrap().unwrap();
    assert_eq!(
        call(&app, &plain_token, "snooze_supervision", json!({})).await["isError"],
        true
    );
    let supervisor = spawn_supervisor(&env);
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let result = call(&app, &token, "snooze_supervision", json!({})).await;
    assert_ne!(result["isError"], true, "{result}");
    let receipt: Value = serde_json::from_str(call_text(&result)).unwrap();
    assert_eq!(receipt["minutes"], 5);
    assert_eq!(receipt["current_completion_silent"], true);
    for minutes in [
        json!(1),
        json!(61),
        json!(-1),
        json!(2.5),
        json!("5"),
        Value::Null,
    ] {
        assert_eq!(
            call(
                &app,
                &token,
                "snooze_supervision",
                json!({"minutes": minutes})
            )
            .await["isError"],
            true
        );
    }
    for minutes in [2, 60] {
        assert_ne!(
            call(
                &app,
                &token,
                "snooze_supervision",
                json!({"minutes": minutes})
            )
            .await["isError"],
            true
        );
    }
    end_supervisor_turn(&env, supervisor);
    assert_eq!(
        call(&app, &token, "snooze_supervision", json!({})).await["isError"],
        true
    );
}

#[tokio::test]
async fn snooze_supervision_after_flag_blocked_preserves_the_question_and_alert() {
    let env = daemon_env();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    let supervisor = spawn_supervisor(&env);
    let token = env.daemon.session_token(supervisor).unwrap().unwrap();
    let question = "The workers are on hold. Kill the remaining replay or leave it?";
    let blocked = call(&app, &token, "flag_blocked", json!({"question": question})).await;
    assert_ne!(blocked["isError"], true, "{blocked}");
    let snoozed = call(&app, &token, "snooze_supervision", json!({"minutes": 60})).await;
    assert_ne!(snoozed["isError"], true, "{snoozed}");
    let session = session_of(&env, supervisor);
    assert_eq!(session.state, SessionState::NeedsInput);
    assert_eq!(session.state_detail, question);
    assert!(session.needs_input_unseen);
    end_supervisor_turn(&env, supervisor);
    let session = session_of(&env, supervisor);
    assert_eq!(session.state, SessionState::NeedsInput);
    assert_eq!(session.state_detail, question);
    assert!(session.needs_input_unseen);
    assert!(!session.idle_unseen);
}

/// A report that repeats a headline the dashboard has shown for a while is
/// answered with a request to update it, once per stale window, and a goal
/// the same way on its own longer window. A changed line is simply recorded.
#[tokio::test]
async fn a_report_repeating_a_stale_headline_or_goal_is_asked_to_update_it() {
    let env = daemon_env();
    let id = spawn_test_session(&env, "p");
    let token = env.daemon.session_token(id).unwrap().unwrap();
    let app = pm_daemon::http::router(Arc::clone(&env.daemon));
    env.daemon.set_report_freshness_thresholds(40, 120);

    let first = call(
        &app,
        &token,
        "report",
        json!({"goal": "Moving auth", "headline": "building"}),
    )
    .await;
    assert_eq!(call_text(&first), "recorded");
    let soon = call(
        &app,
        &token,
        "report",
        json!({"goal": "Moving auth", "headline": "building"}),
    )
    .await;
    assert_eq!(call_text(&soon), "recorded");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let stale = call(
        &app,
        &token,
        "report",
        json!({"goal": "Moving auth", "headline": "building"}),
    )
    .await;
    assert_eq!(stale["isError"], false, "{stale}");
    let text = call_text(&stale);
    assert!(
        text.starts_with("headline has been \"building\" for"),
        "{text}"
    );
    assert!(!text.contains("goal has been"), "{text}");
    // Asked once per window, not on every report.
    let again = call(&app, &token, "report", json!({"headline": "building"})).await;
    assert_eq!(call_text(&again), "recorded");

    tokio::time::sleep(std::time::Duration::from_millis(90)).await;
    let both = call(
        &app,
        &token,
        "report",
        json!({"goal": "Moving auth", "headline": "building"}),
    )
    .await;
    let text = call_text(&both);
    assert!(text.contains("headline has been \"building\""), "{text}");
    assert!(text.contains("goal has been \"Moving auth\""), "{text}");

    let moved = call(
        &app,
        &token,
        "report",
        json!({"goal": "Moving auth", "headline": "testing"}),
    )
    .await;
    assert_eq!(call_text(&moved), "recorded");
    assert_eq!(session_of(&env, id).headline, "testing");
}
