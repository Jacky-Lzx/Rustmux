//! Bounded decoding of raw RGB/RGBA data and row-by-row zlib pixel reads.
//! Byte counts, expanded limits, crop bounds, and complete zlib tails are checked here.

use super::sampling::{RowSampler, sample_output_buffers};
use super::{DecodeError, DecodedImage, MAX_DECODED_IMAGE_BYTES, decoded_size};
use crate::graphics::geometry::PixelRect;
use crate::graphics_store::{ImageFormat, StoredImage};
use crate::graphics_transfer::MAX_STREAMED_RAW_BYTES;
use flate2::bufread::ZlibDecoder;
use png::ColorType;
use std::io::Read;

pub(super) fn decode_raw(image: &StoredImage) -> Result<DecodedImage, DecodeError> {
    let (Some(width), Some(height)) = (image.declared_width, image.declared_height) else {
        return Err(DecodeError::InvalidDimensions);
    };
    let output_size = decoded_size(width, height)?;
    let pixels = match image.format {
        ImageFormat::Rgb => {
            let expected = output_size / 4 * 3;
            if image.data.len() != expected {
                return Err(DecodeError::InvalidData);
            }
            let mut output = Vec::with_capacity(output_size);
            for rgb in image.data.as_chunks::<3>().0 {
                output.extend_from_slice(rgb);
                output.push(255);
            }
            output
        }
        ImageFormat::Rgba => {
            if image.data.len() != output_size {
                return Err(DecodeError::InvalidData);
            }
            image.data.clone()
        }
        ImageFormat::Png | ImageFormat::RgbZlib | ImageFormat::RgbaZlib => unreachable!(),
    };
    Ok(DecodedImage {
        width,
        height,
        pixels,
    })
}

pub(super) fn decode_zlib_sampled(
    image: &StoredImage,
    target_width: u32,
    target_height: u32,
    source_crop: Option<PixelRect>,
    target_region: Option<PixelRect>,
) -> Result<DecodedImage, DecodeError> {
    let region = target_region.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        width: target_width,
        height: target_height,
    });
    Ok(decode_zlib_regions(image, target_width, target_height, source_crop, &[region])?.remove(0))
}

pub(super) fn decode_zlib_regions(
    image: &StoredImage,
    target_width: u32,
    target_height: u32,
    source_crop: Option<PixelRect>,
    regions: &[PixelRect],
) -> Result<Vec<DecodedImage>, DecodeError> {
    let images = sample_output_buffers(regions, target_width, target_height)?;
    let (Some(width), Some(height)) = (image.declared_width, image.declared_height) else {
        return Err(DecodeError::InvalidDimensions);
    };
    let channels = match image.format {
        ImageFormat::RgbZlib => 3usize,
        ImageFormat::RgbaZlib => 4usize,
        _ => return Err(DecodeError::UnsupportedFormat),
    };
    let row_len = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(channels))
        .ok_or(DecodeError::OutputLimit)?;
    let raw_size = row_len
        .checked_mul(usize::try_from(height).map_err(|_| DecodeError::OutputLimit)?)
        .ok_or(DecodeError::OutputLimit)?;
    if row_len > MAX_DECODED_IMAGE_BYTES || raw_size > MAX_STREAMED_RAW_BYTES {
        return Err(DecodeError::OutputLimit);
    }
    let source = source_crop.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        width,
        height,
    });
    if source.width == 0
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
    let mut reader = ZlibDecoder::new(image.data.as_slice());
    let mut row = vec![0; row_len];
    let color = if channels == 3 {
        ColorType::Rgb
    } else {
        ColorType::Rgba
    };
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
        reader
            .read_exact(&mut row)
            .map_err(|_| DecodeError::InvalidData)?;
        sampler.accept_row(source_y, &row);
    }
    let images = sampler.finish()?;
    let mut extra = [0];
    if reader
        .read(&mut extra)
        .map_err(|_| DecodeError::InvalidData)?
        != 0
        || reader.total_in() != image.data.len() as u64
    {
        return Err(DecodeError::InvalidData);
    }
    Ok(images)
}
