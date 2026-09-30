//! Rasterize virtual placements referenced by Unicode placeholder cells.
//! Bounded and sparse source regions share snapshot validation and input budgets.

use super::{
    SnapshotError,
    raster::{SnapshotState, rasterize_placement},
};
use crate::{
    graphics::{
        composite::{CompositeError, MAX_COMPOSITE_INPUT_BYTES},
        decode::{
            ClippedPlacement, DecodeError, MAX_DECODED_IMAGE_BYTES, ResampledPlacement,
            StreamPngError, StreamZlibError,
        },
        geometry::{CellPixelSize, PixelRect, PixelSize},
        placeholder::decode_row,
        store::{ImageFormat, ImageStore},
    },
    screen::Screen,
};
use std::collections::BTreeMap;

pub(super) fn collect_placeholder_clips(
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
mod tests {
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
