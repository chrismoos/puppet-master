//! Clipboard copies a program asks for with OSC 52, picked out of the
//! output stream even when a sequence is split across chunks.

use base64::Engine;

const OSC52: &[u8] = b"\x1b]52;";
const BEL: u8 = 0x07;
const ST: &[u8] = b"\x1b\\";
/// The longest unterminated copy kept waiting for its end, so a stray
/// opener cannot grow the buffer without bound.
const MAX_PENDING: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub struct Osc52 {
    pending: Vec<u8>,
}

impl Osc52 {
    /// Feeds output bytes and returns the text of every copy they completed.
    pub fn push(&mut self, data: &[u8]) -> Vec<String> {
        let mut copies = Vec::new();
        let mut input: &[u8] = data;
        let mut joined;
        if !self.pending.is_empty() {
            joined = std::mem::take(&mut self.pending);
            joined.extend_from_slice(data);
            input = &joined;
        }
        let mut rest = input;
        loop {
            let Some(start) = find(rest, OSC52) else {
                // Keep a tail that may be the start of an opener split across chunks.
                let keep = (1..OSC52.len())
                    .rev()
                    .find(|&n| rest.ends_with(&OSC52[..n]))
                    .unwrap_or(0);
                if keep > 0 {
                    self.pending = rest[rest.len() - keep..].to_vec();
                }
                break;
            };
            let body = &rest[start + OSC52.len()..];
            let end = body
                .iter()
                .position(|&b| b == BEL)
                .map(|at| (at, 1))
                .into_iter()
                .chain(find(body, ST).map(|at| (at, ST.len())))
                .min_by_key(|&(at, _)| at);
            let Some((at, terminator)) = end else {
                if rest.len() - start <= MAX_PENDING {
                    self.pending = rest[start..].to_vec();
                }
                break;
            };
            if let Some(text) = decode(&body[..at]) {
                copies.push(text);
            }
            rest = &body[at + terminator..];
        }
        copies
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `<targets>;<base64>`. A `?` asks to read the clipboard, which is refused.
fn decode(body: &[u8]) -> Option<String> {
    let split = body.iter().position(|&b| b == b';')?;
    let payload = &body[split + 1..];
    if payload == b"?" {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc(text: &str, terminator: &str) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(text);
        format!("\x1b]52;c;{b64}{terminator}").into_bytes()
    }

    #[test]
    fn a_copy_ending_in_bel_or_st_is_read() {
        let mut scanner = Osc52::default();
        let mut stream = b"before".to_vec();
        stream.extend(osc("hello", "\x07"));
        stream.extend(b" between ");
        stream.extend(osc("world", "\x1b\\"));
        assert_eq!(
            scanner.push(&stream),
            vec!["hello".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn a_copy_split_across_chunks_is_read_once_complete() {
        let mut scanner = Osc52::default();
        let whole = osc("split copy", "\x07");
        for cut in 1..whole.len() {
            let mut scanner_at = Osc52::default();
            let mut found = scanner_at.push(&whole[..cut]);
            found.extend(scanner_at.push(&whole[cut..]));
            assert_eq!(found, vec!["split copy".to_string()], "cut at {cut}");
        }
        assert!(scanner.push(b"no copy here").is_empty());
    }

    #[test]
    fn a_clipboard_read_request_is_ignored() {
        let mut scanner = Osc52::default();
        assert!(scanner.push(b"\x1b]52;c;?\x07").is_empty());
    }
}
