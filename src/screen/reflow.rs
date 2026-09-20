//! Primary-grid reflow. Packed rows stay sparse until bounded destination storage.
use super::{Cell, SavedCursor, Screen};
use crate::scrollback::Scrollback;

#[derive(Clone, Copy, Default)]
struct Position {
    row: usize,
    column: usize,
    pending: bool,
}

struct Row {
    cells: Vec<Cell>,
    continued: bool,
}

fn pack(
    cells: &[Cell],
    points: &[(usize, usize, bool)],
    width: usize,
    output: &mut Vec<Row>,
    mapped: &mut [Position; 2],
) {
    let mut row = Row {
        cells: Vec::new(),
        continued: false,
    };
    let mut positions = vec![Position::default(); cells.len() + 1];
    let mut index = 0;
    while index < cells.len() {
        let original_width = usize::from(cells[index].width.max(1));
        let glyph_width = original_width.min(width);
        if row.cells.len() + glyph_width > width {
            output.push(row);
            row = Row {
                cells: Vec::new(),
                continued: true,
            };
        }
        let position = Position {
            row: output.len(),
            column: row.cells.len(),
            pending: false,
        };
        positions[index] = position;
        let mut cell = cells[index].clone();
        if original_width > width {
            cell.character = '\u{fffd}';
            cell.width = 1;
        }
        row.cells.push(cell);
        if original_width == 2 && index + 1 < cells.len() {
            positions[index + 1] = Position {
                column: position.column + usize::from(glyph_width == 2),
                ..position
            };
            if glyph_width == 2 {
                row.cells.push(cells[index + 1].clone());
            }
        }
        index += original_width;
    }
    let end = Position {
        row: output.len(),
        column: row.cells.len().min(width - 1),
        pending: row.cells.len() == width,
    };
    positions[cells.len()] = if end.pending {
        end
    } else {
        Position {
            column: row.cells.len(),
            ..end
        }
    };
    output.push(row);
    let needs_empty = end.pending
        && points
            .iter()
            .any(|(_, offset, pending)| *offset == cells.len() && !pending);
    if needs_empty {
        output.push(Row {
            cells: Vec::new(),
            continued: true,
        });
    }
    for &(id, offset, pending) in points {
        mapped[id] = if offset == cells.len() && end.pending && !pending {
            Position {
                row: end.row + 1,
                column: 0,
                pending: false,
            }
        } else {
            positions[offset]
        };
    }
}

impl Screen {
    pub(super) fn reflow_primary_into(&self, resized: &mut Self) {
        let alternate = self.is_alternate();
        let (cells, used, continued, current, saved) = if let Some(current) = self.saved_main_cursor
        {
            (
                &self.inactive_cells,
                &self.inactive_used,
                &self.inactive_continued,
                current,
                self.inactive_saved_cursor,
            )
        } else {
            (
                &self.cells,
                &self.used,
                &self.continued,
                SavedCursor {
                    row: self.row,
                    column: self.column,
                    style: self.style,
                    wrap_pending: self.wrap_pending(),
                    origin_mode: self.origin_mode,
                    auto_wrap: self.auto_wrap,
                    character_sets: self.character_sets,
                },
                self.saved_cursor,
            )
        };
        // Ignore unused bottom padding, but preserve rows needed by either cursor.
        let last = used
            .iter()
            .rposition(|&used| used != 0)
            .unwrap_or(0)
            .max(current.row)
            .max(saved.map_or(0, |cursor| cursor.row));
        let history = self.scrollback.len();
        let mut output = Vec::new();
        let mut logical = Vec::new();
        let mut points = Vec::new();
        let mut mapped = [Position::default(); 2];
        for index in 0..history + last + 1 {
            let (source, extent, continuation) = if index < history {
                (
                    self.scrollback.row(index).unwrap(),
                    self.scrollback.used(index).unwrap(),
                    self.scrollback.continued(index).unwrap(),
                )
            } else {
                let row = index - history;
                (
                    &cells[row * self.columns..(row + 1) * self.columns],
                    used[row],
                    continued[row],
                )
            };
            if index != 0 && !continuation {
                pack(&logical, &points, resized.columns, &mut output, &mut mapped);
                logical.clear();
                points.clear();
            }
            let mut extent = extent;
            for (id, cursor) in [(0, Some(current)), (1, saved)] {
                if let Some(cursor) = cursor
                    && index == history + cursor.row
                {
                    let pending = cursor.wrap_pending && cursor.auto_wrap;
                    let offset = cursor.column + usize::from(pending);
                    extent = extent.max(offset);
                    points.push((id, logical.len() + offset, pending));
                }
            }
            logical.extend_from_slice(&source[..extent]);
        }
        pack(&logical, &points, resized.columns, &mut output, &mut mapped);
        let start = output.len().saturating_sub(resized.rows).min(mapped[0].row);
        let max_lines = self.scrollback.max_lines();
        let mut new_history = Scrollback::new(max_lines);
        let keep = max_lines.min(crate::scrollback::MAX_CELLS / resized.columns);
        let blank = Cell {
            style: crate::style::Style {
                background: current.style.background,
                ..Default::default()
            },
            ..Default::default()
        };
        for row in output.iter().take(start).skip(start.saturating_sub(keep)) {
            let mut full = vec![blank.clone(); resized.columns];
            full[..row.cells.len()].clone_from_slice(&row.cells);
            new_history.push(&full, row.continued, row.cells.len());
        }
        resized.scrollback = new_history;
        let (destination, lengths, flags) = if alternate {
            (
                &mut resized.inactive_cells,
                &mut resized.inactive_used,
                &mut resized.inactive_continued,
            )
        } else {
            (
                &mut resized.cells,
                &mut resized.used,
                &mut resized.continued,
            )
        };
        destination.fill(blank);
        lengths.fill(0);
        flags.fill(false);
        for (index, row) in output.iter().skip(start).take(resized.rows).enumerate() {
            let offset = index * resized.columns;
            destination[offset..offset + row.cells.len()].clone_from_slice(&row.cells);
            lengths[index] = row.cells.len();
            flags[index] = row.continued;
        }
        let translate = |mut cursor: SavedCursor, position: Position| {
            cursor.row = position.row.saturating_sub(start).min(resized.rows - 1);
            cursor.column = position.column;
            cursor.wrap_pending =
                position.pending && position.row >= start && position.row < start + resized.rows;
            cursor
        };
        let current = translate(current, mapped[0]);
        if alternate {
            resized.saved_main_cursor = Some(current);
            resized.inactive_saved_cursor = saved.map(|cursor| translate(cursor, mapped[1]));
        } else {
            resized.row = current.row;
            resized.column = current.column;
            resized.wrap_pending = current.wrap_pending;
            resized.saved_cursor = saved.map(|cursor| translate(cursor, mapped[1]));
        }
    }
}
