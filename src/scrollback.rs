//! Bounded physical rows from the primary screen, shared by screen snapshots.

use std::{collections::VecDeque, sync::Arc};

use crate::style::Cell;

pub const MAX_LINES: usize = 1_000;
pub const MAX_CELLS: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryRow {
    pub cells: Arc<[Cell]>,
    pub continued: bool,
    pub used: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scrollback {
    rows: Arc<VecDeque<HistoryRow>>,
    cells: usize,
    max_lines: usize,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self::new(MAX_LINES)
    }
}

impl Scrollback {
    pub fn new(max_lines: usize) -> Self {
        Self {
            rows: Arc::default(),
            cells: 0,
            max_lines,
        }
    }

    pub fn clear(&mut self) {
        Arc::make_mut(&mut self.rows).clear();
        self.cells = 0;
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn max_lines(&self) -> usize {
        self.max_lines
    }

    pub fn row(&self, index: usize) -> Option<&[Cell]> {
        self.rows.get(index).map(|row| row.cells.as_ref())
    }

    pub fn continued(&self, index: usize) -> Option<bool> {
        self.rows.get(index).map(|row| row.continued)
    }

    pub fn used(&self, index: usize) -> Option<usize> {
        self.rows.get(index).map(|row| row.used)
    }

    pub fn pop_newest(&mut self) -> Option<HistoryRow> {
        let row = Arc::make_mut(&mut self.rows).pop_back()?;
        self.cells -= row.cells.len();
        Some(row)
    }

    pub fn push(&mut self, row: &[Cell], continued: bool, used: usize) {
        if self.max_lines == 0 {
            self.clear();
            return;
        }
        // An oversized row cannot fit even on its own. Discard older history too,
        // so retained history never jumps across an unrecorded newer row.
        if row.len() > MAX_CELLS {
            self.clear();
            return;
        }
        let rows = Arc::make_mut(&mut self.rows);
        while rows.len() >= self.max_lines || self.cells + row.len() > MAX_CELLS {
            self.cells -= rows
                .pop_front()
                .expect("history exceeds its bound")
                .cells
                .len();
        }
        rows.push_back(HistoryRow {
            cells: Arc::from(row),
            continued,
            used: used.min(row.len()),
        });
        self.cells += row.len();
    }
}
