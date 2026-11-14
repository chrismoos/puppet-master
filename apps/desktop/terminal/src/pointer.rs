//! What the mouse does on the screen: selecting text, finding the link under
//! a cell, and reporting the mouse to a program that asked for it.

use std::ffi::c_void;
use std::ptr;

use crate::ffi;
use crate::vt::{Frame, Vt};

/// A mouse event a program asked to hear about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

/// Where the grid sits in the view, in pixels, for mouse positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseGeometry {
    pub width: u32,
    pub height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding: u32,
}

/// Link schemes the page opens from a terminal.
const LINK_PREFIXES: &[&str] = &["https://", "http://", "pm:"];
/// Characters that end a link found in plain text.
const LINK_STOPS: &[char] = &['<', '>', '"', '\'', '`', '|', '{', '}', '^'];
/// Punctuation that ends a sentence rather than a link.
const LINK_TRAILERS: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']'];
/// Bytes a link or a mouse report is expected to fit in; longer ones are retried.
const BUFFER: usize = 256;

/// Where the viewport sits in the scrollback, in rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scrollbar {
    pub total: u64,
    /// The first visible row, counted from the top of the scrollback.
    pub offset: u64,
    pub len: u64,
}

impl Scrollbar {
    pub fn at_bottom(&self) -> bool {
        self.offset + self.len >= self.total
    }
}

impl Vt {
    fn grid_ref(&self, x: u16, y: u16) -> Option<ffi::GhosttyGridRef> {
        self.grid_ref_at(
            ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_VIEWPORT,
            x,
            u32::from(y),
        )
    }

    fn grid_ref_at(
        &self,
        tag: ffi::GhosttyPointTag,
        x: u16,
        y: u32,
    ) -> Option<ffi::GhosttyGridRef> {
        let point = ffi::GhosttyPoint {
            tag,
            value: ffi::GhosttyPointValue {
                coordinate: ffi::GhosttyPointCoordinate { x, y },
            },
        };
        let mut found: ffi::GhosttyGridRef = unsafe { std::mem::zeroed() };
        let result = unsafe { ffi::ghostty_terminal_grid_ref(self.terminal, point, &mut found) };
        (result == ffi::GhosttyResult_GHOSTTY_SUCCESS).then_some(found)
    }

    fn set_selection(&mut self, selection: Option<&ffi::GhosttySelection>) -> bool {
        let value = selection.map_or(ptr::null(), |s| {
            (s as *const ffi::GhosttySelection).cast::<c_void>()
        });
        let result = unsafe {
            ffi::ghostty_terminal_set(
                self.terminal,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_SELECTION,
                value,
            )
        };
        result == ffi::GhosttyResult_GHOSTTY_SUCCESS
    }

    /// Selects from one viewport cell to another, as a drag does.
    pub fn select_range(&mut self, from: (u16, u16), to: (u16, u16)) -> bool {
        let (Some(start), Some(end)) = (self.grid_ref(from.0, from.1), self.grid_ref(to.0, to.1))
        else {
            return false;
        };
        self.set_selection(Some(&ffi::GhosttySelection {
            start,
            end,
            rectangle: false,
        }))
    }

    pub fn scrollbar(&self) -> Option<Scrollbar> {
        let mut bar = ffi::GhosttyTerminalScrollbar {
            total: 0,
            offset: 0,
            len: 0,
        };
        let result = unsafe {
            ffi::ghostty_terminal_get(
                self.terminal,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_SCROLLBAR,
                (&mut bar as *mut ffi::GhosttyTerminalScrollbar).cast::<c_void>(),
            )
        };
        (result == ffi::GhosttyResult_GHOSTTY_SUCCESS).then_some(Scrollbar {
            total: bar.total,
            offset: bar.offset,
            len: bar.len,
        })
    }

    /// A viewport cell as a point in the whole screen, scrollback included,
    /// which stays put as the viewport scrolls.
    pub fn screen_point(&self, x: u16, y: u16) -> (u16, u32) {
        let offset = self.scrollbar().map_or(0, |bar| bar.offset);
        (
            x,
            u32::try_from(offset)
                .unwrap_or(u32::MAX)
                .saturating_add(u32::from(y)),
        )
    }

    /// Selects between two screen points, from [`Vt::screen_point`].
    pub fn select_screen(&mut self, from: (u16, u32), to: (u16, u32)) -> bool {
        let tag = ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_SCREEN;
        let (Some(start), Some(end)) = (
            self.grid_ref_at(tag, from.0, from.1),
            self.grid_ref_at(tag, to.0, to.1),
        ) else {
            return false;
        };
        self.set_selection(Some(&ffi::GhosttySelection {
            start,
            end,
            rectangle: false,
        }))
    }

    /// Selects the word at a viewport cell, as a double click does.
    pub fn select_word(&mut self, x: u16, y: u16) -> bool {
        let Some(at) = self.grid_ref(x, y) else {
            return false;
        };
        let options = ffi::GhosttyTerminalSelectWordOptions {
            size: std::mem::size_of::<ffi::GhosttyTerminalSelectWordOptions>(),
            ref_: at,
            boundary_codepoints: ptr::null(),
            boundary_codepoints_len: 0,
        };
        let mut selection: ffi::GhosttySelection = unsafe { std::mem::zeroed() };
        let found =
            unsafe { ffi::ghostty_terminal_select_word(self.terminal, &options, &mut selection) };
        found == ffi::GhosttyResult_GHOSTTY_SUCCESS && self.set_selection(Some(&selection))
    }

    /// Selects the line at a viewport cell, as a triple click does.
    pub fn select_line(&mut self, x: u16, y: u16) -> bool {
        let Some(at) = self.grid_ref(x, y) else {
            return false;
        };
        let options = ffi::GhosttyTerminalSelectLineOptions {
            size: std::mem::size_of::<ffi::GhosttyTerminalSelectLineOptions>(),
            ref_: at,
            whitespace: ptr::null(),
            whitespace_len: 0,
            semantic_prompt_boundary: false,
        };
        let mut selection: ffi::GhosttySelection = unsafe { std::mem::zeroed() };
        let found =
            unsafe { ffi::ghostty_terminal_select_line(self.terminal, &options, &mut selection) };
        found == ffi::GhosttyResult_GHOSTTY_SUCCESS && self.set_selection(Some(&selection))
    }

    pub fn clear_selection(&mut self) {
        self.set_selection(None);
    }

    /// The selected text, one line per selected row, trailing blanks trimmed.
    pub fn selected_text(&mut self) -> String {
        let Ok(frame) = self.update() else {
            return String::new();
        };
        let mut lines = Vec::new();
        for y in 0..frame.rows {
            let mut line = String::new();
            let mut any = false;
            for x in 0..frame.cols {
                let cell = frame.cell(x, y);
                if !cell.selected {
                    continue;
                }
                any = true;
                if cell.text.is_empty() {
                    if x == 0 || !frame.cell(x - 1, y).wide {
                        line.push(' ');
                    }
                } else {
                    line.push_str(&cell.text);
                }
            }
            if any {
                lines.push(line.trim_end().to_string());
            }
        }
        lines.join("\n")
    }

    /// The link under a viewport cell: its OSC 8 hyperlink, or a link written
    /// out in the text.
    pub fn link_at(&mut self, x: u16, y: u16) -> Option<String> {
        if let Some(uri) = self.hyperlink_at(x, y) {
            return Some(uri);
        }
        let frame = self.update().ok()?;
        link_in_text(frame, x, y)
    }

    fn hyperlink_at(&self, x: u16, y: u16) -> Option<String> {
        let at = self.grid_ref(x, y)?;
        let mut buffer = vec![0u8; BUFFER];
        loop {
            let mut written = 0usize;
            let result = unsafe {
                ffi::ghostty_grid_ref_hyperlink_uri(
                    &at,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut written,
                )
            };
            if result == ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE && written > buffer.len() {
                buffer.resize(written, 0);
                continue;
            }
            if result != ffi::GhosttyResult_GHOSTTY_SUCCESS || written == 0 {
                return None;
            }
            buffer.truncate(written);
            return String::from_utf8(buffer).ok();
        }
    }

    /// Whether the program asked to hear the mouse instead of the terminal
    /// selecting text.
    pub fn mouse_tracking(&self) -> bool {
        let mut tracking = false;
        let result = unsafe {
            ffi::ghostty_terminal_get(
                self.terminal,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING,
                (&mut tracking as *mut bool).cast::<c_void>(),
            )
        };
        result == ffi::GhosttyResult_GHOSTTY_SUCCESS && tracking
    }

    /// The bytes reporting a mouse event to the program, in the format and
    /// mode it asked for, or nothing when it does not want this one.
    pub fn encode_mouse(
        &mut self,
        action: MouseAction,
        button: Option<MouseButton>,
        position: (f32, f32),
        mods: u16,
        geometry: MouseGeometry,
    ) -> Vec<u8> {
        unsafe {
            if self.mouse_encoder.is_null()
                && ffi::ghostty_mouse_encoder_new(ptr::null(), &mut self.mouse_encoder)
                    != ffi::GhosttyResult_GHOSTTY_SUCCESS
            {
                return Vec::new();
            }
            if self.mouse_event.is_null()
                && ffi::ghostty_mouse_event_new(ptr::null(), &mut self.mouse_event)
                    != ffi::GhosttyResult_GHOSTTY_SUCCESS
            {
                return Vec::new();
            }
            ffi::ghostty_mouse_encoder_set_from_terminal(self.mouse_encoder, self.terminal);
            let size = ffi::GhosttyMouseEncoderSize {
                size: std::mem::size_of::<ffi::GhosttyMouseEncoderSize>(),
                screen_width: geometry.width,
                screen_height: geometry.height,
                cell_width: geometry.cell_width.max(1),
                cell_height: geometry.cell_height.max(1),
                padding_top: geometry.padding,
                padding_bottom: geometry.padding,
                padding_right: geometry.padding,
                padding_left: geometry.padding,
            };
            ffi::ghostty_mouse_encoder_set(
                self.mouse_encoder,
                ffi::GhosttyMouseEncoderOption_GHOSTTY_MOUSE_ENCODER_OPT_SIZE,
                (&size as *const ffi::GhosttyMouseEncoderSize).cast::<c_void>(),
            );
            let action = match action {
                MouseAction::Press => ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS,
                MouseAction::Release => ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_RELEASE,
                MouseAction::Motion => ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_MOTION,
            };
            ffi::ghostty_mouse_event_set_action(self.mouse_event, action);
            match button {
                Some(button) => {
                    let button = match button {
                        MouseButton::Left => ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT,
                        MouseButton::Middle => ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_MIDDLE,
                        MouseButton::Right => ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_RIGHT,
                        MouseButton::WheelUp => ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FOUR,
                        MouseButton::WheelDown => ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FIVE,
                    };
                    ffi::ghostty_mouse_event_set_button(self.mouse_event, button);
                }
                None => {
                    ffi::ghostty_mouse_event_clear_button(self.mouse_event);
                }
            }
            ffi::ghostty_mouse_event_set_mods(self.mouse_event, mods);
            ffi::ghostty_mouse_event_set_position(
                self.mouse_event,
                ffi::GhosttyMousePosition {
                    x: position.0,
                    y: position.1,
                },
            );
            let mut out = vec![0u8; BUFFER];
            let mut written = 0usize;
            let result = ffi::ghostty_mouse_encoder_encode(
                self.mouse_encoder,
                self.mouse_event,
                out.as_mut_ptr(),
                out.len(),
                &mut written,
            );
            if result != ffi::GhosttyResult_GHOSTTY_SUCCESS {
                return Vec::new();
            }
            out.truncate(written);
            out
        }
    }
}

/// A link written out in a row's text that covers column `x`.
pub fn link_in_text(frame: &Frame, x: u16, y: u16) -> Option<String> {
    if y >= frame.rows || x >= frame.cols {
        return None;
    }
    // Each char of the row text, with the column it starts in.
    let mut chars: Vec<(char, u16)> = Vec::new();
    for col in 0..frame.cols {
        let cell = frame.cell(col, y);
        if cell.text.is_empty() {
            if col == 0 || !frame.cell(col - 1, y).wide {
                chars.push((' ', col));
            }
            continue;
        }
        for ch in cell.text.chars() {
            chars.push((ch, col));
        }
    }
    let text: String = chars.iter().map(|&(ch, _)| ch).collect();
    let mut search = 0;
    while search < text.len() {
        let rest = &text[search..];
        let Some((offset, _)) = LINK_PREFIXES
            .iter()
            .filter_map(|prefix| rest.find(prefix).map(|at| (at, *prefix)))
            .min()
        else {
            break;
        };
        let start = search + offset;
        let tail = &text[start..];
        let length = tail
            .find(|c: char| c.is_whitespace() || LINK_STOPS.contains(&c))
            .unwrap_or(tail.len());
        let link = tail[..length].trim_end_matches(LINK_TRAILERS);
        let start_char = text[..start].chars().count();
        let end_char = start_char + link.chars().count();
        if end_char > start_char {
            let first = chars[start_char].1;
            let last = chars[end_char - 1].1;
            if (first..=last).contains(&x) && !LINK_PREFIXES.contains(&link) {
                return Some(link.to_string());
            }
        }
        search = start + length.max(1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_selects_text_across_rows() {
        let mut vt = Vt::new(20, 3).unwrap();
        vt.write(b"hello world\r\nsecond row");
        assert!(vt.select_range((6, 0), (5, 1)));
        assert_eq!(vt.selected_text(), "world\nsecond");
        vt.clear_selection();
        assert_eq!(vt.selected_text(), "");
    }

    #[test]
    fn a_selection_anchored_in_the_screen_survives_scrolling() {
        let mut vt = Vt::new(10, 3).unwrap();
        for line in 0..10 {
            vt.write(format!("line{line}\r\n").as_bytes());
        }
        let bar = vt.scrollbar().unwrap();
        assert!(bar.at_bottom());
        let anchor = vt.screen_point(0, 0);
        assert!(vt.scroll(-2).is_ok());
        assert!(!vt.scrollbar().unwrap().at_bottom());
        let end = vt.screen_point(4, 0);
        assert!(vt.select_screen(end, anchor));
        assert_eq!(
            vt.selected_text(),
            "6\nline7\nl",
            "from line6's fifth column back down to line8's first"
        );
    }

    #[test]
    fn a_double_click_selects_the_word_and_a_triple_click_the_line() {
        let mut vt = Vt::new(30, 2).unwrap();
        vt.write(b"cargo test --release");
        assert!(vt.select_word(2, 0));
        assert_eq!(vt.selected_text(), "cargo");
        assert!(vt.select_line(2, 0));
        assert_eq!(vt.selected_text(), "cargo test --release");
    }

    #[test]
    fn links_written_in_text_are_found_without_trailing_punctuation() {
        let mut vt = Vt::new(60, 2).unwrap();
        vt.write(b"see https://example.com/a?b=1. or pm:item/1/42 now");
        assert_eq!(
            vt.link_at(8, 0).as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(vt.link_at(36, 0).as_deref(), Some("pm:item/1/42"));
        assert_eq!(vt.link_at(1, 0), None);
    }

    #[test]
    fn an_osc8_hyperlink_is_found_under_its_text() {
        let mut vt = Vt::new(40, 2).unwrap();
        vt.write(b"open \x1b]8;;https://ghostty.example/docs\x1b\\the docs\x1b]8;;\x1b\\ here");
        assert_eq!(
            vt.link_at(7, 0).as_deref(),
            Some("https://ghostty.example/docs")
        );
        assert_eq!(vt.link_at(1, 0), None);
    }

    #[test]
    fn the_mouse_goes_to_a_program_only_once_it_asks() {
        let mut vt = Vt::new(80, 24).unwrap();
        assert!(!vt.mouse_tracking());
        vt.write(b"\x1b[?1000h\x1b[?1006h");
        assert!(vt.mouse_tracking());
        let geometry = MouseGeometry {
            width: 800,
            height: 480,
            cell_width: 10,
            cell_height: 20,
            padding: 0,
        };
        let press = vt.encode_mouse(
            MouseAction::Press,
            Some(MouseButton::Left),
            (25.0, 45.0),
            0,
            geometry,
        );
        assert_eq!(press, b"\x1b[<0;3;3M");
    }
}
