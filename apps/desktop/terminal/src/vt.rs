//! The terminal's screen state, held by libghostty-vt, and the frames of
//! cells a renderer draws from it.

use std::ffi::c_void;
use std::fmt;
use std::ptr;

use crate::ffi;

/// Codepoints a single cell's grapheme is expected to fit in; a longer one
/// is read again into a buffer of its exact size.
const GRAPHEME_CAPACITY: usize = 16;

#[derive(Debug)]
pub struct Error {
    call: &'static str,
    code: ffi::GhosttyResult,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} failed with libghostty result {}",
            self.call, self.code
        )
    }
}

impl std::error::Error for Error {}

fn check(call: &'static str, code: ffi::GhosttyResult) -> Result<(), Error> {
    if code == ffi::GhosttyResult_GHOSTTY_SUCCESS {
        Ok(())
    } else {
        Err(Error { call, code })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<ffi::GhosttyColorRgb> for Rgb {
    fn from(color: ffi::GhosttyColorRgb) -> Self {
        Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        }
    }
}

/// libghostty's style flag bits, as `ghostty/vt/style.h` defines them.
pub mod style {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    pub const FAINT: u16 = 1 << 2;
    pub const BLINK: u16 = 1 << 3;
    pub const INVERSE: u16 = 1 << 4;
    pub const INVISIBLE: u16 = 1 << 5;
    pub const STRIKETHROUGH: u16 = 1 << 6;
    pub const OVERLINE: u16 = 1 << 7;
    pub const UNDERLINE_SHIFT: u16 = 8;
    pub const UNDERLINE_MASK: u16 = 0b111 << UNDERLINE_SHIFT;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// The grapheme cluster, empty for a blank cell or a wide character's tail.
    pub text: String,
    pub fg: Rgb,
    pub bg: Rgb,
    /// Whether the cell sets its own background rather than the default.
    pub has_bg: bool,
    pub flags: u16,
    pub selected: bool,
    /// The left half of a two-column character, whose glyph covers the next cell too.
    pub wide: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    HollowBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub blinking: bool,
}

/// What a renderer draws: the visible grid, row-major, and which rows
/// changed since the frame before.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor_color: Rgb,
    pub cursor: Option<Cursor>,
    pub dirty_rows: Vec<u16>,
}

impl Frame {
    pub fn cell(&self, x: u16, y: u16) -> &Cell {
        &self.cells[usize::from(y) * usize::from(self.cols) + usize::from(x)]
    }

    /// One row's text, trailing blanks trimmed, for tests and accessibility.
    pub fn row_text(&self, y: u16) -> String {
        let start = usize::from(y) * usize::from(self.cols);
        let row = &self.cells[start..start + usize::from(self.cols)];
        let text: String = row
            .iter()
            .map(|cell| {
                if cell.text.is_empty() && !cell.wide {
                    " "
                } else {
                    &cell.text
                }
            })
            .collect();
        text.trim_end().to_string()
    }
}

/// A terminal screen fed from a byte stream, with the render state that
/// turns it into frames.
pub struct Vt {
    pub(crate) terminal: ffi::GhosttyTerminal,
    pub(crate) key_encoder: ffi::GhosttyKeyEncoder,
    pub(crate) key_event: ffi::GhosttyKeyEvent,
    pub(crate) mouse_encoder: ffi::GhosttyMouseEncoder,
    pub(crate) mouse_event: ffi::GhosttyMouseEvent,
    state: ffi::GhosttyRenderState,
    rows: ffi::GhosttyRenderStateRowIterator,
    cells: ffi::GhosttyRenderStateRowCells,
    frame: Frame,
    graphemes: Vec<u32>,
}

// The handles are owned by this value and used from one thread at a time.
unsafe impl Send for Vt {}

impl Vt {
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        let mut terminal: ffi::GhosttyTerminal = ptr::null_mut();
        check("ghostty_terminal_new", unsafe {
            ffi::ghostty_terminal_new(ptr::null(), &mut terminal, cols, rows)
        })?;
        Self::around(terminal)
    }

    /// A terminal restored from a snapshot a worker encoded, the same format
    /// the browser restores.
    pub fn restore(snapshot: &[u8]) -> Result<Self, Error> {
        let mut decoder: ffi::GhosttySnapshotDecoder = ptr::null_mut();
        check("ghostty_snapshot_decoder_new_buf", unsafe {
            ffi::ghostty_snapshot_decoder_new_buf(
                ptr::null(),
                &mut decoder,
                snapshot.as_ptr(),
                snapshot.len(),
            )
        })?;
        let mut terminal: ffi::GhosttyTerminal = ptr::null_mut();
        let decoded = check("ghostty_snapshot_decoder_decode", unsafe {
            ffi::ghostty_snapshot_decoder_decode(decoder, &mut terminal)
        });
        unsafe { ffi::ghostty_snapshot_decoder_free(decoder) };
        decoded?;
        Self::around(terminal)
    }

    fn around(terminal: ffi::GhosttyTerminal) -> Result<Self, Error> {
        let mut vt = Vt {
            terminal,
            key_encoder: ptr::null_mut(),
            key_event: ptr::null_mut(),
            mouse_encoder: ptr::null_mut(),
            mouse_event: ptr::null_mut(),
            state: ptr::null_mut(),
            rows: ptr::null_mut(),
            cells: ptr::null_mut(),
            frame: Frame::default(),
            graphemes: vec![0; GRAPHEME_CAPACITY],
        };
        check("ghostty_render_state_new", unsafe {
            ffi::ghostty_render_state_new(ptr::null(), &mut vt.state)
        })?;
        check("ghostty_render_state_row_iterator_new", unsafe {
            ffi::ghostty_render_state_row_iterator_new(ptr::null(), &mut vt.rows)
        })?;
        check("ghostty_render_state_row_cells_new", unsafe {
            ffi::ghostty_render_state_row_cells_new(ptr::null(), &mut vt.cells)
        })?;
        Ok(vt)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        unsafe { ffi::ghostty_terminal_vt_write(self.terminal, bytes.as_ptr(), bytes.len()) };
    }

    /// Resizes the grid. The cell size in pixels answers programs that ask
    /// for the window's pixel dimensions.
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Result<(), Error> {
        check("ghostty_terminal_resize", unsafe {
            ffi::ghostty_terminal_resize(self.terminal, cols, rows, cell_width_px, cell_height_px)
        })
    }

    /// Scrolls the viewport through scrollback by `rows`, negative for up.
    pub fn scroll(&mut self, rows: isize) -> Result<(), Error> {
        let behavior = ffi::GhosttyTerminalScrollViewport {
            tag: ffi::GhosttyTerminalScrollViewportTag_GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA,
            value: ffi::GhosttyTerminalScrollViewportValue { delta: rows },
        };
        check("ghostty_terminal_scroll_viewport", unsafe {
            ffi::ghostty_terminal_scroll_viewport(self.terminal, behavior)
        })
    }

    /// Brings the frame up to date with the terminal, reading only the rows
    /// that changed, and returns it.
    pub fn update(&mut self) -> Result<&Frame, Error> {
        check("ghostty_render_state_update", unsafe {
            ffi::ghostty_render_state_update(self.state, self.terminal)
        })?;
        let cols: u16 = self.get(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_COLS)?;
        let rows: u16 = self.get(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_ROWS)?;
        let dirty: ffi::GhosttyRenderStateDirty =
            self.get(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_DIRTY)?;

        let mut colors: ffi::GhosttyRenderStateColors = unsafe { std::mem::zeroed() };
        colors.size = std::mem::size_of::<ffi::GhosttyRenderStateColors>();
        check("ghostty_render_state_get(colors)", unsafe {
            ffi::ghostty_render_state_get(
                self.state,
                ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_COLORS,
                (&mut colors as *mut ffi::GhosttyRenderStateColors).cast::<c_void>(),
            )
        })?;
        let foreground = Rgb::from(colors.foreground);
        let background = Rgb::from(colors.background);

        let resized = self.frame.cols != cols || self.frame.rows != rows;
        if resized {
            self.frame.cols = cols;
            self.frame.rows = rows;
            self.frame.cells = vec![Cell::default(); usize::from(cols) * usize::from(rows)];
        }
        self.frame.foreground = foreground;
        self.frame.background = background;
        self.frame.cursor_color = if colors.cursor_has_value {
            Rgb::from(colors.cursor)
        } else {
            foreground
        };
        self.frame.dirty_rows.clear();

        let full = resized || dirty == ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_FULL;
        if full || dirty != ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_FALSE {
            self.read_rows(full, foreground, background)?;
            let clean = ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_FALSE;
            check("ghostty_render_state_set(dirty)", unsafe {
                ffi::ghostty_render_state_set(
                    self.state,
                    ffi::GhosttyRenderStateOption_GHOSTTY_RENDER_STATE_OPT_DIRTY,
                    (&clean as *const ffi::GhosttyRenderStateDirty).cast::<c_void>(),
                )
            })?;
        }
        self.frame.cursor = self.read_cursor()?;
        Ok(&self.frame)
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    fn get<T: Copy + Default>(&self, data: ffi::GhosttyRenderStateData) -> Result<T, Error> {
        let mut value = T::default();
        check("ghostty_render_state_get", unsafe {
            ffi::ghostty_render_state_get(self.state, data, (&mut value as *mut T).cast::<c_void>())
        })?;
        Ok(value)
    }

    fn read_cursor(&self) -> Result<Option<Cursor>, Error> {
        let mut cursor: ffi::GhosttyRenderStateCursor = unsafe { std::mem::zeroed() };
        cursor.size = std::mem::size_of::<ffi::GhosttyRenderStateCursor>();
        check("ghostty_render_state_get(cursor)", unsafe {
            ffi::ghostty_render_state_get(
                self.state,
                ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_CURSOR,
                (&mut cursor as *mut ffi::GhosttyRenderStateCursor).cast::<c_void>(),
            )
        })?;
        if !cursor.viewport_has_value || !cursor.visible {
            return Ok(None);
        }
        let shape = match cursor.visual_style {
            ffi::GhosttyCursorStyle_GHOSTTY_CURSOR_STYLE_BAR => CursorShape::Bar,
            ffi::GhosttyCursorStyle_GHOSTTY_CURSOR_STYLE_UNDERLINE => CursorShape::Underline,
            ffi::GhosttyCursorStyle_GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW => CursorShape::HollowBlock,
            _ => CursorShape::Block,
        };
        Ok(Some(Cursor {
            x: cursor.viewport_x,
            y: cursor.viewport_y,
            shape,
            blinking: cursor.blinking,
        }))
    }

    fn read_rows(&mut self, full: bool, foreground: Rgb, background: Rgb) -> Result<(), Error> {
        check("ghostty_render_state_get(row iterator)", unsafe {
            ffi::ghostty_render_state_get(
                self.state,
                ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                (&mut self.rows as *mut ffi::GhosttyRenderStateRowIterator).cast::<c_void>(),
            )
        })?;
        let mut y: u16 = 0;
        while y < self.frame.rows
            && unsafe { ffi::ghostty_render_state_row_iterator_next(self.rows) }
        {
            let mut row_dirty = full;
            if !row_dirty {
                check("ghostty_render_state_row_get(dirty)", unsafe {
                    ffi::ghostty_render_state_row_get(
                        self.rows,
                        ffi::GhosttyRenderStateRowData_GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY,
                        (&mut row_dirty as *mut bool).cast::<c_void>(),
                    )
                })?;
            }
            if row_dirty {
                self.read_row(y, foreground, background)?;
                self.frame.dirty_rows.push(y);
                let clean = false;
                check("ghostty_render_state_row_set(dirty)", unsafe {
                    ffi::ghostty_render_state_row_set(
                        self.rows,
                        ffi::GhosttyRenderStateRowOption_GHOSTTY_RENDER_STATE_ROW_OPT_DIRTY,
                        (&clean as *const bool).cast::<c_void>(),
                    )
                })?;
            }
            y += 1;
        }
        Ok(())
    }

    fn read_row(&mut self, y: u16, foreground: Rgb, background: Rgb) -> Result<(), Error> {
        check("ghostty_render_state_row_get(cells)", unsafe {
            ffi::ghostty_render_state_row_get(
                self.rows,
                ffi::GhosttyRenderStateRowData_GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                (&mut self.cells as *mut ffi::GhosttyRenderStateRowCells).cast::<c_void>(),
            )
        })?;
        let cols = self.frame.cols;
        let start = usize::from(y) * usize::from(cols);
        let mut x: u16 = 0;
        while x < cols && unsafe { ffi::ghostty_render_state_row_cells_next(self.cells) } {
            let cell = self.read_cell(foreground, background)?;
            let index = start + usize::from(x);
            if cell.text.is_empty() && x > 0 && self.is_spacer_tail()? {
                self.frame.cells[index - 1].wide = true;
            }
            self.frame.cells[index] = cell;
            x += 1;
        }
        for blank in x..cols {
            self.frame.cells[start + usize::from(blank)] = Cell {
                fg: foreground,
                bg: background,
                ..Cell::default()
            };
        }
        Ok(())
    }

    fn cell_get<T>(
        &self,
        data: ffi::GhosttyRenderStateRowCellsData,
        out: &mut T,
    ) -> ffi::GhosttyResult {
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells,
                data,
                (out as *mut T).cast::<c_void>(),
            )
        }
    }

    fn read_cell(&mut self, foreground: Rgb, background: Rgb) -> Result<Cell, Error> {
        let mut cell = Cell {
            fg: foreground,
            bg: background,
            ..Cell::default()
        };
        let mut styled = false;
        check(
            "ghostty_render_state_row_cells_get(has styling)",
            self.cell_get(
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING,
                &mut styled,
            ),
        )?;
        if styled {
            let mut style: ffi::GhosttyStyle = unsafe { std::mem::zeroed() };
            check(
                "ghostty_render_state_row_cells_get(style)",
                self.cell_get(
                    ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                    &mut style,
                ),
            )?;
            cell.flags = style.flags;
        }
        let mut color = ffi::GhosttyColorRgb { r: 0, g: 0, b: 0 };
        if self.cell_get(
            ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
            &mut color,
        ) == ffi::GhosttyResult_GHOSTTY_SUCCESS
        {
            cell.bg = Rgb::from(color);
            cell.has_bg = true;
        }
        if self.cell_get(
            ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
            &mut color,
        ) == ffi::GhosttyResult_GHOSTTY_SUCCESS
        {
            cell.fg = Rgb::from(color);
        }
        let mut selected = false;
        check(
            "ghostty_render_state_row_cells_get(selected)",
            self.cell_get(
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_SELECTED,
                &mut selected,
            ),
        )?;
        cell.selected = selected;
        cell.text = self.read_text()?;
        Ok(cell)
    }

    fn read_text(&mut self) -> Result<String, Error> {
        loop {
            let mut buffer = ffi::GhosttyCodepointBuffer {
                ptr: self.graphemes.as_mut_ptr(),
                cap: self.graphemes.len(),
                len: 0,
            };
            let result = self.cell_get(
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
                &mut buffer,
            );
            if result == ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE {
                self.graphemes.resize(buffer.len, 0);
                continue;
            }
            check("ghostty_render_state_row_cells_get(graphemes)", result)?;
            return Ok(self.graphemes[..buffer.len]
                .iter()
                .filter_map(|&cp| char::from_u32(cp))
                .collect());
        }
    }

    fn is_spacer_tail(&self) -> Result<bool, Error> {
        let mut raw: ffi::GhosttyCell = 0;
        check(
            "ghostty_render_state_row_cells_get(raw)",
            self.cell_get(
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                &mut raw,
            ),
        )?;
        let mut wide: ffi::GhosttyCellWide = ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_NARROW;
        check("ghostty_cell_get(wide)", unsafe {
            ffi::ghostty_cell_get(
                raw,
                ffi::GhosttyCellData_GHOSTTY_CELL_DATA_WIDE,
                (&mut wide as *mut ffi::GhosttyCellWide).cast::<c_void>(),
            )
        })?;
        Ok(wide == ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_SPACER_TAIL)
    }
}

impl Drop for Vt {
    fn drop(&mut self) {
        unsafe {
            if !self.mouse_event.is_null() {
                ffi::ghostty_mouse_event_free(self.mouse_event);
            }
            if !self.mouse_encoder.is_null() {
                ffi::ghostty_mouse_encoder_free(self.mouse_encoder);
            }
            if !self.key_event.is_null() {
                ffi::ghostty_key_event_free(self.key_event);
            }
            if !self.key_encoder.is_null() {
                ffi::ghostty_key_encoder_free(self.key_encoder);
            }
            ffi::ghostty_render_state_row_cells_free(self.cells);
            ffi::ghostty_render_state_row_iterator_free(self.rows);
            ffi::ghostty_render_state_free(self.state);
            ffi::ghostty_terminal_free(self.terminal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };

    fn frame_of(vt: &mut Vt) -> Frame {
        vt.update().unwrap().clone()
    }

    #[test]
    fn written_text_lands_in_the_frame_with_its_colors_and_style() {
        let mut vt = Vt::new(20, 4).unwrap();
        vt.write(b"plain \x1b[1;38;2;255;0;0mbold red\x1b[0m");
        let frame = frame_of(&mut vt);
        assert_eq!((frame.cols, frame.rows), (20, 4));
        assert_eq!(frame.row_text(0), "plain bold red");
        let b = frame.cell(6, 0);
        assert_eq!(b.text, "b");
        assert_eq!(b.fg, RED);
        assert_ne!(b.flags & style::BOLD, 0);
        assert_eq!(frame.cell(0, 0).flags & style::BOLD, 0);
        assert_eq!(frame.cursor.map(|c| (c.x, c.y)), Some((14, 0)));
    }

    #[test]
    fn only_rows_that_changed_are_read_again() {
        let mut vt = Vt::new(10, 3).unwrap();
        vt.write(b"one\r\ntwo");
        let first = frame_of(&mut vt);
        assert!(first.dirty_rows.contains(&0) && first.dirty_rows.contains(&1));
        vt.write(b"!");
        let second = frame_of(&mut vt);
        assert_eq!(second.dirty_rows, vec![1]);
        assert_eq!(second.row_text(1), "two!");
        assert_eq!(second.row_text(0), "one");
    }

    #[test]
    fn a_wide_character_marks_its_left_cell_and_leaves_an_empty_tail() {
        let mut vt = Vt::new(10, 2).unwrap();
        vt.write("a界b".as_bytes());
        let frame = frame_of(&mut vt);
        assert!(frame.cell(1, 0).wide);
        assert_eq!(frame.cell(1, 0).text, "界");
        assert_eq!(frame.cell(2, 0).text, "");
        assert_eq!(frame.cell(3, 0).text, "b");
    }

    #[test]
    fn a_worker_snapshot_restores_the_same_screen() {
        let mut source = pm_ghostty::Terminal::new(30, 5).unwrap();
        source.write(b"\x1b[32mhello\x1b[0m from the worker\r\nsecond line");
        let snapshot = source.snapshot().unwrap();
        let mut vt = Vt::restore(&snapshot).unwrap();
        let frame = frame_of(&mut vt);
        assert_eq!(frame.row_text(0), "hello from the worker");
        assert_eq!(frame.row_text(1), "second line");
        assert_ne!(
            frame.cell(0, 0).fg,
            frame.foreground,
            "the green survives the snapshot"
        );
    }

    #[test]
    fn resizing_rereads_the_whole_grid() {
        let mut vt = Vt::new(10, 2).unwrap();
        vt.write(b"abc");
        frame_of(&mut vt);
        vt.resize(20, 4, 8, 16).unwrap();
        let frame = frame_of(&mut vt);
        assert_eq!((frame.cols, frame.rows, frame.cells.len()), (20, 4, 80));
        assert_eq!(frame.dirty_rows.len(), 4);
        assert_eq!(frame.row_text(0), "abc");
    }
}
