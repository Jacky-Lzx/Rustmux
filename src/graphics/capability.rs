//! Opt-in detection of Kitty graphics support on an outer terminal.
//!
//! This is a transport-independent byte filter. The caller sends the query,
//! feeds terminal input before its normal key parser, and forwards the returned
//! non-response bytes. The terminal runtime probes each new attachment.

use std::num::NonZeroU32;
use std::time::{Duration, Instant};

const MAX_GRAPHICS_REPLY_BYTES: usize = 1024;
const MAX_DEVICE_ATTRIBUTES_BYTES: usize = 64;
const OUTER_REPLY_PREFIX_TIMEOUT: Duration = Duration::from_millis(25);
const OUTER_GRAPHICS_REPLY_TIMEOUT: Duration = Duration::from_millis(500);

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

/// Consume replies for Rustmux-owned outer image IDs. Cached placements use
/// q=1, so an error reply means their source image must be uploaded again.
#[derive(Default)]
pub struct OuterImageReplies {
    pending: Vec<u8>,
    pending_since: Option<Instant>,
    utf8_continuations: u8,
}

impl OuterImageReplies {
    pub fn next_expiry(&self) -> Option<Instant> {
        self.pending_since.map(|since| {
            let timeout =
                if self.pending.starts_with(b"\x1b_G") || self.pending.starts_with(b"\x9fG") {
                    OUTER_GRAPHICS_REPLY_TIMEOUT
                } else {
                    OUTER_REPLY_PREFIX_TIMEOUT
                };
            since + timeout
        })
    }

    /// A standalone Esc is also keyboard input; release a stalled candidate
    /// promptly rather than waiting forever for a Kitty reply suffix.
    pub fn expire(&mut self, now: Instant, passthrough: &mut Vec<u8>) {
        if self.next_expiry().is_some_and(|deadline| now >= deadline) {
            passthrough.append(&mut self.pending);
            self.pending_since = None;
        }
    }

    pub fn advance(&mut self, input: &[u8], passthrough: &mut Vec<u8>) -> Vec<u32> {
        let mut failed = Vec::new();
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
                if !matches!(byte, 0x1b | 0x9f) {
                    passthrough.push(byte);
                    self.utf8_continuations = utf8_trailing_bytes(byte);
                    continue;
                }
            }
            self.pending.push(byte);
            loop {
                match inspect(&self.pending) {
                    Candidate::Incomplete => break,
                    Candidate::Unrelated | Candidate::DeviceAttributes => {
                        let first = self.pending.remove(0);
                        passthrough.push(first);
                        self.utf8_continuations = utf8_trailing_bytes(first);
                        if self.pending.is_empty() {
                            break;
                        }
                    }
                    Candidate::Graphics(body) => {
                        if let Some((image_id, is_error)) = owned_image_reply(body) {
                            if is_error {
                                failed.push(image_id);
                            }
                            self.pending.clear();
                        } else {
                            passthrough.append(&mut self.pending);
                        }
                        break;
                    }
                }
            }
        }
        if self.pending.is_empty() {
            self.pending_since = None;
        } else if self.pending_since.is_none() {
            self.pending_since = Some(Instant::now());
        }
        failed
    }
}

fn owned_image_reply(body: &[u8]) -> Option<(u32, bool)> {
    let separator = body.iter().position(|&byte| byte == b';')?;
    let image_id = body[..separator]
        .split(|&byte| byte == b',')
        .find_map(|control| {
            control
                .strip_prefix(b"i=")
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|value| value.parse::<u32>().ok())
        })?;
    if image_id < 0x8000_0000 || body[separator + 1..].is_empty() {
        return None;
    }
    Some((image_id, &body[separator + 1..] != b"OK"))
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

    #[test]
    fn outer_image_errors_are_filtered_across_input_chunks() {
        let input = b"a\x1b_Gi=2147483648,p=7;ENOENT:missing image\x1b\\b\x1b_Gi=31;OK\x1b\\c";
        for split in 0..=input.len() {
            let mut replies = OuterImageReplies::default();
            let mut passthrough = Vec::new();
            let mut failed = replies.advance(&input[..split], &mut passthrough);
            failed.extend(replies.advance(&input[split..], &mut passthrough));
            assert_eq!(failed, vec![0x8000_0000]);
            assert_eq!(passthrough, b"ab\x1b_Gi=31;OK\x1b\\c");
        }
    }

    #[test]
    fn unfinished_outer_reply_releases_keyboard_escape() {
        let mut replies = OuterImageReplies::default();
        let mut passthrough = Vec::new();
        assert!(replies.advance(b"\x1b", &mut passthrough).is_empty());
        assert!(passthrough.is_empty());
        replies.expire(
            Instant::now() + OUTER_REPLY_PREFIX_TIMEOUT,
            &mut passthrough,
        );
        assert_eq!(passthrough, b"\x1b");
        assert!(replies.next_expiry().is_none());
    }

    #[test]
    fn partial_graphics_reply_waits_longer_than_a_keyboard_escape() {
        let mut replies = OuterImageReplies::default();
        let mut passthrough = Vec::new();
        replies.advance(b"\x1b_Gi=2147483648;", &mut passthrough);
        replies.expire(
            Instant::now() + OUTER_REPLY_PREFIX_TIMEOUT,
            &mut passthrough,
        );
        assert!(passthrough.is_empty());
        replies.advance(b"ENOENT\x1b\\", &mut passthrough);
        assert!(passthrough.is_empty());
    }

    #[test]
    fn outer_reply_filter_preserves_utf8_and_arrow_keys() {
        let input = b"\xc3\x9f\x1b[D";
        for split in 0..=input.len() {
            let mut replies = OuterImageReplies::default();
            let mut passthrough = Vec::new();
            replies.advance(&input[..split], &mut passthrough);
            replies.advance(&input[split..], &mut passthrough);
            assert_eq!(passthrough, input);
        }
    }

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
