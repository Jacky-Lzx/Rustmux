//! Bounded window-bar editors for names, pane destinations and close confirmations.

use crate::{
    chrome::{clipped, prepare_row},
    screen::{CursorShape, EraseMode, MouseTracking, Screen},
    style::{Color, Style},
};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_NAME_BYTES: usize = 128;
const ESCAPE_DELAY: Duration = Duration::from_millis(30);
const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
const PINK: Color = Color::Rgb(0xf5, 0xc2, 0xe7);
const LAVENDER: Color = Color::Rgb(0xb4, 0xbe, 0xfe);
const MIN_INPUT_COLUMNS_WITH_HINTS: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EditResult {
    Continue,
    Save,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKind {
    Rename,
    Close,
    ClosePane,
    MovePane,
}

pub(crate) struct WindowPrompt {
    pub kind: PromptKind,
    pub text: String,
    pub destinations: Vec<crate::window::WindowId>,
    utf8: Vec<u8>,
    escape: Vec<u8>,
    escape_at: Option<Instant>,
    paste: bool,
}

impl WindowPrompt {
    pub fn close() -> Self {
        let mut prompt = Self::new("");
        prompt.kind = PromptKind::Close;
        prompt
    }

    pub fn close_pane() -> Self {
        let mut prompt = Self::new("");
        prompt.kind = PromptKind::ClosePane;
        prompt
    }

    pub fn move_pane(destinations: Vec<crate::window::WindowId>) -> Self {
        let mut prompt = Self::new("");
        prompt.kind = PromptKind::MovePane;
        prompt.destinations = destinations;
        prompt
    }

    fn label(&self) -> &'static str {
        match self.kind {
            PromptKind::Rename => "Rename: ",
            PromptKind::Close => "Close window? Type yes: ",
            PromptKind::ClosePane => "Close pane? Type yes: ",
            PromptKind::MovePane => "Move to window #: ",
        }
    }

    fn submit_label(&self) -> &'static str {
        match self.kind {
            PromptKind::Rename => "Save",
            PromptKind::Close | PromptKind::ClosePane => "Close",
            PromptKind::MovePane => "Move",
        }
    }

    pub fn new(_name: &str) -> Self {
        Self {
            kind: PromptKind::Rename,
            text: String::new(),
            destinations: Vec::new(),
            utf8: Vec::new(),
            escape: Vec::new(),
            escape_at: None,
            paste: false,
        }
    }

    pub fn is_rename(&self) -> bool {
        self.kind == PromptKind::Rename
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
        let panel = Style {
            foreground: TEXT,
            background: BASE,
            ..Style::default()
        };
        if self.is_rename() {
            if rows >= 3 {
                screen.save_cursor();
                prepare_prompt_row(&mut screen, rows - 1, panel);
                screen.set_style(Style {
                    foreground: LAVENDER,
                    bold: true,
                    ..panel
                });
                print(&mut screen, &clipped(" RENAME ", columns));
                let hint_width = format!("<Enter> {}  <Esc> Cancel", self.submit_label()).width();
                if " RENAME ".width() + 1 + hint_width <= columns {
                    draw_action_hints(&mut screen, rows - 1, columns, self.submit_label(), panel);
                }
                screen.restore_cursor();
            }
            configure_modal_screen(&mut screen);
            return screen;
        }

        prepare_row(&mut screen, panel);

        let label = clipped(self.label(), columns.saturating_sub(1));
        screen.set_style(Style {
            foreground: LAVENDER,
            bold: true,
            ..panel
        });
        print(&mut screen, &label);

        let label_width = label.width();
        let hint = format!("<Enter> {}  <Esc> Cancel", self.submit_label());
        let hint_width = hint.width();
        let show_hint = label_width + MIN_INPUT_COLUMNS_WITH_HINTS + 1 + hint_width < columns;
        let reserved_hint = usize::from(show_hint) * (hint_width + 1);
        let mut remaining = columns
            .saturating_sub(label_width)
            .saturating_sub(reserved_hint)
            .saturating_sub(1); // Reserve the visible cursor cell.
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
        let cursor_column = screen.cursor().1;

        if show_hint {
            draw_action_hints(&mut screen, 0, columns, self.submit_label(), panel);
        }

        screen.position(0, cursor_column);
        screen.set_style(panel);
        configure_modal_screen(&mut screen);
        screen
    }
}

fn configure_modal_screen(screen: &mut Screen) {
    screen.set_cursor_visible(true);
    screen.set_cursor_shape(CursorShape::SteadyBar);
    screen.set_bracketed_paste(true);
    screen.set_application_cursor_keys(false);
    screen.set_application_keypad(false);
    screen.set_focus_reporting(false);
    screen.set_mouse_tracking(MouseTracking::Off);
    screen.set_sgr_mouse(false);
}

fn prepare_prompt_row(screen: &mut Screen, row: usize, style: Style) {
    screen.set_origin_mode(false);
    screen.set_insert_mode(false);
    screen.set_auto_wrap(false);
    screen.designate_character_set(false, false);
    screen.select_character_set(false);
    screen.set_style(style);
    screen.position(row, 0);
    screen.erase_line(EraseMode::All);
}

fn print(screen: &mut Screen, text: &str) {
    for character in text.chars() {
        screen.print(character);
    }
}

fn draw_hint(screen: &mut Screen, key: &str, label: &str, panel: Style) {
    screen.set_style(Style {
        foreground: PINK,
        bold: true,
        ..panel
    });
    print(screen, key);
    screen.set_style(Style {
        foreground: LAVENDER,
        ..panel
    });
    print(screen, " ");
    print(screen, label);
}

fn draw_action_hints(
    screen: &mut Screen,
    row: usize,
    columns: usize,
    submit_label: &str,
    panel: Style,
) {
    let hint = format!("<Enter> {submit_label}  <Esc> Cancel");
    let hint_width = hint.width();
    if hint_width > columns {
        return;
    }
    screen.position(row, columns - hint_width);
    draw_hint(screen, "<Enter>", submit_label, panel);
    print(screen, "  ");
    draw_hint(screen, "<Esc>", "Cancel", panel);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn type_bytes(prompt: &mut WindowPrompt, bytes: &[u8]) {
        for &byte in bytes {
            assert_eq!(prompt.feed(byte, Instant::now()), EditResult::Continue);
        }
    }

    #[test]
    fn unicode_backspace_clear_limit_and_invalid_input() {
        let mut prompt = WindowPrompt::new("old");
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
        let mut prompt = WindowPrompt::new("");
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
        assert_eq!(WindowPrompt::new("").feed(3, now), EditResult::Cancel);
    }

    #[test]
    fn close_prompt_requires_enter_outside_paste_and_fits_small_screens() {
        let mut prompt = WindowPrompt::close();
        assert_eq!(prompt.kind, PromptKind::Close);
        for byte in b"\x1b[200~yes\r\n\x1b[201~" {
            assert_eq!(prompt.feed(*byte, Instant::now()), EditResult::Continue);
        }
        assert_eq!(prompt.text, "yes");
        assert_eq!(prompt.feed(b'\r', Instant::now()), EditResult::Save);
        for columns in [1, 8, 24, 40] {
            let screen = Screen::new(2, columns).unwrap();
            let view = prompt.overlay(&screen);
            assert_eq!(view.row(1), screen.row(1));
            assert_eq!(view.cursor().0, 0);
            assert!(view.cursor().1 < columns);
            assert!(!view.wrap_pending());
        }
    }

    #[test]
    fn pane_close_prompt_has_distinct_scope_and_requires_explicit_enter() {
        let mut prompt = WindowPrompt::close_pane();
        assert_eq!(prompt.kind, PromptKind::ClosePane);
        assert_eq!(prompt.label(), "Close pane? Type yes: ");
        for byte in b"\x1b[200~yes\r\n\x1b[201~" {
            assert_eq!(prompt.feed(*byte, Instant::now()), EditResult::Continue);
        }
        assert_eq!(prompt.text, "yes");
        assert_eq!(prompt.feed(3, Instant::now()), EditResult::Cancel);
        assert_eq!(prompt.feed(b'\r', Instant::now()), EditResult::Save);
    }

    #[test]
    fn overlay_fits_narrow_screens_and_never_changes_child_state() {
        for columns in [1, 2, 8, 9, 10, 20] {
            let mut original = Screen::new(3, columns).unwrap();
            Parser::new().advance(&mut original, b"abc\x1b[2;3r\x1b[?6h\x1b(0\x1b[?1003h");
            let saved = original.clone();
            let overlay = WindowPrompt::new("very long 中文e\u{301}").overlay(&original);
            assert_eq!(original, saved);
            assert_eq!(overlay.row(0), original.row(0));
            assert_eq!(overlay.row(1), original.row(1));
            assert_eq!(overlay.cursor(), original.cursor());
            assert!(!overlay.wrap_pending());
            assert!(overlay.bracketed_paste());
            assert_eq!(overlay.mouse_tracking(), MouseTracking::Off);
            if columns >= 8 {
                let label: String = overlay.row(2).unwrap()[..8]
                    .iter()
                    .map(|cell| cell.character)
                    .collect();
                assert_eq!(label, " RENAME ");
            }
        }
    }

    #[test]
    fn wide_prompt_uses_shared_colors_and_right_aligned_action_hints() {
        let screen = Screen::new(3, 80).unwrap();
        let rename = WindowPrompt::new("shell").overlay(&screen);
        let rename_row = rename.row(2).unwrap();
        let rename_text: String = rename_row.iter().map(|cell| cell.character).collect();
        assert!(rename_text.starts_with(" RENAME "));
        assert!(rename_text.ends_with("<Enter> Save  <Esc> Cancel"));
        assert_eq!(rename.cursor(), screen.cursor());

        let view = WindowPrompt::close().overlay(&screen);
        let row = view.row(0).unwrap();
        let text: String = row.iter().map(|cell| cell.character).collect();
        assert!(text.starts_with("Close window? Type yes: "));
        assert!(text.ends_with("<Enter> Close  <Esc> Cancel"));
        assert_eq!(view.cursor(), (0, "Close window? Type yes: ".len()));
        assert_eq!(row[0].style.foreground, LAVENDER);
        assert!(row[0].style.bold);
        assert_eq!(row[8].style.foreground, LAVENDER);

        let hint = 80 - "<Enter> Close  <Esc> Cancel".len();
        assert_eq!(row[hint].style.foreground, PINK);
        assert!(row[hint].style.bold);
        assert_eq!(row[hint + "<Enter> ".len()].style.foreground, LAVENDER);

        let move_pane = WindowPrompt::move_pane(Vec::new()).overlay(&screen);
        let move_text: String = move_pane
            .row(0)
            .unwrap()
            .iter()
            .map(|cell| cell.character)
            .collect();
        assert!(move_text.ends_with("<Enter> Move  <Esc> Cancel"));
    }
}
