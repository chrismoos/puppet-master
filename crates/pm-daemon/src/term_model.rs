//! Server-side terminal state model. Every PTY output byte feeds a VT
//! emulator so attach can serialize the terminal's exact current state.
//! A raw byte-ring tail cannot do this: full-screen agents enter the
//! alternate buffer and enable mouse tracking once per process lifetime
//! and then self-manage their transcript, so a mid-stream tail replays
//! into the wrong buffer with no mouse protocol and unrecoverable
//! content.

use std::panic::{catch_unwind, AssertUnwindSafe};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color, Handler, NamedColor, NamedPrivateMode, PrivateMode, Processor, Rgb,
};

pub const MODEL_SCROLLBACK_LINES: usize = 5_000;

/// Byte ring of raw PTY output, shared by the local mux and the
/// controller-side mirror of a remote terminal.
pub(crate) struct Ring {
    buf: Vec<u8>,
    cap: usize,
}

impl Ring {
    pub(crate) fn new(cap: usize) -> Self {
        Ring {
            buf: Vec::new(),
            cap,
        }
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        if data.len() >= self.cap {
            self.buf.clear();
            self.buf.extend_from_slice(&data[data.len() - self.cap..]);
            return;
        }
        let overflow = (self.buf.len() + data.len()).saturating_sub(self.cap);
        if overflow > 0 {
            self.buf.drain(..overflow);
        }
        self.buf.extend_from_slice(data);
    }

    pub(crate) fn reset(&mut self, data: &[u8]) {
        self.buf.clear();
        self.push(data);
    }

    pub(crate) fn snapshot(&self) -> bytes::Bytes {
        bytes::Bytes::copy_from_slice(&self.buf)
    }
}

/// Ring and VT model kept in lockstep. One lock around this pair is what
/// makes snapshot-at-attach consistent with the live stream, both in the
/// local mux and in the controller's mirror of a remote terminal.
pub(crate) struct Retained {
    pub(crate) ring: Ring,
    pub(crate) model: TerminalModel,
    cols: u16,
    rows: u16,
}

impl Retained {
    pub(crate) fn new(cap: usize, cols: u16, rows: u16) -> Self {
        Retained {
            ring: Ring::new(cap),
            model: TerminalModel::new(cols, rows),
            cols,
            rows,
        }
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        self.ring.push(data);
        self.model.advance(data);
    }

    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.model.resize(cols, rows);
    }

    pub(crate) fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Replaces all retained state with a replay payload: the ring holds
    /// the raw bytes and the model is rebuilt by parsing them at the
    /// current dimensions.
    pub(crate) fn reset_from_replay(&mut self, data: &[u8]) {
        self.ring.reset(data);
        self.model = TerminalModel::new(self.cols, self.rows);
        self.model.advance(data);
    }
}

pub struct TerminalModel {
    term: Term<VoidListener>,
    parser: Processor,
    region: ScrollRegion,
    region_parser: Processor,
    /// History lines a resize's reflow pushed out of the screen, watched
    /// until the program's output shows whether it prints them again.
    spill: Option<Spill>,
    /// History was rewritten since this was last read, so a snapshot sent
    /// before it is stale.
    rewritten: bool,
}

/// History left by a reflow. A program that answers a resize by homing the
/// cursor and repainting, as Claude Code does, prints these lines again, so
/// keeping them repeats its header in scrollback on every resize. Only a
/// line the repaint actually reprints is dropped, so a program that repaints
/// less than its old screen loses nothing.
struct Spill {
    /// Positions from the top of history, which stay put while the history
    /// limit is raised and nothing falls off the top.
    lines: Vec<usize>,
    /// History size when the last resize ended, to notice output that
    /// scrolled before the program redrew.
    history: usize,
    /// The program homed the cursor after the resize and is repainting.
    redrawing: bool,
    /// Repaint output seen, which ends the watch once it is clearly over.
    redraw_bytes: usize,
    /// Text the repaint has printed so far, since a line can arrive split
    /// across several reads.
    printed: String,
}

/// History lines a reflow may add while its spill is watched. Raising the
/// limit by this much keeps the oldest line in place, so positions counted
/// from the top stay valid.
const SPILL_HISTORY_MARGIN: usize = 2_000;
/// Repaint output after which unmatched spill lines are kept for good.
const SPILL_REDRAW_LIMIT_BYTES: usize = 256 * 1024;

/// Cursor home, the start of a full-screen redraw.
const CURSOR_HOME: &[u8] = b"\x1b[H";
const CURSOR_HOME_EXPLICIT: &[u8] = b"\x1b[1;1H";

/// alacritty applies DECSTBM but does not expose the region, so a second
/// parser over the same bytes tracks what the snapshot has to restore.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScrollRegion {
    rows: usize,
    top: usize,
    bottom: usize,
}

impl ScrollRegion {
    fn full(rows: usize) -> Self {
        ScrollRegion {
            rows,
            top: 0,
            bottom: rows,
        }
    }

    fn is_full(&self) -> bool {
        self.top == 0 && self.bottom == self.rows
    }
}

impl Handler for ScrollRegion {
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        let bottom = bottom.unwrap_or(self.rows);
        if top >= bottom {
            return;
        }
        self.top = top.saturating_sub(1).min(self.rows);
        self.bottom = bottom.min(self.rows);
    }

    fn reset_state(&mut self) {
        *self = ScrollRegion::full(self.rows);
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        if mode == PrivateMode::Named(NamedPrivateMode::ColumnMode) {
            *self = ScrollRegion::full(self.rows);
        }
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        if mode == PrivateMode::Named(NamedPrivateMode::ColumnMode) {
            *self = ScrollRegion::full(self.rows);
        }
    }
}

enum Redraw {
    /// The cursor went home before any text was printed.
    Home,
    /// Only escape sequences so far, which a PTY read may split off.
    Undecided,
    /// Text was printed first, so this program is not redrawing.
    Not,
}

/// The length of the escape sequence at the start of `rest`, or None when
/// it is cut off by the end of the read. Covers CSI, the string sequences
/// (OSC, DCS, APC, PM, SOS), which a shell's title and command marks use,
/// and two-byte escapes.
fn escape_len(rest: &[u8]) -> Option<usize> {
    match rest.get(1)? {
        b'[' => rest[2..]
            .iter()
            .position(|b| (0x40..=0x7e).contains(b))
            .map(|end| 2 + end + 1),
        b']' | b'P' | b'_' | b'^' | b'X' => {
            let body = &rest[2..];
            body.iter().enumerate().find_map(|(at, byte)| match byte {
                0x07 => Some(2 + at + 1),
                0x1b if body.get(at + 1) == Some(&b'\\') => Some(2 + at + 2),
                _ => None,
            })
        }
        _ => Some(2),
    }
}

/// Reads the start of a program's output after a resize.
fn redraw_signal(bytes: &[u8]) -> Redraw {
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        if rest.starts_with(CURSOR_HOME) || rest.starts_with(CURSOR_HOME_EXPLICIT) {
            return Redraw::Home;
        }
        if rest[0] != 0x1b {
            return Redraw::Not;
        }
        match escape_len(rest) {
            Some(len) => at += len,
            None => return Redraw::Undecided,
        }
    }
    Redraw::Undecided
}

/// Printed text with escape sequences and whitespace removed, so a line
/// laid out with cursor moves compares equal to the same line with spaces.
fn printed_text(bytes: &[u8]) -> String {
    let mut text = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        if rest[0] == 0x1b {
            at += escape_len(rest).unwrap_or(rest.len());
        } else {
            text.push(rest[0]);
            at += 1;
        }
    }
    String::from_utf8_lossy(&text)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

impl TerminalModel {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, MODEL_SCROLLBACK_LINES)
    }

    fn with_history(cols: u16, rows: u16, history: usize) -> Self {
        let config = Config {
            scrolling_history: history,
            ..Config::default()
        };
        let size = TermSize::new(cols.max(2) as usize, rows.max(1) as usize);
        TerminalModel {
            term: Term::new(config, &size, VoidListener),
            parser: Processor::new(),
            region: ScrollRegion::full(size.screen_lines()),
            region_parser: Processor::new(),
            spill: None,
            rewritten: false,
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        let cols = self.term.grid().columns() as u16;
        let rows = self.term.grid().screen_lines() as u16;
        if catch_unwind(AssertUnwindSafe(|| self.advance_inner(bytes))).is_err() {
            self.recover(cols, rows, "output");
        }
    }

    fn advance_inner(&mut self, bytes: &[u8]) {
        if self
            .spill
            .as_ref()
            .is_some_and(|spill| spill.history != self.term.grid().history_size())
        {
            self.end_spill_watch();
        }
        if let Some(mut spill) = self.spill.take() {
            if !spill.redrawing {
                let decided = redraw_signal(bytes);
                match decided {
                    Redraw::Home => spill.redrawing = true,
                    Redraw::Undecided => {}
                    Redraw::Not => {
                        self.end_spill_watch();
                        spill.lines.clear();
                    }
                }
            }
            if spill.redrawing {
                spill.redraw_bytes += bytes.len();
                self.drop_reprinted(&mut spill, bytes);
                let full = self.term.grid().history_size() + bytes.len()
                    >= MODEL_SCROLLBACK_LINES + SPILL_HISTORY_MARGIN;
                if spill.lines.is_empty() || spill.redraw_bytes > SPILL_REDRAW_LIMIT_BYTES || full {
                    self.end_spill_watch();
                } else {
                    self.spill = Some(spill);
                }
            } else if !spill.lines.is_empty() {
                self.spill = Some(spill);
            }
        }
        self.parser.advance(&mut self.term, bytes);
        self.region_parser.advance(&mut self.region, bytes);
        if self
            .spill
            .as_ref()
            .is_some_and(|spill| spill.history != self.term.grid().history_size())
        {
            self.end_spill_watch();
        }
    }

    fn recover(&mut self, cols: u16, rows: u16, operation: &str) {
        tracing::error!(cols, rows, operation, "terminal emulator failed; resetting its retained screen while keeping the live PTY running");
        *self = Self::new(cols, rows);
        self.rewritten = true;
    }

    /// Drops the spill lines whose text the repaint in `bytes` prints again.
    /// Blank spill lines go with them, since only the repaint gave them
    /// their place.
    fn drop_reprinted(&mut self, spill: &mut Spill, bytes: &[u8]) {
        let fresh = printed_text(bytes);
        if fresh.is_empty() {
            return;
        }
        spill.printed.push_str(&fresh);
        let printed = spill.printed.as_str();
        let history = self.term.grid().history_size();
        spill.lines.retain(|top| *top < history);
        let text_of = |top: usize| -> String {
            let row = &self.term.grid()[Line(top as i32 - history as i32)];
            (0..self.term.grid().columns())
                .map(|column| row[Column(column)].c)
                .filter(|c| !c.is_whitespace())
                .collect()
        };
        let texts: Vec<(usize, String)> =
            spill.lines.iter().map(|&top| (top, text_of(top))).collect();
        if !texts
            .iter()
            .any(|(_, text)| !text.is_empty() && printed.contains(text.as_str()))
        {
            return;
        }
        let dropped: Vec<usize> = texts
            .into_iter()
            .filter(|(_, text)| text.is_empty() || printed.contains(text.as_str()))
            .map(|(top, _)| top)
            .collect();
        spill.lines.retain(|top| !dropped.contains(top));
        for top in &mut spill.lines {
            *top -= dropped.iter().filter(|gone| **gone < *top).count();
        }
        self.rebuild_without(&dropped);
        spill.history = self.term.grid().history_size();
    }

    /// Restores the history limit raised while a spill was watched. Lines
    /// past it fall off the top, as they would have without the watch.
    fn end_spill_watch(&mut self) {
        self.spill = None;
        self.term.grid_mut().update_history(MODEL_SCROLLBACK_LINES);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if catch_unwind(AssertUnwindSafe(|| self.resize_inner(cols, rows))).is_err() {
            self.recover(cols, rows, "resize");
        }
    }

    fn resize_inner(&mut self, cols: u16, rows: u16) {
        let size = TermSize::new(cols.max(2) as usize, rows.max(1) as usize);
        let primary = !self.term.mode().contains(TermMode::ALT_SCREEN);
        if primary && self.spill.is_none() {
            // With the limit raised, a full history still grows, so the
            // spill is measurable and nothing shifts under its positions.
            self.term
                .grid_mut()
                .update_history(MODEL_SCROLLBACK_LINES + SPILL_HISTORY_MARGIN);
        }
        let before = self.term.grid().history_size();
        self.region = ScrollRegion::full(size.screen_lines());
        self.term.resize(size);
        let after = self.term.grid().history_size();
        // Resizes arriving faster than the program can redraw, as while a
        // window is dragged, add up until its output decides them.
        // A repaint already under way has had its chance at its lines.
        let mut lines = match self.spill.take() {
            Some(spill) if !spill.redrawing => spill.lines,
            _ => Vec::new(),
        };
        lines.retain(|top| *top < after);
        lines.extend(before..after.max(before));
        if primary && !lines.is_empty() {
            self.spill = Some(Spill {
                lines,
                history: after,
                redrawing: false,
                redraw_bytes: 0,
                printed: String::new(),
            });
        } else {
            self.end_spill_watch();
        }
    }

    /// Rebuilds the model from its own snapshot without the given history
    /// lines, counted from the top. The grid cannot remove lines in place.
    fn rebuild_without(&mut self, dropped: &[usize]) {
        let (cols, rows) = {
            let grid = self.term.grid();
            (grid.columns() as u16, grid.screen_lines() as u16)
        };
        let mut out = String::new();
        out.push_str("\x1b[0m");
        self.write_modes(&mut out);
        self.write_primary_screen_skipping(&mut out, dropped);
        self.write_scroll_region(&mut out);
        self.write_cursor(&mut out);
        if self.term.mode().contains(TermMode::INSERT) {
            out.push_str("\x1b[4h");
        }
        out.push_str("\x1b[0m");
        let limit = MODEL_SCROLLBACK_LINES + SPILL_HISTORY_MARGIN;
        let mut rebuilt = TerminalModel::with_history(cols, rows, limit);
        rebuilt.advance(out.as_bytes());
        let spill = self.spill.take();
        *self = rebuilt;
        self.spill = spill;
        self.rewritten = true;
    }

    /// Whether history was rewritten since the last call.
    pub fn take_rewritten(&mut self) -> bool {
        std::mem::take(&mut self.rewritten)
    }

    /// Escape sequences that rebuild the terminal's current state on a
    /// freshly reset emulator: private modes, content, scrolling region,
    /// cursor, origin, and insert. Client replay prefixes RIS, which
    /// clears region/origin/insert, so the snapshot must put them back.
    /// While the alternate screen is active the primary screen's content
    /// is not reachable through the public grid API, so the snapshot
    /// restores modes and the alternate screen only.
    pub fn snapshot(&self) -> Vec<u8> {
        match catch_unwind(AssertUnwindSafe(|| self.snapshot_inner())) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                tracing::error!("terminal emulator snapshot failed; sending a reset screen without stopping the live PTY");
                b"\x1bc".to_vec()
            }
        }
    }

    fn snapshot_inner(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push_str("\x1b[0m");
        self.write_modes(&mut out);
        let mode = *self.term.mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            out.push_str("\x1b[?1049h");
            self.write_alt_screen(&mut out);
        } else {
            self.write_primary_screen(&mut out);
        }
        self.write_scroll_region(&mut out);
        self.write_cursor(&mut out);
        if mode.contains(TermMode::INSERT) {
            out.push_str("\x1b[4h");
        }
        out.push_str("\x1b[0m");
        out.into_bytes()
    }

    fn write_scroll_region(&self, out: &mut String) {
        if self.region.is_full() {
            return;
        }
        out.push_str(&format!(
            "\x1b[{};{}r",
            self.region.top + 1,
            self.region.bottom
        ));
    }

    fn write_modes(&self, out: &mut String) {
        let mode = *self.term.mode();
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.push_str("\x1b[?25l");
        }
        if mode.contains(TermMode::APP_CURSOR) {
            out.push_str("\x1b[?1h");
        }
        if mode.contains(TermMode::APP_KEYPAD) {
            out.push_str("\x1b=");
        }
        if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            out.push_str("\x1b[?1000h");
        }
        if mode.contains(TermMode::MOUSE_DRAG) {
            out.push_str("\x1b[?1002h");
        }
        if mode.contains(TermMode::MOUSE_MOTION) {
            out.push_str("\x1b[?1003h");
        }
        if mode.contains(TermMode::UTF8_MOUSE) {
            out.push_str("\x1b[?1005h");
        }
        if mode.contains(TermMode::SGR_MOUSE) {
            out.push_str("\x1b[?1006h");
        }
        if mode.contains(TermMode::FOCUS_IN_OUT) {
            out.push_str("\x1b[?1004h");
        }
        if mode.contains(TermMode::BRACKETED_PASTE) {
            out.push_str("\x1b[?2004h");
        }
        if !mode.contains(TermMode::LINE_WRAP) {
            out.push_str("\x1b[?7l");
        }
    }

    fn write_primary_screen(&self, out: &mut String) {
        self.write_primary_screen_skipping(out, &[]);
    }

    fn write_primary_screen_skipping(&self, out: &mut String, skip: &[usize]) {
        let grid = self.term.grid();
        let history = grid.history_size() as i32;
        let skipped = |line: i32| line < 0 && skip.contains(&((line + history) as usize));
        let screen_lines = grid.screen_lines() as i32;
        let columns = grid.columns();

        let mut rows: Vec<(String, bool)> = Vec::new();
        for line in -history..screen_lines {
            if skipped(line) {
                continue;
            }
            let row = &grid[Line(line)];
            let mut text = String::new();
            let mut sgr = SgrState::default();
            let mut pending_blanks = 0usize;
            for column in 0..columns {
                let cell = &row[Column(column)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                if cell.c == ' ' && cell.bg == Color::Named(NamedColor::Background) {
                    pending_blanks += 1;
                    continue;
                }
                for _ in 0..pending_blanks {
                    sgr.apply_default(&mut text);
                    text.push(' ');
                }
                pending_blanks = 0;
                sgr.apply(&mut text, cell.fg, cell.bg, cell.flags);
                text.push(cell.c);
            }
            let wrapped = columns > 0 && row[Column(columns - 1)].flags.contains(Flags::WRAPLINE);
            rows.push((text, wrapped));
        }
        if history == 0 {
            let cursor_row = self.term.grid().cursor.point.line.0.max(0) as usize;
            let last_content = rows
                .iter()
                .rposition(|(text, _)| !text.is_empty())
                .unwrap_or(0);
            rows.truncate(cursor_row.max(last_content) + 1);
        }
        let count = rows.len();
        for (index, (text, wrapped)) in rows.into_iter().enumerate() {
            out.push_str("\x1b[0m");
            out.push_str(&text);
            if !wrapped && index + 1 < count {
                out.push_str("\r\n");
            }
        }
    }

    fn write_alt_screen(&self, out: &mut String) {
        let grid = self.term.grid();
        let screen_lines = grid.screen_lines() as i32;
        let columns = grid.columns();
        for line in 0..screen_lines {
            let row = &grid[Line(line)];
            let mut text = String::new();
            let mut sgr = SgrState::default();
            let mut pending_blanks = 0usize;
            for column in 0..columns {
                let cell = &row[Column(column)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                if cell.c == ' ' && cell.bg == Color::Named(NamedColor::Background) {
                    pending_blanks += 1;
                    continue;
                }
                for _ in 0..pending_blanks {
                    sgr.apply_default(&mut text);
                    text.push(' ');
                }
                pending_blanks = 0;
                sgr.apply(&mut text, cell.fg, cell.bg, cell.flags);
                text.push(cell.c);
            }
            if text.is_empty() {
                continue;
            }
            out.push_str(&format!("\x1b[{};1H\x1b[0m", line + 1));
            out.push_str(&text);
        }
    }

    fn write_cursor(&self, out: &mut String) {
        let cursor = self.term.grid().cursor.point;
        let line = cursor.line.0.max(0) as usize;
        if self.term.mode().contains(TermMode::ORIGIN) {
            out.push_str("\x1b[?6h");
            out.push_str(&format!(
                "\x1b[{};{}H",
                line.saturating_sub(self.region.top) + 1,
                cursor.column.0 + 1
            ));
            return;
        }
        out.push_str(&format!("\x1b[{};{}H", line + 1, cursor.column.0 + 1));
    }

    #[cfg(test)]
    pub fn visible_text(&self) -> Vec<String> {
        let grid = self.term.grid();
        let mut lines = Vec::new();
        for line in 0..grid.screen_lines() as i32 {
            let row = &grid[Line(line)];
            let mut text = String::new();
            for column in 0..grid.columns() {
                text.push(row[Column(column)].c);
            }
            lines.push(text.trim_end().to_string());
        }
        lines
    }

    #[cfg(test)]
    pub fn scrollback_text(&self) -> Vec<String> {
        let grid = self.term.grid();
        let mut lines = Vec::new();
        for line in -(grid.history_size() as i32)..0 {
            let row = &grid[Line(line)];
            let mut text = String::new();
            for column in 0..grid.columns() {
                text.push(row[Column(column)].c);
            }
            lines.push(text.trim_end().to_string());
        }
        lines
    }

    #[cfg(test)]
    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    #[cfg(test)]
    pub fn cursor_position(&self) -> (i32, usize) {
        let point = self.term.grid().cursor.point;
        (point.line.0, point.column.0)
    }
}

#[derive(Default)]
struct SgrState {
    current: Option<(Color, Color, Flags)>,
}

const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

impl SgrState {
    fn apply_default(&mut self, out: &mut String) {
        self.apply(
            out,
            Color::Named(NamedColor::Foreground),
            Color::Named(NamedColor::Background),
            Flags::empty(),
        );
    }

    fn apply(&mut self, out: &mut String, fg: Color, bg: Color, flags: Flags) {
        let style = flags & STYLE_FLAGS;
        if self.current == Some((fg, bg, style)) {
            return;
        }
        self.current = Some((fg, bg, style));
        out.push_str("\x1b[0");
        if style.contains(Flags::BOLD) {
            out.push_str(";1");
        }
        if style.contains(Flags::DIM) {
            out.push_str(";2");
        }
        if style.contains(Flags::ITALIC) {
            out.push_str(";3");
        }
        if style.contains(Flags::UNDERLINE) {
            out.push_str(";4");
        }
        if style.contains(Flags::INVERSE) {
            out.push_str(";7");
        }
        if style.contains(Flags::HIDDEN) {
            out.push_str(";8");
        }
        if style.contains(Flags::STRIKEOUT) {
            out.push_str(";9");
        }
        push_color(out, fg, true);
        push_color(out, bg, false);
        out.push('m');
    }
}

fn push_color(out: &mut String, color: Color, foreground: bool) {
    match color {
        Color::Named(NamedColor::Foreground) if foreground => {}
        Color::Named(NamedColor::Background) if !foreground => {}
        Color::Named(named) => {
            let index = named as usize;
            if index < 8 {
                out.push_str(&format!(
                    ";{}",
                    if foreground { 30 + index } else { 40 + index }
                ));
            } else if index < 16 {
                out.push_str(&format!(
                    ";{}",
                    if foreground { 82 + index } else { 92 + index }
                ));
            }
        }
        Color::Indexed(index) => {
            out.push_str(&format!(
                ";{};5;{}",
                if foreground { 38 } else { 48 },
                index
            ));
        }
        Color::Spec(Rgb { r, g, b }) => {
            out.push_str(&format!(
                ";{};2;{};{};{}",
                if foreground { 38 } else { 48 },
                r,
                g,
                b
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(cols: u16, rows: u16, bytes: &[u8]) -> TerminalModel {
        let mut model = TerminalModel::new(cols, rows);
        model.advance(bytes);
        model
    }

    fn round_trip(cols: u16, rows: u16, bytes: &[u8]) -> (TerminalModel, TerminalModel) {
        let original = fed(cols, rows, bytes);
        let mut restored = TerminalModel::new(cols, rows);
        restored.advance(&original.snapshot());
        (original, restored)
    }

    #[test]
    fn plain_lines_round_trip_with_scrollback() {
        let mut input = Vec::new();
        for index in 0..30 {
            input.extend_from_slice(format!("line-{index:02}\r\n").as_bytes());
        }
        let (original, restored) = round_trip(40, 10, &input);
        assert_eq!(original.visible_text(), restored.visible_text());
        assert_eq!(original.scrollback_text(), restored.scrollback_text());
        assert_eq!(original.cursor_position(), restored.cursor_position());
    }

    #[test]
    fn prompt_with_trailing_blank_lines_round_trip() {
        let mut input = Vec::new();
        for index in 0..20 {
            input.extend_from_slice(format!("output-{index:02}\r\n").as_bytes());
        }
        input.extend_from_slice(b"\x1b[5;1H\x1b[J> prompt\r\n-----\r\nstatus\x1b[5;10H");
        let mut original = fed(40, 10, &input);
        original.resize(40, 15);
        let mut restored = TerminalModel::new(40, 15);
        restored.advance(&original.snapshot());
        assert_eq!(original.visible_text(), restored.visible_text());
        assert_eq!(original.cursor_position(), restored.cursor_position());
    }

    #[test]
    fn initial_width_preserves_cursor_alignment() {
        let mut input = Vec::new();
        for index in 0..50 {
            input.extend_from_slice(format!("output-{index:02}\r\n").as_bytes());
        }
        let rule = "─".repeat(100);
        input.extend_from_slice(
            format!("{rule}\r\n> prompt\r\n{rule}\r\nstatus\r\n{rule}\r\nfooter").as_bytes(),
        );
        input.extend_from_slice(b"\x1b[4A\x1b[10G");
        let original = fed(100, 32, &input);
        let mut restored = TerminalModel::new(100, 32);
        restored.advance(&original.snapshot());
        assert_eq!(original.visible_text(), restored.visible_text());
        assert_eq!(original.cursor_position(), restored.cursor_position());
    }

    #[test]
    fn cleared_screen_with_scrollback_round_trip() {
        let mut input = Vec::new();
        for index in 0..20 {
            input.extend_from_slice(format!("line-{index:02}\r\n").as_bytes());
        }
        input.extend_from_slice(b"\x1b[H\x1b[2J$ \x1b[1;3H");
        let (original, restored) = round_trip(40, 10, &input);
        assert_eq!(original.visible_text(), restored.visible_text());
        assert_eq!(original.scrollback_text(), restored.scrollback_text());
        assert_eq!(original.cursor_position(), restored.cursor_position());
    }

    #[test]
    fn prompt_without_scrollback_trailing_blanks_round_trip() {
        let input = b"> prompt\r\n-----\r\nstatus\x1b[1;10H";
        let (original, restored) = round_trip(40, 10, input);
        assert_eq!(original.visible_text(), restored.visible_text());
        assert_eq!(original.cursor_position(), restored.cursor_position());
    }

    #[test]
    fn alt_screen_and_mouse_modes_round_trip() {
        let mut input = Vec::new();
        input.extend_from_slice(b"before-alt\r\n");
        input.extend_from_slice(
            b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[?1004h",
        );
        input.extend_from_slice(b"\x1b[2J\x1b[H\x1b[3;5Hboxed content\x1b[?25l");
        let (original, restored) = round_trip(60, 12, &input);
        assert!(original.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(original.mode(), restored.mode());
        assert_eq!(original.visible_text(), restored.visible_text());
        assert_eq!(original.cursor_position(), restored.cursor_position());
    }

    #[test]
    fn a_ring_tail_loses_modes_but_the_snapshot_keeps_them() {
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[2J\x1b[H");
        for frame in 0..50 {
            input.extend_from_slice(format!("\x1b[5;1Hframe {frame:04}").as_bytes());
        }
        let original = fed(80, 20, &input);

        let tail_start = input.len() - 200;
        let mut from_tail = TerminalModel::new(80, 20);
        from_tail.advance(&input[tail_start..]);
        assert!(!from_tail.mode().contains(TermMode::ALT_SCREEN));

        let mut from_snapshot = TerminalModel::new(80, 20);
        from_snapshot.advance(&original.snapshot());
        assert!(from_snapshot.mode().contains(TermMode::ALT_SCREEN));
        assert!(from_snapshot.mode().contains(TermMode::SGR_MOUSE));
        assert_eq!(original.visible_text(), from_snapshot.visible_text());
    }

    #[test]
    fn colors_and_styles_round_trip() {
        let input = b"\x1b[1;31mred-bold\x1b[0m plain \x1b[4;38;5;42mgreen-under\x1b[0m\r\n\x1b[7;38;2;10;20;30mrgb-inverse\x1b[0m\r\ndone";
        let (original, restored) = round_trip(50, 8, input);
        assert_eq!(original.visible_text(), restored.visible_text());
        let snapshot = String::from_utf8(original.snapshot()).unwrap();
        assert!(snapshot.contains(";31"));
        assert!(snapshot.contains(";38;5;42"));
        assert!(snapshot.contains(";38;2;10;20;30"));
    }

    #[test]
    fn wrapped_lines_round_trip_without_double_wrapping() {
        let long = "x".repeat(95);
        let input = format!("{long}\r\nshort\r\n");
        let (original, restored) = round_trip(40, 10, input.as_bytes());
        assert_eq!(original.visible_text(), restored.visible_text());
        assert_eq!(original.scrollback_text(), restored.scrollback_text());
    }

    #[test]
    fn a_restored_scroll_region_keeps_scrolling_only_the_region() {
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[10;1H> composer\x1b[1;7r\x1b[1;1H");
        for index in 0..9 {
            input.extend_from_slice(format!("line-{index}\r\n").as_bytes());
        }
        let mut original = fed(80, 10, &input);
        let mut restored = TerminalModel::new(80, 10);
        restored.advance(&original.snapshot());
        assert_eq!(original.visible_text(), restored.visible_text());
        assert_eq!(original.cursor_position(), restored.cursor_position());

        let more = b"more-a\r\nmore-b\r\n";
        original.advance(more);
        restored.advance(more);
        let screen = restored.visible_text();
        assert_eq!(screen[9], "> composer");
        assert_eq!(original.visible_text(), screen);
        assert_eq!(original.cursor_position(), restored.cursor_position());
        assert_ne!(
            screen[6], "> composer",
            "newlines after restore must not scroll the composer row"
        );
    }

    #[test]
    fn an_alt_screen_transcript_pane_survives_restore_without_a_resize() {
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[1;17r\x1b[18;1H+-- box --+\x1b[19;1H| > type |\x1b[20;1H+---------+\x1b[17;1H");
        for index in 0..30 {
            input.extend_from_slice(format!("history-{index:02}\n").as_bytes());
        }
        let mut original = fed(60, 20, &input);
        let mut restored = TerminalModel::new(60, 20);
        restored.advance(&original.snapshot());
        assert!(restored.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(original.visible_text(), restored.visible_text());

        let frame = b"history-30\nhistory-31\n";
        original.advance(frame);
        restored.advance(frame);
        let screen = restored.visible_text();
        assert_eq!(screen[17], "+-- box --+");
        assert_eq!(screen[18], "| > type |");
        assert_eq!(screen[19], "+---------+");
        assert!(
            screen.iter().take(17).any(|row| row.contains("history-31")),
            "new output must scroll inside the region: {screen:?}"
        );
    }

    #[test]
    fn a_full_screen_region_emits_no_decstbm() {
        let model = fed(40, 8, b"\x1b[2;5r\x1bcplain");
        let snapshot = String::from_utf8(model.snapshot()).unwrap();
        assert!(!snapshot.contains('r'), "{snapshot:?}");
        let mut resized = fed(40, 8, b"\x1b[2;5r");
        resized.resize(40, 12);
        let snapshot = String::from_utf8(resized.snapshot()).unwrap();
        assert!(!snapshot.contains("\x1b[2;5r"), "{snapshot:?}");
    }

    #[test]
    fn origin_mode_round_trips_with_a_region_relative_cursor() {
        let mut original = fed(40, 12, b"\x1b[3;8r\x1b[?6h\x1b[2;4Hx");
        let mut restored = TerminalModel::new(40, 12);
        restored.advance(&original.snapshot());
        assert!(restored.mode().contains(TermMode::ORIGIN));
        assert_eq!(original.cursor_position(), restored.cursor_position());
        assert_eq!(original.visible_text(), restored.visible_text());

        let home = b"\x1b[1;1Hy";
        original.advance(home);
        restored.advance(home);
        assert_eq!(restored.visible_text()[2], "y");
        assert_eq!(original.visible_text(), restored.visible_text());
    }

    #[test]
    fn insert_mode_round_trips() {
        let mut original = fed(40, 4, b"abc\x1b[4h\x1b[1;1H");
        let mut restored = TerminalModel::new(40, 4);
        restored.advance(&original.snapshot());
        assert!(restored.mode().contains(TermMode::INSERT));
        original.advance(b"Z");
        restored.advance(b"Z");
        assert_eq!(restored.visible_text()[0], "Zabc");
        assert_eq!(original.visible_text(), restored.visible_text());
    }

    /// Claude Code answers a resize by homing the cursor, erasing every row
    /// and redrawing its whole screen. The reflow's spill into history is
    /// then a copy of what it redraws, and repeated toggles used to stack
    /// its header in scrollback. A program that does not redraw keeps it.
    #[test]
    fn a_full_redraw_after_narrowing_leaves_no_copy_in_scrollback() {
        // A screen the program lays out for its width, as Claude Code does.
        fn screen(model: &mut TerminalModel, cols: usize) {
            model.advance(b"HEADER one\r\nHEADER two\r\n");
            for _ in 0..6 {
                model.advance(&vec![b'-'; cols - 2]);
                model.advance(b"\r\n");
            }
            model.advance(b"prompt> ");
        }
        fn redraw(model: &mut TerminalModel, cols: usize, rows: usize) {
            model.advance(b"\x1b[H");
            for _ in 0..rows {
                model.advance(b"\x1b[2K\x1b[1B");
            }
            model.advance(b"\x1b[H");
            screen(model, cols);
        }
        let header_copies = |model: &TerminalModel| {
            model
                .scrollback_text()
                .iter()
                .filter(|line| line.contains("HEADER one"))
                .count()
        };

        let mut redrawn = TerminalModel::new(140, 10);
        screen(&mut redrawn, 140);
        for _ in 0..3 {
            redrawn.resize(90, 10);
            redrawn.advance(b"\x1b[?25l");
            redraw(&mut redrawn, 90, 10);
            redrawn.resize(140, 10);
            redraw(&mut redrawn, 140, 10);
        }
        assert_eq!(
            header_copies(&redrawn),
            0,
            "{:?}",
            redrawn.scrollback_text()
        );
        assert_eq!(redrawn.visible_text()[0], "HEADER one");

        // A dragged window resizes several times before the program's one
        // redraw for the last size arrives.
        for cols in [120, 100, 90, 80] {
            redrawn.resize(cols, 10);
        }
        redraw(&mut redrawn, 80, 10);
        assert_eq!(
            header_copies(&redrawn),
            0,
            "{:?}",
            redrawn.scrollback_text()
        );

        let mut shell = TerminalModel::new(140, 10);
        screen(&mut shell, 140);
        shell.resize(90, 10);
        shell.advance(b"ls\r\n");
        assert_eq!(
            header_copies(&shell),
            1,
            "a shell's reflowed lines stay in history"
        );
    }

    fn claude_screen(model: &mut TerminalModel, cols: usize) {
        model.advance(b"HEADER one\r\nHEADER two\r\n");
        for _ in 0..6 {
            model.advance(&vec![b'-'; cols - 2]);
            model.advance(b"\r\n");
        }
        model.advance(b"prompt> ");
    }

    fn claude_redraw(model: &mut TerminalModel, cols: usize, rows: usize) {
        model.advance(b"\x1b[H");
        for _ in 0..rows {
            model.advance(b"\x1b[2K\x1b[1B");
        }
        model.advance(b"\x1b[H");
        claude_screen(model, cols);
    }

    fn lines_containing(model: &TerminalModel, needle: &str) -> usize {
        model
            .scrollback_text()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    /// A shell integration's command mark or a window title can come ahead
    /// of the repaint. Neither is printed text, so the repaint still counts.
    #[test]
    fn a_title_or_command_mark_before_the_repaint_still_counts() {
        let mut model = TerminalModel::new(140, 10);
        claude_screen(&mut model, 140);
        model.resize(90, 10);
        model.advance(b"\x1b]777;pm-command;bash redraw.sh\x07");
        model.advance(b"\x1b]0;\xe2\x9c\xb3 Claude Code\x1b\\");
        claude_redraw(&mut model, 90, 10);
        assert_eq!(
            lines_containing(&model, "HEADER one"),
            0,
            "{:?}",
            model.scrollback_text()
        );
    }

    /// A program that homes the cursor and clears but prints something else
    /// has not repainted the spilled lines, so they stay in history.
    #[test]
    fn spill_lines_a_repaint_does_not_print_again_are_kept() {
        let mut model = TerminalModel::new(140, 10);
        claude_screen(&mut model, 140);
        model.resize(90, 10);
        model.advance(b"\x1b[1;1H\x1b[J");
        model.advance(b"a different screen entirely\r\n");
        assert_eq!(
            lines_containing(&model, "HEADER one"),
            1,
            "{:?}",
            model.scrollback_text()
        );
    }

    /// At the history limit a reflow does not grow history, which used to
    /// hide the spill. The limit is raised while a resize is watched.
    #[test]
    fn a_full_history_still_drops_a_reprinted_spill() {
        let mut model = TerminalModel::new(140, 10);
        for line in 0..MODEL_SCROLLBACK_LINES + 50 {
            model.advance(format!("filler {line}\r\n").as_bytes());
        }
        claude_screen(&mut model, 140);
        for _ in 0..3 {
            model.resize(90, 10);
            claude_redraw(&mut model, 90, 10);
            model.resize(140, 10);
            claude_redraw(&mut model, 140, 10);
        }
        assert_eq!(
            lines_containing(&model, "HEADER one"),
            0,
            "{:?}",
            &model.scrollback_text()[MODEL_SCROLLBACK_LINES - 5..]
        );
        assert!(model.scrollback_text().len() <= MODEL_SCROLLBACK_LINES + SPILL_HISTORY_MARGIN);
        assert!(lines_containing(&model, "filler") >= MODEL_SCROLLBACK_LINES - 20);
        model.advance(b"plain output\r\n");
        model.resize(140, 10);
        assert!(
            model.scrollback_text().len() <= MODEL_SCROLLBACK_LINES,
            "the raised limit is restored once the watch ends"
        );
    }

    /// Shrinking the height pushes the top rows into history the same way.
    #[test]
    fn a_shorter_screen_drops_a_reprinted_spill() {
        let mut model = TerminalModel::new(140, 10);
        claude_screen(&mut model, 140);
        model.resize(140, 6);
        model.advance(b"\x1b[H");
        for _ in 0..6 {
            model.advance(b"\x1b[2K\x1b[1B");
        }
        model.advance(b"\x1b[H");
        model.advance(b"HEADER one\r\nHEADER two\r\n");
        for _ in 0..3 {
            model.advance(&[b'-'; 138]);
            model.advance(b"\r\n");
        }
        model.advance(b"prompt> ");
        assert_eq!(
            lines_containing(&model, "HEADER"),
            0,
            "{:?}",
            model.scrollback_text()
        );
    }

    #[test]
    fn resize_flows_into_the_model() {
        let mut model = TerminalModel::new(80, 24);
        model.advance(b"hello\r\n");
        model.resize(120, 40);
        let snapshot = String::from_utf8(model.snapshot()).unwrap();
        assert!(snapshot.contains("hello"));
    }
    #[test]
    fn clearing_history_during_a_split_redraw_keeps_later_output_live() {
        let mut model = TerminalModel::new(140, 10);
        claude_screen(&mut model, 140);
        model.resize(40, 4);
        model.advance(b"\x1b[H");
        model.advance(b"\x1b[2J\x1b[3J\x1b[H");
        model.advance(b"fresh output");
        assert!(model
            .visible_text()
            .iter()
            .any(|line| line.contains("fresh output")));
        assert!(model.scrollback_text().is_empty());
        assert!(
            !model.take_rewritten(),
            "clearing history must not require emulator recovery"
        );
        model.resize(80, 24);
        model.advance(b"\x1b[Hstill live");
        assert!(model
            .visible_text()
            .iter()
            .any(|line| line.contains("still live")));
    }

    #[test]
    fn history_clear_between_rapid_resizes_preserves_the_final_screen() {
        let mut model = TerminalModel::new(140, 10);
        claude_screen(&mut model, 140);
        for (cols, rows) in [(90, 6), (60, 4), (120, 8), (40, 3)] {
            model.resize(cols, rows);
            model.advance(b"\x1b[H");
            model.advance(b"\x1b[2J\x1b[3J\x1b[Hcurrent screen");
            model.advance(b"\x1b[2;1Hfooter");
        }
        assert_eq!(model.visible_text()[0], "current screen");
        assert_eq!(model.visible_text()[1], "footer");
        assert!(model.scrollback_text().is_empty());
        assert!(!model.take_rewritten());
    }

    #[test]
    fn an_emulator_failure_does_not_poison_the_retained_terminal_lock() {
        let retained = std::sync::Mutex::new(Retained::new(1024, 80, 24));
        {
            let mut state = retained.lock().unwrap();
            let outside = state.model.term.grid().screen_lines() as i32;
            state.model.term.grid_mut().cursor.point.line = Line(outside);
            state.push(b"x");
        }
        let mut state = retained.lock().unwrap();
        assert!(state.model.take_rewritten());
        assert_eq!(state.size(), (80, 24));
        state.push(b"still live");
        assert_eq!(state.model.visible_text()[0], "still live");
    }
}
