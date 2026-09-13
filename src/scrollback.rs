//! Bounded physical rows from the primary screen, shared by screen snapshots.

use std::{collections::VecDeque, sync::Arc};

use crate::style::Cell;

pub const MAX_LINES: usize = 1_000;
pub const MAX_CELLS: usize = 65_536;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Scrollback {
    rows: Arc<VecDeque<(Arc<[Cell]>, bool)>>,
    cells: usize,
}

impl Scrollback {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, index: usize) -> Option<&[Cell]> {
        self.rows.get(index).map(|(cells, _)| cells.as_ref())
    }

    pub fn continued(&self, index: usize) -> Option<bool> {
        self.rows.get(index).map(|(_, continued)| *continued)
    }

    pub fn pop_newest(&mut self) -> Option<(Arc<[Cell]>, bool)> {
        let row = Arc::make_mut(&mut self.rows).pop_back()?;
        self.cells -= row.0.len();
        Some(row)
    }

    pub fn push(&mut self, row: &[Cell], continued: bool) {
        // An oversized row cannot fit even on its own. Discard older history too,
        // so retained history never jumps across an unrecorded newer row.
        if row.len() > MAX_CELLS {
            *self = Self::default();
            return;
        }
        let rows = Arc::make_mut(&mut self.rows);
        while rows.len() >= MAX_LINES || self.cells + row.len() > MAX_CELLS {
            self.cells -= rows.pop_front().expect("history exceeds its bound").0.len();
        }
        rows.push_back((Arc::from(row), continued));
        self.cells += row.len();
    }
}
