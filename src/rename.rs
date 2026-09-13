//! Bounded, append-only window-name editor and temporary screen overlay.

use crate::{
    screen::{CursorShape, EraseMode, MouseTracking, Screen},
    style::Style,
};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

const MAX_NAME_BYTES: usize = 128;
const ESCAPE_DELAY: Duration = Duration::from_millis(30);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EditResult {
    Continue,
    Save,
    Cancel,
}

pub(crate) struct RenamePrompt {
    pub text: String,
    utf8: Vec<u8>,
    escape: Vec<u8>,
    escape_at: Option<Instant>,
    paste: bool,
}

impl RenamePrompt {
    pub fn new(name: &str) -> Self {
        let mut prompt = Self {
            text: String::new(),
            utf8: Vec::new(),
            escape: Vec::new(),
            escape_at: None,
            paste: false,
        };
        for character in name.chars() {
            prompt.append(character);
        }
        prompt
    }

    fn append(&mut self, character: char) {
        if !character.is_control() && self.text.len() + character.len_utf8() <= MAX_NAME_BYTES {
            self.text.push(character);
        }
    }

    pub fn cancel_due(&self, now: Instant) -> bool {
        !self.paste
            && self.escape == [27]
            && self
                .escape_at
                .is_some_and(|start| now.saturating_duration_since(start) >= ESCAPE_DELAY)
    }

    pub fn feed(&mut self, byte: u8, now: Instant) -> EditResult {
        if !self.escape.is_empty() {
            self.escape.push(byte);
            if self.escape.len() == 2 && matches!(byte, b'[' | b'O') {
                return EditResult::Continue;
            }
            if (self.escape.len() == 2 && !matches!(byte, b'[' | b'O'))
                || (self.escape.len() > 2 && (0x40..=0x7e).contains(&byte))
                || self.escape.len() >= 16
            {
                if self.escape == b"\x1b[200~" {
                    self.paste = true;
                }
                if self.escape == b"\x1b[201~" {
                    self.paste = false;
                }
                self.escape.clear();
                self.escape_at = None;
            }
            return EditResult::Continue;
        }
        if byte == 27 {
            self.utf8.clear();
            self.escape.push(byte);
            self.escape_at = Some(now);
            return EditResult::Continue;
        }
        if !self.paste {
            match byte {
                b'\r' | b'\n' => return EditResult::Save,
                3 | 7 => return EditResult::Cancel,
                8 | 127 => {
                    self.utf8.clear();
                    self.text.pop();
                    return EditResult::Continue;
                }
                21 => {
                    self.utf8.clear();
                    self.text.clear();
                    return EditResult::Continue;
                }
                _ => {}
            }
        }
        if byte < 32 || byte == 127 {
            self.utf8.clear();
            return EditResult::Continue;
        }
        if byte < 128 {
            self.utf8.clear();
            self.append(char::from(byte));
        } else {
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
        EditResult::Continue
    }

    pub fn overlay(&self, original: &Screen) -> Screen {
        let mut screen = original.clone();
        let (rows, columns) = screen.dimensions();
        screen.set_origin_mode(false);
        screen.set_insert_mode(false);
        screen.set_auto_wrap(false);
        screen.designate_character_set(false, false);
        screen.select_character_set(false);
        screen.set_style(Style {
            inverse: true,
            ..Style::default()
        });
        screen.position(rows - 1, 0);
        screen.erase_line(EraseMode::All);
        let mut remaining = columns.saturating_sub(1); // Reserve the visible cursor cell.
        for character in "Rename: ".chars().take(remaining) {
            screen.print(character);
        }
        remaining = remaining.saturating_sub("Rename: ".len());
        let mut start = self.text.len();
        for (index, character) in self.text.char_indices().rev() {
            let width = character.width().unwrap_or(0);
            if width > remaining {
                break;
            }
            remaining -= width;
            start = index;
        }
        for character in self.text[start..]
            .chars()
            .skip_while(|c| c.width() == Some(0))
        {
            screen.print(character);
        }
        screen.set_cursor_visible(true);
        screen.set_cursor_shape(CursorShape::SteadyBar);
        screen.set_bracketed_paste(true);
        screen.set_application_cursor_keys(false);
        screen.set_application_keypad(false);
        screen.set_focus_reporting(false);
        screen.set_mouse_tracking(MouseTracking::Off);
        screen.set_sgr_mouse(false);
        screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn type_bytes(prompt: &mut RenamePrompt, bytes: &[u8]) {
        for &byte in bytes {
            assert_eq!(prompt.feed(byte, Instant::now()), EditResult::Continue);
        }
    }

    #[test]
    fn unicode_backspace_clear_limit_and_invalid_input() {
        let mut prompt = RenamePrompt::new("old");
        type_bytes(&mut prompt, "\x15中文e\u{301}".as_bytes());
        type_bytes(&mut prompt, &[127]);
        assert_eq!(prompt.text, "中文e");
        type_bytes(&mut prompt, &[0xff]);
        assert_eq!(prompt.text, "中文e");
        assert_eq!(prompt.feed(13, Instant::now()), EditResult::Save);
        type_bytes(&mut prompt, &[21]);
        type_bytes(&mut prompt, "中".repeat(100).as_bytes());
        assert_eq!(prompt.text.len(), 126);
        type_bytes(&mut prompt, b"abc");
        assert_eq!(prompt.text.len(), 128);
    }

    #[test]
    fn pasted_controls_do_not_commit_cancel_or_invoke_shortcuts() {
        let mut prompt = RenamePrompt::new("");
        type_bytes(
            &mut prompt,
            "\x1b[200~中文\x02c\n\x03\x15b\x1b[201~".as_bytes(),
        );
        assert_eq!(prompt.text, "中文cb");
        type_bytes(&mut prompt, b"\x1b[D\x1bOA");
        assert_eq!(prompt.text, "中文cb");
        let now = Instant::now();
        prompt.feed(27, now);
        assert!(!prompt.cancel_due(now));
        assert!(prompt.cancel_due(now + ESCAPE_DELAY));
        assert_eq!(RenamePrompt::new("").feed(3, now), EditResult::Cancel);
    }

    #[test]
    fn overlay_fits_narrow_screens_and_never_changes_child_state() {
        for columns in [1, 2, 8, 9, 10, 20] {
            let mut original = Screen::new(3, columns).unwrap();
            Parser::new().advance(&mut original, b"abc\x1b[2;3r\x1b[?6h\x1b(0\x1b[?1003h");
            let saved = original.clone();
            let overlay = RenamePrompt::new("very long 中文e\u{301}").overlay(&original);
            assert_eq!(original, saved);
            assert_eq!(overlay.row(0), original.row(0));
            assert_eq!(overlay.row(1), original.row(1));
            assert_eq!(overlay.cursor().0, 2);
            assert!(overlay.cursor().1 < columns);
            assert!(!overlay.wrap_pending());
            assert!(overlay.bracketed_paste());
            assert_eq!(overlay.mouse_tracking(), MouseTracking::Off);
            if columns >= 8 {
                let label: String = overlay.row(2).unwrap()[..7]
                    .iter()
                    .map(|cell| cell.character)
                    .collect();
                assert_eq!(label, "Rename:");
            }
        }
    }
}
