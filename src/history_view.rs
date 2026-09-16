//! Read-only navigation over a frozen primary-screen snapshot.
use crate::screen::{MouseTracking, Screen};
use crate::style::Cell;
use base64::Engine;
use std::io;

const MAX_HISTORY_ESCAPE_BYTES: usize = 64;
// Encoded OSC 52 output stays below the terminal's 64 KiB input/IO budget.
const MAX_COPY_TEXT_BYTES: usize = 32 * 1024;
mod search;
use search::{Direction, Hit, QueryHistory, QueryInput};

#[derive(Clone, Copy)]
struct Selection {
    anchor: (usize, usize),
    cursor: (usize, usize),
}
fn base64(input: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .encode(input)
        .into_bytes()
}

fn osc52(text: &str) -> Option<Vec<u8>> {
    if text.len() > MAX_COPY_TEXT_BYTES {
        return None;
    }
    let encoded = base64(text);
    let mut sequence = b"\x1b]52;c;".to_vec();
    sequence.extend(encoded);
    sequence.push(7);
    Some(sequence)
}

fn ordered(first: (usize, usize), second: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

pub(crate) struct HistoryView {
    source: Screen,
    offset: usize,
    escape: Vec<u8>,
    discard_escape: bool,
    origin: (usize, usize),
    paste: bool,
    query: String,
    direction: Direction,
    editor: Option<QueryInput>,
    queries: QueryHistory,
    hits: Vec<Hit>,
    selected: Option<usize>,
    copy_pending: Option<Vec<u8>>,
    copy_too_large: bool,
    selection: Option<Selection>,
}

impl HistoryView {
    pub fn new(source: &Screen) -> Option<Self> {
        if source.is_alternate() || source.history_len() == 0 {
            return None;
        }
        Some(Self {
            source: source.clone(),
            offset: source.history_len().min(source.dimensions().0),
            escape: Vec::new(),
            discard_escape: false,
            origin: (0, 0),
            paste: false,
            query: String::new(),
            direction: Direction::Forward,
            editor: None,
            queries: QueryHistory::default(),
            hits: Vec::new(),
            selected: None,
            copy_pending: None,
            copy_too_large: false,
            selection: None,
        })
    }

    // Zero-based outer-terminal origin, including the window bar when present.
    pub fn set_origin(&mut self, row: usize, column: usize) {
        self.origin = (row, column);
    }

    pub fn take_copy(&mut self) -> Option<Vec<u8>> {
        self.copy_pending.take()
    }

    pub fn query_cursor(&self, columns: usize) -> Option<usize> {
        self.editor.as_ref().map(|editor| editor.display(columns).1)
    }

    pub fn label(&self, columns: usize) -> String {
        if let Some(editor) = &self.editor {
            return editor.label(columns);
        }
        if self.copy_too_large {
            return "Copy too large (32 KiB limit) · q:exit".to_owned();
        }
        if let Some(selection) = self.selection {
            let (start, end) = ordered(selection.anchor, selection.cursor);
            return format!(
                "Select {}:{}–{}:{} · arrows/hjkl:extend y:copy v:cancel",
                start.0 + 1,
                start.1 + 1,
                end.0 + 1,
                end.1 + 1
            );
        }
        let marker = self.direction.marker();
        let search = if self.query.is_empty() {
            String::new()
        } else if self.hits.is_empty() {
            format!(" · no match {marker}{}", self.query)
        } else {
            format!(
                " · {}/{} {marker}{}",
                self.selected.map_or(0, |index| index + 1),
                self.hits.len(),
                self.query
            )
        };
        format!(
            "History {}/{}{} · /?:search n/N:next/prev q:exit",
            self.offset,
            self.source.history_len(),
            search
        )
    }

    // Return true only on an explicit exit key. Consume escape sequences and paste
    // locally so their payload cannot become navigation or reach a child shell.
    pub fn feed(&mut self, byte: u8) -> bool {
        if self.discard_escape {
            self.discard_escape = !(0x40..=0x7e).contains(&byte);
            return false;
        }
        if !self.escape.is_empty() {
            self.escape.push(byte);
            if self.escape.len() == 2 && matches!(byte, b'[' | b'O') {
                return false;
            }
            // Legacy mouse reports have three raw payload bytes after CSI M.
            // Those bytes may themselves be navigation keys or CSI final bytes.
            if self.escape.starts_with(b"\x1b[M") {
                if self.escape.len() == 6 {
                    self.wheel();
                    self.escape.clear();
                }
                return false;
            }
            if self.escape.len() == 2 || (0x40..=0x7e).contains(&byte) {
                self.wheel();
                match self.escape.as_slice() {
                    b"\x1b[200~" => self.paste = true,
                    b"\x1b[201~" => self.paste = false,
                    b"\x1b[A" | b"\x1bOA" if !self.paste => {
                        if let Some(editor) = &mut self.editor {
                            editor.recall(&self.queries, true);
                        } else if self.selection.is_some() {
                            self.move_selection(0, -1);
                        } else {
                            self.up(1);
                        }
                    }
                    b"\x1b[B" | b"\x1bOB" if !self.paste => {
                        if let Some(editor) = &mut self.editor {
                            editor.recall(&self.queries, false);
                        } else if self.selection.is_some() {
                            self.move_selection(0, 1);
                        } else {
                            self.down(1);
                        }
                    }
                    b"\x1b[D" | b"\x1bOD" if !self.paste && self.selection.is_some() => {
                        self.move_selection(-1, 0)
                    }
                    b"\x1b[C" | b"\x1bOC" if !self.paste && self.selection.is_some() => {
                        self.move_selection(1, 0)
                    }
                    b"\x1b[5~" if !self.paste && self.editor.is_none() => {
                        if self.selection.is_some() {
                            self.move_selection(0, -(self.source.dimensions().0 as isize));
                        } else {
                            self.up(self.source.dimensions().0)
                        }
                    }
                    b"\x1b[6~" if !self.paste && self.editor.is_none() => {
                        if self.selection.is_some() {
                            self.move_selection(0, self.source.dimensions().0 as isize);
                        } else {
                            self.down(self.source.dimensions().0)
                        }
                    }
                    _ if !self.paste => {
                        if let Some(editor) = &mut self.editor {
                            editor.edit_sequence(&self.escape);
                        }
                    }
                    _ => {}
                }
                self.escape.clear();
            } else if self.escape.len() >= MAX_HISTORY_ESCAPE_BYTES {
                self.escape.clear();
                self.discard_escape = true;
            }
            return false;
        }
        if byte == 27 {
            if let Some(editor) = &mut self.editor {
                editor.feed(byte); // Discard an incomplete UTF-8 character.
            }
            self.escape.push(byte);
            return false;
        }
        if self.paste {
            if let Some(editor) = &mut self.editor {
                // Paste inserts text only. Controls (including newline, Delete
                // and editing shortcuts) must never execute editor commands.
                editor.feed_paste(byte);
            }
            return false;
        }
        if self.editor.is_some() {
            match byte {
                3 | 7 => self.editor = None,
                b'\r' | b'\n' => {
                    let editor = self.editor.take().unwrap();
                    self.query = editor.text;
                    self.queries.remember(&self.query);
                    self.direction = editor.direction;
                    self.hits = search::find(&self.source, &self.query);
                    let top = self.source.history_len() - self.offset;
                    // Both directions start at the viewport's top row. With no
                    // text cursor, forward chooses that row's first match and
                    // backward its last match. Wrap only when no candidate exists.
                    let index = match self.direction {
                        Direction::Forward => self
                            .hits
                            .iter()
                            .position(|hit| hit.start.0 >= top)
                            .unwrap_or(0),
                        Direction::Backward => self
                            .hits
                            .iter()
                            .rposition(|hit| hit.start.0 <= top)
                            .unwrap_or(self.hits.len().saturating_sub(1)),
                    };
                    self.select(index);
                }
                _ => self.editor.as_mut().unwrap().feed(byte),
            }
            return false;
        }
        if self.selection.is_some() {
            let height = self.source.dimensions().0.max(1) as isize;
            match byte {
                b'y' => {
                    self.copy_pending = self.copy_selection();
                    self.copy_too_large = self.copy_pending.is_none();
                    self.selection = None;
                }
                b'v' => self.selection = None,
                b'q' | 3 => return true,
                b'h' => self.move_selection(-1, 0),
                b'l' => self.move_selection(1, 0),
                b'k' => self.move_selection(0, -1),
                b'j' => self.move_selection(0, 1),
                21 => self.move_selection(0, -height),
                4 => self.move_selection(0, height),
                b'g' => self.move_selection_to_row(0),
                b'G' => self.move_selection_to_row(
                    self.source.history_len() + self.source.dimensions().0 - 1,
                ),
                _ => {}
            }
            return false;
        }
        self.copy_too_large = false;
        match byte {
            b'/' => self.editor = Some(QueryInput::new(Direction::Forward)),
            b'?' => self.editor = Some(QueryInput::new(Direction::Backward)),
            b'y' => {
                self.copy_pending = self.copy_sequence();
                self.copy_too_large = self.copy_pending.is_none();
            }
            b'v' => self.toggle_selection(),
            b'n' => self.next(false),
            b'N' => self.next(true),
            b'q' | 3 => return true,
            b'k' => self.up(1),
            b'j' => self.down(1),
            21 => self.up((self.source.dimensions().0 / 2).max(1)),
            4 => self.down((self.source.dimensions().0 / 2).max(1)),
            b'g' => self.offset = self.source.history_len(),
            b'G' => self.offset = 0,
            _ => {}
        }
        false
    }

    fn copy_sequence(&self) -> Option<Vec<u8>> {
        let (rows, columns) = self.source.dimensions();
        let history = self.source.history_len();
        let mut text = String::new();
        for row in 0..rows {
            if row > 0 {
                if text.len() == MAX_COPY_TEXT_BYTES {
                    return None;
                }
                text.push('\n');
            }
            let index = history - self.offset + row;
            let (cells, used) = if index < history {
                (
                    self.source.history_row(index).expect("snapshot row exists"),
                    self.source.history_row_used_columns(index).unwrap(),
                )
            } else {
                let screen_row = index - history;
                (
                    self.source.row(screen_row).expect("snapshot row exists"),
                    self.source.row_used_columns(screen_row).unwrap(),
                )
            };
            for (column, cell) in cells.iter().take(used.min(columns)).enumerate() {
                if cell.width == 0 {
                    continue;
                }
                // Rendering replaces a clipped wide glyph with blank padding.
                if cell.width == 2 && column + 1 == columns {
                    continue;
                }
                for character in std::iter::once(&cell.character).chain(&cell.combining) {
                    if text.len() + character.len_utf8() > MAX_COPY_TEXT_BYTES {
                        return None;
                    }
                    text.push(*character);
                }
            }
        }
        osc52(&text)
    }

    fn copy_selection(&self) -> Option<Vec<u8>> {
        let selection = self.selection?;
        let (start, end) = ordered(selection.anchor, selection.cursor);
        let width = self.source.dimensions().1;
        let mut text = String::new();
        for row in start.0..=end.0 {
            if row > start.0 && !self.row_data(row).2 {
                if text.len() == MAX_COPY_TEXT_BYTES {
                    return None;
                }
                text.push('\n');
            }
            let (cells, used, _) = self.row_data(row);
            let first = if row == start.0 { start.1 } else { 0 };
            let last = if row == end.0 {
                end.1
            } else {
                width.saturating_sub(1)
            };
            for (column, cell) in cells
                .iter()
                .enumerate()
                .take(used.min(width).min(last.saturating_add(1)))
                .skip(first)
            {
                if cell.width == 0 || (cell.width == 2 && column + 1 == width) {
                    continue;
                }
                for character in std::iter::once(&cell.character).chain(&cell.combining) {
                    if text.len() + character.len_utf8() > MAX_COPY_TEXT_BYTES {
                        return None;
                    }
                    text.push(*character);
                }
            }
        }
        osc52(&text)
    }

    fn row_data(&self, row: usize) -> (&[Cell], usize, bool) {
        let history = self.source.history_len();
        if row < history {
            (
                self.source.history_row(row).unwrap(),
                self.source.history_row_used_columns(row).unwrap(),
                self.source.history_row_continued(row).unwrap(),
            )
        } else {
            let row = row - history;
            (
                self.source.row(row).unwrap(),
                self.source.row_used_columns(row).unwrap(),
                self.source.row_continued(row).unwrap(),
            )
        }
    }

    fn toggle_selection(&mut self) {
        if self.selection.take().is_some() {
            return;
        }
        let row = self.source.history_len() - self.offset;
        let cursor = (row, self.first_cell(row).unwrap_or(0));
        self.selection = Some(Selection {
            anchor: cursor,
            cursor,
        });
    }

    fn move_selection(&mut self, columns: isize, rows: isize) {
        let Some(mut selection) = self.selection else {
            return;
        };
        let total_rows = self.source.history_len() + self.source.dimensions().0;
        if rows != 0 {
            selection.cursor.0 = selection
                .cursor
                .0
                .saturating_add_signed(rows)
                .min(total_rows.saturating_sub(1));
            selection.cursor.1 = self.cell_at_or_before(selection.cursor.0, selection.cursor.1);
        } else if columns < 0 {
            selection.cursor = self.previous_cell(selection.cursor);
        } else {
            selection.cursor = self.next_cell(selection.cursor);
        }
        self.selection = Some(selection);
        self.reveal(selection.cursor.0);
    }

    fn move_selection_to_row(&mut self, row: usize) {
        let Some(mut selection) = self.selection else {
            return;
        };
        selection.cursor = (row, self.cell_at_or_before(row, selection.cursor.1));
        self.selection = Some(selection);
        self.reveal(row);
    }

    fn first_cell(&self, row: usize) -> Option<usize> {
        let (cells, used, _) = self.row_data(row);
        cells
            .iter()
            .take(used.min(self.source.dimensions().1))
            .position(|cell| cell.width != 0)
    }

    fn last_cell(&self, row: usize) -> usize {
        let (cells, used, _) = self.row_data(row);
        cells
            .iter()
            .take(used.min(self.source.dimensions().1))
            .enumerate()
            .rev()
            .find(|(_, cell)| cell.width != 0)
            .map_or(0, |(column, _)| column)
    }

    fn cell_at_or_before(&self, row: usize, column: usize) -> usize {
        (0..=column.min(self.source.dimensions().1.saturating_sub(1)))
            .rev()
            .find(|&column| self.cell_is_base(row, column))
            .unwrap_or(0)
    }

    fn previous_cell(&self, (row, column): (usize, usize)) -> (usize, usize) {
        if let Some(column) = (0..column)
            .rev()
            .find(|&column| self.cell_is_base(row, column))
        {
            return (row, column);
        }
        if row == 0 {
            (0, self.first_cell(0).unwrap_or(0))
        } else {
            (row - 1, self.last_cell(row - 1))
        }
    }

    fn next_cell(&self, (row, column): (usize, usize)) -> (usize, usize) {
        let width = self.source.dimensions().1;
        if let Some(column) = (column + 1..width).find(|&column| self.cell_is_base(row, column)) {
            return (row, column);
        }
        let last_row = self.source.history_len() + self.source.dimensions().0 - 1;
        if row == last_row {
            (row, self.last_cell(row))
        } else {
            (row + 1, self.first_cell(row + 1).unwrap_or(0))
        }
    }

    fn cell_is_base(&self, row: usize, column: usize) -> bool {
        let (cells, used, _) = self.row_data(row);
        column < used.min(self.source.dimensions().1) && cells[column].width != 0
    }

    fn reveal(&mut self, row: usize) {
        let history = self.source.history_len();
        let height = self.source.dimensions().0;
        let top = history - self.offset;
        if row < top {
            self.offset = history - row;
        } else if row >= top + height {
            self.offset = history.saturating_sub(row + 1 - height);
        }
    }

    fn wheel(&mut self) {
        if self.paste || self.editor.is_some() || self.selection.is_some() {
            return;
        }
        let report = match self.escape.as_slice() {
            [27, b'[', b'M', button, column, row] => button
                .checked_sub(32)
                .zip(column.checked_sub(32))
                .zip(row.checked_sub(32))
                .map(|((button, column), row)| {
                    (usize::from(button), usize::from(column), usize::from(row))
                }),
            [27, b'[', b'<', rest @ .., b'M'] => std::str::from_utf8(rest).ok().and_then(|text| {
                let mut parts = text.split(';');
                let mut number = || {
                    let part = parts.next()?;
                    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    part.parse::<usize>().ok()
                };
                let report = (number()?, number()?, number()?);
                parts.next().is_none().then_some(report)
            }),
            _ => None,
        };
        let Some((button, column, row)) = report else {
            return;
        };
        let (rows, columns) = self.source.dimensions();
        let local_row = row.checked_sub(self.origin.0);
        let local_column = column.checked_sub(self.origin.1);
        if !local_row.is_some_and(|r| (1..=rows).contains(&r))
            || !local_column.is_some_and(|c| (1..=columns).contains(&c))
        {
            return;
        }
        // Ignore modifiers but reject motion, horizontal wheels and releases.
        match button & !0x1c {
            64 => self.up(3),
            65 => self.down(3),
            _ => {}
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = self.hits.get(index).map(|_| index);
        if let Some(hit) = self.hits.get(index) {
            self.offset = self.source.history_len().saturating_sub(hit.start.0);
        }
    }

    fn next(&mut self, reverse: bool) {
        let backwards = (self.direction == Direction::Backward) != reverse;
        if let Some(index) = self.selected {
            let count = self.hits.len();
            self.select(if backwards {
                (index + count - 1) % count
            } else {
                (index + 1) % count
            });
        }
    }

    fn up(&mut self, amount: usize) {
        self.copy_too_large = false;
        self.offset = self
            .offset
            .saturating_add(amount)
            .min(self.source.history_len());
    }

    fn down(&mut self, amount: usize) {
        self.copy_too_large = false;
        self.offset = self.offset.saturating_sub(amount);
    }

    pub fn render(&self) -> io::Result<Screen> {
        let (rows, columns) = self.source.dimensions();
        let mut view = Screen::new(rows, columns)?;
        view.set_cursor_visible(false);
        view.set_bracketed_paste(true);
        view.set_mouse_tracking(MouseTracking::Button);
        view.set_sgr_mouse(true);
        let history = self.source.history_len();
        for row in 0..rows {
            let index = history - self.offset + row;
            let cells = if index < history {
                self.source.history_row(index)
            } else {
                self.source.row(index - history)
            }
            .expect("snapshot row exists");
            let used = if index < history {
                self.source.history_row_used_columns(index)
            } else {
                self.source.row_used_columns(index - history)
            }
            .unwrap();
            for (column, cell) in cells.iter().take(columns).enumerate() {
                // Old history widths may differ. Never display half a wide glyph.
                if cell.width == 2 && column + 1 == columns {
                    continue;
                }
                let mut cell = cell.clone();
                let selected_by_range = self.selection.is_some_and(|selection| {
                    let (start, end) = ordered(selection.anchor, selection.cursor);
                    let position = if cell.width == 0 && column > 0 {
                        (index, column - 1)
                    } else {
                        (index, column)
                    };
                    start <= position && position <= end
                });
                let selected_by_search = self.selection.is_none()
                    && self.selected.is_some_and(|selected| {
                        let hit = self.hits[selected];
                        column < used && hit.start <= (index, column) && (index, column) < hit.end
                    });
                if selected_by_range || selected_by_search {
                    cell.style.inverse = !cell.style.inverse;
                }
                view.set_display_cell(row, column, cell);
            }
        }
        Ok(view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn snapshot_navigation_clips_wide_cells_and_does_not_follow_new_output() {
        let mut source = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut source, "A中B\r\nnext\r\nlast".as_bytes());
        source.resize_display(2, 2).unwrap();
        let mut view = HistoryView::new(&source).unwrap();
        source.reset();
        let first = view.render().unwrap();
        assert_eq!(first.row(0).unwrap()[0].character, 'A');
        assert_eq!(first.row(0).unwrap()[1].character, ' ');
        assert!(!first.cursor_visible());
        view.feed(b'G');
        assert_eq!(view.render().unwrap().row(0).unwrap()[0].character, 'n');
        view.feed(b'k');
        assert_eq!(view.render().unwrap(), first);
        for byte in b"\x1b[200~q\x03gGjk\x1b[201~" {
            assert!(!view.feed(*byte));
        }
        assert_eq!(view.render().unwrap(), first);
        for byte in b"\x1b[B" {
            assert!(!view.feed(*byte));
        }
        assert_eq!(view.offset, 0);
        for byte in b"\x1b[5~" {
            view.feed(*byte);
        }
        assert_eq!(view.offset, 1);
        assert!(view.feed(b'q'));
        assert!(HistoryView::new(&source).is_none());
    }

    #[test]
    fn y_copies_visible_rows_as_osc52_and_is_consumed_locally() {
        let mut source = Screen::new(2, 8).unwrap();
        Parser::new().advance(&mut source, "old\r\n中e\u{301}  \r\nnew".as_bytes());
        let mut view = HistoryView::new(&source).unwrap();
        assert!(!view.feed(b'y'));
        let sequence = view.take_copy().unwrap();
        assert!(sequence.starts_with(b"\x1b]52;c;"));
        assert_eq!(sequence.last(), Some(&7));
        assert!(view.take_copy().is_none());
        assert!(!view.feed(b"y"[0]));
        assert!(view.take_copy().is_some());
    }

    #[test]
    fn base64_encoding_matches_osc52_payload_rules() {
        assert_eq!(base64(""), b"");
        assert_eq!(base64("f"), b"Zg==");
        assert_eq!(base64("fo"), b"Zm8=");
        assert_eq!(base64("foo"), b"Zm9v");
        assert_eq!(base64("中\n"), b"5LitCg==");
    }

    #[test]
    fn copy_preserves_explicit_spaces_and_omits_padding_and_clipped_wide_glyphs() {
        let mut source = Screen::new(2, 8).unwrap();
        Parser::new().advance(&mut source, "中e\u{301}  \r\nnext\r\nlast".as_bytes());
        let mut view = HistoryView::new(&source).unwrap();
        let expected = [
            b"\x1b]52;c;".as_slice(),
            &base64("中e\u{301}  \nnext"),
            b"\x07",
        ]
        .concat();
        type_bytes(&mut view, b"y");
        assert_eq!(view.take_copy().unwrap(), expected);
        // The frozen history row retains its old width, but the viewport clips it.
        source.resize_display(2, 1).unwrap();
        let mut narrow = HistoryView::new(&source).unwrap();
        type_bytes(&mut narrow, b"y");
        assert_eq!(narrow.take_copy().unwrap(), b"\x1b]52;c;Cm4=\x07"); // "\nn"
        type_bytes(&mut view, b"/y\x03\x1b[200~yyy\x1b[201~");
        assert!(view.take_copy().is_none());
    }

    #[test]
    fn copy_rejects_oversized_views_without_emitting_partial_clipboard_data() {
        for rows in [254, 255] {
            let mut source = Screen::new(rows, 128).unwrap();
            let line = format!("{}\r\n", "x".repeat(128));
            Parser::new().advance(&mut source, line.repeat(rows + 1).as_bytes());
            let mut view = HistoryView::new(&source).unwrap();
            type_bytes(&mut view, b"gy");
            if rows == 254 {
                let sequence = view.take_copy().unwrap();
                assert!(sequence.len() < 64 * 1024);
            } else {
                assert!(view.take_copy().is_none());
                assert!(view.label(80).contains("Copy too large"));
                type_bytes(&mut view, b"j");
                assert!(!view.label(80).contains("Copy too large"));
            }
        }
    }

    #[test]
    fn selection_copy_joins_soft_wraps_and_preserves_hard_line_breaks() {
        let mut source = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut source, b"abcdEF\r\nhard\r\nend");
        let mut view = HistoryView::new(&source).unwrap();
        assert!(view.row_data(1).2); // EF continues the full abcd row.
        assert!(!view.row_data(2).2); // hard starts after an explicit CRLF.
        view.selection = Some(Selection {
            anchor: (0, 2),
            cursor: (1, 1),
        });
        assert_eq!(view.copy_selection().unwrap(), osc52("cdEF").unwrap());
        view.selection.as_mut().unwrap().cursor = (2, 1);
        assert_eq!(view.copy_selection().unwrap(), osc52("cdEF\nha").unwrap());
        view.selection = Some(Selection {
            anchor: (2, 1),
            cursor: (0, 2),
        });
        assert_eq!(view.copy_selection().unwrap(), osc52("cdEF\nha").unwrap());
    }

    #[test]
    fn selection_navigation_scrolls_and_never_lands_on_wide_placeholders() {
        let mut source = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut source, "A中B\r\nnext\r\nlast".as_bytes());
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"vll");
        let selection = view.selection.unwrap();
        assert_eq!(selection.anchor, (0, 0));
        assert_eq!(selection.cursor, (0, 3)); // l skips the wide placeholder.
        let rendered = view.render().unwrap();
        assert!(
            rendered.row(0).unwrap()[1..=2]
                .iter()
                .all(|cell| cell.style.inverse)
        );
        type_bytes(&mut view, b"jk");
        assert_ne!(view.selection.unwrap().cursor.1, 2);
        type_bytes(&mut view, b"G");
        let last = source.history_len() + source.dimensions().0 - 1;
        assert_eq!(view.selection.unwrap().cursor.0, last);
        assert_eq!(view.offset, 0);
        type_bytes(&mut view, b"g");
        assert_eq!(view.selection.unwrap().cursor.0, 0);
        assert_eq!(view.offset, source.history_len());
        type_bytes(&mut view, b"v");
        assert!(view.selection.is_none());
    }

    #[test]
    fn selection_y_copies_and_returns_to_viewport_copy_mode() {
        let mut source = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut source, b"abcd\r\nnext\r\nlast");
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"vly");
        assert_eq!(view.take_copy().unwrap(), osc52("ab").unwrap());
        assert!(view.selection.is_none());
        type_bytes(&mut view, b"y");
        assert!(view.take_copy().is_some());
        type_bytes(&mut view, b"vy");
        assert_eq!(view.take_copy().unwrap(), osc52("a").unwrap());
        type_bytes(&mut view, b"v/");
        assert!(view.editor.is_none());
        assert!(view.selection.is_some());
        assert!(view.feed(b'q'));
    }

    #[test]
    fn wheel_scrolls_only_inside_the_pane_and_clamps_at_both_ends() {
        let mut source = Screen::new(4, 12).unwrap();
        let mut parser = Parser::new();
        for _ in 0..20 {
            parser.advance(&mut source, b"row\r\n");
        }
        let mut view = HistoryView::new(&source).unwrap();
        view.set_origin(5, 40);
        let rendered = view.render().unwrap();
        assert_eq!(rendered.mouse_tracking(), MouseTracking::Button);
        assert!(rendered.sgr_mouse());
        assert_eq!(source.mouse_tracking(), MouseTracking::Off);
        type_bytes(&mut view, b"G\x1b[<64;41;6M");
        assert_eq!(view.offset, 3);
        type_bytes(&mut view, b"\x1b[<80;52;9M"); // Ctrl + wheel, last cell.
        assert_eq!(view.offset, 6);
        for report in [
            &b"\x1b[<64;40;6M"[..],
            b"\x1b[<64;53;6M",
            b"\x1b[<64;41;5M",
            b"\x1b[<64;41;10M",
            b"\x1b[<64;0;0M",
            b"\x1b[<64;41;6m",
            b"\x1b[<0;41;6M",
            b"\x1b[<96;41;6M",
            b"\x1b[<66;41;6M",
            b"\x1b[<64;999999999999999999999999;6M",
            b"\x1b[<64;;6M",
            b"\x1b[<64;41;6;1M",
        ] {
            type_bytes(&mut view, report);
            assert_eq!(view.offset, 6);
        }
        type_bytes(&mut view, b"\x1b[M`I&"); // Legacy wheel up, column 41, row 6.
        assert_eq!(view.offset, 9);
        type_bytes(&mut view, b"g\x1b[<64;41;6M");
        assert_eq!(view.offset, source.history_len());
        type_bytes(&mut view, b"G\x1b[<65;41;6M");
        assert_eq!(view.offset, 0);
        view.set_origin(0, 0); // No bar and no split offset.
        type_bytes(&mut view, b"\x1b[<64;1;1M");
        assert_eq!(view.offset, 3);
    }

    #[test]
    fn mouse_payload_never_becomes_navigation_or_search_text() {
        let mut source = Screen::new(4, 12).unwrap();
        Parser::new().advance(&mut source, b"a\r\nb\r\nc\r\nd\r\ne\r\nf");
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"G\x1b[Mqjk");
        assert_eq!(view.offset, 0);
        type_bytes(&mut view, b"\x1b[200~\x1b[<64;1;1M\x1b[Mqjk\x1b[201~");
        assert_eq!(view.offset, 0);
        type_bytes(&mut view, b"/find\x1b[<64;1;1M\x1b[Mqjk");
        assert_eq!(view.editor.as_ref().unwrap().text, "find");
        assert_eq!(view.offset, 0);
        type_bytes(&mut view, b"\x03");
        let mut oversized = b"\x1b[<64;".to_vec();
        oversized.extend(std::iter::repeat_n(b'9', 100));
        oversized.extend_from_slice(b";1M");
        type_bytes(&mut view, &oversized);
        assert_eq!(view.offset, 0);
        type_bytes(&mut view, b"\x1b[<64;1;1M");
        assert_eq!(view.offset, source.history_len());
    }

    fn type_bytes(view: &mut HistoryView, text: &[u8]) {
        for &byte in text {
            assert!(!view.feed(byte));
        }
    }

    #[test]
    fn search_wraps_results_cancels_edits_and_leaves_snapshot_unchanged() {
        let mut source = Screen::new(3, 4).unwrap();
        Parser::new().advance(&mut source, b"abXX\r\ncdXX\r\nefXX\r\nlast");
        let original = source.clone();
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"g/XX\r");
        assert_eq!(view.selected, Some(0));
        assert_eq!(view.hits.len(), 3);
        let rendered = view.render().unwrap();
        assert!(!rendered.row(0).unwrap()[1].style.inverse);
        assert!(rendered.row(0).unwrap()[2].style.inverse);
        type_bytes(&mut view, b"N");
        assert_eq!(view.selected, Some(2));
        type_bytes(&mut view, b"n");
        assert_eq!(view.selected, Some(0));
        type_bytes(&mut view, b"/qjk\x03");
        assert!(view.editor.is_none());
        assert_eq!(view.query, "XX");
        type_bytes(&mut view, b"/missing\r");
        let offset = view.offset;
        assert!(view.label(80).contains("no match"));
        type_bytes(&mut view, b"nN");
        assert_eq!(view.offset, offset);
        type_bytes(&mut view, b"/\r");
        assert!(view.query.is_empty());
        assert_eq!(view.source, original);
        assert_eq!(source, original);
        assert!(view.feed(b'q'));
    }

    #[test]
    fn backward_search_anchors_at_top_row_and_repeats_in_search_direction() {
        let mut source = Screen::new(2, 8).unwrap();
        Parser::new().advance(&mut source, b"none\r\nXX XX\r\nplain\r\nXX\r\nend");
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"g?XX\r");
        assert_eq!(view.selected, Some(2)); // No older match: wrap to newest.
        assert!(view.label(80).contains("3/3 ?XX"));
        type_bytes(&mut view, b"n");
        assert_eq!(view.selected, Some(1));
        assert!(view.render().unwrap().row(0).unwrap()[3].style.inverse);
        type_bytes(&mut view, b"n");
        assert_eq!(view.selected, Some(0));
        type_bytes(&mut view, b"n");
        assert_eq!(view.selected, Some(2));
        type_bytes(&mut view, b"N");
        assert_eq!(view.selected, Some(0));
        view.offset = source.history_len() - 1;
        type_bytes(&mut view, b"?XX\r");
        assert_eq!(view.selected, Some(1)); // Rightmost match on the top row.
        type_bytes(&mut view, b"/XX\r");
        assert_eq!(view.selected, Some(0)); // Forward chooses the first.
        type_bytes(&mut view, b"n");
        assert_eq!(view.selected, Some(1));
        type_bytes(&mut view, b"G?XX\r");
        assert_eq!(view.selected, Some(2));
    }

    #[test]
    fn cancelled_search_keeps_direction_and_empty_or_missing_queries_are_safe() {
        let mut source = Screen::new(2, 8).unwrap();
        Parser::new().advance(&mut source, b"XX\r\nXX\r\nXX\r\nend");
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"G?XX\r");
        assert_eq!(view.selected, Some(2));
        type_bytes(&mut view, b"/other\x03n");
        assert_eq!(view.query, "XX");
        assert_eq!(view.direction, Direction::Backward);
        assert_eq!(view.selected, Some(1));
        type_bytes(&mut view, b"/other\x07n");
        assert_eq!(view.selected, Some(0));
        type_bytes(&mut view, b"\x1b[200~?other\r\x1b[201~");
        assert!(view.editor.is_none());
        assert_eq!(view.query, "XX");
        type_bytes(&mut view, b"?qjk/?\x1b[5~\x1b[<64;1;1M");
        assert_eq!(view.editor.as_ref().unwrap().text, "qjk/?");
        assert!(view.label(80).starts_with("Search ?qjk/?"));
        type_bytes(&mut view, b"\x15missing\r");
        assert!(view.label(80).contains("no match ?missing"));
        let offset = view.offset;
        type_bytes(&mut view, b"nN?\rnN");
        assert_eq!(view.offset, offset);
        assert!(view.query.is_empty());
        assert!(view.hits.is_empty());
        assert_eq!(view.selected, None);
        type_bytes(&mut view, b"?end\rnN");
        assert_eq!(view.selected, Some(0)); // One hit remains stable.
    }

    #[test]
    fn query_recall_is_modal_preserves_direction_and_records_only_submissions() {
        let mut source = Screen::new(2, 8).unwrap();
        Parser::new().advance(&mut source, b"one\r\ntwo\r\nend");
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"/one\r?two\r/cancelled\x03/\r");
        let offset = view.offset;
        type_bytes(&mut view, b"?draft\x1b[A");
        assert_eq!(view.editor.as_ref().unwrap().text, "two");
        assert_eq!(view.offset, offset);
        assert_eq!(view.selected, None); // Recall alone does not search.
        type_bytes(&mut view, b"\x1bOA");
        assert_eq!(view.editor.as_ref().unwrap().text, "one");
        type_bytes(&mut view, b"\x1b[200~\x1b[B\r\x1b[201~");
        assert_eq!(view.editor.as_ref().unwrap().text, "one");
        type_bytes(&mut view, b"\x1bOB\x1b[B");
        assert_eq!(view.editor.as_ref().unwrap().text, "draft");
        type_bytes(&mut view, b"\x1b[A\x1b[A\r");
        assert_eq!(view.query, "one");
        assert_eq!(view.direction, Direction::Backward);
        assert_eq!(view.hits.len(), 1);
        let mut reopened = HistoryView::new(&source).unwrap();
        type_bytes(&mut reopened, b"/\x1b[A");
        assert_eq!(reopened.editor.as_ref().unwrap().text, "");
    }

    #[test]
    fn query_editing_sequences_stay_modal_and_paste_cannot_move_cursor() {
        let mut source = Screen::new(2, 10).unwrap();
        Parser::new().advance(&mut source, b"one\r\ntwo\r\nend");
        let mut view = HistoryView::new(&source).unwrap();
        assert_eq!(view.query_cursor(20), None);
        type_bytes(&mut view, b"/onx\x1b[D\x1b[3~e");
        assert_eq!(view.editor.as_ref().unwrap().text, "one");
        type_bytes(&mut view, b"\x1b[200~\x1b[H\x01\x02\x04\x1b[3~\x1b[201~");
        assert_eq!(view.query_cursor(20), Some(11));
        type_bytes(&mut view, b"\r");
        assert_eq!(view.query_cursor(20), None);
        assert_eq!(view.query, "one");
        assert_eq!(view.hits.len(), 1);
    }

    #[test]
    fn word_and_line_deletion_respect_unicode_cursor_boundaries() {
        let mut source = Screen::new(2, 12).unwrap();
        Parser::new().advance(&mut source, b"one\r\ntwo\r\nend");
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, "/one 中文 two".as_bytes());
        type_bytes(&mut view, b"\x17");
        assert_eq!(view.editor.as_ref().unwrap().text, "one 中文");
        type_bytes(&mut view, b"\x17");
        assert_eq!(view.editor.as_ref().unwrap().text, "one");
        type_bytes(&mut view, b"\x01\x0b");
        assert_eq!(view.editor.as_ref().unwrap().text, "");
        type_bytes(&mut view, "one 中文 two".as_bytes());
        type_bytes(&mut view, b"\x01\x06\x17");
        assert_eq!(view.editor.as_ref().unwrap().text, "ne 中文 two");
        type_bytes(&mut view, b"\x05\x17");
        assert_eq!(view.editor.as_ref().unwrap().text, "ne 中文");
        type_bytes(&mut view, b"\x0b");
        assert_eq!(view.editor.as_ref().unwrap().text, "ne 中文");
    }

    #[test]
    fn pasted_query_inserts_at_cursor_without_submitting_or_running_controls() {
        let mut source = Screen::new(2, 12).unwrap();
        Parser::new().advance(&mut source, "A中B\r\nnext\r\nend".as_bytes());
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"/AB\x1b[D\x1b[200~");
        type_bytes(&mut view, "中".as_bytes());
        type_bytes(
            &mut view,
            b"\r\n\t\x01\x02\x03\x04\x05\x06\x07\x08\x15\x7f\x1b[H\x1b[3~\x1b[201~",
        );
        assert_eq!(view.editor.as_ref().unwrap().text, "A中B");
        assert!(view.query.is_empty());
        assert!(view.hits.is_empty());
        type_bytes(&mut view, b"\r");
        assert_eq!(view.query, "A中B");
        assert_eq!(view.hits.len(), 1);
        type_bytes(&mut view, b"?\x1b[A\x1b[200~qjk/?\x1b[201~");
        assert_eq!(view.editor.as_ref().unwrap().text, "A中Bqjk/?");
        type_bytes(&mut view, b"\x03/\x1b[A");
        assert_eq!(view.editor.as_ref().unwrap().text, "A中B");
    }

    #[test]
    fn oversized_unicode_paste_is_bounded_and_incomplete_text_stays_local() {
        let mut source = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut source, b"a\r\nb\r\nc");
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, b"/\x1b[200~");
        type_bytes(&mut view, "中".repeat(1000).as_bytes());
        type_bytes(&mut view, b"abcd\xe4\x1b[201~");
        assert_eq!(
            view.editor.as_ref().unwrap().text,
            format!("{}ab", "中".repeat(42))
        );
        type_bytes(
            &mut view,
            b"\x15\x1b[200~\xe4\r\xb8\xadX\xe4\x1b[201~\xb8\xad",
        );
        assert_eq!(view.editor.as_ref().unwrap().text, "X");
        type_bytes(&mut view, b"\r");
        assert_eq!(view.query, "X");
        assert_eq!(view.query_cursor(20), None);
    }

    #[test]
    fn query_controls_and_paste_cannot_navigate_or_exit_history() {
        let mut source = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut source, b"one\r\ntwo\r\nend");
        let mut view = HistoryView::new(&source).unwrap();
        let offset = view.offset;
        type_bytes(&mut view, b"/qjk\x1b[A\x1b[6~\x1b[200~bad\r\x03\x1b[201~");
        assert_eq!(view.editor.as_ref().unwrap().text, "qjkbad");
        assert_eq!(view.offset, offset);
        type_bytes(&mut view, b"\x15two\r");
        assert_eq!(view.hits.len(), 1);
        assert_eq!(view.hits[0].start, (1, 0));
    }

    #[test]
    fn matching_wide_and_combining_cells_highlights_the_whole_glyph() {
        let mut source = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut source, "中e\u{301}\r\nnext\r\nlast".as_bytes());
        let mut view = HistoryView::new(&source).unwrap();
        type_bytes(&mut view, "/中e\u{301}\r".as_bytes());
        let rendered = view.render().unwrap();
        let row = rendered.row(0).unwrap();
        assert!(row[..3].iter().all(|cell| cell.style.inverse));
        assert!(!row[3].style.inverse);
        assert_eq!(row[1].width, 0);
        assert_eq!(row[2].combining, vec!['\u{301}']);
    }
}
