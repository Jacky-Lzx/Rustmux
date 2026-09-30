//! Bounded complete and row-by-row decoding of static PNG image data.
//! PNG framing, metadata, transformations, and tail validation belong here.

use super::sampling::{RowSampler, sample_output_buffers};
use super::{DecodeError, DecodedImage, MAX_DECODED_IMAGE_BYTES, decoded_size};
use crate::graphics::geometry::PixelRect;
use crate::graphics_store::StoredImage;
use png::{BitDepth, ColorType, Decoder, Limits, Transformations};
use std::io::Cursor;

pub(super) fn decode_png(image: &StoredImage) -> Result<DecodedImage, DecodeError> {
    let mut decoder = Decoder::new_with_limits(
        Cursor::new(image.data.as_slice()),
        Limits {
            bytes: MAX_DECODED_IMAGE_BYTES,
        },
    );
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().map_err(|_| DecodeError::InvalidData)?;
    let info = reader.info();
    if info.animation_control.is_some() {
        return Err(DecodeError::UnsupportedPng);
    }
    let (width, height) = (info.width, info.height);
    if image
        .declared_width
        .is_some_and(|declared| declared != width)
        || image
            .declared_height
            .is_some_and(|declared| declared != height)
    {
        return Err(DecodeError::InvalidDimensions);
    }
    let output_size = decoded_size(width, height)?;
    let source_size = reader
        .output_buffer_size()
        .ok_or(DecodeError::OutputLimit)?;
    if source_size > MAX_DECODED_IMAGE_BYTES {
        return Err(DecodeError::OutputLimit);
    }
    let mut source = vec![0; source_size];
    let frame = reader
        .next_frame(&mut source)
        .map_err(|_| DecodeError::InvalidData)?;
    reader.finish().map_err(|_| DecodeError::InvalidData)?;
    if frame.width != width || frame.height != height || frame.bit_depth != BitDepth::Eight {
        return Err(DecodeError::UnsupportedPng);
    }
    let channels = match frame.color_type {
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Indexed => return Err(DecodeError::UnsupportedPng),
    };
    let source_len = output_size / 4 * channels;
    if frame.buffer_size() != source_len {
        return Err(DecodeError::InvalidData);
    }
    let mut pixels = Vec::with_capacity(output_size);
    for pixel in source[..source_len].chunks_exact(channels) {
        match frame.color_type {
            ColorType::Grayscale => pixels.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]),
            ColorType::GrayscaleAlpha => {
                pixels.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]])
            }
            ColorType::Rgb => pixels.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            ColorType::Rgba => pixels.extend_from_slice(pixel),
            ColorType::Indexed => unreachable!(),
        }
    }
    Ok(DecodedImage {
        width,
        height,
        pixels,
    })
}

pub(super) fn decode_png_thumbnail(
    image: &StoredImage,
    target_width: u32,
    target_height: u32,
) -> Result<DecodedImage, DecodeError> {
    decode_png_sampled(image, target_width, target_height, None, None).map(|(_, decoded)| decoded)
}

pub(super) fn decode_png_sampled(
    image: &StoredImage,
    target_width: u32,
    target_height: u32,
    source_crop: Option<PixelRect>,
    target_region: Option<PixelRect>,
) -> Result<((u32, u32), DecodedImage), DecodeError> {
    let region = target_region.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        width: target_width,
        height: target_height,
    });
    let (dimensions, mut images) =
        decode_png_regions(image, target_width, target_height, source_crop, &[region])?;
    Ok((dimensions, images.remove(0)))
}

pub(super) fn decode_png_regions(
    image: &StoredImage,
    target_width: u32,
    target_height: u32,
    source_crop: Option<PixelRect>,
    regions: &[PixelRect],
) -> Result<((u32, u32), Vec<DecodedImage>), DecodeError> {
    let images = sample_output_buffers(regions, target_width, target_height)?;
    let mut decoder = Decoder::new_with_limits(
        Cursor::new(image.data.as_slice()),
        Limits {
            bytes: MAX_DECODED_IMAGE_BYTES,
        },
    );
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().map_err(|_| DecodeError::InvalidData)?;
    let info = reader.info();
    if info.animation_control.is_some() || info.interlaced {
        return Err(DecodeError::UnsupportedPng);
    }
    let (width, height) = (info.width, info.height);
    let source = source_crop.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        width,
        height,
    });
    if image
        .declared_width
        .is_some_and(|declared| declared != width)
        || image
            .declared_height
            .is_some_and(|declared| declared != height)
        || source.width == 0
        || source.height == 0
        || source
            .x
            .checked_add(source.width)
            .is_none_or(|end| end > width)
        || source
            .y
            .checked_add(source.height)
            .is_none_or(|end| end > height)
        || (source_crop.is_none() && (target_width > width || target_height > height))
    {
        return Err(DecodeError::InvalidDimensions);
    }
    let (color, depth) = reader.output_color_type();
    if depth != BitDepth::Eight {
        return Err(DecodeError::UnsupportedPng);
    }
    let channels = match color {
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Indexed => return Err(DecodeError::UnsupportedPng),
    };
    let row_len = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(channels))
        .ok_or(DecodeError::OutputLimit)?;
    if row_len > MAX_DECODED_IMAGE_BYTES || reader.output_line_size(width) != Some(row_len) {
        return Err(DecodeError::OutputLimit);
    }
    let mut sampler = RowSampler::new(
        regions,
        images,
        source,
        target_width,
        target_height,
        color,
        channels,
    );
    for source_y in 0..height {
        let row = reader
            .next_row()
            .map_err(|_| DecodeError::InvalidData)?
            .ok_or(DecodeError::InvalidData)?;
        let source_row = row.data();
        if source_row.len() != row_len {
            return Err(DecodeError::InvalidData);
        }
        sampler.accept_row(source_y, source_row);
    }
    let images = sampler.finish()?;
    reader.finish().map_err(|_| DecodeError::InvalidData)?;
    Ok(((width, height), images))
}
