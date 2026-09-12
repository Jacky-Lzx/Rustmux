//! Incremental ASCII/CSI parsing for the screen model; not yet used by the CLI.

use crate::screen::{EraseMode, Screen};

#[derive(Debug, Default, Clone, Copy)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi(Parameters),
    String {
        osc: bool,
        escape: bool,
    },
}

/// The supported commands need at most two parameters. Invalid or overflowing
/// parameters poison the whole command; input is consumed until its final byte.
#[derive(Debug, Default, Clone, Copy)]
struct Parameters {
    values: [usize; 2],
    index: usize,
    invalid: bool,
}

/// Retain one parser per output stream and feed chunks in order into its Screen.
/// Incomplete sequences survive calls. Storage is constant even for hostile input.
/// Non-ASCII bytes and unsupported commands are ignored; this is not a full VT parser.
#[derive(Debug, Default)]
pub struct Parser {
    state: State,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&mut self, screen: &mut Screen, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(screen, byte);
        }
    }

    fn byte(&mut self, screen: &mut Screen, byte: u8) {
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
                    screen.write_ascii(&[byte]).expect("printable ASCII");
                }
                State::Ground
            }
            State::Escape => match byte {
                b'[' => State::Csi(Parameters::default()),
                b']' | b'P' | b'X' | b'^' | b'_' => State::String {
                    osc: byte == b']',
                    escape: false,
                },
                0x20..=0x2f => State::EscapeIntermediate,
                _ => State::Ground,
            },
            State::EscapeIntermediate => {
                if (0x20..=0x2f).contains(&byte) {
                    State::EscapeIntermediate
                } else {
                    State::Ground
                }
            }
            State::Csi(mut parameters) => {
                if (0x40..=0x7e).contains(&byte) {
                    if !parameters.invalid {
                        Self::dispatch(screen, parameters, byte);
                    }
                    State::Ground
                } else {
                    if !parameters.invalid {
                        match byte {
                            b'0'..=b'9' => {
                                let value = parameters.values[parameters.index]
                                    .checked_mul(10)
                                    .and_then(|n| n.checked_add(usize::from(byte - b'0')));
                                if let Some(value) = value {
                                    parameters.values[parameters.index] = value;
                                } else {
                                    parameters.invalid = true;
                                }
                            }
                            b';' if parameters.index == 0 => parameters.index = 1,
                            // Private prefixes, intermediates, subparameters and
                            // extra parameters are outside this deliberately small subset.
                            _ => parameters.invalid = true,
                        }
                    }
                    State::Csi(parameters)
                }
            }
            State::String { .. } => unreachable!("strings handled above"),
        };
    }

    fn dispatch(screen: &mut Screen, parameters: Parameters, command: u8) {
        let [first, second] = parameters.values;
        if matches!(command, b'H' | b'f') {
            // CUP/HVP are one-based; omitted and zero coordinates mean one.
            screen.move_to(first.saturating_sub(1), second.saturating_sub(1));
            return;
        }
        if parameters.index != 0 {
            return;
        }
        match command {
            b'A' => screen.move_up(first.max(1)),
            b'B' => screen.move_down(first.max(1)),
            b'C' => screen.move_right(first.max(1)),
            b'D' => screen.move_left(first.max(1)),
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
            .map(|row| screen.row(row).unwrap().iter().collect())
            .collect()
    }

    // Each fixture runs whole, one byte at a time and at every two-chunk split.
    // Compare against explicit expected screen contents, not just parser output.
    fn fixture(input: &[u8], expected: &[&str], cursor: (usize, usize)) {
        let new_screen = || Screen::new(expected.len(), expected[0].len()).unwrap();
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
            b"A\x1b[31mB\x1b[?2JC\x1b[1:2HD\x1b[1;2;3HE\x1b[1;2AF\x1b[3JG\x1b[2 KH\x1b(BI",
            &["ABCDEFGHI   "],
            (0, 9),
        );
        fixture(
            b"A\x1b[99999999999999999999999999999999999JB",
            &["AB  "],
            (0, 2),
        );
        fixture("A中B".as_bytes(), &["AB  "], (0, 2));
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
