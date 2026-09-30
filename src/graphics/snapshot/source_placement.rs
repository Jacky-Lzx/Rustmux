//! Recognize source images eligible for direct outer-terminal placement.
//! Complete placeholder rectangles bypass rasterization; partial rectangles
//! let the caller wait for the child to finish painting before composition.

use super::ImageBand;
use crate::{
    graphics::{
        geometry::{CellPixelSize, PixelRect, PixelSize, PlacementSizing},
        placeholder::decode_row,
        store::{ImageFormat, ImageStore},
    },
    screen::Screen,
};

/// A complete RGB/RGBA/PNG virtual placement whose placeholder cells form one
/// contiguous rectangle. The outer terminal can fit the stored source image
/// into that rectangle without a pane-sized RGBA canvas.
pub(crate) struct SourceImagePlacement<'a> {
    pub data: &'a [u8],
    pub format: u8,
    pub width: u32,
    pub height: u32,
    pub columns: u32,
    pub rows: u32,
    pub column: usize,
    pub row: usize,
}

pub(crate) enum SourceImageProgress<'a> {
    Complete(SourceImagePlacement<'a>),
    Incomplete,
}

#[cfg(test)]
pub(crate) fn source_image_pane_band<'a>(
    store: &'a ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
    band: ImageBand,
) -> Option<SourceImagePlacement<'a>> {
    match source_image_pane_band_progress(store, screen, viewport, cell, band)? {
        SourceImageProgress::Complete(placement) => Some(placement),
        SourceImageProgress::Incomplete => None,
    }
}

pub(crate) fn source_image_pane_band_progress<'a>(
    store: &'a ImageStore,
    screen: &Screen,
    viewport: PixelSize,
    cell: CellPixelSize,
    band: ImageBand,
) -> Option<SourceImageProgress<'a>> {
    if band != ImageBand::AboveText {
        return None;
    }
    let (screen_rows, screen_columns) = screen.dimensions();
    if u128::try_from(screen_columns).ok()? * u128::from(cell.width()) != u128::from(viewport.width)
        || u128::try_from(screen_rows).ok()? * u128::from(cell.height())
            != u128::from(viewport.height)
        || store.placements().any(|placement| {
            placement.geometry.is_some_and(|geometry| {
                geometry.anchor.alternate == screen.is_alternate()
                    && band.contains(geometry.z_index)
            })
        })
    {
        return None;
    }
    // The regular snapshot resolves duplicate virtual identities in insertion
    // order. Require a sole prototype so the direct path cannot choose a
    // different image for the same placeholder cells.
    let mut virtuals = store
        .placements()
        .filter(|placement| placement.virtual_layout.is_some());
    let placement = virtuals.next()?;
    if virtuals.next().is_some() {
        return None;
    }
    let layout = placement.virtual_layout?;
    if !band.contains(layout.z_index) {
        return None;
    }
    let image = store.get(placement.image_id)?;
    let (format, bytes_per_pixel): (u8, usize) = match image.format {
        ImageFormat::Rgb => (24, 3),
        ImageFormat::Rgba => (32, 4),
        ImageFormat::Png => (100, 0),
        _ => return None,
    };
    if layout.sizing != PlacementSizing::Natural
        || layout.cell_offset.x != 0
        || layout.cell_offset.y != 0
    {
        return None;
    }
    let (width, height) = store.validated_image_dimensions(placement.image_id)?;
    if format != 100
        && image.data.len()
            != usize::try_from(width)
                .ok()?
                .checked_mul(usize::try_from(height).ok()?)?
                .checked_mul(bytes_per_pixel)?
    {
        return None;
    }
    let pixel_layout = layout.pixel_layout(width, height, cell)?;
    if pixel_layout.source
        != (PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        })
    {
        return None;
    }
    let columns = usize::try_from(pixel_layout.cell_bounds.width / u32::from(cell.width())).ok()?;
    let rows = usize::try_from(pixel_layout.cell_bounds.height / u32::from(cell.height())).ok()?;
    let expected = columns.checked_mul(rows)?;
    let image_id = store.protocol_image_id(placement.image_id);
    let mut origin = None;
    let mut seen = 0usize;
    for row in 0..screen_rows {
        for (column, reference) in decode_row(screen.row(row)?).into_iter().enumerate() {
            let Some(reference) = reference else { continue };
            if reference.image_id != image_id {
                continue;
            }
            if reference.placement_id.is_some() && reference.placement_id != placement.placement_id
            {
                return None;
            }
            let source_row = usize::try_from(reference.row).ok()?;
            let source_column = usize::try_from(reference.column).ok()?;
            if source_row >= rows || source_column >= columns {
                return None;
            }
            let candidate = (
                column.checked_sub(source_column)?,
                row.checked_sub(source_row)?,
            );
            if origin.is_some_and(|origin| origin != candidate) {
                return None;
            }
            origin = Some(candidate);
            seen = seen.checked_add(1)?;
        }
    }
    let (column, row) = origin?;
    if column.checked_add(columns)? > screen_columns || row.checked_add(rows)? > screen_rows {
        return None;
    }
    if seen > expected {
        return None;
    }
    if seen < expected {
        return Some(SourceImageProgress::Incomplete);
    }
    Some(SourceImageProgress::Complete(SourceImagePlacement {
        data: &image.data,
        format,
        width,
        height,
        columns: u32::try_from(columns).ok()?,
        rows: u32::try_from(rows).ok()?,
        column,
        row,
    }))
}
