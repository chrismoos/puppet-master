//! The colors a terminal draws with when a program sets none: the page's
//! terminal theme, applied to the native screen.

use std::ffi::c_void;

use crate::ffi;
use crate::vt::{Rgb, Vt};

/// The 256-color palette libghostty keeps.
const PALETTE_SIZE: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// The first colors of the palette, from the 16 ANSI colors on; the rest
    /// keep libghostty's defaults.
    pub palette: Vec<Rgb>,
}

fn ghostty(color: Rgb) -> ffi::GhosttyColorRgb {
    ffi::GhosttyColorRgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

impl Vt {
    /// Draws the default colors and palette from `theme` until a program
    /// sets its own.
    pub fn set_theme(&mut self, theme: &Theme) {
        for (option, color) in [
            (
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND,
                theme.foreground,
            ),
            (
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND,
                theme.background,
            ),
            (
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_COLOR_CURSOR,
                theme.cursor,
            ),
        ] {
            let value = ghostty(color);
            unsafe {
                ffi::ghostty_terminal_set(
                    self.terminal,
                    option,
                    (&value as *const ffi::GhosttyColorRgb).cast::<c_void>(),
                )
            };
        }
        let mut palette = [ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 }; PALETTE_SIZE];
        unsafe {
            ffi::ghostty_terminal_get(
                self.terminal,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT,
                palette.as_mut_ptr().cast::<c_void>(),
            )
        };
        for (slot, color) in palette.iter_mut().zip(&theme.palette) {
            *slot = ghostty(*color);
        }
        unsafe {
            ffi::ghostty_terminal_set(
                self.terminal,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_COLOR_PALETTE,
                palette.as_ptr().cast::<c_void>(),
            )
        };
        self.mark_all_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: Rgb = Rgb {
        r: 10,
        g: 20,
        b: 30,
    };
    const PAPER: Rgb = Rgb {
        r: 250,
        g: 245,
        b: 240,
    };
    const RED: Rgb = Rgb {
        r: 200,
        g: 40,
        b: 40,
    };

    fn light() -> Theme {
        Theme {
            foreground: INK,
            background: PAPER,
            cursor: INK,
            palette: vec![Rgb { r: 0, g: 0, b: 0 }, RED],
        }
    }

    #[test]
    fn default_colors_and_the_palette_come_from_the_theme() {
        let mut vt = Vt::new(10, 2).unwrap();
        vt.write(b"a\x1b[31mb");
        let _ = vt.update().unwrap();
        vt.set_theme(&light());
        let frame = vt.update().unwrap();
        assert_eq!(frame.foreground, INK);
        assert_eq!(frame.background, PAPER);
        assert_eq!(frame.cell(0, 0).fg, INK);
        assert_eq!(
            frame.cell(1, 0).fg,
            RED,
            "ANSI red comes from the theme's palette"
        );
    }

    #[test]
    fn a_program_color_still_wins_over_the_theme() {
        let mut vt = Vt::new(10, 2).unwrap();
        vt.set_theme(&light());
        vt.write(b"\x1b[38;2;1;2;3mx");
        assert_eq!(vt.update().unwrap().cell(0, 0).fg, Rgb { r: 1, g: 2, b: 3 });
    }
}
