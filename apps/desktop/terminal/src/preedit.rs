//! Text an input method is still composing, drawn at the cursor before it
//! is committed and sent.

use unicode_width::UnicodeWidthChar;

use crate::vt::{style, Frame};

/// The single underline that marks composing text.
const COMPOSING: u16 = 1 << style::UNDERLINE_SHIFT;

/// `frame` with `text` written over the cells from the cursor, underlined,
/// and the cursor moved to its end. Text past the row's end is cut off.
pub fn overlay(frame: &Frame, text: &str) -> Frame {
    let mut patched = frame.clone();
    let Some(cursor) = patched.cursor.as_mut() else {
        return patched;
    };
    let row = usize::from(cursor.y);
    let cols = usize::from(patched.cols);
    let mut x = usize::from(cursor.x);
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            continue;
        }
        if x + width > cols {
            break;
        }
        let at = row * cols + x;
        let cell = &mut patched.cells[at];
        cell.text = ch.to_string();
        cell.fg = frame.foreground;
        cell.has_bg = false;
        cell.flags = COMPOSING;
        cell.selected = false;
        cell.wide = width == 2;
        if width == 2 {
            let tail = &mut patched.cells[at + 1];
            tail.text.clear();
            tail.has_bg = false;
            tail.flags = COMPOSING;
            tail.wide = false;
        }
        x += width;
    }
    cursor.x = u16::try_from(x.min(cols.saturating_sub(1))).unwrap_or(cursor.x);
    patched.dirty_rows = (0..patched.rows).collect();
    patched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vt::Vt;

    #[test]
    fn composing_text_is_drawn_underlined_at_the_cursor() {
        let mut vt = Vt::new(10, 2).unwrap();
        vt.write(b"$ ");
        let frame = vt.update().unwrap().clone();
        let patched = overlay(&frame, "日本");
        assert_eq!(patched.row_text(0), "$ 日本");
        assert!(patched.cell(2, 0).wide);
        assert_eq!(patched.cell(2, 0).flags, COMPOSING);
        assert_eq!(patched.cursor.unwrap().x, 6);
        assert_eq!(frame.row_text(0), "$", "the screen itself is untouched");
    }

    #[test]
    fn composing_text_stops_at_the_row_end() {
        let mut vt = Vt::new(4, 1).unwrap();
        vt.write(b"ab");
        let frame = vt.update().unwrap().clone();
        assert_eq!(overlay(&frame, "xyz").row_text(0), "abxy");
    }
}
