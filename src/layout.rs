//! Binary pane geometry, independent of PTY ownership and physical rendering.

use std::io;

/// Bounds recursion, geometry storage and future per-window PTY ownership.
pub const MAX_PANES: usize = 64;

/// Each side of a split owns one cell of the gap for its pane border.
const SPLIT_BORDER_CELLS: u16 = 2;

/// Stable within one Layout; independent of pane coordinates and traversal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(u64);

impl PaneId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    /// First pane left, new pane right; reserve one border column per pane.
    Columns,
    /// First pane above, new pane below; reserve one border row per pane.
    Rows,
}

/// Geometric focus direction. Selection does not wrap at the content-area edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Zero-based coordinates relative to the window's content area, excluding its bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub row: u16,
    pub column: u16,
    pub rows: u16,
    pub columns: u16,
}

impl Rect {
    fn focus_score(
        self,
        other: Self,
        direction: Direction,
    ) -> Option<(u32, u32, std::cmp::Reverse<u32>, u32)> {
        let (start, length, cross, span, target, target_length, target_cross, target_span, forward) =
            match direction {
                Direction::Left | Direction::Right => (
                    self.column,
                    self.columns,
                    self.row,
                    self.rows,
                    other.column,
                    other.columns,
                    other.row,
                    other.rows,
                    direction == Direction::Right,
                ),
                Direction::Up | Direction::Down => (
                    self.row,
                    self.rows,
                    self.column,
                    self.columns,
                    other.row,
                    other.rows,
                    other.column,
                    other.columns,
                    direction == Direction::Down,
                ),
            };
        let start = u32::from(start);
        let end = start + u32::from(length);
        let target = u32::from(target);
        let target_end = target + u32::from(target_length);
        let gap = if forward {
            target.checked_sub(end)?
        } else {
            start.checked_sub(target_end)?
        };
        let cross = u32::from(cross);
        let cross_end = cross + u32::from(span);
        let target_cross = u32::from(target_cross);
        let target_cross_end = target_cross + u32::from(target_span);
        let overlap = cross_end
            .min(target_cross_end)
            .checked_sub(cross.max(target_cross))?;
        if overlap == 0 {
            return None;
        }
        let center_distance = (cross + cross_end).abs_diff(target_cross + target_cross_end);
        // Prefer aligned top/left edges before area: an extra row or column in
        // the second half of a split must not steal focus from the aligned half.
        let edge_distance = cross.abs_diff(target_cross);
        Some((
            gap,
            edge_distance,
            std::cmp::Reverse(overlap),
            center_distance,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    pub panes: Vec<(PaneId, Rect)>,
    pub separators: Vec<Rect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Pane(PaneId),
    Split {
        axis: SplitAxis,
        // Preferred first-child fraction of the space excluding the separator.
        share: (u16, u16),
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    fn minimum(&self) -> (u16, u16) {
        match self {
            Self::Pane(_) => (1, 1),
            Self::Split {
                axis,
                first,
                second,
                ..
            } => {
                let (ar, ac) = first.minimum();
                let (br, bc) = second.minimum();
                match axis {
                    SplitAxis::Columns => (ar.max(br), ac + SPLIT_BORDER_CELLS + bc),
                    SplitAxis::Rows => (ar + SPLIT_BORDER_CELLS + br, ac.max(bc)),
                }
            }
        }
    }

    fn split(&mut self, target: PaneId, axis: SplitAxis, new: PaneId) -> bool {
        match self {
            Self::Pane(id) if *id == target => {
                *self = Self::Split {
                    axis,
                    share: (1, 2),
                    first: Box::new(Self::Pane(target)),
                    second: Box::new(Self::Pane(new)),
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                first.split(target, axis, new) || second.split(target, axis, new)
            }
        }
    }

    // Remove a child leaf and promote its sibling into the parent's position.
    // The temporary leaf is immediately dropped with the old parent; no new allocation.
    fn remove(&mut self, target: PaneId) -> bool {
        match self {
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                if matches!(first.as_ref(), Self::Pane(id) if *id == target) {
                    *self = std::mem::replace(second.as_mut(), Self::Pane(target));
                    true
                } else if matches!(second.as_ref(), Self::Pane(id) if *id == target) {
                    *self = std::mem::replace(first.as_mut(), Self::Pane(target));
                    true
                } else {
                    first.remove(target) || second.remove(target)
                }
            }
        }
    }

    // Exchange leaf identities, leaving split axes, ratios and rectangles intact.
    fn exchange(&mut self, a: PaneId, b: PaneId) {
        match self {
            Self::Pane(id) if *id == a => *id = b,
            Self::Pane(id) if *id == b => *id = a,
            Self::Pane(_) => {}
            Self::Split { first, second, .. } => {
                first.exchange(a, b);
                second.exchange(a, b);
            }
        }
    }

    fn parent_axis(&self, target: PaneId) -> Option<SplitAxis> {
        match self {
            Self::Pane(_) => None,
            Self::Split {
                axis,
                first,
                second,
                ..
            } => {
                if matches!(first.as_ref(), Self::Pane(id) if *id == target)
                    || matches!(second.as_ref(), Self::Pane(id) if *id == target)
                {
                    Some(*axis)
                } else {
                    first
                        .parent_axis(target)
                        .or_else(|| second.parent_axis(target))
                }
            }
        }
    }

    fn contains(&self, target: PaneId) -> bool {
        match self {
            Self::Pane(id) => *id == target,
            Self::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }

    // Some(false) means the nearest matching separator is already at its limit;
    // do not fall through to an unrelated outer separator in that case.
    fn adjust(&mut self, target: PaneId, rect: Rect, axis: SplitAxis, delta: i32) -> Option<bool> {
        let Self::Split {
            axis: split_axis,
            share,
            first,
            second,
        } = self
        else {
            return None;
        };
        let (a, b, _) = split_rects(*split_axis, *share, first.minimum(), second.minimum(), rect);
        let result = if first.contains(target) {
            first.adjust(target, a, axis, delta)
        } else {
            second.adjust(target, b, axis, delta)
        };
        if result.is_some() || *split_axis != axis {
            return result;
        }
        Some(adjust_share(
            *split_axis,
            share,
            first.minimum(),
            second.minimum(),
            rect,
            delta,
        ))
    }

    fn adjust_separator(&mut self, remaining: &mut usize, rect: Rect, delta: i32) -> Option<bool> {
        let Self::Split {
            axis,
            share,
            first,
            second,
        } = self
        else {
            return None;
        };
        let (a, b, _) = split_rects(*axis, *share, first.minimum(), second.minimum(), rect);
        if *remaining == 0 {
            return Some(adjust_share(
                *axis,
                share,
                first.minimum(),
                second.minimum(),
                rect,
                delta,
            ));
        }
        *remaining -= 1;
        first
            .adjust_separator(remaining, a, delta)
            .or_else(|| second.adjust_separator(remaining, b, delta))
    }

    fn separator_hitboxes(&self, rect: Rect, hitboxes: &mut Vec<(usize, SplitAxis, Rect)>) {
        let Self::Split {
            axis,
            share,
            first,
            second,
        } = self
        else {
            return;
        };
        let (a, b, separator) = split_rects(*axis, *share, first.minimum(), second.minimum(), rect);
        hitboxes.push((hitboxes.len(), *axis, separator));
        first.separator_hitboxes(a, hitboxes);
        second.separator_hitboxes(b, hitboxes);
    }

    fn place(&self, rect: Rect, geometry: &mut Geometry) {
        match self {
            Self::Pane(id) => geometry.panes.push((*id, rect)),
            Self::Split {
                axis,
                share,
                first,
                second,
            } => {
                let (a, b, separator) =
                    split_rects(*axis, *share, first.minimum(), second.minimum(), rect);
                geometry.separators.push(separator);
                first.place(a, geometry);
                second.place(b, geometry);
            }
        }
    }
}

/// A nonempty split tree. New panes become active; resize preserves IDs and focus.
/// Each leaf requires one cell in both dimensions. New splits prefer equal halves,
/// assigning an odd spare cell to the second subtree, subject to subtree minima.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: Node,
    rows: u16,
    columns: u16,
    active: PaneId,
    next_id: u64,
    count: usize,
    zoomed: bool,
}

impl Layout {
    pub fn new(rows: u16, columns: u16) -> io::Result<Self> {
        if rows == 0 || columns == 0 {
            return Err(invalid("layout dimensions must be nonzero"));
        }
        Ok(Self {
            root: Node::Pane(PaneId(0)),
            rows,
            columns,
            active: PaneId(0),
            next_id: 1,
            count: 1,
            zoomed: false,
        })
    }

    pub fn active(&self) -> PaneId {
        self.active
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (self.rows, self.columns)
    }

    pub fn minimum_size(&self) -> (u16, u16) {
        self.root.minimum()
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Toggle the active pane's full-area view, returning the new zoom state.
    /// A single-pane layout remains unzoomed because it already fills the area.
    pub fn toggle_zoom(&mut self) -> bool {
        self.zoomed = self.count > 1 && !self.zoomed;
        self.zoomed
    }

    /// Visible panes and separators. Zoom shows only the active pane without separators.
    pub fn geometry(&self) -> Geometry {
        if self.zoomed {
            Geometry {
                panes: vec![(
                    self.active,
                    Rect {
                        row: 0,
                        column: 0,
                        rows: self.rows,
                        columns: self.columns,
                    },
                )],
                separators: Vec::new(),
            }
        } else {
            self.tiled_geometry()
        }
    }

    /// Full underlying split geometry, including panes hidden by zoom.
    pub fn tiled_geometry(&self) -> Geometry {
        let mut geometry = Geometry {
            panes: Vec::with_capacity(self.count),
            separators: Vec::with_capacity(self.count - 1),
        };
        self.root.place(
            Rect {
                row: 0,
                column: 0,
                rows: self.rows,
                columns: self.columns,
            },
            &mut geometry,
        );
        geometry
    }

    /// Visible PTY rectangles inside the pane frame. Each two-cell internal gap
    /// already provides one border cell per pane; only panes touching the canvas
    /// edge lose a cell to the outer border. Very small dimensions omit that pair.
    pub fn content_geometry(&self) -> Geometry {
        self.content_geometry_from(self.geometry())
    }

    /// Full underlying PTY geometry, including panes hidden by zoom.
    pub fn tiled_content_geometry(&self) -> Geometry {
        self.content_geometry_from(self.tiled_geometry())
    }

    fn content_geometry_from(&self, mut geometry: Geometry) -> Geometry {
        let horizontal_border = self.columns >= 3;
        let vertical_border = self.rows >= 3;
        for (_, rect) in &mut geometry.panes {
            if vertical_border && rect.row == 0 {
                rect.row += 1;
                rect.rows -= 1;
            }
            if vertical_border && rect.row + rect.rows == self.rows {
                rect.rows -= 1;
            }
            if horizontal_border && rect.column == 0 {
                rect.column += 1;
                rect.columns -= 1;
            }
            if horizontal_border && rect.column + rect.columns == self.columns {
                rect.columns -= 1;
            }
        }
        geometry
    }

    pub fn select(&mut self, id: PaneId) -> io::Result<()> {
        if !self
            .tiled_geometry()
            .panes
            .iter()
            .any(|(pane, _)| *pane == id)
        {
            return Err(io::Error::new(io::ErrorKind::NotFound, "unknown pane ID"));
        }
        self.active = id;
        Ok(())
    }

    /// Select a pane in the requested direction using the underlying tiled geometry.
    /// Require positive perpendicular overlap, then prefer the smallest edge gap,
    /// nearest perpendicular starting edge, largest overlap, nearest perpendicular
    /// center and finally traversal order.
    /// No candidate leaves the complete layout unchanged. Zoom follows the selection.
    pub fn select_direction(&mut self, direction: Direction) -> Option<PaneId> {
        let panes = self.tiled_geometry().panes;
        let source = panes
            .iter()
            .find(|(id, _)| *id == self.active)
            .expect("active pane exists")
            .1;
        let target = panes
            .iter()
            .enumerate()
            .filter(|(_, (id, _))| *id != self.active)
            .filter_map(|(index, (id, rect))| {
                source
                    .focus_score(*rect, direction)
                    .map(|score| ((score, index), *id))
            })
            .min_by_key(|(score, _)| *score)
            .map(|(_, id)| id)?;
        self.active = target;
        Some(target)
    }

    /// Exchange the active pane identity with its nearest geometric neighbor.
    /// Focus follows the active identity; zoomed or edge moves leave the layout alone.
    pub fn move_active(&mut self, direction: Direction) -> bool {
        if self.zoomed {
            return false;
        }
        let panes = self.tiled_geometry().panes;
        let source = panes
            .iter()
            .find(|(id, _)| *id == self.active)
            .expect("active pane exists")
            .1;
        let target = panes
            .iter()
            .enumerate()
            .filter(|(_, (id, _))| *id != self.active)
            .filter_map(|(index, (id, rect))| {
                source
                    .focus_score(*rect, direction)
                    .map(|score| ((score, index), *id))
            })
            .min_by_key(|(score, _)| *score)
            .map(|(_, id)| id);
        if let Some(target) = target {
            self.root.exchange(self.active, target);
            true
        } else {
            false
        }
    }

    /// Exchange the active pane with its traversal successor, wrapping at the end.
    /// Focus follows the same pane identity. Single-pane and zoomed layouts are no-ops.
    pub fn swap_active_next(&mut self) -> bool {
        self.swap_active(false)
    }

    /// Exchange with the traversal predecessor, wrapping at the beginning.
    pub fn swap_active_previous(&mut self) -> bool {
        self.swap_active(true)
    }

    fn swap_active(&mut self, backwards: bool) -> bool {
        if self.count == 1 || self.zoomed {
            return false;
        }
        let panes = self.tiled_geometry().panes;
        let index = panes.iter().position(|(id, _)| *id == self.active).unwrap();
        let target = if backwards {
            (index + self.count - 1) % self.count
        } else {
            (index + 1) % self.count
        };
        self.root.exchange(self.active, panes[target].0);
        true
    }

    pub(crate) fn restore_closed(
        &self,
        before: &Self,
        after: &Self,
        id: PaneId,
    ) -> io::Result<(Self, PaneId)> {
        if self.root == after.root {
            let mut restored = before.clone();
            restored.resize(self.rows, self.columns)?;
            restored.next_id = restored.next_id.max(self.next_id);
            restored.active = id;
            restored.zoomed = false;
            Ok((restored, id))
        } else {
            // Keep newer layout edits; insert beside current focus on the old axis.
            let mut restored = self.clone();
            let id =
                restored.split_active(before.root.parent_axis(id).unwrap_or(SplitAxis::Columns))?;
            Ok((restored, id))
        }
    }

    /// Move the nearest ancestor separator on the requested axis by one cell.
    /// Directions describe separator movement, independently of which child is
    /// active. Preserve focus and IDs. Zoom, no matching split or a minimum-size
    /// boundary leaves the complete layout unchanged and returns false.
    pub fn resize_active(&mut self, direction: Direction) -> bool {
        if self.zoomed {
            return false;
        }
        let (axis, delta) = match direction {
            Direction::Left => (SplitAxis::Columns, -1),
            Direction::Right => (SplitAxis::Columns, 1),
            Direction::Up => (SplitAxis::Rows, -1),
            Direction::Down => (SplitAxis::Rows, 1),
        };
        self.root
            .adjust(
                self.active,
                Rect {
                    row: 0,
                    column: 0,
                    rows: self.rows,
                    columns: self.columns,
                },
                axis,
                delta,
            )
            .unwrap_or(false)
    }

    /// Move one visible separator by a signed number of cells. Separator indexes
    /// use the pre-order returned by `separator_hitboxes` and remain stable while
    /// only ratios change. Invalid indexes, zoom and size limits are no-ops.
    pub(crate) fn resize_separator(&mut self, index: usize, delta: i32) -> bool {
        if self.zoomed || delta == 0 {
            return false;
        }
        let mut remaining = index;
        self.root
            .adjust_separator(
                &mut remaining,
                Rect {
                    row: 0,
                    column: 0,
                    rows: self.rows,
                    columns: self.columns,
                },
                delta,
            )
            .unwrap_or(false)
    }

    pub(crate) fn separator_hitboxes(&self) -> Vec<(usize, SplitAxis, Rect)> {
        if self.zoomed {
            return Vec::new();
        }
        let mut hitboxes = Vec::with_capacity(self.count - 1);
        self.root.separator_hitboxes(
            Rect {
                row: 0,
                column: 0,
                rows: self.rows,
                columns: self.columns,
            },
            &mut hitboxes,
        );
        hitboxes
    }

    /// Split only if the active rectangle can contain content plus both pane borders.
    /// Rejected requests leave IDs, focus, dimensions and the entire tree unchanged.
    pub fn split_active(&mut self, axis: SplitAxis) -> io::Result<PaneId> {
        if self.count == MAX_PANES {
            return Err(invalid("pane limit reached"));
        }
        let rect = self
            .tiled_geometry()
            .panes
            .into_iter()
            .find(|(id, _)| *id == self.active)
            .expect("active pane exists")
            .1;
        let extent = match axis {
            SplitAxis::Columns => rect.columns,
            SplitAxis::Rows => rect.rows,
        };
        if extent < 2 + SPLIT_BORDER_CELLS {
            return Err(invalid("active pane has no space for this split"));
        }
        let next = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("pane IDs exhausted"))?;
        let id = PaneId(self.next_id);
        let replaced = self.root.split(self.active, axis, id);
        debug_assert!(replaced);
        self.active = id;
        self.next_id = next;
        self.count += 1;
        self.zoomed = false;
        Ok(id)
    }

    /// Remove a pane and promote its sibling subtree, returning the active pane.
    /// Closing the active pane selects its traversal successor, or predecessor at
    /// the end. Closing an inactive pane preserves focus. The last pane cannot be
    /// removed: the caller must close its owning window instead.
    pub fn close(&mut self, id: PaneId) -> io::Result<PaneId> {
        let panes = self.tiled_geometry().panes;
        let index = panes
            .iter()
            .position(|(pane, _)| *pane == id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown pane ID"))?;
        if self.count == 1 {
            return Err(invalid("cannot close the last pane in a layout"));
        }
        let next_active = if self.active == id {
            panes
                .get(index + 1)
                .unwrap_or(&panes[index.saturating_sub(1)])
                .0
        } else {
            self.active
        };
        let removed = self.root.remove(id);
        debug_assert!(removed);
        self.count -= 1;
        self.zoomed = false;
        self.active = next_active;
        Ok(self.active)
    }

    /// Reject sizes below the tree's minimum without mutating layout or focus.
    pub fn resize(&mut self, rows: u16, columns: u16) -> io::Result<()> {
        let (minimum_rows, minimum_columns) = self.minimum_size();
        if rows < minimum_rows || columns < minimum_columns {
            return Err(invalid("terminal is too small for the split layout"));
        }
        self.rows = rows;
        self.columns = columns;
        Ok(())
    }
}

// Use integer fractions so manual positions return exactly when outer dimensions
// return. Temporary minimum-size clamps do not overwrite the preferred ratio.
fn split_rects(
    axis: SplitAxis,
    share: (u16, u16),
    (ar, ac): (u16, u16),
    (br, bc): (u16, u16),
    rect: Rect,
) -> (Rect, Rect, Rect) {
    let mut a = rect;
    let mut b = rect;
    let mut separator = rect;
    let first_extent =
        |available: u16| (u32::from(available) * u32::from(share.0) / u32::from(share.1)) as u16;
    match axis {
        SplitAxis::Columns => {
            let available = rect.columns - SPLIT_BORDER_CELLS;
            a.columns = first_extent(available).clamp(ac, available - bc);
            separator.column += a.columns;
            separator.columns = SPLIT_BORDER_CELLS;
            b.column += a.columns + SPLIT_BORDER_CELLS;
            b.columns = available - a.columns;
        }
        SplitAxis::Rows => {
            let available = rect.rows - SPLIT_BORDER_CELLS;
            a.rows = first_extent(available).clamp(ar, available - br);
            separator.row += a.rows;
            separator.rows = SPLIT_BORDER_CELLS;
            b.row += a.rows + SPLIT_BORDER_CELLS;
            b.rows = available - a.rows;
        }
    }
    (a, b, separator)
}

fn adjust_share(
    axis: SplitAxis,
    share: &mut (u16, u16),
    first_minimum: (u16, u16),
    second_minimum: (u16, u16),
    rect: Rect,
    delta: i32,
) -> bool {
    let (extent, available, minimum, other_minimum) = match axis {
        SplitAxis::Columns => (
            split_rects(axis, *share, first_minimum, second_minimum, rect)
                .0
                .columns,
            rect.columns - SPLIT_BORDER_CELLS,
            first_minimum.1,
            second_minimum.1,
        ),
        SplitAxis::Rows => (
            split_rects(axis, *share, first_minimum, second_minimum, rect)
                .0
                .rows,
            rect.rows - SPLIT_BORDER_CELLS,
            first_minimum.0,
            second_minimum.0,
        ),
    };
    let next = (i32::from(extent) + delta)
        .clamp(i32::from(minimum), i32::from(available - other_minimum)) as u16;
    if next == extent {
        return false;
    }
    *share = (next, available);
    true
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_scoring_rejects_diagonals_and_corner_contact() {
        let source = Rect {
            row: 0,
            column: 0,
            rows: 2,
            columns: 2,
        };
        for other in [
            Rect {
                row: 3,
                column: 3,
                rows: 2,
                columns: 2,
            },
            Rect {
                row: 2,
                column: 2,
                rows: 2,
                columns: 2,
            },
        ] {
            for direction in [
                Direction::Left,
                Direction::Right,
                Direction::Up,
                Direction::Down,
            ] {
                assert_eq!(source.focus_score(other, direction), None);
            }
        }
    }

    #[test]
    fn unknown_selection_and_id_exhaustion_leave_layout_unchanged() {
        let mut layout = Layout::new(8, 8).unwrap();
        let before = layout.clone();
        assert_eq!(
            layout.select(PaneId(99)).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(layout, before);
        layout.next_id = u64::MAX;
        let before = layout.clone();
        assert!(layout.split_active(SplitAxis::Columns).is_err());
        assert_eq!(layout, before);
    }
    #[test]
    fn restoring_close_preserves_edits_and_retry_after_small_resize() {
        let mut layout = Layout::new(7, 11).unwrap();
        let closed = layout.split_active(SplitAxis::Columns).unwrap();
        layout.resize_active(Direction::Right);
        let before = layout.clone();
        layout.close(closed).unwrap();
        let after = layout.clone();
        layout.resize(1, 2).unwrap();
        let small = layout.clone();
        assert!(layout.restore_closed(&before, &after, closed).is_err());
        assert_eq!(layout, small);
        layout.resize(7, 11).unwrap();
        let (restored, id) = layout.restore_closed(&before, &after, closed).unwrap();
        assert_eq!(id, closed);
        assert_eq!(restored.geometry(), before.geometry());
        layout.split_active(SplitAxis::Rows).unwrap();
        let edited = layout.geometry();
        let (restored, id) = layout.restore_closed(&before, &after, closed).unwrap();
        assert_ne!(id, closed);
        assert_eq!(restored.geometry().panes.len(), 3);
        assert_eq!(restored.geometry().panes[0], edited.panes[0]);
        assert_eq!(restored.active(), id);
    }

    #[test]
    fn indexed_separator_resize_targets_the_clicked_nested_split() {
        let mut layout = Layout::new(11, 31).unwrap();
        let left = layout.active();
        layout.split_active(SplitAxis::Columns).unwrap();
        layout.select(left).unwrap();
        layout.split_active(SplitAxis::Columns).unwrap();
        let hitboxes = layout.separator_hitboxes();
        assert_eq!(hitboxes.len(), 2);
        assert_eq!(hitboxes[0].0, 0);
        assert_eq!(hitboxes[1].0, 1);
        assert_eq!(hitboxes[0].1, SplitAxis::Columns);
        assert_eq!(hitboxes[1].1, SplitAxis::Columns);

        let outer = layout.geometry().separators[0];
        assert!(layout.resize_separator(0, 3));
        assert_eq!(layout.geometry().separators[0].column, outer.column + 3);
        let outer = layout.geometry().separators[0];
        let inner = layout.geometry().separators[1];
        assert!(layout.resize_separator(1, -2));
        assert_eq!(layout.geometry().separators[0], outer);
        assert_eq!(layout.geometry().separators[1].column, inner.column - 2);

        assert!(!layout.resize_separator(99, 1));
        layout.toggle_zoom();
        assert!(!layout.resize_separator(0, 1));
        assert!(layout.separator_hitboxes().is_empty());
    }
}
