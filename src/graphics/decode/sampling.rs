//! Shared bounded row sampling for PNG and zlib image decoders.
//! Readers retain responsibility for source bounds and complete stream validation.

use super::{DecodeError, DecodedImage, MAX_DECODED_IMAGE_BYTES, decoded_size};
use crate::graphics::geometry::PixelRect;
use png::ColorType;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub(super) const MAX_SAMPLED_REGIONS: usize = 64 * 1024;

pub(super) fn nearest_sample(output_index: usize, source_extent: u32, output_extent: u32) -> u32 {
    // Sample at pixel centers. All extents are nonzero and the quotient is
    // strictly smaller than `source_extent`.
    let center = 2 * u128::try_from(output_index).unwrap() + 1;
    let numerator = center * u128::from(source_extent);
    let denominator = 2 * u128::from(output_extent);
    u32::try_from(numerator / denominator).unwrap()
}

/// Advance the same pixel-center sample across adjacent output columns with
/// quotient/remainder addition instead of dividing once per output pixel.
struct SampledColumns {
    current: u64,
    remainder: u64,
    whole_step: u64,
    fractional_step: u64,
    denominator: u64,
}

impl SampledColumns {
    fn new(first: u32, source_extent: u32, output_extent: u32) -> Self {
        debug_assert!(source_extent != 0 && first < output_extent);
        let denominator = 2 * u64::from(output_extent);
        let numerator = (2 * u128::from(first) + 1) * u128::from(source_extent);
        Self {
            current: u64::try_from(numerator / u128::from(denominator)).unwrap(),
            remainder: u64::try_from(numerator % u128::from(denominator)).unwrap(),
            whole_step: u64::from(source_extent / output_extent),
            fractional_step: 2 * u64::from(source_extent % output_extent),
            denominator,
        }
    }

    fn next(&mut self) -> u32 {
        let sample = u32::try_from(self.current).unwrap();
        self.current += self.whole_step;
        self.remainder += self.fractional_step;
        if self.remainder >= self.denominator {
            self.remainder -= self.denominator;
            self.current += 1;
        }
        sample
    }
}

pub(super) fn sample_output_buffers(
    regions: &[PixelRect],
    target_width: u32,
    target_height: u32,
) -> Result<Vec<DecodedImage>, DecodeError> {
    if regions.len() > MAX_SAMPLED_REGIONS {
        return Err(DecodeError::OutputLimit);
    }
    let mut total_output = 0usize;
    let mut images = Vec::with_capacity(regions.len());
    for region in regions {
        let size = decoded_size(region.width, region.height)?;
        if region
            .x
            .checked_add(region.width)
            .is_none_or(|end| end > target_width)
            || region
                .y
                .checked_add(region.height)
                .is_none_or(|end| end > target_height)
        {
            return Err(DecodeError::InvalidDimensions);
        }
        total_output = total_output
            .checked_add(size)
            .filter(|&total| total <= MAX_DECODED_IMAGE_BYTES)
            .ok_or(DecodeError::OutputLimit)?;
        images.push(DecodedImage {
            width: region.width,
            height: region.height,
            pixels: vec![0; size],
        });
    }
    Ok(images)
}

/// Share pixel-center coordinates, sparse row scheduling and color conversion
/// between the PNG and zlib row readers. Each reader still validates its own
/// framing and complete tail before returning these sampled pixels.
pub(super) struct RowSampler<'a> {
    regions: &'a [PixelRect],
    source: PixelRect,
    target_width: u32,
    target_height: u32,
    color: ColorType,
    channels: usize,
    images: Vec<DecodedImage>,
    next_rows: Vec<usize>,
    pending: BinaryHeap<Reverse<(u32, usize)>>,
}

impl<'a> RowSampler<'a> {
    pub(super) fn new(
        regions: &'a [PixelRect],
        images: Vec<DecodedImage>,
        source: PixelRect,
        target_width: u32,
        target_height: u32,
        color: ColorType,
        channels: usize,
    ) -> Self {
        let mut pending = BinaryHeap::new();
        for (index, region) in regions.iter().enumerate() {
            pending.push(Reverse((
                source.y + nearest_sample(region.y as usize, source.height, target_height),
                index,
            )));
        }
        Self {
            regions,
            source,
            target_width,
            target_height,
            color,
            channels,
            images,
            next_rows: vec![0; regions.len()],
            pending,
        }
    }

    pub(super) fn accept_row(&mut self, source_y: u32, source_row: &[u8]) {
        while self
            .pending
            .peek()
            .is_some_and(|Reverse((requested_y, _))| *requested_y == source_y)
        {
            let Reverse((_, index)) = self.pending.pop().unwrap();
            let region = self.regions[index];
            let target_y = self.next_rows[index];
            let target_stride = usize::try_from(region.width).unwrap() * 4;
            let output_row = &mut self.images[index].pixels
                [target_y * target_stride..(target_y + 1) * target_stride];
            let mut columns = SampledColumns::new(region.x, self.source.width, self.target_width);
            for rgba in output_row.as_chunks_mut::<4>().0.iter_mut() {
                let source_x = usize::try_from(self.source.x + columns.next()).unwrap();
                let offset = source_x * self.channels;
                let pixel = &source_row[offset..offset + self.channels];
                match self.color {
                    ColorType::Grayscale => *rgba = [pixel[0], pixel[0], pixel[0], 255],
                    ColorType::GrayscaleAlpha => *rgba = [pixel[0], pixel[0], pixel[0], pixel[1]],
                    ColorType::Rgb => *rgba = [pixel[0], pixel[1], pixel[2], 255],
                    ColorType::Rgba => rgba.copy_from_slice(pixel),
                    ColorType::Indexed => unreachable!(),
                }
            }
            self.next_rows[index] += 1;
            if self.next_rows[index] < usize::try_from(region.height).unwrap() {
                self.pending.push(Reverse((
                    self.source.y
                        + nearest_sample(
                            usize::try_from(region.y).unwrap() + self.next_rows[index],
                            self.source.height,
                            self.target_height,
                        ),
                    index,
                )));
            }
        }
    }

    pub(super) fn finish(self) -> Result<Vec<DecodedImage>, DecodeError> {
        if !self.pending.is_empty() {
            return Err(DecodeError::InvalidData);
        }
        Ok(self.images)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_columns_match_pixel_center_sampling_at_extreme_extents() {
        for (source_extent, output_extent) in [
            (1, 1),
            (7, 3),
            (3, 7),
            (255, 64),
            (1_000, 1_001),
            (u32::MAX, u32::MAX),
            (u32::MAX, 1),
            (1, u32::MAX),
        ] {
            for first in [0, output_extent / 2, output_extent.saturating_sub(3)] {
                let mut columns = SampledColumns::new(first, source_extent, output_extent);
                for output_index in first..first + (output_extent - first).min(4) {
                    assert_eq!(
                        columns.next(),
                        nearest_sample(output_index as usize, source_extent, output_extent),
                        "source={source_extent}, output={output_extent}, index={output_index}"
                    );
                }
            }
        }
    }
}
