//! Read-only shortcut reference shown above the composed terminal view.

use crate::{
    chrome::clipped,
    screen::{MouseTracking, Screen},
    style::{Color, Style},
};
use std::time::{Duration, Instant};

const ESCAPE_DELAY: Duration = Duration::from_millis(30);
const MAX_ESCAPE_BYTES: usize = 64;
const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
const SURFACE: Color = Color::Rgb(0x31, 0x32, 0x44);
const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
const MUTED: Color = Color::Rgb(0xa6, 0xad, 0xc8);
const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);

const SHORTCUT_ROWS: &[&str] = &[
    "c       New window             &       Close window",
    "n / p   Next / previous window Tab     Last active window",
    "1-0     Select window 1-10     ,       Rename window",
    "< / >   Move window            % / \"   Split right / down",
    "h/j/k/l Focus pane             C-h/j/k/l Resize pane",
    "o       Next pane              x       Close pane",
    "Z       Toggle zoom            z       Restore closed pane",
    "{ / }   Swap pane              !       Pane to new window",
    "m       Move pane to window",
    "[       Browse history         E       Edit history",
    "e       Edit last output       C-b     Send literal Ctrl-B",
];

pub(crate) struct ShortcutHelp {
    session: bool,
    escape: Vec<u8>,
    escape_at: Option<Instant>,
    paste: bool,
}

impl ShortcutHelp {
    pub fn new(session: bool) -> Self {
        Self {
            session,
            escape: Vec::new(),
            escape_at: None,
            paste: false,
        }
    }

    pub fn escape_expired(&self, now: Instant) -> bool {
        !self.paste
            && self.escape == [27]
            && self
                .escape_at
                .is_some_and(|start| now.saturating_duration_since(start) >= ESCAPE_DELAY)
    }

    /// Consume one byte. Returns true only for a complete close command.
    pub fn feed(&mut self, byte: u8, now: Instant) -> bool {
        if !self.escape.is_empty() {
            self.escape.push(byte);
            let complete = match self.escape.as_slice() {
                [27, b'['] | [27, b'O'] => false,
                [27, b'[', rest @ ..] => {
                    rest.last().is_some_and(|byte| (0x40..=0x7e).contains(byte))
                }
                [27, b'O', ..] => self.escape.len() >= 3,
                _ => true,
            } || self.escape.len() >= MAX_ESCAPE_BYTES;
            if complete {
                if self.escape == b"\x1b[200~" {
                    self.paste = true;
                } else if self.escape == b"\x1b[201~" {
                    self.paste = false;
                }
                self.escape.clear();
                self.escape_at = None;
            }
            return false;
        }
        if byte == 27 {
            self.escape.push(byte);
            self.escape_at = Some(now);
            return false;
        }
        !self.paste && matches!(byte, b'q' | b'?')
    }

    pub fn overlay(&self, original: &Screen) -> Screen {
        let mut screen = original.clone();
        let (rows, columns) = screen.dimensions();
        screen.set_origin_mode(false);
        screen.set_insert_mode(false);
        screen.set_auto_wrap(false);
        screen.designate_character_set(false, false);
        screen.select_character_set(false);
        screen.set_cursor_visible(false);
        screen.set_bracketed_paste(false);
        screen.set_application_cursor_keys(false);
        screen.set_application_keypad(false);
        screen.set_focus_reporting(false);
        screen.set_mouse_tracking(MouseTracking::Off);
        screen.set_sgr_mouse(false);

        if rows == 0 || columns == 0 {
            return screen;
        }
        let mut lines = SHORTCUT_ROWS.to_vec();
        if self.session {
            lines.push("C-w     Session Manager");
        }
        let height = (lines.len() + 4).min(rows);
        let width = 68.min(columns);
        let top = (rows - height) / 2;
        let left = (columns - width) / 2;
        let panel = Style {
            foreground: TEXT,
            background: BASE,
            ..Style::default()
        };
        let border = Style {
            foreground: BLUE,
            background: BASE,
            bold: true,
            ..Style::default()
        };
        for row in top..top + height {
            write_at(&mut screen, row, left, &" ".repeat(width), panel, width);
        }
        if width >= 2 {
            write_at(&mut screen, top, left, "┌", border, 1);
            write_at(
                &mut screen,
                top,
                left + 1,
                &"─".repeat(width - 2),
                border,
                width - 2,
            );
            write_at(&mut screen, top, left + width - 1, "┐", border, 1);
            let bottom = top + height - 1;
            write_at(&mut screen, bottom, left, "└", border, 1);
            write_at(
                &mut screen,
                bottom,
                left + 1,
                &"─".repeat(width - 2),
                border,
                width - 2,
            );
            write_at(&mut screen, bottom, left + width - 1, "┘", border, 1);
            for row in top + 1..bottom {
                write_at(&mut screen, row, left, "│", border, 1);
                write_at(&mut screen, row, left + width - 1, "│", border, 1);
            }
        }
        if width > 4 && height > 2 {
            let title = Style {
                foreground: PEACH,
                background: BASE,
                bold: true,
                ..Style::default()
            };
            write_at(
                &mut screen,
                top,
                left + 2,
                " Shortcut Help ",
                title,
                width - 4,
            );
            let available = height.saturating_sub(3);
            for (index, line) in lines.into_iter().take(available).enumerate() {
                write_at(
                    &mut screen,
                    top + 1 + index,
                    left + 2,
                    line,
                    panel,
                    width - 4,
                );
            }
            let footer = Style {
                foreground: MUTED,
                background: SURFACE,
                ..Style::default()
            };
            write_at(
                &mut screen,
                top + height - 2,
                left + 2,
                "Esc / q / ?  Close",
                footer,
                width - 4,
            );
        }
        screen
    }
}

fn write_at(
    screen: &mut Screen,
    row: usize,
    column: usize,
    text: &str,
    style: Style,
    width: usize,
) {
    screen.position(row, column);
    screen.set_style(style);
    for character in clipped(text, width).chars() {
        screen.print(character);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn text(screen: &Screen) -> String {
        (0..screen.dimensions().0)
            .flat_map(|row| {
                screen
                    .row(row)
                    .unwrap()
                    .iter()
                    .filter(|cell| cell.width != 0)
            })
            .map(|cell| cell.character)
            .collect()
    }

    #[test]
    fn overlay_is_read_only_bounded_and_session_aware() {
        let mut original = Screen::new(18, 80).unwrap();
        Parser::new().advance(&mut original, b"child\x1b[?1003h\x1b[?2004h");
        let before = original.clone();
        let local = ShortcutHelp::new(false).overlay(&original);
        assert_eq!(original, before);
        assert!(text(&local).contains("Shortcut Help"));
        assert!(text(&local).contains("Browse history"));
        assert!(!text(&local).contains("Session Manager"));
        assert_eq!(local.mouse_tracking(), MouseTracking::Off);
        assert!(!local.bracketed_paste());

        let named = ShortcutHelp::new(true).overlay(&original);
        assert!(text(&named).contains("Session Manager"));
        for rows in 1..=4 {
            for columns in 1..=12 {
                ShortcutHelp::new(false).overlay(&Screen::new(rows, columns).unwrap());
            }
        }
    }

    #[test]
    fn only_complete_close_keys_exit_and_paste_stays_modal() {
        let now = Instant::now();
        let mut help = ShortcutHelp::new(false);
        for byte in b"\x1b[A" {
            assert!(!help.feed(*byte, now));
        }
        assert!(!help.escape_expired(now + ESCAPE_DELAY));
        assert!(!help.feed(27, now));
        assert!(help.escape_expired(now + ESCAPE_DELAY));

        let mut help = ShortcutHelp::new(false);
        for byte in b"\x1b[200~q?\x1b[201~" {
            assert!(!help.feed(*byte, now));
        }
        assert!(help.feed(b'q', now));
        assert!(ShortcutHelp::new(false).feed(b'?', now));
    }
}
