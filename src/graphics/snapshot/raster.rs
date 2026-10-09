//! Collect visible placement pixels before snapshot composition.
//! Ordinary and virtual placements share dimension validation and input budgets.

use super::{SnapshotError, placeholder_raster::collect_placeholder_clips};
use crate::{
    graphics::{
        composite::{CompositeError, MAX_COMPOSITE_INPUT_BYTES},
        decode::{
            ClippedPlacement, DecodeError, MAX_DECODED_IMAGE_BYTES, ResampledPlacement,
            StreamPngError, StreamZlibError, visible_placement_region,
        },
        geometry::{CellPixelSize, PixelSize, PlacementGeometry, PlacementPixelLayout},
        store::{ImageFormat, ImageStore},
    },
    screen::Screen,
};
use std::collections::BTreeMap;

pub(super) fn rasterize_placement(
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
    if matches!(image.format, ImageFormat::Rgb | ImageFormat::Rgba)
        && let Some((width, height)) = dimensions
    {
        let layout = pixel_layout(width, height, cell).ok_or(SnapshotError::InvalidLayout)?;
        let pixels = image
            .resample_raw_placement_region(layout, layout.destination)
            .map_err(SnapshotError::Resample)?;
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
pub(super) struct SnapshotState {
    dimensions: BTreeMap<u32, Option<(u32, u32)>>,
    pub(super) input_bytes: usize,
}

impl SnapshotState {
    pub(super) fn image_dimensions(
        &mut self,
        store: &ImageStore,
        image_id: u32,
    ) -> Option<(u32, u32)> {
        *self
            .dimensions
            .entry(image_id)
            .or_insert_with(|| store.validated_image_dimensions(image_id))
    }
}

pub(super) fn collect_visible_clips(
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
                            .resample_raw_placement_region(layout, region)
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
