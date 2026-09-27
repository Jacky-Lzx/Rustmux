//! Bounded, pane-local reassembly of Kitty direct-data image transfers.
//!
//! This is a data boundary, not an image decoder or a graphics capability
//! implementation. Callers validate assembled blobs before display or replies.

use crate::graphics::MAX_GRAPHICS_COMMAND_BYTES;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use flate2::bufread::ZlibDecoder;
use std::collections::BTreeMap;
use std::io::Read;

/// Kitten icat's largest observed encoded chunk; larger APCs remain bounded.
pub const MAX_ENCODED_CHUNK_BYTES: usize = 128 * 1024;
/// Cap one in-progress image independently of its number of chunks.
pub const MAX_DIRECT_TRANSFER_BYTES: usize = 16 * 1024 * 1024;
/// Bound the declared expanded size of a compressed raw image. Such transfers
/// remain compressed in the store and are decoded a row at a time.
pub const MAX_STREAMED_RAW_BYTES: usize = 256 * 1024 * 1024;

type Controls = BTreeMap<u8, Vec<u8>>;

#[derive(Debug, Eq, PartialEq)]
pub struct AssembledDirectTransfer {
    controls: Controls,
    pub data: Vec<u8>,
    streamed_raw_zlib: bool,
}

impl AssembledDirectTransfer {
    /// The first chunk's controls, with a final chunk's optional `q` override.
    pub fn control(&self, key: u8) -> Option<&[u8]> {
        self.controls.get(&key).map(Vec::as_slice)
    }

    pub(crate) fn streamed_raw_zlib(&self) -> bool {
        self.streamed_raw_zlib
    }

    /// Replies for queries and data-only uploads cover only these direct-data
    /// controls. An unrecognized key must not receive a false `OK`.
    pub(crate) fn supported_data_only_controls(&self) -> bool {
        self.controls.keys().all(|key| {
            matches!(
                key,
                b'a' | b'f' | b'i' | b'I' | b'm' | b'o' | b'q' | b's' | b't' | b'v' | b'N' | b'S'
            )
        }) && self
            .control(b'N')
            .is_none_or(|value| parse_decimal(value).is_some())
    }

    /// `a=T` accepts the data keys plus only the placement geometry this
    /// store actually implements. Reject unknown keys before any mutation.
    pub(crate) fn supported_display_controls(&self) -> bool {
        self.controls.keys().all(|key| {
            matches!(
                key,
                b'a' | b'f'
                    | b'i'
                    | b'I'
                    | b'm'
                    | b'o'
                    | b'q'
                    | b's'
                    | b't'
                    | b'v'
                    | b'N'
                    | b'p'
                    | b'c'
                    | b'r'
                    | b'z'
                    | b'C'
                    | b'x'
                    | b'y'
                    | b'w'
                    | b'h'
                    | b'X'
                    | b'Y'
                    | b'U'
                    | b'S'
            )
        }) && self
            .control(b'N')
            .is_none_or(|value| parse_decimal(value).is_some())
            && matches!(self.control(b'U'), None | Some(b"0" | b"1"))
    }
}

#[derive(Debug)]
struct Pending {
    controls: Controls,
    data: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct DirectTransferAssembler {
    pending: Option<Pending>,
}

impl DirectTransferAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.pending = None;
    }

    /// Accept one complete APC G command from `GraphicsFramer`. Invalid or
    /// unsupported commands abort an in-progress transfer. Only `t=d`,
    /// optionally zlib-compressed `a=t/T/q` data is assembled; no filesystem
    /// access occurs.
    pub fn accept(&mut self, command: &[u8]) -> Option<AssembledDirectTransfer> {
        let Some((controls, encoded)) = parse_command(command) else {
            self.reset();
            return None;
        };
        let more = match controls.get(&b'm').map(Vec::as_slice) {
            None | Some(b"0") => false,
            Some(b"1") => true,
            _ => {
                self.reset();
                return None;
            }
        };
        if more && encoded.len() % 4 != 0 {
            self.reset();
            return None;
        }
        let Some(chunk) = decode_base64(encoded) else {
            self.reset();
            return None;
        };

        if let Some(mut pending) = self.pending.take() {
            // Later chunks inherit image parameters. Kitten icat repeats the
            // same action on continuation chunks and omits m on the last one.
            if controls
                .get(&b'a')
                .is_some_and(|action| pending.controls.get(&b'a') != Some(action))
                || controls
                    .keys()
                    .any(|key| !matches!(key, b'a' | b'm' | b'q'))
                || !valid_quiet(&controls)
                || !append_bounded(&mut pending.data, &chunk)
            {
                return None;
            }
            if let Some(quiet) = controls.get(&b'q') {
                pending.controls.insert(b'q', quiet.clone());
            }
            if more {
                self.pending = Some(pending);
                return None;
            }
            return finish(pending);
        }

        if !valid_first(&controls) {
            return None;
        }
        let mut pending = Pending {
            controls,
            data: Vec::new(),
        };
        if !append_bounded(&mut pending.data, &chunk) {
            return None;
        }
        if more {
            self.pending = Some(pending);
            None
        } else {
            finish(pending)
        }
    }
}

fn decode_base64(encoded: &[u8]) -> Option<Vec<u8>> {
    // Kitty chunks after encoding, so only the final chunk may omit padding.
    // The caller separately requires all non-final chunks to be 4-byte aligned.
    if encoded.ends_with(b"=") {
        STANDARD.decode(encoded).ok()
    } else {
        STANDARD_NO_PAD.decode(encoded).ok()
    }
}

fn parse_command(command: &[u8]) -> Option<(Controls, &[u8])> {
    if command.len() > MAX_GRAPHICS_COMMAND_BYTES {
        return None;
    }
    let body = if let Some(bytes) = command.strip_prefix(b"\x1b_G") {
        bytes.strip_suffix(b"\x1b\\")?
    } else {
        let bytes = command.strip_prefix(&[0x9f, b'G'])?;
        bytes
            .strip_suffix(&[0x9c])
            .or_else(|| bytes.strip_suffix(b"\x1b\\"))?
    };
    let separator = body.iter().position(|&byte| byte == b';')?;
    let encoded = &body[separator + 1..];
    if encoded.len() > MAX_ENCODED_CHUNK_BYTES {
        return None;
    }
    let mut controls = Controls::new();
    if separator > 0 {
        for pair in body[..separator].split(|&byte| byte == b',') {
            let equals = pair.iter().position(|&byte| byte == b'=')?;
            let (key, with_equals) = pair.split_at(equals);
            let value = &with_equals[1..];
            if key.len() != 1
                || !key[0].is_ascii_alphabetic()
                || value.is_empty()
                || !value.iter().all(u8::is_ascii_graphic)
                || controls.insert(key[0], value.to_vec()).is_some()
            {
                return None;
            }
        }
    }
    Some((controls, encoded))
}

/// Recognize a complete non-direct transfer for a child-facing error reply.
/// This validates only framing and metadata; the decoded path/name is never
/// opened or otherwise inspected. Incomplete chunks and malformed commands
/// remain silent because they cannot be correlated reliably.
pub(crate) fn unsupported_medium_controls(command: &[u8]) -> Option<Controls> {
    let (controls, encoded) = parse_command(command)?;
    if !matches!(
        controls.get(&b't').map(Vec::as_slice),
        Some(b"f" | b"t" | b"s")
    ) || !matches!(controls.get(&b'm').map(Vec::as_slice), None | Some(b"0"))
        || !matches!(
            controls.get(&b'a').map(Vec::as_slice),
            None | Some(b"t" | b"T" | b"q")
        )
        || !matches!(
            controls.get(&b'f').map(Vec::as_slice),
            None | Some(b"24" | b"32" | b"100")
        )
        || !valid_quiet(&controls)
        || encoded.is_empty()
        || decode_base64(encoded).is_none()
    {
        return None;
    }
    Some(controls)
}

fn valid_first(controls: &Controls) -> bool {
    matches!(
        controls.get(&b'a').map(Vec::as_slice),
        None | Some(b"t" | b"T" | b"q")
    ) && matches!(controls.get(&b't').map(Vec::as_slice), None | Some(b"d"))
        && matches!(
            controls.get(&b'f').map(Vec::as_slice),
            None | Some(b"24" | b"32" | b"100")
        )
        && matches!(controls.get(&b'o').map(Vec::as_slice), None | Some(b"z"))
        && controls.get(&b'S').is_none_or(|value| {
            (controls.get(&b'o').is_some()
                || controls.get(&b'a').is_some_and(|action| action == b"q"))
                && parse_positive(value).is_some_and(|size| {
                    let limit = if controls.get(&b'o').map(Vec::as_slice) == Some(b"z")
                        && matches!(
                            controls.get(&b'f').map(Vec::as_slice),
                            None | Some(b"24" | b"32")
                        ) {
                        MAX_STREAMED_RAW_BYTES
                    } else {
                        MAX_DIRECT_TRANSFER_BYTES
                    };
                    size as usize <= limit
                })
        })
        && valid_quiet(controls)
        && b"sv".iter().copied().all(|key| {
            controls
                .get(&key)
                .is_none_or(|value| parse_positive(value).is_some())
        })
        && controls
            .get(&b'i')
            .is_none_or(|value| parse_decimal(value).is_some())
        && controls
            .get(&b'I')
            .is_none_or(|value| parse_decimal(value).is_some())
}

fn valid_quiet(controls: &Controls) -> bool {
    matches!(
        controls.get(&b'q').map(Vec::as_slice),
        None | Some(b"0" | b"1" | b"2")
    )
}

fn parse_positive(value: &[u8]) -> Option<u32> {
    let value = parse_decimal(value)?;
    (value > 0).then_some(value)
}

fn parse_decimal(value: &[u8]) -> Option<u32> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(value).ok()?.parse::<u32>().ok()
}

fn append_bounded(data: &mut Vec<u8>, chunk: &[u8]) -> bool {
    if data
        .len()
        .checked_add(chunk.len())
        .is_none_or(|size| size > MAX_DIRECT_TRANSFER_BYTES)
    {
        return false;
    }
    data.extend_from_slice(chunk);
    true
}

fn finish(pending: Pending) -> Option<AssembledDirectTransfer> {
    let Pending { controls, data } = pending;
    let format = controls.get(&b'f').map(Vec::as_slice).unwrap_or(b"32");
    let raw_size = if format != b"100" {
        let width = parse_positive(controls.get(&b's')?)? as usize;
        let height = parse_positive(controls.get(&b'v')?)? as usize;
        let channels = if format == b"24" { 3 } else { 4 };
        Some(width.checked_mul(height)?.checked_mul(channels)?)
    } else {
        None
    };
    let declared_size = controls
        .get(&b'S')
        .and_then(|value| parse_positive(value))
        .map(|size| size as usize);
    let expected_size = raw_size.or(declared_size);
    let streamed_raw_zlib = controls.contains_key(&b'o')
        && raw_size.is_some_and(|size| size > MAX_DIRECT_TRANSFER_BYTES);
    let data = if streamed_raw_zlib {
        let limit = raw_size?;
        if limit > MAX_STREAMED_RAW_BYTES || declared_size.is_some_and(|size| size != limit) {
            return None;
        }
        data
    } else if controls.contains_key(&b'o') {
        let limit = expected_size?;
        if limit > MAX_DIRECT_TRANSFER_BYTES || declared_size.is_some_and(|size| size != limit) {
            return None;
        }
        let mut decoder = ZlibDecoder::new(data.as_slice());
        let mut decoded = Vec::new();
        (&mut decoder)
            .take(limit as u64 + 1)
            .read_to_end(&mut decoded)
            .ok()?;
        if decoded.len() != limit || decoder.total_in() != data.len() as u64 {
            return None;
        }
        decoded
    } else {
        if raw_size.is_some_and(|size| size != data.len()) {
            return None;
        }
        if declared_size.is_some_and(|size| size != data.len()) {
            return None;
        }
        data
    };
    Some(AssembledDirectTransfer {
        controls,
        data,
        streamed_raw_zlib,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{GraphicsEvent, GraphicsFramer};
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn direct(controls: &str, data: &[u8]) -> Vec<u8> {
        format!("\x1b_G{controls};{}\x1b\\", STANDARD.encode(data)).into_bytes()
    }

    #[test]
    fn framed_rgba_transfer_preserves_controls_and_decoded_data() {
        let mut framer = GraphicsFramer::new();
        let mut assembler = DirectTransferAssembler::new();
        let input = b"left\x1b_Ga=T,f=32,s=1,v=1,i=7,p=9;AQIDBA==\x1b\\right";
        let mut completed = None;
        let mut terminal = Vec::new();
        for byte in input {
            for event in framer.advance(std::slice::from_ref(byte)) {
                match event {
                    GraphicsEvent::Terminal(bytes) => terminal.extend(bytes),
                    GraphicsEvent::Command(bytes) => completed = assembler.accept(&bytes),
                }
            }
        }
        assert_eq!(terminal, b"leftright");
        let image = completed.unwrap();
        assert_eq!(image.data, [1, 2, 3, 4]);
        assert_eq!(image.control(b'a'), Some(b"T".as_slice()));
        assert_eq!(image.control(b'i'), Some(b"7".as_slice()));
        assert_eq!(image.control(b'p'), Some(b"9".as_slice()));
    }

    #[test]
    fn png_chunks_inherit_first_controls_and_accept_final_quiet_override() {
        let mut assembler = DirectTransferAssembler::new();
        assert!(
            assembler
                .accept(b"\x1b_Ga=T,f=100,i=42,m=1;QUJD\x1b\\")
                .is_none()
        );
        assert!(assembler.accept(b"\x1b_Gm=1;REVG\x1b\\").is_none());
        let image = assembler.accept(b"\x1b_Gm=0,q=2;Rw==\x1b\\").unwrap();
        assert_eq!(image.data, b"ABCDEFG");
        assert_eq!(image.control(b'f'), Some(b"100".as_slice()));
        assert_eq!(image.control(b'i'), Some(b"42".as_slice()));
        assert_eq!(image.control(b'q'), Some(b"2".as_slice()));
        assert!(
            assembler
                .accept(b"\x9fGa=t,f=24,s=1,v=1,i=0;AQID\x9c")
                .is_some()
        );
        assert!(
            assembler
                .accept(b"\x9fGa=t,f=24,s=1,v=1;AQID\x1b\\")
                .is_some()
        );
    }

    #[test]
    fn rejects_unsupported_media_and_malformed_fields() {
        let mut assembler = DirectTransferAssembler::new();
        for command in [
            b"\x1b_Ga=T,t=f,f=100;QUJD\x1b\\".as_slice(),
            b"\x1b_Ga=T,o=x,f=100;QUJD\x1b\\",
            b"\x1b_Ga=p,i=1;\x1b\\",
            b"\x1b_Gf=100,f=24;QUJD\x1b\\",
            b"\x1b_Gf=100,;QUJD\x1b\\",
            b"\x1b_Gf=100,i=4294967296;QUJD\x1b\\",
            b"\x1b_Gf=100,q=3;QUJD\x1b\\",
            b"\x1b_Gf=100;***\x1b\\",
        ] {
            assert!(assembler.accept(command).is_none(), "{command:?}");
        }
        assert!(assembler.accept(b"\x1b_Gf=100;QUJD\x1b\\").is_some());
    }

    #[test]
    fn zlib_direct_raw_and_png_transfers_decode_before_validation() {
        let mut assembler = DirectTransferAssembler::new();
        let rgb = direct("a=t,o=z,f=24,s=1,v=1,i=7", &zlib(&[1, 2, 3]));
        let image = assembler.accept(&rgb).unwrap();
        assert_eq!(image.data, [1, 2, 3]);
        assert_eq!(image.control(b'o'), Some(b"z".as_slice()));
        assert!(image.supported_data_only_controls());

        let rgba = zlib(&[1, 2, 3, 4]);
        let encoded = STANDARD.encode(rgba);
        let first = format!("\x1b_Ga=T,o=z,f=32,s=1,v=1,i=8,m=1;{}\x1b\\", &encoded[..4]);
        let last = format!("\x1b_Gm=0,q=2;{}\x1b\\", &encoded[4..]);
        assert!(assembler.accept(first.as_bytes()).is_none());
        let image = assembler.accept(last.as_bytes()).unwrap();
        assert_eq!(image.data, [1, 2, 3, 4]);
        assert_eq!(image.control(b'q'), Some(b"2".as_slice()));
        assert!(image.supported_display_controls());

        let png = direct("a=t,o=z,f=100,S=3,i=9", &zlib(b"PNG"));
        assert_eq!(assembler.accept(&png).unwrap().data, b"PNG");
    }

    #[test]
    fn kitten_direct_probe_accepts_matching_uncompressed_size_only_for_queries() {
        let mut assembler = DirectTransferAssembler::new();
        let query = direct("a=q,t=d,f=24,i=1,s=1,v=1,S=3", b"123");
        let image = assembler.accept(&query).unwrap();
        assert_eq!(image.data, b"123");
        assert_eq!(image.control(b'S'), Some(b"3".as_slice()));
        assert!(
            assembler
                .accept(&direct("a=q,t=d,f=24,i=1,s=1,v=1,S=4", b"123"))
                .is_none()
        );
        assert!(
            assembler
                .accept(&direct("a=T,t=d,f=24,i=1,s=1,v=1,S=3", b"123"))
                .is_none()
        );
    }

    #[test]
    fn final_direct_chunk_accepts_kitty_unpadded_base64() {
        let mut assembler = DirectTransferAssembler::new();
        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
        let encoded = STANDARD_NO_PAD.encode(zlib(&rgba));
        assert_ne!(encoded.len() % 4, 0);
        let command = format!("\x1b_Ga=T,o=z,f=32,s=2,v=1;{encoded}\x1b\\");
        assert_eq!(assembler.accept(command.as_bytes()).unwrap().data, rgba);
        assert!(assembler.accept(b"\x1b_Gf=100;A\x1b\\").is_none());
        assert!(assembler.accept(b"\x1b_Gf=100;QUJ=\x1b\\").is_none());
    }

    #[test]
    fn kitten_continuations_repeat_action_and_omit_final_chunk_marker() {
        let mut assembler = DirectTransferAssembler::new();
        assert!(
            assembler
                .accept(b"\x1b_Ga=T,f=24,s=3,v=1,m=1;AQID\x1b\\")
                .is_none()
        );
        assert!(assembler.accept(b"\x1b_Ga=T,q=2,m=1;BAUG\x1b\\").is_none());
        let image = assembler.accept(b"\x1b_Ga=T,q=2;BwgJ\x1b\\").unwrap();
        assert_eq!(image.data, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(image.control(b'q'), Some(b"2".as_slice()));
        assert!(
            assembler
                .accept(b"\x1b_Ga=T,f=24,s=1,v=1,m=1;AQID\x1b\\")
                .is_none()
        );
        assert!(assembler.accept(b"\x1b_Ga=t;BAUG\x1b\\").is_none());
    }

    #[test]
    fn kitten_sized_chunk_passes_framer_and_assembler() {
        let raw = vec![7; 98_304];
        let command = direct("a=T,f=24,s=32768,v=1", &raw);
        assert_eq!(STANDARD.encode(&raw).len(), MAX_ENCODED_CHUNK_BYTES);
        let mut framer = GraphicsFramer::new();
        let mut assembler = DirectTransferAssembler::new();
        let events = framer.advance(&command);
        assert_eq!(events.len(), 1);
        let GraphicsEvent::Command(command) = &events[0] else {
            panic!("Kitten-sized chunk must be framed");
        };
        assert_eq!(assembler.accept(command).unwrap().data, raw);
    }

    #[test]
    fn zlib_direct_rejects_bad_stream_size_and_controls_then_recovers() {
        let mut assembler = DirectTransferAssembler::new();
        let mut corrupt = zlib(&[1, 2, 3, 4]);
        *corrupt.last_mut().unwrap() ^= 1;
        let valid = zlib(&[1, 2, 3, 4]);
        let mut trailing = valid.clone();
        trailing.push(0);
        for command in [
            direct("a=t,o=z,f=32,s=1,v=1", &corrupt),
            direct("a=t,o=z,f=32,s=1,v=1", &valid[..valid.len() - 1]),
            direct("a=t,o=z,f=32,s=1,v=1", &trailing),
            direct("a=t,o=z,f=32,s=2,v=1", &valid),
            direct("a=t,o=z,f=32,s=1,v=1,S=3", &valid),
            direct("a=t,o=z,f=100", &zlib(b"PNG")),
            direct("a=t,o=z,f=100,S=4", &zlib(b"PNG")),
            direct("a=t,o=z,f=100,S=16777217", &zlib(b"PNG")),
            direct("a=t,o=z,f=100,S=bad", &zlib(b"PNG")),
            direct("a=t,f=100,S=3", b"PNG"),
        ] {
            assert!(assembler.accept(&command).is_none(), "{command:?}");
        }
        assert!(
            assembler
                .accept(&direct("a=t,o=z,f=32,s=1,v=1", &valid))
                .is_some()
        );
    }

    #[test]
    fn zlib_direct_stops_at_declared_output_bound() {
        let mut controls = Controls::new();
        controls.insert(b'f', b"100".to_vec());
        controls.insert(b'o', b"z".to_vec());
        controls.insert(b'S', b"4".to_vec());
        assert!(
            finish(Pending {
                controls,
                data: zlib(&vec![42; 4096])
            })
            .is_none()
        );
    }

    #[test]
    fn validates_raw_dimensions_and_rejects_chunk_parameter_changes() {
        let mut assembler = DirectTransferAssembler::new();
        assert!(
            assembler
                .accept(b"\x1b_Gf=24,s=1,v=1;AQIDBA==\x1b\\")
                .is_none()
        );
        assert!(assembler.accept(b"\x1b_Gf=24,s=0,v=1;AQID\x1b\\").is_none());
        assert!(
            assembler
                .accept(b"\x1b_Gf=24,s=4294967295,v=4294967295;AQID\x1b\\")
                .is_none()
        );
        assert!(assembler.accept(b"\x1b_Gf=100,m=1;QUJD\x1b\\").is_none());
        assert!(assembler.accept(b"\x1b_Gf=100,m=0;RA==\x1b\\").is_none());
        assert!(assembler.accept(b"\x1b_Gf=24,s=1,v=1;AQID\x1b\\").is_some());
    }

    #[test]
    fn malformed_chunk_aborts_pending_transfer_and_next_one_recovers() {
        let mut assembler = DirectTransferAssembler::new();
        assert!(assembler.accept(b"\x1b_Gf=100,m=1;QUJD\x1b\\").is_none());
        assert!(assembler.accept(b"\x1b_Gm=0;%%%\x1b\\").is_none());
        assert!(assembler.accept(b"\x1b_Gm=0;RA==\x1b\\").is_none());
        assert!(assembler.accept(b"\x1b_Gf=100;Rk9P\x1b\\").is_some());
        assert!(assembler.accept(b"\x1b_Gf=100,m=1;QUJD\x1b\\").is_none());
        assembler.reset();
        assert!(assembler.accept(b"\x1b_Gm=0;RA==\x1b\\").is_none());
    }

    #[test]
    fn chunk_and_total_size_limits_are_independent() {
        let mut assembler = DirectTransferAssembler::new();
        let mut command = b"\x1b_Gf=100;".to_vec();
        command.extend(std::iter::repeat_n(b'A', MAX_ENCODED_CHUNK_BYTES + 4));
        command.extend_from_slice(b"\x1b\\");
        assert!(assembler.accept(&command).is_none());
        assert!(assembler.accept(b"\x1b_Gf=100,m=1;QUJ\x1b\\").is_none());
        let mut full = vec![0; MAX_DIRECT_TRANSFER_BYTES];
        assert!(!append_bounded(&mut full, b"x"));
        assert_eq!(full.len(), MAX_DIRECT_TRANSFER_BYTES);
    }

    #[test]
    fn compressed_raw_above_legacy_limit_stays_compressed() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let raw = vec![0; 2048 * 2049 * 4];
        let compressed = zlib(&raw);
        let command = format!(
            "\x1b_Ga=T,o=z,s=2048,v=2049,i=7;{}\x1b\\",
            STANDARD.encode(&compressed)
        );
        let transfer = DirectTransferAssembler::new()
            .accept(command.as_bytes())
            .unwrap();
        assert!(transfer.streamed_raw_zlib());
        assert_eq!(transfer.data, compressed);
    }
}
