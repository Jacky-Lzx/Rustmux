//! Opt-in, bounded conversion of stored Kitty image data to RGBA pixels.
//! Decoding does not place or render an image in a terminal.

use crate::graphics_store::{
    CellPixelSize, ImageFormat, PixelRect, PixelSize, PlacementGeometry, PlacementPixelLayout,
    SignedPixelPoint, StoredImage,
};
use crate::graphics_transfer::MAX_STREAMED_RAW_BYTES;
use flate2::bufread::ZlibDecoder;
use png::{BitDepth, ColorType, Decoder, Limits, Transformations};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::{Cursor, Read};

pub const MAX_DECODED_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RESAMPLED_PLACEMENT_BYTES: usize = MAX_DECODED_IMAGE_BYTES;
const MAX_SAMPLED_REGIONS: usize = 64 * 1024;

#[derive(Debug, Eq, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Row-major, eight-bit RGBA pixels.
    pub pixels: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ResampledPlacement {
    /// Content rectangle relative to the placement's anchor cell. Letterbox
    /// space is not allocated in `pixels`.
    pub destination: PixelRect,
    /// Row-major, eight-bit RGBA content pixels.
    pub pixels: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ClippedPlacement {
    /// Visible rectangle in viewport pixel coordinates.
    pub destination: PixelRect,
    /// Row-major RGBA pixels for only the visible rectangle.
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ClipError {
    InvalidPixels,
    InvalidDestination,
    InvalidGeometry,
    OutputLimit,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResampleError {
    InvalidPixels,
    InvalidLayout,
    OutputLimit,
}

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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DecodeError {
    InvalidDimensions,
    OutputLimit,
    InvalidData,
    UnsupportedFormat,
    UnsupportedPng,
}

impl StoredImage {
    /// Decode pixels on demand. PNG metadata, checksums and decoded byte count
    /// are validated before returning pixels; the stored source is unchanged.
    pub fn decode_rgba(&self) -> Result<DecodedImage, DecodeError> {
        match self.format {
            ImageFormat::Rgb | ImageFormat::Rgba => decode_raw(self),
            ImageFormat::RgbZlib | ImageFormat::RgbaZlib => {
                let (Some(width), Some(height)) = (self.declared_width, self.declared_height)
                else {
                    return Err(DecodeError::InvalidDimensions);
                };
                decode_zlib_sampled(self, width, height, None, None)
            }
            ImageFormat::Png => decode_png(self),
        }
    }

    /// Decode a static PNG row by row into a bounded nearest-neighbor
    /// thumbnail. This is an opt-in preparation step: callers must still map
    /// source crop coordinates before using the result as a placement.
    pub fn decode_png_thumbnail(
        &self,
        target_width: u32,
        target_height: u32,
    ) -> Result<DecodedImage, DecodeError> {
        if self.format != ImageFormat::Png {
            return Err(DecodeError::UnsupportedPng);
        }
        decode_png_thumbnail(self, target_width, target_height)
    }

    /// Validate a static PNG without allocating its full RGBA image and return
    /// the dimensions from the verified PNG header.
    pub fn validated_png_dimensions(&self) -> Result<(u32, u32), DecodeError> {
        if self.format != ImageFormat::Png {
            return Err(DecodeError::UnsupportedPng);
        }
        decode_png_sampled(self, 1, 1, None, None).map(|(dimensions, _)| dimensions)
    }

    /// Check a compressed raw transfer through its last byte without
    /// allocating its expanded RGBA image.
    pub fn validated_zlib_dimensions(&self) -> Result<(u32, u32), DecodeError> {
        if !matches!(self.format, ImageFormat::RgbZlib | ImageFormat::RgbaZlib) {
            return Err(DecodeError::UnsupportedFormat);
        }
        let (Some(width), Some(height)) = (self.declared_width, self.declared_height) else {
            return Err(DecodeError::InvalidDimensions);
        };
        decode_zlib_sampled(self, 1, 1, None, None)?;
        Ok((width, height))
    }

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
        let relative = PixelRect {
            x: region
                .x
                .checked_sub(layout.destination.x)
                .ok_or(StreamZlibError::Resample(ResampleError::InvalidLayout))?,
            y: region
                .y
                .checked_sub(layout.destination.y)
                .ok_or(StreamZlibError::Resample(ResampleError::InvalidLayout))?,
            width: region.width,
            height: region.height,
        };
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
        let relative: Vec<_> = regions
            .iter()
            .map(|region| {
                Some(PixelRect {
                    x: region.x.checked_sub(layout.destination.x)?,
                    y: region.y.checked_sub(layout.destination.y)?,
                    width: region.width,
                    height: region.height,
                })
            })
            .collect::<Option<_>>()
            .ok_or(StreamZlibError::Resample(ResampleError::InvalidLayout))?;
        let images = decode_zlib_regions(
            self,
            layout.destination.width,
            layout.destination.height,
            Some(layout.source),
            &relative,
        )
        .map_err(StreamZlibError::Decode)?;
        Ok(regions
            .iter()
            .copied()
            .zip(images)
            .map(|(destination, image)| ResampledPlacement {
                destination,
                pixels: image.pixels,
            })
            .collect())
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
        let relative = PixelRect {
            x: region
                .x
                .checked_sub(layout.destination.x)
                .ok_or(StreamPngError::Resample(ResampleError::InvalidLayout))?,
            y: region
                .y
                .checked_sub(layout.destination.y)
                .ok_or(StreamPngError::Resample(ResampleError::InvalidLayout))?,
            width: region.width,
            height: region.height,
        };
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
        let relative: Vec<_> = regions
            .iter()
            .map(|region| {
                Some(PixelRect {
                    x: region.x.checked_sub(layout.destination.x)?,
                    y: region.y.checked_sub(layout.destination.y)?,
                    width: region.width,
                    height: region.height,
                })
            })
            .collect::<Option<_>>()
            .ok_or(StreamPngError::Resample(ResampleError::InvalidLayout))?;
        let (_, images) = decode_png_regions(
            self,
            layout.destination.width,
            layout.destination.height,
            Some(layout.source),
            &relative,
        )
        .map_err(StreamPngError::Decode)?;
        Ok(regions
            .iter()
            .copied()
            .zip(images)
            .map(|(destination, image)| ResampledPlacement {
                destination,
                pixels: image.pixels,
            })
            .collect())
    }
}

impl DecodedImage {
    /// Sample a cropped placement into its content rectangle. The caller
    /// composes the returned pixels at `destination`; this does not draw into
    /// a pane or allocate transparent letterbox padding.
    pub fn resample_placement(
        &self,
        layout: PlacementPixelLayout,
    ) -> Result<ResampledPlacement, ResampleError> {
        self.resample_placement_region(layout, layout.destination)
    }

    /// Sample only a rectangle within the destination while keeping the same
    /// pixel-center coordinates as a full placement resample.
    pub fn resample_placement_region(
        &self,
        layout: PlacementPixelLayout,
        region: PixelRect,
    ) -> Result<ResampledPlacement, ResampleError> {
        let expected = decoded_size(self.width, self.height).map_err(|error| match error {
            DecodeError::OutputLimit => ResampleError::OutputLimit,
            _ => ResampleError::InvalidPixels,
        })?;
        if self.pixels.len() != expected {
            return Err(ResampleError::InvalidPixels);
        }
        let source = layout.source;
        let destination = layout.destination;
        if source.width == 0
            || source.height == 0
            || destination.width == 0
            || destination.height == 0
            || source
                .x
                .checked_add(source.width)
                .is_none_or(|end| end > self.width)
            || source
                .y
                .checked_add(source.height)
                .is_none_or(|end| end > self.height)
            || destination.x.checked_add(destination.width).is_none()
            || destination.y.checked_add(destination.height).is_none()
            || region.width == 0
            || region.height == 0
            || region.x < destination.x
            || region.y < destination.y
            || region
                .x
                .checked_add(region.width)
                .is_none_or(|end| end > destination.x + destination.width)
            || region
                .y
                .checked_add(region.height)
                .is_none_or(|end| end > destination.y + destination.height)
        {
            return Err(ResampleError::InvalidLayout);
        }
        let output_size =
            decoded_size(region.width, region.height).map_err(|_| ResampleError::OutputLimit)?;
        let mut pixels = vec![0; output_size];
        let output_width = usize::try_from(region.width).unwrap();
        let input_width = usize::try_from(self.width).unwrap();
        for (y, row) in pixels.chunks_exact_mut(output_width * 4).enumerate() {
            let output_y = usize::try_from(region.y - destination.y).unwrap() + y;
            let source_y = source.y + nearest_sample(output_y, source.height, destination.height);
            for (x, rgba) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let output_x = usize::try_from(region.x - destination.x).unwrap() + x;
                let source_x = source.x + nearest_sample(output_x, source.width, destination.width);
                let index = (usize::try_from(source_y).unwrap() * input_width
                    + usize::try_from(source_x).unwrap())
                    * 4;
                rgba.copy_from_slice(&self.pixels[index..index + 4]);
            }
        }
        Ok(ResampledPlacement {
            destination: region,
            pixels,
        })
    }
}

impl ResampledPlacement {
    /// Intersect the content with a pane-sized viewport. `anchor` is the
    /// signed pixel position of the placement's anchor cell within that
    /// viewport; it may be negative after scrolling. No text or other images
    /// are blended here.
    pub fn clip_to_viewport(
        &self,
        anchor: SignedPixelPoint,
        viewport: PixelSize,
    ) -> Result<Option<ClippedPlacement>, ClipError> {
        self.clip_between_rows(anchor, viewport, i128::MIN, i128::MAX)
    }

    /// Apply permanent top/bottom cell-row clips accumulated while scrolling,
    /// then intersect with the pane viewport. Unclipped edges remain unbounded
    /// so an `X/Y` offset is not mistaken for an extra placement row.
    pub fn clip_to_viewport_with_scroll_clip(
        &self,
        geometry: PlacementGeometry,
        cell: CellPixelSize,
        viewport: PixelSize,
    ) -> Result<Option<ClippedPlacement>, ClipError> {
        let anchor = geometry
            .pixel_anchor(cell)
            .ok_or(ClipError::InvalidGeometry)?;
        let (top, bottom) = scroll_clip_bounds(geometry, cell, anchor)?;
        self.clip_between_rows(anchor, viewport, top, bottom)
    }

    fn clip_between_rows(
        &self,
        anchor: SignedPixelPoint,
        viewport: PixelSize,
        clip_top: i128,
        clip_bottom: i128,
    ) -> Result<Option<ClippedPlacement>, ClipError> {
        let source = self.destination;
        let source_size =
            decoded_size(source.width, source.height).map_err(|error| match error {
                DecodeError::OutputLimit => ClipError::OutputLimit,
                _ => ClipError::InvalidDestination,
            })?;
        if source.x.checked_add(source.width).is_none()
            || source.y.checked_add(source.height).is_none()
        {
            return Err(ClipError::InvalidDestination);
        }
        if self.pixels.len() != source_size {
            return Err(ClipError::InvalidPixels);
        }
        let Some((region, destination)) =
            visible_bounds(source, anchor, viewport, clip_top, clip_bottom)?
        else {
            return Ok(None);
        };
        let skip_x = usize::try_from(region.x - source.x).unwrap();
        let skip_y = usize::try_from(region.y - source.y).unwrap();
        let input_width = usize::try_from(source.width).unwrap();
        let output_width = usize::try_from(region.width).unwrap();
        let mut pixels = vec![0; decoded_size(region.width, region.height).unwrap()];
        for (row, destination_row) in pixels.chunks_exact_mut(output_width * 4).enumerate() {
            let start = ((skip_y + row) * input_width + skip_x) * 4;
            destination_row.copy_from_slice(&self.pixels[start..start + output_width * 4]);
        }
        Ok(Some(ClippedPlacement {
            destination,
            pixels,
        }))
    }
}

pub(crate) fn visible_placement_region(
    destination: PixelRect,
    geometry: PlacementGeometry,
    cell: CellPixelSize,
    viewport: PixelSize,
) -> Result<Option<(PixelRect, PixelRect)>, ClipError> {
    let anchor = geometry
        .pixel_anchor(cell)
        .ok_or(ClipError::InvalidGeometry)?;
    let (top, bottom) = scroll_clip_bounds(geometry, cell, anchor)?;
    visible_bounds(destination, anchor, viewport, top, bottom)
}

fn scroll_clip_bounds(
    geometry: PlacementGeometry,
    cell: CellPixelSize,
    anchor: SignedPixelPoint,
) -> Result<(i128, i128), ClipError> {
    if geometry.clip_top_rows == 0 && geometry.clip_bottom_rows == 0 {
        return Ok((i128::MIN, i128::MAX));
    }
    let rows = geometry.rows.ok_or(ClipError::InvalidGeometry)?;
    if geometry
        .clip_top_rows
        .saturating_add(geometry.clip_bottom_rows)
        >= rows
    {
        return Ok((0, 0));
    }
    let row_height = i128::from(cell.height());
    let top = if geometry.clip_top_rows == 0 {
        i128::MIN
    } else {
        i128::from(anchor.y) + i128::from(geometry.clip_top_rows) * row_height
    };
    let bottom = if geometry.clip_bottom_rows == 0 {
        i128::MAX
    } else {
        i128::from(anchor.y) + i128::from(rows - geometry.clip_bottom_rows) * row_height
    };
    Ok((top, bottom))
}

fn visible_bounds(
    source: PixelRect,
    anchor: SignedPixelPoint,
    viewport: PixelSize,
    clip_top: i128,
    clip_bottom: i128,
) -> Result<Option<(PixelRect, PixelRect)>, ClipError> {
    if source.width == 0
        || source.height == 0
        || source.x.checked_add(source.width).is_none()
        || source.y.checked_add(source.height).is_none()
    {
        return Err(ClipError::InvalidDestination);
    }
    if viewport.width == 0 || viewport.height == 0 {
        return Ok(None);
    }
    let left = i128::from(anchor.x) + i128::from(source.x);
    let top = i128::from(anchor.y) + i128::from(source.y);
    let visible_left = left.max(0);
    let visible_top = top.max(0).max(clip_top);
    let visible_right = (left + i128::from(source.width)).min(i128::from(viewport.width));
    let visible_bottom = (top + i128::from(source.height))
        .min(i128::from(viewport.height))
        .min(clip_bottom);
    if visible_left >= visible_right || visible_top >= visible_bottom {
        return Ok(None);
    }
    let width = u32::try_from(visible_right - visible_left).unwrap();
    let height = u32::try_from(visible_bottom - visible_top).unwrap();
    let region = PixelRect {
        x: source.x + u32::try_from(visible_left - left).unwrap(),
        y: source.y + u32::try_from(visible_top - top).unwrap(),
        width,
        height,
    };
    let destination = PixelRect {
        x: u32::try_from(visible_left).unwrap(),
        y: u32::try_from(visible_top).unwrap(),
        width,
        height,
    };
    Ok(Some((region, destination)))
}

fn nearest_sample(output_index: usize, source_extent: u32, output_extent: u32) -> u32 {
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

fn sample_output_buffers(
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
struct RowSampler<'a> {
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
    fn new(
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

    fn accept_row(&mut self, source_y: u32, source_row: &[u8]) {
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

    fn finish(self) -> Result<Vec<DecodedImage>, DecodeError> {
        if !self.pending.is_empty() {
            return Err(DecodeError::InvalidData);
        }
        Ok(self.images)
    }
}

fn decoded_size(width: u32, height: u32) -> Result<usize, DecodeError> {
    if width == 0 || height == 0 {
        return Err(DecodeError::InvalidDimensions);
    }
    let bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(height).ok()?))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(DecodeError::OutputLimit)?;
    if bytes > MAX_DECODED_IMAGE_BYTES {
        return Err(DecodeError::OutputLimit);
    }
    Ok(bytes)
}

fn decode_raw(image: &StoredImage) -> Result<DecodedImage, DecodeError> {
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

fn decode_zlib_sampled(
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

fn decode_zlib_regions(
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

fn decode_png(image: &StoredImage) -> Result<DecodedImage, DecodeError> {
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

fn decode_png_thumbnail(
    image: &StoredImage,
    target_width: u32,
    target_height: u32,
) -> Result<DecodedImage, DecodeError> {
    decode_png_sampled(image, target_width, target_height, None, None).map(|(_, decoded)| decoded)
}

fn decode_png_sampled(
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

fn decode_png_regions(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        graphics_store::{CellAnchor, CellPixelOffset, ImageStore, PlacementSizing, SourceRect},
        graphics_transfer::DirectTransferAssembler,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn stored(controls: &str, bytes: &[u8]) -> StoredImage {
        let command = format!("\x1b_G{controls};{}\x1b\\", STANDARD.encode(bytes));
        let transfer = DirectTransferAssembler::new()
            .accept(command.as_bytes())
            .unwrap();
        let mut store = ImageStore::new();
        store.insert(transfer).unwrap();
        store.remove(1).unwrap()
    }

    fn png_bytes(color: ColorType, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(color);
            encoder.set_depth(BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(pixels).unwrap();
        }
        bytes
    }

    fn zlib_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn compressed_rgba_streams_crop_and_checks_complete_payload() {
        let mut source = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                source.extend_from_slice(&[x, y, 7, 255]);
            }
        }
        let image = StoredImage {
            format: ImageFormat::RgbaZlib,
            data: zlib_bytes(&source),
            declared_width: Some(4),
            declared_height: Some(4),
        };
        assert_eq!(image.validated_zlib_dimensions(), Ok((4, 4)));
        assert_eq!(image.decode_rgba().unwrap().pixels, source);
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
            cell_bounds: PixelSize {
                width: 4,
                height: 2,
            },
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            },
        };
        assert_eq!(
            image.resample_zlib_placement(layout).unwrap().pixels,
            [
                1, 1, 7, 255, 1, 1, 7, 255, 2, 1, 7, 255, 2, 1, 7, 255, 1, 2, 7, 255, 1, 2, 7, 255,
                2, 2, 7, 255, 2, 2, 7, 255,
            ]
        );
        assert_eq!(
            image
                .resample_zlib_placement_region(
                    layout,
                    PixelRect {
                        x: 1,
                        y: 1,
                        width: 2,
                        height: 1,
                    },
                )
                .unwrap()
                .pixels,
            [1, 2, 7, 255, 2, 2, 7, 255]
        );
        let mut corrupt = image.data.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            StoredImage {
                data: corrupt,
                ..image
            }
            .validated_zlib_dimensions(),
            Err(DecodeError::InvalidData)
        );
        let mut trailing = zlib_bytes(&source);
        trailing.push(0);
        assert_eq!(
            StoredImage {
                format: ImageFormat::RgbaZlib,
                data: trailing,
                declared_width: Some(4),
                declared_height: Some(4),
            }
            .validated_zlib_dimensions(),
            Err(DecodeError::InvalidData)
        );
        let rgb = StoredImage {
            format: ImageFormat::RgbZlib,
            data: zlib_bytes(&[1, 2, 3, 4, 5, 6]),
            declared_width: Some(2),
            declared_height: Some(1),
        };
        assert_eq!(
            rgb.decode_rgba().unwrap().pixels,
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
    }

    #[test]
    fn compressed_raw_samples_sparse_regions_in_one_bounded_pass() {
        let mut source = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                source.extend_from_slice(&[x, y, 7, 255]);
            }
        }
        let image = StoredImage {
            format: ImageFormat::RgbaZlib,
            data: zlib_bytes(&source),
            declared_width: Some(4),
            declared_height: Some(4),
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            cell_bounds: PixelSize {
                width: 4096,
                height: 4096,
            },
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 4096,
                height: 4096,
            },
        };
        let regions = [
            PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            PixelRect {
                x: 2048,
                y: 2048,
                width: 2,
                height: 2,
            },
            PixelRect {
                x: 4095,
                y: 4095,
                width: 1,
                height: 1,
            },
        ];
        let sampled = image
            .resample_zlib_placement_regions(layout, &regions)
            .unwrap();
        assert_eq!(sampled.len(), regions.len());
        for (region, pixels) in regions.into_iter().zip(sampled) {
            assert_eq!(pixels.destination, region);
            assert_eq!(
                pixels,
                image
                    .resample_zlib_placement_region(layout, region)
                    .unwrap()
            );
        }
        assert_eq!(
            image.resample_zlib_placement_regions(
                layout,
                &[
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 2048,
                        height: 2048,
                    },
                    PixelRect {
                        x: 2048,
                        y: 2048,
                        width: 2048,
                        height: 2048,
                    },
                    regions[0],
                ]
            ),
            Err(StreamZlibError::Decode(DecodeError::OutputLimit))
        );
        assert_eq!(
            image.resample_zlib_placement_regions(
                layout,
                &vec![regions[0]; MAX_SAMPLED_REGIONS + 1]
            ),
            Err(StreamZlibError::Decode(DecodeError::OutputLimit))
        );
        let mut corrupt = image.data.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            StoredImage {
                data: corrupt,
                ..image
            }
            .resample_zlib_placement_regions(layout, &regions),
            Err(StreamZlibError::Decode(DecodeError::InvalidData))
        );
    }

    fn indexed_png_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(ColorType::Indexed);
            encoder.set_depth(BitDepth::Eight);
            encoder.set_palette(vec![3, 5, 7]);
            encoder.set_trns(vec![9]);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0]).unwrap();
        }
        bytes
    }

    #[test]
    fn raw_rgb_and_rgba_have_bounded_rgba_output() {
        let rgb = stored("a=t,f=24,i=1,s=1,v=1", &[3, 5, 7]);
        assert_eq!(rgb.decode_rgba().unwrap().pixels, [3, 5, 7, 255]);
        let rgba = stored("a=t,f=32,i=1,s=1,v=1", &[3, 5, 7, 9]);
        assert_eq!(rgba.decode_rgba().unwrap().pixels, [3, 5, 7, 9]);
        assert_eq!(
            decoded_size(u32::MAX, u32::MAX),
            Err(DecodeError::OutputLimit)
        );
    }

    #[test]
    fn png_rgb_rgba_and_grayscale_decode_to_rgba() {
        for (color, source, expected) in [
            (ColorType::Rgb, vec![3, 5, 7], [3, 5, 7, 255]),
            (ColorType::Rgba, vec![3, 5, 7, 9], [3, 5, 7, 9]),
            (ColorType::Grayscale, vec![3], [3, 3, 3, 255]),
            (ColorType::GrayscaleAlpha, vec![3, 9], [3, 3, 3, 9]),
        ] {
            let image = stored("a=t,f=100,i=1", &png_bytes(color, &source));
            let decoded = image.decode_rgba().unwrap();
            assert_eq!((decoded.width, decoded.height), (1, 1));
            assert_eq!(decoded.pixels, expected);
        }
        let image = stored("a=t,f=100,i=1", &indexed_png_bytes());
        assert_eq!(image.decode_rgba().unwrap().pixels, [3, 5, 7, 9]);
    }

    #[test]
    fn corrupt_png_and_mismatched_declared_dimensions_are_rejected() {
        let image = stored("a=t,f=100,i=1", b"not a png");
        assert_eq!(image.decode_rgba(), Err(DecodeError::InvalidData));
        let image = stored("a=t,f=100,i=1,s=2", &png_bytes(ColorType::Rgb, &[1, 2, 3]));
        assert_eq!(image.decode_rgba(), Err(DecodeError::InvalidDimensions));
        let mut corrupt = png_bytes(ColorType::Rgb, &[1, 2, 3]);
        *corrupt.last_mut().unwrap() ^= 1;
        let image = stored("a=t,f=100,i=1", &corrupt);
        assert_eq!(image.decode_rgba(), Err(DecodeError::InvalidData));
    }

    #[test]
    fn oversized_png_streams_into_bounded_thumbnail() {
        let width = 3072u32;
        let height = 3072u32;
        let mut source = vec![0; usize::try_from(width * height * 4).unwrap()];
        let stride = usize::try_from(width * 4).unwrap();
        for (row, pixels) in source.chunks_exact_mut(stride).enumerate() {
            let color = if row < usize::try_from(height / 2).unwrap() {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = color;
            }
        }
        let mut data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut data, width, height);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&source).unwrap();
            writer.finish().unwrap();
        }
        let image = StoredImage {
            format: ImageFormat::Png,
            data,
            declared_width: Some(width),
            declared_height: Some(height),
        };
        assert_eq!(image.decode_rgba(), Err(DecodeError::OutputLimit));
        assert_eq!(image.validated_png_dimensions(), Ok((width, height)));
        assert_eq!(
            image.decode_png_thumbnail(width, height),
            Err(DecodeError::OutputLimit)
        );
        let thumbnail = image.decode_png_thumbnail(64, 64).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), (64, 64));
        assert_eq!(thumbnail.pixels.len(), 64 * 64 * 4);
        assert_eq!(&thumbnail.pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &thumbnail.pixels[63 * 64 * 4..63 * 64 * 4 + 4],
            &[0, 0, 255, 255]
        );
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 64,
                y: height / 2 - 32,
                width: width - 128,
                height: 64,
            },
            cell_bounds: PixelSize {
                width: 8,
                height: 8,
            },
            destination: PixelRect {
                x: 2,
                y: 3,
                width: 4,
                height: 4,
            },
        };
        let placement = image.resample_png_placement(layout).unwrap();
        assert_eq!(placement.destination, layout.destination);
        assert_eq!(placement.pixels.len(), 4 * 4 * 4);
        assert_eq!(&placement.pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &placement.pixels[3 * 4 * 4..3 * 4 * 4 + 4],
            &[0, 0, 255, 255]
        );
    }

    #[test]
    fn thumbnail_rejects_corrupt_tail_and_invalid_targets() {
        let data = png_bytes(ColorType::Rgb, &[3, 5, 7]);
        let image = StoredImage {
            format: ImageFormat::Png,
            data: data.clone(),
            declared_width: None,
            declared_height: None,
        };
        assert_eq!(
            image.decode_png_thumbnail(1, 1).unwrap().pixels,
            [3, 5, 7, 255]
        );
        assert_eq!(
            image.decode_png_thumbnail(0, 1),
            Err(DecodeError::InvalidDimensions)
        );
        assert_eq!(
            image.decode_png_thumbnail(2, 1),
            Err(DecodeError::InvalidDimensions)
        );
        let mut corrupt = data;
        *corrupt.last_mut().unwrap() ^= 1;
        let image = StoredImage {
            data: corrupt,
            ..image
        };
        assert_eq!(
            image.decode_png_thumbnail(1, 1),
            Err(DecodeError::InvalidData)
        );
        assert_eq!(
            image.validated_png_dimensions(),
            Err(DecodeError::InvalidData)
        );
        assert_eq!(
            image.resample_png_placement(PlacementPixelLayout {
                source: PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                cell_bounds: PixelSize {
                    width: 1,
                    height: 1,
                },
                destination: PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            }),
            Err(StreamPngError::Decode(DecodeError::InvalidData))
        );
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            cell_bounds: PixelSize {
                width: 1,
                height: 1,
            },
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 4096,
                height: 4096,
            },
        };
        assert_eq!(
            image.resample_png_placement_region(
                layout,
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                }
            ),
            Err(StreamPngError::Decode(DecodeError::InvalidData))
        );
    }

    #[test]
    fn thumbnail_samples_pixel_centers_in_both_axes() {
        let mut source = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                source.extend_from_slice(&[x, y, 7, 255]);
            }
        }
        let mut data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut data, 4, 4);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&source).unwrap();
            writer.finish().unwrap();
        }
        let image = StoredImage {
            format: ImageFormat::Png,
            data,
            declared_width: None,
            declared_height: None,
        };
        assert_eq!(
            image.decode_png_thumbnail(2, 2).unwrap().pixels,
            [1, 1, 7, 255, 3, 1, 7, 255, 1, 3, 7, 255, 3, 3, 7, 255]
        );
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
            cell_bounds: PixelSize {
                width: 8,
                height: 8,
            },
            destination: PixelRect {
                x: 5,
                y: 7,
                width: 4,
                height: 2,
            },
        };
        let resampled = image.resample_png_placement(layout).unwrap();
        assert_eq!(resampled.destination, layout.destination);
        assert_eq!(
            resampled.pixels,
            [
                1, 1, 7, 255, 1, 1, 7, 255, 2, 1, 7, 255, 2, 1, 7, 255, 1, 2, 7, 255, 1, 2, 7, 255,
                2, 2, 7, 255, 2, 2, 7, 255,
            ]
        );
        let region = PixelRect {
            x: 6,
            y: 8,
            width: 2,
            height: 1,
        };
        let clipped = image.resample_png_placement_region(layout, region).unwrap();
        assert_eq!(clipped.destination, region);
        assert_eq!(clipped.pixels, [1, 2, 7, 255, 2, 2, 7, 255]);
        assert_eq!(
            image.resample_png_placement_region(layout, PixelRect { x: 8, ..region }),
            Err(StreamPngError::Decode(DecodeError::InvalidDimensions))
        );
        let huge = PlacementPixelLayout {
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 4096,
                height: 4096,
            },
            ..layout
        };
        assert_eq!(
            image.resample_png_placement(huge),
            Err(StreamPngError::Decode(DecodeError::OutputLimit))
        );
        assert_eq!(
            image
                .resample_png_placement_region(
                    huge,
                    PixelRect {
                        x: 3072,
                        y: 3072,
                        width: 2,
                        height: 2,
                    }
                )
                .unwrap()
                .pixels,
            [2, 2, 7, 255].repeat(4)
        );
        assert_eq!(
            image.resample_png_placement(PlacementPixelLayout {
                source: PixelRect {
                    x: 3,
                    y: 3,
                    width: 2,
                    height: 2,
                },
                ..layout
            }),
            Err(StreamPngError::Decode(DecodeError::InvalidDimensions))
        );
    }

    #[test]
    fn png_samples_sparse_regions_and_checks_complete_tail() {
        let mut source = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                source.extend_from_slice(&[x, y, 7, 255]);
            }
        }
        let mut data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut data, 4, 4);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&source).unwrap();
            writer.finish().unwrap();
        }
        let image = StoredImage {
            format: ImageFormat::Png,
            data,
            declared_width: Some(4),
            declared_height: Some(4),
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            cell_bounds: PixelSize {
                width: 4096,
                height: 4096,
            },
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 4096,
                height: 4096,
            },
        };
        let regions = [
            PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            PixelRect {
                x: 2048,
                y: 2048,
                width: 2,
                height: 2,
            },
            PixelRect {
                x: 4095,
                y: 4095,
                width: 1,
                height: 1,
            },
        ];
        let sampled = image
            .resample_png_placement_regions(layout, &regions)
            .unwrap();
        for (region, pixels) in regions.into_iter().zip(sampled) {
            assert_eq!(
                pixels,
                image.resample_png_placement_region(layout, region).unwrap()
            );
        }
        assert_eq!(
            image.resample_png_placement_regions(
                layout,
                &[
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 2048,
                        height: 2048,
                    },
                    PixelRect {
                        x: 2048,
                        y: 2048,
                        width: 2048,
                        height: 2048,
                    },
                    regions[0],
                ]
            ),
            Err(StreamPngError::Decode(DecodeError::OutputLimit))
        );
        assert_eq!(
            image
                .resample_png_placement_regions(layout, &vec![regions[0]; MAX_SAMPLED_REGIONS + 1]),
            Err(StreamPngError::Decode(DecodeError::OutputLimit))
        );
        let mut corrupt = image.data.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            StoredImage {
                data: corrupt,
                ..image
            }
            .resample_png_placement_regions(layout, &regions),
            Err(StreamPngError::Decode(DecodeError::InvalidData))
        );
    }

    #[test]
    fn png_and_zlib_use_identical_cropped_sparse_sampling() {
        let mut source = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                source.extend_from_slice(&[x, y, 7, 100 + x + y]);
            }
        }
        let mut png_data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_data, 4, 4);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&source).unwrap();
            writer.finish().unwrap();
        }
        let png = StoredImage {
            format: ImageFormat::Png,
            data: png_data,
            declared_width: Some(4),
            declared_height: Some(4),
        };
        let zlib = StoredImage {
            format: ImageFormat::RgbaZlib,
            data: zlib_bytes(&source),
            declared_width: Some(4),
            declared_height: Some(4),
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
            cell_bounds: PixelSize {
                width: 8,
                height: 8,
            },
            destination: PixelRect {
                x: 5,
                y: 7,
                width: 8,
                height: 8,
            },
        };
        let regions = [
            PixelRect {
                x: 5,
                y: 7,
                width: 1,
                height: 1,
            },
            PixelRect {
                x: 8,
                y: 10,
                width: 2,
                height: 2,
            },
            PixelRect {
                x: 12,
                y: 14,
                width: 1,
                height: 1,
            },
        ];
        assert_eq!(
            png.resample_png_placement_regions(layout, &regions)
                .unwrap(),
            zlib.resample_zlib_placement_regions(layout, &regions)
                .unwrap()
        );
    }

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

    #[test]
    fn sparse_png_and_zlib_match_full_rgba_resampling() {
        for (width, height) in [(3, 2), (5, 4), (7, 6)] {
            for color in [
                ColorType::Grayscale,
                ColorType::GrayscaleAlpha,
                ColorType::Rgb,
                ColorType::Rgba,
            ] {
                let mut raw = Vec::new();
                let mut rgba = Vec::new();
                for y in 0..height {
                    for x in 0..width {
                        let pixel = [
                            (x * 31 + y * 7) as u8,
                            (x * 13 + y * 29) as u8,
                            (x * 3 + y * 17) as u8,
                            (x * 19 + y * 23) as u8,
                        ];
                        match color {
                            ColorType::Grayscale => {
                                raw.push(pixel[0]);
                                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]);
                            }
                            ColorType::GrayscaleAlpha => {
                                raw.extend_from_slice(&[pixel[0], pixel[3]]);
                                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[3]]);
                            }
                            ColorType::Rgb => {
                                raw.extend_from_slice(&pixel[..3]);
                                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                            }
                            ColorType::Rgba => {
                                raw.extend_from_slice(&pixel);
                                rgba.extend_from_slice(&pixel);
                            }
                            ColorType::Indexed => unreachable!(),
                        }
                    }
                }
                let mut png_data = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut png_data, width, height);
                    encoder.set_color(color);
                    encoder.set_depth(BitDepth::Eight);
                    let mut writer = encoder.write_header().unwrap();
                    writer.write_image_data(&raw).unwrap();
                    writer.finish().unwrap();
                }
                let png = StoredImage {
                    format: ImageFormat::Png,
                    data: png_data,
                    declared_width: Some(width),
                    declared_height: Some(height),
                };
                let zlib = match color {
                    ColorType::Rgb | ColorType::Rgba => Some(StoredImage {
                        format: if color == ColorType::Rgb {
                            ImageFormat::RgbZlib
                        } else {
                            ImageFormat::RgbaZlib
                        },
                        data: zlib_bytes(&raw),
                        declared_width: Some(width),
                        declared_height: Some(height),
                    }),
                    _ => None,
                };
                let decoded = DecodedImage {
                    width,
                    height,
                    pixels: rgba,
                };

                for source in [
                    PixelRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    PixelRect {
                        x: 1,
                        y: 1,
                        width: width - 1,
                        height: height - 1,
                    },
                ] {
                    for (target_width, target_height) in [(1, 1), (2, 3), (width + 3, height + 2)] {
                        let destination = PixelRect {
                            x: 11,
                            y: 13,
                            width: target_width,
                            height: target_height,
                        };
                        let layout = PlacementPixelLayout {
                            source,
                            cell_bounds: PixelSize {
                                width: target_width,
                                height: target_height,
                            },
                            destination,
                        };
                        let mut regions = vec![PixelRect {
                            x: destination.x + target_width - 1,
                            y: destination.y + target_height - 1,
                            width: 1,
                            height: 1,
                        }];
                        if target_width > 1 && target_height > 1 {
                            regions.push(PixelRect {
                                x: destination.x,
                                y: destination.y,
                                width: 1,
                                height: 1,
                            });
                        }
                        if target_width >= 4 && target_height >= 4 {
                            regions.push(PixelRect {
                                x: destination.x + 1,
                                y: destination.y + 1,
                                width: 2,
                                height: 2,
                            });
                        }
                        let full = decoded.resample_placement(layout).unwrap();
                        let mut sampled_outputs = vec![
                            png.resample_png_placement_regions(layout, &regions)
                                .unwrap(),
                        ];
                        if let Some(zlib) = &zlib {
                            sampled_outputs.push(
                                zlib.resample_zlib_placement_regions(layout, &regions)
                                    .unwrap(),
                            );
                        }
                        for sampled in sampled_outputs {
                            for (region, actual) in regions.iter().zip(sampled) {
                                let mut expected = Vec::new();
                                for row in 0..region.height {
                                    let offset = (((region.y - destination.y + row) * target_width
                                        + region.x
                                        - destination.x)
                                        * 4)
                                        as usize;
                                    expected.extend_from_slice(
                                        &full.pixels[offset..offset + region.width as usize * 4],
                                    );
                                }
                                assert_eq!(actual.destination, *region);
                                assert_eq!(
                                    actual.pixels, expected,
                                    "{color:?}, {layout:?}, {region:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "set RUSTMUX_COMPAT_IMAGE to a PNG and run this test explicitly"]
    fn user_png_streams_to_thumbnail() {
        let Some(path) = std::env::var_os("RUSTMUX_COMPAT_IMAGE") else {
            println!("SKIP: set RUSTMUX_COMPAT_IMAGE to a PNG file");
            return;
        };
        let data = std::fs::read(path).unwrap();
        assert!(data.len() <= crate::graphics_store::MAX_PANE_IMAGE_BYTES);
        let image = StoredImage {
            format: ImageFormat::Png,
            data,
            declared_width: None,
            declared_height: None,
        };
        let thumbnail = image.decode_png_thumbnail(512, 512).unwrap();
        assert_eq!(thumbnail.pixels.len(), 512 * 512 * 4);
        assert!(
            thumbnail
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
    }

    #[test]
    fn resample_cropped_rgba_content_without_allocating_letterbox_padding() {
        let rgba = [
            1, 0, 0, 10, 2, 0, 0, 20, 3, 0, 0, 30, 4, 0, 0, 40, 5, 0, 0, 50, 6, 0, 0, 60,
        ];
        let image = stored("a=t,f=32,i=1,s=3,v=2", &rgba).decode_rgba().unwrap();
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 1,
                y: 0,
                width: 2,
                height: 2,
            },
            cell_bounds: PixelSize {
                width: 6,
                height: 6,
            },
            destination: PixelRect {
                x: 2,
                y: 1,
                width: 4,
                height: 4,
            },
        };
        let resampled = image.resample_placement(layout).unwrap();
        assert_eq!(resampled.destination, layout.destination);
        assert_eq!(resampled.pixels.len(), 4 * 4 * 4);
        assert_eq!(
            resampled
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| (rgba[0], rgba[3]))
                .collect::<Vec<_>>(),
            [
                (2, 20),
                (2, 20),
                (3, 30),
                (3, 30),
                (2, 20),
                (2, 20),
                (3, 30),
                (3, 30),
                (5, 50),
                (5, 50),
                (6, 60),
                (6, 60),
                (5, 50),
                (5, 50),
                (6, 60),
                (6, 60),
            ]
        );
    }

    #[test]
    fn resample_raw_region_matches_full_pixel_center_sampling() {
        let mut pixels = Vec::new();
        for y in 0..3u8 {
            for x in 0..4u8 {
                pixels.extend_from_slice(&[x, y, 7, 255]);
            }
        }
        let image = DecodedImage {
            width: 4,
            height: 3,
            pixels,
        };
        for (width, height) in [(7, 5), (2, 2)] {
            let layout = PlacementPixelLayout {
                source: PixelRect {
                    x: 1,
                    y: 0,
                    width: 3,
                    height: 3,
                },
                cell_bounds: PixelSize { width, height },
                destination: PixelRect {
                    x: 11,
                    y: 13,
                    width,
                    height,
                },
            };
            let region = PixelRect {
                x: 12,
                y: 14,
                width: width - 1,
                height: height - 1,
            };
            let full = image.resample_placement(layout).unwrap();
            let visible = image.resample_placement_region(layout, region).unwrap();
            assert_eq!(visible.destination, region);
            for y in 0..region.height as usize {
                let full_start = ((y + 1) * width as usize + 1) * 4;
                let visible_start = y * region.width as usize * 4;
                assert_eq!(
                    &visible.pixels[visible_start..visible_start + region.width as usize * 4],
                    &full.pixels[full_start..full_start + region.width as usize * 4]
                );
            }
        }
    }

    #[test]
    fn resample_raw_region_stays_bounded_for_oversized_destination() {
        let image = DecodedImage {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            cell_bounds: PixelSize {
                width: 4096,
                height: 4096,
            },
            destination: PixelRect {
                x: 5,
                y: 7,
                width: 4096,
                height: 4096,
            },
        };
        assert_eq!(
            image.resample_placement(layout),
            Err(ResampleError::OutputLimit)
        );
        let region = PixelRect {
            x: 4099,
            y: 4101,
            width: 2,
            height: 2,
        };
        assert_eq!(
            image.resample_placement_region(layout, region),
            Ok(ResampledPlacement {
                destination: region,
                pixels: [1, 2, 3, 4].repeat(4),
            })
        );
    }

    #[test]
    fn resample_uses_pixel_centers_when_shrinking() {
        let mut pixels = Vec::new();
        for value in 0..16 {
            pixels.extend_from_slice(&[value, 0, 0, 255]);
        }
        let image = DecodedImage {
            width: 4,
            height: 4,
            pixels,
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            cell_bounds: PixelSize {
                width: 2,
                height: 2,
            },
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        };
        let resampled = image.resample_placement(layout).unwrap();
        assert_eq!(
            resampled
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| rgba[0])
                .collect::<Vec<_>>(),
            [5, 7, 13, 15]
        );
    }

    #[test]
    fn resample_rejects_bad_layout_pixels_and_excessive_output() {
        let image = DecodedImage {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            cell_bounds: PixelSize {
                width: 1,
                height: 1,
            },
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
        };
        assert_eq!(
            DecodedImage {
                pixels: vec![1, 2, 3],
                ..image
            }
            .resample_placement(layout),
            Err(ResampleError::InvalidPixels)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                source: PixelRect {
                    x: 1,
                    ..layout.source
                },
                ..layout
            }),
            Err(ResampleError::InvalidLayout)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                source: PixelRect {
                    x: u32::MAX,
                    width: 2,
                    ..layout.source
                },
                ..layout
            }),
            Err(ResampleError::InvalidLayout)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                destination: PixelRect {
                    width: 0,
                    ..layout.destination
                },
                ..layout
            }),
            Err(ResampleError::InvalidLayout)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                destination: PixelRect {
                    width: 10_000,
                    height: 10_000,
                    ..layout.destination
                },
                ..layout
            }),
            Err(ResampleError::OutputLimit)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                destination: PixelRect {
                    x: u32::MAX,
                    width: 2,
                    ..layout.destination
                },
                ..layout
            }),
            Err(ResampleError::InvalidLayout)
        );
    }

    #[test]
    fn viewport_clip_copies_only_visible_rows_and_columns() {
        let mut pixels = Vec::new();
        for value in 1..=12 {
            pixels.extend_from_slice(&[value, 0, 0, 255 - value]);
        }
        let placement = ResampledPlacement {
            destination: PixelRect {
                x: 1,
                y: 1,
                width: 4,
                height: 3,
            },
            pixels,
        };
        let clipped = placement
            .clip_to_viewport(
                SignedPixelPoint { x: -2, y: -2 },
                PixelSize {
                    width: 3,
                    height: 2,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            clipped.destination,
            PixelRect {
                x: 0,
                y: 0,
                width: 3,
                height: 2,
            }
        );
        assert_eq!(
            clipped
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| (rgba[0], rgba[3]))
                .collect::<Vec<_>>(),
            [
                (6, 249),
                (7, 248),
                (8, 247),
                (10, 245),
                (11, 244),
                (12, 243)
            ]
        );
        let right_bottom = placement
            .clip_to_viewport(
                SignedPixelPoint { x: 0, y: 0 },
                PixelSize {
                    width: 3,
                    height: 3,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            right_bottom.destination,
            PixelRect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }
        );
        assert_eq!(
            right_bottom
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| rgba[0])
                .collect::<Vec<_>>(),
            [1, 2, 5, 6]
        );
        let entirely_visible = placement
            .clip_to_viewport(
                SignedPixelPoint { x: 5, y: 6 },
                PixelSize {
                    width: 20,
                    height: 20,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            entirely_visible.destination,
            PixelRect {
                x: 6,
                y: 7,
                width: 4,
                height: 3,
            }
        );
        assert_eq!(entirely_visible.pixels, placement.pixels);
    }

    #[test]
    fn viewport_clip_handles_disjoint_and_invalid_inputs() {
        let placement = ResampledPlacement {
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            pixels: vec![1, 2, 3, 4],
        };
        let viewport = PixelSize {
            width: 2,
            height: 2,
        };
        for anchor in [
            SignedPixelPoint { x: 2, y: 0 },
            SignedPixelPoint { x: -1, y: 0 },
            SignedPixelPoint {
                x: i64::MIN,
                y: i64::MAX,
            },
        ] {
            assert_eq!(placement.clip_to_viewport(anchor, viewport), Ok(None));
        }
        assert_eq!(
            placement.clip_to_viewport(
                SignedPixelPoint { x: 0, y: 0 },
                PixelSize {
                    width: 0,
                    height: 2,
                }
            ),
            Ok(None)
        );
        assert_eq!(
            ResampledPlacement {
                pixels: vec![1, 2, 3],
                ..placement
            }
            .clip_to_viewport(SignedPixelPoint { x: 0, y: 0 }, viewport),
            Err(ClipError::InvalidPixels)
        );
        assert_eq!(
            ResampledPlacement {
                destination: PixelRect {
                    width: 0,
                    ..placement.destination
                },
                pixels: placement.pixels.clone(),
            }
            .clip_to_viewport(SignedPixelPoint { x: 0, y: 0 }, viewport),
            Err(ClipError::InvalidDestination)
        );
        assert_eq!(
            ResampledPlacement {
                destination: PixelRect {
                    x: u32::MAX,
                    width: 2,
                    ..placement.destination
                },
                pixels: placement.pixels.clone(),
            }
            .clip_to_viewport(SignedPixelPoint { x: 0, y: 0 }, viewport),
            Err(ClipError::InvalidDestination)
        );
        assert_eq!(
            ResampledPlacement {
                destination: PixelRect {
                    width: 10_000,
                    height: 10_000,
                    ..placement.destination
                },
                pixels: placement.pixels.clone(),
            }
            .clip_to_viewport(SignedPixelPoint { x: 0, y: 0 }, viewport),
            Err(ClipError::OutputLimit)
        );
    }

    #[test]
    fn scroll_clip_removes_only_the_recorded_pixel_rows() {
        let placement = ResampledPlacement {
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 6,
            },
            pixels: (1..=6).flat_map(|value| [value, 0, 0, 255]).collect(),
        };
        let geometry = PlacementGeometry {
            anchor: CellAnchor::default(),
            row_offset: 0,
            source: SourceRect::default(),
            cell_offset: CellPixelOffset::default(),
            columns: Some(1),
            rows: Some(3),
            sizing: PlacementSizing::FitBox,
            clip_top_rows: 1,
            clip_bottom_rows: 1,
            z_index: 0,
            cursor_stays: false,
        };
        let cell = CellPixelSize::new(1, 2).unwrap();
        let viewport = PixelSize {
            width: 1,
            height: 8,
        };
        let values = |clipped: ClippedPlacement| {
            clipped
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| rgba[0])
                .collect::<Vec<_>>()
        };
        let both = placement
            .clip_to_viewport_with_scroll_clip(geometry, cell, viewport)
            .unwrap()
            .unwrap();
        assert_eq!(both.destination.y, 2);
        assert_eq!(both.destination.height, 2);
        assert_eq!(values(both), [3, 4]);
        assert_eq!(
            visible_placement_region(placement.destination, geometry, cell, viewport),
            Ok(Some((
                PixelRect {
                    x: 0,
                    y: 2,
                    width: 1,
                    height: 2,
                },
                PixelRect {
                    x: 0,
                    y: 2,
                    width: 1,
                    height: 2,
                },
            )))
        );
        let top_only = placement
            .clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_bottom_rows: 0,
                    ..geometry
                },
                cell,
                viewport,
            )
            .unwrap()
            .unwrap();
        assert_eq!(values(top_only), [3, 4, 5, 6]);
        let bottom_only = placement
            .clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_top_rows: 0,
                    ..geometry
                },
                cell,
                viewport,
            )
            .unwrap()
            .unwrap();
        assert_eq!(values(bottom_only), [1, 2, 3, 4]);

        // With no bottom clip, a start-cell Y offset may still protrude past
        // the nominal placement rows without being cut off.
        let shifted = ResampledPlacement {
            destination: PixelRect {
                y: 1,
                ..placement.destination
            },
            pixels: placement.pixels.clone(),
        };
        let top_only = shifted
            .clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_bottom_rows: 0,
                    ..geometry
                },
                cell,
                viewport,
            )
            .unwrap()
            .unwrap();
        assert_eq!(top_only.destination.y, 2);
        assert_eq!(values(top_only), [2, 3, 4, 5, 6]);
    }

    #[test]
    fn scroll_clip_rejects_unknown_geometry_and_full_clips() {
        let placement = ResampledPlacement {
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            pixels: vec![1, 2, 3, 4],
        };
        let geometry = PlacementGeometry {
            anchor: CellAnchor::default(),
            row_offset: 0,
            source: SourceRect::default(),
            cell_offset: CellPixelOffset::default(),
            columns: None,
            rows: None,
            sizing: PlacementSizing::Natural,
            clip_top_rows: 1,
            clip_bottom_rows: 0,
            z_index: 0,
            cursor_stays: false,
        };
        let cell = CellPixelSize::new(1, 1).unwrap();
        let viewport = PixelSize {
            width: 1,
            height: 1,
        };
        assert_eq!(
            placement.clip_to_viewport_with_scroll_clip(geometry, cell, viewport),
            Err(ClipError::InvalidGeometry)
        );
        assert_eq!(
            placement.clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    rows: Some(1),
                    ..geometry
                },
                cell,
                viewport,
            ),
            Ok(None)
        );
        assert_eq!(
            ResampledPlacement {
                pixels: vec![1, 2, 3],
                ..placement
            }
            .clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    rows: Some(1),
                    ..geometry
                },
                cell,
                viewport,
            ),
            Err(ClipError::InvalidPixels)
        );
        assert_eq!(
            placement.clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_top_rows: 0,
                    row_offset: i64::MAX,
                    ..geometry
                },
                cell,
                viewport,
            ),
            Ok(None)
        );
        assert_eq!(
            placement.clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_top_rows: 0,
                    row_offset: i64::MAX,
                    ..geometry
                },
                CellPixelSize::new(1, 2).unwrap(),
                viewport,
            ),
            Err(ClipError::InvalidGeometry)
        );
    }
}
