//! Bounded plain-text capture between OSC 133 command-output markers.

const MAX_OSC_BYTES: usize = 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct SemanticOutput {
    state: State,
    capturing: bool,
    overflowed: bool,
    current: Vec<u8>,
    last: Option<String>,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
    Osc {
        bytes: Vec<u8>,
        overflowed: bool,
    },
    OscEscape {
        bytes: Vec<u8>,
        overflowed: bool,
    },
    String,
    StringEscape,
}

impl SemanticOutput {
    pub fn advance(&mut self, input: &[u8]) {
        for &byte in input {
            if matches!(byte, 0x18 | 0x1a) {
                self.state = State::Ground;
                continue;
            }
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                State::Ground => self.ground(byte),
                State::Escape => match byte {
                    b'[' => State::Csi,
                    b']' => State::Osc {
                        bytes: Vec::new(),
                        overflowed: false,
                    },
                    b'P' | b'X' | b'^' | b'_' => State::String,
                    0x1b => State::Escape,
                    _ => State::Ground,
                },
                State::Csi => {
                    if byte == 0x1b {
                        State::Escape
                    } else if (0x40..=0x7e).contains(&byte) {
                        State::Ground
                    } else {
                        State::Csi
                    }
                }
                State::Osc {
                    mut bytes,
                    mut overflowed,
                } => match byte {
                    7 => {
                        if !overflowed {
                            self.osc(&bytes);
                        }
                        State::Ground
                    }
                    0x1b => State::OscEscape { bytes, overflowed },
                    _ => {
                        push_bounded(&mut bytes, byte, &mut overflowed);
                        State::Osc { bytes, overflowed }
                    }
                },
                State::OscEscape {
                    mut bytes,
                    mut overflowed,
                } => {
                    if byte == b'\\' {
                        if !overflowed {
                            self.osc(&bytes);
                        }
                        State::Ground
                    } else {
                        push_bounded(&mut bytes, 0x1b, &mut overflowed);
                        push_bounded(&mut bytes, byte, &mut overflowed);
                        State::Osc { bytes, overflowed }
                    }
                }
                State::String => {
                    if byte == 0x1b {
                        State::StringEscape
                    } else {
                        State::String
                    }
                }
                State::StringEscape => match byte {
                    b'\\' => State::Ground,
                    0x1b => State::StringEscape,
                    _ => State::String,
                },
            };
        }
    }

    pub fn last_output(&self) -> Option<&str> {
        (!self.capturing).then_some(self.last.as_deref()).flatten()
    }

    pub fn cancel_current(&mut self) {
        self.capturing = false;
        self.overflowed = false;
        self.current = Vec::new();
    }

    fn ground(&mut self, byte: u8) -> State {
        match byte {
            0x1b => State::Escape,
            b'\n' | b'\t' if self.capturing => {
                self.push(byte);
                State::Ground
            }
            b'\r' => State::Ground,
            8 if self.capturing => {
                remove_last_scalar(&mut self.current);
                State::Ground
            }
            0x20..=0x7e | 0x80..=0xff if self.capturing => {
                self.push(byte);
                State::Ground
            }
            _ => State::Ground,
        }
    }

    fn osc(&mut self, control: &[u8]) {
        let mut fields = control.split(|&byte| byte == b';');
        if fields.next() != Some(b"133".as_slice()) {
            return;
        }
        match fields.next() {
            Some(b"C") => {
                self.current.clear();
                self.capturing = true;
                self.overflowed = false;
            }
            Some(b"D" | b"A") if self.capturing => self.complete(),
            _ => {}
        }
    }

    fn push(&mut self, byte: u8) {
        if self.current.len() == MAX_OUTPUT_BYTES {
            self.overflowed = true;
        } else if !self.overflowed {
            self.current.push(byte);
        }
    }

    fn complete(&mut self) {
        self.last = if self.overflowed {
            None
        } else {
            Some(String::from_utf8_lossy(&std::mem::take(&mut self.current)).into_owned())
        };
        self.cancel_current();
    }
}

fn push_bounded(bytes: &mut Vec<u8>, byte: u8, overflowed: &mut bool) {
    if bytes.len() == MAX_OSC_BYTES {
        *overflowed = true;
    } else if !*overflowed {
        bytes.push(byte);
    }
}

fn remove_last_scalar(bytes: &mut Vec<u8>) {
    while let Some(byte) = bytes.pop() {
        if byte & 0xc0 != 0x80 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_completed_output_and_strips_terminal_controls() {
        let mut output = SemanticOutput::default();
        for chunk in [
            b"ignored\x1b]133;".as_slice(),
            b"C\x07red\x1b[31m \xe4",
            b"\xb8\xad\x1b[0m\r\nnext\tX\x1b]133;D;0\x1b",
            b"\\prompt",
        ] {
            output.advance(chunk);
        }
        assert_eq!(output.last_output(), Some("red 中\nnext\tX"));
    }

    #[test]
    fn running_incomplete_and_oversized_commands_are_unavailable() {
        let mut output = SemanticOutput::default();
        output.advance(b"\x1b]133;C\x07old\x1b]133;D\x07");
        assert_eq!(output.last_output(), Some("old"));
        output.advance(b"\x1b]133;C\x07running");
        assert_eq!(output.last_output(), None);
        output.advance(b"\x1b]133;C\x07new\x1b]133;A\x07");
        assert_eq!(output.last_output(), Some("new"));

        output.advance(b"\x1b]133;C\x07");
        output.advance(&vec![b'x'; MAX_OUTPUT_BYTES + 1]);
        output.advance(b"\x1b]133;D\x07");
        assert_eq!(output.last_output(), None);
    }

    #[test]
    fn malformed_cancelled_and_overlong_markers_do_not_create_boundaries() {
        let mut output = SemanticOutput::default();
        output.advance(b"\x1b]133;Cat\x07text\x1b]133;D\x07");
        assert_eq!(output.last_output(), None);
        output.advance(b"\x1b]133;\x18C\x07text\x1b]133;D\x07");
        assert_eq!(output.last_output(), None);

        let mut marker = b"\x1b]133;".to_vec();
        marker.extend(std::iter::repeat_n(b'x', MAX_OSC_BYTES + 1));
        marker.push(7);
        output.advance(&marker);
        assert_eq!(output.last_output(), None);
    }

    #[test]
    fn backspace_removes_one_complete_utf8_scalar() {
        let mut output = SemanticOutput::default();
        output.advance(b"\x1b]133;C\x07A\xe4\xb8\xad\x08B\x1b]133;D\x07");
        assert_eq!(output.last_output(), Some("AB"));
    }
}
