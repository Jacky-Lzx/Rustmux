//! Compose validated pane screens into one content-area frame for the renderer.

use crate::{
    chrome::separator_style,
    layout::{Layout, PaneId, Rect},
    pane::MAX_CELLS,
    screen::Screen,
    style::Cell,
};
use std::io;
use std::ops::BitOrAssign;

#[derive(Clone, Copy, PartialEq, Eq)]
struct LineMask(u8);

impl LineMask {
    const EMPTY: Self = Self(0);
    const NORTH: Self = Self(1);
    const SOUTH: Self = Self(2);
    const WEST: Self = Self(4);
    const EAST: Self = Self(8);
    const VERTICAL: Self = Self(Self::NORTH.0 | Self::SOUTH.0);
    const HORIZONTAL: Self = Self(Self::WEST.0 | Self::EAST.0);
    const SINGLE_CELL_SEPARATOR: Self = Self(16);
    const T_RIGHT: Self = Self(Self::NORTH.0 | Self::SOUTH.0 | Self::WEST.0);
    const T_LEFT: Self = Self(Self::NORTH.0 | Self::SOUTH.0 | Self::EAST.0);
    const T_UP: Self = Self(Self::NORTH.0 | Self::WEST.0 | Self::EAST.0);
    const T_DOWN: Self = Self(Self::SOUTH.0 | Self::WEST.0 | Self::EAST.0);
}

impl BitOrAssign for LineMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Compose visible panes without changing child screens. Coordinates exclude the bar.
/// Every visible screen must exactly match its rectangle. Hidden screens may be
/// omitted; supplied IDs must belong to the layout and must not be duplicated.
/// The active screen supplies cursor appearance and supported terminal input modes.
pub fn compose(layout: &Layout, screens: &[(PaneId, &Screen)]) -> io::Result<Screen> {
    compose_with_highlight(layout, screens, None)
}

pub(crate) fn compose_with_highlight(
    layout: &Layout,
    screens: &[(PaneId, &Screen)],
    highlighted: Option<PaneId>,
) -> io::Result<Screen> {
    let (rows, columns) = layout.dimensions();
    if usize::from(rows) * usize::from(columns) > MAX_CELLS {
        return Err(invalid("composed screen exceeds cell limit"));
    }
    let tiled = layout.tiled_geometry();
    for (index, (id, _)) in screens.iter().enumerate() {
        if !tiled.panes.iter().any(|(pane, _)| pane == id)
            || screens[..index].iter().any(|(pane, _)| pane == id)
        {
            return Err(invalid("unknown or duplicate pane screen"));
        }
    }
    let geometry = layout.geometry();
    let highlighted = highlighted
        .map(|id| {
            geometry
                .panes
                .iter()
                .find(|(pane, _)| *pane == id)
                .map(|(_, rect)| *rect)
                .ok_or_else(|| invalid("highlighted pane is not visible"))
        })
        .transpose()?;
    let mut visible = Vec::with_capacity(geometry.panes.len());
    for (id, rect) in &geometry.panes {
        let source = screens
            .iter()
            .find(|(pane, _)| pane == id)
            .map(|(_, screen)| *screen)
            .ok_or_else(|| invalid("missing visible pane screen"))?;
        if source.dimensions() != (usize::from(rect.rows), usize::from(rect.columns)) {
            return Err(invalid("pane screen does not match its visible rectangle"));
        }
        visible.push((*id, *rect, source));
    }
    let (_, active_rect, active) = visible
        .iter()
        .find(|(id, _, _)| *id == layout.active())
        .expect("active pane is visible");
    let cursor = active.cursor();
    let mut frame = (*active).clone();
    frame.resize_display(usize::from(rows), usize::from(columns))?;
    for (_, rect, source) in &visible {
        frame.copy_display_cells(source, usize::from(rect.row), usize::from(rect.column));
    }
    // Rasterize separator membership first, then connect neighboring segments.
    let width = usize::from(columns);
    let mut lines = vec![LineMask::EMPTY; usize::from(rows) * width];
    for rect in &geometry.separators {
        let direction = if rect.rows == 1 && rect.columns == 1 {
            LineMask::SINGLE_CELL_SEPARATOR
        } else if rect.columns == 1 {
            LineMask::VERTICAL
        } else {
            LineMask::HORIZONTAL
        }; // N/S or W/E.
        for row in rect.row..rect.row + rect.rows {
            for column in rect.column..rect.column + rect.columns {
                lines[usize::from(row) * width + usize::from(column)] = direction;
            }
        }
    }
    for (index, &base) in lines
        .iter()
        .enumerate()
        .filter(|(_, mask)| **mask != LineMask::EMPTY)
    {
        let row = index / width;
        let column = index % width;
        let mut mask = if base == LineMask::SINGLE_CELL_SEPARATOR {
            // A one-cell separator is vertical only when it has pane cells on
            // both sides; otherwise its panes lie above and below.
            if column > 0
                && column + 1 < width
                && lines[index - 1] == LineMask::EMPTY
                && lines[index + 1] == LineMask::EMPTY
            {
                LineMask::VERTICAL
            } else {
                LineMask::HORIZONTAL
            }
        } else {
            base
        };
        if row > 0 && lines[index - width] != LineMask::EMPTY {
            mask |= LineMask::NORTH;
        }
        if row + 1 < usize::from(rows) && lines[index + width] != LineMask::EMPTY {
            mask |= LineMask::SOUTH;
        }
        if column > 0 && lines[index - 1] != LineMask::EMPTY {
            mask |= LineMask::WEST;
        }
        if column + 1 < width && lines[index + 1] != LineMask::EMPTY {
            mask |= LineMask::EAST;
        }
        let character = match mask {
            LineMask::VERTICAL => '│',
            LineMask::HORIZONTAL => '─',
            LineMask::T_RIGHT => '┤',
            LineMask::T_LEFT => '├',
            LineMask::T_UP => '┴',
            LineMask::T_DOWN => '┬',
            _ => '┼',
        };
        frame.set_display_cell(
            row,
            column,
            Cell {
                character,
                style: separator_style(
                    borders(*active_rect, row as u16, column as u16),
                    highlighted.is_some_and(|rect| borders(rect, row as u16, column as u16)),
                ),
                ..Cell::default()
            },
        );
    }
    frame.set_display_cursor(
        usize::from(active_rect.row) + cursor.0,
        usize::from(active_rect.column) + cursor.1,
    );
    Ok(frame)
}

fn borders(rect: Rect, row: u16, column: u16) -> bool {
    let bottom = rect.row + rect.rows;
    let right = rect.column + rect.columns;
    let beside = (column.checked_add(1) == Some(rect.column) || column == right)
        && row >= rect.row.saturating_sub(1)
        && row <= bottom;
    let above_or_below = (row.checked_add(1) == Some(rect.row) || row == bottom)
        && column >= rect.column.saturating_sub(1)
        && column <= right;
    beside || above_or_below
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::SplitAxis;

    #[test]
    fn active_and_history_highlights_follow_only_the_selected_pane_border() {
        let mut layout = Layout::new(5, 9).unwrap();
        let left = layout.active();
        layout.split_active(SplitAxis::Columns).unwrap();
        let selected = layout.split_active(SplitAxis::Rows).unwrap();
        let screens: Vec<_> = layout
            .geometry()
            .panes
            .iter()
            .map(|(id, rect)| {
                (
                    *id,
                    Screen::new(rect.rows.into(), rect.columns.into()).unwrap(),
                )
            })
            .collect();
        let references: Vec<_> = screens.iter().map(|(id, screen)| (*id, screen)).collect();

        let ordinary = compose_with_highlight(&layout, &references, None).unwrap();
        let highlighted = compose_with_highlight(&layout, &references, Some(selected)).unwrap();
        assert_eq!(
            ordinary.row(0).unwrap()[4].style,
            separator_style(false, false)
        );
        assert_eq!(
            highlighted.row(0).unwrap()[4].style,
            separator_style(false, false)
        );
        for (row, column) in [(2, 4), (2, 7), (3, 4), (4, 4)] {
            assert_eq!(
                ordinary.row(row).unwrap()[column].style,
                separator_style(true, false),
                "active separator at {row},{column} was not highlighted"
            );
            assert_eq!(
                highlighted.row(row).unwrap()[column].style,
                separator_style(true, true),
                "history separator at {row},{column} was not highlighted"
            );
        }

        layout.select(left).unwrap();
        let focused_left = compose_with_highlight(&layout, &references, None).unwrap();
        assert_eq!(
            focused_left.row(0).unwrap()[4].style,
            separator_style(true, false)
        );
        assert_eq!(
            focused_left.row(2).unwrap()[7].style,
            separator_style(false, false)
        );
    }
}
