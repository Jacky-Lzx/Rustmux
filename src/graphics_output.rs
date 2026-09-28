//! Opt-in Kitty direct-RGBA and PNG output for a caller-controlled outer terminal.
//!
//! This module only encodes a placement. The terminal runtime separately
//! negotiates support, positions the cursor, deletes old placements, and
//! queues output for the running multiplexer.

use crate::graphics_decode::{DecodedImage, MAX_DECODED_IMAGE_BYTES};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::io::{self, Write};

// 3072 raw bytes produce exactly the protocol's 4096-byte Base64 limit.
const RAW_CHUNK_BYTES: usize = 3072;
const MORE_HEADER: &[u8] = b"\x1b_Gm=1;";
const FINAL_HEADER: &[u8] = b"\x1b_Gm=0;";
const APC_END: &[u8] = b"\x1b\\";

/// A validated RGBA image encoded once as PNG, ready for a Kitty `f=100`
/// direct transfer. Preparing it separately lets callers compare exact output
/// lengths without compressing the same image twice.
pub struct EncodedKittyPng {
    bytes: Vec<u8>,
}

impl EncodedKittyPng {
    pub fn from_rgba(image: &DecodedImage) -> io::Result<Self> {
        validate(image, 1)?;
        let opaque = image
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255);
        let mut output = BoundedPngBuffer::default();
        {
            let mut encoder = png::Encoder::new(&mut output, image.width, image.height);
            encoder.set_color(if opaque {
                png::ColorType::Rgb
            } else {
                png::ColorType::Rgba
            });
            encoder.set_depth(png::BitDepth::Eight);
            // Overlay encoding runs on the render path; favor responsiveness
            // over squeezing the last few bytes from an already smaller PNG.
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().map_err(io::Error::other)?;
            if opaque {
                // Keep only one converted row, not another full-sized RGB image.
                let width = usize::try_from(image.width).unwrap();
                let mut rgb_row = vec![0; width * 3];
                let mut stream = writer.stream_writer().map_err(io::Error::other)?;
                for rgba_row in image.pixels.chunks_exact(width * 4) {
                    for (rgba, rgb) in rgba_row
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(rgb_row.as_chunks_mut::<3>().0.iter_mut())
                    {
                        rgb.copy_from_slice(&rgba[..3]);
                    }
                    stream.write_all(&rgb_row)?;
                }
                stream.finish().map_err(io::Error::other)?;
            } else {
                writer
                    .write_image_data(&image.pixels)
                    .map_err(io::Error::other)?;
            }
            writer.finish().map_err(io::Error::other)?;
        }
        Ok(Self {
            bytes: output.bytes,
        })
    }

    pub fn placement_len(&self, image_id: u32, z_index: i32) -> io::Result<usize> {
        validate_id(image_id)?;
        let chunks = self.bytes.chunks(RAW_CHUNK_BYTES);
        let mut total = 0usize;
        for (index, raw) in chunks.enumerate() {
            let more = (index + 1) * RAW_CHUNK_BYTES < self.bytes.len();
            let header_len = if index == 0 {
                png_first_header(image_id, z_index, more).len()
            } else if more {
                MORE_HEADER.len()
            } else {
                FINAL_HEADER.len()
            };
            total = total
                .checked_add(header_len + raw.len().div_ceil(3) * 4 + APC_END.len())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "Kitty output length overflow")
                })?;
        }
        Ok(total)
    }

    /// Reject an over-budget placement before writing any bytes.
    pub fn write_with_limit(
        &self,
        image_id: u32,
        z_index: i32,
        max_bytes: usize,
        output: &mut impl Write,
    ) -> io::Result<()> {
        if self.placement_len(image_id, z_index)? > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Kitty placement exceeds output budget",
            ));
        }
        let mut chunks = self.bytes.chunks(RAW_CHUNK_BYTES).peekable();
        let mut first = true;
        while let Some(raw) = chunks.next() {
            let more = chunks.peek().is_some();
            if first {
                output.write_all(png_first_header(image_id, z_index, more).as_bytes())?;
                first = false;
            } else {
                output.write_all(if more { MORE_HEADER } else { FINAL_HEADER })?;
            }
            output.write_all(STANDARD.encode(raw).as_bytes())?;
            output.write_all(APC_END)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct BoundedPngBuffer {
    bytes: Vec<u8>,
}

impl Write for BoundedPngBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > MAX_DECODED_IMAGE_BYTES - self.bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PNG output exceeds image budget",
            ));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn png_first_header(image_id: u32, z_index: i32, more: bool) -> String {
    format!(
        "\x1b_Ga=T,f=100,i={image_id},z={z_index},C=1,q=2,m={};",
        u8::from(more)
    )
}

fn validate_id(image_id: u32) -> io::Result<()> {
    if image_id == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Kitty image ID",
        ));
    }
    Ok(())
}

/// Exact byte count of the encoded placement, including all APC wrappers.
/// Rejects the same invalid images and IDs as the writer without allocating
/// an encoded pixel buffer or writing any output.
pub fn kitty_rgba_placement_len(
    image: &DecodedImage,
    image_id: u32,
    z_index: i32,
) -> io::Result<usize> {
    validate(image, image_id)?;
    let mut chunks = image.pixels.chunks(RAW_CHUNK_BYTES).peekable();
    let mut total = 0usize;
    let mut first = true;
    while let Some(raw) = chunks.next() {
        let header_len = if first {
            first = false;
            first_header(image, image_id, z_index, chunks.peek().is_some()).len()
        } else {
            if chunks.peek().is_some() {
                MORE_HEADER.len()
            } else {
                FINAL_HEADER.len()
            }
        };
        let encoded_len = raw.len().div_ceil(3) * 4;
        total = total
            .checked_add(header_len + encoded_len + APC_END.len())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Kitty output length overflow")
            })?;
    }
    Ok(total)
}

/// Write only if the complete encoded placement fits `max_bytes`.
///
/// Exceeding the budget returns `InvalidInput` before the first byte is
/// written. Ordinary writer errors can still leave a partial transfer.
pub fn write_kitty_rgba_placement_with_limit(
    image: &DecodedImage,
    image_id: u32,
    z_index: i32,
    max_bytes: usize,
    output: &mut impl Write,
) -> io::Result<()> {
    if kitty_rgba_placement_len(image, image_id, z_index)? > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Kitty placement exceeds output budget",
        ));
    }
    write_kitty_rgba_placement(image, image_id, z_index, output)
}

/// Write one `a=T` placement at the outer terminal's current cursor.
///
/// The image ID must be nonzero. `C=1` leaves the outer cursor in place and
/// `q=2` suppresses terminal replies. The caller must have established Kitty
/// support, positioned the cursor, and arranged image-ID lifecycle handling.
/// A write failure can leave a partial transfer on the output stream.
pub fn write_kitty_rgba_placement(
    image: &DecodedImage,
    image_id: u32,
    z_index: i32,
    output: &mut impl Write,
) -> io::Result<()> {
    validate(image, image_id)?;

    let mut chunks = image.pixels.chunks(RAW_CHUNK_BYTES).peekable();
    let mut first = true;
    while let Some(raw) = chunks.next() {
        let more = chunks.peek().is_some();
        if first {
            output.write_all(first_header(image, image_id, z_index, more).as_bytes())?;
            first = false;
        } else {
            output.write_all(if more { MORE_HEADER } else { FINAL_HEADER })?;
        }
        let encoded = STANDARD.encode(raw);
        output.write_all(encoded.as_bytes())?;
        output.write_all(APC_END)?;
    }
    Ok(())
}

fn validate(image: &DecodedImage, image_id: u32) -> io::Result<()> {
    let expected = usize::try_from(image.width)
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(image.height).ok()?))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|&size| size <= MAX_DECODED_IMAGE_BYTES);
    if image_id == 0
        || image.width == 0
        || image.height == 0
        || expected != Some(image.pixels.len())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Kitty RGBA image or image ID",
        ));
    }
    Ok(())
}

fn first_header(image: &DecodedImage, image_id: u32, z_index: i32, more: bool) -> String {
    format!(
        "\x1b_Ga=T,f=32,s={},v={},i={},z={},C=1,q=2,m={};",
        image.width,
        image.height,
        image_id,
        z_index,
        u8::from(more),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{GraphicsEvent, GraphicsFramer};
    use crate::graphics_transfer::DirectTransferAssembler;

    fn image(pixels: Vec<u8>, width: u32, height: u32) -> DecodedImage {
        DecodedImage {
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn one_pixel_command_has_expected_controls_and_payload() {
        let mut output = Vec::new();
        write_kitty_rgba_placement(&image(vec![1, 2, 3, 4], 1, 1), 7, -9, &mut output).unwrap();
        assert_eq!(
            output,
            b"\x1b_Ga=T,f=32,s=1,v=1,i=7,z=-9,C=1,q=2,m=0;AQIDBA==\x1b\\"
        );
    }

    #[test]
    fn chunk_boundary_and_reassembly() {
        for raw_len in [RAW_CHUNK_BYTES, RAW_CHUNK_BYTES + 4] {
            let pixels: Vec<u8> = (0..raw_len).map(|index| index as u8).collect();
            let rgba = image(pixels.clone(), 1, u32::try_from(raw_len / 4).unwrap());
            let mut output = Vec::new();
            write_kitty_rgba_placement(&rgba, 3, 0, &mut output).unwrap();

            let events = GraphicsFramer::new().advance(&output);
            assert_eq!(events.len(), if raw_len == RAW_CHUNK_BYTES { 1 } else { 2 });
            let mut assembler = DirectTransferAssembler::new();
            let mut transfer = None;
            for (index, event) in events.into_iter().enumerate() {
                let GraphicsEvent::Command(command) = event else {
                    panic!("output should contain graphics commands only");
                };
                let encoded = command
                    .split(|&byte| byte == b';')
                    .nth(1)
                    .unwrap()
                    .strip_suffix(b"\x1b\\")
                    .unwrap();
                assert!(encoded.len() <= 4096);
                if index == 0 && raw_len > RAW_CHUNK_BYTES {
                    assert_eq!(encoded.len(), 4096);
                }
                transfer = assembler.accept(&command).or(transfer);
            }
            let transfer = transfer.unwrap();
            assert_eq!(transfer.data, pixels);
            assert_eq!(transfer.control(b'a'), Some(b"T".as_slice()));
            assert_eq!(transfer.control(b'q'), Some(b"2".as_slice()));
            assert_eq!(transfer.control(b'i'), Some(b"3".as_slice()));
        }
    }

    #[test]
    fn invalid_inputs_write_nothing() {
        let invalid = [
            (image(vec![0; 4], 1, 1), 0),
            (image(vec![], 0, 1), 1),
            (image(vec![], u32::MAX, u32::MAX), 1),
            (image(vec![0; 3], 1, 1), 1),
            (image(vec![], 4096, 4096), 1),
        ];
        for (image, id) in invalid {
            let mut output = Vec::new();
            assert_eq!(
                write_kitty_rgba_placement(&image, id, 0, &mut output)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert!(output.is_empty());
        }
    }

    #[test]
    fn writer_failure_is_returned() {
        let mut output = io::sink();
        write_kitty_rgba_placement(&image(vec![0; 4], 1, 1), 1, 0, &mut output).unwrap();
        let mut output = FailingWriter;
        assert_eq!(
            write_kitty_rgba_placement(&image(vec![0; 4], 1, 1), 1, 0, &mut output)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn preflight_matches_exact_bytes_across_chunk_boundaries() {
        for raw_len in [4, RAW_CHUNK_BYTES, RAW_CHUNK_BYTES + 4, RAW_CHUNK_BYTES * 2] {
            let rgba = image(vec![0x7f; raw_len], 1, u32::try_from(raw_len / 4).unwrap());
            let expected = kitty_rgba_placement_len(&rgba, 1234, i32::MIN).unwrap();
            let mut unbounded = Vec::new();
            write_kitty_rgba_placement(&rgba, 1234, i32::MIN, &mut unbounded).unwrap();
            assert_eq!(expected, unbounded.len());

            let mut bounded = Vec::new();
            write_kitty_rgba_placement_with_limit(&rgba, 1234, i32::MIN, expected, &mut bounded)
                .unwrap();
            assert_eq!(bounded, unbounded);

            let mut rejected = Vec::new();
            assert_eq!(
                write_kitty_rgba_placement_with_limit(
                    &rgba,
                    1234,
                    i32::MIN,
                    expected - 1,
                    &mut rejected,
                )
                .unwrap_err()
                .kind(),
                io::ErrorKind::InvalidInput
            );
            assert!(rejected.is_empty());
        }
    }

    #[test]
    fn frame_sized_budget_rejects_large_image_without_writing() {
        let rgba = image(vec![0; 1024 * 3100 * 4], 1024, 3100);
        let mut output = Vec::new();
        assert!(kitty_rgba_placement_len(&rgba, 1, 0).unwrap() > 16 * 1024 * 1024);
        assert_eq!(
            write_kitty_rgba_placement_with_limit(&rgba, 1, 0, 16 * 1024 * 1024, &mut output)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(output.is_empty());
    }

    #[test]
    fn png_placement_round_trips_pixels_and_chunk_controls() {
        let mut state = 0x1234_5678u32;
        let pixels: Vec<u8> = (0..128 * 128 * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let prepared = EncodedKittyPng::from_rgba(&image(pixels.clone(), 128, 128)).unwrap();
        let expected_len = prepared.placement_len(42, -3).unwrap();
        let mut output = Vec::new();
        prepared
            .write_with_limit(42, -3, expected_len, &mut output)
            .unwrap();
        assert_eq!(output.len(), expected_len);

        let events = GraphicsFramer::new().advance(&output);
        assert!(events.len() > 1);
        let mut assembler = DirectTransferAssembler::new();
        let mut transfer = None;
        for event in events {
            let GraphicsEvent::Command(command) = event else {
                panic!("output should contain graphics commands only");
            };
            let encoded = command
                .split(|&byte| byte == b';')
                .nth(1)
                .unwrap()
                .strip_suffix(APC_END)
                .unwrap();
            assert!(encoded.len() <= 4096);
            transfer = assembler.accept(&command).or(transfer);
        }
        let transfer = transfer.unwrap();
        assert_eq!(transfer.control(b'a'), Some(b"T".as_slice()));
        assert_eq!(transfer.control(b'f'), Some(b"100".as_slice()));
        assert_eq!(transfer.control(b'i'), Some(b"42".as_slice()));
        assert_eq!(transfer.control(b'z'), Some(b"-3".as_slice()));
        assert_eq!(transfer.control(b'C'), Some(b"1".as_slice()));
        assert_eq!(transfer.control(b'q'), Some(b"2".as_slice()));
        assert_eq!(transfer.control(b's'), None);
        assert_eq!(transfer.control(b'v'), None);
        let mut reader = png::Decoder::new(std::io::Cursor::new(transfer.data))
            .read_info()
            .unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut decoded).unwrap();
        assert_eq!((frame.width, frame.height), (128, 128));
        assert_eq!(frame.color_type, png::ColorType::Rgba);
        assert_eq!(&decoded[..frame.buffer_size()], pixels);
    }

    #[test]
    fn opaque_png_uses_rgb_and_preserves_pixels_across_rows() {
        let pixels: Vec<u8> = (0..15)
            .flat_map(|index| {
                [
                    (index * 17) as u8,
                    (index * 11) as u8,
                    (index * 5) as u8,
                    255,
                ]
            })
            .collect();
        let prepared = EncodedKittyPng::from_rgba(&image(pixels.clone(), 5, 3)).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(&prepared.bytes))
            .read_info()
            .unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut decoded).unwrap();
        assert_eq!((frame.width, frame.height), (5, 3));
        assert_eq!(frame.color_type, png::ColorType::Rgb);
        let expected: Vec<u8> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| pixel[..3].iter().copied())
            .collect();
        assert_eq!(&decoded[..frame.buffer_size()], expected);
    }

    #[test]
    fn png_preflight_rejects_budget_and_zero_id_without_writing() {
        let prepared = EncodedKittyPng::from_rgba(&image(vec![0; 4], 1, 1)).unwrap();
        let expected_len = prepared.placement_len(1, 0).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            prepared
                .write_with_limit(1, 0, expected_len - 1, &mut output)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(output.is_empty());
        assert_eq!(
            prepared.placement_len(0, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            prepared
                .write_with_limit(0, 0, expected_len, &mut output)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(output.is_empty());
    }

    #[test]
    fn png_preparation_rejects_invalid_rgba_and_caps_encoded_size() {
        assert!(EncodedKittyPng::from_rgba(&image(vec![1, 2, 3], 1, 1)).is_err());
        let mut buffer = BoundedPngBuffer::default();
        buffer.bytes.resize(MAX_DECODED_IMAGE_BYTES - 1, 0);
        assert_eq!(
            buffer.write(&[1, 2]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(buffer.bytes.len(), MAX_DECODED_IMAGE_BYTES - 1);
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
