//! A fixed-size text grid, independent of PTY I/O and escape-sequence parsing.

use std::io;
use unicode_width::UnicodeWidthChar;

const MAX_COMBINING_SCALARS: usize = 16;

use crate::style::{Cell, Style};

/// Inclusive erase range relative to the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    ToEnd,
    ToStart,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SavedCursor {
    row: usize,
    column: usize,
    style: Style,
    wrap_pending: bool,
}

/// Minimal screen state with zero-based coordinates and full-screen scrolling.
///
/// The text API accepts printable ASCII, LF, CR and BS. Cursor movement and
/// erasure are separate operations used by the parser. Grapheme-cluster shaping,
/// scrollback and resizing belong to later steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    rows: usize,
    columns: usize,
    cells: Vec<Cell>,
    inactive_cells: Vec<Cell>,
    saved_main_cursor: Option<SavedCursor>,
    style: Style,
    row: usize,
    column: usize,
    wrap_pending: bool,
}

impl Screen {
    /// Create a blank screen with the cursor at the upper-left corner.
    pub fn new(rows: usize, columns: usize) -> io::Result<Self> {
        let length = rows
            .checked_mul(columns)
            .filter(|&length| length != 0)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid screen dimensions")
            })?;
        let mut cells = Vec::new();
        cells.try_reserve_exact(length).map_err(io::Error::other)?;
        cells.resize(length, Cell::default());
        // Reserve both grids at construction so mode switching cannot fail allocation.
        let mut inactive_cells = Vec::new();
        inactive_cells
            .try_reserve_exact(length)
            .map_err(io::Error::other)?;
        inactive_cells.resize(length, Cell::default());
        Ok(Self {
            rows,
            columns,
            cells,
            inactive_cells,
            saved_main_cursor: None,
            style: Style::default(),
            row: 0,
            column: 0,
            wrap_pending: false,
        })
    }

    pub fn is_alternate(&self) -> bool {
        self.saved_main_cursor.is_some()
    }

    /// Enter a cleared alternate grid while retaining the current coordinates and style.
    /// Repeated entry is a no-op: this is a mode, not a stack of nested screens.
    pub fn enter_alternate(&mut self) {
        if self.is_alternate() {
            return;
        }
        self.saved_main_cursor = Some(SavedCursor {
            row: self.row,
            column: self.column,
            style: self.style,
            wrap_pending: self.wrap_pending,
        });
        std::mem::swap(&mut self.cells, &mut self.inactive_cells);
        let blank = self.blank();
        self.cells.fill(blank);
        self.wrap_pending = false;
    }

    /// Restore main cells, coordinates, writing style and delayed wrap.
    /// A reset while already on the main screen is a no-op.
    pub fn leave_alternate(&mut self) {
        let Some(saved) = self.saved_main_cursor.take() else {
            return;
        };
        std::mem::swap(&mut self.cells, &mut self.inactive_cells);
        // Release discarded combining suffixes; the next visit starts blank.
        self.inactive_cells.fill(Cell::default());
        self.row = saved.row;
        self.column = saved.column;
        self.style = saved.style;
        self.wrap_pending = saved.wrap_pending;
    }

    pub fn dimensions(&self) -> (usize, usize) {
        (self.rows, self.columns)
    }

    /// Return (row, column). A pending wrap keeps the cursor in the last column.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.column)
    }

    pub fn wrap_pending(&self) -> bool {
        self.wrap_pending
    }

    /// Borrow a row, including its trailing blank cells.
    pub fn row(&self, row: usize) -> Option<&[Cell]> {
        if row >= self.rows {
            return None;
        }
        let start = row * self.columns;
        Some(&self.cells[start..start + self.columns])
    }

    /// Apply printable ASCII and LF/CR/BS. Unsupported bytes reject the entire
    /// input before mutation; this is a model API, not a terminal byte parser.
    pub fn write_ascii(&mut self, bytes: &[u8]) -> io::Result<()> {
        if !bytes
            .iter()
            .all(|byte| matches!(byte, b' '..=b'~' | b'\n' | b'\r' | 8))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported screen input",
            ));
        }
        for &byte in bytes {
            match byte {
                b'\n' => {
                    // LF moves vertically; CR is what returns to the left edge.
                    self.wrap_pending = false;
                    self.line_feed();
                }
                b'\r' => {
                    self.wrap_pending = false;
                    self.column = 0;
                }
                8 => {
                    // BS moves left without erasing or crossing a row boundary.
                    self.wrap_pending = false;
                    self.column = self.column.saturating_sub(1);
                }
                _ => self.print(char::from(byte)),
            }
        }
        Ok(())
    }

    /// Write one decoded scalar using non-CJK Unicode character widths.
    /// Controls are ignored. This does not implement grapheme-cluster shaping.
    pub fn print(&mut self, mut character: char) {
        if character.is_control() {
            return;
        }
        let Some(mut width) = character.width() else {
            return;
        };
        if width == 0 {
            let column = if self.wrap_pending {
                self.column
            } else if self.column > 0 {
                self.column - 1
            } else {
                return;
            };
            let mut index = self.row * self.columns + column;
            if self.cells[index].width == 0 {
                index -= 1;
            }
            if self.cells[index].combining.len() < MAX_COMBINING_SCALARS {
                self.cells[index].combining.push(character);
            }
            return;
        }
        // The model supports one- and two-column scalars. A one-column screen
        // cannot hold a wide glyph; use a visible replacement instead.
        if width > 2 || width > self.columns {
            character = '\u{fffd}';
            width = 1;
        }
        if self.wrap_pending || self.column + width > self.columns {
            if !self.wrap_pending {
                let start = self.row * self.columns + self.column;
                self.clear_range(start..(self.row + 1) * self.columns);
            }
            self.column = 0;
            self.line_feed();
            self.wrap_pending = false;
        }
        let index = self.row * self.columns + self.column;
        self.clear_range(index..index + width);
        self.cells[index] = Cell {
            character,
            width: width as u8,
            style: self.style,
            ..Cell::default()
        };
        if width == 2 {
            self.cells[index + 1] = Cell {
                width: 0,
                style: self.style,
                ..Cell::default()
            };
        }
        if self.column + width == self.columns {
            self.column = self.columns - 1;
            self.wrap_pending = true;
        } else {
            self.column += width;
        }
    }

    // Any write or erase touching half a wide glyph clears both halves.
    fn clear_range(&mut self, mut range: std::ops::Range<usize>) {
        if self.cells[range.start].width == 0 {
            range.start -= 1;
        }
        if self.cells[range.end - 1].width == 2 {
            range.end += 1;
        }
        let blank = self.blank();
        self.cells[range].fill(blank);
    }

    /// Attributes used for subsequent writes; existing cells are unaffected.
    pub fn style(&self) -> Style {
        self.style
    }

    /// Changing attributes leaves the cursor and pending wrap unchanged.
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    fn blank(&self) -> Cell {
        // Erasure and newly exposed rows use the active background, without
        // copying text decorations or inverse into the blank cells.
        Cell {
            style: Style {
                background: self.style.background,
                ..Style::default()
            },
            ..Cell::default()
        }
    }

    /// Position the cursor using zero-based coordinates, clamped to the grid.
    /// Explicit movement cancels delayed wrapping and never scrolls.
    pub fn move_to(&mut self, row: usize, column: usize) {
        self.row = row.min(self.rows - 1);
        self.column = column.min(self.columns - 1);
        self.wrap_pending = false;
    }

    pub fn move_up(&mut self, count: usize) {
        self.move_to(self.row.saturating_sub(count), self.column);
    }

    pub fn move_down(&mut self, count: usize) {
        self.move_to(self.row.saturating_add(count), self.column);
    }

    pub fn move_left(&mut self, count: usize) {
        self.move_to(self.row, self.column.saturating_sub(count));
    }

    pub fn move_right(&mut self, count: usize) {
        self.move_to(self.row, self.column.saturating_add(count));
    }

    /// Blank part or all of the current row, including the cursor cell.
    /// Cursor coordinates stay unchanged; delayed wrapping is cancelled.
    pub fn erase_line(&mut self, mode: EraseMode) {
        let start = self.row * self.columns;
        let cursor = start + self.column;
        let end = start + self.columns;
        let range = match mode {
            EraseMode::ToEnd => cursor..end,
            EraseMode::ToStart => start..cursor + 1,
            EraseMode::All => start..end,
        };
        self.clear_range(range);
        self.wrap_pending = false;
    }

    /// Blank part or all of the grid, including the cursor cell, without homing.
    /// This model has no saved lines, so only the visible grid is affected.
    pub fn erase_display(&mut self, mode: EraseMode) {
        let cursor = self.row * self.columns + self.column;
        let range = match mode {
            EraseMode::ToEnd => cursor..self.cells.len(),
            EraseMode::ToStart => 0..cursor + 1,
            EraseMode::All => 0..self.cells.len(),
        };
        self.clear_range(range);
        self.wrap_pending = false;
    }

    fn line_feed(&mut self) {
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.cells.rotate_left(self.columns);
            let last_row = (self.rows - 1) * self.columns;
            let blank = self.blank();
            self.cells[last_row..].fill(blank);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Screen;

    fn lines(screen: &Screen) -> Vec<String> {
        (0..screen.dimensions().0)
            .map(|row| {
                screen
                    .row(row)
                    .unwrap()
                    .iter()
                    .map(|cell| cell.character)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn controls_move_without_erasing_and_lf_preserves_column() {
        let mut screen = Screen::new(3, 5).unwrap();
        screen.write_ascii(b"abc\x08X\nY\rZ").unwrap();
        assert_eq!(lines(&screen), ["abX  ", "Z  Y ", "     "]);
        assert_eq!(screen.cursor(), (1, 1));
        screen.write_ascii(b"\r\x08\x08Q").unwrap();
        assert_eq!(lines(&screen)[1], "Q  Y ");
        assert_eq!(screen.cursor(), (1, 1));
    }

    #[test]
    fn last_column_wraps_only_when_next_character_arrives() {
        let mut screen = Screen::new(2, 3).unwrap();
        screen.write_ascii(b"abcdef").unwrap();
        assert_eq!(lines(&screen), ["abc", "def"]);
        assert_eq!(screen.cursor(), (1, 2));
        assert!(screen.wrap_pending());
        screen.write_ascii(b"g").unwrap();
        assert_eq!(lines(&screen), ["def", "g  "]);
        assert_eq!(screen.cursor(), (1, 1));
        assert!(!screen.wrap_pending());
    }

    #[test]
    fn controls_cancel_pending_wrap() {
        for (control, expected, cursor) in [
            (b'\r', vec!["Xbc", "   "], (0, 1)),
            (8, vec!["aXc", "   "], (0, 2)),
            (b'\n', vec!["abc", "  X"], (1, 2)),
        ] {
            let mut screen = Screen::new(2, 3).unwrap();
            screen.write_ascii(b"abc").unwrap();
            screen.write_ascii(&[control, b'X']).unwrap();
            assert_eq!(lines(&screen), expected);
            assert_eq!(screen.cursor(), cursor);
        }
    }

    #[test]
    fn scrolls_one_line_and_handles_a_single_cell_screen() {
        let mut screen = Screen::new(2, 4).unwrap();
        screen.write_ascii(b"one\r\ntwo\r\n").unwrap();
        assert_eq!(lines(&screen), ["two ", "    "]);
        assert_eq!(screen.cursor(), (1, 0));
        let mut screen = Screen::new(1, 1).unwrap();
        screen.write_ascii(b"ab").unwrap();
        assert_eq!(lines(&screen), ["b"]);
        screen.write_ascii(b"\n").unwrap();
        assert_eq!(lines(&screen), [" "]);
        assert_eq!(screen.cursor(), (0, 0));
        assert!(!screen.wrap_pending());
    }

    #[test]
    fn chunk_boundaries_do_not_change_screen_state() {
        let input = b"abcdefg\r\nhi\x08J\nklmnop";
        let mut expected = Screen::new(3, 4).unwrap();
        expected.write_ascii(input).unwrap();
        for split in 0..=input.len() {
            let mut screen = Screen::new(3, 4).unwrap();
            screen.write_ascii(&input[..split]).unwrap();
            screen.write_ascii(&input[split..]).unwrap();
            assert_eq!(screen, expected);
        }
    }

    #[test]
    fn invalid_input_and_dimensions_are_rejected() {
        for dimensions in [(0, 3), (3, 0), (usize::MAX, 2)] {
            assert!(Screen::new(dimensions.0, dimensions.1).is_err());
        }
        let mut screen = Screen::new(2, 3).unwrap();
        assert_eq!(lines(&screen), ["   ", "   "]);
        assert!(screen.row(2).is_none());
        screen.write_ascii(b"abc").unwrap();
        let before = screen.clone();
        for input in [b"ok\x1b".as_slice(), b"\t", b"\x7f", "中".as_bytes()] {
            assert!(screen.write_ascii(input).is_err());
            assert_eq!(screen, before);
        }
    }
}
