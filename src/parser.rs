//! Incremental UTF-8/CSI parsing for the CLI screen model.

/// Maximum reply bytes per consumed input byte, including a final byte that
/// completes a query begun in an earlier chunk. Two decimal usize coordinates
/// plus CSI, separator and final byte fit in this conservative bound.
pub const MAX_REPLY_BYTES: usize = 4 + 2 * (usize::BITS as usize / 3 + 1);

use crate::screen::{CursorShape, EraseMode, Screen};

#[derive(Debug, Default, Clone, Copy)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    DesignateCharacterSet {
        g1: bool,
    },
    Csi,
    String {
        osc: bool,
        escape: bool,
    },
}

/// Keep at most 32 parameters for combined SGR commands. Invalid or overflowing
/// parameters poison the whole command; input is consumed until its final byte.
#[derive(Debug, Default, Clone, Copy)]
struct Parameters {
    values: [Option<usize>; 32],
    index: usize,
    /// True when this value continues the preceding parameter after a colon.
    subparameter: [bool; 32],
    invalid: bool,
    private: bool,
    soft_reset: bool,
    cursor_shape: bool,
}

impl Parameters {
    fn sgr(&self, mut style: crate::style::Style) -> Option<crate::style::Style> {
        let len = self.index + 1;
        let mut start = 0;
        while start < len {
            let mut end = start + 1;
            if end < len && self.subparameter[end] {
                while end < len && self.subparameter[end] {
                    end += 1;
                }
                let group = &self.values[start..end];
                if matches!(group[0], Some(38 | 48 | 58)) {
                    // Kitty omits the color-space slot; accept an empty or zero
                    // slot too. Keep group boundaries so components never become SGR codes.
                    let normalized;
                    let color = match group {
                        [_, Some(5), Some(_)] | [_, Some(2), Some(_), Some(_), Some(_)] => group,
                        [code, Some(2), None | Some(0), Some(r), Some(g), Some(b)] => {
                            normalized = [*code, Some(2), Some(*r), Some(*g), Some(*b)];
                            &normalized
                        }
                        _ => return None,
                    };
                    style = style.sgr(color)?;
                }
                // Unsupported subparameter groups (for example 4:3) are ignored
                // as a unit, without treating their values as separate attributes.
            } else {
                while end < len && !(end + 1 < len && self.subparameter[end + 1]) {
                    end += 1;
                }
                style = style.sgr(&self.values[start..end])?;
            }
            start = end;
        }
        Some(style)
    }
}

/// Retain one parser per output stream and feed chunks in order into its Screen.
/// Incomplete sequences survive calls. Storage is constant even for hostile input.
/// Invalid UTF-8 is replaced; unsupported commands are ignored. This is not a full VT parser.
#[derive(Debug, Default)]
pub struct Parser {
    state: State,
    parameters: Parameters,
    utf8: [u8; 4],
    utf8_len: usize,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse for display only, discarding terminal replies.
    pub fn advance(&mut self, screen: &mut Screen, bytes: &[u8]) {
        self.advance_with_replies(screen, bytes, &mut |_| {});
    }

    /// Deliver replies synchronously in stream order, without storing them.
    /// The caller must queue or consume each reply. At most MAX_REPLY_BYTES
    /// reply bytes are emitted for each input byte, including split queries.
    pub fn advance_with_replies(
        &mut self,
        screen: &mut Screen,
        bytes: &[u8],
        reply: &mut impl FnMut(&[u8]),
    ) {
        for &byte in bytes {
            self.byte(screen, byte, reply);
        }
    }

    /// End a stream, replacing an incomplete UTF-8 prefix and discarding unfinished
    /// control sequences. Do not call between chunks of the same stream.
    pub fn finish(&mut self, screen: &mut Screen) {
        if self.utf8_len != 0 {
            screen.print('\u{fffd}');
        }
        self.utf8_len = 0;
        self.state = State::Ground;
    }

    fn text_byte(&mut self, screen: &mut Screen, byte: u8, reply: &mut impl FnMut(&[u8])) {
        self.utf8[self.utf8_len] = byte;
        self.utf8_len += 1;
        match std::str::from_utf8(&self.utf8[..self.utf8_len]) {
            Ok(text) => {
                screen.print(text.chars().next().expect("one decoded scalar"));
                self.utf8_len = 0;
            }
            Err(error) => {
                if let Some(invalid_len) = error.error_len() {
                    let pending = self.utf8;
                    let length = self.utf8_len;
                    self.utf8_len = 0;
                    screen.print('\u{fffd}');
                    // Reprocess bytes after the invalid prefix: an ESC or ASCII
                    // character here must retain its ordinary meaning.
                    for &byte in &pending[invalid_len..length] {
                        self.byte(screen, byte, reply);
                    }
                }
            }
        }
    }

    fn byte(&mut self, screen: &mut Screen, byte: u8, reply: &mut impl FnMut(&[u8])) {
        if self.utf8_len != 0 {
            self.text_byte(screen, byte, reply);
            return;
        }
        // CAN and SUB cancel any incomplete sequence, including strings.
        if matches!(byte, 0x18 | 0x1a) {
            self.state = State::Ground;
            return;
        }
        if let State::String { osc, escape } = self.state {
            // OSC accepts BEL or ST (ESC backslash); other strings require ST.
            // Never buffer or render string payload, including embedded controls.
            self.state = if (osc && byte == 7) || (escape && byte == b'\\') {
                State::Ground
            } else {
                State::String {
                    osc,
                    escape: byte == 0x1b,
                }
            };
            return;
        }
        if byte == 0x1b {
            self.state = State::Escape;
            return;
        }
        if matches!(byte, 0x0e | 0x0f) {
            screen.select_character_set(byte == 0x0e);
            return;
        }
        if byte == b'\t' {
            screen.tab();
            return;
        }
        // Supported C0 controls execute inside ESC/CSI without ending the sequence.
        if matches!(byte, 8 | b'\n' | b'\r') {
            screen
                .write_ascii(&[byte])
                .expect("supported ASCII control");
            return;
        }
        if byte < 0x20 || byte == 0x7f {
            return;
        }
        self.state = match self.state {
            State::Ground => {
                if byte.is_ascii() {
                    screen.print(char::from(byte));
                } else {
                    self.text_byte(screen, byte, reply);
                }
                State::Ground
            }
            State::Escape => match byte {
                b'=' | b'>' => {
                    screen.set_application_keypad(byte == b'=');
                    State::Ground
                }
                b'c' => {
                    screen.reset();
                    *self = Self::new();
                    State::Ground
                }
                b'(' | b')' => State::DesignateCharacterSet { g1: byte == b')' },
                b'H' => {
                    screen.set_tab_stop(true);
                    State::Ground
                }
                b'D' | b'E' => {
                    screen.line_feed();
                    if byte == b'E' {
                        screen.move_to(screen.cursor().0, 0);
                    }
                    State::Ground
                }
                b'M' => {
                    screen.reverse_index();
                    State::Ground
                }
                b'7' => {
                    screen.save_cursor();
                    State::Ground
                }
                b'8' => {
                    screen.restore_cursor();
                    State::Ground
                }
                b'[' => {
                    self.parameters = Parameters::default();
                    State::Csi
                }
                b']' | b'P' | b'X' | b'^' | b'_' => State::String {
                    osc: byte == b']',
                    escape: false,
                },
                0x20..=0x2f => State::EscapeIntermediate,
                _ => State::Ground,
            },
            State::DesignateCharacterSet { g1 } => {
                if matches!(byte, b'B' | b'0') {
                    screen.designate_character_set(g1, byte == b'0');
                }
                if (0x20..=0x2f).contains(&byte) {
                    State::EscapeIntermediate
                } else {
                    State::Ground
                }
            }
            State::EscapeIntermediate => {
                if (0x20..=0x2f).contains(&byte) {
                    State::EscapeIntermediate
                } else {
                    State::Ground
                }
            }
            State::Csi => {
                let parameters = &mut self.parameters;
                if (0x40..=0x7e).contains(&byte) {
                    if !parameters.invalid {
                        Self::dispatch(screen, parameters, byte, reply);
                    }
                    State::Ground
                } else {
                    if !parameters.invalid {
                        match byte {
                            _ if parameters.soft_reset || parameters.cursor_shape => {
                                parameters.invalid = true
                            }
                            b' ' if !parameters.private && parameters.index == 0 => {
                                parameters.cursor_shape = true
                            }
                            b'!' if !parameters.private
                                && parameters.index == 0
                                && parameters.values[0].is_none() =>
                            {
                                parameters.soft_reset = true
                            }
                            b'0'..=b'9' => {
                                let value = parameters.values[parameters.index]
                                    .unwrap_or(0)
                                    .checked_mul(10)
                                    .and_then(|n| n.checked_add(usize::from(byte - b'0')));
                                if let Some(value) = value {
                                    parameters.values[parameters.index] = Some(value);
                                } else {
                                    parameters.invalid = true;
                                }
                            }
                            b'?' if !parameters.private
                                && parameters.index == 0
                                && parameters.values[0].is_none() =>
                            {
                                parameters.private = true
                            }
                            b';' | b':' if parameters.index + 1 < parameters.values.len() => {
                                parameters.index += 1;
                                parameters.subparameter[parameters.index] = byte == b':'
                            }
                            // Other prefixes, intermediates and
                            // extra parameters are outside this deliberately small subset.
                            _ => parameters.invalid = true,
                        }
                    }
                    State::Csi
                }
            }
            State::String { .. } => unreachable!("strings handled above"),
        };
    }

    fn dispatch(
        screen: &mut Screen,
        parameters: &Parameters,
        command: u8,
        reply: &mut impl FnMut(&[u8]),
    ) {
        if parameters.cursor_shape {
            if command == b'q' {
                let shape = match parameters.values[0].unwrap_or(0) {
                    0 | 1 => CursorShape::BlinkingBlock,
                    2 => CursorShape::SteadyBlock,
                    3 => CursorShape::BlinkingUnderline,
                    4 => CursorShape::SteadyUnderline,
                    5 => CursorShape::BlinkingBar,
                    6 => CursorShape::SteadyBar,
                    _ => return,
                };
                screen.set_cursor_shape(shape);
            }
            return;
        }
        if parameters.soft_reset {
            if command == b'p' {
                screen.soft_reset();
            }
            return;
        }
        if parameters.private {
            if !parameters.subparameter.contains(&true) && matches!(command, b'h' | b'l') {
                for mode in &parameters.values[..=parameters.index] {
                    if *mode == Some(6) {
                        screen.set_origin_mode(command == b'h');
                    }
                    if *mode == Some(7) {
                        screen.set_auto_wrap(command == b'h');
                    }
                    if *mode == Some(1) {
                        screen.set_application_cursor_keys(command == b'h');
                    }
                    if *mode == Some(2004) {
                        screen.set_bracketed_paste(command == b'h');
                    }
                    if *mode == Some(25) {
                        screen.set_cursor_visible(command == b'h');
                    }
                    if *mode == Some(1049) {
                        if command == b'h' {
                            screen.enter_alternate();
                        } else {
                            screen.leave_alternate();
                        }
                    }
                }
            }
            return;
        }
        if command == b'm' {
            if let Some(style) = parameters.sgr(screen.style()) {
                screen.set_style(style);
            }
            return;
        }
        if parameters.subparameter.contains(&true) {
            return;
        }
        if matches!(command, b'h' | b'l') {
            for mode in &parameters.values[..=parameters.index] {
                if *mode == Some(4) {
                    screen.set_insert_mode(command == b'h');
                }
            }
            return;
        }
        let first = parameters.values[0].unwrap_or(0);
        let second = parameters.values[1].unwrap_or(0);
        if command == b'r' {
            if parameters.index <= 1 {
                let bottom = if second == 0 {
                    screen.dimensions().0
                } else {
                    second
                };
                screen.set_scroll_region(first.max(1) - 1, bottom - 1);
            }
            return;
        }
        if matches!(command, b'H' | b'f') {
            if parameters.index > 1 {
                return;
            }
            // CUP/HVP are one-based; omitted and zero coordinates mean one.
            screen.position(first.saturating_sub(1), second.saturating_sub(1));
            return;
        }
        if parameters.index != 0 {
            return;
        }
        match command {
            b'n' => match first {
                5 => reply(b"\x1b[0n"),
                6 => {
                    let (row, column) = screen.cursor();
                    let row = if screen.origin_mode() {
                        row - screen.scroll_region().0
                    } else {
                        row
                    };
                    let response = format!("\x1b[{};{}R", row + 1, column + 1);
                    debug_assert!(response.len() <= MAX_REPLY_BYTES);
                    reply(response.as_bytes());
                }
                _ => {}
            },
            b'A' => screen.move_up(first.max(1)),
            b'B' => screen.move_down(first.max(1)),
            b'C' => screen.move_right(first.max(1)),
            b'D' => screen.move_left(first.max(1)),
            b'G' | b'`' => screen.move_to(screen.cursor().0, first.saturating_sub(1)),
            b'd' => screen.position_row(first.saturating_sub(1)),
            b'E' => {
                screen.move_down(first.max(1));
                screen.move_to(screen.cursor().0, 0);
            }
            b'F' => {
                screen.move_up(first.max(1));
                screen.move_to(screen.cursor().0, 0);
            }
            b'I' => screen.tab_forward(first.max(1)),
            b'Z' => screen.tab_backward(first.max(1)),
            b'g' => match first {
                0 => screen.set_tab_stop(false),
                3 => screen.clear_tab_stops(),
                _ => {}
            },
            b'@' => screen.insert_characters(first.max(1)),
            b'P' => screen.delete_characters(first.max(1)),
            b'X' => screen.erase_characters(first.max(1)),
            b'L' => screen.insert_lines(first.max(1)),
            b'M' => screen.delete_lines(first.max(1)),
            b'S' => screen.scroll_up(first.max(1)),
            b'T' => screen.scroll_down(first.max(1)),
            b'J' | b'K' => {
                let mode = match first {
                    0 => EraseMode::ToEnd,
                    1 => EraseMode::ToStart,
                    2 => EraseMode::All,
                    _ => return,
                };
                if command == b'J' {
                    screen.erase_display(mode);
                } else {
                    screen.erase_line(mode);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Parser;
    use crate::screen::Screen;

    fn lines(screen: &Screen) -> Vec<String> {
        (0..screen.dimensions().0)
            .map(|row| {
                screen
                    .row(row)
                    .unwrap()
                    .iter()
                    .map(|cell| cell.character)
                    .collect()
            })
            .collect()
    }

    // Each fixture runs whole, one byte at a time and at every two-chunk split.
    // Compare against explicit expected screen contents, not just parser output.
    fn fixture(input: &[u8], expected: &[&str], cursor: (usize, usize)) {
        let new_screen = || Screen::new(expected.len(), expected[0].chars().count()).unwrap();
        let mut reference = new_screen();
        Parser::new().advance(&mut reference, input);
        assert_eq!(lines(&reference), expected);
        assert_eq!(reference.cursor(), cursor);
        for split in 0..=input.len() {
            let mut screen = new_screen();
            let mut parser = Parser::new();
            parser.advance(&mut screen, &input[..split]);
            parser.advance(&mut screen, &input[split..]);
            assert_eq!(screen, reference, "split at {split}");
        }
        let mut screen = new_screen();
        let mut parser = Parser::new();
        for byte in input {
            parser.advance(&mut screen, &[*byte]);
        }
        assert_eq!(screen, reference);
    }

    #[test]
    fn tabs_advance_without_erasing_or_wrapping() {
        fixture(b"A\tB\tC", &["A       B      C"], (0, 15));
        fixture(b"abcd\tX", &["abcX"], (0, 3));
    }

    #[test]
    fn position_defaults_zero_and_clamps() {
        fixture(
            b"\x1b[2;3HX\x1b[0;0fY\x1b[999;999HZ",
            &["Y   ", "  X ", "   Z"],
            (2, 3),
        );
        fixture(b"\x1b[2HX\x1b[;3HY\x1b[HZ", &["Z Y ", "X   "], (0, 1));
    }

    #[test]
    fn relative_movement_defaults_and_boundaries_do_not_scroll() {
        fixture(
            b"\x1b[2;2HX\x1b[A\x1b[0DZ\x1b[2B\x1b[99CW\x1b[99A\x1b[99DQ",
            &["QZ  ", " X  ", "   W"],
            (0, 1),
        );
        fixture(b"\x1b[B\x1b[CX\x1b[0A\x1b[0BY", &["    ", " XY "], (1, 3));
    }

    #[test]
    fn erase_modes_include_cursor_without_moving_it() {
        for (suffix, expected) in [
            ("J", ["abcd", "e   ", "    "]),
            ("0J", ["abcd", "e   ", "    "]),
            ("1J", ["    ", "  gh", "ijkl"]),
            ("2J", ["    ", "    ", "    "]),
            ("K", ["abcd", "e   ", "ijkl"]),
            ("0K", ["abcd", "e   ", "ijkl"]),
            ("1K", ["abcd", "  gh", "ijkl"]),
            ("2K", ["abcd", "    ", "ijkl"]),
        ] {
            let input = format!("abcdefghijkl\x1b[2;2H\x1b[{suffix}");
            fixture(input.as_bytes(), &expected, (1, 1));
        }
    }

    #[test]
    fn commands_cancel_pending_wrap_and_one_cell_works() {
        fixture(b"abcd\x1b[DX", &["abXd", "    "], (0, 3));
        fixture(b"abcd\x1b[KX", &["abcX", "    "], (0, 3));
        fixture(b"abcd\x1b[2JX", &["   X", "    "], (0, 3));
        fixture(b"A\x1b[999;999H\x1b[2KZ", &["Z"], (0, 0));
    }

    #[test]
    fn unsupported_and_malformed_sequences_do_not_leak_into_text() {
        fixture(
            b"A\x1b[999mB\x1b[?2JC\x1b[1:2HD\x1b[1;2;3HE\x1b[1;2AF\x1b[3JG\x1b[2 KH\x1b(BI",
            &["ABCDEFGHI   "],
            (0, 9),
        );
        fixture(
            b"A\x1b[99999999999999999999999999999999999JB",
            &["AB  "],
            (0, 2),
        );
        fixture("AéB".as_bytes(), &["AéB  "], (0, 3));
    }

    #[test]
    fn string_payload_is_skipped_and_split_terminators_work() {
        fixture(b"A\x1b]0;hidden\x07B\x1b]more\x1b\\C\x1bPpayload\x07\n\x1b[2J\x1b\\D\x1bXhidden\x1b\\E\x1b^hidden\x1b\\F\x1b_hidden\x1b\\G",
            &["ABCDEFG   "], (0, 7));
    }

    #[test]
    fn cancellation_escape_restart_and_embedded_controls() {
        fixture(
            b"A\x1b[99\x18B\x1b]hidden\x1aC\x1b[99\x1b[2;2HX",
            &["ABC ", " X  "],
            (1, 2),
        );
        fixture(b"abc\x1b[\r2CX", &["abX ", "    "], (0, 3));
        fixture(b"abc\x1b[\n\x082CX", &["abc ", "   X"], (1, 3));
        fixture(b"A\x1b[\x00\x7f2CX", &["A  X "], (0, 4));
        fixture(b"A\x1b[2;", &["A   "], (0, 1));
    }

    #[test]
    fn long_sequences_are_bounded_and_recover() {
        let mut parser = Parser::new();
        let mut screen = Screen::new(1, 4).unwrap();
        parser.advance(&mut screen, b"A\x1b[");
        let digits = [b'9'; 4096];
        for _ in 0..256 {
            parser.advance(&mut screen, &digits);
        }
        parser.advance(&mut screen, b"JB\x1b]");
        for _ in 0..256 {
            parser.advance(&mut screen, &digits);
        }
        parser.advance(&mut screen, b"\x1b\\C");
        assert_eq!(lines(&screen), ["ABC "]);
        assert_eq!(screen.cursor(), (0, 3));
    }
}
