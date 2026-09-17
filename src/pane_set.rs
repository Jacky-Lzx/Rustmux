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

    /// Creation order, which can differ from layout traversal order after splitting or swapping.
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

    /// Adjust geometry without replacing contents; synchronize PTY sizes separately.
    pub fn resize_active(&mut self, direction: Direction) -> bool {
        self.layout.resize_active(direction)
    }

    pub(crate) fn resize_separator(&mut self, index: usize, delta: i32) -> bool {
        self.layout.resize_separator(index, delta)
    }

    /// Move the active identity to the next layout slot without replacing contents.
    pub fn swap_active_next(&mut self) -> bool {
        self.layout.swap_active_next()
    }

    /// Move the active identity to the previous layout slot without replacing contents.
    pub fn swap_active_previous(&mut self) -> bool {
        self.layout.swap_active_previous()
    }

    pub fn toggle_zoom(&mut self) -> bool {
        self.layout.toggle_zoom()
    }

    /// Geometry only; the caller remains responsible for resizing PTYs and screens.
    pub fn resize(&mut self, rows: u16, columns: u16) -> io::Result<()> {
        let mut candidate = self.layout.clone();
        candidate.resize(rows, columns)?;
        require_content_cells(&candidate)?;
        self.layout = candidate;
        Ok(())
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
        require_content_cells(&candidate)?;
        let rect = candidate
            .tiled_content_geometry()
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

    pub(crate) fn restore_with(
        &mut self,
        before: &Layout,
        after: &Layout,
        id: PaneId,
        create: impl FnOnce(Rect) -> io::Result<T>,
    ) -> io::Result<()> {
        let (layout, id) = self.layout.restore_closed(before, after, id)?;
        let rect = layout
            .tiled_content_geometry()
            .panes
            .into_iter()
            .find(|(pane, _)| *pane == id)
            .unwrap()
            .1;
        self.entries.try_reserve(1).map_err(io::Error::other)?;
        let content = create(rect)?;
        self.entries.push((id, content));
        self.layout = layout;
        Ok(())
    }

    // The owning Windows operation immediately removes an emptied source window.
    // Destination validation/reservation happens before the content is extracted.
    pub(crate) fn transfer_active_to(
        &mut self,
        target: &mut Self,
        axis: SplitAxis,
    ) -> io::Result<()> {
        target.split_with(axis, |_, _| {
            if self.entries.len() == 1 {
                Ok(self.entries.pop().unwrap().1)
            } else {
                self.close(self.layout.active())
            }
        })?;
        Ok(())
    }

    /// Prepare destination storage before removing the active pane from this set.
    pub(crate) fn detach_active(&mut self) -> io::Result<Option<Self>> {
        if self.entries.len() == 1 {
            return Ok(None);
        }
        let (rows, columns) = self.layout.dimensions();
        let layout = Layout::new(rows, columns)?;
        let mut entries = Vec::new();
        entries.try_reserve(1).map_err(io::Error::other)?;
        let content = self.close(self.layout.active())?;
        entries.push((layout.active(), content));
        Ok(Some(Self { layout, entries }))
    }

    pub(crate) fn into_single(mut self) -> T {
        assert_eq!(self.entries.len(), 1);
        self.entries.pop().unwrap().1
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

impl PaneSet<crate::pane::Pane> {
    /// Bring owned terminal screens and PTYs to the current layout's dimensions.
    /// The zoomed active pane uses the full visible rectangle; hidden panes keep
    /// their tiled sizes. Unchanged sizes are skipped, preserving output holds.
    /// All changed screens are prepared before any PTY ioctl. Commit failures can
    /// follow earlier commits: the caller must stop using this set and clean up,
    /// rather than render a potentially inconsistent layout. Layout is not rolled back.
    pub fn synchronize_sizes(&mut self) -> io::Result<()> {
        let (rows, columns) = self.layout.dimensions();
        if usize::from(rows) * usize::from(columns) > crate::pane::MAX_CELLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pane layout exceeds cell limit",
            ));
        }
        let mut sizes = self.layout.tiled_content_geometry().panes;
        if self.layout.is_zoomed() {
            let active = self.layout.active();
            let visible = self.layout.content_geometry().panes[0].1;
            let (_, rect) = sizes
                .iter_mut()
                .find(|(id, _)| *id == active)
                .expect("active pane exists");
            *rect = visible;
        }
        // Refresh child status before preparing; an exited child needs no ioctl.
        for (_, pane) in &mut self.entries {
            let (shell, _, _, state) = pane.parts_mut();
            if state.status.is_none() {
                state.status = shell.try_wait()?;
            }
        }
        let mut prepared = Vec::with_capacity(self.entries.len());
        for (id, pane) in &mut self.entries {
            let (_, rect) = sizes
                .iter()
                .find(|(candidate, _)| candidate == id)
                .expect("every content has a layout leaf");
            if pane.screen().dimensions() != (usize::from(rect.rows), usize::from(rect.columns)) {
                prepared.push(pane.prepare_resize(rect.rows, rect.columns)?);
            }
        }
        for resize in prepared {
            resize.commit()?;
        }
        Ok(())
    }
}

fn require_content_cells(layout: &Layout) -> io::Result<()> {
    if layout
        .tiled_content_geometry()
        .panes
        .iter()
        .any(|(_, rect)| rect.rows == 0 || rect.columns == 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pane has no space inside its border",
        ));
    }
    Ok(())
}
