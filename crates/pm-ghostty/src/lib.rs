use std::cell::Cell;
use std::ffi::{c_int, c_void};
use std::fmt;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};

const SUCCESS: c_int = 0;
const DATA_COLS: c_int = 1;
const DATA_ROWS: c_int = 2;
const DATA_CURSOR_X: c_int = 3;
const DATA_CURSOR_Y: c_int = 4;
const DATA_ACTIVE_SCREEN: c_int = 6;
const DATA_SCROLLBACK_ROWS: c_int = 15;
const SCREEN_ALTERNATE: c_int = 1;
const MODE_ANY_MOUSE: u16 = 1003;

unsafe extern "C" {
    fn ghostty_terminal_new(
        allocator: *const c_void,
        terminal: *mut *mut c_void,
        cols: u16,
        rows: u16,
    ) -> c_int;
    fn ghostty_terminal_free(terminal: *mut c_void);
    fn ghostty_terminal_resize(
        terminal: *mut c_void,
        cols: u16,
        rows: u16,
        cell_width: u32,
        cell_height: u32,
    ) -> c_int;
    fn ghostty_terminal_vt_write(terminal: *mut c_void, bytes: *const u8, len: usize);
    fn ghostty_terminal_get(terminal: *mut c_void, kind: c_int, out: *mut c_void) -> c_int;
    fn ghostty_snapshot_encode_alloc(
        terminal: *mut c_void,
        allocator: *const c_void,
        out: *mut *mut u8,
        len: *mut usize,
    ) -> c_int;
    fn pm_ghostty_configure(terminal: *mut c_void) -> c_int;
    fn pm_ghostty_restore(bytes: *const u8, len: usize, out: *mut *mut c_void) -> c_int;
    fn pm_ghostty_format(
        terminal: *mut c_void,
        vt: bool,
        out: *mut *mut u8,
        len: *mut usize,
    ) -> c_int;
    fn pm_ghostty_row(
        terminal: *mut c_void,
        history: bool,
        y: u32,
        out: *mut *mut u8,
        len: *mut usize,
    ) -> c_int;
    fn pm_ghostty_mode(terminal: *mut c_void, mode: u16, ansi: bool, out: *mut bool) -> c_int;
    fn pm_ghostty_free(bytes: *mut u8, len: usize);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    operation: &'static str,
    code: c_int,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Ghostty {} failed ({})",
            self.operation, self.code
        )
    }
}

impl std::error::Error for Error {}

fn check(operation: &'static str, code: c_int) -> Result<(), Error> {
    if code == SUCCESS {
        Ok(())
    } else {
        Err(Error { operation, code })
    }
}

pub struct Terminal {
    pointer: NonNull<c_void>,
    not_sync: PhantomData<Cell<()>>,
}

// A terminal has no thread affinity, but its API calls must remain serialized.
unsafe impl Send for Terminal {}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        check("create", unsafe {
            ghostty_terminal_new(ptr::null(), &mut raw, cols.max(1), rows.max(1))
        })?;
        let terminal = Self::from_raw(raw)?;
        check("configure", unsafe { pm_ghostty_configure(terminal.raw()) })?;
        Ok(terminal)
    }

    fn from_raw(raw: *mut c_void) -> Result<Self, Error> {
        Ok(Self {
            pointer: NonNull::new(raw).ok_or(Error {
                operation: "null terminal",
                code: SUCCESS,
            })?,
            not_sync: PhantomData,
        })
    }

    fn raw(&self) -> *mut c_void {
        self.pointer.as_ptr()
    }

    pub fn restore(bytes: &[u8]) -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        check("restore", unsafe {
            pm_ghostty_restore(bytes.as_ptr(), bytes.len(), &mut raw)
        })?;
        let terminal = Self::from_raw(raw)?;
        check("configure", unsafe { pm_ghostty_configure(terminal.raw()) })?;
        Ok(terminal)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        unsafe { ghostty_terminal_vt_write(self.raw(), bytes.as_ptr(), bytes.len()) }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        check("resize", unsafe {
            ghostty_terminal_resize(self.raw(), cols.max(1), rows.max(1), 0, 0)
        })
    }

    pub fn snapshot(&self) -> Result<Vec<u8>, Error> {
        self.output("snapshot", |out, len| unsafe {
            ghostty_snapshot_encode_alloc(self.raw(), ptr::null(), out, len)
        })
    }

    pub fn snapshot_vt(&self) -> Result<Vec<u8>, Error> {
        self.output("VT snapshot", |out, len| unsafe {
            pm_ghostty_format(self.raw(), true, out, len)
        })
    }

    pub fn plain_text(&self) -> Result<String, Error> {
        self.output("plain text", |out, len| unsafe {
            pm_ghostty_format(self.raw(), false, out, len)
        })
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    fn output(
        &self,
        operation: &'static str,
        call: impl FnOnce(*mut *mut u8, *mut usize) -> c_int,
    ) -> Result<Vec<u8>, Error> {
        let mut raw = ptr::null_mut();
        let mut len = 0;
        check(operation, call(&mut raw, &mut len))?;
        let bytes = if len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(raw, len) }.to_vec()
        };
        unsafe { pm_ghostty_free(raw, len) };
        Ok(bytes)
    }

    fn get<T: Default>(&self, kind: c_int) -> Result<T, Error> {
        let mut value = T::default();
        check("query", unsafe {
            ghostty_terminal_get(self.raw(), kind, (&mut value as *mut T).cast())
        })?;
        Ok(value)
    }

    pub fn dimensions(&self) -> Result<(u16, u16), Error> {
        Ok((self.get(DATA_COLS)?, self.get(DATA_ROWS)?))
    }

    pub fn cursor_position(&self) -> Result<(u16, u16), Error> {
        Ok((self.get(DATA_CURSOR_X)?, self.get(DATA_CURSOR_Y)?))
    }

    pub fn is_alternate(&self) -> Result<bool, Error> {
        Ok(self.get::<c_int>(DATA_ACTIVE_SCREEN)? == SCREEN_ALTERNATE)
    }

    pub fn mouse_tracking(&self) -> Result<bool, Error> {
        let mut value = false;
        check("mouse mode", unsafe {
            pm_ghostty_mode(self.raw(), MODE_ANY_MOUSE, false, &mut value)
        })?;
        Ok(value)
    }

    pub fn visible_text(&self) -> Result<Vec<String>, Error> {
        self.rows(false, usize::from(self.get::<u16>(DATA_ROWS)?))
    }

    pub fn scrollback_text(&self) -> Result<Vec<String>, Error> {
        self.rows(true, self.get(DATA_SCROLLBACK_ROWS)?)
    }

    fn rows(&self, history: bool, count: usize) -> Result<Vec<String>, Error> {
        (0..count)
            .map(|y| {
                self.output("row text", |out, len| unsafe {
                    pm_ghostty_row(self.raw(), history, y as u32, out, len)
                })
                .map(|bytes| String::from_utf8_lossy(&bytes).trim_end().to_owned())
            })
            .collect()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { ghostty_terminal_free(self.raw()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(bytes: &[u8]) -> Terminal {
        let mut terminal = Terminal::new(40, 8).unwrap();
        terminal.write(bytes);
        terminal
    }

    #[test]
    fn native_fixture_round_trip_and_live_continuation() {
        let fixture = include_bytes!("../testdata/native-state.snapshot");
        let mut original = Terminal::new(40, 8).unwrap();
        original.write(b"primary state\x1b[?1049h\x1b[H\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[38;2;120;130;140m\x1b[48;2;10;20;30m\x1b[1;3;4malternate state\x1b[3;5H\x1b[38;2;9;8");
        assert_eq!(original.snapshot().unwrap(), fixture);
        let mut restored = Terminal::restore(fixture).unwrap();
        assert_eq!(restored.snapshot().unwrap(), fixture);
        assert_eq!(restored.dimensions().unwrap(), (40, 8));
        assert_eq!(restored.cursor_position().unwrap(), (4, 2));
        assert_eq!(restored.visible_text().unwrap()[0], "alternate state");
        restored.write(b";7mcontinued");
        assert_eq!(restored.visible_text().unwrap()[2], "    continued");
        restored.write(b"\x1b[?1049l");
        assert_eq!(restored.visible_text().unwrap()[0], "primary state");
    }

    #[test]
    fn binary_snapshot_preserves_both_screens_and_modes() {
        let original = terminal(b"primary\x1b[?1049h\x1b[?1003h\x1b[3;5Halternate");
        let mut restored = Terminal::restore(&original.snapshot().unwrap()).unwrap();
        assert!(restored.is_alternate().unwrap());
        assert!(restored.mouse_tracking().unwrap());
        assert_eq!(
            restored.visible_text().unwrap(),
            original.visible_text().unwrap()
        );
        assert_eq!(
            restored.cursor_position().unwrap(),
            original.cursor_position().unwrap()
        );
        restored.write(b"\x1b[?1049l");
        assert!(restored.visible_text().unwrap()[0].starts_with("primary"));
    }

    #[test]
    fn binary_snapshot_resumes_unfinished_utf8_and_escape_sequences() {
        for (prefix, suffix) in [
            (&b"\x1b[38;2;10;20"[..], &b";30mstyled"[..]),
            (&b"\xe2\x94"[..], &b"\x80"[..]),
        ] {
            let mut original = terminal(prefix);
            let mut restored = Terminal::restore(&original.snapshot().unwrap()).unwrap();
            original.write(suffix);
            restored.write(suffix);
            assert_eq!(
                restored.snapshot_vt().unwrap(),
                original.snapshot_vt().unwrap()
            );
            assert_eq!(
                restored.visible_text().unwrap(),
                original.visible_text().unwrap()
            );
            Terminal::restore(&restored.snapshot().unwrap()).unwrap();
        }
    }

    #[test]
    fn rejects_corrupt_truncated_and_trailing_snapshot_data() {
        let bytes = terminal(b"keep me").snapshot().unwrap();
        assert!(Terminal::restore(&bytes[..bytes.len() - 1]).is_err());
        let mut corrupt = bytes.clone();
        let at = corrupt.len() / 2;
        corrupt[at] ^= 1;
        assert!(Terminal::restore(&corrupt).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(Terminal::restore(&trailing).is_err());
    }

    #[test]
    fn vt_snapshot_restores_live_screen_styles_cursor_and_modes() {
        let original = terminal(b"\x1b[?1049h\x1b[?1003h\x1b[4;3H\x1b[38;2;10;20;30mstyled");
        let mut restored = Terminal::new(40, 8).unwrap();
        restored.write(&original.snapshot_vt().unwrap());
        assert_eq!(
            restored.visible_text().unwrap(),
            original.visible_text().unwrap()
        );
        assert_eq!(
            restored.cursor_position().unwrap(),
            original.cursor_position().unwrap()
        );
        assert!(restored.is_alternate().unwrap());
        assert!(restored.mouse_tracking().unwrap());
    }

    #[test]
    fn resize_after_restore_matches_uninterrupted_terminal() {
        let mut original =
            terminal(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\r\nnine");
        let mut restored = Terminal::restore(&original.snapshot().unwrap()).unwrap();
        original.resize(23, 4).unwrap();
        restored.resize(23, 4).unwrap();
        original.write(b"\r\nafter resize");
        restored.write(b"\r\nafter resize");
        assert_eq!(
            restored.visible_text().unwrap(),
            original.visible_text().unwrap()
        );
        assert_eq!(
            restored.scrollback_text().unwrap(),
            original.scrollback_text().unwrap()
        );
    }
}
