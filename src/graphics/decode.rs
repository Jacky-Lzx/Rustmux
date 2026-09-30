//! Opt-in, bounded conversion of stored Kitty image data to RGBA pixels.
//! Decoding does not place or render an image in a terminal.

mod placement;
mod png_reader;
mod sampling;

pub(crate) use placement::visible_placement_region;
pub use placement::{ClipError, ClippedPlacement, ResampleError, ResampledPlacement};

use self::png_reader::{decode_png, decode_png_regions, decode_png_sampled, decode_png_thumbnail};
use self::sampling::{RowSampler, sample_output_buffers};
use super::geometry::{PixelRect, PlacementPixelLayout};
use crate::graphics_store::{ImageFormat, StoredImage};
use crate::graphics_transfer::MAX_STREAMED_RAW_BYTES;
use flate2::bufread::ZlibDecoder;
use png::ColorType;
use std::io::Read;

pub const MAX_DECODED_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RESAMPLED_PLACEMENT_BYTES: usize = MAX_DECODED_IMAGE_BYTES;

#[derive(Debug, Eq, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Row-major, eight-bit RGBA pixels.
    pub pixels: Vec<u8>,
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

#[cfg(test)]
mod tests {
    use super::sampling::MAX_SAMPLED_REGIONS;
    use super::*;
    use crate::{
        graphics::geometry::PixelSize, graphics_store::ImageStore,
        graphics_transfer::DirectTransferAssembler,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::ZlibEncoder};
    use png::BitDepth;
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
}
