//! Crop, tile, and encode images for the outer Kitty terminal.

use std::io;

use super::{
    decode::DecodedImage,
    output::{EncodedKittyPng, kitty_rgb_placement_len, kitty_rgba_placement_len},
    store::CellPixelSize,
};

// Preferred raw tile target; an indivisible larger cell gets exact frame preflight.
const MAX_KITTY_TILE_RAW_BYTES: usize = 8 * 1024 * 1024;

// Tiny images are cheaper to send as raw RGBA than to compress on every render.
const MIN_KITTY_PNG_CANDIDATE_BYTES: usize = 256 * 1024;

/// Drop transparent pane-sized margins before encoding an outer placement.
/// Keeping the crop aligned to cells lets the cursor represent its origin
/// without introducing a separate pixel-offset protocol path.
pub(crate) fn crop_overlay_to_visible_cells(
    image: DecodedImage,
    cell: CellPixelSize,
) -> Option<(DecodedImage, usize, usize)> {
    let width = usize::try_from(image.width).ok()?;
    let height = usize::try_from(image.height).ok()?;
    if width == 0
        || height == 0
        || image.pixels.len() != width.checked_mul(height)?.checked_mul(4)?
    {
        return None;
    }
    let mut left = width;
    let mut top = height;
    let mut right = 0;
    let mut bottom = 0;
    for (index, pixel) in image.pixels.as_chunks::<4>().0.iter().enumerate() {
        if pixel[3] != 0 {
            let x = index % width;
            let y = index / width;
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    if left == width {
        return None;
    }
    let cell_width = usize::from(cell.width());
    let cell_height = usize::from(cell.height());
    left = left / cell_width * cell_width;
    top = top / cell_height * cell_height;
    right = right
        .div_ceil(cell_width)
        .saturating_mul(cell_width)
        .min(width);
    bottom = bottom
        .div_ceil(cell_height)
        .saturating_mul(cell_height)
        .min(height);
    let cropped_width = right - left;
    let cropped_height = bottom - top;
    if left == 0 && top == 0 && right == width && bottom == height {
        return Some((image, 0, 0));
    }
    let mut pixels = Vec::with_capacity(cropped_width * cropped_height * 4);
    for y in top..bottom {
        let start = (y * width + left) * 4;
        pixels.extend_from_slice(&image.pixels[start..start + cropped_width * 4]);
    }
    Some((
        DecodedImage {
            width: u32::try_from(cropped_width).ok()?,
            height: u32::try_from(cropped_height).ok()?,
            pixels,
        },
        left / cell_width,
        top / cell_height,
    ))
}

/// Split a large image into cell-aligned rectangles. Keeping complete cells
/// together avoids relying on Kitty's pixel-offset placement controls.
pub(crate) fn overlay_tile_size(
    image: &DecodedImage,
    cell: CellPixelSize,
) -> Option<(usize, usize)> {
    let cell_width = usize::from(cell.width());
    let cell_height = usize::from(cell.height());
    let cell_bytes = cell_width.checked_mul(cell_height)?.checked_mul(4)?;
    // A single physical cell can exceed the preferred tile target while its
    // actual encoded placement still fits a frame. The exact output preflight
    // below decides that case instead of silently dropping it here.
    let max_cells = (MAX_KITTY_TILE_RAW_BYTES / cell_bytes).max(1);
    let columns = usize::try_from(image.width).ok()?.div_ceil(cell_width);
    if columns == 0 {
        return None;
    }
    let tile_columns = columns.min(max_cells);
    let tile_rows = max_cells / tile_columns;
    Some((
        tile_columns.checked_mul(cell_width)?,
        tile_rows.checked_mul(cell_height)?,
    ))
}

pub(crate) fn overlay_tile(
    image: &DecodedImage,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Option<DecodedImage> {
    let source_width = usize::try_from(image.width).ok()?;
    let mut pixels = Vec::with_capacity(width.checked_mul(height)?.checked_mul(4)?);
    for row in y..y + height {
        let start = (row * source_width + x) * 4;
        pixels.extend_from_slice(&image.pixels[start..start + width * 4]);
    }
    Some(DecodedImage {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        pixels,
    })
}

pub(crate) enum OverlayPayload {
    Rgb,
    Rgba,
    Png(EncodedKittyPng),
}

pub(crate) fn prepare_overlay_payload(
    image: &DecodedImage,
    image_id: u32,
    z_index: i32,
) -> io::Result<(OverlayPayload, usize)> {
    let rgba_len = kitty_rgba_placement_len(image, image_id, z_index)?;
    let (raw_payload, raw_len) = match kitty_rgb_placement_len(image, image_id, z_index) {
        Ok(rgb_len) if rgb_len < rgba_len => (OverlayPayload::Rgb, rgb_len),
        _ => (OverlayPayload::Rgba, rgba_len),
    };
    if image.pixels.len() >= MIN_KITTY_PNG_CANDIDATE_BYTES
        && let Ok(png) = EncodedKittyPng::from_rgba(image)
        && let Ok(png_len) = png.placement_len(image_id, z_index)
        && png_len < raw_len
    {
        return Ok((OverlayPayload::Png(png), png_len));
    }
    Ok((raw_payload, raw_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::MAX_FRAME;

    #[test]
    fn outer_image_crop_preserves_cell_aligned_origin_and_pixels() {
        let cell = CellPixelSize::new(2, 2).unwrap();
        let mut pixels = vec![0; 7 * 5 * 4];
        pixels[(3 * 7 + 3) * 4..(3 * 7 + 3) * 4 + 4].copy_from_slice(&[4, 5, 6, 255]);
        let image = DecodedImage {
            width: 7,
            height: 5,
            pixels,
        };
        let (cropped, column, row) = crop_overlay_to_visible_cells(image, cell).unwrap();
        assert_eq!((column, row), (1, 1));
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(&cropped.pixels[12..16], &[4, 5, 6, 255]);
        assert_eq!(cropped.pixels.iter().filter(|&&byte| byte != 0).count(), 4);
        assert!(
            crop_overlay_to_visible_cells(
                DecodedImage {
                    width: 2,
                    height: 2,
                    pixels: vec![0; 16],
                },
                cell,
            )
            .is_none()
        );
    }

    #[test]
    fn outer_image_tile_accepts_one_large_cell_when_encoded_output_fits() {
        let cell = CellPixelSize::new(1600, 1600).unwrap();
        let image = DecodedImage {
            width: 1600,
            height: 1600,
            pixels: vec![255; 1600 * 1600 * 4],
        };
        assert!(image.pixels.len() > MAX_KITTY_TILE_RAW_BYTES);
        assert_eq!(overlay_tile_size(&image, cell), Some((1600, 1600)));
        let placement = kitty_rgba_placement_len(&image, 0x8000_0000, 0).unwrap();
        assert!(b"\x1b[3;2H".len() + placement + b"\x1b[4;3H".len() <= MAX_FRAME);

        let too_large = DecodedImage {
            width: 2000,
            height: 2000,
            pixels: vec![255; 2000 * 2000 * 4],
        };
        let large_cell = CellPixelSize::new(2000, 2000).unwrap();
        assert_eq!(
            overlay_tile_size(&too_large, large_cell),
            Some((2000, 2000))
        );
        assert!(kitty_rgba_placement_len(&too_large, 0x8000_0000, 0).unwrap() > MAX_FRAME);
        let (payload, encoded_len) = prepare_overlay_payload(&too_large, 0x8000_0000, 0).unwrap();
        assert!(matches!(payload, OverlayPayload::Png(_)));
        assert!(encoded_len < MAX_FRAME);
    }

    #[test]
    fn overlay_uses_png_only_when_it_reduces_wire_bytes() {
        let small = DecodedImage {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        };
        assert!(matches!(
            prepare_overlay_payload(&small, 7, 0).unwrap().0,
            OverlayPayload::Rgba
        ));

        let small_opaque = DecodedImage {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 255, 4, 5, 6, 255],
        };
        let (payload, rgb_len) = prepare_overlay_payload(&small_opaque, 7, 0).unwrap();
        assert!(matches!(payload, OverlayPayload::Rgb));
        assert_eq!(
            rgb_len,
            kitty_rgb_placement_len(&small_opaque, 7, 0).unwrap()
        );

        let flat = DecodedImage {
            width: 512,
            height: 512,
            pixels: vec![255; 512 * 512 * 4],
        };
        let (payload, png_len) = prepare_overlay_payload(&flat, 7, 0).unwrap();
        assert!(matches!(payload, OverlayPayload::Png(_)));
        assert!(png_len < kitty_rgba_placement_len(&flat, 7, 0).unwrap());

        let mut state = 0x1234_5678u32;
        let noise = DecodedImage {
            width: 512,
            height: 512,
            pixels: (0..512 * 512 * 4)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as u8
                })
                .collect(),
        };
        let (payload, wire_len) = prepare_overlay_payload(&noise, 7, 0).unwrap();
        assert!(matches!(payload, OverlayPayload::Rgba));
        assert_eq!(wire_len, kitty_rgba_placement_len(&noise, 7, 0).unwrap());
    }
}
