//! Recognize source images eligible for direct outer-terminal placement.
//! Complete placeholder rectangles bypass rasterization; partial rectangles
//! let the caller wait for the child to finish painting before composition.

use super::ImageBand;
use crate::{
    graphics::{
        geometry::{CellPixelSize, PixelRect, PixelSize, PlacementGeometry, PlacementSizing},
        placeholder::decode_row,
        store::{ImageFormat, ImageStore},
    },
    screen::Screen,
};

/// A sole RGB/RGBA/PNG placement fully contained in the pane, or a complete
/// virtual placeholder rectangle. Both can reuse native outer image data.
pub(crate) struct SourceImagePlacement<'a> {
    pub generation: u64,
    /// Regular crop/fit controls; None retains virtual-placeholder sizing.
    pub geometry: Option<PlacementGeometry>,
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
    let (screen_rows, screen_columns) = screen.dimensions();
    if u128::try_from(screen_columns).ok()? * u128::from(cell.width()) != u128::from(viewport.width)
        || u128::try_from(screen_rows).ok()? * u128::from(cell.height())
            != u128::from(viewport.height)
    {
        return None;
    }
    let mut regulars = store.placements().filter(|placement| {
        placement.geometry.is_some_and(|geometry| {
            geometry.anchor.alternate == screen.is_alternate() && band.contains(geometry.z_index)
        })
    });
    if let Some(placement) = regulars.next() {
        // Compositing is still required for overlapping/multiple placements or
        // pane/scroll-margin clipping. Native placement cannot clip to a pane.
        if regulars.next().is_some()
            || store
                .placements()
                .any(|p| p.virtual_layout.is_some_and(|v| band.contains(v.z_index)))
        {
            return None;
        }
        let geometry = placement.geometry?;
        if geometry.clip_top_rows != 0 || geometry.clip_bottom_rows != 0 {
            return None;
        }
        let image = store.get(placement.image_id)?;
        let format = match image.format {
            ImageFormat::Rgb => 24,
            ImageFormat::Rgba => 32,
            ImageFormat::Png => 100,
            _ => return None,
        };
        let (width, height) = store.validated_image_dimensions(placement.image_id)?;
        let layout = geometry.pixel_layout(width, height, cell)?;
        let anchor = geometry.pixel_anchor(cell)?;
        let right = anchor
            .x
            .checked_add(i64::from(layout.destination.x))?
            .checked_add(i64::from(layout.destination.width))?;
        let bottom = anchor
            .y
            .checked_add(i64::from(layout.destination.y))?
            .checked_add(i64::from(layout.destination.height))?;
        if anchor.x < 0
            || anchor.y < 0
            || right > i64::from(viewport.width)
            || bottom > i64::from(viewport.height)
        {
            return None;
        }
        return Some(SourceImageProgress::Complete(SourceImagePlacement {
            generation: store.image_generation(placement.image_id)?,
            geometry: Some(geometry),
            data: &image.data,
            format,
            width,
            height,
            columns: layout.cell_bounds.width / u32::from(cell.width()),
            rows: layout.cell_bounds.height / u32::from(cell.height()),
            column: geometry.anchor.column,
            row: usize::try_from(anchor.y / i64::from(cell.height())).ok()?,
        }));
    }
    if band != ImageBand::AboveText {
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
        generation: store.image_generation(placement.image_id)?,
        geometry: None,
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

impl SourceImagePlacement<'_> {
    pub(crate) fn placement_controls(&self) -> String {
        let Some(geometry) = self.geometry else {
            return if self.format == 100 {
                String::new()
            } else {
                format!(",c={},r={}", self.columns, self.rows)
            };
        };
        let source = geometry.source;
        let mut controls = format!(",x={},y={}", source.left, source.top);
        if let Some(width) = source.width {
            controls.push_str(&format!(",w={width}"));
        }
        if let Some(height) = source.height {
            controls.push_str(&format!(",h={height}"));
        }
        if matches!(
            geometry.sizing,
            PlacementSizing::FitWidth | PlacementSizing::FitBox
        ) {
            controls.push_str(&format!(
                ",c={}",
                geometry.columns.expect("validated fit width")
            ));
        }
        if matches!(
            geometry.sizing,
            PlacementSizing::FitHeight | PlacementSizing::FitBox
        ) {
            controls.push_str(&format!(
                ",r={}",
                geometry.rows.expect("validated fit height")
            ));
        }
        if geometry.cell_offset.x != 0 {
            controls.push_str(&format!(",X={}", geometry.cell_offset.x));
        }
        if geometry.cell_offset.y != 0 {
            controls.push_str(&format!(",Y={}", geometry.cell_offset.y));
        }
        controls
    }
}
