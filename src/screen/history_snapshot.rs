//! Primary text history only. Saved strings may contain SGR, never executable controls.

use serde::{Deserialize, Serialize};

use super::*;
use crate::{parser::Parser, style::write_sgr};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedRow {
    pub text: String,
    pub continued: bool,
}

impl Screen {
    pub(crate) fn saved_history(&self, colors: bool, limit: usize) -> Vec<SavedRow> {
        let (cells, used, continued) = if self.alternate {
            (
                &self.inactive_cells,
                &self.inactive_used,
                &self.inactive_continued,
            )
        } else {
            (&self.cells, &self.used, &self.continued)
        };
        let visible = used.iter().rposition(|&n| n != 0).map_or(0, |row| row + 1);
        let total = self.history_len() + visible;
        let start = total.saturating_sub(limit);
        (start..total)
            .map(|index| {
                let (row, length, linked) = if index < self.history_len() {
                    (
                        self.history_row(index).unwrap(),
                        self.history_row_used_columns(index).unwrap(),
                        self.history_row_continued(index).unwrap(),
                    )
                } else {
                    let row = index - self.history_len();
                    (
                        &cells[row * self.columns..(row + 1) * self.columns],
                        used[row],
                        continued[row],
                    )
                };
                let mut bytes = Vec::new();
                let mut style = None;
                for cell in &row[..length.min(row.len())] {
                    if cell.width == 0 {
                        continue;
                    }
                    if colors && style != Some(cell.style) {
                        bytes.extend_from_slice(b"\x1b[");
                        write_sgr(&mut bytes, cell.style).expect("Vec write");
                        style = Some(cell.style);
                    }
                    let mut utf8 = [0; 4];
                    bytes.extend_from_slice(cell.character.encode_utf8(&mut utf8).as_bytes());
                    for character in &cell.combining {
                        bytes.extend_from_slice(character.encode_utf8(&mut utf8).as_bytes());
                    }
                }
                SavedRow {
                    text: String::from_utf8(bytes).expect("encoded UTF-8"),
                    continued: index > start && linked,
                }
            })
            .collect()
    }

    /// Reflow into the new width on a disposable screen; install only its history.
    /// The live screen, cursor, parser, modes, colors and replies stay fresh.
    pub(crate) fn restore_history(&mut self, rows: &[SavedRow], colors: bool) -> io::Result<()> {
        validate_rows(rows, colors)?;
        let mut scratch =
            Self::new_with_scrollback_limit(1, self.columns, self.scrollback.max_lines())?;
        let mut parser = Parser::new();
        for (index, row) in rows.iter().enumerate() {
            parser.advance(&mut scratch, row.text.as_bytes());
            if rows.get(index + 1).is_none_or(|next| !next.continued) {
                parser.advance(&mut scratch, b"\r\n");
            }
        }
        self.scrollback = scratch.scrollback;
        Ok(())
    }
}

pub(crate) fn validate_rows(rows: &[SavedRow], colors: bool) -> io::Result<()> {
    if rows.len() > crate::scrollback::MAX_CELLS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many saved history rows",
        ));
    }
    for row in rows {
        let mut text = row.text.as_str();
        while !text.is_empty() {
            if colors && text.starts_with("\x1b[") {
                let Some(end) = text.find('m').filter(|&end| end <= 512) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid saved SGR",
                    ));
                };
                if !text[2..end]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b';' | b':'))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unsafe saved control sequence",
                    ));
                }
                text = &text[end + 1..];
            } else {
                let character = text.chars().next().unwrap();
                if character.is_control() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unsafe saved history character",
                    ));
                }
                text = &text[character.len_utf8()..];
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Color;

    #[test]
    fn saved_history_drops_hyperlink_controls_but_retains_text_and_style() {
        let mut screen = Screen::new(2, 20).unwrap();
        Parser::new().advance(
            &mut screen,
            b"\x1b]8;id=test;https://example.test\x1b\\\x1b[31mLINK\r\nNEXT",
        );
        assert!(screen.row(0).unwrap()[0].hyperlink.is_some());
        let rows = screen.saved_history(true, 100);
        assert!(rows.iter().all(|row| !row.text.contains("\x1b]")));
        assert!(rows[0].text.ends_with("LINK"));
        let mut restored = Screen::new(2, 20).unwrap();
        restored.restore_history(&rows, true).unwrap();
        assert!(
            restored
                .history_row(0)
                .unwrap()
                .iter()
                .all(|cell| cell.hyperlink.is_none())
        );
        assert!(
            validate_rows(
                &[SavedRow {
                    text: "\x1b]8;;https://example.test\x1b\\LINK".into(),
                    continued: false
                }],
                true
            )
            .is_err()
        );
    }

    #[test]
    fn roundtrip_reflows_unicode_styles_and_explicit_spaces_into_history_only() {
        let mut screen = Screen::new(3, 8).unwrap();
        Parser::new().advance(
            &mut screen,
            "\x1b[31m界e\u{301}  abcdefgh\r\nlast  ".as_bytes(),
        );
        let saved = screen.saved_history(true, 100);
        assert!(saved.iter().any(|row| row.continued));
        let mut restored = Screen::new(3, 5).unwrap();
        restored.restore_history(&saved, true).unwrap();
        assert_eq!(restored.cursor(), (0, 0));
        assert_eq!(restored.row_used_columns(0), Some(0));
        assert_eq!(
            restored.history_row(0).unwrap()[0].style.foreground,
            Color::Indexed(1)
        );
        let text: String = (0..restored.history_len())
            .flat_map(|i| restored.history_row(i).unwrap())
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(text.starts_with("界e  abcdefgh"));
        assert!(text.contains("last  "));
        assert_eq!(restored.history_row(0).unwrap()[1].width, 0);
        assert_eq!(
            restored.history_row(0).unwrap()[2].combining,
            vec!['\u{301}']
        );
    }

    #[test]
    fn saves_primary_under_alternate_screen_and_honors_disabled_history() {
        let mut screen = Screen::new(3, 20).unwrap();
        Parser::new().advance(&mut screen, b"primary\x1b[?1049halternate");
        assert_eq!(screen.saved_history(false, 1)[0].text, "primary");
        assert!(screen.saved_history(false, 0).is_empty());
        let mut restored = Screen::new_with_scrollback_limit(2, 10, 0).unwrap();
        restored
            .restore_history(&screen.saved_history(false, 1), false)
            .unwrap();
        assert_eq!(restored.history_len(), 0);
    }

    #[test]
    fn hostile_history_cannot_change_live_modes_or_send_replies() {
        for text in [
            "\x1b]52;c;aGVsbG8=\x07",
            "\x1b[?1049h",
            "\x1b[6n",
            "\n",
            "\u{009b}6n",
        ] {
            let mut screen = Screen::new(3, 20).unwrap();
            let before = screen.clone();
            assert!(
                screen
                    .restore_history(
                        &[SavedRow {
                            text: text.into(),
                            continued: false
                        }],
                        true
                    )
                    .is_err()
            );
            assert_eq!(screen, before);
        }
    }
}
