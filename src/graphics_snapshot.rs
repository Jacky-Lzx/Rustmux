//! Explicit image-only snapshot of one pane's stored Kitty placements.
//! The terminal runtime uses all three stacking bands when Kitty support is known.

use std::collections::BTreeMap;

use crate::{
    graphics_composite::{
        CompositeError, ImageLayer, MAX_COMPOSITE_INPUT_BYTES, compose_image_layers,
    },
    graphics_decode::{
        ClipError, ClippedPlacement, DecodeError, DecodedImage, MAX_DECODED_IMAGE_BYTES,
        ResampleError, ResampledPlacement, StreamPngError,
    },
    graphics_placeholder::decode_row,
    graphics_store::{
        CellPixelSize, ImageFormat, ImageStore, PixelRect, PixelSize, PlacementPixelLayout,
    },
    screen::Screen,
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
    compose_snapshot(store, None, alternate, viewport, cell)
}

/// Include visible Unicode placeholder cells from the active pane screen.
pub(crate) fn compose_pane_snapshot(
    store: &ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<DecodedImage, SnapshotError> {
    compose_snapshot(store, Some(screen), screen.is_alternate(), viewport, cell)
}

fn compose_snapshot(
    store: &ImageStore,
    screen: Option<&Screen>,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<DecodedImage, SnapshotError> {
    validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, screen, alternate, viewport, cell, |_| true)?;
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
    compose_planes(store, None, alternate, viewport, cell)
}

pub(crate) fn compose_pane_planes(
    store: &ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<ImagePlanes, SnapshotError> {
    compose_planes(store, Some(screen), screen.is_alternate(), viewport, cell)
}

fn compose_planes(
    store: &ImageStore,
    screen: Option<&Screen>,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
) -> Result<ImagePlanes, SnapshotError> {
    let canvas_bytes = validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, screen, alternate, viewport, cell, |_| true)?;
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
    compose_band(store, None, alternate, viewport, cell, band)
}

pub(crate) fn compose_pane_band(
    store: &ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
    band: ImageBand,
) -> Result<Option<DecodedImage>, SnapshotError> {
    compose_band(
        store,
        Some(screen),
        screen.is_alternate(),
        viewport,
        cell,
        band,
    )
}

fn compose_band(
    store: &ImageStore,
    screen: Option<&Screen>,
    alternate: bool,
    viewport: PixelSize,
    cell: CellPixelSize,
    band: ImageBand,
) -> Result<Option<DecodedImage>, SnapshotError> {
    validate_viewport(viewport)?;
    let clipped = collect_visible_clips(store, screen, alternate, viewport, cell, |z| {
        band.contains(z)
    })?;
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

fn rasterize_placement(
    store: &ImageStore,
    image_id: u32,
    cell: CellPixelSize,
    pixel_layout: impl FnOnce(u32, u32, CellPixelSize) -> Option<PlacementPixelLayout>,
) -> Result<(PlacementPixelLayout, ResampledPlacement), SnapshotError> {
    let image = store.get(image_id).ok_or(SnapshotError::MissingImage)?;
    let dimensions = store.known_image_dimensions(image_id);
    if image.format == ImageFormat::Png
        && let Some((width, height)) = dimensions
        && u128::from(width) * u128::from(height) * 4 > MAX_DECODED_IMAGE_BYTES as u128
    {
        let layout = pixel_layout(width, height, cell).ok_or(SnapshotError::InvalidLayout)?;
        let pixels = image
            .resample_png_placement(layout)
            .map_err(|error| match error {
                StreamPngError::Decode(error) => SnapshotError::Decode(error),
                StreamPngError::Resample(error) => SnapshotError::Resample(error),
            })?;
        return Ok((layout, pixels));
    }
    let decoded = image.decode_rgba().map_err(SnapshotError::Decode)?;
    let layout =
        pixel_layout(decoded.width, decoded.height, cell).ok_or(SnapshotError::InvalidLayout)?;
    let pixels = decoded
        .resample_placement(layout)
        .map_err(SnapshotError::Resample)?;
    Ok((layout, pixels))
}

fn collect_visible_clips(
    store: &ImageStore,
    screen: Option<&Screen>,
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
        let (_, pixels) =
            rasterize_placement(store, placement.image_id, cell, |width, height, cell| {
                geometry.pixel_layout(width, height, cell)
            })?;
        if let Some(visible) = pixels
            .clip_to_viewport_with_scroll_clip(geometry, cell, viewport)
            .map_err(SnapshotError::Clip)?
        {
            input_bytes = input_bytes
                .checked_add(visible.pixels.len())
                .filter(|&total| total <= MAX_COMPOSITE_INPUT_BYTES)
                .ok_or(SnapshotError::Composite(CompositeError::InputLimit))?;
            clipped.push((
                store.protocol_image_id(placement.image_id),
                geometry.z_index,
                visible,
            ));
        }
    }
    if let Some(screen) = screen {
        collect_placeholder_clips(
            store,
            screen,
            viewport,
            cell,
            &include_z,
            &mut clipped,
            &mut input_bytes,
        )?;
    }
    Ok(clipped)
}

fn collect_placeholder_clips(
    store: &ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
    include_z: &impl Fn(i32) -> bool,
    clipped: &mut Vec<(u32, i32, ClippedPlacement)>,
    input_bytes: &mut usize,
) -> Result<(), SnapshotError> {
    let (rows, columns) = screen.dimensions();
    if (rows as u128) * u128::from(cell.height()) != u128::from(viewport.height)
        || (columns as u128) * u128::from(cell.width()) != u128::from(viewport.width)
    {
        return Err(SnapshotError::InvalidViewport);
    }
    let virtuals: Vec<_> = store
        .placements()
        .filter(|placement| placement.virtual_layout.is_some())
        .collect();
    if virtuals.is_empty() {
        return Ok(());
    }
    let mut by_identity = BTreeMap::new();
    for (index, placement) in virtuals.iter().enumerate() {
        let image_id = store.protocol_image_id(placement.image_id);
        if image_id == 0 {
            continue;
        }
        by_identity
            .entry((image_id, placement.placement_id))
            .or_insert(index);
        by_identity.entry((image_id, None)).or_insert(index);
    }
    let mut extents = vec![None; virtuals.len()];
    let mut rasters: Vec<Option<ResampledPlacement>> = (0..virtuals.len()).map(|_| None).collect();
    let mut raster_bytes = 0usize;
    for row in 0..rows {
        for (column, reference) in decode_row(screen.row(row).unwrap()).into_iter().enumerate() {
            let Some(reference) = reference else {
                continue;
            };
            let Some(&index) = by_identity.get(&(reference.image_id, reference.placement_id))
            else {
                continue;
            };
            let placement = virtuals[index];
            let layout = placement.virtual_layout.unwrap();
            if !include_z(layout.z_index)
                || !layout.may_contain_cell(reference.row, reference.column)
            {
                continue;
            }
            if extents[index].is_none()
                && let Some((width, height)) = store.known_image_dimensions(placement.image_id)
            {
                let pixel_layout = layout
                    .pixel_layout(width, height, cell)
                    .ok_or(SnapshotError::InvalidLayout)?;
                extents[index] = Some((
                    pixel_layout.cell_bounds.width / u32::from(cell.width()),
                    pixel_layout.cell_bounds.height / u32::from(cell.height()),
                ));
            }
            if extents[index]
                .is_some_and(|(columns, rows)| reference.row >= rows || reference.column >= columns)
            {
                continue;
            }
            if rasters[index].is_none() {
                let (pixel_layout, raster) =
                    rasterize_placement(store, placement.image_id, cell, |width, height, cell| {
                        layout.pixel_layout(width, height, cell)
                    })?;
                let columns = pixel_layout.cell_bounds.width / u32::from(cell.width());
                let rows = pixel_layout.cell_bounds.height / u32::from(cell.height());
                extents[index] = Some((columns, rows));
                if reference.row >= rows || reference.column >= columns {
                    continue;
                }
                raster_bytes = raster_bytes
                    .checked_add(raster.pixels.len())
                    .filter(|&total| total <= MAX_COMPOSITE_INPUT_BYTES)
                    .ok_or(SnapshotError::Composite(CompositeError::InputLimit))?;
                rasters[index] = Some(raster);
            }
            if let Some(tile) = clip_virtual_cell(
                rasters[index].as_ref().unwrap(),
                reference.row,
                reference.column,
                row,
                column,
                cell,
            )? {
                *input_bytes = input_bytes
                    .checked_add(tile.pixels.len())
                    .filter(|&total| total <= MAX_COMPOSITE_INPUT_BYTES)
                    .ok_or(SnapshotError::Composite(CompositeError::InputLimit))?;
                clipped.push((reference.image_id, layout.z_index, tile));
            }
        }
    }
    Ok(())
}

fn clip_virtual_cell(
    raster: &ResampledPlacement,
    source_row: u32,
    source_column: u32,
    screen_row: usize,
    screen_column: usize,
    cell: CellPixelSize,
) -> Result<Option<ClippedPlacement>, SnapshotError> {
    let invalid = || SnapshotError::InvalidLayout;
    let cell_width = u32::from(cell.width());
    let cell_height = u32::from(cell.height());
    let source_left = source_column.checked_mul(cell_width).ok_or_else(invalid)?;
    let source_top = source_row.checked_mul(cell_height).ok_or_else(invalid)?;
    let content = raster.destination;
    let left = source_left.max(content.x);
    let top = source_top.max(content.y);
    let right = source_left
        .checked_add(cell_width)
        .ok_or_else(invalid)?
        .min(content.x.checked_add(content.width).ok_or_else(invalid)?);
    let bottom = source_top
        .checked_add(cell_height)
        .ok_or_else(invalid)?
        .min(content.y.checked_add(content.height).ok_or_else(invalid)?);
    if left >= right || top >= bottom {
        return Ok(None);
    }
    let width = right - left;
    let height = bottom - top;
    let x = u32::try_from(screen_column)
        .ok()
        .and_then(|column| column.checked_mul(cell_width))
        .and_then(|x| x.checked_add(left - source_left))
        .ok_or_else(invalid)?;
    let y = u32::try_from(screen_row)
        .ok()
        .and_then(|row| row.checked_mul(cell_height))
        .and_then(|y| y.checked_add(top - source_top))
        .ok_or_else(invalid)?;
    let source_width = usize::try_from(content.width).map_err(|_| invalid())?;
    let copy_width = usize::try_from(width).map_err(|_| invalid())?;
    let start_x = usize::try_from(left - content.x).map_err(|_| invalid())?;
    let start_y = usize::try_from(top - content.y).map_err(|_| invalid())?;
    let mut pixels = Vec::with_capacity(
        copy_width
            .checked_mul(usize::try_from(height).map_err(|_| invalid())?)
            .and_then(|count| count.checked_mul(4))
            .ok_or_else(invalid)?,
    );
    for offset_y in 0..usize::try_from(height).map_err(|_| invalid())? {
        let start = (start_y + offset_y)
            .checked_mul(source_width)
            .and_then(|index| index.checked_add(start_x))
            .and_then(|index| index.checked_mul(4))
            .ok_or_else(invalid)?;
        let end = start.checked_add(copy_width * 4).ok_or_else(invalid)?;
        pixels.extend_from_slice(raster.pixels.get(start..end).ok_or_else(invalid)?);
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

#[cfg(test)]
mod placeholder_tests {
    use super::*;

    #[test]
    fn each_placeholder_clips_only_its_letterboxed_source_cell() {
        let raster = ResampledPlacement {
            destination: PixelRect {
                x: 1,
                y: 0,
                width: 2,
                height: 2,
            },
            pixels: vec![1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255],
        };
        let cell = CellPixelSize::new(2, 2).unwrap();
        let left = clip_virtual_cell(&raster, 0, 0, 1, 0, cell)
            .unwrap()
            .unwrap();
        assert_eq!(
            left.destination,
            PixelRect {
                x: 1,
                y: 2,
                width: 1,
                height: 2,
            }
        );
        assert_eq!(left.pixels, [1, 0, 0, 255, 3, 0, 0, 255]);

        let right = clip_virtual_cell(&raster, 0, 1, 2, 3, cell)
            .unwrap()
            .unwrap();
        assert_eq!(
            right.destination,
            PixelRect {
                x: 6,
                y: 4,
                width: 1,
                height: 2,
            }
        );
        assert_eq!(right.pixels, [2, 0, 0, 255, 4, 0, 0, 255]);
        assert!(
            clip_virtual_cell(&raster, 1, 0, 0, 0, cell)
                .unwrap()
                .is_none()
        );
    }
}
