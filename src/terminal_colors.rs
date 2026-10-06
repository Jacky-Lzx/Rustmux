//! Outer-terminal palette discovery, kept separate from pane-local OSC overrides.
//!
//! Requests precede the graphics probe's existing primary-DA barrier. The caller
//! stops this filter at that barrier or its bounded timeout; no second DA query
//! is issued. Unknown input and unfinished candidates remain keyboard input.

use std::sync::Arc;

pub(crate) type Rgb = (u8, u8, u8);
const MAX_REPLY_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalColors {
    pub palette: [Rgb; 256],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
}

impl Default for TerminalColors {
    fn default() -> Self {
        Self {
            palette: std::array::from_fn(|index| crate::theme::default_palette_color(index as u8)),
            foreground: crate::theme::DEFAULT_FOREGROUND_RGB,
            background: crate::theme::DEFAULT_BACKGROUND_RGB,
            cursor: crate::theme::DEFAULT_CURSOR_RGB,
        }
    }
}

pub(crate) struct ColorProbe {
    colors: Arc<TerminalColors>,
    pending: Vec<u8>,
    utf8_continuations: u8,
}

impl ColorProbe {
    pub fn new() -> Self {
        Self {
            colors: Arc::new(TerminalColors::default()),
            pending: Vec::new(),
            utf8_continuations: 0,
        }
    }

    pub fn request_bytes(&self) -> Vec<u8> {
        let mut request = String::new();
        for index in 0..256 {
            use std::fmt::Write;
            write!(request, "\x1b]4;{index};?\x1b\\").unwrap();
        }
        request.push_str("\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");
        request.into_bytes()
    }

    pub fn colors(&self) -> Arc<TerminalColors> {
        Arc::clone(&self.colors)
    }

    /// Consume only valid replies to our palette/default-color requests.
    /// Storage stays bounded even for malformed or unterminated OSC strings.
    pub fn advance(&mut self, input: &[u8], passthrough: &mut Vec<u8>) -> bool {
        let mut changed = false;
        for &byte in input {
            if self.pending.is_empty() {
                if self.utf8_continuations > 0 {
                    if byte & 0xc0 == 0x80 {
                        self.utf8_continuations -= 1;
                        passthrough.push(byte);
                        continue;
                    }
                    self.utf8_continuations = 0;
                }
                if !matches!(byte, 0x1b | 0x9d) {
                    passthrough.push(byte);
                    self.utf8_continuations = utf8_trailing_bytes(byte);
                    continue;
                }
            }
            self.pending.push(byte);
            loop {
                match inspect(&self.pending) {
                    Candidate::Incomplete => break,
                    Candidate::Unrelated => {
                        let first = self.pending.remove(0);
                        passthrough.push(first);
                        self.utf8_continuations = utf8_trailing_bytes(first);
                        if self.pending.is_empty() {
                            break;
                        }
                        // A UTF-8 lead released from an unrelated candidate can
                        // precede a C1-looking continuation in the same buffer.
                        if self.utf8_continuations > 0 {
                            passthrough.append(&mut self.pending);
                            break;
                        }
                    }
                    Candidate::Reply(body) => {
                        if let Some((target, color)) = parse_reply(body) {
                            let colors = Arc::make_mut(&mut self.colors);
                            let value = match target {
                                Target::Palette(index) => &mut colors.palette[usize::from(index)],
                                Target::Foreground => &mut colors.foreground,
                                Target::Background => &mut colors.background,
                                Target::Cursor => &mut colors.cursor,
                            };
                            changed |= *value != color;
                            *value = color;
                            self.pending.clear();
                        } else {
                            passthrough.append(&mut self.pending);
                        }
                        break;
                    }
                }
            }
        }
        changed
    }

    pub fn finish(&mut self, passthrough: &mut Vec<u8>) {
        passthrough.append(&mut self.pending);
    }
}

enum Candidate<'a> {
    Incomplete,
    Unrelated,
    Reply(&'a [u8]),
}

fn inspect(bytes: &[u8]) -> Candidate<'_> {
    if bytes.len() > MAX_REPLY_BYTES {
        return Candidate::Unrelated;
    }
    let body = match bytes {
        [0x1b] => return Candidate::Incomplete,
        [0x1b, b']', body @ ..] | [0x9d, body @ ..] => body,
        _ => return Candidate::Unrelated,
    };
    let prefixes: &[&[u8]] = &[b"4;", b"10;", b"11;", b"12;"];
    if !prefixes
        .iter()
        .any(|prefix| prefix.starts_with(body) || body.starts_with(prefix))
    {
        return Candidate::Unrelated;
    }
    if let Some(body) = body
        .strip_suffix(b"\x1b\\")
        .or_else(|| body.strip_suffix(b"\x07"))
        .or_else(|| body.strip_suffix(b"\x9c"))
    {
        Candidate::Reply(body)
    } else if body.iter().enumerate().any(|(index, &byte)| {
        !byte.is_ascii() || (byte < 0x20 && !(byte == 0x1b && index + 1 == body.len()))
    }) {
        Candidate::Unrelated
    } else {
        Candidate::Incomplete
    }
}

enum Target {
    Palette(u8),
    Foreground,
    Background,
    Cursor,
}

fn parse_reply(body: &[u8]) -> Option<(Target, Rgb)> {
    let mut fields = std::str::from_utf8(body).ok()?.split(';');
    let target = match fields.next()? {
        "4" => {
            let index = fields.next()?;
            if index.is_empty() || !index.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Target::Palette(index.parse().ok()?)
        }
        "10" => Target::Foreground,
        "11" => Target::Background,
        "12" => Target::Cursor,
        _ => return None,
    };
    let value = fields.next()?;
    if !value.starts_with("rgb:") || !value.is_ascii() || fields.next().is_some() {
        return None;
    }
    Some((target, crate::parser::parse_color(value)?))
}

fn utf8_trailing_bytes(byte: u8) -> u8 {
    match byte {
        0xc2..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf4 => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_covers_every_palette_entry_and_default_without_an_extra_da() {
        let request = ColorProbe::new().request_bytes();
        let request = std::str::from_utf8(&request).unwrap();
        for index in 0..256 {
            assert!(request.contains(&format!("\x1b]4;{index};?\x1b\\")));
        }
        assert!(request.ends_with("\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\"));
        assert!(!request.contains("\x1b[c"));
    }

    #[test]
    fn every_split_consumes_replies_and_preserves_keyboard_graphics_and_da() {
        let input = b"key\x1b]4;1;rgb:f3f3/8b8b/a8a8\x1b\\\x1b]10;rgb:1/2/3\x07\x1b]11;rgb:04/05/06\x1b\\\x1b]12;rgb:07/08/09\x07\x1b_Gi=31;OK\x1b\\\x1b[?1;2cend";
        for split in 0..=input.len() {
            let mut probe = ColorProbe::new();
            let mut passthrough = Vec::new();
            probe.advance(&input[..split], &mut passthrough);
            probe.advance(&input[split..], &mut passthrough);
            probe.finish(&mut passthrough);
            assert_eq!(
                passthrough, b"key\x1b_Gi=31;OK\x1b\\\x1b[?1;2cend",
                "split {split}"
            );
            assert_eq!(probe.colors.palette[1], (0xf3, 0x8b, 0xa8));
            assert_eq!(probe.colors.foreground, (17, 34, 51));
            assert_eq!(probe.colors.background, (4, 5, 6));
            assert_eq!(probe.colors.cursor, (7, 8, 9));
        }
    }

    #[test]
    fn malformed_unknown_and_partial_strings_are_lossless_and_bounded() {
        for input in [
            b"\x1b]4;256;rgb:1/2/3\x07".as_slice(),
            b"\x1b]4;1;rgb:zz/22/33\x1b\\",
            b"\x1b]4;1;rgb:11/22/33;extra\x07",
            b"\x1b]0;window title\x07",
            b"\x1b]4;1;?\x07",
            b"\x1b[A\x1b[200~paste\x1b[201~",
            b"\x1b]4;1;rgb:",
            b"\x1b",
        ] {
            let mut probe = ColorProbe::new();
            let mut passthrough = Vec::new();
            assert!(!probe.advance(input, &mut passthrough));
            probe.finish(&mut passthrough);
            assert_eq!(passthrough, input);
        }
        let input = [b"\x1b]4;".as_slice(), &[b'x'; 10000]].concat();
        let mut probe = ColorProbe::new();
        let mut passthrough = Vec::new();
        for &byte in &input {
            probe.advance(&[byte], &mut passthrough);
            assert!(probe.pending.len() <= MAX_REPLY_BYTES);
        }
        probe.finish(&mut passthrough);
        assert_eq!(passthrough, input);
    }

    #[test]
    fn c1_replies_do_not_consume_utf8_continuation_bytes() {
        let mut probe = ColorProbe::new();
        let mut passthrough = Vec::new();
        let text = "\u{009d}你好🦀".as_bytes();
        for &byte in text {
            probe.advance(&[byte], &mut passthrough);
        }
        probe.advance(b"\x9d4;10;rgb:aa/bb/cc\x9c", &mut passthrough);
        probe.finish(&mut passthrough);
        assert_eq!(passthrough, text);
        assert_eq!(probe.colors.palette[10], (0xaa, 0xbb, 0xcc));
    }
    #[test]
    fn pane_overrides_survive_theme_changes_and_resets_restore_inherited_colors() {
        use crate::{parser::Parser, screen::Screen, style::Color};
        let mut initial = TerminalColors::default();
        initial.palette[1] = (0xf3, 0x8b, 0xa8);
        initial.foreground = (1, 2, 3);
        initial.background = (4, 5, 6);
        initial.cursor = (7, 8, 9);
        let initial = Arc::new(initial);
        let mut pane = Screen::new(2, 8).unwrap();
        pane.inherit_colors(&initial);
        let mut other = Screen::new(2, 8).unwrap();
        other.inherit_colors(&initial);
        let mut parser = Parser::new();
        let mut replies = Vec::new();
        parser.advance_with_replies(
            &mut pane,
            b"\x1b[31mA\x1b[0mB\x1b]4;1;?\x07\x1b]10;?\x07",
            &mut |reply| replies.extend_from_slice(reply),
        );
        assert_eq!(
            replies,
            b"\x1b]4;1;rgb:f3f3/8b8b/a8a8\x07\x1b]10;rgb:0101/0202/0303\x07"
        );
        let mut frame = Screen::new(2, 8).unwrap();
        frame.copy_display_cells(&pane, 0, 0);
        assert_eq!(
            frame.row(0).unwrap()[0].style.foreground,
            Color::Rgb(0xf3, 0x8b, 0xa8)
        );
        assert_eq!(
            frame.row(0).unwrap()[1].style.foreground,
            Color::Rgb(1, 2, 3)
        );
        assert_eq!(
            frame.row(0).unwrap()[1].style.background,
            Color::Rgb(4, 5, 6)
        );

        parser.advance(
            &mut pane,
            b"\x1b]4;1;#112233\x07\x1b]10;#445566;#778899;#aabbcc\x07",
        );
        let mut next = (*initial).clone();
        next.palette[1] = (10, 11, 12);
        next.foreground = (13, 14, 15);
        next.background = (16, 17, 18);
        next.cursor = (19, 20, 21);
        let next = Arc::new(next);
        pane.inherit_colors(&next);
        assert_eq!(pane.palette_color(1), (0x11, 0x22, 0x33));
        assert_eq!(pane.default_foreground(), (0x44, 0x55, 0x66));
        assert_eq!(pane.default_background(), (0x77, 0x88, 0x99));
        assert_eq!(pane.cursor_color(), (0xaa, 0xbb, 0xcc));
        assert_eq!(other.palette_color(1), initial.palette[1]);
        assert_eq!(other.default_foreground(), initial.foreground);

        parser.advance(
            &mut pane,
            b"\x1b]104;1\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07",
        );
        pane.resize(4, 10).unwrap();
        parser.advance(&mut pane, b"\x1b[?1049h\x1b[?1049l");
        assert_eq!(pane.palette_color(1), next.palette[1]);
        assert_eq!(pane.default_foreground(), next.foreground);
        assert_eq!(pane.default_background(), next.background);
        assert_eq!(pane.cursor_color(), next.cursor);
        let mut history = Screen::new(4, 10).unwrap();
        history.copy_dynamic_colors(&pane);
        assert_eq!(history.palette_color(1), next.palette[1]);
        assert_eq!(history.default_foreground(), next.foreground);
        let mut view = crate::history_view::HistoryView::new(&pane).unwrap();
        assert_eq!(view.render().unwrap().palette_color(1), next.palette[1]);
        view.inherit_colors(&initial);
        assert_eq!(view.render().unwrap().palette_color(1), initial.palette[1]);
        parser.advance(&mut pane, b"\x1b]4;1;#112233\x07\x1b]104\x07");
        assert_eq!(pane.palette_color(1), next.palette[1]);
    }
    #[test]
    fn color_pop_restores_overrides_and_live_outer_inheritance() {
        use crate::{parser::Parser, screen::Screen};
        let initial = Arc::new(TerminalColors::default());
        let mut pane = Screen::new(2, 8).unwrap();
        pane.inherit_colors(&initial);
        pane.set_default_foreground((1, 2, 3));
        pane.set_palette_color(1, (4, 5, 6));
        Parser::new().advance(&mut pane, b"\x1b]30001\x1b\\");
        pane.set_default_foreground((30, 31, 32));
        pane.set_default_background((33, 34, 35));
        pane.set_cursor_color((36, 37, 38));
        for index in 0..=255 {
            pane.set_palette_color(index, (39, 40, 41));
        }
        let mut next = (*initial).clone();
        next.foreground = (10, 11, 12);
        next.background = (13, 14, 15);
        next.cursor = (16, 17, 18);
        next.palette.fill((19, 20, 21));
        let next = Arc::new(next);
        pane.inherit_colors(&next);
        Parser::new().advance(&mut pane, b"\x1b]30101\x1b\\");
        assert_eq!(pane.default_foreground(), (1, 2, 3));
        assert_eq!(pane.default_background(), next.background);
        assert_eq!(pane.cursor_color(), next.cursor);
        for index in 0..=255 {
            assert_eq!(
                pane.palette_color(index),
                if index == 1 {
                    (4, 5, 6)
                } else {
                    next.palette[usize::from(index)]
                }
            );
        }
        Parser::new().advance(&mut pane, b"\x1b]110\x1b\\\x1b]104;1\x1b\\");
        assert_eq!(pane.default_foreground(), next.foreground);
        assert_eq!(pane.palette_color(1), next.palette[1]);
    }
}
