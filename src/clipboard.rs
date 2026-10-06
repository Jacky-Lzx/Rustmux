//! Opt-in bounded observation of child OSC 52 writes; never forwards reads.
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};

const MAX_TEXT_BYTES: usize = 32 * 1024;
const MAX_ENCODED_BYTES: usize = MAX_TEXT_BYTES.div_ceil(3) * 4;
const MAX_CONTROL_BYTES: usize = 3 + 12 + 1 + MAX_ENCODED_BYTES;

#[derive(Debug, Default, Clone, Copy)]
enum State {
    #[default]
    Ground,
    Escape,
    Intermediate,
    Csi,
    String {
        osc: bool,
        escape: bool,
    },
}

#[derive(Debug, Default)]
pub(crate) struct Observer {
    state: State,
    enabled: bool,
    capture: bool,
    current: Vec<u8>,
    pending: Option<Vec<u8>>,
}
impl Observer {
    pub fn configure(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.capture = false;
            self.current.clear();
            self.pending = None;
        }
    }
    pub fn take(&mut self) -> Option<Vec<u8>> {
        self.pending.take()
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if matches!(byte, 0x18 | 0x1a) {
                self.state = State::Ground;
                self.capture = false;
                self.current.clear();
                continue;
            }
            if let State::String { osc, escape } = self.state {
                let bell = osc && byte == 7;
                if bell || (escape && byte == b'\\') {
                    if self.capture && (!bell || !escape) {
                        self.finish();
                    }
                    self.current.clear();
                    self.capture = false;
                    self.state = State::Ground;
                } else {
                    if escape {
                        self.capture = false;
                        self.current.clear();
                    }
                    let next_escape = byte == 0x1b;
                    if self.capture && !next_escape {
                        if self.current.len() == MAX_CONTROL_BYTES {
                            self.capture = false;
                            self.current.clear();
                        } else {
                            self.current.push(byte);
                            // All other string controls are consumed without buffering.
                            if self.current.len() <= 3 && !b"52;".starts_with(&self.current) {
                                self.capture = false;
                                self.current.clear();
                            }
                        }
                    }
                    self.state = State::String {
                        osc,
                        escape: next_escape,
                    };
                }
                continue;
            }
            if byte == 0x1b {
                self.state = State::Escape;
                continue;
            }
            // Match the display parser's treatment of C0 controls in ESC/CSI.
            if byte < 0x20 || byte == 0x7f {
                continue;
            }
            self.state = match self.state {
                State::Ground => State::Ground,
                State::Escape => match byte {
                    b'[' => State::Csi,
                    b']' | b'P' | b'_' | b'^' | b'X' => {
                        self.current.clear();
                        self.capture = byte == b']' && self.enabled;
                        State::String {
                            osc: byte == b']',
                            escape: false,
                        }
                    }
                    0x20..=0x2f => State::Intermediate,
                    _ => State::Ground,
                },
                State::Intermediate => {
                    if (0x30..=0x7e).contains(&byte) {
                        State::Ground
                    } else {
                        State::Intermediate
                    }
                }
                State::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        State::Ground
                    } else {
                        State::Csi
                    }
                }
                State::String { .. } => unreachable!(),
            };
        }
    }

    fn finish(&mut self) {
        if !self.enabled {
            return;
        }
        let Some(body) = self.current.strip_prefix(b"52;") else {
            return;
        };
        let Some(separator) = body.iter().position(|&byte| byte == b';') else {
            return;
        };
        let (selection, payload) = (&body[..separator], &body[separator + 1..]);
        if selection.len() > 12
            || !selection.iter().all(|byte| b"cpqs01234567".contains(byte))
            || payload.len() > MAX_ENCODED_BYTES
            || payload == b"?"
        {
            return;
        }
        let Ok(data) = STANDARD
            .decode(payload)
            .or_else(|_| STANDARD_NO_PAD.decode(payload))
        else {
            return;
        };
        if data.len() > MAX_TEXT_BYTES {
            return;
        }
        // Re-encode rather than forward untrusted control bytes. Every request
        // is one complete canonical BEL-terminated write, including explicit empty writes.
        let mut wire = b"\x1b]52;".to_vec();
        wire.extend_from_slice(selection);
        wire.push(b';');
        wire.extend_from_slice(STANDARD.encode(data).as_bytes());
        wire.push(7);
        self.pending = Some(wire);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parser::Parser, screen::Screen};

    fn observer() -> Observer {
        let mut o = Observer::default();
        o.configure(true);
        o
    }
    fn wire(selection: &str, bytes: &[u8]) -> Vec<u8> {
        format!("\x1b]52;{selection};{}\x07", STANDARD.encode(bytes)).into_bytes()
    }
    #[test]
    fn both_terminators_and_every_chunk_split_emit_one_canonical_request() {
        let expected = wire("c", "hello 中\n\x00".as_bytes());
        for terminator in ["\x07", "\x1b\\"] {
            let input = format!(
                "abc\x1b]52;c;{}{}def",
                STANDARD_NO_PAD.encode("hello 中\n\x00"),
                terminator
            );
            for split in 0..=input.len() {
                let mut o = observer();
                o.advance(&input.as_bytes()[..split]);
                o.advance(&input.as_bytes()[split..]);
                assert_eq!(o.take().unwrap(), expected);
                assert!(o.take().is_none());
            }
            let mut o = observer();
            for byte in input.bytes() {
                o.advance(&[byte]);
            }
            assert_eq!(o.take().unwrap(), expected);
        }
    }
    #[test]
    fn selectors_and_explicit_empty_writes_are_preserved() {
        for selection in ["", "c", "p", "q", "s", "cpqs01234567"] {
            let mut o = observer();
            o.advance(&wire(selection, b""));
            assert_eq!(o.take().unwrap(), wire(selection, b""));
        }
    }
    #[test]
    fn reads_and_malformed_requests_do_not_forward_or_clear_clipboard() {
        for input in [
            "52;c;?",
            "52;c;!",
            "52;c;YWJj;",
            "52;bad;YWJj",
            "052;c;YWJj",
            "52;c",
            "52",
            "52;ccccccccccccc;YWJj",
            "52;c;YQ===",
            "52;c;Zh==",
            "52;c;Y WJj",
            "52;c;é",
            "52;c;YW\x1bXJj",
        ] {
            let mut o = observer();
            o.advance(format!("\x1b]{input}\x07").as_bytes());
            assert!(o.take().is_none(), "{input:?}");
        }
    }
    #[test]
    fn disabled_capture_cannot_be_revived_by_enabling_halfway() {
        let mut o = Observer::default();
        o.advance(b"\x1b]52;c;YW");
        o.configure(true);
        o.advance(b"Jj\x07");
        assert!(o.take().is_none());
        o.advance(&wire("c", b"new"));
        assert_eq!(o.take().unwrap(), wire("c", b"new"));
    }
    #[test]
    fn disabling_drops_pending_and_partial_requests_even_after_reenable() {
        let mut o = observer();
        o.advance(&wire("c", b"old"));
        o.advance(b"\x1b]52;c;YW");
        o.configure(false);
        o.configure(true);
        o.advance(b"Jj\x07");
        assert!(o.take().is_none());
        o.advance(&wire("c", b"new"));
        assert_eq!(o.take().unwrap(), wire("c", b"new"));
    }
    #[test]
    fn nested_controls_in_other_strings_never_trigger_clipboard() {
        for start in *b"P_^X]" {
            let mut o = observer();
            let mut input = vec![0x1b, start];
            input.extend_from_slice(b"ignored");
            input.extend_from_slice(&wire("c", b"nested"));
            input.extend_from_slice(b"\x1b\\");
            o.advance(&input);
            assert!(o.take().is_none(), "{start}");
            o.advance(&wire("c", b"real"));
            assert_eq!(o.take().unwrap(), wire("c", b"real"));
        }
    }
    #[test]
    fn incomplete_cancelled_and_poisoned_strings_have_no_effect() {
        for suffix in [
            b"".as_slice(),
            b"\x1b",
            b"\x18",
            b"\x1a",
            b"\x1bX\x07",
            b"\x1b\x07",
        ] {
            let mut o = observer();
            o.advance(b"\x1b]52;c;YWJj");
            o.advance(suffix);
            assert!(o.take().is_none());
        }
        for cancel in [0x18, 0x1a] {
            let mut o = observer();
            o.advance(b"\x1b]52;c;YW");
            o.advance(&[cancel]);
            o.advance(&wire("c", b"real"));
            assert_eq!(o.take().unwrap(), wire("c", b"real"));
        }
    }
    #[test]
    fn decoded_and_encoded_limits_reject_whole_requests_and_recover() {
        let mut o = observer();
        let maximum = wire("cpqs01234567", &vec![0xff; MAX_TEXT_BYTES]);
        o.advance(&maximum);
        assert_eq!(o.take().unwrap(), maximum);
        o.advance(&wire("c", &vec![0xff; MAX_TEXT_BYTES + 1]));
        assert!(o.take().is_none());
        o.advance(b"\x1b]52;c;");
        o.advance(&vec![b'A'; 1024 * 1024]);
        assert!(o.current.len() <= MAX_CONTROL_BYTES);
        o.advance(b"\x07");
        assert!(o.take().is_none());
        o.advance(&wire("c", b"real"));
        assert_eq!(o.take().unwrap(), wire("c", b"real"));
    }
    #[test]
    fn only_latest_valid_write_is_pending_without_a_backlog() {
        let mut o = observer();
        for count in 0..1000 {
            o.advance(&wire("c", count.to_string().as_bytes()));
        }
        o.advance(b"\x1b]52;c;?\x07");
        assert_eq!(o.take().unwrap(), wire("c", b"999"));
        assert!(o.take().is_none());
    }
    #[test]
    fn ansi_sequences_and_screen_replay_are_independent_of_copy_effects() {
        let mut o = observer();
        let mut screen = Screen::new(2, 8).unwrap();
        let input = b"abc\x1b[31m\x1b]52;c;YWJj\x07\x1b[0m";
        Parser::new().advance(&mut screen, input);
        o.advance(input);
        assert_eq!(screen.cursor(), (0, 3));
        assert_eq!(o.take().unwrap(), wire("c", b"abc"));
        let mut disabled = Observer::default();
        disabled.advance(input);
        assert!(disabled.take().is_none());
        // An ESC inside an unfinished CSI starts a new control, while ESC
        // intermediate sequences and C1 bytes do not masquerade as OSC.
        o.advance(b"\x1b[123\x1b]52;c;YWJj\x07");
        assert!(o.take().is_some());
        o.advance(b"\x1b(]52;c;YWJj\x07\x9d52;c;YWJj\x07");
        assert!(o.take().is_none());
    }
}
