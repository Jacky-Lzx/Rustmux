//! Shared bounded framing for Kitty clipboard, file-transfer and drag-and-drop controls.
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protocol {
    #[default]
    Clipboard,
    File,
    Drag,
}
impl Protocol {
    fn prefix(self) -> &'static [u8] {
        match self {
            Self::Clipboard => b"\x1b]5522;",
            Self::File => b"\x1b]5113;",
            Self::Drag => b"\x1b]72;",
        }
    }
    fn max_packet(self) -> usize {
        match self {
            Self::Clipboard => 8192,
            Self::File => 16 * 1024,
            Self::Drag => 8192,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) enum State {
    #[default]
    Ground,
    Escape,
    Prefix(Vec<u8>),
    Csi(Vec<u8>),
    Other {
        osc: bool,
        escape: bool,
    },
    Packet {
        protocol: Protocol,
        bytes: Vec<u8>,
        escape: bool,
        valid: bool,
    },
}

/// Recognize bounded top-level IPC strings; never extract commands from pasted
/// text or nested DCS/APC/OSC controls. IPC protocols share one host framer so
/// an ambiguous Escape is held for only one timeout.
#[derive(Debug, Default)]
pub(crate) struct Framer {
    pub(crate) state: State,
    paste: bool,
    prefix_since: Option<Instant>,
    pub(crate) bad_packet: Option<Protocol>,
    allow_bel: bool,
    protocol: Protocol,
    capture_ipc: bool,
    canceled_prefix: Option<Protocol>,
}
impl Framer {
    pub(crate) fn file() -> Self {
        Self {
            protocol: Protocol::File,
            ..Self::default()
        }
    }
    pub(crate) fn drag() -> Self {
        Self {
            protocol: Protocol::Drag,
            ..Self::default()
        }
    }
    pub(crate) fn host() -> Self {
        Self {
            allow_bel: true,
            capture_ipc: true,
            ..Self::default()
        }
    }

    pub(crate) fn advance(&mut self, byte: u8, pass: &mut Vec<u8>) -> Option<(Protocol, Vec<u8>)> {
        let mut packet = None;
        self.state = match std::mem::take(&mut self.state) {
            State::Ground if byte == 0x1b => {
                self.prefix_since = Some(Instant::now());
                State::Escape
            }
            State::Ground => {
                pass.push(byte);
                State::Ground
            }
            State::Escape => match byte {
                b']' if !self.paste => State::Prefix(b"\x1b]".to_vec()),
                b'[' => {
                    pass.extend_from_slice(b"\x1b[");
                    State::Csi(Vec::new())
                }
                b']' | b'P' | b'_' | b'^' | b'X' if !self.paste => {
                    pass.extend_from_slice(&[0x1b, byte]);
                    State::Other {
                        osc: byte == b']',
                        escape: false,
                    }
                }
                0x1b => {
                    pass.push(0x1b);
                    State::Escape
                }
                _ => {
                    pass.extend_from_slice(&[0x1b, byte]);
                    State::Ground
                }
            },
            State::Prefix(mut bytes) => {
                bytes.push(byte);
                let protocol = if self.protocol.prefix().starts_with(&bytes) {
                    Some(self.protocol)
                } else if self.capture_ipc && Protocol::File.prefix().starts_with(&bytes) {
                    Some(Protocol::File)
                } else if self.capture_ipc && Protocol::Drag.prefix().starts_with(&bytes) {
                    Some(Protocol::Drag)
                } else {
                    None
                };
                if let Some(protocol) = protocol {
                    if bytes.len() == protocol.prefix().len() {
                        State::Packet {
                            protocol,
                            bytes: Vec::new(),
                            escape: false,
                            valid: self.canceled_prefix != Some(protocol),
                        }
                    } else {
                        State::Prefix(bytes)
                    }
                } else {
                    pass.extend_from_slice(&bytes);
                    if matches!(byte, 7 | 0x18 | 0x1a) {
                        State::Ground
                    } else {
                        State::Other {
                            osc: true,
                            escape: byte == 0x1b,
                        }
                    }
                }
            }
            State::Csi(mut bytes) => {
                pass.push(byte);
                if (0x40..=0x7e).contains(&byte) {
                    if bytes == b"200" && byte == b'~' {
                        self.paste = true;
                    }
                    if bytes == b"201" && byte == b'~' {
                        self.paste = false;
                    }
                    State::Ground
                } else if matches!(byte, 0x18 | 0x1a) {
                    State::Ground
                } else if byte == 0x1b {
                    State::Escape
                } else {
                    if bytes.len() < 32 {
                        bytes.push(byte);
                    }
                    State::Csi(bytes)
                }
            }
            State::Other { osc, escape } => {
                pass.push(byte);
                if matches!(byte, 0x18 | 0x1a) || (osc && byte == 7) || (escape && byte == b'\\') {
                    State::Ground
                } else {
                    State::Other {
                        osc,
                        escape: byte == 0x1b,
                    }
                }
            }
            State::Packet {
                protocol,
                mut bytes,
                escape,
                mut valid,
            } => {
                if byte == 7 && self.allow_bel && valid && !escape {
                    packet = Some((protocol, bytes));
                    State::Ground
                } else if matches!(byte, 7 | 0x18 | 0x1a) {
                    self.bad_packet = Some(protocol);
                    State::Ground
                } else if escape && byte == b'\\' {
                    if valid {
                        packet = Some((protocol, bytes));
                    } else {
                        self.bad_packet = Some(protocol);
                    }
                    State::Ground
                } else {
                    if escape || (byte != 0x1b && !(0x20..=0x7e).contains(&byte)) {
                        valid = false;
                        bytes.clear();
                    }
                    if valid && byte != 0x1b {
                        if bytes.len() == protocol.max_packet() {
                            valid = false;
                            bytes.clear();
                        } else {
                            bytes.push(byte);
                        }
                    }
                    State::Packet {
                        protocol,
                        bytes,
                        escape: byte == 0x1b,
                        valid,
                    }
                }
            }
        };
        if !matches!(self.state, State::Escape | State::Prefix(_)) {
            self.prefix_since = None;
            self.canceled_prefix = None;
        }
        packet
    }
    pub(crate) fn expire_prefix(&mut self, now: Instant, pass: &mut Vec<u8>) {
        if self
            .prefix_since
            .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_millis(50))
        {
            match std::mem::take(&mut self.state) {
                State::Escape => pass.push(0x1b),
                State::Prefix(bytes) => {
                    pass.extend(bytes);
                    self.state = State::Other {
                        osc: true,
                        escape: false,
                    };
                }
                state => self.state = state,
            }
            self.prefix_since = None;
            self.canceled_prefix = None;
        }
    }
    pub(crate) fn cancel_packet(&mut self) {
        // A clipboard policy/UI change must not interrupt a fragmented file/drag
        // reply sharing this host framer. Defer ambiguous-prefix cancellation
        // until its protocol is known.
        if matches!(self.state, State::Prefix(_) | State::Escape) {
            self.canceled_prefix = Some(self.protocol);
        }
        if let State::Packet {
            protocol,
            bytes,
            valid,
            ..
        } = &mut self.state
            && *protocol == self.protocol
        {
            bytes.clear();
            *valid = false;
        }
    }
}
