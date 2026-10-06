mod support;

use std::collections::BTreeMap;

use pm_daemon::plan::{
    PlanBatchDraftInput, PlanDecisionDraftInput, PlanOptionDraft, PlanResponseInput, PlanUpdate,
};
use pm_protocol::domain::{ItemWrite, PlanDecisionMode, PlanState, SessionState};
use support::{daemon_env, spawn_test_session};

#[tokio::test]
async fn durable_plan_decisions_drive_the_session_and_live_snapshot() {
    let env = daemon_env();
    let session_id = spawn_test_session(&env, "prepare a plan");
    std::fs::create_dir_all(env.project_root().join("docs")).unwrap();
    std::fs::write(
        env.project_root().join("docs/plan.md"),
        "# Durable plan\n\n| Choice | Result |\n| --- | --- |\n| Store | pending |\n",
    )
    .unwrap();

    let plan = env
        .daemon
        .create_plan(
            session_id,
            "Architecture plan",
            "Choose the durable boundaries",
            "docs/plan.md",
            &[],
        )
        .unwrap();
    let synced = env
        .daemon
        .sync_plan(session_id, plan.id, None)
        .await
        .unwrap();
    assert_eq!(synced.revision, 1);

    let (focused, decision) = env
        .daemon
        .present_plan_decision(
            session_id,
            plan.id,
            "store",
            "Choose the primary store",
            "Pick the operational default.",
            "The choice shapes migration and recovery.",
            PlanDecisionMode::Single,
            true,
            true,
            &[
                PlanOptionDraft::new("postgres", "Postgres", "Strong relational model."),
                PlanOptionDraft::new("sqlite", "SQLite", "Simple single-host operation."),
            ],
        )
        .unwrap();
    assert_eq!(focused.active_decision_id, Some(decision.id));
    let snapshot = env.daemon.subscribe().0;
    assert_eq!(snapshot.plans[0].active_decision_id, Some(decision.id));
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|entry| entry.id == session_id)
            .unwrap()
            .state,
        SessionState::NeedsInput
    );

    env.daemon
        .post_plan_agent_message(
            session_id,
            plan.id,
            Some(decision.id),
            "Which tradeoff matters most?",
        )
        .unwrap();
    let detail = env.daemon.plan_detail(plan.id).unwrap();
    assert!(detail.markdown.contains("| Choice | Result |"));
    assert_eq!(detail.decisions[0].options.len(), 2);
    assert_eq!(detail.messages[0].body, "Which tradeoff matters most?");

    let invalid = env
        .daemon
        .submit_plan_response(
            plan.id,
            decision.id,
            PlanResponseInput {
                selected_option_keys: vec!["missing".into()],
                custom_label: String::new(),
                custom_detail_markdown: String::new(),
                notes: BTreeMap::new(),
            },
        )
        .await
        .unwrap_err();
    assert!(invalid.to_string().contains("not available"));

    let (next_focus, next_decision) = env
        .daemon
        .present_plan_decision(
            session_id,
            plan.id,
            "rollout",
            "Choose the rollout",
            "Pick a rollout shape.",
            "",
            PlanDecisionMode::Single,
            false,
            true,
            &[PlanOptionDraft::new("gradual", "Gradual", "Lower risk.")],
        )
        .unwrap();
    assert_eq!(next_focus.active_decision_id, Some(next_decision.id));

    let resolved = env
        .daemon
        .resolve_plan_decision(session_id, plan.id, "store", "Postgres is selected.")
        .unwrap();
    assert_eq!(resolved.active_decision_id, Some(next_decision.id));
    let resolved = env
        .daemon
        .resolve_plan_decision(session_id, plan.id, "rollout", "Use a gradual rollout.")
        .unwrap();
    assert_eq!(resolved.active_decision_id, None);
    let accepted = env
        .daemon
        .update_plan(
            session_id,
            plan.id,
            PlanUpdate {
                state: Some(PlanState::Accepted),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(accepted.state, PlanState::Accepted);
    env.daemon
        .update_plan(
            session_id,
            plan.id,
            PlanUpdate {
                state: Some(PlanState::Archived),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(env.daemon.subscribe().0.plans.is_empty());
}

#[tokio::test]
async fn only_the_owning_session_can_change_a_plan() {
    let env = daemon_env();
    let owner = spawn_test_session(&env, "owner");
    let other = spawn_test_session(&env, "other");
    let plan = env.daemon.create_plan(owner, "Owned", "", "", &[]).unwrap();

    let error = env
        .daemon
        .update_plan(
            other,
            plan.id,
            PlanUpdate {
                summary: Some("takeover"),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("belongs to session"));
}

#[tokio::test]
async fn plan_item_links_use_bucket_local_item_numbers() {
    let env = daemon_env();
    let owner = spawn_test_session(&env, "owner");
    let plan = env
        .daemon
        .create_plan(owner, "Linked", "", "", &[])
        .unwrap();
    let own_item = env
        .daemon
        .upsert_item(
            plan.bucket_id,
            &ItemWrite {
                title: Some("Same bucket".into()),
                project_id: Some(env.project_id),
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .0;
    let linked = env
        .daemon
        .update_plan(
            owner,
            plan.id,
            PlanUpdate {
                linked_item_ids: Some(&[own_item.id]),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(linked.linked_item_ids, vec![own_item.id]);

    let other_bucket = env.daemon.create_bucket("other-bucket").unwrap();
    let other_project = env
        .daemon
        .create_project(
            other_bucket,
            "other-project",
            env.project_root().to_str().unwrap(),
        )
        .unwrap();
    for title in ["Other one", "Other two"] {
        env.daemon
            .upsert_item(
                other_bucket,
                &ItemWrite {
                    title: Some(title.into()),
                    project_id: Some(other_project),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
    }
    let error = env
        .daemon
        .update_plan(
            owner,
            plan.id,
            PlanUpdate {
                linked_item_ids: Some(&[2]),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("not in the plan's bucket"));
}

#[tokio::test]
async fn plan_decision_drafts_persist_and_clear_on_submission() {
    let env = daemon_env();
    let session_id = spawn_test_session(&env, "prepare plan with drafts");
    std::fs::create_dir_all(env.project_root().join("docs")).unwrap();
    std::fs::write(env.project_root().join("docs/plan.md"), "# Plan\n").unwrap();

    let plan = env
        .daemon
        .create_plan(
            session_id,
            "Draft Plan",
            "Test draft persistence",
            "docs/plan.md",
            &[],
        )
        .unwrap();

    let (_focused, decision) = env
        .daemon
        .present_plan_decision(
            session_id,
            plan.id,
            "storage_choice",
            "Storage Engine",
            "Pick storage",
            "",
            PlanDecisionMode::Single,
            true,
            true,
            &[
                PlanOptionDraft::new("postgres", "Postgres", ""),
                PlanOptionDraft::new("sqlite", "SQLite", ""),
            ],
        )
        .unwrap();

    let mut notes = BTreeMap::new();
    notes.insert("sqlite".into(), "Faster for tests".into());
    let draft = PlanResponseInput {
        selected_option_keys: vec!["sqlite".into()],
        custom_label: "Custom WAL mode".into(),
        custom_detail_markdown: "Enable WAL mode with 64MB cache.".into(),
        notes,
    };

    let updated = env
        .daemon
        .save_plan_drafts(
            plan.id,
            PlanBatchDraftInput {
                drafts: vec![PlanDecisionDraftInput {
                    decision_id: decision.id,
                    draft: draft.clone(),
                }],
            },
        )
        .unwrap();
    assert_eq!(updated.id, plan.id);

    let detail = env.daemon.plan_detail(plan.id).unwrap();
    let d = &detail.decisions[0];
    assert!(d.response.is_none());
    let stored_draft = d.draft.as_ref().expect("draft should be stored");
    assert_eq!(
        stored_draft.selected_option_keys,
        vec!["sqlite".to_string()]
    );
    assert_eq!(stored_draft.custom_label, "Custom WAL mode");
    assert_eq!(
        stored_draft.custom_detail_markdown,
        "Enable WAL mode with 64MB cache."
    );
    assert_eq!(
        stored_draft.notes.get("sqlite").map(|s| s.as_str()),
        Some("Faster for tests")
    );

    // Final response submission should clear the draft.
    let submission = PlanResponseInput {
        selected_option_keys: vec!["sqlite".into()],
        custom_label: String::new(),
        custom_detail_markdown: String::new(),
        notes: stored_draft.notes.clone(),
    };
    env.daemon
        .submit_plan_response(plan.id, decision.id, submission)
        .await
        .unwrap();

    let detail_after = env.daemon.plan_detail(plan.id).unwrap();
    let d_after = &detail_after.decisions[0];
    assert!(d_after.response.is_some());
    assert!(d_after.draft.is_none());
}

#[tokio::test]
async fn recommended_option_persists_for_single_mode_and_normalizes() {
    let env = daemon_env();
    let session_id = spawn_test_session(&env, "recommendation test");
    let plan = env
        .daemon
        .create_plan(
            session_id,
            "Recommendation Plan",
            "Test recommended flag",
            "docs/plan.md",
            &[],
        )
        .unwrap();

    let (_focused, _decision) = env
        .daemon
        .present_plan_decision(
            session_id,
            plan.id,
            "auth_strategy",
            "Authentication Strategy",
            "Pick authentication method",
            "",
            PlanDecisionMode::Single,
            true,
            true,
            &[
                PlanOptionDraft::new("basic", "Basic Auth", ""),
                PlanOptionDraft::recommended("jwt", "JWT Tokens", "Standard token auth", true),
            ],
        )
        .unwrap();

    let detail = env.daemon.plan_detail(plan.id).unwrap();
    let d = &detail.decisions[0];
    assert_eq!(d.options.len(), 2);
    assert!(!d.options[0].recommended);
    assert!(d.options[1].recommended);

    // If multiple options are marked recommended in single mode, only the first is kept.
    let (_focused, _decision2) = env
        .daemon
        .present_plan_decision(
            session_id,
            plan.id,
            "transport",
            "Transport",
            "Pick transport",
            "",
            PlanDecisionMode::Single,
            true,
            true,
            &[
                PlanOptionDraft::recommended("tcp", "TCP", "", true),
                PlanOptionDraft::recommended("udp", "UDP", "", true),
            ],
        )
        .unwrap();

    let detail = env.daemon.plan_detail(plan.id).unwrap();
    let d = detail
        .decisions
        .iter()
        .find(|d| d.key == "transport")
        .unwrap();
    assert!(d.options[0].recommended);
    assert!(!d.options[1].recommended);

    // In multiple mode, recommendations are disabled.
    let (_focused, _decision3) = env
        .daemon
        .present_plan_decision(
            session_id,
            plan.id,
            "features",
            "Features",
            "Pick features",
            "",
            PlanDecisionMode::Multiple,
            true,
            true,
            &[
                PlanOptionDraft::recommended("f1", "Feature 1", "", true),
                PlanOptionDraft::new("f2", "Feature 2", ""),
            ],
        )
        .unwrap();

    let detail = env.daemon.plan_detail(plan.id).unwrap();
    let d = detail
        .decisions
        .iter()
        .find(|d| d.key == "features")
        .unwrap();
    assert!(!d.options[0].recommended);
    assert!(!d.options[1].recommended);
}
