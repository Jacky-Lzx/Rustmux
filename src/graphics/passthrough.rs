//! Decode one tmux DCS transport layer before Kitty APC framing.

use super::MAX_GRAPHICS_COMMAND_BYTES;

const PREFIX: &[u8] = b"\x1bPtmux;";

#[derive(Debug, Default)]
pub(super) struct TmuxPassthrough {
    state: State,
    utf8_continuations: u8,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Prefix(Vec<u8>),
    Wrapped {
        bytes: Vec<u8>,
        escape: bool,
    },
    Discard {
        escape: bool,
    },
    String {
        escape: bool,
        osc: bool,
    },
}

impl TmuxPassthrough {
    pub(super) fn can_bypass(&self, input: &[u8]) -> bool {
        if !input.is_ascii() {
            return false;
        }
        match self.state {
            State::Ground if self.utf8_continuations == 0 => !input.contains(&0x1b),
            State::String { escape: false, osc } => !input
                .iter()
                .any(|&byte| matches!(byte, 0x18 | 0x1a | 0x1b) || (osc && byte == 7)),
            _ => false,
        }
    }

    pub(super) fn advance(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        for &byte in input {
            if matches!(self.state, State::Ground) {
                if self.utf8_continuations > 0 {
                    if byte & 0xc0 == 0x80 {
                        output.push(byte);
                        self.utf8_continuations -= 1;
                        continue;
                    }
                    self.utf8_continuations = 0;
                }
                self.utf8_continuations = match byte {
                    0xc2..=0xdf => 1,
                    0xe0..=0xef => 2,
                    0xf0..=0xf4 => 3,
                    _ => 0,
                };
                if self.utf8_continuations > 0 {
                    output.push(byte);
                    continue;
                }
            }
            self.state = match std::mem::take(&mut self.state) {
                State::Ground => match byte {
                    0x1b => State::Escape,
                    0x90 | 0x98 | 0x9d..=0x9f => {
                        output.push(byte);
                        State::String {
                            escape: false,
                            osc: byte == 0x9d,
                        }
                    }
                    _ => {
                        output.push(byte);
                        State::Ground
                    }
                },
                State::Escape => match byte {
                    b'P' => State::Prefix(b"\x1bP".to_vec()),
                    b'X' | b']' | b'^' | b'_' => {
                        output.extend_from_slice(&[0x1b, byte]);
                        State::String {
                            escape: false,
                            osc: byte == b']',
                        }
                    }
                    0x1b => {
                        output.push(0x1b);
                        State::Escape
                    }
                    _ => {
                        output.extend_from_slice(&[0x1b, byte]);
                        State::Ground
                    }
                },
                State::Prefix(mut prefix) => {
                    let escape = prefix.last() == Some(&0x1b);
                    let matches = byte == PREFIX[prefix.len()];
                    prefix.push(byte);
                    if !matches {
                        output.extend_from_slice(&prefix);
                        string_state(byte, escape, false)
                    } else if prefix.len() == PREFIX.len() {
                        State::Wrapped {
                            bytes: Vec::new(),
                            escape: false,
                        }
                    } else {
                        State::Prefix(prefix)
                    }
                }
                State::Wrapped { mut bytes, escape } => {
                    if matches!(byte, 0x18 | 0x1a) {
                        State::Ground
                    } else if escape && byte == b'\\' {
                        output.append(&mut bytes);
                        State::Ground
                    } else if escape && byte != 0x1b {
                        State::Discard { escape: false }
                    } else if !escape && byte == 0x1b {
                        State::Wrapped {
                            bytes,
                            escape: true,
                        }
                    } else if bytes.len() == MAX_GRAPHICS_COMMAND_BYTES {
                        State::Discard { escape: false }
                    } else {
                        bytes.push(byte);
                        State::Wrapped {
                            bytes,
                            escape: false,
                        }
                    }
                }
                State::Discard { escape } => {
                    if matches!(byte, 0x18 | 0x1a) || (escape && byte == b'\\') {
                        State::Ground
                    } else {
                        // A doubled ESC is payload, not the start of outer ST.
                        State::Discard {
                            escape: !escape && byte == 0x1b,
                        }
                    }
                }
                State::String { escape, osc } => {
                    output.push(byte);
                    string_state(byte, escape, osc)
                }
            };
        }
        output
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
        self.utf8_continuations = 0;
        match std::mem::take(&mut self.state) {
            State::Escape => vec![0x1b],
            State::Prefix(bytes) => bytes,
            _ => Vec::new(),
        }
    }
}

fn string_state(byte: u8, escape: bool, osc: bool) -> State {
    if matches!(byte, 0x18 | 0x1a | 0x9c) || (escape && byte == b'\\') || (osc && byte == 7) {
        State::Ground
    } else {
        State::String {
            escape: byte == 0x1b,
            osc,
        }
    }
}
