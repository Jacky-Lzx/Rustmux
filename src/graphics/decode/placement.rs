//! Bounded sampling and clipping of already-decoded RGBA placement pixels.
//! Stream decoding stays in the parent module and shares its sampling math.

use super::{DecodeError, DecodedImage, decoded_size, nearest_sample};
use crate::graphics::geometry::{
    CellPixelSize, PixelRect, PixelSize, PlacementGeometry, PlacementPixelLayout, SignedPixelPoint,
};

#[derive(Debug, Eq, PartialEq)]
pub struct ResampledPlacement {
    /// Content rectangle relative to the placement's anchor cell. Letterbox
    /// space is not allocated in `pixels`.
    pub destination: PixelRect,
    /// Row-major, eight-bit RGBA content pixels.
    pub pixels: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ClippedPlacement {
    /// Visible rectangle in viewport pixel coordinates.
    pub destination: PixelRect,
    /// Row-major RGBA pixels for only the visible rectangle.
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ClipError {
    InvalidPixels,
    InvalidDestination,
    InvalidGeometry,
    OutputLimit,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResampleError {
    InvalidPixels,
    InvalidLayout,
    OutputLimit,
}

impl DecodedImage {
    /// Sample a cropped placement into its content rectangle. The caller
    /// composes the returned pixels at `destination`; this does not draw into
    /// a pane or allocate transparent letterbox padding.
    pub fn resample_placement(
        &self,
        layout: PlacementPixelLayout,
    ) -> Result<ResampledPlacement, ResampleError> {
        self.resample_placement_region(layout, layout.destination)
    }

    /// Sample only a rectangle within the destination while keeping the same
    /// pixel-center coordinates as a full placement resample.
    pub fn resample_placement_region(
        &self,
        layout: PlacementPixelLayout,
        region: PixelRect,
    ) -> Result<ResampledPlacement, ResampleError> {
        let expected = decoded_size(self.width, self.height).map_err(|error| match error {
            DecodeError::OutputLimit => ResampleError::OutputLimit,
            _ => ResampleError::InvalidPixels,
        })?;
        if self.pixels.len() != expected {
            return Err(ResampleError::InvalidPixels);
        }
        let source = layout.source;
        let destination = layout.destination;
        if source.width == 0
            || source.height == 0
            || destination.width == 0
            || destination.height == 0
            || source
                .x
                .checked_add(source.width)
                .is_none_or(|end| end > self.width)
            || source
                .y
                .checked_add(source.height)
                .is_none_or(|end| end > self.height)
            || destination.x.checked_add(destination.width).is_none()
            || destination.y.checked_add(destination.height).is_none()
            || region.width == 0
            || region.height == 0
            || region.x < destination.x
            || region.y < destination.y
            || region
                .x
                .checked_add(region.width)
                .is_none_or(|end| end > destination.x + destination.width)
            || region
                .y
                .checked_add(region.height)
                .is_none_or(|end| end > destination.y + destination.height)
        {
            return Err(ResampleError::InvalidLayout);
        }
        let output_size =
            decoded_size(region.width, region.height).map_err(|_| ResampleError::OutputLimit)?;
        let mut pixels = vec![0; output_size];
        let output_width = usize::try_from(region.width).unwrap();
        let input_width = usize::try_from(self.width).unwrap();
        for (y, row) in pixels.chunks_exact_mut(output_width * 4).enumerate() {
            let output_y = usize::try_from(region.y - destination.y).unwrap() + y;
            let source_y = source.y + nearest_sample(output_y, source.height, destination.height);
            for (x, rgba) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let output_x = usize::try_from(region.x - destination.x).unwrap() + x;
                let source_x = source.x + nearest_sample(output_x, source.width, destination.width);
                let index = (usize::try_from(source_y).unwrap() * input_width
                    + usize::try_from(source_x).unwrap())
                    * 4;
                rgba.copy_from_slice(&self.pixels[index..index + 4]);
            }
        }
        Ok(ResampledPlacement {
            destination: region,
            pixels,
        })
    }
}

impl ResampledPlacement {
    /// Intersect the content with a pane-sized viewport. `anchor` is the
    /// signed pixel position of the placement's anchor cell within that
    /// viewport; it may be negative after scrolling. No text or other images
    /// are blended here.
    pub fn clip_to_viewport(
        &self,
        anchor: SignedPixelPoint,
        viewport: PixelSize,
    ) -> Result<Option<ClippedPlacement>, ClipError> {
        self.clip_between_rows(anchor, viewport, i128::MIN, i128::MAX)
    }

    /// Apply permanent top/bottom cell-row clips accumulated while scrolling,
    /// then intersect with the pane viewport. Unclipped edges remain unbounded
    /// so an `X/Y` offset is not mistaken for an extra placement row.
    pub fn clip_to_viewport_with_scroll_clip(
        &self,
        geometry: PlacementGeometry,
        cell: CellPixelSize,
        viewport: PixelSize,
    ) -> Result<Option<ClippedPlacement>, ClipError> {
        let anchor = geometry
            .pixel_anchor(cell)
            .ok_or(ClipError::InvalidGeometry)?;
        let (top, bottom) = scroll_clip_bounds(geometry, cell, anchor)?;
        self.clip_between_rows(anchor, viewport, top, bottom)
    }

    fn clip_between_rows(
        &self,
        anchor: SignedPixelPoint,
        viewport: PixelSize,
        clip_top: i128,
        clip_bottom: i128,
    ) -> Result<Option<ClippedPlacement>, ClipError> {
        let source = self.destination;
        let source_size =
            decoded_size(source.width, source.height).map_err(|error| match error {
                DecodeError::OutputLimit => ClipError::OutputLimit,
                _ => ClipError::InvalidDestination,
            })?;
        if source.x.checked_add(source.width).is_none()
            || source.y.checked_add(source.height).is_none()
        {
            return Err(ClipError::InvalidDestination);
        }
        if self.pixels.len() != source_size {
            return Err(ClipError::InvalidPixels);
        }
        let Some((region, destination)) =
            visible_bounds(source, anchor, viewport, clip_top, clip_bottom)?
        else {
            return Ok(None);
        };
        let skip_x = usize::try_from(region.x - source.x).unwrap();
        let skip_y = usize::try_from(region.y - source.y).unwrap();
        let input_width = usize::try_from(source.width).unwrap();
        let output_width = usize::try_from(region.width).unwrap();
        let mut pixels = vec![0; decoded_size(region.width, region.height).unwrap()];
        for (row, destination_row) in pixels.chunks_exact_mut(output_width * 4).enumerate() {
            let start = ((skip_y + row) * input_width + skip_x) * 4;
            destination_row.copy_from_slice(&self.pixels[start..start + output_width * 4]);
        }
        Ok(Some(ClippedPlacement {
            destination,
            pixels,
        }))
    }
}

pub(crate) fn visible_placement_region(
    destination: PixelRect,
    geometry: PlacementGeometry,
    cell: CellPixelSize,
    viewport: PixelSize,
) -> Result<Option<(PixelRect, PixelRect)>, ClipError> {
    let anchor = geometry
        .pixel_anchor(cell)
        .ok_or(ClipError::InvalidGeometry)?;
    let (top, bottom) = scroll_clip_bounds(geometry, cell, anchor)?;
    visible_bounds(destination, anchor, viewport, top, bottom)
}

fn scroll_clip_bounds(
    geometry: PlacementGeometry,
    cell: CellPixelSize,
    anchor: SignedPixelPoint,
) -> Result<(i128, i128), ClipError> {
    if geometry.clip_top_rows == 0 && geometry.clip_bottom_rows == 0 {
        return Ok((i128::MIN, i128::MAX));
    }
    let rows = geometry.rows.ok_or(ClipError::InvalidGeometry)?;
    if geometry
        .clip_top_rows
        .saturating_add(geometry.clip_bottom_rows)
        >= rows
    {
        return Ok((0, 0));
    }
    let row_height = i128::from(cell.height());
    let top = if geometry.clip_top_rows == 0 {
        i128::MIN
    } else {
        i128::from(anchor.y) + i128::from(geometry.clip_top_rows) * row_height
    };
    let bottom = if geometry.clip_bottom_rows == 0 {
        i128::MAX
    } else {
        i128::from(anchor.y) + i128::from(rows - geometry.clip_bottom_rows) * row_height
    };
    Ok((top, bottom))
}

fn visible_bounds(
    source: PixelRect,
    anchor: SignedPixelPoint,
    viewport: PixelSize,
    clip_top: i128,
    clip_bottom: i128,
) -> Result<Option<(PixelRect, PixelRect)>, ClipError> {
    if source.width == 0
        || source.height == 0
        || source.x.checked_add(source.width).is_none()
        || source.y.checked_add(source.height).is_none()
    {
        return Err(ClipError::InvalidDestination);
    }
    if viewport.width == 0 || viewport.height == 0 {
        return Ok(None);
    }
    let left = i128::from(anchor.x) + i128::from(source.x);
    let top = i128::from(anchor.y) + i128::from(source.y);
    let visible_left = left.max(0);
    let visible_top = top.max(0).max(clip_top);
    let visible_right = (left + i128::from(source.width)).min(i128::from(viewport.width));
    let visible_bottom = (top + i128::from(source.height))
        .min(i128::from(viewport.height))
        .min(clip_bottom);
    if visible_left >= visible_right || visible_top >= visible_bottom {
        return Ok(None);
    }
    let width = u32::try_from(visible_right - visible_left).unwrap();
    let height = u32::try_from(visible_bottom - visible_top).unwrap();
    let region = PixelRect {
        x: source.x + u32::try_from(visible_left - left).unwrap(),
        y: source.y + u32::try_from(visible_top - top).unwrap(),
        width,
        height,
    };
    let destination = PixelRect {
        x: u32::try_from(visible_left).unwrap(),
        y: u32::try_from(visible_top).unwrap(),
        width,
        height,
    };
    Ok(Some((region, destination)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::geometry::{CellAnchor, CellPixelOffset, PlacementSizing, SourceRect};

    #[test]
    fn resample_cropped_rgba_content_without_allocating_letterbox_padding() {
        let rgba = [
            1, 0, 0, 10, 2, 0, 0, 20, 3, 0, 0, 30, 4, 0, 0, 40, 5, 0, 0, 50, 6, 0, 0, 60,
        ];
        let image = DecodedImage {
            width: 3,
            height: 2,
            pixels: rgba.to_vec(),
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 1,
                y: 0,
                width: 2,
                height: 2,
            },
            cell_bounds: PixelSize {
                width: 6,
                height: 6,
            },
            destination: PixelRect {
                x: 2,
                y: 1,
                width: 4,
                height: 4,
            },
        };
        let resampled = image.resample_placement(layout).unwrap();
        assert_eq!(resampled.destination, layout.destination);
        assert_eq!(resampled.pixels.len(), 4 * 4 * 4);
        assert_eq!(
            resampled
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| (rgba[0], rgba[3]))
                .collect::<Vec<_>>(),
            [
                (2, 20),
                (2, 20),
                (3, 30),
                (3, 30),
                (2, 20),
                (2, 20),
                (3, 30),
                (3, 30),
                (5, 50),
                (5, 50),
                (6, 60),
                (6, 60),
                (5, 50),
                (5, 50),
                (6, 60),
                (6, 60),
            ]
        );
    }

    #[test]
    fn resample_raw_region_matches_full_pixel_center_sampling() {
        let mut pixels = Vec::new();
        for y in 0..3u8 {
            for x in 0..4u8 {
                pixels.extend_from_slice(&[x, y, 7, 255]);
            }
        }
        let image = DecodedImage {
            width: 4,
            height: 3,
            pixels,
        };
        for (width, height) in [(7, 5), (2, 2)] {
            let layout = PlacementPixelLayout {
                source: PixelRect {
                    x: 1,
                    y: 0,
                    width: 3,
                    height: 3,
                },
                cell_bounds: PixelSize { width, height },
                destination: PixelRect {
                    x: 11,
                    y: 13,
                    width,
                    height,
                },
            };
            let region = PixelRect {
                x: 12,
                y: 14,
                width: width - 1,
                height: height - 1,
            };
            let full = image.resample_placement(layout).unwrap();
            let visible = image.resample_placement_region(layout, region).unwrap();
            assert_eq!(visible.destination, region);
            for y in 0..region.height as usize {
                let full_start = ((y + 1) * width as usize + 1) * 4;
                let visible_start = y * region.width as usize * 4;
                assert_eq!(
                    &visible.pixels[visible_start..visible_start + region.width as usize * 4],
                    &full.pixels[full_start..full_start + region.width as usize * 4]
                );
            }
        }
    }

    #[test]
    fn resample_raw_region_stays_bounded_for_oversized_destination() {
        let image = DecodedImage {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            cell_bounds: PixelSize {
                width: 4096,
                height: 4096,
            },
            destination: PixelRect {
                x: 5,
                y: 7,
                width: 4096,
                height: 4096,
            },
        };
        assert_eq!(
            image.resample_placement(layout),
            Err(ResampleError::OutputLimit)
        );
        let region = PixelRect {
            x: 4099,
            y: 4101,
            width: 2,
            height: 2,
        };
        assert_eq!(
            image.resample_placement_region(layout, region),
            Ok(ResampledPlacement {
                destination: region,
                pixels: [1, 2, 3, 4].repeat(4),
            })
        );
    }

    #[test]
    fn resample_uses_pixel_centers_when_shrinking() {
        let mut pixels = Vec::new();
        for value in 0..16 {
            pixels.extend_from_slice(&[value, 0, 0, 255]);
        }
        let image = DecodedImage {
            width: 4,
            height: 4,
            pixels,
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            cell_bounds: PixelSize {
                width: 2,
                height: 2,
            },
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        };
        let resampled = image.resample_placement(layout).unwrap();
        assert_eq!(
            resampled
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| rgba[0])
                .collect::<Vec<_>>(),
            [5, 7, 13, 15]
        );
    }

    #[test]
    fn resample_rejects_bad_layout_pixels_and_excessive_output() {
        let image = DecodedImage {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        };
        let layout = PlacementPixelLayout {
            source: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            cell_bounds: PixelSize {
                width: 1,
                height: 1,
            },
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
        };
        assert_eq!(
            DecodedImage {
                pixels: vec![1, 2, 3],
                ..image
            }
            .resample_placement(layout),
            Err(ResampleError::InvalidPixels)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                source: PixelRect {
                    x: 1,
                    ..layout.source
                },
                ..layout
            }),
            Err(ResampleError::InvalidLayout)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                source: PixelRect {
                    x: u32::MAX,
                    width: 2,
                    ..layout.source
                },
                ..layout
            }),
            Err(ResampleError::InvalidLayout)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                destination: PixelRect {
                    width: 0,
                    ..layout.destination
                },
                ..layout
            }),
            Err(ResampleError::InvalidLayout)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                destination: PixelRect {
                    width: 10_000,
                    height: 10_000,
                    ..layout.destination
                },
                ..layout
            }),
            Err(ResampleError::OutputLimit)
        );
        assert_eq!(
            image.resample_placement(PlacementPixelLayout {
                destination: PixelRect {
                    x: u32::MAX,
                    width: 2,
                    ..layout.destination
                },
                ..layout
            }),
            Err(ResampleError::InvalidLayout)
        );
    }

    #[test]
    fn viewport_clip_copies_only_visible_rows_and_columns() {
        let mut pixels = Vec::new();
        for value in 1..=12 {
            pixels.extend_from_slice(&[value, 0, 0, 255 - value]);
        }
        let placement = ResampledPlacement {
            destination: PixelRect {
                x: 1,
                y: 1,
                width: 4,
                height: 3,
            },
            pixels,
        };
        let clipped = placement
            .clip_to_viewport(
                SignedPixelPoint { x: -2, y: -2 },
                PixelSize {
                    width: 3,
                    height: 2,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            clipped.destination,
            PixelRect {
                x: 0,
                y: 0,
                width: 3,
                height: 2,
            }
        );
        assert_eq!(
            clipped
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| (rgba[0], rgba[3]))
                .collect::<Vec<_>>(),
            [
                (6, 249),
                (7, 248),
                (8, 247),
                (10, 245),
                (11, 244),
                (12, 243)
            ]
        );
        let right_bottom = placement
            .clip_to_viewport(
                SignedPixelPoint { x: 0, y: 0 },
                PixelSize {
                    width: 3,
                    height: 3,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            right_bottom.destination,
            PixelRect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }
        );
        assert_eq!(
            right_bottom
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| rgba[0])
                .collect::<Vec<_>>(),
            [1, 2, 5, 6]
        );
        let entirely_visible = placement
            .clip_to_viewport(
                SignedPixelPoint { x: 5, y: 6 },
                PixelSize {
                    width: 20,
                    height: 20,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            entirely_visible.destination,
            PixelRect {
                x: 6,
                y: 7,
                width: 4,
                height: 3,
            }
        );
        assert_eq!(entirely_visible.pixels, placement.pixels);
    }

    #[test]
    fn viewport_clip_handles_disjoint_and_invalid_inputs() {
        let placement = ResampledPlacement {
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            pixels: vec![1, 2, 3, 4],
        };
        let viewport = PixelSize {
            width: 2,
            height: 2,
        };
        for anchor in [
            SignedPixelPoint { x: 2, y: 0 },
            SignedPixelPoint { x: -1, y: 0 },
            SignedPixelPoint {
                x: i64::MIN,
                y: i64::MAX,
            },
        ] {
            assert_eq!(placement.clip_to_viewport(anchor, viewport), Ok(None));
        }
        assert_eq!(
            placement.clip_to_viewport(
                SignedPixelPoint { x: 0, y: 0 },
                PixelSize {
                    width: 0,
                    height: 2,
                }
            ),
            Ok(None)
        );
        assert_eq!(
            ResampledPlacement {
                pixels: vec![1, 2, 3],
                ..placement
            }
            .clip_to_viewport(SignedPixelPoint { x: 0, y: 0 }, viewport),
            Err(ClipError::InvalidPixels)
        );
        assert_eq!(
            ResampledPlacement {
                destination: PixelRect {
                    width: 0,
                    ..placement.destination
                },
                pixels: placement.pixels.clone(),
            }
            .clip_to_viewport(SignedPixelPoint { x: 0, y: 0 }, viewport),
            Err(ClipError::InvalidDestination)
        );
        assert_eq!(
            ResampledPlacement {
                destination: PixelRect {
                    x: u32::MAX,
                    width: 2,
                    ..placement.destination
                },
                pixels: placement.pixels.clone(),
            }
            .clip_to_viewport(SignedPixelPoint { x: 0, y: 0 }, viewport),
            Err(ClipError::InvalidDestination)
        );
        assert_eq!(
            ResampledPlacement {
                destination: PixelRect {
                    width: 10_000,
                    height: 10_000,
                    ..placement.destination
                },
                pixels: placement.pixels.clone(),
            }
            .clip_to_viewport(SignedPixelPoint { x: 0, y: 0 }, viewport),
            Err(ClipError::OutputLimit)
        );
    }

    #[test]
    fn scroll_clip_removes_only_the_recorded_pixel_rows() {
        let placement = ResampledPlacement {
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 6,
            },
            pixels: (1..=6).flat_map(|value| [value, 0, 0, 255]).collect(),
        };
        let geometry = PlacementGeometry {
            anchor: CellAnchor::default(),
            row_offset: 0,
            source: SourceRect::default(),
            cell_offset: CellPixelOffset::default(),
            columns: Some(1),
            rows: Some(3),
            sizing: PlacementSizing::FitBox,
            clip_top_rows: 1,
            clip_bottom_rows: 1,
            z_index: 0,
            cursor_stays: false,
        };
        let cell = CellPixelSize::new(1, 2).unwrap();
        let viewport = PixelSize {
            width: 1,
            height: 8,
        };
        let values = |clipped: ClippedPlacement| {
            clipped
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|rgba| rgba[0])
                .collect::<Vec<_>>()
        };
        let both = placement
            .clip_to_viewport_with_scroll_clip(geometry, cell, viewport)
            .unwrap()
            .unwrap();
        assert_eq!(both.destination.y, 2);
        assert_eq!(both.destination.height, 2);
        assert_eq!(values(both), [3, 4]);
        assert_eq!(
            visible_placement_region(placement.destination, geometry, cell, viewport),
            Ok(Some((
                PixelRect {
                    x: 0,
                    y: 2,
                    width: 1,
                    height: 2,
                },
                PixelRect {
                    x: 0,
                    y: 2,
                    width: 1,
                    height: 2,
                },
            )))
        );
        let top_only = placement
            .clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_bottom_rows: 0,
                    ..geometry
                },
                cell,
                viewport,
            )
            .unwrap()
            .unwrap();
        assert_eq!(values(top_only), [3, 4, 5, 6]);
        let bottom_only = placement
            .clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_top_rows: 0,
                    ..geometry
                },
                cell,
                viewport,
            )
            .unwrap()
            .unwrap();
        assert_eq!(values(bottom_only), [1, 2, 3, 4]);

        // With no bottom clip, a start-cell Y offset may still protrude past
        // the nominal placement rows without being cut off.
        let shifted = ResampledPlacement {
            destination: PixelRect {
                y: 1,
                ..placement.destination
            },
            pixels: placement.pixels.clone(),
        };
        let top_only = shifted
            .clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_bottom_rows: 0,
                    ..geometry
                },
                cell,
                viewport,
            )
            .unwrap()
            .unwrap();
        assert_eq!(top_only.destination.y, 2);
        assert_eq!(values(top_only), [2, 3, 4, 5, 6]);
    }

    #[test]
    fn scroll_clip_rejects_unknown_geometry_and_full_clips() {
        let placement = ResampledPlacement {
            destination: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            pixels: vec![1, 2, 3, 4],
        };
        let geometry = PlacementGeometry {
            anchor: CellAnchor::default(),
            row_offset: 0,
            source: SourceRect::default(),
            cell_offset: CellPixelOffset::default(),
            columns: None,
            rows: None,
            sizing: PlacementSizing::Natural,
            clip_top_rows: 1,
            clip_bottom_rows: 0,
            z_index: 0,
            cursor_stays: false,
        };
        let cell = CellPixelSize::new(1, 1).unwrap();
        let viewport = PixelSize {
            width: 1,
            height: 1,
        };
        assert_eq!(
            placement.clip_to_viewport_with_scroll_clip(geometry, cell, viewport),
            Err(ClipError::InvalidGeometry)
        );
        assert_eq!(
            placement.clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    rows: Some(1),
                    ..geometry
                },
                cell,
                viewport,
            ),
            Ok(None)
        );
        assert_eq!(
            ResampledPlacement {
                pixels: vec![1, 2, 3],
                ..placement
            }
            .clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    rows: Some(1),
                    ..geometry
                },
                cell,
                viewport,
            ),
            Err(ClipError::InvalidPixels)
        );
        assert_eq!(
            placement.clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_top_rows: 0,
                    row_offset: i64::MAX,
                    ..geometry
                },
                cell,
                viewport,
            ),
            Ok(None)
        );
        assert_eq!(
            placement.clip_to_viewport_with_scroll_clip(
                PlacementGeometry {
                    clip_top_rows: 0,
                    row_offset: i64::MAX,
                    ..geometry
                },
                CellPixelSize::new(1, 2).unwrap(),
                viewport,
            ),
            Err(ClipError::InvalidGeometry)
        );
    }
}
