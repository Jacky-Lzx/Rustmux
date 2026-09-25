//! Bounded image data and placement references for one pane. Optional cell
//! anchors are recorded, but no pixels are rendered here.

use crate::{
    graphics::MAX_GRAPHICS_COMMAND_BYTES, graphics_transfer::AssembledDirectTransfer,
    screen::ScrollEvent,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MAX_PANE_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PANE_IMAGES: usize = 256;
pub const MAX_PANE_PLACEMENTS: usize = 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ImageFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Eq, PartialEq)]
pub struct StoredImage {
    pub format: ImageFormat,
    pub data: Vec<u8>,
    pub(crate) declared_width: Option<u32>,
    pub(crate) declared_height: Option<u32>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StoreError {
    UnsupportedIdentity,
    UnsupportedAction,
    InvalidPlacement,
    InvalidData,
    MissingImage,
    TooLarge,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Placement {
    pub image_id: u32,
    /// None is an anonymous placement (the protocol's absent or zero `p`).
    pub placement_id: Option<u32>,
    /// Only pane-local, cursor-anchored placements have geometry so far.
    pub geometry: Option<PlacementGeometry>,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct CellAnchor {
    pub row: usize,
    pub column: usize,
    pub alternate: bool,
}

#[derive(Clone, Copy)]
struct CellRegion {
    top: i128,
    bottom: i128,
    left: u128,
    right: u128,
}

impl CellRegion {
    fn screen(rows: usize, columns: usize) -> Self {
        Self {
            top: 0,
            bottom: rows as i128,
            left: 0,
            right: columns as u128,
        }
    }

    fn cell(row: i128, column: u128) -> Self {
        Self {
            top: row,
            bottom: row + 1,
            left: column,
            right: column + 1,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PlacementGeometry {
    pub anchor: CellAnchor,
    /// Signed displacement from the original cursor anchor as text rows move.
    pub row_offset: i64,
    /// Pixel rectangle selected from the source image before cell sizing.
    pub source: SourceRect,
    /// Pixel offset within the first cell, independent of the cell extent.
    pub cell_offset: CellPixelOffset,
    /// Explicit cell extent, when requested. Missing values need pixel-cell
    /// geometry before a renderer can infer them.
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    /// Whether the original command requested scaling, before missing extents
    /// are inferred from the image and physical cell dimensions.
    pub sizing: PlacementSizing,
    /// Permanently clipped placement cell rows after crossing a scroll margin.
    pub clip_top_rows: u32,
    pub clip_bottom_rows: u32,
    pub z_index: i32,
    pub cursor_stays: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PlacementSizing {
    Natural,
    FitWidth,
    FitHeight,
    FitBox,
}

/// Pixel geometry relative to the placement's anchor cell. This does not
/// include clipping against the pane or scroll margins.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PlacementPixelLayout {
    pub source: PixelRect,
    /// The `c` by `r` cell rectangle, excluding the first-cell `X/Y` offset.
    pub cell_bounds: PixelSize,
    /// The image content after scaling and any letterbox/pillarbox padding.
    pub destination: PixelRect,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SignedPixelPoint {
    pub x: i64,
    pub y: i64,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct SourceRect {
    pub left: u32,
    pub top: u32,
    /// None selects the remaining source width from `left`.
    pub width: Option<u32>,
    /// None selects the remaining source height from `top`.
    pub height: Option<u32>,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct CellPixelOffset {
    pub x: u32,
    pub y: u32,
}

impl SourceRect {
    fn intersected_dimensions(self, image_width: u32, image_height: u32) -> Option<(u32, u32)> {
        let width = image_width
            .saturating_sub(self.left)
            .min(self.width.unwrap_or(u32::MAX));
        let height = image_height
            .saturating_sub(self.top)
            .min(self.height.unwrap_or(u32::MAX));
        (width != 0 && height != 0).then_some((width, height))
    }
}

impl PlacementGeometry {
    /// Locate the original anchor cell within a pane after tracked row shifts.
    /// Negative y positions are valid for placements entering scrollback.
    pub fn pixel_anchor(self, cell: CellPixelSize) -> Option<SignedPixelPoint> {
        let column = i128::try_from(self.anchor.column).ok()?;
        let row = i128::try_from(self.anchor.row).ok()? + i128::from(self.row_offset);
        Some(SignedPixelPoint {
            x: i64::try_from(column.checked_mul(i128::from(cell.width))?).ok()?,
            y: i64::try_from(row.checked_mul(i128::from(cell.height))?).ok()?,
        })
    }

    /// Resolve an opt-in placement's pixel dimensions without decoding or
    /// drawing pixels. Recompute inferred extents for this cell size rather
    /// than trusting extents inferred for a previous terminal size.
    pub fn pixel_layout(
        self,
        image_width: u32,
        image_height: u32,
        cell: CellPixelSize,
    ) -> Option<PlacementPixelLayout> {
        if self.cell_offset.x >= u32::from(cell.width)
            || self.cell_offset.y >= u32::from(cell.height)
        {
            return None;
        }
        let (source_width, source_height) = self
            .source
            .intersected_dimensions(image_width, image_height)?;
        let explicit = match self.sizing {
            PlacementSizing::Natural => (None, None),
            PlacementSizing::FitWidth => (Some(self.columns?), None),
            PlacementSizing::FitHeight => (None, Some(self.rows?)),
            PlacementSizing::FitBox => (Some(self.columns?), Some(self.rows?)),
        };
        let (columns, rows) =
            infer_cell_extent(source_width, source_height, explicit.0, explicit.1, cell)?;
        let cell_bounds = PixelSize {
            width: columns.checked_mul(u32::from(cell.width))?,
            height: rows.checked_mul(u32::from(cell.height))?,
        };
        let (width, height) = match self.sizing {
            PlacementSizing::Natural => (source_width, source_height),
            PlacementSizing::FitWidth => (
                cell_bounds.width,
                round_scaled(source_height, cell_bounds.width, source_width)?,
            ),
            PlacementSizing::FitHeight => (
                round_scaled(source_width, cell_bounds.height, source_height)?,
                cell_bounds.height,
            ),
            PlacementSizing::FitBox => {
                if u128::from(cell_bounds.width) * u128::from(source_height)
                    <= u128::from(cell_bounds.height) * u128::from(source_width)
                {
                    (
                        cell_bounds.width,
                        round_scaled(source_height, cell_bounds.width, source_width)?
                            .min(cell_bounds.height),
                    )
                } else {
                    (
                        round_scaled(source_width, cell_bounds.height, source_height)?
                            .min(cell_bounds.width),
                        cell_bounds.height,
                    )
                }
            }
        };
        let padding_x = if self.sizing == PlacementSizing::FitBox {
            (cell_bounds.width - width) / 2
        } else {
            0
        };
        let padding_y = if self.sizing == PlacementSizing::FitBox {
            (cell_bounds.height - height) / 2
        } else {
            0
        };
        let x = self.cell_offset.x.checked_add(padding_x)?;
        let y = self.cell_offset.y.checked_add(padding_y)?;
        x.checked_add(width)?;
        y.checked_add(height)?;
        Some(PlacementPixelLayout {
            source: PixelRect {
                x: self.source.left,
                y: self.source.top,
                width: source_width,
                height: source_height,
            },
            cell_bounds,
            destination: PixelRect {
                x,
                y,
                width,
                height,
            },
        })
    }
}

fn round_scaled(source_other: u32, target: u32, source_axis: u32) -> Option<u32> {
    let numerator = u128::from(source_other) * u128::from(target);
    let rounded = (numerator + u128::from(source_axis / 2)) / u128::from(source_axis);
    u32::try_from(rounded.max(1)).ok()
}

/// Caller-supplied physical size of one terminal cell. The runtime propagates
/// this through detached-session attachment and resize messages when exact.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CellPixelSize {
    width: u16,
    height: u16,
}

impl CellPixelSize {
    pub fn new(width: u16, height: u16) -> Option<Self> {
        (width != 0 && height != 0).then_some(Self { width, height })
    }

    pub fn width(self) -> u16 {
        self.width
    }

    pub fn height(self) -> u16 {
        self.height
    }

    /// Derive a cell size only when the reported terminal pixel dimensions
    /// cover an exact grid. Zero or leftover pixels may represent unknown
    /// geometry or terminal padding, so they must not be guessed away.
    pub fn from_terminal_size(
        rows: u16,
        columns: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Option<Self> {
        if rows == 0
            || columns == 0
            || pixel_width == 0
            || pixel_height == 0
            || !pixel_width.is_multiple_of(columns)
            || !pixel_height.is_multiple_of(rows)
        {
            return None;
        }
        Self::new(pixel_width / columns, pixel_height / rows)
    }
}

#[derive(Debug, Default)]
pub struct ImageStore {
    images: BTreeMap<u32, StoredImage>,
    decoded_dimensions: BTreeMap<u32, Option<(u32, u32)>>,
    oldest: VecDeque<u32>,
    placements: VecDeque<Placement>,
    total_bytes: usize,
    revision: u64,
}

impl ImageStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: u32) -> Option<&StoredImage> {
        self.images.get(&id)
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Changes whenever retained image data or placement state changes.
    /// Multiple internal mutations in one command may advance it more than
    /// once. Equality is only meaningful while the same store instance lives.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn placements(&self) -> impl Iterator<Item = &Placement> {
        self.placements.iter()
    }

    /// Clear visible, cursor-anchored references on one screen without
    /// discarding their image data. Unanchored virtual/relative references are
    /// not classified as visible until their layout is modeled.
    pub fn clear_screen_placements(&mut self, alternate: bool) {
        let before = self.placements.len();
        self.placements.retain(|placement| {
            placement
                .geometry
                .is_none_or(|geometry| geometry.anchor.alternate != alternate)
        });
        if self.placements.len() != before {
            self.changed();
        }
    }

    /// Follow a physical text-row shift. Explicit-height placements can be
    /// clipped at margins; unknown-height placements only move when their
    /// anchor is in a full-screen shift, so their extent is never guessed.
    pub(crate) fn scroll_placements(&mut self, event: ScrollEvent, history_len: usize) {
        if !self.placements.iter().any(|placement| {
            placement
                .geometry
                .is_some_and(|geometry| geometry.anchor.alternate == event.alternate)
        }) {
            return;
        }
        let before = self.placements.clone();
        let top = event.top as i64;
        let bottom = event.bottom as i64 + 1;
        let lines = event.lines as i64;
        self.placements.retain_mut(|placement| {
            let Some(geometry) = placement.geometry.as_mut() else {
                return true;
            };
            if geometry.anchor.alternate != event.alternate {
                return true;
            }
            let start = (geometry.anchor.row as i64).saturating_add(geometry.row_offset);
            let visible_top = start.saturating_add(i64::from(geometry.clip_top_rows));
            let Some(rows) = geometry.rows else {
                if (event.full_screen && (top..bottom).contains(&start))
                    || (event.archive && start < top)
                {
                    geometry.row_offset =
                        geometry
                            .row_offset
                            .saturating_add(if event.down { lines } else { -lines });
                }
                // The source height is unknown; do not discard a placement
                // that might still overlap the screen or retained history.
                return true;
            };
            let visible_bottom = start
                .saturating_add(i64::from(rows))
                .saturating_sub(i64::from(geometry.clip_bottom_rows));
            let within_region = visible_top >= top && visible_bottom <= bottom;
            if !within_region && !(event.archive && visible_bottom <= top) {
                return true;
            }
            geometry.row_offset =
                geometry
                    .row_offset
                    .saturating_add(if event.down { lines } else { -lines });
            let shifted_start = (geometry.anchor.row as i64).saturating_add(geometry.row_offset);
            if event.archive {
                return shifted_start
                    .saturating_add(i64::from(rows))
                    .saturating_sub(i64::from(geometry.clip_bottom_rows))
                    > -(history_len as i64);
            }
            geometry.clip_top_rows = geometry
                .clip_top_rows
                .max(top.saturating_sub(shifted_start).clamp(0, i64::from(rows)) as u32);
            geometry.clip_bottom_rows = geometry.clip_bottom_rows.max(
                shifted_start
                    .saturating_add(i64::from(rows))
                    .saturating_sub(bottom)
                    .clamp(0, i64::from(rows)) as u32,
            );
            geometry
                .clip_top_rows
                .saturating_add(geometry.clip_bottom_rows)
                < rows
        });
        if self.placements != before {
            self.changed();
        }
    }

    /// Retain a completed transfer with an explicit nonzero image ID. Query
    /// commands and image-number allocation require a later protocol stage.
    /// Replacement is atomic if the new transfer cannot fit by itself.
    pub fn insert(&mut self, transfer: AssembledDirectTransfer) -> Result<u32, StoreError> {
        self.insert_inner(transfer, None, None, false)
            .map(|(id, _)| id)
    }

    /// Pane path: snapshot the cursor at the final chunk of `a=T`.
    pub fn insert_at(
        &mut self,
        transfer: AssembledDirectTransfer,
        anchor: CellAnchor,
    ) -> Result<u32, StoreError> {
        self.insert_inner(transfer, Some(anchor), None, false)
            .map(|(id, _)| id)
    }

    /// The running terminal prevalidates PNG before mutating the store; public
    /// opt-in pane APIs keep their deferred-decoding behavior.
    pub(crate) fn insert_for_pane(
        &mut self,
        transfer: AssembledDirectTransfer,
        anchor: CellAnchor,
        cell_pixels: Option<CellPixelSize>,
        validate_png: bool,
    ) -> Result<Option<PlacementGeometry>, StoreError> {
        let (id, geometry) =
            self.insert_inner(transfer, Some(anchor), cell_pixels, validate_png)?;
        Ok(self.resolve_latest_geometry(id, geometry, cell_pixels))
    }

    fn insert_inner(
        &mut self,
        transfer: AssembledDirectTransfer,
        anchor: Option<CellAnchor>,
        cell_pixels: Option<CellPixelSize>,
        validate_png: bool,
    ) -> Result<(u32, Option<PlacementGeometry>), StoreError> {
        let display = match transfer.control(b'a') {
            None | Some(b"t") => false,
            Some(b"T") => true,
            _ => return Err(StoreError::UnsupportedAction),
        };
        let placement_id = if display {
            parse_optional_placement_id(transfer.control(b'p'))?
        } else {
            None
        };
        let geometry = if display
            && transfer.control(b'U') != Some(b"1")
            && transfer.control(b'P').is_none()
            && transfer.control(b'Q').is_none()
        {
            let parsed = parse_geometry(
                anchor.unwrap_or_default(),
                |key| transfer.control(key),
                cell_pixels,
            )?;
            anchor.map(|_| parsed)
        } else {
            None
        };
        if transfer.control(b'I').is_some() {
            return Err(StoreError::UnsupportedIdentity);
        }
        let id = transfer
            .control(b'i')
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|&id| id != 0)
            .ok_or(StoreError::UnsupportedIdentity)?;
        let format = match transfer.control(b'f') {
            None | Some(b"32") => ImageFormat::Rgba,
            Some(b"24") => ImageFormat::Rgb,
            Some(b"100") => ImageFormat::Png,
            _ => return Err(StoreError::UnsupportedAction),
        };
        let size = transfer.data.len();
        let declared_width = transfer.control(b's').and_then(parse_positive_u32);
        let declared_height = transfer.control(b'v').and_then(parse_positive_u32);
        if size > MAX_PANE_IMAGE_BYTES {
            return Err(StoreError::TooLarge);
        }
        let image = StoredImage {
            format,
            data: transfer.data,
            declared_width,
            declared_height,
        };
        // The runtime must not replace a displayable image with corrupt or
        // unsupported PNG bytes. Raw lengths were checked by the assembler.
        // Cache successful dimensions so sized placement does not decode twice.
        let decoded_dimensions = if validate_png && format == ImageFormat::Png {
            let decoded = image.decode_rgba().map_err(|_| StoreError::InvalidData)?;
            Some((decoded.width, decoded.height))
        } else {
            None
        };
        self.remove(id);
        while self.images.len() >= MAX_PANE_IMAGES || self.total_bytes + size > MAX_PANE_IMAGE_BYTES
        {
            let oldest = *self.oldest.front().expect("nonempty image store");
            self.remove(oldest);
        }
        self.total_bytes += size;
        self.oldest.push_back(id);
        self.images.insert(id, image);
        if let Some(dimensions) = decoded_dimensions {
            self.decoded_dimensions.insert(id, Some(dimensions));
        }
        self.changed();
        if display {
            self.place_with_geometry(id, placement_id, geometry)?;
        }
        Ok((id, geometry))
    }

    /// Record only an explicit-ID placement reference without an anchor.
    /// This low-level method has no screen to move; acknowledgements are absent.
    pub fn place(&mut self, image_id: u32, placement_id: Option<u32>) -> Result<(), StoreError> {
        self.place_with_geometry(image_id, placement_id, None)
    }

    fn place_with_geometry(
        &mut self,
        image_id: u32,
        placement_id: Option<u32>,
        geometry: Option<PlacementGeometry>,
    ) -> Result<(), StoreError> {
        if placement_id == Some(0) {
            return Err(StoreError::InvalidPlacement);
        }
        if !self.images.contains_key(&image_id) {
            return Err(StoreError::MissingImage);
        }
        if let Some(placement_id) = placement_id {
            self.placements.retain(|placement| {
                placement.image_id != image_id || placement.placement_id != Some(placement_id)
            });
        }
        if self.placements.len() >= MAX_PANE_PLACEMENTS {
            self.placements.pop_front();
        }
        self.placements.push_back(Placement {
            image_id,
            placement_id,
            geometry,
        });
        self.changed();
        Ok(())
    }

    /// A strict, deliberately small APC G control-command subset for
    /// placement and deletion. Screen-dependent selectors require a pane.
    pub fn accept_control(&mut self, command: &[u8]) -> Result<(), StoreError> {
        self.accept_control_inner(command, None, None, None)
            .map(|_| ())
    }

    /// Pane path: snapshot the cursor when an `a=p` command arrives.
    pub fn accept_control_at(
        &mut self,
        command: &[u8],
        anchor: CellAnchor,
    ) -> Result<(), StoreError> {
        self.accept_control_inner(command, Some(anchor), None, None)
            .map(|_| ())
    }

    pub(crate) fn accept_control_for_pane(
        &mut self,
        command: &[u8],
        anchor: CellAnchor,
        cell_pixels: Option<CellPixelSize>,
        viewport: (usize, usize),
    ) -> Result<Option<PlacementGeometry>, StoreError> {
        let geometry =
            self.accept_control_inner(command, Some(anchor), cell_pixels, Some(viewport))?;
        let id = self.placements.back().map(|placement| placement.image_id);
        Ok(id.and_then(|id| self.resolve_latest_geometry(id, geometry, cell_pixels)))
    }

    fn resolve_latest_geometry(
        &mut self,
        image_id: u32,
        geometry: Option<PlacementGeometry>,
        cell_pixels: Option<CellPixelSize>,
    ) -> Option<PlacementGeometry> {
        let mut geometry = geometry?;
        let Some(cell_pixels) = cell_pixels else {
            return Some(geometry);
        };
        if geometry.columns.is_some() && geometry.rows.is_some() {
            return Some(geometry);
        }
        let dimensions = if let Some(&cached) = self.decoded_dimensions.get(&image_id) {
            cached
        } else {
            let decoded = self
                .images
                .get(&image_id)
                .and_then(|image| image.decode_rgba().ok())
                .map(|image| (image.width, image.height));
            self.decoded_dimensions.insert(image_id, decoded);
            decoded
        };
        let Some((width, height)) = dimensions
            .and_then(|(width, height)| geometry.source.intersected_dimensions(width, height))
        else {
            return Some(geometry);
        };
        let Some((columns, rows)) =
            infer_cell_extent(width, height, geometry.columns, geometry.rows, cell_pixels)
        else {
            return Some(geometry);
        };
        geometry.columns = Some(columns);
        geometry.rows = Some(rows);
        if let Some(last) = self.placements.back_mut() {
            last.geometry = Some(geometry);
        }
        Some(geometry)
    }

    fn accept_control_inner(
        &mut self,
        command: &[u8],
        anchor: Option<CellAnchor>,
        cell_pixels: Option<CellPixelSize>,
        viewport: Option<(usize, usize)>,
    ) -> Result<Option<PlacementGeometry>, StoreError> {
        let controls = parse_control_command(command).ok_or(StoreError::UnsupportedAction)?;
        match controls.get(&b'a').map(Vec::as_slice) {
            Some(b"p") => {
                if !only_keys(&controls, b"aipqcrzCxywhXY") {
                    return Err(StoreError::UnsupportedAction);
                }
                let id = required_id(&controls)?;
                let placement_id =
                    parse_optional_placement_id(controls.get(&b'p').map(Vec::as_slice))?;
                let parsed = parse_geometry(
                    anchor.unwrap_or_default(),
                    |key| controls.get(&key).map(Vec::as_slice),
                    cell_pixels,
                )?;
                let geometry = anchor.map(|_| parsed);
                self.place_with_geometry(id, placement_id, geometry)?;
                Ok(geometry)
            }
            Some(b"d") => {
                match controls.get(&b'd').map(Vec::as_slice) {
                    None | Some(b"a" | b"A") => {
                        if !only_keys(&controls, b"adq") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let (Some(anchor), Some((rows, columns))) = (anchor, viewport) else {
                            return Err(StoreError::UnsupportedAction);
                        };
                        self.delete_intersecting_placements(
                            anchor.alternate,
                            CellRegion::screen(rows, columns),
                            None,
                            controls.get(&b'd').is_some_and(|value| value == b"A"),
                        );
                    }
                    Some(b"c" | b"C") => {
                        if !only_keys(&controls, b"adq") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let Some(anchor) = anchor else {
                            return Err(StoreError::UnsupportedAction);
                        };
                        let row = anchor.row as i128;
                        let column = anchor.column as u128;
                        self.delete_intersecting_placements(
                            anchor.alternate,
                            CellRegion::cell(row, column),
                            None,
                            controls.get(&b'd').is_some_and(|value| value == b"C"),
                        );
                    }
                    Some(b"p" | b"P") => {
                        if !only_keys(&controls, b"adqxy") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let (Some(anchor), Some((rows, columns))) = (anchor, viewport) else {
                            return Err(StoreError::UnsupportedAction);
                        };
                        let (row, column) = required_delete_cell(&controls, rows, columns)?;
                        self.delete_intersecting_placements(
                            anchor.alternate,
                            CellRegion::cell(row, column),
                            None,
                            controls.get(&b'd').is_some_and(|value| value == b"P"),
                        );
                    }
                    Some(b"q" | b"Q") => {
                        if !only_keys(&controls, b"adqxyz") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let (Some(anchor), Some((rows, columns))) = (anchor, viewport) else {
                            return Err(StoreError::UnsupportedAction);
                        };
                        let (row, column) = required_delete_cell(&controls, rows, columns)?;
                        let z_index = required_delete_z_index(&controls)?;
                        self.delete_intersecting_placements(
                            anchor.alternate,
                            CellRegion::cell(row, column),
                            Some(z_index),
                            controls.get(&b'd').is_some_and(|value| value == b"Q"),
                        );
                    }
                    Some(b"x" | b"X") => {
                        if !only_keys(&controls, b"adqx") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let (Some(anchor), Some((_, columns))) = (anchor, viewport) else {
                            return Err(StoreError::UnsupportedAction);
                        };
                        let column = required_delete_coordinate(&controls, b'x', columns)?;
                        self.delete_column_placements(
                            anchor.alternate,
                            column as u128,
                            controls.get(&b'd').is_some_and(|value| value == b"X"),
                        );
                    }
                    Some(b"y" | b"Y") => {
                        if !only_keys(&controls, b"adqy") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let (Some(anchor), Some((rows, _))) = (anchor, viewport) else {
                            return Err(StoreError::UnsupportedAction);
                        };
                        let row = required_delete_coordinate(&controls, b'y', rows)?;
                        self.delete_row_placements(
                            anchor.alternate,
                            row as i128,
                            controls.get(&b'd').is_some_and(|value| value == b"Y"),
                        );
                    }
                    Some(b"z" | b"Z") => {
                        if !only_keys(&controls, b"adqz") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let Some(anchor) = anchor else {
                            return Err(StoreError::UnsupportedAction);
                        };
                        let z_index = required_delete_z_index(&controls)?;
                        self.delete_z_placements(
                            anchor.alternate,
                            z_index,
                            controls.get(&b'd').is_some_and(|value| value == b"Z"),
                        );
                    }
                    Some(b"i" | b"I") => {
                        if !only_keys(&controls, b"adipq") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let id = required_id(&controls)?;
                        let placement_id =
                            parse_optional_placement_id(controls.get(&b'p').map(Vec::as_slice))?;
                        self.delete_placements(
                            id,
                            placement_id,
                            controls.get(&b'd').is_some_and(|value| value == b"I"),
                        );
                    }
                    Some(b"r" | b"R") => {
                        if !only_keys(&controls, b"adqxy") {
                            return Err(StoreError::UnsupportedAction);
                        }
                        let first = required_delete_image_bound(&controls, b'x')?;
                        let last = required_delete_image_bound(&controls, b'y')?;
                        self.delete_image_range(
                            first,
                            last,
                            controls.get(&b'd').is_some_and(|value| value == b"R"),
                        );
                    }
                    _ => return Err(StoreError::UnsupportedAction),
                }
                Ok(None)
            }
            _ => Err(StoreError::UnsupportedAction),
        }
    }

    /// Delete placements whose modeled cell rectangle intersects the selected
    /// screen region. Hard deletion frees only now-unreferenced images.
    fn delete_intersecting_placements(
        &mut self,
        alternate: bool,
        region: CellRegion,
        z_filter: Option<i32>,
        free_data: bool,
    ) {
        self.delete_matching_placements(free_data, |placement| {
            let Some(geometry) = placement.geometry else {
                return false;
            };
            if geometry.anchor.alternate != alternate {
                return false;
            }
            if z_filter.is_some_and(|z| geometry.z_index != z) {
                return false;
            }
            let (Some(height), Some(width)) = (geometry.rows, geometry.columns) else {
                return false;
            };
            let top = geometry.anchor.row as i128
                + i128::from(geometry.row_offset)
                + i128::from(geometry.clip_top_rows);
            let bottom =
                geometry.anchor.row as i128 + i128::from(geometry.row_offset) + i128::from(height)
                    - i128::from(geometry.clip_bottom_rows);
            let left = geometry.anchor.column as u128;
            let right = left + u128::from(width);
            top < region.bottom
                && bottom > region.top
                && top < bottom
                && left < region.right
                && right > region.left
                && right > left
        });
    }

    /// A column selector spans all rows on the active screen, including
    /// scrollback. Only the placement width needs to be known.
    fn delete_column_placements(&mut self, alternate: bool, column: u128, free_data: bool) {
        self.delete_matching_placements(free_data, |placement| {
            let Some(geometry) = placement.geometry else {
                return false;
            };
            if geometry.anchor.alternate != alternate {
                return false;
            }
            let Some(width) = geometry.columns else {
                return false;
            };
            let left = geometry.anchor.column as u128;
            left <= column && column < left + u128::from(width)
        });
    }

    /// A row selector needs a known height, but not a known width. Scrolled
    /// placements are matched only where they still intersect the screen row.
    fn delete_row_placements(&mut self, alternate: bool, row: i128, free_data: bool) {
        self.delete_matching_placements(free_data, |placement| {
            let Some(geometry) = placement.geometry else {
                return false;
            };
            if geometry.anchor.alternate != alternate {
                return false;
            }
            let Some(height) = geometry.rows else {
                return false;
            };
            let top = geometry.anchor.row as i128
                + i128::from(geometry.row_offset)
                + i128::from(geometry.clip_top_rows);
            let bottom =
                geometry.anchor.row as i128 + i128::from(geometry.row_offset) + i128::from(height)
                    - i128::from(geometry.clip_bottom_rows);
            top <= row && row < bottom
        });
    }

    /// A z-index selector spans the active screen and its scrollback, but not
    /// the other screen. Placements without geometry cannot be matched.
    fn delete_z_placements(&mut self, alternate: bool, z_index: i32, free_data: bool) {
        self.delete_matching_placements(free_data, |placement| {
            placement.geometry.is_some_and(|geometry| {
                geometry.anchor.alternate == alternate && geometry.z_index == z_index
            })
        });
    }

    fn delete_matching_placements(
        &mut self,
        free_data: bool,
        mut matches: impl FnMut(&Placement) -> bool,
    ) {
        let mut touched_images = BTreeSet::new();
        self.placements.retain(|placement| {
            let remove = matches(placement);
            if remove {
                touched_images.insert(placement.image_id);
            }
            !remove
        });
        if !touched_images.is_empty() {
            self.changed();
        }
        if free_data {
            for id in touched_images {
                if !self
                    .placements
                    .iter()
                    .any(|placement| placement.image_id == id)
                {
                    self.remove(id);
                }
            }
        }
    }

    fn delete_placements(&mut self, image_id: u32, placement_id: Option<u32>, free_data: bool) {
        let before = self.placements.len();
        self.placements.retain(|placement| {
            placement.image_id != image_id
                || placement_id.is_some_and(|id| placement.placement_id != Some(id))
        });
        if self.placements.len() != before {
            self.changed();
        }
        if free_data
            && !self
                .placements
                .iter()
                .any(|placement| placement.image_id == image_id)
        {
            self.remove(image_id);
        }
    }

    /// Image-ID ranges are pane-wide, independent of screen, viewport, or
    /// placement geometry. Hard deletion also frees data-only images.
    fn delete_image_range(&mut self, first: u32, last: u32, free_data: bool) {
        if first > last {
            return;
        }
        self.delete_matching_placements(false, |placement| {
            (first..=last).contains(&placement.image_id)
        });
        if free_data {
            let unreferenced: Vec<u32> = self
                .images
                .range(first..=last)
                .map(|(&id, _)| id)
                .filter(|id| {
                    !self
                        .placements
                        .iter()
                        .any(|placement| placement.image_id == *id)
                })
                .collect();
            for id in unreferenced {
                self.remove(id);
            }
        }
    }

    /// Explicit data removal, including its placement references.
    pub fn remove(&mut self, id: u32) -> Option<StoredImage> {
        let removed = self.images.remove(&id)?;
        self.decoded_dimensions.remove(&id);
        self.total_bytes -= removed.data.len();
        self.oldest.retain(|&entry| entry != id);
        self.placements.retain(|placement| placement.image_id != id);
        self.changed();
        Some(removed)
    }

    pub fn clear(&mut self) {
        let changed = !self.images.is_empty() || !self.placements.is_empty();
        self.images.clear();
        self.decoded_dimensions.clear();
        self.oldest.clear();
        self.placements.clear();
        self.total_bytes = 0;
        if changed {
            self.changed();
        }
    }
}

pub(crate) type Controls = BTreeMap<u8, Vec<u8>>;

/// Parse control-only APCs identically for store mutation and child replies.
pub(crate) fn parse_control_command(command: &[u8]) -> Option<Controls> {
    if command.len() > MAX_GRAPHICS_COMMAND_BYTES {
        return None;
    }
    let body = if let Some(bytes) = command.strip_prefix(b"\x1b_G") {
        bytes.strip_suffix(b"\x1b\\")?
    } else {
        let bytes = command.strip_prefix(&[0x9f, b'G'])?;
        bytes
            .strip_suffix(&[0x9c])
            .or_else(|| bytes.strip_suffix(b"\x1b\\"))?
    };
    let body = body.strip_suffix(b";").unwrap_or(body);
    if body.is_empty() || body.contains(&b';') {
        return None;
    }
    let mut controls = Controls::new();
    for pair in body.split(|&byte| byte == b',') {
        let equals = pair.iter().position(|&byte| byte == b'=')?;
        let (key, with_equals) = pair.split_at(equals);
        let value = &with_equals[1..];
        if key.len() != 1
            || !key[0].is_ascii_alphabetic()
            || value.is_empty()
            || !value.iter().all(u8::is_ascii_graphic)
            || controls.insert(key[0], value.to_vec()).is_some()
        {
            return None;
        }
    }
    if controls
        .get(&b'q')
        .is_some_and(|q| !matches!(q.as_slice(), b"0" | b"1" | b"2"))
    {
        return None;
    }
    Some(controls)
}

fn only_keys(controls: &Controls, allowed: &[u8]) -> bool {
    controls.keys().all(|key| allowed.contains(key))
}

fn required_id(controls: &Controls) -> Result<u32, StoreError> {
    controls
        .get(&b'i')
        .and_then(|bytes| parse_positive_u32(bytes))
        .ok_or(StoreError::UnsupportedIdentity)
}

fn required_delete_image_bound(controls: &Controls, key: u8) -> Result<u32, StoreError> {
    controls
        .get(&key)
        .and_then(|bytes| parse_u32(bytes))
        .ok_or(StoreError::UnsupportedIdentity)
}

/// Kitty delete coordinates are one-based screen cells, unlike source pixel
/// coordinates in placement commands.
fn required_delete_cell(
    controls: &Controls,
    rows: usize,
    columns: usize,
) -> Result<(i128, u128), StoreError> {
    let column = required_delete_coordinate(controls, b'x', columns)?;
    let row = required_delete_coordinate(controls, b'y', rows)?;
    Ok((row as i128, column as u128))
}

fn required_delete_coordinate(
    controls: &Controls,
    key: u8,
    limit: usize,
) -> Result<usize, StoreError> {
    let value = controls
        .get(&key)
        .and_then(|value| parse_positive_u32(value))
        .and_then(|value| usize::try_from(value - 1).ok())
        .ok_or(StoreError::InvalidPlacement)?;
    (value < limit)
        .then_some(value)
        .ok_or(StoreError::InvalidPlacement)
}

fn required_delete_z_index(controls: &Controls) -> Result<i32, StoreError> {
    controls
        .get(&b'z')
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(|value| value.parse::<i32>().ok())
        .ok_or(StoreError::InvalidPlacement)
}

fn parse_optional_placement_id(value: Option<&[u8]>) -> Result<Option<u32>, StoreError> {
    value
        .map(|bytes| {
            let id = parse_u32(bytes).ok_or(StoreError::InvalidPlacement)?;
            Ok((id != 0).then_some(id))
        })
        .transpose()
        .map(Option::flatten)
}

fn parse_positive_u32(bytes: &[u8]) -> Option<u32> {
    parse_u32(bytes).filter(|&value| value != 0)
}

fn parse_u32(bytes: &[u8]) -> Option<u32> {
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<u32>().ok()
}

fn infer_cell_extent(
    pixel_width: u32,
    pixel_height: u32,
    columns: Option<u32>,
    rows: Option<u32>,
    cell: CellPixelSize,
) -> Option<(u32, u32)> {
    let ceil_ratio = |numerator: u128, denominator: u128| {
        u32::try_from(numerator.checked_add(denominator - 1)? / denominator)
            .ok()
            .filter(|&value| value != 0)
    };
    let width = u128::from(pixel_width);
    let height = u128::from(pixel_height);
    let cell_width = u128::from(cell.width);
    let cell_height = u128::from(cell.height);
    match (columns, rows) {
        (Some(columns), Some(rows)) => Some((columns, rows)),
        (Some(columns), None) => Some((
            columns,
            ceil_ratio(
                u128::from(columns) * cell_width * height,
                width * cell_height,
            )?,
        )),
        (None, Some(rows)) => Some((
            ceil_ratio(u128::from(rows) * cell_height * width, height * cell_width)?,
            rows,
        )),
        (None, None) => Some((
            ceil_ratio(width, cell_width)?,
            ceil_ratio(height, cell_height)?,
        )),
    }
}

fn parse_geometry<'a>(
    anchor: CellAnchor,
    control: impl Fn(u8) -> Option<&'a [u8]>,
    cell_pixels: Option<CellPixelSize>,
) -> Result<PlacementGeometry, StoreError> {
    let extent = |key| {
        control(key)
            .map(|bytes| parse_u32(bytes).ok_or(StoreError::InvalidPlacement))
            .transpose()
            .map(|value| value.filter(|&number| number != 0))
    };
    let coordinate = |key| {
        control(key)
            .map(|bytes| parse_u32(bytes).ok_or(StoreError::InvalidPlacement))
            .transpose()
            .map(|value| value.unwrap_or(0))
    };
    let cell_offset = CellPixelOffset {
        x: coordinate(b'X')?,
        y: coordinate(b'Y')?,
    };
    if cell_pixels.is_some_and(|cell| {
        cell_offset.x >= u32::from(cell.width) || cell_offset.y >= u32::from(cell.height)
    }) {
        return Err(StoreError::InvalidPlacement);
    }
    let z_index = control(b'z')
        .map(|bytes| {
            std::str::from_utf8(bytes)
                .ok()
                .and_then(|value| value.parse::<i32>().ok())
                .ok_or(StoreError::InvalidPlacement)
        })
        .transpose()?
        .unwrap_or(0);
    let cursor_stays = match control(b'C') {
        None | Some(b"0") => false,
        Some(b"1") => true,
        _ => return Err(StoreError::InvalidPlacement),
    };
    let columns = extent(b'c')?;
    let rows = extent(b'r')?;
    let sizing = match (columns, rows) {
        (None, None) => PlacementSizing::Natural,
        (Some(_), None) => PlacementSizing::FitWidth,
        (None, Some(_)) => PlacementSizing::FitHeight,
        (Some(_), Some(_)) => PlacementSizing::FitBox,
    };
    Ok(PlacementGeometry {
        anchor,
        row_offset: 0,
        source: SourceRect {
            left: coordinate(b'x')?,
            top: coordinate(b'y')?,
            width: extent(b'w')?,
            height: extent(b'h')?,
        },
        cell_offset,
        columns,
        rows,
        sizing,
        clip_top_rows: 0,
        clip_bottom_rows: 0,
        z_index,
        cursor_stays,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics_transfer::DirectTransferAssembler;

    fn transfer(command: &[u8]) -> AssembledDirectTransfer {
        DirectTransferAssembler::new().accept(command).unwrap()
    }

    #[test]
    fn revision_tracks_data_and_placement_changes_but_not_noops() {
        let mut store = ImageStore::new();
        assert_eq!(store.revision(), 0);
        assert_eq!(
            store.insert(transfer(b"\x1b_Ga=t,f=100;QQ==\x1b\\")),
            Err(StoreError::UnsupportedIdentity)
        );
        assert_eq!(store.revision(), 0);
        store.clear();
        assert_eq!(store.revision(), 0);

        let anchor = CellAnchor {
            row: 2,
            column: 1,
            alternate: false,
        };
        store
            .insert_at(transfer(b"\x1b_Ga=T,f=100,i=7,r=1;QQ==\x1b\\"), anchor)
            .unwrap();
        let inserted = store.revision();
        assert_ne!(inserted, 0);
        store.accept_control(b"\x1b_Ga=d,d=i,i=99\x1b\\").unwrap();
        assert_eq!(store.revision(), inserted);
        store
            .accept_control_at(b"\x1b_Ga=p,i=7,p=1,r=1\x1b\\", anchor)
            .unwrap();
        let placed = store.revision();
        assert_ne!(placed, inserted);
        store
            .accept_control(b"\x1b_Ga=d,d=i,i=7,p=1\x1b\\")
            .unwrap();
        let deleted = store.revision();
        assert_ne!(deleted, placed);
        store
            .accept_control(b"\x1b_Ga=d,d=i,i=7,p=1\x1b\\")
            .unwrap();
        assert_eq!(store.revision(), deleted);
        assert!(store.remove(7).is_some());
        let removed = store.revision();
        assert_ne!(removed, deleted);
        assert!(store.remove(7).is_none());
        store.clear();
        assert_eq!(store.revision(), removed);
    }

    #[test]
    fn revision_tracks_scroll_and_screen_clear_only_when_placements_change() {
        let mut store = ImageStore::new();
        let anchor = CellAnchor {
            row: 2,
            column: 1,
            alternate: false,
        };
        store
            .insert_at(transfer(b"\x1b_Ga=T,f=100,i=7,r=1;QQ==\x1b\\"), anchor)
            .unwrap();
        let event = ScrollEvent {
            top: 0,
            bottom: 4,
            lines: 1,
            down: false,
            alternate: true,
            archive: false,
            full_screen: true,
        };
        let before = store.revision();
        store.scroll_placements(event, 0);
        store.clear_screen_placements(true);
        assert_eq!(store.revision(), before);
        store.scroll_placements(
            ScrollEvent {
                alternate: false,
                ..event
            },
            0,
        );
        let shifted = store.revision();
        assert_ne!(shifted, before);
        store.clear_screen_placements(false);
        let cleared = store.revision();
        assert_ne!(cleared, shifted);
        store.clear_screen_placements(false);
        assert_eq!(store.revision(), cleared);
        store.clear();
        assert_ne!(store.revision(), cleared);
    }

    #[test]
    fn replacement_and_explicit_removal_account_for_bytes() {
        let mut store = ImageStore::new();
        assert_eq!(
            store.insert(transfer(b"\x1b_Ga=T,f=100,i=7;QUJD\x1b\\")),
            Ok(7)
        );
        assert_eq!(store.total_bytes(), 3);
        assert_eq!(
            store.insert(transfer(b"\x1b_Ga=t,f=100,i=7;RA==\x1b\\")),
            Ok(7)
        );
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(7).unwrap().data, b"D");
        assert_eq!(store.total_bytes(), 1);
        assert_eq!(store.remove(7).unwrap().data, b"D");
        assert!(store.is_empty());
        assert_eq!(store.total_bytes(), 0);
    }

    #[test]
    fn unsupported_identity_and_query_do_not_mutate_store() {
        let mut store = ImageStore::new();
        for command in [
            b"\x1b_Ga=t,f=100;QQ==\x1b\\".as_slice(),
            b"\x1b_Ga=t,f=100,i=0;QQ==\x1b\\",
            b"\x1b_Ga=t,f=100,i=7,I=9;QQ==\x1b\\",
        ] {
            assert_eq!(
                store.insert(transfer(command)),
                Err(StoreError::UnsupportedIdentity)
            );
        }
        assert_eq!(
            store.insert(transfer(b"\x1b_Ga=q,f=100,i=7;QQ==\x1b\\")),
            Err(StoreError::UnsupportedAction)
        );
        assert!(store.is_empty());
    }

    #[test]
    fn oldest_images_are_evicted_at_count_limit() {
        let mut store = ImageStore::new();
        for id in 1..=MAX_PANE_IMAGES + 1 {
            let command = format!("\x1b_Ga=t,f=100,i={id};QQ==\x1b\\");
            store.insert(transfer(command.as_bytes())).unwrap();
        }
        assert_eq!(store.len(), MAX_PANE_IMAGES);
        assert_eq!(store.total_bytes(), MAX_PANE_IMAGES);
        assert!(store.get(1).is_none());
        assert!(store.get(2).is_some());
        store.clear();
        assert!(store.is_empty());
    }

    #[test]
    fn transmit_and_put_track_anonymous_and_named_references() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=T,f=100,i=7,p=9;QQ==\x1b\\"))
            .unwrap();
        assert_eq!(
            store.placements().copied().collect::<Vec<_>>(),
            [Placement {
                image_id: 7,
                placement_id: Some(9),
                geometry: None,
            }]
        );
        store.accept_control(b"\x1b_Ga=p,i=7,p=9\x1b\\").unwrap();
        assert_eq!(store.placements().count(), 1);
        store.accept_control(b"\x1b_Ga=p,i=7,p=0\x1b\\").unwrap();
        store.accept_control(b"\x1b_Ga=p,i=7\x1b\\").unwrap();
        assert_eq!(store.placements().count(), 3);
        assert_eq!(
            store
                .placements()
                .filter(|placement| placement.placement_id.is_none())
                .count(),
            2
        );
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=7;Qg==\x1b\\"))
            .unwrap();
        assert_eq!(store.placements().count(), 0);
        assert_eq!(store.get(7).unwrap().data, b"B");
    }

    #[test]
    fn soft_and_hard_id_deletion_have_distinct_data_lifetimes() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\"))
            .unwrap();
        store.place(7, Some(2)).unwrap();
        store
            .accept_control(b"\x1b_Ga=d,d=I,i=7,p=1\x1b\\")
            .unwrap();
        assert!(store.get(7).is_some());
        assert_eq!(store.placements().count(), 1);
        store
            .accept_control(b"\x1b_Ga=d,d=i,i=7,p=2\x1b\\")
            .unwrap();
        assert!(store.get(7).is_some());
        assert_eq!(store.placements().count(), 0);
        store.accept_control(b"\x1b_Ga=d,d=I,i=7\x1b\\").unwrap();
        assert!(store.is_empty());
        assert_eq!(store.total_bytes(), 0);
    }

    #[test]
    fn image_range_delete_is_inclusive_and_frees_data_only_images() {
        let mut store = ImageStore::new();
        for id in [7, 8, 9, u32::MAX] {
            let command = format!("\x1b_Ga=T,f=100,i={id},p=1;QQ==\x1b\\");
            store.insert(transfer(command.as_bytes())).unwrap();
        }
        store
            .accept_control(b"\x1b_Ga=d,d=r,x=7,y=8\x1b\\")
            .unwrap();
        assert_eq!(
            store.placements().map(|p| p.image_id).collect::<Vec<_>>(),
            [9, u32::MAX]
        );
        assert!(store.get(7).is_some());
        assert!(store.get(8).is_some());

        store.accept_control(b"\x1b_Ga=p,i=7,p=2\x1b\\").unwrap();
        store
            .accept_control(b"\x1b_Ga=d,d=R,x=7,y=8\x1b\\")
            .unwrap();
        assert!(store.get(7).is_none());
        assert!(store.get(8).is_none());
        assert!(store.get(9).is_some());
        assert!(store.get(u32::MAX).is_some());
        store
            .accept_control(b"\x1b_Ga=d,d=R,x=4294967295,y=4294967295\x1b\\")
            .unwrap();
        assert!(store.get(u32::MAX).is_none());
        assert_eq!(store.total_bytes(), 1);
    }

    #[test]
    fn image_range_delete_rejects_bad_controls_without_mutation() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\"))
            .unwrap();
        let original = store.revision();
        for command in [
            b"\x1b_Ga=d,d=R,x=7\x1b\\".as_slice(),
            b"\x1b_Ga=d,d=R,y=7\x1b\\",
            b"\x1b_Ga=d,d=R,x=bad,y=7\x1b\\",
            b"\x1b_Ga=d,d=R,x=7,y=4294967296\x1b\\",
            b"\x1b_Ga=d,d=R,x=7,y=7,i=7\x1b\\",
        ] {
            assert!(store.accept_control(command).is_err());
            assert_eq!(store.revision(), original);
        }
        store
            .accept_control(b"\x1b_Ga=d,d=R,x=8,y=7\x1b\\")
            .unwrap();
        assert_eq!(store.revision(), original);
        store
            .accept_control(b"\x1b_Ga=d,d=R,x=0,y=7\x1b\\")
            .unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn z_delete_needs_screen_identity_but_no_viewport_or_resolved_extent() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=7;QQ==\x1b\\"))
            .unwrap();
        let main = CellAnchor::default();
        let alternate = CellAnchor {
            alternate: true,
            ..main
        };
        store
            .accept_control_at(b"\x1b_Ga=p,i=7,p=1,z=-2\x1b\\", main)
            .unwrap();
        store
            .accept_control_at(b"\x1b_Ga=p,i=7,p=2,z=-2\x1b\\", alternate)
            .unwrap();
        store.place(7, Some(3)).unwrap();
        assert!(
            store
                .placements()
                .next()
                .unwrap()
                .geometry
                .unwrap()
                .rows
                .is_none()
        );
        let revision = store.revision();
        assert_eq!(
            store.accept_control(b"\x1b_Ga=d,d=Z,z=-2\x1b\\"),
            Err(StoreError::UnsupportedAction)
        );
        assert_eq!(store.revision(), revision);
        store
            .accept_control_at(b"\x1b_Ga=d,d=Z,z=-2\x1b\\", alternate)
            .unwrap();
        let ids: Vec<_> = store.placements().map(|p| p.placement_id).collect();
        assert_eq!(ids, [Some(1), Some(3)]);
        assert!(store.get(7).is_some());
        store
            .accept_control_at(b"\x1b_Ga=d,d=Z,z=-2\x1b\\", main)
            .unwrap();
        assert_eq!(store.placements().next().unwrap().placement_id, Some(3));
        assert!(store.get(7).is_some());
        store
            .accept_control_at(b"\x1b_Ga=d,d=Z,z=0\x1b\\", main)
            .unwrap();
        assert_eq!(store.placements().count(), 1);
    }

    #[test]
    fn cursor_delete_requires_anchor_and_can_free_unreferenced_data() {
        let mut store = ImageStore::new();
        let anchor = CellAnchor::default();
        store
            .insert_at(
                transfer(b"\x1b_Ga=T,f=100,i=7,p=1,c=1,r=1;QQ==\x1b\\"),
                anchor,
            )
            .unwrap();
        assert_eq!(
            store.accept_control(b"\x1b_Ga=d,d=c\x1b\\"),
            Err(StoreError::UnsupportedAction)
        );
        store
            .accept_control_at(b"\x1b_Ga=d,d=c\x1b\\", anchor)
            .unwrap();
        assert_eq!(store.placements().count(), 0);
        assert!(store.get(7).is_some());
        store
            .accept_control_at(b"\x1b_Ga=p,i=7,p=2,c=1,r=1\x1b\\", anchor)
            .unwrap();
        store
            .accept_control_at(b"\x1b_Ga=d,d=C\x1b\\", anchor)
            .unwrap();
        assert!(store.get(7).is_none());
    }

    #[test]
    fn invalid_controls_and_missing_images_do_not_change_references() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=7;QQ==\x1b\\"))
            .unwrap();
        assert_eq!(
            store.accept_control(b"\x1b_Ga=p,i=8,p=2\x1b\\"),
            Err(StoreError::MissingImage)
        );
        assert_eq!(store.place(7, Some(0)), Err(StoreError::InvalidPlacement));
        for command in [
            b"\x1b_Ga=p,i=7,p=no\x1b\\".as_slice(),
            b"\x1b_Ga=d,d=I,i=7,p=no\x1b\\",
            b"\x1b_Ga=d,d=A\x1b\\",
            b"\x1b_Ga=p,i=7,z=no\x1b\\",
            b"\x1b_Ga=d,d=I,i=7;payload\x1b\\",
        ] {
            assert!(store.accept_control(command).is_err());
        }
        assert!(store.get(7).is_some());
        assert_eq!(store.placements().count(), 0);
        store.accept_control(b"\x9fGa=p,i=7,p=2\x9c").unwrap();
        assert_eq!(store.placements().count(), 1);
        let oversized = format!(
            "\x1b_Ga=p,i=7,q={};\x1b\\",
            "0".repeat(MAX_GRAPHICS_COMMAND_BYTES)
        );
        assert_eq!(
            store.accept_control(oversized.as_bytes()),
            Err(StoreError::UnsupportedAction)
        );
        assert_eq!(store.placements().count(), 1);
    }

    #[test]
    fn evicting_image_also_removes_its_references() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=T,f=100,i=1;QQ==\x1b\\"))
            .unwrap();
        for id in 2..=MAX_PANE_IMAGES + 1 {
            let command = format!("\x1b_Ga=t,f=100,i={id};QQ==\x1b\\");
            store.insert(transfer(command.as_bytes())).unwrap();
        }
        assert!(store.get(1).is_none());
        assert_eq!(store.placements().count(), 0);
    }

    #[test]
    fn placement_references_are_bounded() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=1;QQ==\x1b\\"))
            .unwrap();
        for id in 1..=MAX_PANE_PLACEMENTS + 1 {
            store.place(1, Some(id as u32)).unwrap();
        }
        assert_eq!(store.placements().count(), MAX_PANE_PLACEMENTS);
        assert!(
            !store
                .placements()
                .any(|placement| placement.placement_id == Some(1))
        );
        assert!(
            store
                .placements()
                .any(|placement| placement.placement_id == Some(2))
        );
    }

    #[test]
    fn anchored_placement_records_explicit_cell_layout_and_replacement() {
        let mut store = ImageStore::new();
        let first = CellAnchor {
            row: 2,
            column: 3,
            alternate: false,
        };
        store
            .insert_at(
                transfer(b"\x1b_Ga=T,f=100,i=7,p=9,c=2,r=3,z=-4,C=1,X=3,Y=4;QQ==\x1b\\"),
                first,
            )
            .unwrap();
        assert_eq!(
            store.placements().next().unwrap().geometry,
            Some(PlacementGeometry {
                anchor: first,
                row_offset: 0,
                source: SourceRect::default(),
                cell_offset: CellPixelOffset { x: 3, y: 4 },
                columns: Some(2),
                rows: Some(3),
                sizing: PlacementSizing::FitBox,
                clip_top_rows: 0,
                clip_bottom_rows: 0,
                z_index: -4,
                cursor_stays: true,
            })
        );
        let second = CellAnchor {
            row: 4,
            column: 5,
            alternate: true,
        };
        store
            .accept_control_at(b"\x1b_Ga=p,i=7,p=9,c=1,r=2,z=3,X=1,Y=2\x1b\\", second)
            .unwrap();
        let placements: Vec<_> = store.placements().copied().collect();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].geometry.unwrap().anchor, second);
        assert_eq!(placements[0].geometry.unwrap().columns, Some(1));
        assert_eq!(placements[0].geometry.unwrap().rows, Some(2));
        assert_eq!(
            placements[0].geometry.unwrap().sizing,
            PlacementSizing::FitBox
        );
        assert_eq!(
            placements[0].geometry.unwrap().cell_offset,
            CellPixelOffset { x: 1, y: 2 }
        );
        assert_eq!(placements[0].geometry.unwrap().z_index, 3);
        assert!(!placements[0].geometry.unwrap().cursor_stays);
    }

    #[test]
    fn malformed_layout_does_not_replace_existing_image() {
        let mut store = ImageStore::new();
        store
            .insert(transfer(b"\x1b_Ga=t,f=100,i=7;QQ==\x1b\\"))
            .unwrap();
        let anchor = CellAnchor::default();
        for command in [
            b"\x1b_Ga=T,f=100,i=7,c=no;Qg==\x1b\\".as_slice(),
            b"\x1b_Ga=T,f=100,i=7,x=no;Qg==\x1b\\",
            b"\x1b_Ga=T,f=100,i=7,w=no;Qg==\x1b\\",
        ] {
            assert_eq!(
                store.insert_at(transfer(command), anchor),
                Err(StoreError::InvalidPlacement)
            );
        }
        assert_eq!(store.get(7).unwrap().data, b"A");
        assert!(store.placements().next().is_none());
    }

    #[test]
    fn cell_pixel_extent_uses_ceiling_and_preserves_aspect_ratio() {
        assert_eq!(CellPixelSize::new(0, 1), None);
        assert_eq!(CellPixelSize::new(1, 0), None);
        let cell = CellPixelSize::new(2, 3).unwrap();
        assert_eq!(infer_cell_extent(5, 7, None, None, cell), Some((3, 3)));
        assert_eq!(infer_cell_extent(5, 7, Some(2), None, cell), Some((2, 2)));
        assert_eq!(infer_cell_extent(5, 7, None, Some(2), cell), Some((3, 2)));
        assert_eq!(
            infer_cell_extent(5, 7, Some(4), Some(5), cell),
            Some((4, 5))
        );
        assert_eq!(
            infer_cell_extent(
                1,
                u32::MAX,
                Some(u32::MAX),
                None,
                CellPixelSize::new(u16::MAX, 1).unwrap(),
            ),
            None
        );
    }

    #[test]
    fn terminal_pixels_require_an_exact_nonzero_cell_grid() {
        let cell = CellPixelSize::new(9, 20);
        assert_eq!(CellPixelSize::from_terminal_size(30, 100, 900, 600), cell);
        assert_eq!(CellPixelSize::from_terminal_size(0, 100, 900, 600), None);
        assert_eq!(CellPixelSize::from_terminal_size(30, 0, 900, 600), None);
        assert_eq!(CellPixelSize::from_terminal_size(30, 100, 0, 600), None);
        assert_eq!(CellPixelSize::from_terminal_size(30, 100, 900, 0), None);
        assert_eq!(CellPixelSize::from_terminal_size(30, 100, 901, 600), None);
        assert_eq!(CellPixelSize::from_terminal_size(30, 100, 900, 601), None);
    }

    #[test]
    fn source_rectangle_intersects_image_without_overflow() {
        assert_eq!(
            SourceRect::default().intersected_dimensions(6, 4),
            Some((6, 4))
        );
        assert_eq!(
            SourceRect {
                left: 2,
                top: 1,
                width: Some(3),
                height: Some(2),
            }
            .intersected_dimensions(6, 4),
            Some((3, 2))
        );
        assert_eq!(
            SourceRect {
                left: 4,
                top: 3,
                width: Some(u32::MAX),
                height: Some(u32::MAX),
            }
            .intersected_dimensions(6, 4),
            Some((2, 1))
        );
        assert_eq!(
            SourceRect {
                left: 6,
                ..SourceRect::default()
            }
            .intersected_dimensions(6, 4),
            None
        );
    }

    #[test]
    fn pixel_layout_preserves_natural_size_after_extent_inference() {
        let cell = CellPixelSize::new(2, 2).unwrap();
        let geometry = PlacementGeometry {
            anchor: CellAnchor::default(),
            row_offset: 0,
            source: SourceRect {
                left: 2,
                top: 1,
                width: Some(5),
                height: Some(3),
            },
            cell_offset: CellPixelOffset { x: 1, y: 1 },
            columns: Some(99),
            rows: Some(99),
            sizing: PlacementSizing::Natural,
            clip_top_rows: 0,
            clip_bottom_rows: 0,
            z_index: 0,
            cursor_stays: false,
        };
        assert_eq!(
            geometry.pixel_layout(6, 4, cell),
            Some(PlacementPixelLayout {
                source: PixelRect {
                    x: 2,
                    y: 1,
                    width: 4,
                    height: 3,
                },
                cell_bounds: PixelSize {
                    width: 4,
                    height: 4,
                },
                destination: PixelRect {
                    x: 1,
                    y: 1,
                    width: 4,
                    height: 3,
                },
            })
        );
        assert_eq!(
            geometry
                .pixel_layout(6, 4, CellPixelSize::new(4, 3).unwrap())
                .unwrap()
                .cell_bounds,
            PixelSize {
                width: 4,
                height: 3,
            }
        );
        assert_eq!(
            PlacementGeometry {
                source: SourceRect {
                    left: 6,
                    ..SourceRect::default()
                },
                ..geometry
            }
            .pixel_layout(6, 4, cell),
            None
        );
        assert_eq!(
            PlacementGeometry {
                cell_offset: CellPixelOffset { x: 2, y: 0 },
                ..geometry
            }
            .pixel_layout(6, 4, cell),
            None
        );
    }

    #[test]
    fn pixel_anchor_handles_scrolled_rows_and_overflow() {
        let geometry = PlacementGeometry {
            anchor: CellAnchor {
                row: 2,
                column: 3,
                alternate: false,
            },
            row_offset: -4,
            source: SourceRect::default(),
            cell_offset: CellPixelOffset::default(),
            columns: None,
            rows: None,
            sizing: PlacementSizing::Natural,
            clip_top_rows: 0,
            clip_bottom_rows: 0,
            z_index: 0,
            cursor_stays: false,
        };
        assert_eq!(
            geometry.pixel_anchor(CellPixelSize::new(2, 3).unwrap()),
            Some(SignedPixelPoint { x: 6, y: -6 })
        );
        assert_eq!(
            PlacementGeometry {
                row_offset: i64::MAX,
                ..geometry
            }
            .pixel_anchor(CellPixelSize::new(2, 3).unwrap()),
            None
        );
    }

    #[test]
    fn pixel_layout_scales_one_axis_and_centers_a_two_axis_box() {
        let cell = CellPixelSize::new(2, 2).unwrap();
        let base = PlacementGeometry {
            anchor: CellAnchor::default(),
            row_offset: 0,
            source: SourceRect::default(),
            cell_offset: CellPixelOffset::default(),
            columns: Some(3),
            rows: Some(99),
            sizing: PlacementSizing::FitWidth,
            clip_top_rows: 0,
            clip_bottom_rows: 0,
            z_index: 0,
            cursor_stays: false,
        };
        let width_fit = base.pixel_layout(4, 3, cell).unwrap();
        assert_eq!(
            width_fit.cell_bounds,
            PixelSize {
                width: 6,
                height: 6
            }
        );
        assert_eq!(
            width_fit.destination,
            PixelRect {
                x: 0,
                y: 0,
                width: 6,
                height: 5,
            }
        );

        let height_fit = PlacementGeometry {
            columns: Some(99),
            rows: Some(2),
            sizing: PlacementSizing::FitHeight,
            ..base
        }
        .pixel_layout(4, 3, cell)
        .unwrap();
        assert_eq!(
            height_fit.cell_bounds,
            PixelSize {
                width: 6,
                height: 4
            }
        );
        assert_eq!(height_fit.destination.width, 5);
        assert_eq!(height_fit.destination.height, 4);

        let box_fit = PlacementGeometry {
            columns: Some(3),
            rows: Some(2),
            sizing: PlacementSizing::FitBox,
            ..base
        }
        .pixel_layout(2, 4, cell)
        .unwrap();
        assert_eq!(
            box_fit.cell_bounds,
            PixelSize {
                width: 6,
                height: 4
            }
        );
        assert_eq!(
            box_fit.destination,
            PixelRect {
                x: 2,
                y: 0,
                width: 2,
                height: 4,
            }
        );

        let letterbox = PlacementGeometry {
            columns: Some(2),
            rows: Some(3),
            sizing: PlacementSizing::FitBox,
            ..base
        }
        .pixel_layout(4, 2, cell)
        .unwrap();
        assert_eq!(
            letterbox.cell_bounds,
            PixelSize {
                width: 4,
                height: 6
            }
        );
        assert_eq!(letterbox.destination.y, 2);
        assert_eq!(letterbox.destination.height, 2);
        assert_eq!(
            PlacementGeometry {
                columns: Some(u32::MAX),
                rows: Some(1),
                sizing: PlacementSizing::FitBox,
                ..base
            }
            .pixel_layout(4, 3, cell),
            None
        );
    }
}
