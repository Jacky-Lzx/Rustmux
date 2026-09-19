//! Actionable shortcut reference shown above the composed terminal view.

use crate::{
    chrome::clipped,
    screen::{MouseTracking, Screen},
    style::{Color, Style},
};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

const ESCAPE_DELAY: Duration = Duration::from_millis(30);
const MAX_ESCAPE_BYTES: usize = 64;
const KEY_WIDTH: usize = 11;
const TWO_COLUMN_WIDTH: usize = 58;
const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
const SURFACE: Color = Color::Rgb(0x31, 0x32, 0x44);
const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
const MUTED: Color = Color::Rgb(0xa6, 0xad, 0xc8);
const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);

#[derive(Clone, Copy)]
struct Command {
    key: &'static str,
    label: &'static str,
    actions: &'static [(usize, u8)],
}

const COMMANDS: &[Command] = &[
    Command {
        key: "c",
        label: "New window",
        actions: &[(0, b'c')],
    },
    Command {
        key: "&",
        label: "Close window",
        actions: &[(0, b'&')],
    },
    Command {
        key: "n/p",
        label: "Switch window",
        actions: &[(0, b'n'), (2, b'p')],
    },
    Command {
        key: "Tab",
        label: "Last window",
        actions: &[(0, b'\t')],
    },
    Command {
        key: "1-0",
        label: "Select window",
        actions: &[(0, b'1'), (2, b'0')],
    },
    Command {
        key: ",",
        label: "Rename window",
        actions: &[(0, b',')],
    },
    Command {
        key: "</>",
        label: "Move window",
        actions: &[(0, b'<'), (2, b'>')],
    },
    Command {
        key: "%/\"",
        label: "Split right/down",
        actions: &[(0, b'%'), (2, b'"')],
    },
    Command {
        key: "h/j/k/l",
        label: "Focus pane",
        actions: &[(0, b'h'), (2, b'j'), (4, b'k'), (6, b'l')],
    },
    Command {
        key: "C-h/j/k/l",
        label: "Resize pane",
        actions: &[(2, 8), (4, 10), (6, 11), (8, 12)],
    },
    Command {
        key: "o",
        label: "Next pane",
        actions: &[(0, b'o')],
    },
    Command {
        key: "x",
        label: "Close pane",
        actions: &[(0, b'x')],
    },
    Command {
        key: "Z",
        label: "Toggle zoom",
        actions: &[(0, b'Z')],
    },
    Command {
        key: "z",
        label: "Restore pane",
        actions: &[(0, b'z')],
    },
    Command {
        key: "{/}",
        label: "Swap pane",
        actions: &[(0, b'{'), (2, b'}')],
    },
    Command {
        key: "!",
        label: "Pane to window",
        actions: &[(0, b'!')],
    },
    Command {
        key: "m",
        label: "Move pane",
        actions: &[(0, b'm')],
    },
    Command {
        key: "[",
        label: "Browse history",
        actions: &[(0, b'[')],
    },
    Command {
        key: "E",
        label: "Edit history",
        actions: &[(0, b'E')],
    },
    Command {
        key: "e",
        label: "Edit last output",
        actions: &[(0, b'e')],
    },
    Command {
        key: "C-b",
        label: "Literal Ctrl-B",
        actions: &[(2, 2)],
    },
];

const SESSION_COMMAND: Command = Command {
    key: "C-w",
    label: "Session Manager",
    actions: &[(2, 23)],
};

#[cfg(test)]
pub(crate) fn documented_actions(session: bool) -> Vec<u8> {
    let mut actions: Vec<_> = COMMANDS
        .iter()
        .chain(session.then_some(&SESSION_COMMAND))
        .flat_map(|command| command.actions.iter().map(|(_, action)| *action))
        .chain(b'2'..=b'9')
        .collect();
    actions.sort_unstable();
    actions.dedup();
    actions
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HelpEvent {
    Continue,
    Redraw,
    Close,
    Action(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Target {
    Action(u8),
    Previous,
    Next,
    Close,
}

#[derive(Clone, Copy)]
struct Hitbox {
    row: usize,
    start: usize,
    end: usize,
    target: Target,
}

pub(crate) struct ShortcutHelp {
    session: bool,
    page: usize,
    pages: usize,
    escape: Vec<u8>,
    escape_at: Option<Instant>,
    paste: bool,
    hitboxes: Vec<Hitbox>,
    pressed: Option<Target>,
}

impl ShortcutHelp {
    pub fn new(session: bool) -> Self {
        Self {
            session,
            page: 0,
            pages: 1,
            escape: Vec::new(),
            escape_at: None,
            paste: false,
            hitboxes: Vec::new(),
            pressed: None,
        }
    }

    pub fn escape_expired(&self, now: Instant) -> bool {
        !self.paste
            && self.escape == [27]
            && self
                .escape_at
                .is_some_and(|start| now.saturating_duration_since(start) >= ESCAPE_DELAY)
    }

    pub fn feed(&mut self, byte: u8, now: Instant) -> HelpEvent {
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
            if !complete {
                return HelpEvent::Continue;
            }
            let sequence = std::mem::take(&mut self.escape);
            self.escape_at = None;
            if sequence == b"\x1b[200~" {
                self.paste = true;
                return HelpEvent::Continue;
            }
            if sequence == b"\x1b[201~" {
                self.paste = false;
                return HelpEvent::Continue;
            }
            if self.paste {
                return HelpEvent::Continue;
            }
            if matches!(sequence.as_slice(), b"\x1b[D" | b"\x1b[5~") {
                return self.change_page(-1);
            }
            if matches!(sequence.as_slice(), b"\x1b[C" | b"\x1b[6~") {
                return self.change_page(1);
            }
            return self.mouse_event(&sequence);
        }
        if byte == 27 {
            self.escape.push(byte);
            self.escape_at = Some(now);
            return HelpEvent::Continue;
        }
        if self.paste {
            return HelpEvent::Continue;
        }
        if matches!(byte, b'q' | b'?') {
            return HelpEvent::Close;
        }
        if self.has_action(byte) {
            HelpEvent::Action(byte)
        } else {
            HelpEvent::Continue
        }
    }

    pub fn overlay(&mut self, original: &Screen) -> Screen {
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
        screen.set_mouse_tracking(MouseTracking::Button);
        screen.set_sgr_mouse(true);
        self.hitboxes.clear();

        if rows == 0 || columns == 0 {
            return screen;
        }
        let commands = self.commands();
        let height = 16.min(rows);
        let width = 72.min(columns);
        let top = (rows - height) / 2;
        let left = (columns - width) / 2;
        let inner_width = width.saturating_sub(4);
        let content_rows = height.saturating_sub(3);
        let columns_per_page = usize::from(inner_width >= TWO_COLUMN_WIDTH) + 1;
        let page_size = (content_rows * columns_per_page).max(1);
        self.pages = commands.len().div_ceil(page_size).max(1);
        self.page = self.page.min(self.pages - 1);

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
        self.draw_border(&mut screen, top, left, height, width, border);
        if width <= 4 || height <= 2 {
            return screen;
        }
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
            inner_width,
        );

        let column_width = inner_width / columns_per_page;
        let start = self.page * page_size;
        for (slot, command) in commands.into_iter().skip(start).take(page_size).enumerate() {
            let column_index = slot / content_rows.max(1);
            let row_index = slot % content_rows.max(1);
            let row = top + 1 + row_index;
            let column = left + 2 + column_index * column_width;
            self.draw_command(&mut screen, row, column, column_width, command, panel);
        }

        let footer = Style {
            foreground: MUTED,
            background: SURFACE,
            ..Style::default()
        };
        let footer_row = top + height - 2;
        let footer_text = if self.pages > 1 {
            format!("←/→ Pages  {}/{}  Esc/q/? Close", self.page + 1, self.pages)
        } else {
            "Esc / q / ?  Close".to_owned()
        };
        write_at(
            &mut screen,
            footer_row,
            left + 2,
            &footer_text,
            footer,
            inner_width,
        );
        if self.pages > 1 {
            self.hitboxes.push(Hitbox {
                row: footer_row + 1,
                start: left + 3,
                end: left + 8,
                target: Target::Previous,
            });
            self.hitboxes.push(Hitbox {
                row: footer_row + 1,
                start: left + 8,
                end: left + 14,
                target: Target::Next,
            });
        }
        let close_start = left + 2 + footer_text.width().saturating_sub("Close".len());
        self.hitboxes.push(Hitbox {
            row: footer_row + 1,
            start: close_start + 1,
            end: (left + width - 1).min(close_start + 6),
            target: Target::Close,
        });
        screen
    }

    fn commands(&self) -> Vec<Command> {
        let mut commands = COMMANDS.to_vec();
        if self.session {
            commands.push(SESSION_COMMAND);
        }
        commands
    }

    fn has_action(&self, byte: u8) -> bool {
        (b'1'..=b'9').contains(&byte)
            || COMMANDS
                .iter()
                .chain(self.session.then_some(&SESSION_COMMAND))
                .flat_map(|command| command.actions.iter())
                .any(|(_, action)| *action == byte)
    }

    fn change_page(&mut self, delta: i32) -> HelpEvent {
        if self.pages <= 1 {
            return HelpEvent::Continue;
        }
        self.page = if delta < 0 {
            self.page.checked_sub(1).unwrap_or(self.pages - 1)
        } else {
            (self.page + 1) % self.pages
        };
        self.pressed = None;
        HelpEvent::Redraw
    }

    fn mouse_event(&mut self, sequence: &[u8]) -> HelpEvent {
        let Some((button, column, row, release)) = sgr_mouse(sequence) else {
            return HelpEvent::Continue;
        };
        if button & 0b1100_0011 == 64 {
            return self.change_page(-1);
        }
        if button & 0b1100_0011 == 65 {
            return self.change_page(1);
        }
        if button & 32 != 0 {
            self.pressed = None;
            return HelpEvent::Continue;
        }
        let target = self
            .hitboxes
            .iter()
            .find(|hitbox| hitbox.row == row && column >= hitbox.start && column < hitbox.end)
            .map(|hitbox| hitbox.target);
        if release {
            let clicked = self
                .pressed
                .take()
                .filter(|pressed| Some(*pressed) == target);
            return clicked.map_or(HelpEvent::Continue, |target| self.activate(target));
        }
        if button & 0b11 == 0 {
            self.pressed = target;
        }
        HelpEvent::Continue
    }

    fn activate(&mut self, target: Target) -> HelpEvent {
        match target {
            Target::Action(byte) => HelpEvent::Action(byte),
            Target::Previous => self.change_page(-1),
            Target::Next => self.change_page(1),
            Target::Close => HelpEvent::Close,
        }
    }

    fn draw_border(
        &self,
        screen: &mut Screen,
        top: usize,
        left: usize,
        height: usize,
        width: usize,
        style: Style,
    ) {
        if width < 2 {
            return;
        }
        write_at(screen, top, left, "┌", style, 1);
        write_at(
            screen,
            top,
            left + 1,
            &"─".repeat(width - 2),
            style,
            width - 2,
        );
        write_at(screen, top, left + width - 1, "┐", style, 1);
        let bottom = top + height - 1;
        write_at(screen, bottom, left, "└", style, 1);
        write_at(
            screen,
            bottom,
            left + 1,
            &"─".repeat(width - 2),
            style,
            width - 2,
        );
        write_at(screen, bottom, left + width - 1, "┘", style, 1);
        for row in top + 1..bottom {
            write_at(screen, row, left, "│", style, 1);
            write_at(screen, row, left + width - 1, "│", style, 1);
        }
    }

    fn draw_command(
        &mut self,
        screen: &mut Screen,
        row: usize,
        column: usize,
        width: usize,
        command: Command,
        style: Style,
    ) {
        let key_width = KEY_WIDTH.min(width);
        let key = clipped(command.key, key_width);
        let text = format!("{key:<key_width$}{}", command.label);
        write_at(screen, row, column, &text, style, width);
        for (offset, action) in command.actions {
            if *offset < key.width().min(key_width) {
                self.hitboxes.push(Hitbox {
                    row: row + 1,
                    start: column + 1 + offset,
                    end: column + 2 + offset,
                    target: Target::Action(*action),
                });
            }
        }
        if let Some((_, action)) = command.actions.first() {
            self.hitboxes.push(Hitbox {
                row: row + 1,
                start: column + 1,
                end: column + width + 1,
                target: Target::Action(*action),
            });
        }
    }
}

fn sgr_mouse(sequence: &[u8]) -> Option<(u16, usize, usize, bool)> {
    if !sequence.starts_with(b"\x1b[<") || !matches!(sequence.last(), Some(b'M' | b'm')) {
        return None;
    }
    let text = std::str::from_utf8(&sequence[3..sequence.len() - 1]).ok()?;
    let mut parts = text.split(';');
    let button = parts.next()?.parse().ok()?;
    let column = parts.next()?.parse().ok()?;
    let row = parts.next()?.parse().ok()?;
    parts
        .next()
        .is_none()
        .then_some((button, column, row, sequence.last() == Some(&b'm')))
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
    fn overlay_is_bounded_paginated_and_session_aware() {
        let mut original = Screen::new(18, 80).unwrap();
        Parser::new().advance(&mut original, b"child\x1b[?1003h\x1b[?2004h");
        let before = original.clone();
        let mut local = ShortcutHelp::new(false);
        let view = local.overlay(&original);
        assert_eq!(original, before);
        assert!(text(&view).contains("Shortcut Help"));
        assert!(text(&view).contains("Browse history"));
        assert!(!text(&view).contains("Session Manager"));
        assert_eq!(view.mouse_tracking(), MouseTracking::Button);
        assert!(view.sgr_mouse());
        assert!(!view.bracketed_paste());

        let mut named = ShortcutHelp::new(true);
        assert!(text(&named.overlay(&original)).contains("Session Manager"));
        let mut short = ShortcutHelp::new(false);
        let first = text(&short.overlay(&Screen::new(6, 40).unwrap()));
        assert!(first.contains("1/"));
        for byte in b"\x1b[6~" {
            short.feed(*byte, Instant::now());
        }
        let second = text(&short.overlay(&Screen::new(6, 40).unwrap()));
        assert_ne!(first, second);
        let mut wheel = HelpEvent::Continue;
        for byte in b"\x1b[<64;1;1M" {
            wheel = short.feed(*byte, Instant::now());
        }
        assert_eq!(wheel, HelpEvent::Redraw);
        short.overlay(&Screen::new(6, 40).unwrap());
        assert!(
            short
                .hitboxes
                .iter()
                .any(|hitbox| hitbox.target == Target::Next)
        );
        short.overlay(&Screen::new(18, 80).unwrap());
        assert_eq!(short.page, 0);
        assert_eq!(short.pages, 1);

        for rows in 1..=4 {
            for columns in 1..=12 {
                ShortcutHelp::new(false).overlay(&Screen::new(rows, columns).unwrap());
            }
        }
    }

    #[test]
    fn actions_close_keys_sequences_and_paste_stay_modal() {
        let now = Instant::now();
        let mut help = ShortcutHelp::new(false);
        assert_eq!(help.feed(b'c', now), HelpEvent::Action(b'c'));
        assert_eq!(help.feed(b'5', now), HelpEvent::Action(b'5'));
        assert_eq!(help.feed(23, now), HelpEvent::Continue);
        assert_eq!(ShortcutHelp::new(true).feed(23, now), HelpEvent::Action(23));
        for byte in b"\x1b[A" {
            assert_eq!(help.feed(*byte, now), HelpEvent::Continue);
        }
        assert!(!help.escape_expired(now + ESCAPE_DELAY));
        assert_eq!(help.feed(27, now), HelpEvent::Continue);
        assert!(help.escape_expired(now + ESCAPE_DELAY));

        let mut help = ShortcutHelp::new(false);
        for byte in b"\x1b[200~cq?\x1b[201~" {
            assert_eq!(help.feed(*byte, now), HelpEvent::Continue);
        }
        assert_eq!(help.feed(b'q', now), HelpEvent::Close);
        assert_eq!(ShortcutHelp::new(false).feed(b'?', now), HelpEvent::Close);
    }

    #[test]
    fn mouse_click_activates_exact_key_and_drag_cancels() {
        let now = Instant::now();
        let mut help = ShortcutHelp::new(false);
        help.overlay(&Screen::new(18, 80).unwrap());
        let hitbox = help
            .hitboxes
            .iter()
            .find(|hitbox| hitbox.target == Target::Action(b'p'))
            .copied()
            .unwrap();
        let column = hitbox.start;
        let press = format!("\x1b[<0;{column};{}M", hitbox.row);
        let release = format!("\x1b[<0;{column};{}m", hitbox.row);
        for byte in press.bytes() {
            assert_eq!(help.feed(byte, now), HelpEvent::Continue);
        }
        let mut event = HelpEvent::Continue;
        for byte in release.bytes() {
            event = help.feed(byte, now);
        }
        assert_eq!(event, HelpEvent::Action(b'p'));

        for byte in press.bytes() {
            help.feed(byte, now);
        }
        for byte in format!("\x1b[<32;{};{}M", column + 1, hitbox.row).bytes() {
            help.feed(byte, now);
        }
        for byte in release.bytes() {
            event = help.feed(byte, now);
        }
        assert_eq!(event, HelpEvent::Continue);
    }
}
