//! Read-only navigation over a frozen primary-screen snapshot.
use crate::screen::Screen;
use std::io;

const MAX_HISTORY_ESCAPE_BYTES: usize = 64;
mod search;
use search::{Hit, QueryInput};

pub(crate) struct HistoryView {
    source: Screen,
    offset: usize,
    escape: Vec<u8>,
    paste: bool,
    query: String,
    editor: Option<QueryInput>,
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
            paste: false,
            query: String::new(),
            editor: None,
            hits: Vec::new(),
            selected: None,
        })
    }

    pub fn label(&self, columns: usize) -> String {
        if let Some(editor) = &self.editor {
            return editor.label(columns);
        }
        let search = if self.query.is_empty() {
            String::new()
        } else if self.hits.is_empty() {
            format!(" · no match /{}", self.query)
        } else {
            format!(
                " · {}/{} /{}",
                self.selected.map_or(0, |index| index + 1),
                self.hits.len(),
                self.query
            )
        };
        format!(
            "History {}/{}{} · /:search n/N:next/prev q:exit",
            self.offset,
            self.source.history_len(),
            search
        )
    }

    // Return true only on an explicit exit key. Consume escape sequences and paste
    // locally so their payload cannot become navigation or reach a child shell.
    pub fn feed(&mut self, byte: u8) -> bool {
        if !self.escape.is_empty() {
            self.escape.push(byte);
            if self.escape.len() == 2 && matches!(byte, b'[' | b'O') {
                return false;
            }
            if self.escape.len() == 2 || (0x40..=0x7e).contains(&byte) {
                match self.escape.as_slice() {
                    b"\x1b[200~" => self.paste = true,
                    b"\x1b[201~" => self.paste = false,
                    b"\x1b[A" | b"\x1bOA" if !self.paste && self.editor.is_none() => self.up(1),
                    b"\x1b[B" | b"\x1bOB" if !self.paste && self.editor.is_none() => self.down(1),
                    b"\x1b[5~" if !self.paste && self.editor.is_none() => {
                        self.up(self.source.dimensions().0)
                    }
                    b"\x1b[6~" if !self.paste && self.editor.is_none() => {
                        self.down(self.source.dimensions().0)
                    }
                    _ => {}
                }
                self.escape.clear();
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
                    self.query = self.editor.take().unwrap().text;
                    self.hits = search::find(&self.source, &self.query);
                    let top = self.source.history_len() - self.offset;
                    let index = self
                        .hits
                        .iter()
                        .position(|hit| hit.start.0 >= top)
                        .unwrap_or(0);
                    self.select(index);
                }
                _ => self.editor.as_mut().unwrap().feed(byte),
            }
            return false;
        }
        match byte {
            b'/' => self.editor = Some(QueryInput::default()),
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

    fn select(&mut self, index: usize) {
        self.selected = self.hits.get(index).map(|_| index);
        if let Some(hit) = self.hits.get(index) {
            self.offset = self.source.history_len().saturating_sub(hit.start.0);
        }
    }

    fn next(&mut self, backwards: bool) {
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
