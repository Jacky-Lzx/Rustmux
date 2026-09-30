//! Streamed PNG and zlib placement orchestration with shared region conversion.
//! Format readers retain source checks, allocation limits, and complete-tail validation.

use super::png_reader::{decode_png_regions, decode_png_sampled};
use super::raw_reader::{decode_zlib_regions, decode_zlib_sampled};
use super::{DecodeError, DecodedImage, ResampleError, ResampledPlacement};
use crate::graphics::geometry::{PixelRect, PlacementPixelLayout};
use crate::graphics_store::{ImageFormat, StoredImage};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StreamPngError {
    Decode(DecodeError),
    Resample(ResampleError),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StreamZlibError {
    Decode(DecodeError),
    Resample(ResampleError),
}

impl StoredImage {
    /// Sample a source crop of a compressed raw image into bounded placement
    /// pixels while expanding only one source row at a time.
    pub fn resample_zlib_placement(
        &self,
        layout: PlacementPixelLayout,
    ) -> Result<ResampledPlacement, StreamZlibError> {
        if !matches!(self.format, ImageFormat::RgbZlib | ImageFormat::RgbaZlib) {
            return Err(StreamZlibError::Decode(DecodeError::UnsupportedFormat));
        }
        if !valid_stream_layout(layout) {
            return Err(StreamZlibError::Resample(ResampleError::InvalidLayout));
        }
        let image = decode_zlib_sampled(
            self,
            layout.destination.width,
            layout.destination.height,
            Some(layout.source),
            None,
        )
        .map_err(StreamZlibError::Decode)?;
        Ok(ResampledPlacement {
            destination: layout.destination,
            pixels: image.pixels,
        })
    }

    /// Sample only a visible subrectangle of the full destination. Sampling
    /// coordinates still refer to the original destination so clipping does
    /// not change nearest-neighbor pixel selection.
    pub fn resample_zlib_placement_region(
        &self,
        layout: PlacementPixelLayout,
        region: PixelRect,
    ) -> Result<ResampledPlacement, StreamZlibError> {
        if !matches!(self.format, ImageFormat::RgbZlib | ImageFormat::RgbaZlib) {
            return Err(StreamZlibError::Decode(DecodeError::UnsupportedFormat));
        }
        if !valid_stream_layout(layout) {
            return Err(StreamZlibError::Resample(ResampleError::InvalidLayout));
        }
        let relative = relative_region(layout.destination, region)
            .ok_or(StreamZlibError::Resample(ResampleError::InvalidLayout))?;
        let image = decode_zlib_sampled(
            self,
            layout.destination.width,
            layout.destination.height,
            Some(layout.source),
            Some(relative),
        )
        .map_err(StreamZlibError::Decode)?;
        Ok(ResampledPlacement {
            destination: region,
            pixels: image.pixels,
        })
    }

    /// Sample disjoint destination rectangles during one complete zlib pass.
    /// The aggregate RGBA output, rather than their enclosing box, is bounded.
    pub(crate) fn resample_zlib_placement_regions(
        &self,
        layout: PlacementPixelLayout,
        regions: &[PixelRect],
    ) -> Result<Vec<ResampledPlacement>, StreamZlibError> {
        if !matches!(self.format, ImageFormat::RgbZlib | ImageFormat::RgbaZlib) {
            return Err(StreamZlibError::Decode(DecodeError::UnsupportedFormat));
        }
        if !valid_stream_layout(layout) {
            return Err(StreamZlibError::Resample(ResampleError::InvalidLayout));
        }
        let relative = relative_regions(layout.destination, regions)
            .ok_or(StreamZlibError::Resample(ResampleError::InvalidLayout))?;
        let images = decode_zlib_regions(
            self,
            layout.destination.width,
            layout.destination.height,
            Some(layout.source),
            &relative,
        )
        .map_err(StreamZlibError::Decode)?;
        Ok(placements_from_images(regions, images))
    }

    /// Decode and scale a PNG source crop directly into a placement without
    /// materializing the full RGBA source. The layout must use dimensions
    /// obtained from this image's validated PNG metadata.
    pub fn resample_png_placement(
        &self,
        layout: PlacementPixelLayout,
    ) -> Result<ResampledPlacement, StreamPngError> {
        if self.format != ImageFormat::Png {
            return Err(StreamPngError::Decode(DecodeError::UnsupportedPng));
        }
        if !valid_stream_layout(layout) {
            return Err(StreamPngError::Resample(ResampleError::InvalidLayout));
        }
        let (_, image) = decode_png_sampled(
            self,
            layout.destination.width,
            layout.destination.height,
            Some(layout.source),
            None,
        )
        .map_err(StreamPngError::Decode)?;
        Ok(ResampledPlacement {
            destination: layout.destination,
            pixels: image.pixels,
        })
    }

    /// Sample only a visible rectangle of the full PNG destination while
    /// retaining the original pixel-center coordinates for nearest sampling.
    pub fn resample_png_placement_region(
        &self,
        layout: PlacementPixelLayout,
        region: PixelRect,
    ) -> Result<ResampledPlacement, StreamPngError> {
        if self.format != ImageFormat::Png {
            return Err(StreamPngError::Decode(DecodeError::UnsupportedPng));
        }
        if !valid_stream_layout(layout) {
            return Err(StreamPngError::Resample(ResampleError::InvalidLayout));
        }
        let relative = relative_region(layout.destination, region)
            .ok_or(StreamPngError::Resample(ResampleError::InvalidLayout))?;
        let (_, image) = decode_png_sampled(
            self,
            layout.destination.width,
            layout.destination.height,
            Some(layout.source),
            Some(relative),
        )
        .map_err(StreamPngError::Decode)?;
        Ok(ResampledPlacement {
            destination: region,
            pixels: image.pixels,
        })
    }

    /// Sample disjoint PNG destination rectangles in a single row-decoder
    /// pass, with an aggregate RGBA output bound and complete tail validation.
    pub(crate) fn resample_png_placement_regions(
        &self,
        layout: PlacementPixelLayout,
        regions: &[PixelRect],
    ) -> Result<Vec<ResampledPlacement>, StreamPngError> {
        if self.format != ImageFormat::Png {
            return Err(StreamPngError::Decode(DecodeError::UnsupportedPng));
        }
        if !valid_stream_layout(layout) {
            return Err(StreamPngError::Resample(ResampleError::InvalidLayout));
        }
        let relative = relative_regions(layout.destination, regions)
            .ok_or(StreamPngError::Resample(ResampleError::InvalidLayout))?;
        let (_, images) = decode_png_regions(
            self,
            layout.destination.width,
            layout.destination.height,
            Some(layout.source),
            &relative,
        )
        .map_err(StreamPngError::Decode)?;
        Ok(placements_from_images(regions, images))
    }
}

fn valid_stream_layout(layout: PlacementPixelLayout) -> bool {
    layout.source.width != 0
        && layout.source.height != 0
        && layout.destination.width != 0
        && layout.destination.height != 0
        && layout
            .destination
            .x
            .checked_add(layout.destination.width)
            .is_some()
        && layout
            .destination
            .y
            .checked_add(layout.destination.height)
            .is_some()
}

fn relative_region(destination: PixelRect, region: PixelRect) -> Option<PixelRect> {
    Some(PixelRect {
        x: region.x.checked_sub(destination.x)?,
        y: region.y.checked_sub(destination.y)?,
        width: region.width,
        height: region.height,
    })
}

fn relative_regions(destination: PixelRect, regions: &[PixelRect]) -> Option<Vec<PixelRect>> {
    regions
        .iter()
        .map(|&region| relative_region(destination, region))
        .collect()
}

fn placements_from_images(
    regions: &[PixelRect],
    images: Vec<DecodedImage>,
) -> Vec<ResampledPlacement> {
    regions
        .iter()
        .copied()
        .zip(images)
        .map(|(destination, image)| ResampledPlacement {
            destination,
            pixels: image.pixels,
        })
        .collect()
}
