//! Ordered window ownership and focus, independent of PTY polling and rendering.

use std::io;

/// Stable within one Windows collection. IDs are never reused by that collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(u64);

impl WindowId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub struct Window<T> {
    id: WindowId,
    name: String,
    content: T,
}

impl<T> Window<T> {
    pub fn id(&self) -> WindowId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn content(&self) -> &T {
        &self.content
    }

    pub fn content_mut(&mut self) -> &mut T {
        &mut self.content
    }

    pub fn into_content(self) -> T {
        self.content
    }
}

/// Owns window contents in creation order; no Clone bound or process operations.
/// Empty collections have no focus. New windows become active. Closing the active
/// window selects its successor, or its predecessor when removing the last entry.
#[derive(Debug)]
pub struct Windows<T> {
    entries: Vec<Window<T>>,
    active: usize,
    next_id: Option<u64>,
}

impl<T> Default for Windows<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            active: 0,
            next_id: Some(0),
        }
    }
}

impl<T> Windows<T> {
    /// Insert an already constructed content value and activate it. On ID
    /// exhaustion the supplied value is dropped and the collection is unchanged.
    pub fn create(&mut self, name: String, content: T) -> io::Result<WindowId> {
        let value = self
            .next_id
            .ok_or_else(|| io::Error::other("window IDs exhausted"))?;
        let id = WindowId(value);
        self.entries.push(Window { id, name, content });
        self.next_id = value.checked_add(1);
        self.active = self.entries.len() - 1;
        Ok(id)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Window<T>> {
        self.entries.iter()
    }

    pub fn active(&self) -> Option<&Window<T>> {
        self.entries.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Window<T>> {
        self.entries.get_mut(self.active)
    }

    pub fn get(&self, id: WindowId) -> Option<&Window<T>> {
        self.entries.iter().find(|window| window.id == id)
    }

    /// Allows background output to update an inactive window without focusing it.
    pub fn get_mut(&mut self, id: WindowId) -> Option<&mut Window<T>> {
        self.entries.iter_mut().find(|window| window.id == id)
    }

    pub fn select(&mut self, id: WindowId) -> io::Result<()> {
        self.active = self.index(id)?;
        Ok(())
    }

    pub fn select_next(&mut self) -> Option<WindowId> {
        if self.entries.is_empty() {
            return None;
        }
        self.active = if self.active + 1 == self.entries.len() {
            0
        } else {
            self.active + 1
        };
        Some(self.entries[self.active].id)
    }

    pub fn select_previous(&mut self) -> Option<WindowId> {
        if self.entries.is_empty() {
            return None;
        }
        self.active = if self.active == 0 {
            self.entries.len() - 1
        } else {
            self.active - 1
        };
        Some(self.entries[self.active].id)
    }

    /// Names are opaque metadata, including empty or duplicate names. A UI must
    /// escape controls before displaying them; this model never emits names.
    pub fn rename(&mut self, id: WindowId, name: String) -> io::Result<()> {
        let index = self.index(id)?;
        self.entries[index].name = name;
        Ok(())
    }

    /// Return ownership to the caller, which decides how to stop/reap a process.
    /// Removing an inactive entry preserves the active window's identity.
    pub fn close(&mut self, id: WindowId) -> io::Result<Window<T>> {
        let index = self.index(id)?;
        let removed = self.entries.remove(index);
        if index < self.active {
            self.active -= 1;
        } else if self.active == self.entries.len() {
            self.active = self.active.saturating_sub(1);
        }
        Ok(removed)
    }

    fn index(&self, id: WindowId) -> io::Result<usize> {
        self.entries
            .iter()
            .position(|window| window.id == id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown window ID"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_ids_do_not_wrap_or_change_focus() {
        let mut windows = Windows {
            next_id: Some(u64::MAX),
            ..Windows::default()
        };
        let last = windows.create(String::new(), 1).unwrap();
        assert_eq!(last.get(), u64::MAX);
        assert!(windows.create(String::new(), 2).is_err());
        assert_eq!(windows.active().unwrap().id(), last);
        assert_eq!(windows.iter().len(), 1);
    }
}
