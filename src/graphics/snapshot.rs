//! Explicit image-only snapshot of one pane's stored Kitty placements.
//! The terminal runtime uses all three stacking bands when Kitty support is known.

use std::collections::BTreeMap;

use super::geometry::{
    CellPixelSize, PixelRect, PixelSize, PlacementGeometry, PlacementPixelLayout,
};
use crate::{
    graphics_composite::{
        CompositeError, ImageLayer, MAX_COMPOSITE_INPUT_BYTES, compose_image_layers,
    },
    graphics_decode::{
        ClipError, ClippedPlacement, DecodeError, DecodedImage, MAX_DECODED_IMAGE_BYTES,
        ResampleError, ResampledPlacement, StreamPngError, StreamZlibError,
        visible_placement_region,
    },
    graphics_placeholder::decode_row,
    graphics_store::{ImageFormat, ImageStore},
    screen::Screen,
};

mod source_placement;

#[cfg(test)]
pub(crate) use source_placement::source_image_pane_band;
pub(crate) use source_placement::{
    SourceImagePlacement, SourceImageProgress, source_image_pane_band_progress,
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
    dimensions: Option<(u32, u32)>,
    cell: CellPixelSize,
    pixel_layout: impl FnOnce(u32, u32, CellPixelSize) -> Option<PlacementPixelLayout>,
) -> Result<(PlacementPixelLayout, ResampledPlacement), SnapshotError> {
    let image = store.get(image_id).ok_or(SnapshotError::MissingImage)?;
    if matches!(image.format, ImageFormat::RgbZlib | ImageFormat::RgbaZlib)
        && let Some((width, height)) = dimensions
    {
        let layout = pixel_layout(width, height, cell).ok_or(SnapshotError::InvalidLayout)?;
        let pixels = image
            .resample_zlib_placement(layout)
            .map_err(|error| match error {
                StreamZlibError::Decode(error) => SnapshotError::Decode(error),
                StreamZlibError::Resample(error) => SnapshotError::Resample(error),
            })?;
        return Ok((layout, pixels));
    }
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

fn rasterize_visible_placement(
    store: &ImageStore,
    image_id: u32,
    dimensions: Option<(u32, u32)>,
    cell: CellPixelSize,
    geometry: PlacementGeometry,
    viewport: PixelSize,
) -> Result<Option<ClippedPlacement>, SnapshotError> {
    let (_, pixels) =
        rasterize_placement(store, image_id, dimensions, cell, |width, height, cell| {
            geometry.pixel_layout(width, height, cell)
        })?;
    pixels
        .clip_to_viewport_with_scroll_clip(geometry, cell, viewport)
        .map_err(SnapshotError::Clip)
}

#[derive(Default)]
struct SnapshotState {
    dimensions: BTreeMap<u32, Option<(u32, u32)>>,
    input_bytes: usize,
}

impl SnapshotState {
    fn image_dimensions(&mut self, store: &ImageStore, image_id: u32) -> Option<(u32, u32)> {
        *self
            .dimensions
            .entry(image_id)
            .or_insert_with(|| store.validated_image_dimensions(image_id))
    }
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
    let mut state = SnapshotState::default();
    for placement in store.placements() {
        let Some(geometry) = placement.geometry else {
            continue;
        };
        if geometry.anchor.alternate != alternate || !include_z(geometry.z_index) {
            continue;
        }
        let image = store
            .get(placement.image_id)
            .ok_or(SnapshotError::MissingImage)?;
        let visible_layout = if let Some((width, height)) =
            state.image_dimensions(store, placement.image_id)
        {
            let layout = geometry
                .pixel_layout(width, height, cell)
                .ok_or(SnapshotError::InvalidLayout)?;
            let oversized_png = image.format == ImageFormat::Png
                && (u128::from(width) * u128::from(height) * 4 > MAX_DECODED_IMAGE_BYTES as u128
                    || u128::from(layout.destination.width)
                        * u128::from(layout.destination.height)
                        * 4
                        > MAX_DECODED_IMAGE_BYTES as u128);
            Some((layout, oversized_png))
        } else {
            None
        };
        let visible = if let Some((layout, oversized_png)) = visible_layout {
            if let Some((region, destination)) =
                visible_placement_region(layout.destination, geometry, cell, viewport)
                    .map_err(SnapshotError::Clip)?
            {
                match image.format {
                    ImageFormat::Rgb | ImageFormat::Rgba => {
                        let pixels = image
                            .decode_rgba()
                            .map_err(SnapshotError::Decode)?
                            .resample_placement_region(layout, region)
                            .map_err(SnapshotError::Resample)?;
                        Some(ClippedPlacement {
                            destination,
                            pixels: pixels.pixels,
                        })
                    }
                    ImageFormat::RgbZlib | ImageFormat::RgbaZlib => {
                        let pixels = image
                            .resample_zlib_placement_region(layout, region)
                            .map_err(|error| match error {
                                StreamZlibError::Decode(error) => SnapshotError::Decode(error),
                                StreamZlibError::Resample(error) => SnapshotError::Resample(error),
                            })?;
                        Some(ClippedPlacement {
                            destination,
                            pixels: pixels.pixels,
                        })
                    }
                    ImageFormat::Png => match image.resample_png_placement_region(layout, region) {
                        Ok(pixels) => Some(ClippedPlacement {
                            destination,
                            pixels: pixels.pixels,
                        }),
                        Err(StreamPngError::Decode(DecodeError::UnsupportedPng))
                            if !oversized_png =>
                        {
                            // Adam7 needs the bounded full-frame decoder.
                            rasterize_visible_placement(
                                store,
                                placement.image_id,
                                state.image_dimensions(store, placement.image_id),
                                cell,
                                geometry,
                                viewport,
                            )?
                        }
                        Err(StreamPngError::Decode(error)) => {
                            return Err(SnapshotError::Decode(error));
                        }
                        Err(StreamPngError::Resample(error)) => {
                            return Err(SnapshotError::Resample(error));
                        }
                    },
                }
            } else {
                None
            }
        } else {
            rasterize_visible_placement(
                store,
                placement.image_id,
                state.image_dimensions(store, placement.image_id),
                cell,
                geometry,
                viewport,
            )?
        };
        if let Some(visible) = visible {
            state.input_bytes = state
                .input_bytes
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
            &mut state,
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
    state: &mut SnapshotState,
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
    // Find the source-backed pixels referenced by this screen before sampling
    // PNG or compressed raw data, even when the complete raster is small.
    let needs_bounded_raster = virtuals.iter().any(|placement| {
        store.get(placement.image_id).is_some_and(|image| {
            matches!(
                image.format,
                ImageFormat::RgbZlib | ImageFormat::RgbaZlib | ImageFormat::Png
            )
        })
    });
    let mut known_layouts = vec![None; virtuals.len()];
    let mut requested_regions: Vec<Option<PixelRect>> = vec![None; virtuals.len()];
    let mut sparse_regions: Vec<BTreeMap<(u32, u32), PixelRect>> =
        (0..virtuals.len()).map(|_| BTreeMap::new()).collect();
    if needs_bounded_raster {
        for row in 0..rows {
            for reference in decode_row(screen.row(row).unwrap()).into_iter().flatten() {
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
                if known_layouts[index].is_none()
                    && let Some((width, height)) = state.image_dimensions(store, placement.image_id)
                {
                    known_layouts[index] = Some(
                        layout
                            .pixel_layout(width, height, cell)
                            .ok_or(SnapshotError::InvalidLayout)?,
                    );
                }
                let Some(pixel_layout) = known_layouts[index] else {
                    continue;
                };
                let columns = pixel_layout.cell_bounds.width / u32::from(cell.width());
                let rows = pixel_layout.cell_bounds.height / u32::from(cell.height());
                if reference.row >= rows || reference.column >= columns {
                    continue;
                }
                if let Some(region) = virtual_cell_region(
                    pixel_layout.destination,
                    reference.row,
                    reference.column,
                    cell,
                )? {
                    if store.get(placement.image_id).is_some_and(|image| {
                        matches!(
                            image.format,
                            ImageFormat::RgbZlib | ImageFormat::RgbaZlib | ImageFormat::Png
                        )
                    }) {
                        sparse_regions[index]
                            .entry((reference.row, reference.column))
                            .or_insert(region);
                    }
                    requested_regions[index] = Some(match requested_regions[index] {
                        Some(previous) => union_region(previous, region),
                        None => region,
                    });
                }
            }
        }
    }
    let mut extents = vec![None; virtuals.len()];
    let mut rasters: Vec<Option<ResampledPlacement>> = (0..virtuals.len()).map(|_| None).collect();
    let mut sparse_rasters: Vec<Option<BTreeMap<(u32, u32), ResampledPlacement>>> =
        (0..virtuals.len()).map(|_| None).collect();
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
                && let Some((width, height)) = state.image_dimensions(store, placement.image_id)
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
            if known_layouts[index].is_some() && requested_regions[index].is_none() {
                continue;
            }
            if rasters[index].is_none() && sparse_rasters[index].is_none() {
                let image = store
                    .get(placement.image_id)
                    .ok_or(SnapshotError::MissingImage)?;
                let sparse = matches!(
                    image.format,
                    ImageFormat::RgbZlib | ImageFormat::RgbaZlib | ImageFormat::Png
                ) && requested_regions[index].is_some_and(|bounds| {
                    u128::from(bounds.width) * u128::from(bounds.height) * 4
                        > MAX_DECODED_IMAGE_BYTES as u128
                });
                if sparse {
                    let pixel_layout = known_layouts[index].ok_or(SnapshotError::InvalidLayout)?;
                    let regions: Vec<_> = sparse_regions[index].values().copied().collect();
                    let sampled = match image.format {
                        ImageFormat::RgbZlib | ImageFormat::RgbaZlib => image
                            .resample_zlib_placement_regions(pixel_layout, &regions)
                            .map_err(|error| match error {
                                StreamZlibError::Decode(error) => SnapshotError::Decode(error),
                                StreamZlibError::Resample(error) => SnapshotError::Resample(error),
                            })?,
                        ImageFormat::Png => image
                            .resample_png_placement_regions(pixel_layout, &regions)
                            .map_err(|error| match error {
                                StreamPngError::Decode(error) => SnapshotError::Decode(error),
                                StreamPngError::Resample(error) => SnapshotError::Resample(error),
                            })?,
                        ImageFormat::Rgb | ImageFormat::Rgba => unreachable!(),
                    };
                    let sampled_bytes: usize = sampled.iter().map(|tile| tile.pixels.len()).sum();
                    raster_bytes = raster_bytes
                        .checked_add(sampled_bytes)
                        .filter(|&total| total <= MAX_COMPOSITE_INPUT_BYTES)
                        .ok_or(SnapshotError::Composite(CompositeError::InputLimit))?;
                    sparse_rasters[index] =
                        Some(sparse_regions[index].keys().copied().zip(sampled).collect());
                } else {
                    let streamed = if matches!(
                        image.format,
                        ImageFormat::RgbZlib | ImageFormat::RgbaZlib | ImageFormat::Png
                    ) {
                        known_layouts[index].zip(requested_regions[index])
                    } else {
                        None
                    };
                    let (pixel_layout, raster) = if let Some((pixel_layout, region)) = streamed {
                        let raster = match image.format {
                            ImageFormat::RgbZlib | ImageFormat::RgbaZlib => image
                                .resample_zlib_placement_region(pixel_layout, region)
                                .map_err(|error| match error {
                                    StreamZlibError::Decode(error) => SnapshotError::Decode(error),
                                    StreamZlibError::Resample(error) => {
                                        SnapshotError::Resample(error)
                                    }
                                })?,
                            ImageFormat::Png => {
                                match image.resample_png_placement_region(pixel_layout, region) {
                                    Ok(raster) => raster,
                                    Err(StreamPngError::Decode(DecodeError::UnsupportedPng))
                                        if state
                                            .image_dimensions(store, placement.image_id)
                                            .is_some_and(|(width, height)| {
                                                u128::from(width) * u128::from(height) * 4
                                                    <= MAX_DECODED_IMAGE_BYTES as u128
                                            })
                                            && u128::from(pixel_layout.destination.width)
                                                * u128::from(pixel_layout.destination.height)
                                                * 4
                                                <= MAX_DECODED_IMAGE_BYTES as u128 =>
                                    {
                                        // Adam7 retains the bounded full-frame path.
                                        rasterize_placement(
                                            store,
                                            placement.image_id,
                                            state.image_dimensions(store, placement.image_id),
                                            cell,
                                            |width, height, cell| {
                                                layout.pixel_layout(width, height, cell)
                                            },
                                        )?
                                        .1
                                    }
                                    Err(StreamPngError::Decode(error)) => {
                                        return Err(SnapshotError::Decode(error));
                                    }
                                    Err(StreamPngError::Resample(error)) => {
                                        return Err(SnapshotError::Resample(error));
                                    }
                                }
                            }
                            ImageFormat::Rgb | ImageFormat::Rgba => unreachable!(),
                        };
                        (pixel_layout, raster)
                    } else {
                        rasterize_placement(
                            store,
                            placement.image_id,
                            state.image_dimensions(store, placement.image_id),
                            cell,
                            |width, height, cell| layout.pixel_layout(width, height, cell),
                        )?
                    };
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
            }
            let raster = if let Some(sparse) = &sparse_rasters[index] {
                sparse.get(&(reference.row, reference.column))
            } else {
                rasters[index].as_ref()
            };
            let Some(raster) = raster else {
                continue;
            };
            if let Some(tile) =
                clip_virtual_cell(raster, reference.row, reference.column, row, column, cell)?
            {
                state.input_bytes = state
                    .input_bytes
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
    let Some(region) = virtual_cell_region(content, source_row, source_column, cell)? else {
        return Ok(None);
    };
    let (left, top, width, height) = (region.x, region.y, region.width, region.height);
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

fn virtual_cell_region(
    content: PixelRect,
    source_row: u32,
    source_column: u32,
    cell: CellPixelSize,
) -> Result<Option<PixelRect>, SnapshotError> {
    let invalid = || SnapshotError::InvalidLayout;
    let cell_width = u32::from(cell.width());
    let cell_height = u32::from(cell.height());
    let left = source_column.checked_mul(cell_width).ok_or_else(invalid)?;
    let top = source_row.checked_mul(cell_height).ok_or_else(invalid)?;
    let right = left.checked_add(cell_width).ok_or_else(invalid)?;
    let bottom = top.checked_add(cell_height).ok_or_else(invalid)?;
    let content_right = content.x.checked_add(content.width).ok_or_else(invalid)?;
    let content_bottom = content.y.checked_add(content.height).ok_or_else(invalid)?;
    let left = left.max(content.x);
    let top = top.max(content.y);
    let right = right.min(content_right);
    let bottom = bottom.min(content_bottom);
    if left >= right || top >= bottom {
        return Ok(None);
    }
    Ok(Some(PixelRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    }))
}

fn union_region(left: PixelRect, right: PixelRect) -> PixelRect {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    PixelRect {
        x,
        y,
        width: (left.x + left.width).max(right.x + right.width) - x,
        height: (left.y + left.height).max(right.y + right.height) - y,
    }
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
        let bounded = ResampledPlacement {
            destination: PixelRect {
                x: 2,
                y: 0,
                width: 1,
                height: 2,
            },
            pixels: vec![2, 0, 0, 255, 4, 0, 0, 255],
        };
        assert_eq!(
            clip_virtual_cell(&bounded, 0, 1, 2, 3, cell)
                .unwrap()
                .unwrap(),
            right
        );
        assert!(
            clip_virtual_cell(&bounded, 0, 0, 1, 0, cell)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            union_region(
                virtual_cell_region(raster.destination, 0, 0, cell)
                    .unwrap()
                    .unwrap(),
                virtual_cell_region(raster.destination, 0, 1, cell)
                    .unwrap()
                    .unwrap(),
            ),
            raster.destination
        );
    }
}

#[cfg(test)]
mod deferred_snapshot_tests {
    use super::*;
    use crate::{graphics_store::CellAnchor, graphics_transfer::DirectTransferAssembler};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    #[test]
    fn small_png_samples_only_visible_source_crop() {
        let pixels: Vec<u8> = vec![
            1, 2, 3, 255, 11, 12, 13, 255, 21, 22, 23, 255, 31, 32, 33, 255, 4, 5, 6, 255, 14, 15,
            16, 255, 24, 25, 26, 255, 34, 35, 36, 255,
        ];
        let expected = vec![
            11, 12, 13, 255, 21, 22, 23, 255, 14, 15, 16, 255, 24, 25, 26, 255,
        ];
        let mut data = Vec::new();
        let mut encoder = png::Encoder::new(&mut data, 4, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
        let command = format!(
            "\x1b_Ga=T,f=100,i=7,x=1,y=0,w=3,h=2,c=3,r=2;{}\x1b\\",
            STANDARD.encode(data)
        );
        let transfer = DirectTransferAssembler::new()
            .accept(command.as_bytes())
            .unwrap();
        let mut store = ImageStore::new();
        store.insert_at(transfer, CellAnchor::default()).unwrap();
        let snapshot = compose_store_snapshot(
            &store,
            false,
            PixelSize {
                width: 2,
                height: 2,
            },
            CellPixelSize::new(1, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot.pixels, expected);
    }

    #[test]
    fn small_interlaced_png_retains_full_decode_fallback() {
        let mut info = png::Info::with_size(1, 1);
        info.color_type = png::ColorType::Rgba;
        info.bit_depth = png::BitDepth::Eight;
        info.interlaced = true;
        let mut data = Vec::new();
        png::Encoder::with_info(&mut data, info)
            .unwrap()
            .write_header()
            .unwrap()
            .write_image_data(&[11, 12, 13, 255])
            .unwrap();
        let command = format!("\x1b_Ga=T,f=100,i=7;{}\x1b\\", STANDARD.encode(data));
        let transfer = DirectTransferAssembler::new()
            .accept(command.as_bytes())
            .unwrap();
        let mut store = ImageStore::new();
        store.insert_at(transfer, CellAnchor::default()).unwrap();
        let snapshot = compose_store_snapshot(
            &store,
            false,
            PixelSize {
                width: 1,
                height: 1,
            },
            CellPixelSize::new(1, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot.pixels, [11, 12, 13, 255]);
    }

    #[test]
    fn deferred_large_png_and_zlib_raw_render_without_sized_upload() {
        let width = 3072;
        let height = 3072;
        let pixels = [17, 23, 31, 255].repeat(width * height);
        for format in [ImageFormat::Png, ImageFormat::RgbaZlib] {
            let (controls, data) = if format == ImageFormat::Png {
                let mut data = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut data, width as u32, height as u32);
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    let mut writer = encoder.write_header().unwrap();
                    writer.write_image_data(&pixels).unwrap();
                    writer.finish().unwrap();
                }
                (format!("f=100,s={width},v={height}"), data)
            } else {
                let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&pixels).unwrap();
                (
                    format!("f=32,o=z,s={width},v={height}"),
                    encoder.finish().unwrap(),
                )
            };
            let upload = |data: &[u8]| {
                let command = format!(
                    "\x1b_Ga=T,i=7,c=2,r=2,{controls};{}\x1b\\",
                    STANDARD.encode(data)
                );
                DirectTransferAssembler::new()
                    .accept(command.as_bytes())
                    .unwrap()
            };
            let mut store = ImageStore::new();
            store
                .insert_at(upload(&data), CellAnchor::default())
                .unwrap();
            assert_eq!(store.known_image_dimensions(7), None);
            let viewport = PixelSize {
                width: 2,
                height: 2,
            };
            let cell = CellPixelSize::new(1, 1).unwrap();
            let snapshot = compose_store_snapshot(&store, false, viewport, cell).unwrap();
            assert_eq!(snapshot.pixels, [17, 23, 31, 255].repeat(4));
            // Read-only composition does not change the store's validation cache.
            assert_eq!(store.known_image_dimensions(7), None);

            let mut corrupt = data;
            *corrupt.last_mut().unwrap() ^= 1;
            store
                .insert_at(upload(&corrupt), CellAnchor::default())
                .unwrap();
            assert!(matches!(
                compose_store_snapshot(&store, false, viewport, cell),
                Err(SnapshotError::Decode(_))
            ));
        }
    }
}
