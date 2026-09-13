//! Read-only navigation over a frozen primary-screen snapshot.
use crate::screen::Screen;
use std::io;

const MAX_HISTORY_ESCAPE_BYTES: usize = 64;

pub(crate) struct HistoryView {
    source: Screen,
    offset: usize,
    escape: Vec<u8>,
    paste: bool,
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
        })
    }

    pub fn label(&self) -> String {
        format!(
            "History {}/{} · q:exit",
            self.offset,
            self.source.history_len()
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
                    b"\x1b[A" | b"\x1bOA" if !self.paste => self.up(1),
                    b"\x1b[B" | b"\x1bOB" if !self.paste => self.down(1),
                    b"\x1b[5~" if !self.paste => self.up(self.source.dimensions().0),
                    b"\x1b[6~" if !self.paste => self.down(self.source.dimensions().0),
                    _ => {}
                }
                self.escape.clear();
            }
            return false;
        }
        if byte == 27 {
            self.escape.push(byte);
            return false;
        }
        if self.paste {
            return false;
        }
        match byte {
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
            for (column, cell) in cells.iter().take(columns).enumerate() {
                // Old history widths may differ. Never display half a wide glyph.
                if cell.width == 2 && column + 1 == columns {
                    continue;
                }
                view.set_display_cell(row, column, cell.clone());
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
        source.resize(2, 2).unwrap();
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
}
