//! Opt-in, bounded image-only composition for clipped Kitty placements.
//! Text/background ordering and terminal redraw are not handled here.

use super::geometry::PixelSize;
use crate::{
    graphics_decode::{ClippedPlacement, DecodedImage, MAX_DECODED_IMAGE_BYTES},
    graphics_store::MAX_PANE_PLACEMENTS,
    pane::MAX_CELLS,
};

pub const MAX_COMPOSITE_INPUT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_COMPOSITE_LAYERS: usize = MAX_PANE_PLACEMENTS + MAX_CELLS;

#[derive(Debug, Clone, Copy)]
pub struct ImageLayer<'a> {
    pub image_id: u32,
    pub z_index: i32,
    pub placement: &'a ClippedPlacement,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CompositeError {
    InvalidViewport,
    OutputLimit,
    TooManyLayers,
    InvalidLayer,
    InputLimit,
}

/// Blend visible RGBA image content onto a transparent viewport. Lower `z`
/// and, for equal `z`, lower image ID are drawn first. Equal keys retain input
/// order; the protocol does not define the order of those ties.
pub fn compose_image_layers(
    viewport: PixelSize,
    layers: &[ImageLayer<'_>],
) -> Result<DecodedImage, CompositeError> {
    if viewport.width == 0 || viewport.height == 0 {
        return Err(CompositeError::InvalidViewport);
    }
    let canvas_bytes = rgba_bytes(viewport.width, viewport.height)
        .filter(|&bytes| bytes <= MAX_DECODED_IMAGE_BYTES)
        .ok_or(CompositeError::OutputLimit)?;
    if layers.len() > MAX_COMPOSITE_LAYERS {
        return Err(CompositeError::TooManyLayers);
    }
    let mut input_bytes = 0usize;
    for layer in layers {
        let rect = layer.placement.destination;
        let Some(bytes) = rgba_bytes(rect.width, rect.height) else {
            return Err(CompositeError::InvalidLayer);
        };
        if rect
            .x
            .checked_add(rect.width)
            .is_none_or(|right| right > viewport.width)
            || rect
                .y
                .checked_add(rect.height)
                .is_none_or(|bottom| bottom > viewport.height)
            || layer.placement.pixels.len() != bytes
        {
            return Err(CompositeError::InvalidLayer);
        }
        input_bytes = input_bytes
            .checked_add(bytes)
            .filter(|&total| total <= MAX_COMPOSITE_INPUT_BYTES)
            .ok_or(CompositeError::InputLimit)?;
    }

    let mut sorted: Vec<_> = layers.iter().collect();
    sorted.sort_by_key(|layer| (layer.z_index, layer.image_id));
    let mut pixels = vec![0; canvas_bytes];
    let canvas_width = usize::try_from(viewport.width).unwrap();
    for layer in sorted {
        let rect = layer.placement.destination;
        let layer_width = usize::try_from(rect.width).unwrap();
        let left = usize::try_from(rect.x).unwrap();
        let top = usize::try_from(rect.y).unwrap();
        for (row, source_row) in layer
            .placement
            .pixels
            .chunks_exact(layer_width * 4)
            .enumerate()
        {
            let start = ((top + row) * canvas_width + left) * 4;
            let target_row = &mut pixels[start..start + layer_width * 4];
            for (target, source) in target_row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(source_row.as_chunks::<4>().0.iter())
            {
                blend_source_over(target, source);
            }
        }
    }
    Ok(DecodedImage {
        width: viewport.width,
        height: viewport.height,
        pixels,
    })
}

fn rgba_bytes(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

fn blend_source_over(target: &mut [u8; 4], source: &[u8; 4]) {
    let source_alpha = u32::from(source[3]);
    if source_alpha == 0 {
        return;
    }
    let target_alpha = u32::from(target[3]);
    if source_alpha == 255 || target_alpha == 0 {
        *target = *source;
        return;
    }
    let remaining = 255 - source_alpha;
    let alpha_numerator = source_alpha * 255 + target_alpha * remaining;
    for channel in 0..3 {
        let color_numerator = u32::from(source[channel]) * source_alpha * 255
            + u32::from(target[channel]) * target_alpha * remaining;
        target[channel] = ((color_numerator + alpha_numerator / 2) / alpha_numerator) as u8;
    }
    target[3] = ((alpha_numerator + 127) / 255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics_store::PixelRect;

    fn pixel(x: u32, y: u32, rgba: [u8; 4]) -> ClippedPlacement {
        ClippedPlacement {
            destination: PixelRect {
                x,
                y,
                width: 1,
                height: 1,
            },
            pixels: rgba.to_vec(),
        }
    }

    #[test]
    fn transparent_canvas_and_offset_content() {
        let viewport = PixelSize {
            width: 2,
            height: 1,
        };
        assert_eq!(
            compose_image_layers(viewport, &[]).unwrap().pixels,
            [0, 0, 0, 0, 0, 0, 0, 0]
        );
        let content = pixel(1, 0, [3, 5, 7, 255]);
        let transparent = pixel(1, 0, [100, 200, 50, 0]);
        let layers = [
            ImageLayer {
                image_id: 1,
                z_index: -1,
                placement: &content,
            },
            ImageLayer {
                image_id: 2,
                z_index: 1,
                placement: &transparent,
            },
        ];
        assert_eq!(
            compose_image_layers(viewport, &layers).unwrap().pixels,
            [0, 0, 0, 0, 3, 5, 7, 255]
        );
    }

    #[test]
    fn equal_z_uses_image_id_and_blends_straight_alpha() {
        let lower = pixel(0, 0, [255, 0, 0, 128]);
        let upper = pixel(0, 0, [0, 0, 255, 128]);
        // Deliberately provide the higher ID first: lower IDs are underneath.
        let layers = [
            ImageLayer {
                image_id: 2,
                z_index: 0,
                placement: &upper,
            },
            ImageLayer {
                image_id: 1,
                z_index: 0,
                placement: &lower,
            },
        ];
        assert_eq!(
            compose_image_layers(
                PixelSize {
                    width: 1,
                    height: 1,
                },
                &layers,
            )
            .unwrap()
            .pixels,
            [85, 0, 170, 192]
        );
    }

    #[test]
    fn z_index_precedes_image_id_and_equal_keys_keep_input_order() {
        let green = pixel(0, 0, [0, 255, 0, 255]);
        let red = pixel(0, 0, [255, 0, 0, 255]);
        let viewport = PixelSize {
            width: 1,
            height: 1,
        };
        assert_eq!(
            compose_image_layers(
                viewport,
                &[
                    ImageLayer {
                        image_id: 1,
                        z_index: 0,
                        placement: &red,
                    },
                    ImageLayer {
                        image_id: 100,
                        z_index: -1,
                        placement: &green,
                    },
                ],
            )
            .unwrap()
            .pixels,
            red.pixels
        );
        assert_eq!(
            compose_image_layers(
                viewport,
                &[
                    ImageLayer {
                        image_id: 1,
                        z_index: 0,
                        placement: &red,
                    },
                    ImageLayer {
                        image_id: 1,
                        z_index: 0,
                        placement: &green,
                    },
                ],
            )
            .unwrap()
            .pixels,
            green.pixels
        );
    }

    #[test]
    fn invalid_geometry_and_work_limits_are_rejected() {
        let viewport = PixelSize {
            width: 1,
            height: 1,
        };
        let valid = pixel(0, 0, [1, 2, 3, 4]);
        let layer = ImageLayer {
            image_id: 1,
            z_index: 0,
            placement: &valid,
        };
        assert_eq!(
            compose_image_layers(
                PixelSize {
                    width: 0,
                    height: 1
                },
                &[]
            ),
            Err(CompositeError::InvalidViewport)
        );
        assert_eq!(
            compose_image_layers(
                PixelSize {
                    width: 10_000,
                    height: 10_000,
                },
                &[],
            ),
            Err(CompositeError::OutputLimit)
        );
        assert_eq!(
            compose_image_layers(viewport, &vec![layer; MAX_COMPOSITE_LAYERS + 1]),
            Err(CompositeError::TooManyLayers)
        );
        let misplaced = pixel(1, 0, [1, 2, 3, 4]);
        assert_eq!(
            compose_image_layers(
                viewport,
                &[ImageLayer {
                    placement: &misplaced,
                    ..layer
                }],
            ),
            Err(CompositeError::InvalidLayer)
        );
        let malformed = ClippedPlacement {
            destination: valid.destination,
            pixels: vec![1, 2, 3],
        };
        assert_eq!(
            compose_image_layers(
                viewport,
                &[ImageLayer {
                    placement: &malformed,
                    ..layer
                }],
            ),
            Err(CompositeError::InvalidLayer)
        );
        let large = ClippedPlacement {
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 512,
                height: 512,
            },
            pixels: vec![0; 512 * 512 * 4],
        };
        let layers = vec![
            ImageLayer {
                placement: &large,
                ..layer
            };
            65
        ];
        assert_eq!(
            compose_image_layers(
                PixelSize {
                    width: 512,
                    height: 512,
                },
                &layers,
            ),
            Err(CompositeError::InputLimit)
        );
    }
}
