//! Bounded framing of Kitty graphics APC commands in a child output stream.
//!
//! This is the protocol-input boundary only. The terminal still does not
//! display images or advertise graphics support.

/// Kitty limits a direct-data chunk to 4096 encoded bytes. Allow room for
/// control fields without retaining an unbounded APC from a hostile child.
pub const MAX_GRAPHICS_COMMAND_BYTES: usize = 16 * 1024;

/// Events retain their order relative to ordinary terminal output.
#[derive(Debug, Eq, PartialEq)]
pub enum GraphicsEvent {
    Terminal(Vec<u8>),
    Command(Vec<u8>),
}

#[derive(Debug, Default)]
pub struct GraphicsFramer {
    state: State,
    utf8_continuations: u8,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    ApcPrefix {
        c1: bool,
    },
    Graphics {
        command: Vec<u8>,
        escape: bool,
        c1: bool,
    },
    Discard {
        escape: bool,
        c1: bool,
    },
    OtherApc {
        escape: bool,
        c1: bool,
    },
}

impl GraphicsFramer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Separate complete APC G commands from the rest of the byte stream.
    /// Partial commands remain buffered across calls. Oversized, cancelled or
    /// incorrectly terminated graphics commands are discarded.
    /// Non-graphics APCs pass through unchanged for the display parser.
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<GraphicsEvent> {
        let mut events = Vec::new();
        for &byte in bytes {
            if matches!(self.state, State::Ground) {
                if self.utf8_continuations > 0 {
                    if byte & 0xc0 == 0x80 {
                        terminal_byte(&mut events, byte);
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
                    terminal_byte(&mut events, byte);
                    continue;
                }
            }

            self.state = match std::mem::take(&mut self.state) {
                State::Ground => match byte {
                    0x1b => State::Escape,
                    0x9f => State::ApcPrefix { c1: true },
                    _ => {
                        terminal_byte(&mut events, byte);
                        State::Ground
                    }
                },
                State::Escape => match byte {
                    b'_' => State::ApcPrefix { c1: false },
                    0x1b => {
                        terminal_byte(&mut events, 0x1b);
                        State::Escape
                    }
                    0x9f => {
                        terminal_byte(&mut events, 0x1b);
                        State::ApcPrefix { c1: true }
                    }
                    _ => {
                        terminal_byte(&mut events, 0x1b);
                        terminal_byte(&mut events, byte);
                        State::Ground
                    }
                },
                State::ApcPrefix { c1 } if byte == b'G' => State::Graphics {
                    command: if c1 {
                        vec![0x9f, b'G']
                    } else {
                        vec![0x1b, b'_', b'G']
                    },
                    escape: false,
                    c1,
                },
                State::ApcPrefix { c1 } => {
                    if c1 {
                        terminal_byte(&mut events, 0x9f);
                    } else {
                        terminal_byte(&mut events, 0x1b);
                        terminal_byte(&mut events, b'_');
                    }
                    terminal_byte(&mut events, byte);
                    if matches!(byte, 0x18 | 0x1a) || (c1 && byte == 0x9c) {
                        State::Ground
                    } else {
                        State::OtherApc {
                            escape: byte == 0x1b,
                            c1,
                        }
                    }
                }
                State::Graphics {
                    mut command,
                    escape,
                    c1,
                } => {
                    if matches!(byte, 0x18 | 0x1a) {
                        State::Ground
                    } else if escape && byte != b'\\' {
                        State::Discard {
                            escape: byte == 0x1b,
                            c1,
                        }
                    } else {
                        command.push(byte);
                        if (escape && byte == b'\\') || (c1 && byte == 0x9c) {
                            if command.len() <= MAX_GRAPHICS_COMMAND_BYTES {
                                events.push(GraphicsEvent::Command(command));
                            }
                            State::Ground
                        } else if command.len() >= MAX_GRAPHICS_COMMAND_BYTES {
                            State::Discard {
                                escape: byte == 0x1b,
                                c1,
                            }
                        } else {
                            State::Graphics {
                                command,
                                escape: byte == 0x1b,
                                c1,
                            }
                        }
                    }
                }
                State::Discard { escape, c1 } => {
                    if matches!(byte, 0x18 | 0x1a)
                        || (escape && byte == b'\\')
                        || (c1 && byte == 0x9c)
                    {
                        State::Ground
                    } else {
                        State::Discard {
                            escape: byte == 0x1b,
                            c1,
                        }
                    }
                }
                State::OtherApc { escape, c1 } => {
                    terminal_byte(&mut events, byte);
                    if matches!(byte, 0x18 | 0x1a)
                        || (c1 && byte == 0x9c)
                        || (escape && byte == b'\\')
                    {
                        State::Ground
                    } else {
                        State::OtherApc {
                            escape: byte == 0x1b,
                            c1,
                        }
                    }
                }
            };
        }
        events
    }

    /// Flush an incomplete ordinary escape prefix at EOF; incomplete graphics
    /// commands are intentionally not emitted.
    pub fn finish(&mut self) -> Vec<GraphicsEvent> {
        let mut events = Vec::new();
        match std::mem::take(&mut self.state) {
            State::Escape => terminal_byte(&mut events, 0x1b),
            State::ApcPrefix { c1: false } => {
                terminal_byte(&mut events, 0x1b);
                terminal_byte(&mut events, b'_');
            }
            State::ApcPrefix { c1: true } => terminal_byte(&mut events, 0x9f),
            _ => {}
        }
        self.utf8_continuations = 0;
        events
    }
}

fn terminal_byte(events: &mut Vec<GraphicsEvent>, byte: u8) {
    if let Some(GraphicsEvent::Terminal(bytes)) = events.last_mut() {
        bytes.push(byte);
    } else {
        events.push(GraphicsEvent::Terminal(vec![byte]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coalesce(events: impl IntoIterator<Item = GraphicsEvent>) -> Vec<GraphicsEvent> {
        let mut result = Vec::new();
        for event in events {
            match event {
                GraphicsEvent::Terminal(bytes) => {
                    for byte in bytes {
                        terminal_byte(&mut result, byte);
                    }
                }
                command => result.push(command),
            }
        }
        result
    }

    #[test]
    fn preserves_order_at_every_two_chunk_split() {
        let input = b"left\x1b_Gi=3,m=0;YWJj\x1b\\right";
        let expected = vec![
            GraphicsEvent::Terminal(b"left".to_vec()),
            GraphicsEvent::Command(b"\x1b_Gi=3,m=0;YWJj\x1b\\".to_vec()),
            GraphicsEvent::Terminal(b"right".to_vec()),
        ];
        for split in 0..=input.len() {
            let mut framer = GraphicsFramer::new();
            let events = framer
                .advance(&input[..split])
                .into_iter()
                .chain(framer.advance(&input[split..]));
            assert_eq!(coalesce(events), expected, "split at {split}");
        }
        let mut framer = GraphicsFramer::new();
        let events = input
            .iter()
            .flat_map(|byte| framer.advance(std::slice::from_ref(byte)))
            .collect::<Vec<_>>();
        assert_eq!(coalesce(events), expected, "one byte per read");
    }

    #[test]
    fn transfer_chunks_are_separate_commands() {
        let mut framer = GraphicsFramer::new();
        assert_eq!(
            framer.advance(b"\x1b_Gi=3,m=1;YWJj\x1b\\\x1b_Gm=0;ZA==\x1b\\"),
            vec![
                GraphicsEvent::Command(b"\x1b_Gi=3,m=1;YWJj\x1b\\".to_vec()),
                GraphicsEvent::Command(b"\x1b_Gm=0;ZA==\x1b\\".to_vec()),
            ]
        );
    }

    #[test]
    fn other_apc_and_utf8_continuation_are_not_graphics() {
        let mut framer = GraphicsFramer::new();
        assert_eq!(
            framer.advance(b"\xdf\x9f\x1b_Xnot-graphics\x1b\\"),
            vec![GraphicsEvent::Terminal(
                b"\xdf\x9f\x1b_Xnot-graphics\x1b\\".to_vec()
            )]
        );
        assert_eq!(
            framer.advance(b"\x9fGa=d,d=A\x9c"),
            vec![GraphicsEvent::Command(b"\x9fGa=d,d=A\x9c".to_vec())]
        );
    }

    #[test]
    fn oversized_cancelled_and_invalid_commands_do_not_leak() {
        let mut framer = GraphicsFramer::new();
        let mut oversized = b"\x1b_G;".to_vec();
        oversized.extend(std::iter::repeat_n(b'A', MAX_GRAPHICS_COMMAND_BYTES));
        oversized.extend_from_slice(b"\x1b\\tail");
        assert_eq!(
            framer.advance(&oversized),
            vec![GraphicsEvent::Terminal(b"tail".to_vec())]
        );
        assert_eq!(
            framer.advance(b"\x1b_G;AAAA\x18ok\x1b_G;BBB\x1bx\x1b\\after"),
            vec![GraphicsEvent::Terminal(b"okafter".to_vec())]
        );
    }

    #[test]
    fn eof_flushes_only_ordinary_prefixes() {
        let mut framer = GraphicsFramer::new();
        assert!(framer.advance(b"\x1b_").is_empty());
        assert_eq!(
            framer.finish(),
            vec![GraphicsEvent::Terminal(b"\x1b_".to_vec())]
        );
        assert!(framer.advance(b"\x1b_Gi=1;AAAA").is_empty());
        assert!(framer.finish().is_empty());
        assert_eq!(
            framer.advance(b"ready"),
            vec![GraphicsEvent::Terminal(b"ready".to_vec())]
        );
    }
}
