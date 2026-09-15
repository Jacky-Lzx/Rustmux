//! Literal search over logical lines, with UTF-8 byte offsets mapped to cells.
use crate::screen::Screen;
use unicode_width::UnicodeWidthChar;

pub(super) const MAX_QUERY_BYTES: usize = 128;
const CONTROL_BACKSPACE: u8 = 8;
const CONTROL_DELETE: u8 = 127;
const CONTROL_CLEAR_LINE: u8 = 21;
const CONTROL_BYTE_START: u8 = 0;
const CONTROL_BYTE_END: u8 = 31;
const PRINTABLE_BYTE_START: u8 = 32;
const PRINTABLE_BYTE_END: u8 = 126;
pub(super) const MAX_QUERY_HISTORY: usize = 20;
const KEY_HOME: u8 = 1;
const KEY_LEFT: u8 = 2;
const KEY_DELETE: u8 = 4;
const KEY_END: u8 = 5;
const KEY_RIGHT: u8 = 6;
const CONTROL_KILL_LINE: u8 = 11;
const CONTROL_DELETE_PREVIOUS_WORD: u8 = 23;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Hit {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

struct Span {
    start: usize,
    end: usize,
    hit: Hit,
}

pub(super) fn find(source: &Screen, query: &str) -> Vec<Hit> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut text = String::new();
    let mut spans = Vec::new();
    let history = source.history_len();
    for row in 0..history + source.dimensions().0 {
        let (cells, used, continued) = if row < history {
            (
                source.history_row(row).unwrap(),
                source.history_row_used_columns(row).unwrap(),
                source.history_row_continued(row).unwrap(),
            )
        } else {
            let index = row - history;
            (
                source.row(index).unwrap(),
                source.row_used_columns(index).unwrap(),
                source.row_continued(index).unwrap(),
            )
        };
        if row > 0 && !continued {
            text.push('\n');
        }
        for (column, cell) in cells.iter().take(used).enumerate() {
            if cell.width == 0 {
                continue;
            }
            let start = text.len();
            text.push(cell.character);
            text.extend(cell.combining.iter());
            spans.push(Span {
                start,
                end: text.len(),
                hit: Hit {
                    start: (row, column),
                    end: (row, column + usize::from(cell.width)),
                },
            });
        }
    }
    // Check character boundaries so overlapping matches are retained. The query
    // editor excludes controls, so matches cannot cross the hard-line separator.
    text.char_indices()
        .filter(|(start, _)| text[*start..].starts_with(query))
        .map(|(start, _)| {
            let first = spans.partition_point(|span| span.end <= start);
            let last = spans.partition_point(|span| span.start < start + query.len()) - 1;
            Hit {
                start: spans[first].hit.start,
                end: spans[last].hit.end,
            }
        })
        .collect()
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    #[default]
    Forward,
    Backward,
}

impl Direction {
    pub fn marker(self) -> char {
        match self {
            Self::Forward => '/',
            Self::Backward => '?',
        }
    }
}

// Oldest first. Search text is already limited to 128 UTF-8 bytes by the editor.
#[derive(Default)]
pub(super) struct QueryHistory(Vec<String>);

impl QueryHistory {
    pub fn remember(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }
        self.0.retain(|previous| previous != query);
        self.0.push(query.to_owned());
        if self.0.len() > MAX_QUERY_HISTORY {
            self.0.remove(0);
        }
    }
}

#[derive(Default)]
pub(super) struct QueryInput {
    pub text: String,
    pub direction: Direction,
    utf8: Vec<u8>,
    recalled: Option<usize>,
    draft: String,
    cursor: usize,
    draft_cursor: usize,
}

impl QueryInput {
    pub fn new(direction: Direction) -> Self {
        Self {
            direction,
            ..Self::default()
        }
    }

    pub fn recall(&mut self, history: &QueryHistory, older: bool) {
        self.utf8.clear();
        if history.0.is_empty() {
            return;
        }
        if older {
            let index = if let Some(index) = self.recalled {
                index.saturating_sub(1)
            } else {
                self.draft = self.text.clone();
                self.draft_cursor = self.cursor;
                history.0.len() - 1
            };
            self.text.clone_from(&history.0[index]);
            self.recalled = Some(index);
            self.cursor = self.text.len();
        } else if let Some(index) = self.recalled {
            if index + 1 < history.0.len() {
                self.text.clone_from(&history.0[index + 1]);
                self.recalled = Some(index + 1);
                self.cursor = self.text.len();
            } else {
                self.text = std::mem::take(&mut self.draft);
                self.cursor = self.draft_cursor;
                self.recalled = None;
            }
        }
    }

    pub fn label(&self, columns: usize) -> String {
        self.display(columns).0
    }

    pub fn display(&self, columns: usize) -> (String, usize) {
        let marker = self.direction.marker();
        let prefix = if columns >= 9 {
            format!("Search {marker}")
        } else if columns > 1 {
            marker.to_string()
        } else {
            String::new()
        };
        // Reserve one cell for the insertion cursor, even at the end of text.
        let mut remaining = columns.saturating_sub(prefix.len() + 1);
        let mut start = self.cursor;
        let mut before_width = 0;
        for (index, character) in self.text[..self.cursor].char_indices().rev() {
            let width = character.width().unwrap_or(0);
            if width > remaining {
                break;
            }
            remaining -= width;
            before_width += width;
            start = index;
        }
        let tail: String = self.text[start..]
            .chars()
            .skip_while(|character| character.width() == Some(0))
            .collect();
        (
            format!("{prefix}{tail} · Up/Down:recall Enter:find Ctrl-C:cancel"),
            (prefix.len() + before_width).min(columns.saturating_sub(1)),
        )
    }

    pub fn edit_sequence(&mut self, sequence: &[u8]) {
        match sequence {
            b"\x1b\x7f" | b"\x1b\x08" => self.feed(CONTROL_DELETE_PREVIOUS_WORD),
            b"\x1bd" => self.delete_next_word(),
            b"\x1b[D" | b"\x1bOD" => self.feed(KEY_LEFT),
            b"\x1b[C" | b"\x1bOC" => self.feed(KEY_RIGHT),
            b"\x1b[1;5D" | b"\x1b[5D" => self.move_word_left(),
            b"\x1b[1;5C" | b"\x1b[5C" => self.move_word_right(),
            b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" | b"\x1b[7~" => self.feed(KEY_HOME),
            b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => self.feed(KEY_END),
            b"\x1b[3~" => self.feed(KEY_DELETE),
            _ => {}
        }
    }

    pub fn feed_paste(&mut self, byte: u8) {
        if byte >= 32 && byte != 127 {
            self.feed(byte);
        } else {
            self.utf8.clear();
        }
    }

    pub fn feed(&mut self, byte: u8) {
        let old_len = self.text.len();
        match byte {
            KEY_HOME | KEY_LEFT | KEY_END | KEY_RIGHT => {
                self.utf8.clear();
                self.cursor = match byte {
                    KEY_HOME => 0,
                    KEY_END => self.text.len(),
                    KEY_LEFT => self.text[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i),
                    _ => {
                        self.cursor
                            + self.text[self.cursor..]
                                .chars()
                                .next()
                                .map_or(0, char::len_utf8)
                    }
                };
            }
            KEY_DELETE => {
                self.utf8.clear();
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            CONTROL_KILL_LINE => {
                self.utf8.clear();
                self.text.truncate(self.cursor);
            }
            CONTROL_DELETE_PREVIOUS_WORD => {
                self.utf8.clear();
                self.delete_previous_word();
            }
            CONTROL_BACKSPACE | CONTROL_DELETE => {
                self.utf8.clear();
                if self.cursor > 0 {
                    self.cursor = self.text[..self.cursor]
                        .char_indices()
                        .next_back()
                        .unwrap()
                        .0;
                    self.text.remove(self.cursor);
                }
            }
            CONTROL_CLEAR_LINE => {
                self.utf8.clear();
                self.text.clear();
                self.cursor = 0;
            }
            CONTROL_BYTE_START..=CONTROL_BYTE_END => self.utf8.clear(),
            PRINTABLE_BYTE_START..=PRINTABLE_BYTE_END => {
                self.utf8.clear();
                self.append(char::from(byte));
            }
            _ => {
                self.utf8.push(byte);
                match std::str::from_utf8(&self.utf8) {
                    Ok(text) => {
                        let character = text.chars().next().unwrap();
                        self.append(character);
                        self.utf8.clear();
                    }
                    Err(error) if error.error_len().is_some() => self.utf8.clear(),
                    Err(_) => {}
                }
            }
        }
        // Editing a recalled query makes it a new draft. Stored entries stay
        // unchanged; subsequent Up starts again at the most recent submission.
        if self.text.len() != old_len {
            self.recalled = None;
            self.draft.clear();
        }
    }

    fn append(&mut self, character: char) {
        if !character.is_control() && self.text.len() + character.len_utf8() <= MAX_QUERY_BYTES {
            self.text.insert(self.cursor, character);
            self.cursor += character.len_utf8();
        }
    }

    fn delete_previous_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let before = &self.text[..self.cursor];
        let mut boundary = self.cursor;
        let mut seen_word = false;
        for (index, character) in before.char_indices().rev() {
            if character.is_whitespace() {
                if seen_word {
                    boundary = index;
                    break;
                }
            } else {
                seen_word = true;
            }
            boundary = index;
        }
        self.text.drain(boundary..self.cursor);
        self.cursor = boundary;
    }

    fn delete_next_word(&mut self) {
        if self.cursor == self.text.len() {
            return;
        }
        let after = &self.text[self.cursor..];
        let mut end = self.cursor;
        let mut seen_word = false;
        for (offset, character) in after.char_indices() {
            if character.is_whitespace() {
                if seen_word {
                    end = self.cursor + offset + character.len_utf8();
                    break;
                }
            } else {
                seen_word = true;
            }
            end = self.cursor + offset + character.len_utf8();
        }
        self.text.drain(self.cursor..end);
    }

    fn move_word_left(&mut self) {
        let before = &self.text[..self.cursor];
        let mut boundary = self.cursor;
        let mut seen_word = false;
        for (index, character) in before.char_indices().rev() {
            if character.is_whitespace() {
                if seen_word {
                    boundary = index + character.len_utf8();
                    break;
                }
            } else {
                seen_word = true;
            }
            boundary = index;
        }
        self.cursor = boundary;
    }

    fn move_word_right(&mut self) {
        let after = &self.text[self.cursor..];
        let mut offset = 0;
        let mut seen_word = false;
        for character in after.chars() {
            let width = character.len_utf8();
            if character.is_whitespace() {
                if seen_word {
                    offset += width;
                    break;
                }
            } else {
                seen_word = true;
            }
            offset += width;
        }
        self.cursor += offset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn logical_search_crosses_history_and_wide_wrap_but_not_hard_breaks() {
        let mut source = Screen::new(3, 4).unwrap();
        Parser::new().advance(&mut source, "ab中e\u{301}Z\r\nhard\r\nnext".as_bytes());
        assert_eq!(source.history_len(), 1);
        assert_eq!(
            find(&source, "中e\u{301}"),
            vec![Hit {
                start: (0, 2),
                end: (1, 1)
            }]
        );
        assert_eq!(find(&source, "\u{301}")[0].start, (1, 0));
        assert!(find(&source, "Zhard").is_empty());
        assert!(find(&source, "HARD").is_empty());
        assert!(find(&source, "").is_empty());
        let mut wide = Screen::new(2, 4).unwrap();
        Parser::new().advance(&mut wide, "abc中".as_bytes());
        assert_eq!(
            find(&wide, "c中")[0],
            Hit {
                start: (0, 2),
                end: (1, 2)
            }
        );
        assert!(find(&wide, "c 中").is_empty());
    }

    #[test]
    fn explicit_spaces_and_overlapping_matches_are_searchable() {
        let mut source = Screen::new(3, 4).unwrap();
        Parser::new().advance(&mut source, b"aaaa  B");
        assert_eq!(find(&source, "aa").len(), 3);
        assert_eq!(
            find(&source, "  B")[0],
            Hit {
                start: (1, 0),
                end: (1, 3)
            }
        );
    }

    #[test]
    fn query_editor_bounds_utf8_and_handles_backspace_clear_and_invalid_bytes() {
        let mut input = QueryInput::default();
        for &byte in "中e\u{301}".as_bytes() {
            input.feed(byte);
        }
        input.feed(127);
        assert_eq!(input.text, "中e");
        input.feed(0xff);
        input.feed(0xe4);
        input.feed(b'x');
        assert_eq!(input.text, "中ex");
        input.feed(21);
        for byte in "中".repeat(100).bytes() {
            input.feed(byte);
        }
        assert_eq!(input.text.len(), 126);
        for byte in b"abc" {
            input.feed(*byte);
        }
        assert_eq!(input.text.len(), MAX_QUERY_BYTES);
    }

    #[test]
    fn query_history_is_bounded_deduplicated_and_restores_unicode_drafts() {
        let mut history = QueryHistory::default();
        let mut editor = QueryInput::new(Direction::Backward);
        for byte in "草稿".bytes() {
            editor.feed(byte);
        }
        editor.recall(&history, true);
        assert_eq!(editor.text, "草稿");
        for index in 0..25 {
            history.remember(&format!("query{index}"));
        }
        assert_eq!(history.0.len(), MAX_QUERY_HISTORY);
        assert_eq!(history.0[0], "query5");
        history.remember("");
        history.remember("query5");
        assert_eq!(history.0.len(), MAX_QUERY_HISTORY);
        assert_eq!(history.0[0], "query6");
        assert_eq!(history.0.last().unwrap(), "query5");
        editor.recall(&history, false);
        assert_eq!(editor.text, "草稿");
        editor.recall(&history, true);
        assert_eq!(editor.text, "query5");
        for _ in 0..25 {
            editor.recall(&history, true);
        }
        assert_eq!(editor.text, "query6");
        for _ in 0..25 {
            editor.recall(&history, false);
        }
        assert_eq!(editor.text, "草稿");
        assert_eq!(editor.direction, Direction::Backward);
    }

    #[test]
    fn editing_a_recalled_query_creates_a_draft_without_mutating_history() {
        let mut history = QueryHistory::default();
        history.remember("old");
        history.remember("中");
        let mut editor = QueryInput::default();
        editor.recall(&history, true);
        for byte in "文".bytes() {
            editor.feed(byte);
        }
        assert_eq!(editor.text, "中文");
        editor.recall(&history, true);
        assert_eq!(editor.text, "中");
        editor.recall(&history, false);
        assert_eq!(editor.text, "中文");
        editor.recall(&history, true);
        editor.feed(127);
        assert!(editor.text.is_empty());
        editor.recall(&history, true);
        editor.recall(&history, false);
        assert!(editor.text.is_empty());
        editor.feed(0xe4); // Incomplete UTF-8 is discarded when recalling.
        editor.recall(&history, true);
        editor.feed(0xb8);
        editor.feed(0xad);
        assert_eq!(editor.text, "中");
        editor.feed(21);
        editor.recall(&history, true);
        editor.recall(&history, false);
        assert!(editor.text.is_empty());
        assert_eq!(history.0, ["old", "中"]);
    }

    #[test]
    fn editing_moves_on_unicode_boundaries_and_keeps_draft_cursor() {
        let mut editor = QueryInput::default();
        for b in "A中e\u{301}Z".bytes() {
            editor.feed(b);
        }
        editor.edit_sequence(b"\x1b[D");
        editor.feed(127); // Delete the combining scalar before Z.
        assert_eq!(editor.text, "A中eZ");
        editor.edit_sequence(b"\x1bOD");
        editor.edit_sequence(b"\x1b[3~");
        editor.feed(b'x');
        assert_eq!(editor.text, "A中xZ");
        editor.edit_sequence(b"\x1bOH");
        editor.feed(127); // Start boundary.
        editor.feed(b'!');
        assert_eq!(editor.text, "!A中xZ");
        let draft_cursor = editor.cursor;
        let mut history = QueryHistory::default();
        history.remember("old");
        editor.recall(&history, true);
        editor.feed(2); // Moving alone does not detach recalled text.
        editor.recall(&history, false);
        assert_eq!(editor.cursor, draft_cursor);
        editor.feed(b'?');
        assert_eq!(editor.text, "!?A中xZ");
        editor.edit_sequence(b"\x1bOF");
        editor.feed(4); // End boundary.
        editor.feed(6);
        assert_eq!(editor.cursor, editor.text.len());
        editor.feed(21);
        for _ in 0..128 {
            editor.feed(b'a');
        }
        editor.feed(1);
        editor.feed(b'b');
        assert_eq!(editor.cursor, 0);
        assert_eq!(editor.text.len(), MAX_QUERY_BYTES);
        editor.feed(4);
        editor.feed(b'b');
        assert!(editor.text.starts_with("ba"));
    }

    #[test]
    fn label_scrolls_with_insertion_cursor_even_in_narrow_columns() {
        let mut editor = QueryInput::default();
        for b in "abcdefgh中".bytes() {
            editor.feed(b);
        }
        for width in 1..20 {
            let (_, cursor) = editor.display(width);
            assert!(cursor < width);
        }
        editor.feed(1);
        let (label, cursor) = editor.display(12);
        assert_eq!(crate::chrome::clipped(&label, 12), "Search /abcd");
        assert_eq!(cursor, 8);
        editor.feed(6);
        assert_eq!(editor.display(12).1, 9);
        editor.feed(5);
        assert_eq!(
            crate::chrome::clipped(&editor.display(12).0, 12),
            "Search /h中 "
        );
        assert_eq!(editor.display(12).1, 11);
    }

    #[test]
    fn word_motion_skips_whitespace_and_preserves_unicode_boundaries() {
        let mut editor = QueryInput::default();
        for byte in "one  中文 two end".bytes() {
            editor.feed(byte);
        }
        editor.edit_sequence(b"\x1b[1;5D");
        assert_eq!(&editor.text[..editor.cursor], "one  中文 two ");
        editor.edit_sequence(b"\x1b[5D");
        assert_eq!(&editor.text[..editor.cursor], "one  中文 ");
        editor.edit_sequence(b"\x1b[1;5C");
        assert_eq!(&editor.text[..editor.cursor], "one  中文 two ");
        editor.edit_sequence(b"\x1b[5C");
        assert_eq!(editor.cursor, editor.text.len());
        editor.edit_sequence(b"\x1b[1;5D");
        assert_eq!(&editor.text[..editor.cursor], "one  中文 two ");
        editor.edit_sequence(b"\x1b[1;5D");
        assert_eq!(&editor.text[..editor.cursor], "one  中文 ");
        editor.edit_sequence(b"\x1b[1;5D");
        assert_eq!(&editor.text[..editor.cursor], "one  ");
        editor.edit_sequence(b"\x1b[1;5D");
        assert_eq!(editor.cursor, 0);
    }

    #[test]
    fn alt_word_editing_matches_ctrl_word_semantics() {
        let mut editor = QueryInput::default();
        for byte in "one 中文 two end".bytes() {
            editor.feed(byte);
        }
        editor.edit_sequence(b"\x1b\x7f");
        assert_eq!(editor.text, "one 中文 two");
        editor.feed(1);
        editor.edit_sequence(b"\x1bd");
        assert_eq!(editor.text, "中文 two");
        editor.edit_sequence(b"\x1b\x08");
        assert_eq!(editor.text, "中文 two");
        editor.feed(5);
        editor.edit_sequence(b"\x1bd");
        assert_eq!(editor.text, "中文 two");
    }

    #[test]
    fn narrow_query_label_keeps_the_latest_complete_characters_visible() {
        let mut input = QueryInput::default();
        for byte in "abcdef中文".bytes() {
            input.feed(byte);
        }
        assert_eq!(
            crate::chrome::clipped(&input.label(13), 13),
            "Search /中文 "
        );
        assert_eq!(crate::chrome::clipped(&input.label(4), 4), "/文 ");
        input.direction = Direction::Backward;
        assert_eq!(
            crate::chrome::clipped(&input.label(13), 13),
            "Search ?中文 "
        );
        assert_eq!(crate::chrome::clipped(&input.label(4), 4), "?文 ");
    }
}
