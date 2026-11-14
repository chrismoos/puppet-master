//! OSC sequences the app acts on, picked out of the output stream even when
//! a sequence is split across chunks: clipboard copies (OSC 52) and the
//! foreground command a shell reports (OSC 777 `pm-command`).

use base64::Engine;

const OSC52: &[u8] = b"\x1b]52;";
const OSC_COMMAND: &[u8] = b"\x1b]777;pm-command;";
const BEL: u8 = 0x07;
const ST: &[u8] = b"\x1b\\";
/// The longest unterminated copy kept waiting for its end, so a stray
/// opener cannot grow the buffer without bound.
const MAX_PENDING: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc {
    Copy(String),
    Command(String),
}

/// Watches for every sequence the app acts on.
#[derive(Default)]
pub struct OscWatch {
    copies: Scanner,
    commands: Scanner,
}

impl OscWatch {
    pub fn push(&mut self, data: &[u8]) -> Vec<Osc> {
        let mut found: Vec<Osc> = self
            .copies
            .push(OSC52, data)
            .iter()
            .filter_map(|body| decode(body))
            .map(Osc::Copy)
            .collect();
        found.extend(
            self.commands
                .push(OSC_COMMAND, data)
                .into_iter()
                .filter_map(|body| String::from_utf8(body).ok())
                .map(Osc::Command),
        );
        found
    }
}

/// Finds one OSC opener and returns each complete body after it.
#[derive(Default)]
struct Scanner {
    pending: Vec<u8>,
}

impl Scanner {
    fn push(&mut self, opener: &[u8], data: &[u8]) -> Vec<Vec<u8>> {
        let mut bodies = Vec::new();
        let mut input: &[u8] = data;
        let mut joined;
        if !self.pending.is_empty() {
            joined = std::mem::take(&mut self.pending);
            joined.extend_from_slice(data);
            input = &joined;
        }
        let mut rest = input;
        loop {
            let Some(start) = find(rest, opener) else {
                // Keep a tail that may be the start of an opener split across chunks.
                let keep = (1..opener.len())
                    .rev()
                    .find(|&n| rest.ends_with(&opener[..n]))
                    .unwrap_or(0);
                if keep > 0 {
                    self.pending = rest[rest.len() - keep..].to_vec();
                }
                break;
            };
            let body = &rest[start + opener.len()..];
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
            bodies.push(body[..at].to_vec());
            rest = &body[at + terminator..];
        }
        bodies
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

    fn copy(text: &str, terminator: &str) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(text);
        format!("\x1b]52;c;{b64}{terminator}").into_bytes()
    }

    fn copied(text: &str) -> Osc {
        Osc::Copy(text.to_string())
    }

    #[test]
    fn a_copy_ending_in_bel_or_st_is_read() {
        let mut watch = OscWatch::default();
        let mut stream = b"before".to_vec();
        stream.extend(copy("hello", "\x07"));
        stream.extend(b" between ");
        stream.extend(copy("world", "\x1b\\"));
        assert_eq!(watch.push(&stream), vec![copied("hello"), copied("world")]);
    }

    #[test]
    fn a_copy_split_across_chunks_is_read_once_complete() {
        let whole = copy("split copy", "\x07");
        for cut in 1..whole.len() {
            let mut watch = OscWatch::default();
            let mut found = watch.push(&whole[..cut]);
            found.extend(watch.push(&whole[cut..]));
            assert_eq!(found, vec![copied("split copy")], "cut at {cut}");
        }
        assert!(OscWatch::default().push(b"no copy here").is_empty());
    }

    #[test]
    fn a_clipboard_read_request_is_ignored() {
        assert!(OscWatch::default().push(b"\x1b]52;c;?\x07").is_empty());
    }

    #[test]
    fn a_shell_reports_its_foreground_command() {
        let mut watch = OscWatch::default();
        let mut found = watch.push(b"$ \x1b]777;pm-command;vim not");
        found.extend(watch.push(b"es.md\x07"));
        assert_eq!(found, vec![Osc::Command("vim notes.md".into())]);
    }
}
