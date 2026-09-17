//! A resizable text grid, independent of PTY I/O and escape-sequence parsing.

use std::io;

mod reflow;
use unicode_width::UnicodeWidthChar;

const MAX_COMBINING_SCALARS: usize = 16;
const DEFAULT_TAB_WIDTH: usize = 8;

use crate::{
    scrollback::Scrollback,
    style::{Cell, Style},
};

/// Maximum retained physical history rows per screen.
pub const SCROLLBACK_MAX_LINES: usize = crate::scrollback::MAX_LINES;
/// Maximum retained history cells, independent of the visible grid limit.
pub const SCROLLBACK_MAX_CELLS: usize = crate::scrollback::MAX_CELLS;

/// DECSCUSR shapes; blinking is delegated to the outer terminal.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CursorShape {
    #[default]
    BlinkingBlock = 1,
    SteadyBlock = 2,
    BlinkingUnderline = 3,
    SteadyUnderline = 4,
    BlinkingBar = 5,
    SteadyBar = 6,
}

/// Mutually exclusive mouse tracking modes. Encoding is configured separately.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum MouseTracking {
    #[default]
    Off = 0,
    Button = 1000,
    Drag = 1002,
    Any = 1003,
}

/// Inclusive erase range relative to the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    ToEnd,
    ToStart,
    All,
}

/// G0/G1 designations and the currently invoked set. Non-ASCII text is unchanged.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CharacterSets {
    graphics: [bool; 2],
    active: usize,
}

impl CharacterSets {
    fn translate(self, character: char) -> char {
        if !self.graphics[self.active] {
            return character;
        }
        // DEC Special Graphics 0x5f..=0x7e, represented by Unicode glyphs.
        // Control pictures here are visible symbols, not executable controls.
        const GRAPHICS: [char; 32] = [
            ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺',
            '⎻', '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
        ];
        match character {
            '_'..='~' => GRAPHICS[character as usize - '_' as usize],
            _ => character,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SavedCursor {
    row: usize,
    column: usize,
    style: Style,
    wrap_pending: bool,
    origin_mode: bool,
    auto_wrap: bool,
    character_sets: CharacterSets,
}

/// Screen state with zero-based coordinates and per-grid vertical scrolling margins.
///
/// The text API accepts printable ASCII, LF, CR and BS. Cursor movement and
/// erasure are separate operations used by the parser. Grapheme-cluster shaping
/// remains outside this model; history browsing uses a separate snapshot view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    rows: usize,
    columns: usize,
    cells: Vec<Cell>,
    inactive_cells: Vec<Cell>,
    scrollback: Scrollback,
    primary_scroll_count: u64,
    continued: Vec<bool>,
    inactive_continued: Vec<bool>,
    used: Vec<usize>,
    inactive_used: Vec<usize>,
    saved_main_cursor: Option<SavedCursor>,
    saved_cursor: Option<SavedCursor>,
    inactive_saved_cursor: Option<SavedCursor>,
    cursor_visible: bool,
    cursor_shape: CursorShape,
    insert_mode: bool,
    bracketed_paste: bool,
    focus_reporting: bool,
    synchronized_output: bool,
    mouse_tracking: MouseTracking,
    sgr_mouse: bool,
    application_cursor_keys: bool,
    application_keypad: bool,
    tab_stops: Vec<bool>,
    scroll_region: (usize, usize),
    inactive_scroll_region: (usize, usize),
    style: Style,
    row: usize,
    column: usize,
    wrap_pending: bool,
    origin_mode: bool,
    auto_wrap: bool,
    character_sets: CharacterSets,
}

impl Screen {
    /// Number of retained primary-screen rows, ordered oldest to newest.
    pub fn history_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Number of rows scrolled from the primary grid by terminal output.
    pub(crate) fn primary_scroll_count(&self) -> u64 {
        self.primary_scroll_count
    }

    /// Read a retained physical row at its original width, including cell styles.
    pub fn history_row(&self, index: usize) -> Option<&[Cell]> {
        self.scrollback.row(index)
    }

    /// Whether this row was reached by automatic wrapping from its predecessor.
    pub fn row_continued(&self, row: usize) -> Option<bool> {
        self.continued.get(row).copied()
    }

    /// Whether a retained history row continues its preceding physical row.
    pub fn history_row_continued(&self, row: usize) -> Option<bool> {
        self.scrollback.continued(row)
    }

    /// Meaningful columns in a row, including explicitly written trailing spaces.
    pub fn row_used_columns(&self, row: usize) -> Option<usize> {
        self.used.get(row).copied()
    }

    /// Meaningful columns in a retained physical row at its original width.
    pub fn history_row_used_columns(&self, row: usize) -> Option<usize> {
        self.scrollback.used(row)
    }

    /// Discard primary-screen history without changing either visible grid or modes.
    pub fn clear_history(&mut self) {
        self.scrollback = Scrollback::default();
    }

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
        let mut tab_stops = Vec::new();
        tab_stops
            .try_reserve_exact(columns)
            .map_err(io::Error::other)?;
        tab_stops.extend((0..columns).map(|column| column != 0 && column % DEFAULT_TAB_WIDTH == 0));
        let mut continued = Vec::new();
        continued
            .try_reserve_exact(rows)
            .map_err(io::Error::other)?;
        continued.resize(rows, false);
        let mut inactive_continued = Vec::new();
        inactive_continued
            .try_reserve_exact(rows)
            .map_err(io::Error::other)?;
        inactive_continued.resize(rows, false);
        let mut used = Vec::new();
        used.try_reserve_exact(rows).map_err(io::Error::other)?;
        used.resize(rows, 0);
        let mut inactive_used = Vec::new();
        inactive_used
            .try_reserve_exact(rows)
            .map_err(io::Error::other)?;
        inactive_used.resize(rows, 0);
        Ok(Self {
            used,
            inactive_used,
            continued,
            inactive_continued,
            rows,
            columns,
            cells,
            inactive_cells,
            scrollback: Scrollback::default(),
            primary_scroll_count: 0,
            saved_main_cursor: None,
            saved_cursor: None,
            inactive_saved_cursor: None,
            cursor_visible: true,
            cursor_shape: CursorShape::default(),
            insert_mode: false,
            bracketed_paste: false,
            focus_reporting: false,
            synchronized_output: false,
            mouse_tracking: MouseTracking::Off,
            sgr_mouse: false,
            application_cursor_keys: false,
            application_keypad: false,
            tab_stops,
            scroll_region: (0, rows - 1),
            inactive_scroll_region: (0, rows - 1),
            style: Style::default(),
            row: 0,
            column: 0,
            wrap_pending: false,
            origin_mode: false,
            auto_wrap: true,
            character_sets: CharacterSets::default(),
        })
    }

    /// RIS: restore initial model state at the current size without allocating.
    /// Both grids and saved cursors are cleared; the active grid becomes main.
    pub fn reset(&mut self) {
        self.cells.fill(Cell::default());
        self.inactive_cells.fill(Cell::default());
        self.clear_history();
        self.continued.fill(false);
        self.inactive_continued.fill(false);
        self.used.fill(0);
        self.inactive_used.fill(0);
        self.saved_main_cursor = None;
        self.saved_cursor = None;
        self.inactive_saved_cursor = None;
        self.cursor_visible = true;
        self.synchronized_output = false;
        self.cursor_shape = CursorShape::default();
        self.insert_mode = false;
        self.application_cursor_keys = false;
        self.application_keypad = false;
        self.bracketed_paste = false;
        self.focus_reporting = false;
        self.mouse_tracking = MouseTracking::Off;
        self.sgr_mouse = false;
        for (column, stop) in self.tab_stops.iter_mut().enumerate() {
            *stop = column != 0 && column % DEFAULT_TAB_WIDTH == 0;
        }
        self.scroll_region = (0, self.rows - 1);
        self.inactive_scroll_region = (0, self.rows - 1);
        self.style = Style::default();
        self.row = 0;
        self.column = 0;
        self.wrap_pending = false;
        self.origin_mode = false;
        self.auto_wrap = true;
        self.character_sets = CharacterSets::default();
    }

    /// DECSTR: reset supported modes while retaining cells, cursor coordinates,
    /// active grid and tab stops. Autowrap follows the XTerm default (enabled).
    pub fn soft_reset(&mut self) {
        self.cursor_visible = true;
        self.synchronized_output = false;
        self.cursor_shape = CursorShape::default();
        self.insert_mode = false;
        self.application_cursor_keys = false;
        self.application_keypad = false;
        self.scroll_region = (0, self.rows - 1);
        self.style = Style::default();
        self.wrap_pending = false;
        self.origin_mode = false;
        self.auto_wrap = true;
        self.character_sets = CharacterSets::default();
        // Unlike RIS, DECSTR establishes a home-position save slot for the
        // current grid and preserves the inactive grid and mode-1049 snapshot.
        self.saved_cursor = Some(SavedCursor {
            row: 0,
            column: 0,
            style: self.style,
            wrap_pending: false,
            origin_mode: false,
            auto_wrap: true,
            character_sets: self.character_sets,
        });
    }

    /// Resize both grids. Width changes reflow the primary grid and its history,
    /// mapping current/saved cursors through logical lines. The alternate grid clips.
    /// Same-width height changes archive/restore rows to retain the primary cursor.
    /// Invalid dimensions or reported allocation errors leave the model unchanged;
    /// infallible cloning/history allocations may abort on exhaustion.
    /// Scrolling margins reset. Primary reflow preserves a mapped pending wrap;
    /// other size changes clear it. An unchanged size is a no-op.
    pub fn resize(&mut self, rows: usize, columns: usize) -> io::Result<()> {
        if columns != self.columns {
            let mut resized = self.clone();
            resized.resize_grid(rows, columns, false)?;
            self.reflow_primary_into(&mut resized);
            *self = resized;
            Ok(())
        } else {
            self.resize_grid(rows, columns, true)
        }
    }

    /// Reflow completed output while keeping the live shell prompt's physical rows.
    /// Shells such as fish repaint after SIGWINCH by moving relative to the old
    /// prompt height. Reflowing those rows first would make that cleanup miss the
    /// newly wrapped prefix and leave duplicate prompts behind.
    pub(crate) fn resize_preserving_tail(
        &mut self,
        rows: usize,
        columns: usize,
        start: (usize, usize),
    ) -> io::Result<(usize, usize)> {
        if rows == 0
            || columns == 0
            || self.is_alternate()
            || start.0 > self.row
            || start.0 >= self.rows
        {
            let cursor_offset = self.row.saturating_sub(start.0);
            self.resize(rows, columns)?;
            return Ok((
                self.row.saturating_sub(cursor_offset),
                start.1.min(columns.saturating_sub(1)),
            ));
        }
        let old_columns = self.columns;
        let end_row = self.row;
        let tail_rows = end_row - start.0 + 1;
        let tail_cells = self.cells[start.0 * old_columns..(end_row + 1) * old_columns].to_vec();
        let tail_used = self.used[start.0..=end_row].to_vec();
        let cursor_offset = self.row - start.0;
        let cursor_column = self.column;

        let blank = self.blank();
        self.cells[start.0 * old_columns..].fill(blank);
        self.used[start.0..].fill(0);
        self.continued[start.0..].fill(false);
        self.row = start.0;
        self.column = start.1.min(old_columns - 1);
        self.wrap_pending = false;
        self.resize(rows, columns)?;

        let target_row = self.row.min(rows.saturating_sub(tail_rows));
        let blank = self.blank();
        for offset in 0..tail_rows.min(rows - target_row) {
            let target = (target_row + offset) * columns;
            self.cells[target..target + columns].fill(blank.clone());
            let source = &tail_cells[offset * old_columns..(offset + 1) * old_columns];
            let copied = old_columns.min(columns);
            for (column, cell) in source.iter().take(copied).enumerate() {
                if cell.width == 2 && column + 1 == columns {
                    continue;
                }
                self.cells[target + column] = cell.clone();
            }
            self.used[target_row + offset] = Self::clipped_used(source, tail_used[offset], columns);
            self.continued[target_row + offset] = false;
        }
        self.row = (target_row + cursor_offset).min(rows - 1);
        self.column = cursor_column.min(columns - 1);
        self.wrap_pending = false;
        Ok((target_row, start.1.min(columns - 1)))
    }

    /// Resize a disposable render canvas without archiving or restoring history.
    pub(crate) fn resize_display(&mut self, rows: usize, columns: usize) -> io::Result<()> {
        self.resize_grid(rows, columns, false)
    }

    fn resize_grid(&mut self, rows: usize, columns: usize, history: bool) -> io::Result<()> {
        if self.dimensions() == (rows, columns) {
            return Ok(());
        }
        // Allocate both destinations before taking any content out of the old grids.
        // Cell suffixes are moved, not cloned, so copying the overlap cannot allocate.
        let mut resized = Self::new(rows, columns)?;
        let main_row = self.saved_main_cursor.map_or(self.row, |saved| saved.row);
        let shift = if history {
            main_row.saturating_sub(rows - 1)
        } else {
            0
        };
        let restore = if history {
            rows.saturating_sub(self.rows).min(self.scrollback.len())
        } else {
            0
        };
        let (active_restore, inactive_restore) = if self.is_alternate() {
            (0, restore)
        } else {
            (restore, 0)
        };
        let (active_shift, inactive_shift) = if self.is_alternate() {
            (0, shift)
        } else {
            (shift, 0)
        };
        // Build history on the destination before moving cells out of the source.
        // Snapshot sharing keeps the old history untouched during preparation.
        resized.scrollback = self.scrollback.clone();
        let main_cells = if self.is_alternate() {
            &self.inactive_cells
        } else {
            &self.cells
        };
        let main_continued = if self.is_alternate() {
            &self.inactive_continued
        } else {
            &self.continued
        };
        let main_used = if self.is_alternate() {
            &self.inactive_used
        } else {
            &self.used
        };
        for (index, row) in main_cells[..shift * self.columns]
            .chunks(self.columns)
            .enumerate()
        {
            resized
                .scrollback
                .push(row, main_continued[index], main_used[index]);
        }
        resized.cursor_visible = self.cursor_visible;
        resized.cursor_shape = self.cursor_shape;
        resized.insert_mode = self.insert_mode;
        resized.bracketed_paste = self.bracketed_paste;
        resized.focus_reporting = self.focus_reporting;
        resized.synchronized_output = self.synchronized_output;
        resized.mouse_tracking = self.mouse_tracking;
        resized.sgr_mouse = self.sgr_mouse;
        resized.application_cursor_keys = self.application_cursor_keys;
        resized.application_keypad = self.application_keypad;
        resized.origin_mode = self.origin_mode;
        resized.auto_wrap = self.auto_wrap;
        resized.character_sets = self.character_sets;
        let retained_columns = self.columns.min(columns);
        resized.tab_stops[..retained_columns].copy_from_slice(&self.tab_stops[..retained_columns]);
        let clamp = |saved: SavedCursor, shift: usize, restore: usize| SavedCursor {
            row: (saved.row.saturating_sub(shift) + restore).min(rows - 1),
            column: saved.column.min(columns - 1),
            wrap_pending: false,
            ..saved
        };
        resized.saved_cursor = self
            .saved_cursor
            .map(|saved| clamp(saved, active_shift, active_restore));
        resized.inactive_saved_cursor = self
            .inactive_saved_cursor
            .map(|saved| clamp(saved, inactive_shift, inactive_restore));
        resized.style = self.style;
        resized.cells.fill(self.blank());
        if self.blank().style != Style::default() {
            resized.used.fill(columns);
        }
        resized.row = (self.row.saturating_sub(active_shift) + active_restore).min(rows - 1);
        resized.column = self.column.min(columns - 1);
        if let Some(saved) = self.saved_main_cursor {
            resized.saved_main_cursor = Some(SavedCursor {
                row: (saved.row.saturating_sub(inactive_shift) + inactive_restore).min(rows - 1),
                column: saved.column.min(columns - 1),
                wrap_pending: false,
                ..saved
            });
            if saved.style.background != crate::style::Color::Default {
                resized.inactive_used.fill(columns);
            }
            resized.inactive_cells.fill(Cell {
                style: Style {
                    background: saved.style.background,
                    ..Style::default()
                },
                ..Cell::default()
            });
        }
        let mut mixed_history_widths = false;
        for row in (0..restore).rev() {
            let crate::scrollback::HistoryRow {
                cells,
                continued,
                used,
            } = resized
                .scrollback
                .pop_newest()
                .expect("restorable history row");
            mixed_history_widths |= cells.len() != columns;
            if cells.len() == columns {
                if self.is_alternate() {
                    resized.inactive_continued[row] = continued;
                } else {
                    resized.continued[row] = continued;
                }
            }
            let restored_used = Self::clipped_used(&cells, used, columns);
            if self.is_alternate() {
                resized.inactive_used[row] =
                    if cells.len() < columns && resized.inactive_used[row] == columns {
                        columns
                    } else {
                        restored_used
                    };
            } else {
                resized.used[row] = if cells.len() < columns && resized.used[row] == columns {
                    columns
                } else {
                    restored_used
                };
            }
            let destination = if self.is_alternate() {
                &mut resized.inactive_cells
            } else {
                &mut resized.cells
            };
            for (column, cell) in cells.iter().take(columns).enumerate() {
                if cell.width == 2 && column + 1 == columns {
                    continue;
                }
                destination[row * columns + column] = cell.clone();
            }
        }
        for (offset, cells) in self.cells[active_shift * self.columns..]
            .chunks(self.columns)
            .take(rows - active_restore)
            .enumerate()
        {
            let used = Self::clipped_used(cells, self.used[active_shift + offset], columns);
            resized.used[active_restore + offset] =
                if columns > self.columns && self.blank().style != Style::default() {
                    columns
                } else {
                    used
                };
        }
        for (offset, cells) in self.inactive_cells[inactive_shift * self.columns..]
            .chunks(self.columns)
            .take(rows - inactive_restore)
            .enumerate()
        {
            let used =
                Self::clipped_used(cells, self.inactive_used[inactive_shift + offset], columns);
            let padded = self
                .saved_main_cursor
                .is_some_and(|saved| saved.style.background != crate::style::Color::Default);
            resized.inactive_used[inactive_restore + offset] = if columns > self.columns && padded {
                columns
            } else {
                used
            };
        }
        Self::move_overlap(
            &mut self.cells[active_shift * self.columns..],
            self.columns,
            &mut resized.cells[active_restore * columns..],
            columns,
        );
        Self::move_overlap(
            &mut self.inactive_cells[inactive_shift * self.columns..],
            self.columns,
            &mut resized.inactive_cells[inactive_restore * columns..],
            columns,
        );
        if self.columns == columns && !mixed_history_widths {
            for (target, source) in resized.continued[active_restore..]
                .iter_mut()
                .zip(&self.continued[active_shift..])
            {
                *target = *source;
            }
            for (target, source) in resized.inactive_continued[inactive_restore..]
                .iter_mut()
                .zip(&self.inactive_continued[inactive_shift..])
            {
                *target = *source;
            }
        } else {
            // Width clipping invalidates the visible row relationships until reflow exists.
            resized.continued.fill(false);
            resized.inactive_continued.fill(false);
        }
        *self = resized;
        Ok(())
    }

    /// Reserve a top row on a disposable render copy, shifting active cells and cursor.
    /// This is display composition, not a terminal resize operation for the child.
    pub(crate) fn prepend_display_row(&mut self) -> io::Result<()> {
        self.resize_display(self.rows + 1, self.columns)?;
        self.cells.rotate_right(self.columns);
        self.used.rotate_right(1);
        self.used[0] = 0;
        self.continued.rotate_right(1);
        self.continued[0] = false;
        self.row += 1;
        Ok(())
    }

    // Display-only assembly helpers. The compositor validates all rectangles before
    // calling these; direct cell copies preserve styles, wide cells and suffixes.
    pub(crate) fn copy_display_cells(&mut self, source: &Screen, row: usize, column: usize) {
        for offset in 0..source.rows {
            let start = (row + offset) * self.columns + column;
            self.cells[start..start + source.columns]
                .clone_from_slice(source.row(offset).expect("source row exists"));
        }
    }

    pub(crate) fn set_display_cell(&mut self, row: usize, column: usize, cell: Cell) {
        self.cells[row * self.columns + column] = cell;
    }

    pub(crate) fn set_display_cursor(&mut self, row: usize, column: usize) {
        self.row = row;
        self.column = column;
        self.wrap_pending = false;
    }

    fn clipped_used(cells: &[Cell], used: usize, columns: usize) -> usize {
        let mut end = used.min(columns);
        if end == columns && end > 0 && cells.get(end - 1).is_some_and(|cell| cell.width == 2) {
            end -= 1;
        }
        end
    }

    fn move_overlap(source: &mut [Cell], old_columns: usize, target: &mut [Cell], columns: usize) {
        for (old_row, new_row) in source
            .chunks_mut(old_columns)
            .zip(target.chunks_mut(columns))
        {
            for (column, (old, new)) in old_row.iter_mut().zip(new_row).enumerate() {
                // A clipped wide leader is replaced by the destination's blank;
                // its continuation lies outside the retained overlap.
                if old.width == 2 && column + 1 == columns {
                    continue;
                }
                *new = std::mem::take(old);
            }
        }
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
            origin_mode: self.origin_mode,
            auto_wrap: self.auto_wrap,
            character_sets: self.character_sets,
        });
        std::mem::swap(&mut self.cells, &mut self.inactive_cells);
        std::mem::swap(&mut self.continued, &mut self.inactive_continued);
        std::mem::swap(&mut self.used, &mut self.inactive_used);
        std::mem::swap(&mut self.scroll_region, &mut self.inactive_scroll_region);
        std::mem::swap(&mut self.saved_cursor, &mut self.inactive_saved_cursor);
        let blank = self.blank();
        self.used.fill(if blank.style == Style::default() {
            0
        } else {
            self.columns
        });
        self.cells.fill(blank);
        self.continued.fill(false);
        self.wrap_pending = false;
    }

    /// Restore main cells, coordinates, writing style and delayed wrap.
    /// A reset while already on the main screen is a no-op.
    pub fn leave_alternate(&mut self) {
        let Some(saved) = self.saved_main_cursor.take() else {
            return;
        };
        std::mem::swap(&mut self.cells, &mut self.inactive_cells);
        std::mem::swap(&mut self.continued, &mut self.inactive_continued);
        std::mem::swap(&mut self.used, &mut self.inactive_used);
        std::mem::swap(&mut self.scroll_region, &mut self.inactive_scroll_region);
        std::mem::swap(&mut self.saved_cursor, &mut self.inactive_saved_cursor);
        // Release discarded combining suffixes; the next visit starts blank.
        self.inactive_cells.fill(Cell::default());
        self.inactive_continued.fill(false);
        self.inactive_used.fill(0);
        self.inactive_saved_cursor = None;
        self.inactive_scroll_region = (0, self.rows - 1);
        self.apply_saved_cursor(saved);
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Visibility is a global terminal mode, independent of saved cursor state.
    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    /// Shape is global and independent of visibility and saved cursor positions.
    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.cursor_shape = shape;
    }

    pub fn application_keypad(&self) -> bool {
        self.application_keypad
    }

    /// Global keypad input state, independent of cursor-key mode and saved cursors.
    pub fn set_application_keypad(&mut self, enabled: bool) {
        self.application_keypad = enabled;
    }

    pub fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys
    }

    /// DECCKM is global input state, independent of either grid's saved cursor.
    pub fn set_application_cursor_keys(&mut self, enabled: bool) {
        self.application_cursor_keys = enabled;
    }

    pub fn mouse_tracking(&self) -> MouseTracking {
        self.mouse_tracking
    }

    pub fn set_mouse_tracking(&mut self, tracking: MouseTracking) {
        self.mouse_tracking = tracking;
    }

    pub fn sgr_mouse(&self) -> bool {
        self.sgr_mouse
    }

    /// Encoding alone does not enable mouse event reporting.
    pub fn set_sgr_mouse(&mut self, enabled: bool) {
        self.sgr_mouse = enabled;
    }

    pub fn synchronized_output(&self) -> bool {
        self.synchronized_output
    }

    /// The event loop defers painting, not parsing or input, while enabled.
    pub fn set_synchronized_output(&mut self, enabled: bool) {
        self.synchronized_output = enabled;
    }

    pub fn focus_reporting(&self) -> bool {
        self.focus_reporting
    }

    /// Global input mode, preserved by cursor saves, grid switches and soft reset.
    pub fn set_focus_reporting(&mut self, enabled: bool) {
        self.focus_reporting = enabled;
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    /// A global input mode, independent of saved cursor state and soft reset.
    pub fn set_bracketed_paste(&mut self, enabled: bool) {
        self.bracketed_paste = enabled;
    }

    pub fn insert_mode(&self) -> bool {
        self.insert_mode
    }

    /// IRM is global, independent of cursor saves and screen switching.
    /// Changing it does not move the cursor or cancel pending wrap.
    pub fn set_insert_mode(&mut self, enabled: bool) {
        self.insert_mode = enabled;
    }

    /// Replace this grid's single DECSC slot; this is not a stack.
    pub fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            row: self.row,
            column: self.column,
            style: self.style,
            wrap_pending: self.wrap_pending,
            origin_mode: self.origin_mode,
            auto_wrap: self.auto_wrap,
            character_sets: self.character_sets,
        });
    }

    /// Restore this grid's saved coordinates, attributes and pending wrap.
    /// Without a prior save, leave the current state unchanged.
    pub fn restore_cursor(&mut self) {
        if let Some(saved) = self.saved_cursor {
            self.apply_saved_cursor(saved);
        }
    }

    fn apply_saved_cursor(&mut self, saved: SavedCursor) {
        self.origin_mode = saved.origin_mode;
        self.auto_wrap = saved.auto_wrap;
        self.character_sets = saved.character_sets;
        self.move_to(saved.row, saved.column);
        self.style = saved.style;
        // A changed margin can clamp the saved row. Do not restore delayed
        // wrapping to a different physical cell.
        self.wrap_pending = saved.wrap_pending && self.cursor() == (saved.row, saved.column);
    }

    pub fn origin_mode(&self) -> bool {
        self.origin_mode
    }

    /// DECOM changes the coordinate origin and homes, even when set repeatedly.
    pub fn set_origin_mode(&mut self, enabled: bool) {
        self.origin_mode = enabled;
        self.position(0, 0);
    }

    /// Address zero-based coordinates relative to the active protocol origin.
    pub fn position(&mut self, row: usize, column: usize) {
        let top = if self.origin_mode {
            self.scroll_region.0
        } else {
            0
        };
        self.move_to(top.saturating_add(row), column);
    }

    /// Address a row relative to the origin while retaining the current column.
    pub fn position_row(&mut self, row: usize) {
        self.position(row, self.column);
    }

    pub fn dimensions(&self) -> (usize, usize) {
        (self.rows, self.columns)
    }

    /// Return (row, column). A pending wrap keeps the cursor in the last column.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.column)
    }

    /// Whether the next positive-width character will trigger delayed wrapping.
    pub fn wrap_pending(&self) -> bool {
        self.auto_wrap && self.wrap_pending
    }

    pub fn auto_wrap(&self) -> bool {
        self.auto_wrap
    }

    /// DECAWM changes wrapping without moving the cursor or clearing its edge state.
    /// The internal edge flag also locates combining suffixes when wrapping is off.
    pub fn set_auto_wrap(&mut self, enabled: bool) {
        self.auto_wrap = enabled;
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

    /// Designate G0 (false) or G1 (true), without changing the invoked set.
    pub(crate) fn designate_character_set(&mut self, g1: bool, graphics: bool) {
        self.character_sets.graphics[usize::from(g1)] = graphics;
    }

    pub(crate) fn select_character_set(&mut self, g1: bool) {
        self.character_sets.active = usize::from(g1);
    }

    /// Translate the invoked character set and write using non-CJK Unicode widths.
    /// Controls are ignored. This does not implement grapheme-cluster shaping.
    pub fn print(&mut self, mut character: char) {
        if character.is_control() {
            return;
        }
        character = self.character_sets.translate(character);
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
                self.used[self.row] = self.used[self.row]
                    .max(index % self.columns + usize::from(self.cells[index].width));
            }
            return;
        }
        // The model supports one- and two-column scalars. A one-column screen
        // cannot hold a wide glyph; use a visible replacement instead.
        if width > 2 || width > self.columns {
            character = '\u{fffd}';
            width = 1;
        }
        if self.auto_wrap && (self.wrap_pending || self.column + width > self.columns) {
            if !self.wrap_pending {
                let start = self.row * self.columns + self.column;
                self.clear_range(start..(self.row + 1) * self.columns);
                self.used[self.row] = self.used[self.row].min(self.column);
            }
            self.column = 0;
            self.advance_line(true);
            self.wrap_pending = false;
        }
        // With wrapping off, a wide glyph that cannot fit is ignored rather
        // than leaving half a glyph or moving the cursor backward.
        if self.column + width > self.columns {
            return;
        }
        if self.insert_mode {
            let continued = self.continued[self.row];
            self.insert_characters(width);
            self.continued[self.row] = continued;
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
        self.used[self.row] = self.used[self.row].max(self.column + width);
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
        for row in range.start / self.columns..=(range.end - 1) / self.columns {
            let start = range.start.saturating_sub(row * self.columns);
            let end = (range.end - row * self.columns).min(self.columns);
            if blank.style != Style::default() {
                self.used[row] = self.used[row].max(end);
            } else if end >= self.used[row] {
                self.used[row] = self.used[row].min(start);
            }
        }
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

    /// Position in physical zero-based coordinates, clamped to the active bounds.
    /// Explicit movement cancels delayed wrapping and never scrolls.
    pub fn move_to(&mut self, row: usize, column: usize) {
        self.row = if self.origin_mode {
            row.clamp(self.scroll_region.0, self.scroll_region.1)
        } else {
            row.min(self.rows - 1)
        };
        self.column = column.min(self.columns - 1);
        self.wrap_pending = false;
    }

    /// Advance to the next tab stop, or the right edge, without erasing or wrapping.
    pub fn tab(&mut self) {
        self.tab_forward(1);
    }

    /// Set or clear the stop at the current column without changing cursor state.
    pub fn set_tab_stop(&mut self, enabled: bool) {
        self.tab_stops[self.column] = enabled;
    }

    pub fn clear_tab_stops(&mut self) {
        self.tab_stops.fill(false);
    }

    /// Stop search is bounded by screen width even for enormous counts.
    /// Zero counts are a no-op in the model; the parser handles protocol defaults.
    pub fn tab_forward(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let column = (self.column + 1..self.columns)
            .filter(|&column| self.tab_stops[column])
            .nth(count - 1)
            .unwrap_or(self.columns - 1);
        self.move_to(self.row, column);
    }

    pub fn tab_backward(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        let column = (0..self.column)
            .rev()
            .filter(|&column| self.tab_stops[column])
            .nth(count - 1)
            .unwrap_or(0);
        self.move_to(self.row, column);
    }

    pub fn move_up(&mut self, count: usize) {
        let top = if self.row >= self.scroll_region.0 {
            self.scroll_region.0
        } else {
            0
        };
        self.move_to(self.row.saturating_sub(count).max(top), self.column);
    }

    pub fn move_down(&mut self, count: usize) {
        let bottom = if self.row <= self.scroll_region.1 {
            self.scroll_region.1
        } else {
            self.rows - 1
        };
        self.move_to(self.row.saturating_add(count).min(bottom), self.column);
    }

    pub fn move_left(&mut self, count: usize) {
        self.move_to(self.row, self.column.saturating_sub(count));
    }

    pub fn move_right(&mut self, count: usize) {
        self.move_to(self.row, self.column.saturating_add(count));
    }

    /// Insert blank columns at the cursor, discarding content past the right edge.
    /// Coordinates stay unchanged; zero count is a no-op.
    pub fn insert_characters(&mut self, count: usize) {
        let count = count.min(self.columns - self.column);
        if count == 0 {
            return;
        }
        self.continued[self.row] = false;
        if self.row + 1 < self.rows {
            self.continued[self.row + 1] = false;
        }
        let start = self.row * self.columns + self.column;
        let end = (self.row + 1) * self.columns;
        // Inserting inside a wide glyph splits it: clear both halves first.
        if self.cells[start].width == 0 {
            self.clear_range(start..start + 1);
        }
        // The last retained column cannot be the first half of a wide glyph.
        let retained_end = end - count;
        if retained_end > start && self.cells[retained_end - 1].width == 2 {
            self.clear_range(retained_end - 1..retained_end);
        }
        let blank = self.blank();
        let used = self.used[self.row];
        self.used[self.row] = if used > self.column {
            (used + count).min(self.columns)
        } else {
            used
        };
        if blank.style != Style::default() {
            self.used[self.row] = self.used[self.row].max(self.column + count);
        }
        self.cells[start..end].rotate_right(count);
        self.cells[start..start + count].fill(blank);
        self.wrap_pending = false;
    }

    /// Delete columns at the cursor and shift the remainder left within this row.
    /// Split wide glyphs are blanked, but the shift still uses the requested columns.
    pub fn delete_characters(&mut self, count: usize) {
        let count = count.min(self.columns - self.column);
        if count == 0 {
            return;
        }
        self.continued[self.row] = false;
        if self.row + 1 < self.rows {
            self.continued[self.row + 1] = false;
        }
        let start = self.row * self.columns + self.column;
        let end = (self.row + 1) * self.columns;
        let used = self.used[self.row];
        self.clear_range(start..start + count);
        let blank = self.blank();
        self.used[self.row] = if blank.style != Style::default() {
            self.columns
        } else if used <= self.column {
            used
        } else if used <= self.column + count {
            self.used[self.row].min(self.column)
        } else {
            used - count
        };
        self.cells[start..end].rotate_left(count);
        self.cells[end - count..end].fill(blank);
        self.wrap_pending = false;
    }

    /// Blank columns without shifting text; touching half a wide glyph erases both.
    pub fn erase_characters(&mut self, count: usize) {
        let count = count.min(self.columns - self.column);
        if count == 0 {
            return;
        }
        self.continued[self.row] = false;
        if self.row + 1 < self.rows {
            self.continued[self.row + 1] = false;
        }
        let start = self.row * self.columns + self.column;
        self.clear_range(start..start + count);
        self.wrap_pending = false;
    }

    /// Blank part or all of the current row, including the cursor cell.
    /// Cursor coordinates stay unchanged; delayed wrapping is cancelled.
    pub fn erase_line(&mut self, mode: EraseMode) {
        self.continued[self.row] = false;
        if self.row + 1 < self.rows {
            self.continued[self.row + 1] = false;
        }
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
    /// Retained history and continuation links outside the erased rows are unchanged.
    pub fn erase_display(&mut self, mode: EraseMode) {
        let cursor = self.row * self.columns + self.column;
        let range = match mode {
            EraseMode::ToEnd => cursor..self.cells.len(),
            EraseMode::ToStart => 0..cursor + 1,
            EraseMode::All => 0..self.cells.len(),
        };
        // A continuation belongs to the boundary before its row. Sever links
        // into and out of erased rows, preserving unrelated wrapped output (for
        // example, above a prompt that clears the rest of the screen with ED).
        let first_row = range.start / self.columns;
        let after_last_row = (range.end - 1) / self.columns + 1;
        let flags_end = (after_last_row + 1).min(self.rows);
        self.continued[first_row..flags_end].fill(false);
        self.clear_range(range);
        self.wrap_pending = false;
    }

    /// Inclusive zero-based top and bottom rows for the active grid.
    pub fn scroll_region(&self) -> (usize, usize) {
        self.scroll_region
    }

    /// Set valid vertical margins and home the cursor at the active origin.
    /// Reversed, single-row or out-of-bounds regions leave all state unchanged,
    /// except that a one-row screen accepts its full-height region.
    pub fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        if bottom >= self.rows || top > bottom || (top == bottom && self.rows != 1) {
            return;
        }
        self.scroll_region = (top, bottom);
        self.position(0, 0);
    }

    /// Insert blank rows at the cursor through the bottom margin.
    /// Outside the region (or for zero count), leave all state unchanged.
    pub fn insert_lines(&mut self, count: usize) {
        if count == 0 || self.row < self.scroll_region.0 || self.row > self.scroll_region.1 {
            return;
        }
        self.shift_rows(self.row, self.scroll_region.1, count, true);
        self.move_to(self.row, 0);
    }

    /// Delete rows at the cursor, filling from the bottom with blank rows.
    pub fn delete_lines(&mut self, count: usize) {
        if count == 0 || self.row < self.scroll_region.0 || self.row > self.scroll_region.1 {
            return;
        }
        self.shift_rows(self.row, self.scroll_region.1, count, false);
        self.continued[self.row] = false;
        self.move_to(self.row, 0);
    }

    /// Scroll the entire region upward, retaining cursor coordinates.
    pub fn scroll_up(&mut self, count: usize) {
        if count != 0 {
            if !self.is_alternate() && self.scroll_region == (0, self.rows - 1) {
                let captured = count.min(self.rows);
                for (index, row) in self.cells[..captured * self.columns]
                    .chunks(self.columns)
                    .enumerate()
                {
                    self.scrollback
                        .push(row, self.continued[index], self.used[index]);
                }
                self.primary_scroll_count =
                    self.primary_scroll_count.saturating_add(captured as u64);
            }
            self.shift_rows(self.scroll_region.0, self.scroll_region.1, count, false);
            self.wrap_pending = false;
        }
    }

    /// Scroll the entire region downward, retaining cursor coordinates.
    pub fn scroll_down(&mut self, count: usize) {
        if count != 0 {
            self.shift_rows(self.scroll_region.0, self.scroll_region.1, count, true);
            self.wrap_pending = false;
        }
    }

    // Clamp before multiplication. Moving whole rows preserves wide-cell pairs
    // and moves combining suffix allocations without cloning or allocating.
    fn shift_rows(&mut self, top: usize, bottom: usize, count: usize, down: bool) {
        let lines = count.min(bottom - top + 1);
        let amount = lines * self.columns;
        let blank_used = if self.blank().style == Style::default() {
            0
        } else {
            self.columns
        };
        if down {
            self.used[top..=bottom].rotate_right(lines);
            self.used[top..top + lines].fill(blank_used);
        } else {
            self.used[top..=bottom].rotate_left(lines);
            self.used[bottom + 1 - lines..=bottom].fill(blank_used);
        }
        if down {
            self.continued[top..=bottom].rotate_right(lines);
            self.continued[top..top + lines].fill(false);
            if top + lines <= bottom {
                self.continued[top + lines] = false;
            }
        } else {
            self.continued[top..=bottom].rotate_left(lines);
            self.continued[bottom + 1 - lines..=bottom].fill(false);
            if top != 0 || bottom != self.rows - 1 {
                self.continued[top] = false;
            }
        }
        if bottom + 1 < self.rows {
            self.continued[bottom + 1] = false;
        }
        let start = top * self.columns;
        let end = (bottom + 1) * self.columns;
        let blank = self.blank();
        if down {
            self.cells[start..end].rotate_right(amount);
            self.cells[start..start + amount].fill(blank);
        } else {
            self.cells[start..end].rotate_left(amount);
            self.cells[end - amount..end].fill(blank);
        }
    }

    /// LF/IND: preserve the column and scroll only when at the bottom margin.
    /// Outside the region, move toward the physical bottom without scrolling.
    pub fn line_feed(&mut self) {
        self.advance_line(false);
    }

    fn advance_line(&mut self, continued: bool) {
        self.wrap_pending = false;
        if self.row == self.scroll_region.1 {
            self.scroll_up(1);
            self.continued[self.row] = continued;
        } else if self.row + 1 < self.rows {
            self.row += 1;
            self.continued[self.row] = continued;
        }
    }

    /// RI: preserve the column and scroll downward only at the top margin.
    pub fn reverse_index(&mut self) {
        self.wrap_pending = false;
        if self.row == self.scroll_region.0 {
            self.scroll_down(1);
        } else {
            self.row = self.row.saturating_sub(1);
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

    #[test]
    fn prompt_tail_keeps_physical_rows_while_completed_output_reflows() {
        let mut screen = Screen::new(6, 12).unwrap();
        screen.write_ascii(b"abcdefghijklmnop\r\n").unwrap();
        let prompt_start = screen.cursor();
        screen.write_ascii(b"\r\nSTATUS\r\n> ").unwrap();

        assert_eq!(
            screen.resize_preserving_tail(6, 6, prompt_start).unwrap(),
            (3, 0)
        );

        assert_eq!(
            lines(&screen),
            ["abcdef", "ghijkl", "mnop  ", "      ", "STATUS", ">     "]
        );
        assert_eq!(screen.cursor(), (5, 2));
    }
}
