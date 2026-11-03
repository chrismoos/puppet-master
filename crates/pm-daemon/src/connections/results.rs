use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// Results up to this many serialized bytes are stored and returned whole.
pub const MAX_INLINE_RESULT_BYTES: usize = 32 * 1024;
/// How much of a large result's file its record keeps as a preview.
const PREVIEW_BYTES: usize = 8 * 1024;

/// Fields of a result whose single text block is the whole payload, so the
/// file holds that text rather than the JSON around it.
const BARE_TEXT_FIELDS: [&str; 4] = ["isError", "status", "content", "truncated_at_limit"];

pub(super) enum Kept {
    Inline(Value),
    Large(Large),
}

/// A result too big to return: the summary its record keeps, and the full
/// payload for a file on the host running the session.
pub(super) struct Large {
    pub summary: Value,
    pub filename: String,
    pub content: String,
}

impl Large {
    pub fn with_file(mut self, stored: Result<String, String>) -> Value {
        match stored {
            Ok(path) => self.summary["file"] = json!(path),
            Err(error) => self.summary["file_error"] = json!(error),
        }
        self.summary
    }
}

pub(super) fn keep(call_id: &str, result: Value) -> Kept {
    let serialized = result.to_string();
    if serialized.len() <= MAX_INLINE_RESULT_BYTES {
        return Kept::Inline(result);
    }
    let mut summary = Map::new();
    for field in ["isError", "status", "truncated_at_limit"] {
        if let Some(value) = result.get(field) {
            summary.insert(field.into(), value.clone());
        }
    }
    let (content, extension) = match bare_text(&result) {
        Some(text) => {
            let extension = if serde_json::from_str::<Value>(text).is_ok() {
                "json"
            } else {
                "txt"
            };
            (text.to_owned(), extension)
        }
        None => {
            if let Some(blocks) = result["content"].as_array() {
                let blocks = blocks
                    .iter()
                    .map(|block| json!({"type": block["type"], "bytes": block.to_string().len()}))
                    .collect::<Vec<_>>();
                summary.insert("blocks".into(), json!(blocks));
            }
            (serialized, "json")
        }
    };
    summary.insert("truncated".into(), json!(true));
    summary.insert("bytes".into(), json!(content.len()));
    summary.insert(
        "sha256".into(),
        json!(hex::encode(Sha256::digest(content.as_bytes()))),
    );
    summary.insert("preview".into(), json!(head(&content, PREVIEW_BYTES)));
    summary.insert(
        "note".into(),
        json!("This result is too large to return. Read the parts you need from file with your own tools rather than the whole file."),
    );
    Kept::Large(Large {
        summary: Value::Object(summary),
        filename: format!("connection-call-{call_id}.{extension}"),
        content,
    })
}

fn bare_text(result: &Value) -> Option<&str> {
    let object = result.as_object()?;
    if !object
        .keys()
        .all(|key| BARE_TEXT_FIELDS.contains(&key.as_str()))
    {
        return None;
    }
    match object.get("content")?.as_array()?.as_slice() {
        [block] if block["type"] == "text" => block["text"].as_str(),
        _ => None,
    }
}

/// How many lines of a large argument set an approval previews.
const ARGUMENTS_PREVIEW_LINES: usize = 40;
/// Most fields an outline of large arguments lists.
const MAX_OUTLINE_FIELDS: usize = 50;
/// Longest value an outline quotes before shortening it.
const MAX_OUTLINE_VALUE_CHARS: usize = 80;

/// The exact bytes a call's arguments are executed, hashed and downloaded as.
pub(super) fn arguments_bytes(arguments: &Value) -> Vec<u8> {
    arguments.to_string().into_bytes()
}

/// For arguments over the inline budget, their size and digest in place of the arguments.
pub(super) fn arguments_digest(arguments: &Value) -> Option<Value> {
    let bytes = arguments_bytes(arguments);
    (bytes.len() > MAX_INLINE_RESULT_BYTES).then(|| {
        json!({"truncated": true, "bytes": bytes.len(), "sha256": hex::encode(Sha256::digest(&bytes))})
    })
}

/// For arguments over the inline budget, what an approval shows instead of all of them:
/// the digest, an outline of their fields, and their first lines.
pub(super) fn arguments_summary(arguments: &Value) -> Option<Value> {
    let mut summary = arguments_digest(arguments)?;
    let pretty = serde_json::to_string_pretty(arguments).unwrap_or_default();
    let lines = pretty.lines().count();
    let preview = pretty
        .lines()
        .take(ARGUMENTS_PREVIEW_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    summary["lines"] = json!(lines);
    summary["preview"] = json!(head(&preview, PREVIEW_BYTES));
    summary["outline"] = json!(outline(arguments));
    Some(summary)
}

/// Top-level fields, and the fields of top-level objects, each with its value or its size.
fn outline(arguments: &Value) -> Vec<Value> {
    let mut fields = Vec::new();
    let Some(object) = arguments.as_object() else {
        return vec![json!({"field": "", "shape": shape(arguments)})];
    };
    for (key, value) in object {
        match value.as_object() {
            Some(children) if !children.is_empty() => {
                for (child, value) in children {
                    fields.push(json!({"field": format!("{key}.{child}"), "shape": shape(value)}));
                }
            }
            _ => fields.push(json!({"field": key, "shape": shape(value)})),
        }
    }
    fields.truncate(MAX_OUTLINE_FIELDS);
    fields
}

fn shape(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let kind = |item: &Value| match item {
                Value::String(_) => "string",
                Value::Number(_) => "number",
                Value::Bool(_) => "boolean",
                Value::Null => "null",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            let first = items.first().map(kind);
            let noun = match first {
                Some(first) if items.iter().all(|item| kind(item) == first) => first,
                Some(_) => "value",
                None => return "empty array".into(),
            };
            let plural = if items.len() == 1 { "" } else { "s" };
            format!("array of {} {noun}{plural}", items.len())
        }
        Value::Object(fields) => {
            let plural = if fields.len() == 1 { "" } else { "s" };
            format!("object, {} key{plural}", fields.len())
        }
        scalar => {
            let text = scalar.to_string();
            if text.chars().count() > MAX_OUTLINE_VALUE_CHARS {
                format!(
                    "{}…",
                    text.chars()
                        .take(MAX_OUTLINE_VALUE_CHARS)
                        .collect::<String>()
                )
            } else {
                text
            }
        }
    }
}

/// The longest prefix of `text` within `limit` bytes that ends on a character boundary.
pub(super) fn head(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn large(result: Value) -> Large {
        match keep("abc", result) {
            Kept::Large(large) => large,
            Kept::Inline(_) => panic!("expected a large result"),
        }
    }

    fn rest(body: &str) -> Value {
        json!({"isError":false,"status":200,"content":[{"type":"text","text":body}]})
    }

    #[test]
    fn a_result_at_the_budget_is_returned_whole_and_one_byte_more_is_not() {
        let overhead = rest("").to_string().len();
        let fits = rest(&"a".repeat(MAX_INLINE_RESULT_BYTES - overhead));
        assert_eq!(fits.to_string().len(), MAX_INLINE_RESULT_BYTES);
        assert!(matches!(keep("abc", fits), Kept::Inline(_)));
        let over = rest(&"a".repeat(MAX_INLINE_RESULT_BYTES - overhead + 1));
        assert!(matches!(keep("abc", over), Kept::Large(_)));
    }

    #[test]
    fn a_large_rest_body_goes_to_the_file_as_sent() {
        let body = json!({"items": vec!["x".repeat(100); 1000]}).to_string();
        let large =
            large(json!({"isError":true,"status":404,"content":[{"type":"text","text":body}]}));
        assert_eq!(large.content, body);
        assert_eq!(large.filename, "connection-call-abc.json");
        assert_eq!(large.summary["isError"], true);
        assert_eq!(large.summary["status"], 404);
        assert_eq!(large.summary["bytes"], body.len());
        assert_eq!(
            large.summary["sha256"],
            hex::encode(Sha256::digest(body.as_bytes()))
        );
        assert_eq!(large.summary["preview"], &body[..PREVIEW_BYTES]);
        assert!(large.summary.get("blocks").is_none());
        assert!(large.summary.to_string().len() <= MAX_INLINE_RESULT_BYTES);
    }

    #[test]
    fn a_large_text_body_that_is_not_json_gets_a_text_file() {
        let large = large(rest(&"line\n".repeat(10_000)));
        assert_eq!(large.filename, "connection-call-abc.txt");
    }

    #[test]
    fn a_large_mcp_result_keeps_its_json_and_lists_blocks_without_their_data() {
        let image = "A".repeat(50_000);
        let result = json!({"content":[{"type":"text","text":"chart"},{"type":"image","mimeType":"image/png","data":image}],"isError":false});
        let large = large(result.clone());
        assert_eq!(large.content, result.to_string());
        assert_eq!(large.filename, "connection-call-abc.json");
        assert_eq!(large.summary["blocks"][0]["type"], "text");
        assert_eq!(large.summary["blocks"][1]["type"], "image");
        assert!(large.summary["blocks"][1]["bytes"].as_u64().unwrap() > 50_000);
        assert!(!large
            .summary
            .to_string()
            .contains(&image[..PREVIEW_BYTES + 1]));
    }

    #[test]
    fn structured_content_keeps_the_whole_result_in_the_file() {
        let result = json!({"content":[{"type":"text","text":"x".repeat(40_000)}],"structuredContent":{"n":1}});
        assert_eq!(large(result.clone()).content, result.to_string());
    }

    #[test]
    fn the_preview_ends_on_a_character_boundary() {
        let body = format!("{}é{}", "a".repeat(PREVIEW_BYTES - 1), "b".repeat(40_000));
        let preview = large(rest(&body)).summary["preview"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(preview, "a".repeat(PREVIEW_BYTES - 1));
    }

    fn batch(count: usize) -> Value {
        let domains = (0..count)
            .map(|i| format!("domain-{i}.example"))
            .collect::<Vec<_>>();
        json!({"path":{"campaign":"q4"},"body":{"priority":"high","domains":domains,"notify":{"email":"ops","on":"done"}}})
    }

    #[test]
    fn small_arguments_need_no_summary() {
        assert!(arguments_digest(&batch(10)).is_none());
        assert!(arguments_summary(&batch(10)).is_none());
    }

    #[test]
    fn large_arguments_are_outlined_previewed_and_hashed_as_their_exact_bytes() {
        let arguments = batch(2000);
        let summary = arguments_summary(&arguments).unwrap();
        let bytes = arguments_bytes(&arguments);
        assert_eq!(summary["bytes"], bytes.len());
        assert_eq!(summary["sha256"], hex::encode(Sha256::digest(&bytes)));
        assert_eq!(
            summary["outline"],
            json!([
                {"field":"body.domains","shape":"array of 2000 strings"},
                {"field":"body.notify","shape":"object, 2 keys"},
                {"field":"body.priority","shape":"\"high\""},
                {"field":"path.campaign","shape":"\"q4\""},
            ])
        );
        let preview = summary["preview"].as_str().unwrap();
        assert_eq!(preview.lines().count(), ARGUMENTS_PREVIEW_LINES);
        assert!(serde_json::to_string_pretty(&arguments)
            .unwrap()
            .starts_with(preview));
        assert!(summary["lines"].as_u64().unwrap() > 2000);
        assert!(summary.to_string().len() <= MAX_INLINE_RESULT_BYTES);
    }

    #[test]
    fn an_outline_names_mixed_arrays_and_shortens_long_values() {
        assert_eq!(shape(&json!([1, "a"])), "array of 2 values");
        assert_eq!(shape(&json!([])), "empty array");
        assert_eq!(shape(&json!([{}])), "array of 1 object");
        assert!(shape(&json!("x".repeat(500))).ends_with('…'));
    }

    #[test]
    fn the_file_outcome_is_recorded_on_the_summary() {
        let stored = large(rest(&"a".repeat(40_000))).with_file(Ok("/state/f.txt".into()));
        assert_eq!(stored["file"], "/state/f.txt");
        let failed = large(rest(&"a".repeat(40_000))).with_file(Err("host offline".into()));
        assert_eq!(failed["file_error"], "host offline");
        assert!(failed.get("file").is_none());
    }
}
