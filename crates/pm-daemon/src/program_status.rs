//! The Program Status Protocol (OSC 7501), consumed on an agent terminal:
//! a bounded streaming scanner that removes the sequences from the output
//! stream, the report grammar with its limits, and the per-terminal record
//! store.
//!
//! A terminal that supports the protocol writes back exactly one thing, the
//! fixed feature query reply. Ids, titles and messages never flow back to
//! the program.

use std::collections::HashMap;

use base64::Engine;
use pm_protocol::domain::{ProgramStatusKind, ProgramStatusRecord, ProgramStatusState};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
/// CAN and SUB abort a control string in a VT parser.
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const OSC_INTRODUCER: u8 = b']';
const ST_FINAL: u8 = b'\\';
/// `ESC c`, a full reset (RIS), which removes every record.
const RIS_FINAL: u8 = b'c';
const PARAM_SEPARATOR: u8 = b';';

const PROGRAM_STATUS_OSC: &[u8] = b"7501";
/// OSC 133 is shell integration, and `A` marks a new prompt.
const SHELL_INTEGRATION_OSC: u32 = 133;
const PROMPT_START: u8 = b'A';

/// Whole sequence, `ESC ]` through the terminator.
pub const MAX_SEQUENCE_BYTES: usize = 4096;
const INTRODUCER_BYTES: usize = 2 + PROGRAM_STATUS_OSC.len() + 1;
const MAX_KEY_BYTES: usize = 16;
const MAX_MSG_ENCODED_BYTES: usize = 2732;
const MAX_MSG_DECODED_BYTES: usize = 2048;
const MAX_TITLE_ENCODED_BYTES: usize = 256;
const MAX_TITLE_DECODED_BYTES: usize = 192;
const MAX_APP_BYTES: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_ID_SEGMENT_BYTES: usize = 32;
const MAX_ID_LEVELS: usize = 8;
const ID_SEPARATOR: char = '/';
const MAX_PROGRESS: u32 = 100;
/// Records one terminal holds before the least recently updated is evicted.
pub const MAX_RECORDS: usize = 256;

const QUERY_BODY: &[u8] = b"?";
const STATE_CLEAR: &str = "clear";

/// The terminator a sequence ended with. The query reply uses the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    St,
    Bel,
}

/// The only bytes a supporting terminal ever writes back to the program.
pub fn query_reply(terminator: Terminator) -> &'static [u8] {
    match terminator {
        Terminator::St => b"\x1b]7501;?\x1b\\",
        Terminator::Bel => b"\x1b]7501;?\x07",
    }
}

/// Something the scanner saw in the output stream that the terminal acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEvent {
    /// A complete OSC 7501 sequence within the size limit, body only.
    Sequence {
        body: Vec<u8>,
        terminator: Terminator,
    },
    /// An OSC 7501 sequence over [`MAX_SEQUENCE_BYTES`], discarded whole.
    Oversized,
    /// `ESC c`.
    FullReset,
    /// OSC 133 A.
    PromptStart,
}

#[derive(Debug)]
enum ScanState {
    Ground,
    Escape,
    OscNumber {
        number: u32,
        digits: usize,
        held: bool,
    },
    ProgramStatus {
        body: Vec<u8>,
        oversized: bool,
    },
    ProgramStatusEscape {
        body: Vec<u8>,
        oversized: bool,
    },
    OtherOsc {
        prompt: PromptMatch,
    },
    OtherOscEscape {
        prompt: PromptMatch,
    },
}

/// How far an OSC 133 body has matched `A` followed by `;` or the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptMatch {
    NotPrompt,
    Expecting,
    SawA,
    Matched,
}

impl PromptMatch {
    fn advance(self, byte: u8) -> Self {
        match (self, byte) {
            (PromptMatch::Expecting, PROMPT_START) => PromptMatch::SawA,
            (PromptMatch::SawA, PARAM_SEPARATOR) => PromptMatch::Matched,
            (PromptMatch::Matched, _) => PromptMatch::Matched,
            _ => PromptMatch::NotPrompt,
        }
    }

    fn at_end(self) -> bool {
        matches!(self, PromptMatch::SawA | PromptMatch::Matched)
    }
}

/// Removes OSC 7501 sequences from a PTY output stream that arrives in
/// arbitrary chunks and reports what it found.
///
/// Holds back at most the `ESC ] 7501` introducer across a read boundary,
/// and buffers a sequence body only up to [`MAX_SEQUENCE_BYTES`]. Bytes
/// that cannot be the start of an OSC 7501 pass through as soon as that is
/// known, so every other sequence reaches viewers unchanged.
#[derive(Debug)]
pub struct Scanner {
    state: ScanState,
    held: Vec<u8>,
}

impl Default for Scanner {
    fn default() -> Self {
        Scanner {
            state: ScanState::Ground,
            held: Vec::with_capacity(INTRODUCER_BYTES),
        }
    }
}

impl Scanner {
    /// Appends the bytes of `input` that viewers should see to `out`.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>, events: &mut Vec<ScanEvent>) {
        for &byte in input {
            self.step(byte, out, events);
        }
    }

    /// Releases an introducer still held when the stream ends.
    pub fn end_of_stream(&mut self, out: &mut Vec<u8>) {
        self.flush_held(out);
        self.state = ScanState::Ground;
    }

    fn flush_held(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.held);
    }

    fn step(&mut self, byte: u8, out: &mut Vec<u8>, events: &mut Vec<ScanEvent>) {
        match std::mem::replace(&mut self.state, ScanState::Ground) {
            ScanState::Ground => self.ground(byte, out),
            ScanState::Escape => match byte {
                OSC_INTRODUCER => {
                    self.held.push(byte);
                    self.state = ScanState::OscNumber {
                        number: 0,
                        digits: 0,
                        held: true,
                    };
                }
                ESC => {
                    self.flush_held(out);
                    self.held.push(byte);
                    self.state = ScanState::Escape;
                }
                _ => {
                    if byte == RIS_FINAL {
                        events.push(ScanEvent::FullReset);
                    }
                    self.flush_held(out);
                    out.push(byte);
                }
            },
            ScanState::OscNumber {
                number,
                digits,
                held,
            } => self.osc_number(number, digits, held, byte, out, events),
            ScanState::ProgramStatus {
                mut body,
                oversized,
            } => match byte {
                BEL => self.finish(body, oversized, Terminator::Bel, events),
                ESC => self.state = ScanState::ProgramStatusEscape { body, oversized },
                CAN | SUB => {}
                _ => {
                    let oversized = oversized || !fits(&body, Terminator::Bel);
                    if oversized {
                        body = Vec::new();
                    } else {
                        body.push(byte);
                    }
                    self.state = ScanState::ProgramStatus { body, oversized };
                }
            },
            ScanState::ProgramStatusEscape { body, oversized } => {
                if byte == ST_FINAL {
                    self.finish(body, oversized, Terminator::St, events);
                } else {
                    // An ESC that is not a terminator aborts the sequence and
                    // starts a new one, as it does in a VT parser.
                    self.held.push(ESC);
                    self.state = ScanState::Escape;
                    self.step(byte, out, events);
                }
            }
            ScanState::OtherOsc { prompt } => {
                out.push(byte);
                match byte {
                    BEL => {
                        if prompt.at_end() {
                            events.push(ScanEvent::PromptStart);
                        }
                    }
                    ESC => self.state = ScanState::OtherOscEscape { prompt },
                    CAN | SUB => {}
                    _ => {
                        self.state = ScanState::OtherOsc {
                            prompt: prompt.advance(byte),
                        }
                    }
                }
            }
            ScanState::OtherOscEscape { prompt } => {
                if byte == ST_FINAL {
                    out.push(byte);
                    if prompt.at_end() {
                        events.push(ScanEvent::PromptStart);
                    }
                } else {
                    // The ESC already went out with the OSC it aborted.
                    self.state = ScanState::Escape;
                    self.step(byte, out, events);
                }
            }
        }
    }

    fn ground(&mut self, byte: u8, out: &mut Vec<u8>) {
        if byte == ESC {
            self.held.push(byte);
            self.state = ScanState::Escape;
        } else {
            out.push(byte);
        }
    }

    fn osc_number(
        &mut self,
        number: u32,
        digits: usize,
        held: bool,
        byte: u8,
        out: &mut Vec<u8>,
        events: &mut Vec<ScanEvent>,
    ) {
        if byte.is_ascii_digit() {
            let still_ours =
                held && digits < PROGRAM_STATUS_OSC.len() && PROGRAM_STATUS_OSC[digits] == byte;
            if still_ours {
                self.held.push(byte);
            } else {
                self.flush_held(out);
                out.push(byte);
            }
            self.state = ScanState::OscNumber {
                number: number
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0')),
                digits: digits + 1,
                held: still_ours,
            };
            return;
        }
        if byte == PARAM_SEPARATOR && held && digits == PROGRAM_STATUS_OSC.len() {
            self.held.clear();
            self.state = ScanState::ProgramStatus {
                body: Vec::new(),
                oversized: false,
            };
            return;
        }
        self.flush_held(out);
        let prompt = if byte == PARAM_SEPARATOR && number == SHELL_INTEGRATION_OSC && digits > 0 {
            PromptMatch::Expecting
        } else {
            PromptMatch::NotPrompt
        };
        if byte == PARAM_SEPARATOR {
            out.push(byte);
            self.state = ScanState::OtherOsc { prompt };
        } else {
            self.state = ScanState::OtherOsc { prompt };
            self.step(byte, out, events);
        }
    }

    fn finish(
        &mut self,
        body: Vec<u8>,
        oversized: bool,
        terminator: Terminator,
        events: &mut Vec<ScanEvent>,
    ) {
        if oversized || !fits_terminated(&body, terminator) {
            events.push(ScanEvent::Oversized);
        } else {
            events.push(ScanEvent::Sequence { body, terminator });
        }
    }
}

/// Whether one more body byte could still end within the limit.
fn fits(body: &[u8], shortest: Terminator) -> bool {
    INTRODUCER_BYTES + body.len() + 1 + terminator_len(shortest) <= MAX_SEQUENCE_BYTES
}

fn fits_terminated(body: &[u8], terminator: Terminator) -> bool {
    INTRODUCER_BYTES + body.len() + terminator_len(terminator) <= MAX_SEQUENCE_BYTES
}

fn terminator_len(terminator: Terminator) -> usize {
    match terminator {
        Terminator::St => 2,
        Terminator::Bel => 1,
    }
}

/// A report the store can apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Set {
        id: String,
        state: ProgramStatusState,
        kind: Option<ProgramStatusKind>,
        progress: Option<u32>,
        app: String,
        title: String,
        msg: String,
    },
    /// Removes `id` and everything beneath it, or every record for the root.
    Clear { id: String },
}

/// What one OSC 7501 body asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Query,
    Update(Update),
}

/// Why a report was not applied. Nothing from it reaches the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// No `state`, or one this terminal does not know.
    State,
    /// An `id` that does not match the grammar or its limits.
    Id,
    /// A key over 16 bytes.
    KeyTooLong,
    /// `app` over its byte limit.
    AppTooLong,
    /// A `msg` or `title` that is too long, is not base64 of UTF-8, or
    /// decodes to text with a control character.
    Text,
}

impl Rejected {
    pub fn as_str(self) -> &'static str {
        match self {
            Rejected::State => "missing or unknown state",
            Rejected::Id => "invalid id",
            Rejected::KeyTooLong => "key over the length limit",
            Rejected::AppTooLong => "app over the length limit",
            Rejected::Text => "invalid msg or title",
        }
    }
}

fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.,+/=-".contains(&byte)
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte)
}

fn trim(bytes: &[u8]) -> &[u8] {
    bytes.trim_ascii()
}

/// Parses the body of one OSC 7501 sequence, checking every pair against
/// the limits before anything is returned to apply.
pub fn parse_body(body: &[u8]) -> Result<Report, Rejected> {
    if trim(body) == QUERY_BODY {
        return Ok(Report::Query);
    }
    let mut state = None;
    let mut id = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    let mut title = None;
    let mut msg = None;
    for pair in body.split(|&b| b == b':') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let key = trim(&pair[..eq]);
        let value = trim(&pair[eq + 1..]);
        if key.len() > MAX_KEY_BYTES {
            return Err(Rejected::KeyTooLong);
        }
        if key.is_empty() || !key.iter().all(u8::is_ascii_lowercase) {
            continue;
        }
        if !value.iter().all(|&b| is_value_byte(b)) {
            continue;
        }
        // The value set is ASCII, so this cannot fail.
        let value = std::str::from_utf8(value).unwrap_or_default();
        match key {
            b"state" => state = Some(value),
            b"id" => id = Some(value),
            b"kind" => kind = Some(value),
            b"progress" => progress = Some(value),
            b"app" => {
                if value.len() > MAX_APP_BYTES {
                    return Err(Rejected::AppTooLong);
                }
                app = Some(value);
            }
            b"title" => {
                title = Some(decode_text(
                    value,
                    MAX_TITLE_ENCODED_BYTES,
                    MAX_TITLE_DECODED_BYTES,
                )?)
            }
            b"msg" => {
                msg = Some(decode_text(
                    value,
                    MAX_MSG_ENCODED_BYTES,
                    MAX_MSG_DECODED_BYTES,
                )?)
            }
            _ => {}
        }
    }
    let state = state.ok_or(Rejected::State)?;
    let id = match id {
        Some(id) => valid_id(id).ok_or(Rejected::Id)?.to_string(),
        None => String::new(),
    };
    if state == STATE_CLEAR {
        return Ok(Report::Update(Update::Clear { id }));
    }
    let state = ProgramStatusState::parse(state).ok_or(Rejected::State)?;
    let kind = kind
        .filter(|_| state == ProgramStatusState::Blocked)
        .and_then(ProgramStatusKind::parse);
    let progress = progress
        .filter(|_| {
            matches!(
                state,
                ProgramStatusState::Working | ProgramStatusState::Blocked
            )
        })
        .and_then(parse_progress);
    let app = app
        .filter(|app| !app.is_empty() && app.bytes().all(is_name_byte))
        .unwrap_or_default()
        .to_string();
    Ok(Report::Update(Update::Set {
        id,
        state,
        kind,
        progress,
        app,
        title: title.unwrap_or_default(),
        msg: msg.unwrap_or_default(),
    }))
}

fn valid_id(id: &str) -> Option<&str> {
    if id.len() > MAX_ID_BYTES {
        return None;
    }
    let mut levels = 0;
    for segment in id.split(ID_SEPARATOR) {
        levels += 1;
        if segment.is_empty()
            || segment.len() > MAX_ID_SEGMENT_BYTES
            || !segment.bytes().all(is_name_byte)
        {
            return None;
        }
    }
    (levels <= MAX_ID_LEVELS).then_some(id)
}

fn parse_progress(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|p| *p <= MAX_PROGRESS)
}

fn decode_text(value: &str, max_encoded: usize, max_decoded: usize) -> Result<String, Rejected> {
    if value.len() > max_encoded {
        return Err(Rejected::Text);
    }
    let decoded = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(value.trim_end_matches('='))
        .map_err(|_| Rejected::Text)?;
    if decoded.len() > max_decoded {
        return Err(Rejected::Text);
    }
    let text = String::from_utf8(decoded).map_err(|_| Rejected::Text)?;
    if text.chars().any(is_control) {
        return Err(Rejected::Text);
    }
    Ok(text
        .chars()
        .filter(|c| !is_invisible_formatting(*c))
        .collect())
}

/// C0, DEL and C1.
fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}

/// Bidirectional overrides, isolates and marks, zero-width and other
/// invisible formatting characters, which let text shown outside the grid
/// read differently from what it contains.
fn is_invisible_formatting(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{34f}'
            | '\u{61c}'
            | '\u{115f}'
            | '\u{1160}'
            | '\u{17b4}'
            | '\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fffb}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

/// The records a change adds or replaces and the ids it removes, applied
/// after dropping everything when `reset` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub reset: bool,
    pub records: Vec<ProgramStatusRecord>,
    pub removed: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.reset && self.records.is_empty() && self.removed.is_empty()
    }
}

/// One terminal's records, keyed by id with the root under the empty id.
#[derive(Debug, Default)]
pub struct RecordStore {
    records: HashMap<String, (u64, ProgramStatusRecord)>,
    sequence: u64,
}

fn is_beneath(id: &str, ancestor: &str) -> bool {
    id.len() > ancestor.len()
        && id.starts_with(ancestor)
        && id.as_bytes()[ancestor.len()] == ID_SEPARATOR as u8
}

impl RecordStore {
    pub fn apply(&mut self, update: Update, now_unix_ms: i64) -> Changes {
        match update {
            Update::Clear { id } if id.is_empty() => self.reset(),
            Update::Clear { id } => {
                let mut removed: Vec<String> = self
                    .records
                    .keys()
                    .filter(|key| **key == id || is_beneath(key, &id))
                    .cloned()
                    .collect();
                removed.sort();
                for key in &removed {
                    self.records.remove(key);
                }
                Changes {
                    removed,
                    ..Changes::default()
                }
            }
            Update::Set {
                id,
                state,
                kind,
                progress,
                app,
                title,
                msg,
            } => {
                let mut changes = Changes::default();
                if !self.records.contains_key(&id) && self.records.len() >= MAX_RECORDS {
                    if let Some(oldest) = self
                        .records
                        .iter()
                        .min_by_key(|(_, (sequence, _))| *sequence)
                        .map(|(key, _)| key.clone())
                    {
                        self.records.remove(&oldest);
                        changes.removed.push(oldest);
                    }
                }
                self.sequence += 1;
                let record = ProgramStatusRecord {
                    id: id.clone(),
                    state,
                    kind,
                    progress,
                    app,
                    title,
                    msg,
                    updated_at_unix_ms: now_unix_ms,
                };
                self.records.insert(id, (self.sequence, record.clone()));
                changes.records.push(record);
                changes
            }
        }
    }

    /// Removes every record, for `ESC c` and for a root `clear`.
    pub fn reset(&mut self) -> Changes {
        let had_records = !self.records.is_empty();
        self.records.clear();
        Changes {
            reset: had_records,
            ..Changes::default()
        }
    }

    /// Drops working and blocked records, which end with the process or at
    /// the next shell prompt. Idle, done and error records stay.
    pub fn drop_transient(&mut self) -> Changes {
        let mut removed: Vec<String> = self
            .records
            .iter()
            .filter(|(_, (_, record))| {
                matches!(
                    record.state,
                    ProgramStatusState::Working | ProgramStatusState::Blocked
                )
            })
            .map(|(key, _)| key.clone())
            .collect();
        removed.sort();
        for key in &removed {
            self.records.remove(key);
        }
        Changes {
            removed,
            ..Changes::default()
        }
    }

    /// Mirrors a change produced by another store, such as a worker's.
    pub fn merge(&mut self, changes: &Changes) {
        if changes.reset {
            self.records.clear();
        }
        for id in &changes.removed {
            self.records.remove(id);
        }
        for record in &changes.records {
            self.sequence += 1;
            self.records
                .insert(record.id.clone(), (self.sequence, record.clone()));
        }
    }

    pub fn root(&self) -> Option<&ProgramStatusRecord> {
        self.records.get("").map(|(_, record)| record)
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Every record as stored, sorted by id so the root comes first.
    pub fn snapshot(&self) -> Vec<ProgramStatusRecord> {
        let mut records: Vec<_> = self
            .records
            .values()
            .map(|(_, record)| record.clone())
            .collect();
        records.sort_by(|a, b| a.id.cmp(&b.id));
        records
    }

    /// [`Self::snapshot`] with each record's `app` taken from its nearest
    /// ancestor that has one when it has none of its own.
    pub fn resolved(&self) -> Vec<ProgramStatusRecord> {
        let mut records = self.snapshot();
        for record in &mut records {
            if record.app.is_empty() {
                record.app = self.inherited_app(&record.id).unwrap_or_default();
            }
        }
        records
    }

    fn inherited_app(&self, id: &str) -> Option<String> {
        let mut current = id;
        while !current.is_empty() {
            current = current
                .rfind(ID_SEPARATOR)
                .map(|at| &current[..at])
                .unwrap_or("");
            if let Some((_, parent)) = self.records.get(current) {
                if !parent.app.is_empty() {
                    return Some(parent.app.clone());
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn scan(chunks: &[&[u8]]) -> (Vec<u8>, Vec<ScanEvent>) {
        let mut scanner = Scanner::default();
        let mut out = Vec::new();
        let mut events = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, &mut out, &mut events);
        }
        (out, events)
    }

    fn set(body: &str) -> Update {
        match parse_body(body.as_bytes()) {
            Ok(Report::Update(update)) => update,
            other => panic!("{body:?} parsed to {other:?}"),
        }
    }

    #[derive(Debug)]
    struct Fields {
        id: String,
        state: ProgramStatusState,
        kind: Option<ProgramStatusKind>,
        progress: Option<u32>,
        app: String,
        title: String,
        msg: String,
    }

    fn fields(body: &str) -> Fields {
        match set(body) {
            Update::Set {
                id,
                state,
                kind,
                progress,
                app,
                title,
                msg,
            } => Fields {
                id,
                state,
                kind,
                progress,
                app,
                title,
                msg,
            },
            other => panic!("{body:?} parsed to {other:?}"),
        }
    }

    #[test]
    fn a_query_is_recognized_with_either_terminator() {
        let (out, events) = scan(&[b"a\x1b]7501;?\x1b\\b\x1b]7501;?\x07c"]);
        assert_eq!(out, b"abc");
        assert_eq!(
            events,
            vec![
                ScanEvent::Sequence {
                    body: b"?".to_vec(),
                    terminator: Terminator::St
                },
                ScanEvent::Sequence {
                    body: b"?".to_vec(),
                    terminator: Terminator::Bel
                },
            ]
        );
        assert_eq!(parse_body(b"?"), Ok(Report::Query));
        assert_eq!(parse_body(b" ? "), Ok(Report::Query));
        assert_eq!(query_reply(Terminator::St), b"\x1b]7501;?\x1b\\");
        assert_eq!(query_reply(Terminator::Bel), b"\x1b]7501;?\x07");
    }

    #[test]
    fn a_sequence_split_at_every_byte_is_still_removed_and_reported() {
        let stream = b"head\x1b]7501;state=working:app=claude-code\x1b\\tail";
        let chunks: Vec<&[u8]> = stream.chunks(1).collect();
        let (out, events) = scan(&chunks);
        assert_eq!(out, b"headtail");
        assert_eq!(
            events,
            vec![ScanEvent::Sequence {
                body: b"state=working:app=claude-code".to_vec(),
                terminator: Terminator::St
            }]
        );
    }

    #[test]
    fn other_sequences_pass_through_unchanged_across_splits() {
        let stream: &[u8] =
            b"\x1b]0;title\x07\x1b]750;x\x07\x1b]75012;y\x1b\\\x1b[31mred\x1b]8;;u\x1b\\\x1b]7501\x07z";
        for split in 0..stream.len() {
            let (out, events) = scan(&[&stream[..split], &stream[split..]]);
            assert_eq!(out, stream, "split at {split}");
            assert!(events.is_empty(), "split at {split}: {events:?}");
        }
    }

    #[test]
    fn only_an_unresolved_introducer_is_held_across_a_read() {
        let mut scanner = Scanner::default();
        let mut out = Vec::new();
        let mut events = Vec::new();
        scanner.feed(b"ok\x1b]75", &mut out, &mut events);
        assert_eq!(out, b"ok");
        scanner.feed(b"9;x\x07", &mut out, &mut events);
        assert_eq!(out, b"ok\x1b]759;x\x07");
        assert!(events.is_empty());
    }

    #[test]
    fn a_held_introducer_is_released_when_the_stream_ends() {
        let mut scanner = Scanner::default();
        let mut out = Vec::new();
        let mut events = Vec::new();
        scanner.feed(b"tail\x1b]750", &mut out, &mut events);
        scanner.end_of_stream(&mut out);
        assert_eq!(out, b"tail\x1b]750");
    }

    #[test]
    fn an_escape_inside_the_body_aborts_it_and_starts_the_next_sequence() {
        let (out, events) = scan(&[b"\x1b]7501;state=working\x1b[1mX"]);
        assert_eq!(out, b"\x1b[1mX");
        assert!(events.is_empty());
        let (out, events) = scan(&[b"\x1b]7501;state=working\x1b]7501;?\x07"]);
        assert!(out.is_empty());
        assert_eq!(
            events,
            vec![ScanEvent::Sequence {
                body: b"?".to_vec(),
                terminator: Terminator::Bel
            }]
        );
    }

    #[test]
    fn a_sequence_at_the_size_limit_is_kept_and_one_byte_over_is_discarded() {
        let at_limit = MAX_SEQUENCE_BYTES - INTRODUCER_BYTES - 2;
        let mut stream = b"\x1b]7501;".to_vec();
        stream.extend(std::iter::repeat_n(b'a', at_limit));
        stream.extend(b"\x1b\\");
        assert_eq!(stream.len(), MAX_SEQUENCE_BYTES);
        let (out, events) = scan(&[&stream]);
        assert!(out.is_empty());
        assert!(
            matches!(&events[..], [ScanEvent::Sequence { body, .. }] if body.len() == at_limit)
        );

        let mut over = b"\x1b]7501;".to_vec();
        over.extend(std::iter::repeat_n(b'a', at_limit + 1));
        over.extend(b"\x1b\\after");
        let (out, events) = scan(&[&over]);
        assert_eq!(out, b"after");
        assert_eq!(events, vec![ScanEvent::Oversized]);
    }

    #[test]
    fn an_endless_sequence_never_buffers_past_the_limit() {
        let mut scanner = Scanner::default();
        let mut out = Vec::new();
        let mut events = Vec::new();
        scanner.feed(b"\x1b]7501;", &mut out, &mut events);
        let chunk = vec![b'a'; 64 * 1024];
        for _ in 0..16 {
            scanner.feed(&chunk, &mut out, &mut events);
            let buffered = match &scanner.state {
                ScanState::ProgramStatus { body, .. } => body.capacity(),
                other => panic!("left the sequence: {other:?}"),
            };
            assert!(buffered <= MAX_SEQUENCE_BYTES);
        }
        scanner.feed(b"\x07", &mut out, &mut events);
        assert!(out.is_empty());
        assert_eq!(events, vec![ScanEvent::Oversized]);
    }

    #[test]
    fn a_full_reset_and_a_prompt_start_are_reported_and_passed_through() {
        let stream: &[u8] =
            b"\x1bc\x1b]133;A\x07\x1b]133;A;cl=m\x1b\\\x1b]133;C\x07\x1b]133;AB\x07";
        for split in 0..stream.len() {
            let (out, events) = scan(&[&stream[..split], &stream[split..]]);
            assert_eq!(out, stream);
            assert_eq!(
                events,
                vec![
                    ScanEvent::FullReset,
                    ScanEvent::PromptStart,
                    ScanEvent::PromptStart
                ],
                "split at {split}"
            );
        }
    }

    #[test]
    fn grammar_trims_whitespace_skips_malformed_pairs_and_keeps_the_last_duplicate() {
        let body = format!(
            " state = blocked : kind=question:noequals:=x:Bad=1:msg=a!b:app=one:app=two:extra=ignored:msg={}",
            b64("Which branch?")
        );
        let parsed = fields(&body);
        assert_eq!(parsed.id, "");
        assert_eq!(parsed.state, ProgramStatusState::Blocked);
        assert_eq!(parsed.kind, Some(ProgramStatusKind::Question));
        assert_eq!(parsed.progress, None);
        assert_eq!(parsed.app, "two");
        assert_eq!(parsed.title, "");
        assert_eq!(parsed.msg, "Which branch?");
    }

    #[test]
    fn a_missing_or_unknown_state_is_ignored() {
        assert_eq!(parse_body(b"app=x"), Err(Rejected::State));
        assert_eq!(parse_body(b"state=paused"), Err(Rejected::State));
        assert_eq!(parse_body(b"state=w\xc3\xb6rking"), Err(Rejected::State));
        assert_eq!(parse_body(b""), Err(Rejected::State));
    }

    #[test]
    fn ids_follow_the_grammar_and_never_fall_back_to_the_root() {
        assert_eq!(
            fields("state=idle:id=build/test.unit_1+x-y").id,
            "build/test.unit_1+x-y"
        );
        for bad in ["id=", "id=a//b", "id=/a", "id=a/", "id=a,b", "id=a=b"] {
            assert_eq!(
                parse_body(format!("state=idle:{bad}").as_bytes()),
                Err(Rejected::Id),
                "{bad}"
            );
        }
        let segment = "s".repeat(MAX_ID_SEGMENT_BYTES);
        assert!(parse_body(format!("state=idle:id={segment}").as_bytes()).is_ok());
        assert_eq!(
            parse_body(format!("state=idle:id={segment}s").as_bytes()),
            Err(Rejected::Id)
        );
        let deep = ["a"; MAX_ID_LEVELS].join("/");
        assert!(parse_body(format!("state=idle:id={deep}").as_bytes()).is_ok());
        assert_eq!(
            parse_body(format!("state=idle:id={deep}/a").as_bytes()),
            Err(Rejected::Id)
        );
        let long = vec!["a".repeat(31); 5].join("/");
        assert!(long.len() > MAX_ID_BYTES);
        assert_eq!(
            parse_body(format!("state=idle:id={long}").as_bytes()),
            Err(Rejected::Id)
        );
    }

    #[test]
    fn kind_and_progress_apply_only_to_the_states_that_carry_them() {
        let parsed = fields("state=working:kind=auth:progress=40");
        assert_eq!((parsed.kind, parsed.progress), (None, Some(40)));
        let parsed = fields("state=blocked:kind=auth:progress=100");
        assert_eq!(
            (parsed.kind, parsed.progress),
            (Some(ProgramStatusKind::Auth), Some(100))
        );
        let parsed = fields("state=done:kind=auth:progress=40");
        assert_eq!((parsed.kind, parsed.progress), (None, None));
        assert_eq!(fields("state=blocked:kind=coffee").kind, None);
        for bad in ["101", "-1", "+5", "4.5", "", "1000"] {
            let parsed = fields(&format!("state=working:progress={bad}"));
            assert_eq!(parsed.progress, None, "{bad}");
        }
    }

    #[test]
    fn an_app_outside_its_character_set_is_absent_and_one_too_long_discards_the_report() {
        assert_eq!(fields("state=idle:app=a,b").app, "");
        assert_eq!(fields("state=idle:app=a/b").app, "");
        let app = "a".repeat(MAX_APP_BYTES);
        assert_eq!(fields(&format!("state=idle:app={app}")).app, app);
        assert_eq!(
            parse_body(format!("state=idle:app={app}a").as_bytes()),
            Err(Rejected::AppTooLong)
        );
    }

    #[test]
    fn a_key_over_the_limit_discards_the_whole_report() {
        let key = "k".repeat(MAX_KEY_BYTES);
        assert!(parse_body(format!("state=idle:{key}=1").as_bytes()).is_ok());
        assert_eq!(
            parse_body(format!("state=idle:{key}k=1").as_bytes()),
            Err(Rejected::KeyTooLong)
        );
    }

    #[test]
    fn text_limits_are_checked_encoded_first_then_decoded() {
        let at_limit = "a".repeat(MAX_MSG_DECODED_BYTES);
        assert_eq!(
            fields(&format!("state=idle:msg={}", b64(&at_limit))).msg,
            at_limit
        );
        let over = "a".repeat(MAX_MSG_DECODED_BYTES + 1);
        assert_eq!(
            parse_body(format!("state=idle:msg={}", b64(&over)).as_bytes()),
            Err(Rejected::Text)
        );
        let padded = format!("{}{}", b64("hi"), "=".repeat(MAX_MSG_ENCODED_BYTES));
        assert_eq!(
            parse_body(format!("state=idle:msg={padded}").as_bytes()),
            Err(Rejected::Text),
            "an encoded value over the limit is refused before decoding"
        );
        let title = "t".repeat(MAX_TITLE_DECODED_BYTES);
        assert_eq!(
            fields(&format!("state=idle:title={}", b64(&title))).title,
            title
        );
        assert_eq!(
            parse_body(format!("state=idle:title={}", b64(&format!("{title}t"))).as_bytes()),
            Err(Rejected::Text)
        );
        let long_encoding = format!("{}{}", b64("t"), "=".repeat(MAX_TITLE_ENCODED_BYTES));
        assert_eq!(
            parse_body(format!("state=idle:title={long_encoding}").as_bytes()),
            Err(Rejected::Text)
        );
    }

    #[test]
    fn base64_padding_is_optional_and_bad_base64_discards_the_report() {
        assert_eq!(fields("state=idle:msg=aGk").msg, "hi");
        assert_eq!(fields("state=idle:msg=aGk=").msg, "hi");
        assert_eq!(parse_body(b"state=idle:msg=a"), Err(Rejected::Text));
        assert_eq!(parse_body(b"state=idle:msg=a-b_"), Err(Rejected::Text));
        let not_utf8 = base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe]);
        assert_eq!(
            parse_body(format!("state=idle:msg={not_utf8}").as_bytes()),
            Err(Rejected::Text)
        );
    }

    #[test]
    fn control_characters_discard_the_report_and_invisible_formatting_is_removed() {
        for control in [
            "a\u{1b}b",
            "a\u{7}",
            "\u{7f}",
            "x\u{85}y",
            "\u{9b}31m",
            "a\nb",
        ] {
            assert_eq!(
                parse_body(format!("state=idle:msg={}", b64(control)).as_bytes()),
                Err(Rejected::Text),
                "{control:?}"
            );
            assert_eq!(
                parse_body(format!("state=idle:title={}", b64(control)).as_bytes()),
                Err(Rejected::Text),
                "{control:?}"
            );
        }
        let disguised = "invoice\u{202e}fdp.exe\u{200b}\u{2066}x\u{2069}\u{feff}";
        assert_eq!(
            fields(&format!("state=idle:msg={}", b64(disguised))).msg,
            "invoicefdp.exex"
        );
    }

    #[test]
    fn a_bad_pair_anywhere_discards_the_report_even_when_a_later_duplicate_is_valid() {
        let over = "a".repeat(MAX_MSG_DECODED_BYTES + 1);
        let body = format!("state=working:msg={}:msg={}", b64(&over), b64("ok"));
        assert_eq!(parse_body(body.as_bytes()), Err(Rejected::Text));
    }

    #[test]
    fn a_clear_needs_no_other_keys_and_still_validates_its_id() {
        assert_eq!(set("state=clear"), Update::Clear { id: String::new() });
        assert_eq!(
            set("state=clear:id=a/b"),
            Update::Clear { id: "a/b".into() }
        );
        assert_eq!(parse_body(b"state=clear:id=a//b"), Err(Rejected::Id));
    }

    fn apply(store: &mut RecordStore, body: &str) -> Changes {
        store.apply(set(body), 0)
    }

    fn ids(store: &RecordStore) -> Vec<String> {
        store.snapshot().into_iter().map(|r| r.id).collect()
    }

    #[test]
    fn each_report_replaces_its_record_completely() {
        let mut store = RecordStore::default();
        apply(
            &mut store,
            &format!("state=working:app=x:progress=10:msg={}", b64("one")),
        );
        apply(&mut store, "state=working");
        let root = store.root().unwrap();
        assert_eq!(root.app, "");
        assert_eq!(root.progress, None);
        assert_eq!(root.msg, "");
    }

    #[test]
    fn clearing_an_id_removes_it_and_its_descendants_only() {
        let mut store = RecordStore::default();
        for id in ["build", "build/test", "build/test/unit", "builder", "other"] {
            apply(&mut store, &format!("state=working:id={id}"));
        }
        apply(&mut store, "state=idle");
        let changes = apply(&mut store, "state=clear:id=build");
        assert_eq!(
            changes.removed,
            vec!["build", "build/test", "build/test/unit"]
        );
        assert_eq!(ids(&store), vec!["", "builder", "other"]);
        let changes = apply(&mut store, "state=clear:id=build/test");
        assert!(changes.removed.is_empty());
        let changes = apply(&mut store, "state=clear");
        assert!(changes.reset);
        assert!(store.is_empty());
        assert!(apply(&mut store, "state=clear").is_empty());
    }

    #[test]
    fn the_least_recently_updated_record_is_evicted_at_the_cap() {
        let mut store = RecordStore::default();
        for index in 0..MAX_RECORDS {
            apply(&mut store, &format!("state=working:id=r{index}"));
        }
        apply(&mut store, "state=working:id=r0");
        let changes = apply(&mut store, "state=working:id=new");
        assert_eq!(changes.removed, vec!["r1"]);
        assert_eq!(store.snapshot().len(), MAX_RECORDS);
        let changes = apply(&mut store, "state=done:id=r0");
        assert!(
            changes.removed.is_empty(),
            "updating an existing record evicts nothing"
        );
    }

    #[test]
    fn process_exit_and_a_new_prompt_drop_only_working_and_blocked() {
        let mut store = RecordStore::default();
        for (id, state) in [
            ("a", "working"),
            ("b", "blocked"),
            ("c", "idle"),
            ("d", "done"),
            ("e", "error"),
        ] {
            apply(&mut store, &format!("state={state}:id={id}"));
        }
        let changes = store.drop_transient();
        assert_eq!(changes.removed, vec!["a", "b"]);
        assert_eq!(ids(&store), vec!["c", "d", "e"]);
    }

    #[test]
    fn a_full_reset_removes_every_record() {
        let mut store = RecordStore::default();
        apply(&mut store, "state=done");
        apply(&mut store, "state=done:id=x");
        assert!(store.reset().reset);
        assert!(store.is_empty());
    }

    #[test]
    fn a_child_takes_app_from_its_nearest_ancestor_that_has_one() {
        let mut store = RecordStore::default();
        apply(&mut store, "state=working:app=deploy");
        apply(&mut store, "state=working:id=eu");
        apply(&mut store, "state=working:id=eu/west:app=kubectl");
        apply(&mut store, "state=working:id=eu/west/pod");
        apply(&mut store, "state=working:id=orphan/child");
        let apps: Vec<(String, String)> = store
            .resolved()
            .into_iter()
            .map(|r| (r.id, r.app))
            .collect();
        assert_eq!(
            apps,
            vec![
                ("".into(), "deploy".into()),
                ("eu".into(), "deploy".into()),
                ("eu/west".into(), "kubectl".into()),
                ("eu/west/pod".into(), "kubectl".into()),
                ("orphan/child".into(), "deploy".into()),
            ]
        );
    }

    #[test]
    fn a_mirror_merging_changes_ends_up_with_the_same_records() {
        let mut origin = RecordStore::default();
        let mut mirror = RecordStore::default();
        for body in [
            "state=working",
            "state=working:id=a",
            "state=blocked:id=a/b:kind=permission",
            "state=clear:id=a",
            "state=done",
        ] {
            mirror.merge(&apply(&mut origin, body));
        }
        mirror.merge(&origin.drop_transient());
        assert_eq!(origin.snapshot(), mirror.snapshot());
        mirror.merge(&origin.reset());
        assert!(mirror.is_empty());
    }
}
