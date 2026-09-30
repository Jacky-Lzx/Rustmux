//! Pure cell and pixel geometry for Kitty image placement.
//! No image storage, decoding, or terminal output occurs here.

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct CellAnchor {
    pub row: usize,
    pub column: usize,
    pub alternate: bool,
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
    pub(super) fn intersected_dimensions(
        self,
        image_width: u32,
        image_height: u32,
    ) -> Option<(u32, u32)> {
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

pub(super) fn infer_cell_extent(
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

#[cfg(test)]
mod tests {
    use super::*;

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
