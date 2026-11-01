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

    #[test]
    fn the_file_outcome_is_recorded_on_the_summary() {
        let stored = large(rest(&"a".repeat(40_000))).with_file(Ok("/state/f.txt".into()));
        assert_eq!(stored["file"], "/state/f.txt");
        let failed = large(rest(&"a".repeat(40_000))).with_file(Err("host offline".into()));
        assert_eq!(failed["file_error"], "host offline");
        assert!(failed.get("file").is_none());
    }
}
