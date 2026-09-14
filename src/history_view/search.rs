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

#[derive(Default)]
pub(super) struct QueryInput {
    pub text: String,
    pub direction: Direction,
    utf8: Vec<u8>,
}

impl QueryInput {
    pub fn new(direction: Direction) -> Self {
        Self {
            direction,
            ..Self::default()
        }
    }

    pub fn label(&self, columns: usize) -> String {
        let marker = self.direction.marker();
        let prefix = if columns >= 8 {
            format!("Search {marker}")
        } else {
            marker.to_string()
        };
        let mut remaining = columns.saturating_sub(prefix.len());
        let mut start = self.text.len();
        for (index, character) in self.text.char_indices().rev() {
            let width = character.width().unwrap_or(0);
            if width > remaining {
                break;
            }
            remaining -= width;
            start = index;
        }
        let tail: String = self.text[start..]
            .chars()
            .skip_while(|character| character.width() == Some(0))
            .collect();
        format!("{prefix}{tail} · Enter:find Ctrl-C:cancel")
    }

    pub fn feed(&mut self, byte: u8) {
        match byte {
            CONTROL_BACKSPACE | CONTROL_DELETE => {
                self.utf8.clear();
                self.text.pop();
            }
            CONTROL_CLEAR_LINE => {
                self.utf8.clear();
                self.text.clear();
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
    }

    fn append(&mut self, character: char) {
        if !character.is_control() && self.text.len() + character.len_utf8() <= MAX_QUERY_BYTES {
            self.text.push(character);
        }
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
    fn narrow_query_label_keeps_the_latest_complete_characters_visible() {
        let mut input = QueryInput::default();
        for byte in "abcdef中文".bytes() {
            input.feed(byte);
        }
        assert_eq!(crate::chrome::clipped(&input.label(12), 12), "Search /中文");
        assert_eq!(crate::chrome::clipped(&input.label(3), 3), "/文");
        input.direction = Direction::Backward;
        assert_eq!(crate::chrome::clipped(&input.label(12), 12), "Search ?中文");
        assert_eq!(crate::chrome::clipped(&input.label(3), 3), "?文");
    }
}
