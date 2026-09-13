//! Pane contents keyed by stable layout identity, independent of process I/O.

use crate::layout::{Direction, Layout, PaneId, Rect, SplitAxis};
use std::io;

/// Owns exactly one content value per layout leaf, without requiring T: Clone.
/// Geometry operations never resize or otherwise mutate those content values.
#[derive(Debug)]
pub struct PaneSet<T> {
    layout: Layout,
    entries: Vec<(PaneId, T)>,
}

impl<T> PaneSet<T> {
    /// Own an initial content value. Invalid dimensions drop the supplied value.
    pub fn new(rows: u16, columns: u16, content: T) -> io::Result<Self> {
        let layout = Layout::new(rows, columns)?;
        let id = layout.active();
        Ok(Self {
            layout,
            entries: vec![(id, content)],
        })
    }

    /// Read-only access prevents creating layout leaves without owned contents.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Creation order, which can differ from layout traversal order after splitting.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (PaneId, &T)> {
        self.entries.iter().map(|(id, content)| (*id, content))
    }

    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (PaneId, &mut T)> {
        self.entries.iter_mut().map(|(id, content)| (*id, content))
    }

    pub fn get(&self, id: PaneId) -> Option<&T> {
        self.entries
            .iter()
            .find(|(pane, _)| *pane == id)
            .map(|(_, content)| content)
    }

    pub fn get_mut(&mut self, id: PaneId) -> Option<&mut T> {
        self.entries
            .iter_mut()
            .find(|(pane, _)| *pane == id)
            .map(|(_, content)| content)
    }

    pub fn active(&self) -> &T {
        self.get(self.layout.active())
            .expect("every layout leaf owns contents")
    }

    pub fn active_mut(&mut self) -> &mut T {
        self.get_mut(self.layout.active())
            .expect("every layout leaf owns contents")
    }

    pub fn select(&mut self, id: PaneId) -> io::Result<()> {
        self.layout.select(id)
    }

    /// Move focus geometrically without moving or recreating owned contents.
    pub fn select_direction(&mut self, direction: Direction) -> Option<PaneId> {
        self.layout.select_direction(direction)
    }

    pub fn toggle_zoom(&mut self) -> bool {
        self.layout.toggle_zoom()
    }

    /// Geometry only; the caller remains responsible for resizing PTYs and screens.
    pub fn resize(&mut self, rows: u16, columns: u16) -> io::Result<()> {
        self.layout.resize(rows, columns)
    }

    /// Validate a prospective split before invoking the content factory exactly once.
    /// The factory receives the new ID and its post-split tiled rectangle. On error,
    /// existing contents, layout, focus and zoom remain unchanged; no ID is consumed.
    /// External side effects performed by the factory cannot be rolled back here.
    pub fn split_with(
        &mut self,
        axis: SplitAxis,
        create: impl FnOnce(PaneId, Rect) -> io::Result<T>,
    ) -> io::Result<PaneId> {
        let mut candidate = self.layout.clone();
        let id = candidate.split_active(axis)?;
        let rect = candidate
            .tiled_geometry()
            .panes
            .into_iter()
            .find(|(pane, _)| *pane == id)
            .expect("new pane exists")
            .1;
        // Reserve storage before the factory can start a process or acquire resources.
        self.entries.try_reserve(1).map_err(io::Error::other)?;
        let content = create(id, rect)?;
        self.entries.push((id, content));
        self.layout = candidate;
        Ok(id)
    }

    /// Remove a layout leaf and return its owned contents without dropping them.
    /// Unknown IDs and the last pane are rejected without changing either collection.
    /// The caller decides when to clean up a returned PTY or transfer other resources.
    pub fn close(&mut self, id: PaneId) -> io::Result<T> {
        let index = self
            .entries
            .iter()
            .position(|(pane, _)| *pane == id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown pane ID"))?;
        self.layout.close(id)?;
        Ok(self.entries.remove(index).1)
    }
}
