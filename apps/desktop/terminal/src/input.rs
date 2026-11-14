//! Keys and pastes turned into the bytes a program expects, by libghostty's
//! encoder, which follows the terminal's own modes such as application
//! cursor keys and the kitty keyboard protocol.

use std::ffi::c_void;
use std::ptr;

use crate::ffi;
use crate::vt::Vt;

/// A physical key, as libghostty names it.
pub type Key = ffi::GhosttyKey;

/// Modifier bits for [`KeyPress::mods`].
pub mod mods {
    pub const SHIFT: u16 = super::ffi::GHOSTTY_MODS_SHIFT as u16;
    pub const CTRL: u16 = super::ffi::GHOSTTY_MODS_CTRL as u16;
    pub const ALT: u16 = super::ffi::GHOSTTY_MODS_ALT as u16;
    pub const SUPER: u16 = super::ffi::GHOSTTY_MODS_SUPER as u16;
}

/// Key names for a platform's key code table.
pub mod keys {
    macro_rules! keys {
        ($($name:ident => $ffi:ident),* $(,)?) => {
            $(pub const $name: super::Key = super::ffi::$ffi;)*
        };
    }
    keys! {
        A => GhosttyKey_GHOSTTY_KEY_A, B => GhosttyKey_GHOSTTY_KEY_B, C => GhosttyKey_GHOSTTY_KEY_C,
        D => GhosttyKey_GHOSTTY_KEY_D, E => GhosttyKey_GHOSTTY_KEY_E, F => GhosttyKey_GHOSTTY_KEY_F,
        G => GhosttyKey_GHOSTTY_KEY_G, H => GhosttyKey_GHOSTTY_KEY_H, I => GhosttyKey_GHOSTTY_KEY_I,
        J => GhosttyKey_GHOSTTY_KEY_J, K => GhosttyKey_GHOSTTY_KEY_K, L => GhosttyKey_GHOSTTY_KEY_L,
        M => GhosttyKey_GHOSTTY_KEY_M, N => GhosttyKey_GHOSTTY_KEY_N, O => GhosttyKey_GHOSTTY_KEY_O,
        P => GhosttyKey_GHOSTTY_KEY_P, Q => GhosttyKey_GHOSTTY_KEY_Q, R => GhosttyKey_GHOSTTY_KEY_R,
        S => GhosttyKey_GHOSTTY_KEY_S, T => GhosttyKey_GHOSTTY_KEY_T, U => GhosttyKey_GHOSTTY_KEY_U,
        V => GhosttyKey_GHOSTTY_KEY_V, W => GhosttyKey_GHOSTTY_KEY_W, X => GhosttyKey_GHOSTTY_KEY_X,
        Y => GhosttyKey_GHOSTTY_KEY_Y, Z => GhosttyKey_GHOSTTY_KEY_Z,
        DIGIT_0 => GhosttyKey_GHOSTTY_KEY_DIGIT_0, DIGIT_1 => GhosttyKey_GHOSTTY_KEY_DIGIT_1,
        DIGIT_2 => GhosttyKey_GHOSTTY_KEY_DIGIT_2, DIGIT_3 => GhosttyKey_GHOSTTY_KEY_DIGIT_3,
        DIGIT_4 => GhosttyKey_GHOSTTY_KEY_DIGIT_4, DIGIT_5 => GhosttyKey_GHOSTTY_KEY_DIGIT_5,
        DIGIT_6 => GhosttyKey_GHOSTTY_KEY_DIGIT_6, DIGIT_7 => GhosttyKey_GHOSTTY_KEY_DIGIT_7,
        DIGIT_8 => GhosttyKey_GHOSTTY_KEY_DIGIT_8, DIGIT_9 => GhosttyKey_GHOSTTY_KEY_DIGIT_9,
        BACKQUOTE => GhosttyKey_GHOSTTY_KEY_BACKQUOTE, BACKSLASH => GhosttyKey_GHOSTTY_KEY_BACKSLASH,
        BRACKET_LEFT => GhosttyKey_GHOSTTY_KEY_BRACKET_LEFT, BRACKET_RIGHT => GhosttyKey_GHOSTTY_KEY_BRACKET_RIGHT,
        COMMA => GhosttyKey_GHOSTTY_KEY_COMMA, EQUAL => GhosttyKey_GHOSTTY_KEY_EQUAL,
        MINUS => GhosttyKey_GHOSTTY_KEY_MINUS, PERIOD => GhosttyKey_GHOSTTY_KEY_PERIOD,
        QUOTE => GhosttyKey_GHOSTTY_KEY_QUOTE, SEMICOLON => GhosttyKey_GHOSTTY_KEY_SEMICOLON,
        SLASH => GhosttyKey_GHOSTTY_KEY_SLASH,
        BACKSPACE => GhosttyKey_GHOSTTY_KEY_BACKSPACE, ENTER => GhosttyKey_GHOSTTY_KEY_ENTER,
        SPACE => GhosttyKey_GHOSTTY_KEY_SPACE, TAB => GhosttyKey_GHOSTTY_KEY_TAB,
        ESCAPE => GhosttyKey_GHOSTTY_KEY_ESCAPE, DELETE => GhosttyKey_GHOSTTY_KEY_DELETE,
        HOME => GhosttyKey_GHOSTTY_KEY_HOME, END => GhosttyKey_GHOSTTY_KEY_END,
        PAGE_UP => GhosttyKey_GHOSTTY_KEY_PAGE_UP, PAGE_DOWN => GhosttyKey_GHOSTTY_KEY_PAGE_DOWN,
        ARROW_UP => GhosttyKey_GHOSTTY_KEY_ARROW_UP, ARROW_DOWN => GhosttyKey_GHOSTTY_KEY_ARROW_DOWN,
        ARROW_LEFT => GhosttyKey_GHOSTTY_KEY_ARROW_LEFT, ARROW_RIGHT => GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT,
        HELP => GhosttyKey_GHOSTTY_KEY_HELP, NUMPAD_ENTER => GhosttyKey_GHOSTTY_KEY_NUMPAD_ENTER,
        F1 => GhosttyKey_GHOSTTY_KEY_F1, F2 => GhosttyKey_GHOSTTY_KEY_F2, F3 => GhosttyKey_GHOSTTY_KEY_F3,
        F4 => GhosttyKey_GHOSTTY_KEY_F4, F5 => GhosttyKey_GHOSTTY_KEY_F5, F6 => GhosttyKey_GHOSTTY_KEY_F6,
        F7 => GhosttyKey_GHOSTTY_KEY_F7, F8 => GhosttyKey_GHOSTTY_KEY_F8, F9 => GhosttyKey_GHOSTTY_KEY_F9,
        F10 => GhosttyKey_GHOSTTY_KEY_F10, F11 => GhosttyKey_GHOSTTY_KEY_F11, F12 => GhosttyKey_GHOSTTY_KEY_F12,
        UNIDENTIFIED => GhosttyKey_GHOSTTY_KEY_UNIDENTIFIED,
    }
}

/// One key press, as the platform reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub key: Key,
    pub mods: u16,
    /// The text the key produced with its modifiers, empty for none.
    pub text: String,
    /// What the key produces with no modifiers, which the encoder needs for
    /// Ctrl and Alt combinations.
    pub unshifted: Option<char>,
    pub repeat: bool,
}

/// DEC private mode 2004.
const BRACKETED_PASTE: ffi::GhosttyMode = 2004;
/// Bytes a key's encoding is expected to fit in; a longer one is retried.
const KEY_BUFFER: usize = 64;

impl Vt {
    /// The bytes `press` sends to the program, or nothing for a key it does
    /// not encode.
    pub fn encode_key(&mut self, press: &KeyPress) -> Vec<u8> {
        unsafe {
            if self.key_encoder.is_null()
                && ffi::ghostty_key_encoder_new(ptr::null(), &mut self.key_encoder)
                    != ffi::GhosttyResult_GHOSTTY_SUCCESS
            {
                return Vec::new();
            }
            if self.key_event.is_null()
                && ffi::ghostty_key_event_new(ptr::null(), &mut self.key_event)
                    != ffi::GhosttyResult_GHOSTTY_SUCCESS
            {
                return Vec::new();
            }
            ffi::ghostty_key_encoder_set_from_terminal(self.key_encoder, self.terminal);
            let action = if press.repeat {
                ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_REPEAT
            } else {
                ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS
            };
            ffi::ghostty_key_event_set_action(self.key_event, action);
            ffi::ghostty_key_event_set_key(self.key_event, press.key);
            ffi::ghostty_key_event_set_mods(self.key_event, press.mods);
            ffi::ghostty_key_event_set_utf8(
                self.key_event,
                ffi::GhosttyString {
                    ptr: press.text.as_ptr(),
                    len: press.text.len(),
                },
            );
            ffi::ghostty_key_event_set_unshifted_codepoint(
                self.key_event,
                press.unshifted.map_or(0, u32::from),
            );
            let mut out = vec![0u8; KEY_BUFFER];
            loop {
                let mut written = 0usize;
                let result = ffi::ghostty_key_encoder_encode(
                    self.key_encoder,
                    self.key_event,
                    out.as_mut_ptr(),
                    out.len(),
                    &mut written,
                );
                if result == ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE && written > out.len() {
                    out.resize(written, 0);
                    continue;
                }
                if result != ffi::GhosttyResult_GHOSTTY_SUCCESS {
                    return Vec::new();
                }
                out.truncate(written);
                return out;
            }
        }
    }

    /// Pasted text as the program expects it, wrapped in bracketed-paste
    /// markers when the program asked for them.
    pub fn encode_paste(&self, text: &str) -> Vec<u8> {
        let mut config = ffi::GhosttyTerminalModeConfig {
            mode: BRACKETED_PASTE,
            value: false,
        };
        let bracketed = unsafe {
            ffi::ghostty_terminal_get(
                self.terminal,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_MODE,
                (&mut config as *mut ffi::GhosttyTerminalModeConfig).cast::<c_void>(),
            ) == ffi::GhosttyResult_GHOSTTY_SUCCESS
                && config.value
        };
        let mut data = text.as_bytes().to_vec();
        let mut out = vec![0u8; data.len() + KEY_BUFFER];
        loop {
            let mut written = 0usize;
            let result = unsafe {
                ffi::ghostty_paste_encode(
                    data.as_mut_ptr().cast(),
                    data.len(),
                    bracketed,
                    out.as_mut_ptr(),
                    out.len(),
                    &mut written,
                )
            };
            if result == ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE && written > out.len() {
                out.resize(written, 0);
                continue;
            }
            if result != ffi::GhosttyResult_GHOSTTY_SUCCESS {
                return text.as_bytes().to_vec();
            }
            out.truncate(written);
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, mods: u16, text: &str, unshifted: Option<char>) -> KeyPress {
        KeyPress {
            key,
            mods,
            text: text.into(),
            unshifted,
            repeat: false,
        }
    }

    #[test]
    fn printable_keys_send_their_text_and_control_keys_their_codes() {
        let mut vt = Vt::new(80, 24).unwrap();
        assert_eq!(vt.encode_key(&press(keys::A, 0, "a", Some('a'))), b"a");
        assert_eq!(
            vt.encode_key(&press(keys::A, mods::SHIFT, "A", Some('a'))),
            b"A"
        );
        assert_eq!(
            vt.encode_key(&press(keys::C, mods::CTRL, "", Some('c'))),
            b"\x03"
        );
        assert_eq!(vt.encode_key(&press(keys::ENTER, 0, "\r", None)), b"\r");
        assert_eq!(vt.encode_key(&press(keys::BACKSPACE, 0, "", None)), b"\x7f");
    }

    #[test]
    fn cursor_keys_follow_application_cursor_mode() {
        let mut vt = Vt::new(80, 24).unwrap();
        assert_eq!(
            vt.encode_key(&press(keys::ARROW_UP, 0, "", None)),
            b"\x1b[A"
        );
        vt.write(b"\x1b[?1h");
        assert_eq!(
            vt.encode_key(&press(keys::ARROW_UP, 0, "", None)),
            b"\x1bOA"
        );
    }

    #[test]
    fn a_paste_is_bracketed_only_when_the_program_asked() {
        let mut vt = Vt::new(80, 24).unwrap();
        assert_eq!(vt.encode_paste("hi"), b"hi");
        vt.write(b"\x1b[?2004h");
        assert_eq!(vt.encode_paste("hi"), b"\x1b[200~hi\x1b[201~");
    }
}
