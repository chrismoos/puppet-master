use pm_ghostty::{Error, Terminal};

pub(crate) struct Ring {
    buf: Vec<u8>,
    cap: usize,
}

impl Ring {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
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

pub(crate) struct Retained {
    pub(crate) ring: Ring,
    pub(crate) model: TerminalModel,
    cols: u16,
    rows: u16,
}

impl Retained {
    pub(crate) fn new(cap: usize, cols: u16, rows: u16) -> Self {
        Self {
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
        self.model.resize(cols, rows);
        self.cols = cols;
        self.rows = rows;
    }

    pub(crate) fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    pub(crate) fn try_reset_from_replay(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut model = TerminalModel::restore(data)?;
        model.term.resize(self.cols, self.rows)?;
        self.model = model;
        self.ring.reset(&[]);
        Ok(())
    }
}

pub struct TerminalModel {
    term: Terminal,
}

impl TerminalModel {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            term: Terminal::new(cols, rows).expect("initialize retained terminal"),
        }
    }

    pub fn restore(bytes: &[u8]) -> Result<Self, Error> {
        Ok(Self {
            term: Terminal::restore(bytes)?,
        })
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.term.write(bytes);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if let Err(error) = self.term.resize(cols, rows) {
            tracing::error!(%error, cols, rows, "failed to resize retained terminal");
        }
    }

    pub fn take_rewritten(&mut self) -> bool {
        false
    }

    pub fn try_snapshot(&self) -> Result<Vec<u8>, Error> {
        self.term.snapshot()
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.try_snapshot().expect("encode terminal snapshot")
    }

    pub fn try_snapshot_vt(&self) -> Result<Vec<u8>, Error> {
        self.term.snapshot_vt()
    }

    pub fn snapshot_vt(&self) -> Vec<u8> {
        self.try_snapshot_vt().expect("encode VT terminal snapshot")
    }

    pub fn snapshot_with_history(&self, _lines: usize) -> Vec<u8> {
        self.snapshot()
    }

    #[cfg(test)]
    pub fn visible_text(&self) -> Vec<String> {
        self.term.visible_text().expect("read screen")
    }

    #[cfg(test)]
    pub fn scrollback_text(&self) -> Vec<String> {
        self.term.scrollback_text().expect("read history")
    }

    #[cfg(test)]
    pub fn is_alternate(&self) -> bool {
        self.term.is_alternate().expect("read active screen")
    }

    #[cfg(test)]
    pub fn mouse_tracking(&self) -> bool {
        self.term.mouse_tracking().expect("read mouse mode")
    }

    #[cfg(test)]
    pub fn cursor_position(&self) -> (i32, usize) {
        let (x, y) = self.term.cursor_position().expect("read cursor");
        (i32::from(y), usize::from(x))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_restores_state_without_recording_binary_as_output() {
        let mut worker = Retained::new(128, 40, 8);
        worker.push(b"primary\x1b[?1049h\x1b[?1003h\x1b[3;5Halternate");
        let mut controller = Retained::new(128, 40, 8);
        controller
            .try_reset_from_replay(&worker.model.snapshot())
            .unwrap();
        controller.push(b" live");
        worker.push(b" live");
        assert_eq!(controller.ring.snapshot(), &b" live"[..]);
        assert_eq!(worker.model.visible_text(), controller.model.visible_text());
        assert_eq!(
            worker.model.cursor_position(),
            controller.model.cursor_position()
        );
        assert!(controller.model.is_alternate());
        assert!(controller.model.mouse_tracking());
    }

    #[test]
    fn invalid_replay_keeps_previous_terminal_and_raw_output() {
        let mut retained = Retained::new(128, 40, 8);
        retained.push(b"keep this");
        assert!(retained.try_reset_from_replay(b"not a snapshot").is_err());
        assert_eq!(retained.ring.snapshot(), &b"keep this"[..]);
        assert_eq!(retained.model.visible_text()[0], "keep this");
    }

    #[test]
    fn binary_snapshot_preserves_state_after_raw_ring_wraps() {
        let mut retained = Retained::new(32, 40, 8);
        retained.push(b"\x1b[?1049h\x1b[?1003h");
        retained.push(&[b'.'; 128]);
        retained.push(b"\x1b[3;5Hlast frame");
        let restored = TerminalModel::restore(&retained.model.snapshot()).unwrap();
        assert!(restored.is_alternate());
        assert!(restored.mouse_tracking());
        assert_eq!(restored.visible_text(), retained.model.visible_text());
    }

    #[test]
    fn snapshot_during_partial_sequence_resumes_with_live_output() {
        let mut retained = Retained::new(128, 40, 8);
        retained.push(b"before\x1b[38;2;10;20");
        let mut restored = TerminalModel::restore(&retained.model.snapshot()).unwrap();
        let live = b";30mafter";
        retained.push(live);
        restored.advance(live);
        assert_eq!(restored.snapshot_vt(), retained.model.snapshot_vt());
    }

    #[test]
    fn binary_snapshot_retains_history_and_screen() {
        let mut model = TerminalModel::new(40, 8);
        for line in 0..100 {
            model.advance(format!("line-{line}\r\n").as_bytes());
        }
        let restored = TerminalModel::restore(&model.snapshot()).unwrap();
        assert_eq!(restored.visible_text(), model.visible_text());
        assert_eq!(restored.scrollback_text(), model.scrollback_text());
        assert!(restored.scrollback_text().len() > 80);
    }

    #[test]
    fn vt_snapshot_restores_legacy_viewer_state() {
        let mut original = TerminalModel::new(40, 8);
        original.advance(b"\x1b[?1049h\x1b[?1003h\x1b[3;5Hscreen");
        let mut restored = TerminalModel::new(40, 8);
        restored.advance(&original.snapshot_vt());
        assert_eq!(restored.visible_text(), original.visible_text());
        assert_eq!(restored.cursor_position(), original.cursor_position());
        assert!(restored.is_alternate());
        assert!(restored.mouse_tracking());
    }
}
