//! The reasoning behind a spoken answer.
//!
//! Runs in the daemon rather than on each client, because the
//! credential is already sealed here, the tools execute here against
//! real state, and a second client is then a microphone and a speaker
//! rather than a second implementation of all of this.
//!
//! The answer is requested whole rather than streamed. Speech is
//! pipelined a sentence at a time instead, which is where most of the
//! perceived latency goes: synthesis of the first sentence overlaps
//! playback rather than waiting for the last one.

use serde_json::{json, Value};

/// Names the model profile the voice agent reasons with. The profile
/// carries the endpoint and the sealed key, so voice shares one
/// credential store with everything else.
pub const PROFILE_SETTING: &str = "voice.model.profile";

/// Chosen for tool-call judgment over raw speed: this agent picks
/// among every session and project the user has, and acting on the
/// wrong one costs more than a slower answer.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Enough for a short spoken answer and the tool calls that precede it.
const MAX_TOKENS: u32 = 2048;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("{0}")]
    NotConfigured(String),
    #[error("the model refused the request: {0}")]
    Refused(String),
    #[error("could not reach the model: {0}")]
    Unreachable(String),
    #[error("the model's answer could not be read: {0}")]
    Unreadable(String),
}

/// Where to reason, and with what. Resolved from a model profile, so the
/// endpoint and key are the ones an operator already configured.
#[derive(Debug, Clone)]
pub struct ModelEndpoint {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// One tool the model asked to run.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// What the model produced in one exchange: words to say, tools to run,
/// or both.
#[derive(Debug, Clone)]
pub struct Turn {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// The assistant blocks exactly as they came back, so they can be
    /// appended to the conversation without being rebuilt from the
    /// parsed form and losing anything the parse ignored.
    pub content: Value,
}

#[derive(Default)]
pub struct ModelClient {
    client: reqwest::Client,
}

impl ModelClient {
    /// One exchange. `messages` is the conversation so far in the
    /// provider's own shape, which is also what tool results are
    /// appended to.
    pub async fn turn(
        &self,
        endpoint: &ModelEndpoint,
        system: &str,
        messages: &[Value],
        tools: &Value,
    ) -> Result<Turn, ModelError> {
        // An empty base url means "use the agent CLI's own account",
        // which is meaningless here: the daemon is the one making the
        // call and has no account to fall back on.
        if endpoint.base_url.is_empty() {
            return Err(ModelError::NotConfigured(
                "the voice model's profile has no base url. Set one on its \
                 anthropic-messages endpoint, because the daemon calls the API itself \
                 rather than through an agent's account"
                    .into(),
            ));
        }
        if endpoint.api_key.is_empty() {
            return Err(ModelError::NotConfigured(
                "the voice model's profile has no api key".into(),
            ));
        }
        let url = format!("{}/v1/messages", endpoint.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": endpoint.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": messages,
        });
        // A round with nothing left to do but answer carries no tools,
        // and a provider is within its rights to refuse an empty list,
        // so the field is left out rather than sent empty.
        if !tools.as_array().is_some_and(|tools| tools.is_empty()) {
            body["tools"] = tools.clone();
        }
        let response = self
            .client
            .post(&url)
            .header("x-api-key", &endpoint.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Unreachable(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ModelError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            let detail: String = text.chars().take(400).collect();
            return Err(ModelError::Refused(format!("{status}: {detail}")));
        }
        let parsed: Value =
            serde_json::from_str(&text).map_err(|e| ModelError::Unreadable(e.to_string()))?;
        Ok(read_turn(&parsed))
    }
}

/// Pulls the words and the tool calls out of one answer. Blocks of a
/// kind this does not know are carried in `content` and otherwise
/// ignored, so a provider adding one does not break a turn.
fn read_turn(answer: &Value) -> Turn {
    let blocks = answer
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in &blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(said) = block.get("text").and_then(Value::as_str) {
                    text.push_str(said);
                }
            }
            Some("tool_use") => tool_calls.push(ToolCall {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            _ => continue,
        }
    }
    Turn {
        text: text.trim().to_string(),
        tool_calls,
        content: Value::Array(blocks),
    }
}

/// Splits an answer where a listener hears a pause, so the first
/// sentence can be rendered while the rest is still being synthesized.
///
/// Abbreviations are not special-cased: a split in the wrong place costs
/// a slightly early pause, and the alternative is a list of exceptions
/// that is wrong in a different language.
pub fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        if !matches!(c, '.' | '!' | '?' | '\n') {
            continue;
        }
        // A terminator only ends a sentence when what follows is a gap,
        // so a decimal or a version number stays whole.
        let ends = chars
            .peek()
            .map(|next| next.is_whitespace())
            .unwrap_or(true);
        if ends && !current.trim().is_empty() {
            out.push(current.trim().to_string());
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_with_words_and_a_tool_call_yields_both() {
        let turn = read_turn(&json!({
            "content": [
                {"type": "text", "text": "Looking now."},
                {"type": "tool_use", "id": "t1", "name": "list_sessions", "input": {}}
            ]
        }));
        assert_eq!(turn.text, "Looking now.");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "list_sessions");
        assert_eq!(turn.tool_calls[0].id, "t1");
    }

    /// A block kind this does not know must not lose the rest of the
    /// answer, because the conversation is replayed from `content`.
    #[test]
    fn an_unknown_block_is_carried_without_breaking_the_turn() {
        let turn = read_turn(&json!({
            "content": [
                {"type": "something_new", "payload": 1},
                {"type": "text", "text": "Done."}
            ]
        }));
        assert_eq!(turn.text, "Done.");
        assert!(turn.tool_calls.is_empty());
        assert_eq!(turn.content.as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn an_empty_answer_says_nothing_rather_than_failing() {
        let turn = read_turn(&json!({}));
        assert!(turn.text.is_empty());
        assert!(turn.tool_calls.is_empty());
    }

    #[test]
    fn sentences_split_where_a_listener_hears_a_pause() {
        assert_eq!(
            sentences("The suite passed. Nothing needs you."),
            vec!["The suite passed.", "Nothing needs you."]
        );
    }

    /// Splitting inside a number would make speech stop mid-figure.
    #[test]
    fn a_decimal_does_not_end_a_sentence() {
        assert_eq!(sentences("It took 1.5 hours."), vec!["It took 1.5 hours."]);
    }

    #[test]
    fn an_answer_without_a_terminator_is_still_one_sentence() {
        assert_eq!(sentences("still running"), vec!["still running"]);
        assert!(sentences("   ").is_empty());
    }
}
