//! Read-only navigation over a frozen primary-screen snapshot.
use crate::screen::{MouseTracking, Screen};
use std::io;

const MAX_HISTORY_ESCAPE_BYTES: usize = 64;
mod search;
use search::{Direction, Hit, QueryHistory, QueryInput};

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
        })
    }

    // Zero-based outer-terminal origin, including the window bar when present.
    pub fn set_origin(&mut self, row: usize, column: usize) {
        self.origin = (row, column);
    }

    pub fn label(&self, columns: usize) -> String {
        if let Some(editor) = &self.editor {
            return editor.label(columns);
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
                        } else {
                            self.up(1);
                        }
                    }
                    b"\x1b[B" | b"\x1bOB" if !self.paste => {
                        if let Some(editor) = &mut self.editor {
                            editor.recall(&self.queries, false);
                        } else {
                            self.down(1);
                        }
                    }
                    b"\x1b[5~" if !self.paste && self.editor.is_none() => {
                        self.up(self.source.dimensions().0)
                    }
                    b"\x1b[6~" if !self.paste && self.editor.is_none() => {
                        self.down(self.source.dimensions().0)
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
        match byte {
            b'/' => self.editor = Some(QueryInput::new(Direction::Forward)),
            b'?' => self.editor = Some(QueryInput::new(Direction::Backward)),
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

    fn wheel(&mut self) {
        if self.paste || self.editor.is_some() {
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
        self.offset = self
            .offset
            .saturating_add(amount)
            .min(self.source.history_len());
    }

    fn down(&mut self, amount: usize) {
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
                if self.selected.is_some_and(|selected| {
                    let hit = self.hits[selected];
                    column < used && hit.start <= (index, column) && (index, column) < hit.end
                }) {
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
    fn query_controls_and_paste_cannot_navigate_or_exit_history() {
        let mut source = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut source, b"one\r\ntwo\r\nend");
        let mut view = HistoryView::new(&source).unwrap();
        let offset = view.offset;
        type_bytes(&mut view, b"/qjk\x1b[A\x1b[6~\x1b[200~bad\r\x03\x1b[201~");
        assert_eq!(view.editor.as_ref().unwrap().text, "qjk");
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
