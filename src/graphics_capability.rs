//! Opt-in detection of Kitty graphics support on an outer terminal.
//!
//! This is a transport-independent byte filter. The caller sends the query,
//! feeds terminal input before its normal key parser, and forwards the returned
//! non-response bytes. The terminal runtime probes each new attachment.

use std::num::NonZeroU32;

const MAX_GRAPHICS_REPLY_BYTES: usize = 1024;
const MAX_DEVICE_ATTRIBUTES_BYTES: usize = 64;

/// Whether the outer terminal answered the graphics query before primary DA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphicsSupport {
    Supported,
    Unsupported,
}

/// Incremental filter for one Kitty graphics query and its DA barrier.
///
/// Replies are retained only up to fixed limits. Unknown input is passed
/// through byte-for-byte. A caller may call [`finish`](Self::finish) on timeout
/// or disconnect to release an unfinished candidate; a timeout alone is not
/// proof that the terminal lacks support. The caller must reserve `image_id`
/// for this query and avoid another concurrent primary-DA request.
pub struct GraphicsCapabilityProbe {
    image_id: NonZeroU32,
    require_ok: bool,
    pending: Vec<u8>,
    utf8_continuations: u8,
    support: Option<GraphicsSupport>,
    complete: bool,
}

impl GraphicsCapabilityProbe {
    pub fn new(image_id: NonZeroU32) -> Self {
        Self {
            image_id,
            require_ok: false,
            pending: Vec::new(),
            utf8_continuations: 0,
            support: None,
            complete: false,
        }
    }

    /// Probe a particular transport: an error reply means that transport
    /// failed even though the terminal implements the graphics protocol.
    pub fn new_strict(image_id: NonZeroU32) -> Self {
        Self {
            require_ok: true,
            ..Self::new(image_id)
        }
    }

    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Query a one-pixel RGB image without storing or displaying it, followed
    /// immediately by primary device attributes as the FIFO response barrier.
    pub fn request_bytes(&self) -> Vec<u8> {
        format!(
            "\x1b_Gi={},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c",
            self.image_id
        )
        .into_bytes()
    }

    /// Filter one arbitrary terminal-input chunk into `passthrough`.
    ///
    /// Returns the support decision only when it first becomes known. A
    /// graphics reply identifies support even if its payload is an error; a
    /// primary DA reply first identifies lack of support. The DA reply is
    /// consumed after either outcome so it cannot reach the key parser.
    pub fn advance(&mut self, input: &[u8], passthrough: &mut Vec<u8>) -> Option<GraphicsSupport> {
        let mut decision = None;
        for &byte in input {
            if self.complete {
                passthrough.push(byte);
                continue;
            }
            if self.pending.is_empty() {
                if self.utf8_continuations > 0 {
                    if byte & 0xc0 == 0x80 {
                        self.utf8_continuations -= 1;
                        passthrough.push(byte);
                        continue;
                    }
                    self.utf8_continuations = 0;
                }
                if !matches!(byte, 0x1b | 0x9f | 0x9b) {
                    passthrough.push(byte);
                    self.utf8_continuations = utf8_trailing_bytes(byte);
                    continue;
                }
            }
            self.pending.push(byte);
            loop {
                if self.utf8_continuations > 0 {
                    if self.pending[0] & 0xc0 == 0x80 {
                        self.utf8_continuations -= 1;
                        passthrough.push(self.pending.remove(0));
                        if self.pending.is_empty() {
                            break;
                        }
                        continue;
                    }
                    self.utf8_continuations = 0;
                }
                match inspect(&self.pending) {
                    Candidate::Incomplete => break,
                    Candidate::Unrelated => {
                        let first = self.pending.remove(0);
                        passthrough.push(first);
                        self.utf8_continuations = utf8_trailing_bytes(first);
                        if self.pending.is_empty() {
                            break;
                        }
                    }
                    Candidate::Graphics(body) => {
                        if is_matching_reply(body, self.image_id) && self.support.is_none() {
                            let message = body
                                .iter()
                                .position(|&byte| byte == b';')
                                .map(|separator| &body[separator + 1..]);
                            self.support =
                                Some(if self.require_ok && message != Some(b"OK".as_slice()) {
                                    GraphicsSupport::Unsupported
                                } else {
                                    GraphicsSupport::Supported
                                });
                            decision = self.support;
                            self.pending.clear();
                        } else {
                            passthrough.append(&mut self.pending);
                        }
                        break;
                    }
                    Candidate::DeviceAttributes => {
                        if self.support.is_none() {
                            self.support = Some(GraphicsSupport::Unsupported);
                            decision = self.support;
                        }
                        self.complete = true;
                        self.pending.clear();
                        break;
                    }
                }
            }
        }
        decision
    }

    pub fn support(&self) -> Option<GraphicsSupport> {
        self.support
    }

    /// Stop probing and forward any incomplete escape-sequence candidate.
    pub fn finish(&mut self, passthrough: &mut Vec<u8>) -> Option<GraphicsSupport> {
        passthrough.append(&mut self.pending);
        self.complete = true;
        self.support
    }
}

enum Candidate<'a> {
    Incomplete,
    Unrelated,
    Graphics(&'a [u8]),
    DeviceAttributes,
}

fn inspect(bytes: &[u8]) -> Candidate<'_> {
    let (start, graphics) = match bytes {
        [0x1b] => return Candidate::Incomplete,
        [0x1b, b'_'] | [0x1b, b'['] => return Candidate::Incomplete,
        [0x1b, b'_', b'G', ..] => (3, true),
        [0x1b, b'[', b'?', ..] => (3, false),
        [0x9f] | [0x9b] => return Candidate::Incomplete,
        [0x9f, b'G', ..] => (2, true),
        [0x9b, b'?', ..] => (2, false),
        _ => return Candidate::Unrelated,
    };
    if graphics {
        if bytes.len() > MAX_GRAPHICS_REPLY_BYTES {
            return Candidate::Unrelated;
        }
        if bytes.last() == Some(&0x9c) {
            return Candidate::Graphics(&bytes[start..bytes.len() - 1]);
        }
        if bytes.ends_with(b"\x1b\\") {
            return Candidate::Graphics(&bytes[start..bytes.len() - 2]);
        }
        return Candidate::Incomplete;
    }
    if bytes.len() > MAX_DEVICE_ATTRIBUTES_BYTES {
        return Candidate::Unrelated;
    }
    for &byte in &bytes[start..] {
        match byte {
            b'0'..=b'9' | b';' => {}
            b'c' if bytes.last() == Some(&b'c') => return Candidate::DeviceAttributes,
            _ => return Candidate::Unrelated,
        }
    }
    Candidate::Incomplete
}

fn is_matching_reply(body: &[u8], image_id: NonZeroU32) -> bool {
    let Some(separator) = body.iter().position(|&byte| byte == b';') else {
        return false;
    };
    let expected = format!("i={image_id}");
    body[..separator] == *expected.as_bytes()
        && !body[separator + 1..].is_empty()
        && body[separator + 1..]
            .iter()
            .all(|&byte| (b' '..=b'~').contains(&byte))
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

    fn probe() -> GraphicsCapabilityProbe {
        GraphicsCapabilityProbe::new(NonZeroU32::new(31).unwrap())
    }

    #[test]
    fn query_uses_direct_data_and_da_barrier() {
        assert_eq!(
            probe().request_bytes(),
            b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c"
        );
    }

    #[test]
    fn every_split_preserves_keys_and_consumes_both_replies() {
        let input = b"a\x1b_Gi=31;OK\x1b\\b\x1b[?1;2cc";
        for split in 0..=input.len() {
            let mut probe = probe();
            let mut forwarded = Vec::new();
            let first = probe.advance(&input[..split], &mut forwarded);
            let second = probe.advance(&input[split..], &mut forwarded);
            assert_eq!(
                first.or(second),
                Some(GraphicsSupport::Supported),
                "split {split}"
            );
            assert_eq!(probe.support(), Some(GraphicsSupport::Supported));
            assert_eq!(forwarded, b"abc", "split {split}");
        }
        let mut probe = probe();
        let mut forwarded = Vec::new();
        let mut decision = None;
        for &byte in input {
            decision = probe.advance(&[byte], &mut forwarded).or(decision);
        }
        assert_eq!(decision, Some(GraphicsSupport::Supported));
        assert_eq!(forwarded, b"abc");
    }

    #[test]
    fn da_first_means_unsupported_and_later_bytes_pass_through() {
        let mut probe = probe();
        let mut forwarded = Vec::new();
        assert_eq!(
            probe.advance(b"x\x1b[?62;4c\x1b_Gi=31;OK\x1b\\", &mut forwarded),
            Some(GraphicsSupport::Unsupported)
        );
        assert_eq!(forwarded, b"x\x1b_Gi=31;OK\x1b\\");
    }

    #[test]
    fn error_reply_still_proves_support_and_ignores_unrelated_ids() {
        let mut probe = probe();
        let mut forwarded = Vec::new();
        assert_eq!(
            probe.advance(
                b"\x1b_Gi=32;OK\x1b\\\x1b_Gi=31;EINVAL: test\x1b\\\x1b[?1;2c",
                &mut forwarded
            ),
            Some(GraphicsSupport::Supported)
        );
        assert_eq!(forwarded, b"\x1b_Gi=32;OK\x1b\\");
    }

    #[test]
    fn strict_transport_probe_requires_ok() {
        let id = NonZeroU32::new(32).unwrap();
        for (reply, expected) in [
            (
                b"\x1b_Gi=32;OK\x1b\\".as_slice(),
                GraphicsSupport::Supported,
            ),
            (
                b"\x1b_Gi=32;EBADF:Failed to read image file\x1b\\".as_slice(),
                GraphicsSupport::Unsupported,
            ),
        ] {
            let mut probe = GraphicsCapabilityProbe::new_strict(id);
            let mut forwarded = Vec::new();
            assert_eq!(probe.advance(reply, &mut forwarded), Some(expected));
            assert!(forwarded.is_empty());
            probe.advance(b"\x1b[?1;2c", &mut forwarded);
            assert!(probe.complete());
        }
    }

    #[test]
    fn c1_replies_work_without_consuming_utf8_continuation_bytes() {
        let mut probe = probe();
        let mut forwarded = Vec::new();
        assert_eq!(
            probe.advance(b"\xc2\x9fG\x9fGi=31;OK\x9c\x9b?1;2c", &mut forwarded),
            Some(GraphicsSupport::Supported)
        );
        assert_eq!(forwarded, b"\xc2\x9fG");
    }

    #[test]
    fn rejected_prefix_does_not_turn_following_utf8_into_a_control() {
        let mut probe = probe();
        let mut forwarded = Vec::new();
        assert_eq!(
            probe.advance(b"\x1b\xc2\x9fGi=31;OK\x9c\x1b[?1;2c", &mut forwarded),
            Some(GraphicsSupport::Unsupported)
        );
        assert_eq!(forwarded, b"\x1b\xc2\x9fGi=31;OK\x9c");
    }

    #[test]
    fn three_byte_utf8_continuations_are_not_c1_controls() {
        let mut probe = probe();
        let mut forwarded = Vec::new();
        assert_eq!(
            probe.advance(b"\xe2\x9f\x9b?1;2c\x1b[?1;2c", &mut forwarded),
            Some(GraphicsSupport::Unsupported)
        );
        assert_eq!(forwarded, b"\xe2\x9f\x9b?1;2c");
    }

    #[test]
    fn incomplete_and_malformed_candidates_are_forwarded() {
        let mut probe = probe();
        let mut forwarded = Vec::new();
        assert_eq!(probe.advance(b"\x1b[A\x1b_Gi=31;", &mut forwarded), None);
        assert_eq!(forwarded, b"\x1b[A");
        assert_eq!(probe.finish(&mut forwarded), None);
        assert_eq!(forwarded, b"\x1b[A\x1b_Gi=31;");
        assert_eq!(probe.advance(b"tail", &mut forwarded), None);
        assert_eq!(forwarded, b"\x1b[A\x1b_Gi=31;tail");
    }

    #[test]
    fn oversized_replies_do_not_grow_without_bound() {
        let mut probe = probe();
        let mut forwarded = Vec::new();
        let mut input = b"\x1b_G".to_vec();
        input.extend(vec![b'x'; MAX_GRAPHICS_REPLY_BYTES]);
        probe.advance(&input, &mut forwarded);
        assert!(probe.pending.len() <= MAX_GRAPHICS_REPLY_BYTES);
        let mut complete = Vec::new();
        probe.finish(&mut complete);
        forwarded.extend(complete);
        assert_eq!(forwarded, input);
    }
}
