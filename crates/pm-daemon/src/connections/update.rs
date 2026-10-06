//! An agent's proposed update to a connection: a candidate setup and policy
//! the user reviews and applies as one change.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use std::collections::BTreeMap;

use super::{openapi, Config, Connection, Kind, OAuthConfig, Policy, PolicyDraft, Tool, ToolRule};

/// The part of a config that is not policy, as a proposal stores it.
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Setup {
    pub name: String,
    pub project_id: u64,
    pub kind: Kind,
    pub endpoint: String,
    pub schema: Option<Value>,
    pub oauth: Option<OAuthConfig>,
}

impl Setup {
    pub(super) fn of(config: &Config) -> Self {
        Self {
            name: config.name.clone(),
            project_id: config.project_id,
            kind: config.kind.clone(),
            endpoint: config.endpoint.clone(),
            schema: config.schema.clone(),
            oauth: config.oauth.clone(),
        }
    }

    pub(super) fn with_policy(self, policy: PolicyDraft) -> Config {
        Config {
            name: self.name,
            project_id: self.project_id,
            kind: self.kind,
            endpoint: self.endpoint,
            schema: self.schema,
            read_policy: policy.read_policy,
            write_policy: policy.write_policy,
            unknown_policy: policy.unknown_policy,
            rules: policy.rules,
            oauth: self.oauth,
        }
    }
}

pub(super) fn policy_of(config: &Config) -> PolicyDraft {
    PolicyDraft {
        read_policy: config.read_policy.clone(),
        write_policy: config.write_policy.clone(),
        unknown_policy: config.unknown_policy.clone(),
        rules: config.rules.clone(),
    }
}

/// What a policy file may hold. Everything is optional, because the file names only what changes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyPatch {
    read_policy: Option<Policy>,
    write_policy: Option<Policy>,
    unknown_policy: Option<Policy>,
    #[serde(default)]
    rules: BTreeMap<String, Option<ToolRule>>,
}

/// `current` with the patch's defaults and rules laid over it. A tool the patch maps to null loses its rule.
pub(super) fn patched_policy(current: &PolicyDraft, patch: &Value) -> Result<PolicyDraft> {
    let patch: PolicyPatch = serde_json::from_value(patch.clone())
        .context("The policy file may hold read_policy, write_policy, unknown_policy and rules")?;
    let mut policy = current.clone();
    if let Some(read) = patch.read_policy {
        policy.read_policy = read;
    }
    if let Some(write) = patch.write_policy {
        policy.write_policy = write;
    }
    if let Some(unknown) = patch.unknown_policy {
        policy.unknown_policy = unknown;
    }
    for (tool, rule) in patch.rules {
        match rule {
            Some(rule) => policy.rules.insert(tool, rule),
            None => policy.rules.remove(&tool),
        };
    }
    Ok(policy)
}

pub(super) struct Candidate {
    pub config: Config,
    pub tools: Vec<Tool>,
}

/// A stored credential and a passed test belong to one endpoint, kind, project and OAuth setup.
pub(super) fn requires_activation(old: &Config, new: &Config) -> bool {
    old.kind != new.kind
        || old.endpoint != new.endpoint
        || old.project_id != new.project_id
        || old.oauth != new.oauth
}

/// Classifications describe one service's tools, so they do not carry to another kind or endpoint.
pub(super) fn resets_rules(old: &Config, new: &Config) -> bool {
    old.kind != new.kind || old.endpoint != new.endpoint
}

/// REST tools derive from the document alone. MCP tools are only known from the live server.
pub(super) fn tools_for(config: &Config, discovered: &[Tool]) -> Result<Vec<Tool>> {
    if config.kind == Kind::Openapi {
        openapi::import(config.schema.as_ref().context("Missing OpenAPI document")?)
    } else {
        Ok(discovered.to_vec())
    }
}

/// The live config with the supplied fields replaced and the policy file laid
/// over it. A tool the file does not name keeps its classification only when
/// its definition is unchanged.
pub(super) fn candidate(
    current: &Connection,
    supplied: &Value,
    project: Option<u64>,
    policy: Option<&Value>,
) -> Result<Candidate> {
    let supplied = match supplied {
        Value::Null => serde_json::Map::new(),
        Value::Object(fields) => fields.clone(),
        _ => bail!("config must be an object"),
    };
    let mut merged = serde_json::to_value(&current.config)?;
    for (field, value) in supplied {
        merged[field] = value;
    }
    let mut config: Config = serde_json::from_value(merged)?;
    if let Some(project) = project {
        config.project_id = project;
    }
    config.validate()?;
    let tools = tools_for(&config, &current.tools)?;
    if resets_rules(&current.config, &config) {
        config.rules.clear();
    } else {
        config.rules.retain(|name, _| {
            let before = current.tools.iter().find(|tool| &tool.name == name);
            before.is_some() && before == tools.iter().find(|tool| &tool.name == name)
        });
    }
    if let Some(policy) = policy {
        let patched = patched_policy(&policy_of(&config), policy)?;
        config.read_policy = patched.read_policy;
        config.write_policy = patched.write_policy;
        config.unknown_policy = patched.unknown_policy;
        config.rules = patched.rules;
    }
    for name in config.rules.keys() {
        if !tools.iter().any(|tool| &tool.name == name) {
            bail!("Unknown tool in policy: {name}");
        }
    }
    Ok(Candidate { config, tools })
}

/// What the candidate changes, for the review. Fails when it changes nothing.
pub(super) fn changes(current: &Connection, candidate: &Candidate) -> Result<Value> {
    let (old, new) = (&current.config, &candidate.config);
    let mut fields = Vec::new();
    if old.name != new.name {
        fields.push(json!({"field":"name","from":old.name,"to":new.name}));
    }
    if old.kind != new.kind {
        fields.push(json!({"field":"kind","from":old.kind,"to":new.kind}));
    }
    if old.endpoint != new.endpoint {
        fields.push(json!({"field":"endpoint","from":old.endpoint,"to":new.endpoint}));
    }
    if old.project_id != new.project_id {
        fields.push(json!({"field":"project_id","from":old.project_id,"to":new.project_id}));
    }
    if old.oauth != new.oauth {
        fields.push(json!({"field":"oauth","from":old.oauth,"to":new.oauth}));
    }
    if old.schema != new.schema {
        fields.push(json!({"field":"schema"}));
    }
    let before = |name: &str| current.tools.iter().find(|tool| tool.name == name);
    let added: Vec<Value> = candidate
        .tools
        .iter()
        .filter(|tool| before(&tool.name).is_none())
        .map(|tool| json!({"name":tool.name,"description":tool.description,"suggested_access":tool.suggested_access}))
        .collect();
    let changed: Vec<&str> = candidate
        .tools
        .iter()
        .filter(|tool| before(&tool.name).is_some_and(|old| old != *tool))
        .map(|tool| tool.name.as_str())
        .collect();
    let removed: Vec<&str> = current
        .tools
        .iter()
        .filter(|tool| !candidate.tools.iter().any(|new| new.name == tool.name))
        .map(|tool| tool.name.as_str())
        .collect();
    let policy_changed =
        serde_json::to_value(policy_of(old))? != serde_json::to_value(policy_of(new))?;
    if fields.is_empty()
        && added.is_empty()
        && changed.is_empty()
        && removed.is_empty()
        && !policy_changed
    {
        bail!("The update changes nothing");
    }
    Ok(json!({
        "fields": fields,
        "tools": {"added": added, "removed": removed, "changed": changed},
        "requires_activation": requires_activation(old, new),
    }))
}
