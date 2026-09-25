//! Explicit image-only snapshot of one pane's stored Kitty placements.
//! The terminal runtime uses all three stacking bands when Kitty support is known.

use crate::{
    graphics_composite::{
        CompositeError, ImageLayer, MAX_COMPOSITE_INPUT_BYTES, compose_image_layers,
    },
    graphics_decode::{
        ClipError, ClippedPlacement, DecodeError, DecodedImage, MAX_DECODED_IMAGE_BYTES,
        ResampleError,
    },
    graphics_store::{CellPixelSize, ImageStore, PixelSize},
};

/// Kitty's special boundary below which images are also behind cells with a
/// non-default background. At the boundary they remain above those colors.
pub const BACKGROUND_Z_BOUNDARY: i32 = i32::MIN / 2;
pub const MAX_PLANE_CANVAS_BYTES: usize = 64 * 1024 * 1024;

/// One Kitty text/background stacking band. Each band is composited before
/// transmission so source-image z and ID ordering remains pane-local.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ImageBand {
    BehindBackground,
    BehindText,
    AboveText,
}

impl ImageBand {
    pub const ALL: [Self; 3] = [Self::BehindBackground, Self::BehindText, Self::AboveText];

    pub fn output_z(self) -> i32 {
        match self {
            Self::BehindBackground => i32::MIN,
            Self::BehindText => -1,
            Self::AboveText => 0,
        }
    }

    fn contains(self, z: i32) -> bool {
        match self {
            Self::BehindBackground => z < BACKGROUND_Z_BOUNDARY,
            Self::BehindText => (BACKGROUND_Z_BOUNDARY..0).contains(&z),
            Self::AboveText => z >= 0,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ImagePlanes {
    /// `z < BACKGROUND_Z_BOUNDARY`: behind non-default cell backgrounds.
    pub behind_background: Option<DecodedImage>,
    /// `BACKGROUND_Z_BOUNDARY <= z < 0`: behind glyphs but above backgrounds.
    pub behind_text: Option<DecodedImage>,
    /// `z >= 0`: above text.
    pub above_text: Option<DecodedImage>,
}

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
    validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, alternate, viewport, cell, |_| true)?;
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

/// Preserve the three Kitty image/text stacking bands without combining them
/// with cell colors or glyphs. An absent band allocates no canvas. Up to two
/// full-size canvases fit the aggregate output limit; a third is rejected.
pub fn compose_store_planes(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<ImagePlanes, SnapshotError> {
    let canvas_bytes = validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, alternate, viewport, cell, |_| true)?;
    let mut behind_background = Vec::new();
    let mut behind_text = Vec::new();
    let mut above_text = Vec::new();
    for (image_id, z_index, placement) in &clipped {
        let layer = ImageLayer {
            image_id: *image_id,
            z_index: *z_index,
            placement,
        };
        if *z_index < BACKGROUND_Z_BOUNDARY {
            behind_background.push(layer);
        } else if *z_index < 0 {
            behind_text.push(layer);
        } else {
            above_text.push(layer);
        }
    }
    let populated = usize::from(!behind_background.is_empty())
        + usize::from(!behind_text.is_empty())
        + usize::from(!above_text.is_empty());
    if canvas_bytes * populated > MAX_PLANE_CANVAS_BYTES {
        return Err(SnapshotError::OutputLimit);
    }
    Ok(ImagePlanes {
        behind_background: compose_nonempty(viewport, &behind_background)?,
        behind_text: compose_nonempty(viewport, &behind_text)?,
        above_text: compose_nonempty(viewport, &above_text)?,
    })
}

/// Compose one band without decoding or allocating the other two. An invalid
/// placement in a different band does not suppress this band's output.
pub fn compose_store_band(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
    band: ImageBand,
) -> Result<Option<DecodedImage>, SnapshotError> {
    validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, alternate, viewport, cell, |z| band.contains(z))?;
    let layers: Vec<_> = clipped
        .iter()
        .map(|(image_id, z_index, placement)| ImageLayer {
            image_id: *image_id,
            z_index: *z_index,
            placement,
        })
        .collect();
    compose_nonempty(viewport, &layers)
}

/// Compatibility helper for callers interested only in images above text.
pub fn compose_store_above_text(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<Option<DecodedImage>, SnapshotError> {
    compose_store_band(store, alternate, viewport, cell, ImageBand::AboveText)
}

fn compose_nonempty(
    viewport: PixelSize,
    layers: &[ImageLayer<'_>],
) -> Result<Option<DecodedImage>, SnapshotError> {
    if layers.is_empty() {
        return Ok(None);
    }
    compose_image_layers(viewport, layers)
        .map(Some)
        .map_err(SnapshotError::Composite)
}

fn validate_viewport(viewport: PixelSize) -> Result<usize, SnapshotError> {
    if viewport.width == 0 || viewport.height == 0 {
        return Err(SnapshotError::InvalidViewport);
    }
    let canvas_bytes = u128::from(viewport.width) * u128::from(viewport.height) * 4;
    if canvas_bytes > MAX_DECODED_IMAGE_BYTES as u128 {
        return Err(SnapshotError::OutputLimit);
    }
    Ok(usize::try_from(canvas_bytes).unwrap())
}

fn collect_visible_clips(
    store: &ImageStore,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
    include_z: impl Fn(i32) -> bool,
) -> Result<Vec<(u32, i32, ClippedPlacement)>, SnapshotError> {
    let mut clipped = Vec::new();
    let mut input_bytes = 0usize;
    for placement in store.placements() {
        let Some(geometry) = placement.geometry else {
            continue;
        };
        if geometry.anchor.alternate != alternate || !include_z(geometry.z_index) {
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
    Ok(clipped)
}
