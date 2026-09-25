//! Explicit image-only snapshot of one pane's stored Kitty placements.
//! The normal terminal renderer does not call this path.

use crate::{
    graphics_composite::{
        CompositeError, ImageLayer, MAX_COMPOSITE_INPUT_BYTES, compose_image_layers,
    },
    graphics_decode::{
        ClipError, DecodeError, DecodedImage, MAX_DECODED_IMAGE_BYTES, ResampleError,
    },
    graphics_store::{CellPixelSize, ImageStore, PixelSize},
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SnapshotError {
    InvalidViewport,
    OutputLimit,
    MissingImage,
    InvalidLayout,
    Decode(DecodeError),
    Resample(ResampleError),
    Clip(ClipError),
    Composite(CompositeError),
}

/// Decode, size, clip, and blend cursor-anchored placements on the selected
/// screen. Unanchored placements and references on the other screen are not
/// visible. An invalid visible image fails the snapshot rather than producing
/// a silently incomplete frame. No image data or placement state is mutated.
pub fn compose_store_snapshot(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<DecodedImage, SnapshotError> {
    if viewport.width == 0 || viewport.height == 0 {
        return Err(SnapshotError::InvalidViewport);
    }
    let canvas_bytes = u128::from(viewport.width) * u128::from(viewport.height) * 4;
    if canvas_bytes > MAX_DECODED_IMAGE_BYTES as u128 {
        return Err(SnapshotError::OutputLimit);
    }

    let mut clipped = Vec::new();
    let mut input_bytes = 0usize;
    for placement in store.placements() {
        let Some(geometry) = placement.geometry else {
            continue;
        };
        if geometry.anchor.alternate != alternate {
            continue;
        }
        let image = store
            .get(placement.image_id)
            .ok_or(SnapshotError::MissingImage)?
            .decode_rgba()
            .map_err(SnapshotError::Decode)?;
        let layout = geometry
            .pixel_layout(image.width, image.height, cell)
            .ok_or(SnapshotError::InvalidLayout)?;
        let pixels = image
            .resample_placement(layout)
            .map_err(SnapshotError::Resample)?;
        if let Some(visible) = pixels
            .clip_to_viewport_with_scroll_clip(geometry, cell, viewport)
            .map_err(SnapshotError::Clip)?
        {
            input_bytes = input_bytes
                .checked_add(visible.pixels.len())
                .filter(|&total| total <= MAX_COMPOSITE_INPUT_BYTES)
                .ok_or(SnapshotError::Composite(CompositeError::InputLimit))?;
            clipped.push((placement.image_id, geometry.z_index, visible));
        }
    }
    let layers: Vec<_> = clipped
        .iter()
        .map(|(image_id, z_index, placement)| ImageLayer {
            image_id: *image_id,
            z_index: *z_index,
            placement,
        })
        .collect();
    compose_image_layers(viewport, &layers).map_err(SnapshotError::Composite)
}
