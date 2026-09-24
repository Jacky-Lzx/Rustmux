//! Opt-in, bounded conversion of stored Kitty image data to RGBA pixels.
//! Decoding does not place or render an image in a terminal.

use crate::graphics_store::{
    ImageFormat, PixelRect, PixelSize, PlacementPixelLayout, SignedPixelPoint, StoredImage,
};
use png::{BitDepth, ColorType, Decoder, Limits, Transformations};
use std::io::Cursor;

pub const MAX_DECODED_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RESAMPLED_PLACEMENT_BYTES: usize = MAX_DECODED_IMAGE_BYTES;

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
    OutputLimit,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResampleError {
    InvalidPixels,
    InvalidLayout,
    OutputLimit,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DecodeError {
    InvalidDimensions,
    OutputLimit,
    InvalidData,
    UnsupportedPng,
}

impl StoredImage {
    /// Decode pixels on demand. PNG metadata, checksums and decoded byte count
    /// are validated before returning pixels; the stored source is unchanged.
    pub fn decode_rgba(&self) -> Result<DecodedImage, DecodeError> {
        match self.format {
            ImageFormat::Rgb | ImageFormat::Rgba => decode_raw(self),
            ImageFormat::Png => decode_png(self),
        }
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
        {
            return Err(ResampleError::InvalidLayout);
        }
        let output_size = decoded_size(destination.width, destination.height)
            .map_err(|_| ResampleError::OutputLimit)?;
        let mut pixels = vec![0; output_size];
        let output_width = usize::try_from(destination.width).unwrap();
        let input_width = usize::try_from(self.width).unwrap();
        for (y, row) in pixels.chunks_exact_mut(output_width * 4).enumerate() {
            let source_y = source.y + nearest_sample(y, source.height, destination.height);
            for (x, rgba) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let source_x = source.x + nearest_sample(x, source.width, destination.width);
                let index = (usize::try_from(source_y).unwrap() * input_width
                    + usize::try_from(source_x).unwrap())
                    * 4;
                rgba.copy_from_slice(&self.pixels[index..index + 4]);
            }
        }
        Ok(ResampledPlacement {
            destination,
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
        if viewport.width == 0 || viewport.height == 0 {
            return Ok(None);
        }
        let left = i128::from(anchor.x) + i128::from(source.x);
        let top = i128::from(anchor.y) + i128::from(source.y);
        let right = left + i128::from(source.width);
        let bottom = top + i128::from(source.height);
        let visible_left = left.max(0);
        let visible_top = top.max(0);
        let visible_right = right.min(i128::from(viewport.width));
        let visible_bottom = bottom.min(i128::from(viewport.height));
        if visible_left >= visible_right || visible_top >= visible_bottom {
            return Ok(None);
        }
        // Intersection with a validated source and viewport bounds all these
        // values to u32/usize, even for an extreme signed anchor.
        let x = u32::try_from(visible_left).unwrap();
        let y = u32::try_from(visible_top).unwrap();
        let width = u32::try_from(visible_right - visible_left).unwrap();
        let height = u32::try_from(visible_bottom - visible_top).unwrap();
        let skip_x = usize::try_from(visible_left - left).unwrap();
        let skip_y = usize::try_from(visible_top - top).unwrap();
        let input_width = usize::try_from(source.width).unwrap();
        let output_width = usize::try_from(width).unwrap();
        let mut pixels = vec![0; decoded_size(width, height).unwrap()];
        for (row, destination_row) in pixels.chunks_exact_mut(output_width * 4).enumerate() {
            let start = ((skip_y + row) * input_width + skip_x) * 4;
            destination_row.copy_from_slice(&self.pixels[start..start + output_width * 4]);
        }
        Ok(Some(ClippedPlacement {
            destination: PixelRect {
                x,
                y,
                width,
                height,
            },
            pixels,
        }))
    }
}

fn nearest_sample(output_index: usize, source_extent: u32, output_extent: u32) -> u32 {
    // Sample at pixel centers. All extents are nonzero and the quotient is
    // strictly smaller than `source_extent`.
    let center = 2 * u128::try_from(output_index).unwrap() + 1;
    let numerator = center * u128::from(source_extent);
    let denominator = 2 * u128::from(output_extent);
    u32::try_from(numerator / denominator).unwrap()
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
        ImageFormat::Png => unreachable!(),
    };
    Ok(DecodedImage {
        width,
        height,
        pixels,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        graphics_store::{ImageStore, PixelSize},
        graphics_transfer::DirectTransferAssembler,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};

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
}
