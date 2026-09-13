//! Binary pane geometry, independent of PTY ownership and physical rendering.

use std::io;

/// Bounds recursion, geometry storage and future per-window PTY ownership.
pub const MAX_PANES: usize = 64;

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
    /// First pane left, new pane right; reserve one separator column.
    Columns,
    /// First pane above, new pane below; reserve one separator row.
    Rows,
}

/// Zero-based coordinates relative to the window's content area, excluding its bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub row: u16,
    pub column: u16,
    pub rows: u16,
    pub columns: u16,
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
            } => {
                let (ar, ac) = first.minimum();
                let (br, bc) = second.minimum();
                match axis {
                    SplitAxis::Columns => (ar.max(br), ac + 1 + bc),
                    SplitAxis::Rows => (ar + 1 + br, ac.max(bc)),
                }
            }
        }
    }

    fn split(&mut self, target: PaneId, axis: SplitAxis, new: PaneId) -> bool {
        match self {
            Self::Pane(id) if *id == target => {
                *self = Self::Split {
                    axis,
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

    fn place(&self, rect: Rect, geometry: &mut Geometry) {
        match self {
            Self::Pane(id) => geometry.panes.push((*id, rect)),
            Self::Split {
                axis,
                first,
                second,
            } => {
                let (ar, ac) = first.minimum();
                let (br, bc) = second.minimum();
                let mut a = rect;
                let mut b = rect;
                let mut separator = rect;
                match axis {
                    SplitAxis::Columns => {
                        let available = rect.columns - 1;
                        a.columns = (available / 2).clamp(ac, available - bc);
                        separator.column += a.columns;
                        separator.columns = 1;
                        b.column += a.columns + 1;
                        b.columns = available - a.columns;
                    }
                    SplitAxis::Rows => {
                        let available = rect.rows - 1;
                        a.rows = (available / 2).clamp(ar, available - br);
                        separator.row += a.rows;
                        separator.rows = 1;
                        b.row += a.rows + 1;
                        b.rows = available - a.rows;
                    }
                }
                geometry.separators.push(separator);
                first.place(a, geometry);
                second.place(b, geometry);
            }
        }
    }
}

/// A nonempty split tree. New panes become active; resize preserves IDs and focus.
/// Each leaf requires one cell in both dimensions. Splits prefer equal halves,
/// assigning an odd spare cell to the second subtree, subject to subtree minima.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: Node,
    rows: u16,
    columns: u16,
    active: PaneId,
    next_id: u64,
    count: usize,
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

    pub fn geometry(&self) -> Geometry {
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

    pub fn select(&mut self, id: PaneId) -> io::Result<()> {
        if !self.geometry().panes.iter().any(|(pane, _)| *pane == id) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "unknown pane ID"));
        }
        self.active = id;
        Ok(())
    }

    /// Split only if the active rectangle can contain two cells plus a separator.
    /// Rejected requests leave IDs, focus, dimensions and the entire tree unchanged.
    pub fn split_active(&mut self, axis: SplitAxis) -> io::Result<PaneId> {
        if self.count == MAX_PANES {
            return Err(invalid("pane limit reached"));
        }
        let rect = self
            .geometry()
            .panes
            .into_iter()
            .find(|(id, _)| *id == self.active)
            .expect("active pane exists")
            .1;
        let extent = match axis {
            SplitAxis::Columns => rect.columns,
            SplitAxis::Rows => rect.rows,
        };
        if extent < 3 {
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
        Ok(id)
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

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
