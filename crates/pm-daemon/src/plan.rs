use std::collections::BTreeMap;
use std::collections::BTreeSet;

use pm_protocol::domain::{Event, Plan, PlanDecisionMode, PlanState, LOCAL_WORKER_ID};
use serde::{Deserialize, Serialize};

use crate::daemon::{Daemon, DaemonError};
pub use crate::plan_store::PlanOptionDraft;
use crate::plan_store::{
    PlanDecision, PlanDecisionDraft, PlanDetail, PlanMessage, PlanResponseDraft, PLAN_MARKDOWN_MAX,
};
use crate::storage::now_unix_ms;

#[derive(Debug, Clone, Default)]
pub struct PlanUpdate<'a> {
    pub name: Option<&'a str>,
    pub summary: Option<&'a str>,
    pub state: Option<PlanState>,
    pub markdown_path: Option<&'a str>,
    pub linked_item_ids: Option<&'a [u64]>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanResponseInput {
    pub selected_option_keys: Vec<String>,
    pub custom_label: String,
    pub custom_detail_markdown: String,
    pub notes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDecisionResponseInput {
    pub decision_id: u64,
    pub response: PlanResponseInput,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanBatchResponseInput {
    pub responses: Vec<PlanDecisionResponseInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDecisionDraftInput {
    pub decision_id: u64,
    pub draft: PlanResponseInput,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanBatchDraftInput {
    pub drafts: Vec<PlanDecisionDraftInput>,
}

#[derive(Debug, Clone)]
pub struct PlanDecisionPresentation {
    pub key: String,
    pub title: String,
    pub prompt_markdown: String,
    pub detail_markdown: String,
    pub mode: PlanDecisionMode,
    pub allow_custom: bool,
    pub require_selection: bool,
    pub options: Vec<PlanOptionDraft>,
}

pub fn plan_json(plan: &Plan) -> serde_json::Value {
    serde_json::json!({
        "id": plan.id,
        "projectId": plan.project_id,
        "bucketId": plan.bucket_id,
        "owningSessionId": plan.owning_session_id,
        "creatingSessionId": plan.creating_session_id,
        "name": plan.name,
        "summary": plan.summary,
        "state": plan.state.as_str(),
        "markdownPath": plan.markdown_path,
        "revision": plan.revision,
        "activeDecisionId": plan.active_decision_id,
        "linkedItemIds": plan.linked_item_ids,
        "createdAtUnixMs": plan.created_at_unix_ms,
        "updatedAtUnixMs": plan.updated_at_unix_ms,
    })
}

fn active_decisions<'a>(plan: &Plan, decisions: &'a [PlanDecision]) -> Vec<&'a PlanDecision> {
    let Some(active_id) = plan.active_decision_id else {
        return Vec::new();
    };
    let Some(first) = decisions.iter().find(|decision| decision.id == active_id) else {
        return Vec::new();
    };
    let Some(batch_key) = first.batch_key.as_deref() else {
        return vec![first];
    };
    let mut active = decisions
        .iter()
        .filter(|decision| {
            decision.batch_key.as_deref() == Some(batch_key) && decision.state != "resolved"
        })
        .collect::<Vec<_>>();
    active.sort_by_key(|decision| decision.batch_position.unwrap_or_default());
    active
}

fn validate_decision(
    key: &str,
    title: &str,
    mode: PlanDecisionMode,
    allow_custom: bool,
    options: &[PlanOptionDraft],
) -> Result<(), DaemonError> {
    if key.trim().is_empty() || title.trim().is_empty() {
        return Err(DaemonError::Rejected(
            "decision key and title are required".into(),
        ));
    }
    if mode != PlanDecisionMode::Dialogue && options.is_empty() && !allow_custom {
        return Err(DaemonError::Rejected(
            "a selectable decision needs at least one option or a custom response".into(),
        ));
    }
    let mut option_keys = BTreeSet::new();
    for opt in options {
        if opt.key.trim().is_empty() || opt.label.trim().is_empty() {
            return Err(DaemonError::Rejected(
                "option keys and labels must not be empty".into(),
            ));
        }
        if !option_keys.insert(&opt.key) {
            return Err(DaemonError::Rejected(format!(
                "option key {:?} is duplicated",
                opt.key
            )));
        }
    }
    Ok(())
}

fn normalize_options(mode: PlanDecisionMode, options: &[PlanOptionDraft]) -> Vec<PlanOptionDraft> {
    if mode != PlanDecisionMode::Single {
        return options
            .iter()
            .map(|opt| {
                let mut opt = opt.clone();
                opt.recommended = false;
                opt
            })
            .collect();
    }
    let mut found_recommended = false;
    options
        .iter()
        .map(|opt| {
            let mut opt = opt.clone();
            if opt.recommended {
                if found_recommended {
                    opt.recommended = false;
                } else {
                    found_recommended = true;
                }
            }
            opt
        })
        .collect()
}

fn validate_response(
    decision: &PlanDecision,
    response: &PlanResponseInput,
) -> Result<(), DaemonError> {
    let valid_keys = decision
        .options
        .iter()
        .map(|option| option.key.as_str())
        .collect::<BTreeSet<_>>();
    if response
        .selected_option_keys
        .iter()
        .any(|key| !valid_keys.contains(key.as_str()))
    {
        return Err(DaemonError::Rejected(
            "the response contains an option that is not available".into(),
        ));
    }
    let has_custom = !response.custom_label.trim().is_empty();
    if has_custom && !decision.allow_custom {
        return Err(DaemonError::Rejected(
            "this decision does not accept a custom option".into(),
        ));
    }
    let selection_count = response.selected_option_keys.len() + usize::from(has_custom);
    match decision.mode.as_str() {
        "single" if decision.require_selection && selection_count != 1 => Err(
            DaemonError::Rejected("select exactly one option for this decision".into()),
        ),
        "single" if !decision.require_selection && selection_count > 1 => Err(
            DaemonError::Rejected("select at most one option for this decision".into()),
        ),
        "multiple" if decision.require_selection && selection_count == 0 => Err(
            DaemonError::Rejected("select at least one option for this decision".into()),
        ),
        "dialogue" => Err(DaemonError::Rejected(
            "dialogue decisions do not accept submitted selections".into(),
        )),
        _ => Ok(()),
    }
}

/// The turn text a plan message arrives as. It names the ids
/// `post_plan_message` takes, because an agent given only the plan's name
/// has to look the id up before it can answer, and a message sent against
/// a decision loses that thread without the decision id.
fn format_user_message(plan: &Plan, decision_id: Option<u64>, body: &str) -> String {
    let target = match decision_id {
        Some(decision_id) => format!("plan {}, decision {decision_id}", plan.id),
        None => format!("plan {}", plan.id),
    };
    format!(
        "User message on plan `{}` ({target}):\n\n{body}\n\nReply with \
         `post_plan_message` ({target}) so the answer reaches the plan the \
         user is reading, not only this terminal.",
        plan.name
    )
}

fn format_response(decision: &PlanDecision, response: &PlanResponseInput) -> String {
    let selected = decision
        .options
        .iter()
        .filter(|option| response.selected_option_keys.contains(&option.key))
        .map(|option| format!("- {} (`{}`)", option.label, option.key))
        .collect::<Vec<_>>();
    let mut body = format!(
        "Decision `{}`: {}\n\nSelected:\n{}",
        decision.key,
        decision.title,
        if selected.is_empty() {
            "- none".into()
        } else {
            selected.join("\n")
        }
    );
    if !response.custom_label.trim().is_empty() {
        body.push_str(&format!(
            "\n\nUser option: {}\n\n{}",
            response.custom_label, response.custom_detail_markdown
        ));
    }
    if !response.notes.is_empty() {
        body.push_str("\n\nOption notes:");
        for (key, note) in &response.notes {
            if !note.trim().is_empty() {
                body.push_str(&format!("\n- `{key}`: {note}"));
            }
        }
    }
    body
}

impl Daemon {
    pub(crate) fn owned_plan(&self, session_id: u64, plan_id: u64) -> Result<Plan, DaemonError> {
        let plan = self.storage().get_plan(plan_id)?;
        if plan.owning_session_id != session_id {
            return Err(DaemonError::Rejected(format!(
                "plan {plan_id} belongs to session {}",
                plan.owning_session_id
            )));
        }
        Ok(plan)
    }

    pub fn create_plan(
        &self,
        session_id: u64,
        name: &str,
        summary: &str,
        markdown_path: &str,
        linked_item_ids: &[u64],
    ) -> Result<Plan, DaemonError> {
        if name.trim().is_empty() {
            return Err(DaemonError::Rejected("plan name is required".into()));
        }
        let session = self.storage().get_session(session_id)?;
        let plan = self.storage().create_plan(
            session.project_id,
            session_id,
            name.trim(),
            summary,
            markdown_path,
            linked_item_ids,
            now_unix_ms(),
        )?;
        if plan.state == PlanState::Archived {
            self.publish(Event::PlanRemoved(plan.id));
        } else {
            self.publish(Event::PlanChanged(plan.clone()));
        }
        Ok(plan)
    }

    pub fn update_plan(
        &self,
        session_id: u64,
        plan_id: u64,
        update: PlanUpdate<'_>,
    ) -> Result<Plan, DaemonError> {
        self.owned_plan(session_id, plan_id)?;
        if update.name.is_some_and(|name| name.trim().is_empty()) {
            return Err(DaemonError::Rejected("plan name must not be empty".into()));
        }
        let plan = self.storage().update_plan(
            plan_id,
            update.name.map(str::trim),
            update.summary,
            update.state,
            update.markdown_path,
            update.linked_item_ids,
            now_unix_ms(),
        )?;
        if plan.state == PlanState::Archived {
            self.publish(Event::PlanRemoved(plan.id));
        } else {
            self.publish(Event::PlanChanged(plan.clone()));
        }
        Ok(plan)
    }

    pub async fn sync_plan(
        &self,
        session_id: u64,
        plan_id: u64,
        requested_path: Option<&str>,
    ) -> Result<Plan, DaemonError> {
        let plan = self.owned_plan(session_id, plan_id)?;
        let session = self.storage().get_session(session_id)?;
        let path = requested_path.unwrap_or(&plan.markdown_path);
        if path.trim().is_empty() {
            return Err(DaemonError::Rejected(
                "write the plan to a durable Markdown file and provide markdown_path".into(),
            ));
        }
        let bytes = if session.worker_id == LOCAL_WORKER_ID {
            crate::attachments::scoped_local_file(&session.cwd, path)?.0
        } else {
            let result = self
                .worker_file_read(
                    session.worker_id,
                    session.cwd.clone(),
                    path.to_string(),
                    PLAN_MARKDOWN_MAX as u64,
                )
                .await?;
            if !result.ok {
                return Err(DaemonError::Rejected(result.error));
            }
            result.content
        };
        let markdown = String::from_utf8(bytes)
            .map_err(|_| DaemonError::Rejected("plan file must be UTF-8 Markdown".into()))?;
        let plan = self
            .storage()
            .sync_plan_markdown(plan_id, path, &markdown, now_unix_ms())?;
        self.publish(Event::PlanChanged(plan.clone()));
        Ok(plan)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn present_plan_decision(
        &self,
        session_id: u64,
        plan_id: u64,
        key: &str,
        title: &str,
        prompt_markdown: &str,
        detail_markdown: &str,
        mode: PlanDecisionMode,
        allow_custom: bool,
        require_selection: bool,
        options: &[PlanOptionDraft],
    ) -> Result<(Plan, PlanDecision), DaemonError> {
        self.owned_plan(session_id, plan_id)?;
        validate_decision(key, title, mode, allow_custom, options)?;
        let normalized = normalize_options(mode, options);
        let (plan, decision) = self.storage().upsert_plan_decision(
            plan_id,
            key,
            title,
            prompt_markdown,
            detail_markdown,
            mode,
            allow_custom,
            require_selection,
            &normalized,
            now_unix_ms(),
        )?;
        self.set_session_needs_input(session_id, &format!("{} — {}", plan.name, decision.title))?;
        self.publish(Event::PlanChanged(plan.clone()));
        Ok((plan, decision))
    }

    pub fn present_plan_decision_batch(
        &self,
        session_id: u64,
        plan_id: u64,
        batch_key: &str,
        presentations: &[PlanDecisionPresentation],
    ) -> Result<(Plan, Vec<PlanDecision>), DaemonError> {
        self.owned_plan(session_id, plan_id)?;
        if batch_key.trim().is_empty() {
            return Err(DaemonError::Rejected("batch key is required".into()));
        }
        if !(2..=8).contains(&presentations.len()) {
            return Err(DaemonError::Rejected(
                "a decision batch must contain between 2 and 8 decisions".into(),
            ));
        }
        let mut decision_keys = BTreeSet::new();
        for presentation in presentations {
            validate_decision(
                &presentation.key,
                &presentation.title,
                presentation.mode,
                presentation.allow_custom,
                &presentation.options,
            )?;
            if !decision_keys.insert(presentation.key.as_str()) {
                return Err(DaemonError::Rejected(format!(
                    "decision key {:?} is duplicated",
                    presentation.key
                )));
            }
        }
        let normalized_options = presentations
            .iter()
            .map(|presentation| normalize_options(presentation.mode, &presentation.options))
            .collect::<Vec<_>>();
        let drafts = presentations
            .iter()
            .zip(&normalized_options)
            .map(|(presentation, normalized)| PlanDecisionDraft {
                key: &presentation.key,
                title: &presentation.title,
                prompt_markdown: &presentation.prompt_markdown,
                detail_markdown: &presentation.detail_markdown,
                mode: presentation.mode,
                allow_custom: presentation.allow_custom,
                require_selection: presentation.require_selection,
                options: normalized,
            })
            .collect::<Vec<_>>();
        let (plan, decisions) = self.storage().upsert_plan_decision_batch(
            plan_id,
            Some(batch_key.trim()),
            &drafts,
            now_unix_ms(),
        )?;
        self.set_session_needs_input(
            session_id,
            &format!("{} — {} decisions", plan.name, decisions.len()),
        )?;
        self.publish(Event::PlanChanged(plan.clone()));
        Ok((plan, decisions))
    }

    pub fn resolve_plan_decision(
        &self,
        session_id: u64,
        plan_id: u64,
        key: &str,
        resolution_markdown: &str,
    ) -> Result<Plan, DaemonError> {
        self.owned_plan(session_id, plan_id)?;
        let plan = self.storage().resolve_plan_decision(
            plan_id,
            key,
            resolution_markdown,
            now_unix_ms(),
        )?;
        self.publish(Event::PlanChanged(plan.clone()));
        Ok(plan)
    }

    pub fn plan_detail(&self, plan_id: u64) -> Result<PlanDetail, DaemonError> {
        let plan = self.storage().get_plan(plan_id)?;
        let decisions = self.storage().plan_decisions(plan_id)?;
        let active_decision_ids = active_decisions(&plan, &decisions)
            .into_iter()
            .map(|decision| decision.id)
            .collect::<Vec<_>>();
        let mut value = plan_json(&plan);
        value["activeDecisionIds"] = serde_json::json!(active_decision_ids);
        Ok(PlanDetail {
            plan: value,
            markdown: self.storage().plan_markdown(plan_id)?,
            decisions,
            messages: self.storage().plan_messages(plan_id)?,
        })
    }

    pub fn post_plan_agent_message(
        &self,
        session_id: u64,
        plan_id: u64,
        decision_id: Option<u64>,
        body: &str,
    ) -> Result<PlanMessage, DaemonError> {
        let plan = self.owned_plan(session_id, plan_id)?;
        let message = self.storage().add_plan_message(
            plan_id,
            decision_id.or(plan.active_decision_id),
            "session",
            Some(session_id),
            body,
            now_unix_ms(),
        )?;
        self.publish(Event::PlanChanged(self.storage().get_plan(plan_id)?));
        Ok(message)
    }

    pub async fn submit_plan_response(
        &self,
        plan_id: u64,
        decision_id: u64,
        response: PlanResponseInput,
    ) -> Result<Plan, DaemonError> {
        let (plan, session_id, body) = self.accept_plan_responses(
            plan_id,
            PlanBatchResponseInput {
                responses: vec![PlanDecisionResponseInput {
                    decision_id,
                    response,
                }],
            },
        )?;
        self.deliver_plan_responses(session_id, &body).await?;
        Ok(plan)
    }

    pub fn accept_plan_responses(
        &self,
        plan_id: u64,
        submission: PlanBatchResponseInput,
    ) -> Result<(Plan, u64, String), DaemonError> {
        let plan = self.storage().get_plan(plan_id)?;
        let decisions = self.storage().plan_decisions(plan_id)?;
        let active = active_decisions(&plan, &decisions);
        if active.is_empty() {
            return Err(DaemonError::Rejected(
                "this plan has no active decisions".into(),
            ));
        }
        let submitted_ids = submission
            .responses
            .iter()
            .map(|entry| entry.decision_id)
            .collect::<BTreeSet<_>>();
        let active_ids = active
            .iter()
            .map(|decision| decision.id)
            .collect::<BTreeSet<_>>();
        if active.iter().any(|decision| decision.state != "open") {
            return Err(DaemonError::Rejected(
                "these decisions have already been submitted".into(),
            ));
        }
        if submitted_ids != active_ids || submission.responses.len() != active.len() {
            return Err(DaemonError::Rejected(
                "submit every active decision exactly once".into(),
            ));
        }
        let mut stored = Vec::with_capacity(active.len());
        let mut sections = Vec::with_capacity(active.len());
        for decision in active {
            let response = &submission
                .responses
                .iter()
                .find(|entry| entry.decision_id == decision.id)
                .expect("active and submitted decision sets match")
                .response;
            validate_response(decision, response)?;
            stored.push(PlanResponseDraft {
                decision_id: decision.id,
                selected_option_keys: &response.selected_option_keys,
                custom_label: &response.custom_label,
                custom_detail_markdown: &response.custom_detail_markdown,
                notes: &response.notes,
            });
            sections.push(format_response(decision, response));
        }
        let body = format!(
            "User submitted {} for plan `{}`. Respond immediately: resolve these decisions and present the next decision or independent batch before doing deeper work.\n\n{}",
            if stored.len() == 1 { "a planning decision" } else { "a planning decision batch" },
            plan.name,
            sections.join("\n\n---\n\n")
        );
        self.storage()
            .submit_plan_responses(plan_id, &stored, now_unix_ms())?;
        self.storage().add_plan_message(
            plan_id,
            if stored.len() == 1 {
                Some(stored[0].decision_id)
            } else {
                None
            },
            "user",
            None,
            &body,
            now_unix_ms(),
        )?;
        let plan = self.storage().get_plan(plan_id)?;
        self.publish(Event::PlanChanged(plan.clone()));
        let owning_session_id = plan.owning_session_id;
        Ok((plan, owning_session_id, body))
    }

    pub fn save_plan_drafts(
        &self,
        plan_id: u64,
        submission: PlanBatchDraftInput,
    ) -> Result<Plan, DaemonError> {
        let plan = self.storage().get_plan(plan_id)?;
        let decisions = self.storage().plan_decisions(plan_id)?;
        if submission.drafts.is_empty() {
            return Ok(plan);
        }
        let mut updates = Vec::with_capacity(submission.drafts.len());
        for entry in &submission.drafts {
            let decision = decisions.iter().find(|d| d.id == entry.decision_id).ok_or(
                DaemonError::Storage(crate::storage::StorageError::NotFound(
                    "plan decision",
                    entry.decision_id,
                )),
            )?;
            if decision.state != "open" {
                return Err(DaemonError::Rejected(
                    "cannot save draft for non-open decision".into(),
                ));
            }
            if entry.draft.custom_label.chars().count() > 10_000 {
                return Err(DaemonError::Rejected("custom label too long".into()));
            }
            if entry.draft.custom_detail_markdown.chars().count() > 65_536 {
                return Err(DaemonError::Rejected(
                    "custom detail markdown too long".into(),
                ));
            }
            if entry
                .draft
                .notes
                .values()
                .any(|n| n.chars().count() > 65_536)
            {
                return Err(DaemonError::Rejected("note too long".into()));
            }
            updates.push(crate::plan_store::PlanDraftUpdate {
                decision_id: entry.decision_id,
                selected_option_keys: &entry.draft.selected_option_keys,
                custom_label: &entry.draft.custom_label,
                custom_detail_markdown: &entry.draft.custom_detail_markdown,
                notes: &entry.draft.notes,
            });
        }
        let updated_plan =
            self.storage()
                .save_plan_decision_drafts(plan_id, &updates, now_unix_ms())?;
        self.publish(Event::PlanChanged(updated_plan.clone()));
        Ok(updated_plan)
    }

    pub async fn deliver_plan_responses(
        &self,
        session_id: u64,
        body: &str,
    ) -> Result<(), DaemonError> {
        let outcome = self.send_plan_input(session_id, body).await?;
        if matches!(outcome.input_state, "not_delivered" | "partial") {
            return Err(DaemonError::Rejected(outcome.message));
        }
        Ok(())
    }

    pub async fn post_plan_user_message(
        &self,
        plan_id: u64,
        decision_id: Option<u64>,
        body: &str,
    ) -> Result<PlanMessage, DaemonError> {
        let plan = self.storage().get_plan(plan_id)?;
        let outcome = self
            .send_plan_input(
                plan.owning_session_id,
                &format_user_message(&plan, decision_id, body),
            )
            .await?;
        if matches!(outcome.input_state, "not_delivered" | "partial") {
            return Err(DaemonError::Rejected(outcome.message));
        }
        let message = self.storage().add_plan_message(
            plan_id,
            decision_id,
            "user",
            None,
            body,
            now_unix_ms(),
        )?;
        self.publish(Event::PlanChanged(self.storage().get_plan(plan_id)?));
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_store::PlanResponse;

    fn decision(id: u64, batch_position: Option<u32>) -> PlanDecision {
        PlanDecision {
            id,
            key: format!("decision-{id}"),
            title: format!("Decision {id}"),
            prompt_markdown: String::new(),
            detail_markdown: String::new(),
            mode: "single".into(),
            state: "open".into(),
            allow_custom: true,
            require_selection: true,
            batch_key: batch_position.map(|_| "setup".into()),
            batch_position,
            resolution_markdown: String::new(),
            options: vec![crate::plan_store::PlanOption {
                id,
                key: "default".into(),
                label: "Default".into(),
                detail_markdown: String::new(),
                recommended: false,
            }],
            response: None::<PlanResponse>,
            draft: None,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
        }
    }

    fn named_plan(id: u64, name: &str) -> Plan {
        Plan {
            id,
            project_id: 1,
            bucket_id: 1,
            owning_session_id: 1,
            creating_session_id: 1,
            name: name.into(),
            summary: String::new(),
            state: PlanState::Active,
            markdown_path: String::new(),
            revision: 0,
            active_decision_id: None,
            linked_item_ids: Vec::new(),
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
        }
    }

    /// The plan id is what `post_plan_message` takes, so a message naming
    /// only the plan cannot be answered without looking the id up first.
    #[test]
    fn a_plan_message_carries_the_plan_id_and_the_body() {
        let plan = named_plan(12, "Femtocell Layer 3 SDUs");

        let text = format_user_message(&plan, None, "does RRC fit?");

        assert!(text.contains("`Femtocell Layer 3 SDUs`"), "{text}");
        assert!(text.contains("(plan 12)"), "{text}");
        assert!(text.contains("does RRC fit?"), "{text}");
        assert!(text.contains("`post_plan_message`"), "{text}");
    }

    /// A message sent against a decision has to be answered on that
    /// decision, so its id travels beside the plan id.
    #[test]
    fn a_decision_message_carries_both_ids() {
        let plan = named_plan(12, "Plan");

        let text = format_user_message(&plan, Some(4), "pick the second one");

        assert_eq!(
            text.matches("plan 12, decision 4").count(),
            2,
            "the header and the reply hint both name the decision: {text}"
        );
    }

    #[test]
    fn a_plan_message_without_a_decision_never_names_one() {
        let plan = named_plan(7, "Plan");

        let text = format_user_message(&plan, None, "body");

        assert!(!text.contains("decision"), "{text}");
    }

    #[test]
    fn active_batch_is_returned_in_declared_order() {
        let plan = Plan {
            id: 1,
            project_id: 1,
            bucket_id: 1,
            owning_session_id: 1,
            creating_session_id: 1,
            name: "Plan".into(),
            summary: String::new(),
            state: PlanState::Active,
            markdown_path: String::new(),
            revision: 0,
            active_decision_id: Some(2),
            linked_item_ids: Vec::new(),
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
        };
        let decisions = vec![
            decision(2, Some(1)),
            decision(1, Some(0)),
            decision(3, None),
        ];

        let ids = active_decisions(&plan, &decisions)
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();

        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn custom_choice_counts_as_one_single_selection() {
        let decision = decision(1, None);
        let response = PlanResponseInput {
            selected_option_keys: Vec::new(),
            custom_label: "Another approach".into(),
            custom_detail_markdown: "Details".into(),
            notes: BTreeMap::new(),
        };

        assert!(validate_response(&decision, &response).is_ok());
    }

    #[test]
    fn optional_selection_single_accepts_empty() {
        let mut dec = decision(1, None);
        dec.require_selection = false;
        let response = PlanResponseInput {
            selected_option_keys: Vec::new(),
            custom_label: String::new(),
            custom_detail_markdown: String::new(),
            notes: BTreeMap::new(),
        };
        assert!(validate_response(&dec, &response).is_ok());
    }

    #[test]
    fn optional_selection_single_rejects_two() {
        let mut dec = decision(1, None);
        dec.require_selection = false;
        let response = PlanResponseInput {
            selected_option_keys: vec!["default".into()],
            custom_label: "Also custom".into(),
            custom_detail_markdown: String::new(),
            notes: BTreeMap::new(),
        };
        assert!(validate_response(&dec, &response).is_err());
    }

    #[test]
    fn optional_selection_multiple_accepts_empty() {
        let mut dec = decision(1, None);
        dec.mode = "multiple".into();
        dec.require_selection = false;
        let response = PlanResponseInput {
            selected_option_keys: Vec::new(),
            custom_label: String::new(),
            custom_detail_markdown: String::new(),
            notes: BTreeMap::new(),
        };
        assert!(validate_response(&dec, &response).is_ok());
    }

    #[test]
    fn required_selection_single_rejects_empty() {
        let dec = decision(1, None);
        let response = PlanResponseInput {
            selected_option_keys: Vec::new(),
            custom_label: String::new(),
            custom_detail_markdown: String::new(),
            notes: BTreeMap::new(),
        };
        assert!(validate_response(&dec, &response).is_err());
    }
}
