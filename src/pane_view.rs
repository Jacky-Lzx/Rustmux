//! Compose validated pane screens into one content-area frame for the renderer.

use crate::{
    chrome::pane_border_style,
    layout::{Layout, PaneId, Rect},
    pane::MAX_CELLS,
    screen::Screen,
    style::Cell,
};
use std::io;

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
    compose_with_titles(layout, screens, highlighted, &[], &[])
}

pub(crate) fn compose_with_titles(
    layout: &Layout,
    screens: &[(PaneId, &Screen)],
    highlighted: Option<PaneId>,
    titles: &[(PaneId, &str)],
    bells: &[PaneId],
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
    let content_geometry = layout.content_geometry();
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
    for (id, rect) in &content_geometry.panes {
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
    // Each pane owns all four sides of its frame. A split reserves two cells so
    // adjacent panes remain visually distinct instead of sharing one separator.
    for (id, rect) in &geometry.panes {
        draw_frame(
            &mut frame,
            *rect,
            rows,
            columns,
            pane_border_style(
                *id == layout.active(),
                highlighted.is_some_and(|candidate| candidate == *rect),
                bells.contains(id),
            ),
        );
    }
    for (id, rect) in &geometry.panes {
        let Some(title) = titles
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, title)| *title)
        else {
            continue;
        };
        draw_title(
            &mut frame,
            *rect,
            rows,
            columns,
            title,
            bells.contains(id),
            pane_border_style(
                *id == layout.active(),
                highlighted.is_some_and(|candidate| candidate == *rect),
                bells.contains(id),
            ),
        );
    }
    frame.set_display_cursor(
        usize::from(active_rect.row) + cursor.0,
        usize::from(active_rect.column) + cursor.1,
    );
    Ok(frame)
}

fn frame_bounds(rect: Rect, rows: u16, columns: u16) -> (u16, u16, u16, u16) {
    let top = if rect.row == 0 { 0 } else { rect.row - 1 };
    let left = if rect.column == 0 { 0 } else { rect.column - 1 };
    let bottom = if rect.row + rect.rows == rows {
        rows - 1
    } else {
        rect.row + rect.rows
    };
    let right = if rect.column + rect.columns == columns {
        columns - 1
    } else {
        rect.column + rect.columns
    };
    (top, left, bottom, right)
}

pub(crate) fn hitboxes(layout: &Layout) -> Vec<(PaneId, Rect)> {
    let (rows, columns) = layout.dimensions();
    layout
        .geometry()
        .panes
        .into_iter()
        .map(|(id, rect)| {
            let (top, left, bottom, right) = frame_bounds(rect, rows, columns);
            (
                id,
                Rect {
                    row: top,
                    column: left,
                    rows: bottom - top + 1,
                    columns: right - left + 1,
                },
            )
        })
        .collect()
}

fn draw_frame(frame: &mut Screen, rect: Rect, rows: u16, columns: u16, style: crate::style::Style) {
    let (top, left, bottom, right) = frame_bounds(rect, rows, columns);
    if rows >= 3 {
        for column in left..=right {
            frame.set_display_cell(
                usize::from(top),
                usize::from(column),
                Cell {
                    character: '─',
                    style,
                    ..Cell::default()
                },
            );
            frame.set_display_cell(
                usize::from(bottom),
                usize::from(column),
                Cell {
                    character: '─',
                    style,
                    ..Cell::default()
                },
            );
        }
    }
    if columns >= 3 {
        for row in top..=bottom {
            frame.set_display_cell(
                usize::from(row),
                usize::from(left),
                Cell {
                    character: '│',
                    style,
                    ..Cell::default()
                },
            );
            frame.set_display_cell(
                usize::from(row),
                usize::from(right),
                Cell {
                    character: '│',
                    style,
                    ..Cell::default()
                },
            );
        }
    }
    if rows >= 3 && columns >= 3 {
        for (row, column, character) in [
            (top, left, '┌'),
            (top, right, '┐'),
            (bottom, left, '└'),
            (bottom, right, '┘'),
        ] {
            frame.set_display_cell(
                usize::from(row),
                usize::from(column),
                Cell {
                    character,
                    style,
                    ..Cell::default()
                },
            );
        }
    }
}

fn draw_title(
    frame: &mut Screen,
    rect: Rect,
    rows: u16,
    columns: u16,
    title: &str,
    bell: bool,
    style: crate::style::Style,
) {
    let (row, left, _, right) = frame_bounds(rect, rows, columns);
    if rows < 3 || right <= left + 1 {
        return;
    }
    let marker = if bell { " [!]" } else { "" };
    let label = crate::chrome::clipped(
        &format!("─ {title}{marker} "),
        usize::from(right - left - 1),
    );
    let mut column = usize::from(left + 1);
    for character in label.chars() {
        let width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if width == 0 || column + width > usize::from(right) {
            continue;
        }
        frame.set_display_cell(
            usize::from(row),
            column,
            Cell {
                character,
                width: width as u8,
                style,
                ..Cell::default()
            },
        );
        if width == 2 {
            frame.set_display_cell(
                usize::from(row),
                column + 1,
                Cell {
                    width: 0,
                    style,
                    ..Cell::default()
                },
            );
        }
        column += width;
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::SplitAxis;
    use crate::theme::{DEFAULT_BACKGROUND, DEFAULT_FOREGROUND};

    #[test]
    fn active_and_history_highlights_follow_only_the_selected_pane_border() {
        let mut layout = Layout::new(7, 13).unwrap();
        let left = layout.active();
        layout.split_active(SplitAxis::Columns).unwrap();
        let selected = layout.split_active(SplitAxis::Rows).unwrap();
        let screens: Vec<_> = layout
            .content_geometry()
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
            ordinary.row(0).unwrap()[5].style,
            pane_border_style(false, false, false)
        );
        assert_eq!(
            highlighted.row(0).unwrap()[5].style,
            pane_border_style(false, false, false)
        );
        for (row, column) in [(3, 6), (3, 8), (4, 6), (6, 6)] {
            assert_eq!(
                ordinary.row(row).unwrap()[column].style,
                pane_border_style(true, false, false),
                "active border at {row},{column} was not highlighted"
            );
            assert_eq!(
                highlighted.row(row).unwrap()[column].style,
                pane_border_style(true, true, false),
                "history border at {row},{column} was not highlighted"
            );
        }

        layout.select(left).unwrap();
        let focused_left = compose_with_highlight(&layout, &references, None).unwrap();
        assert_eq!(
            focused_left.row(0).unwrap()[5].style,
            pane_border_style(true, false, false)
        );
        assert_eq!(
            focused_left.row(3).unwrap()[8].style,
            pane_border_style(false, false, false)
        );
    }

    #[test]
    fn outer_frame_reserves_content_cells_and_renders_titles() {
        let mut layout = Layout::new(5, 17).unwrap();
        let left = layout.active();
        let right = layout.split_active(SplitAxis::Columns).unwrap();
        assert_eq!(
            layout.content_geometry().panes,
            vec![
                (
                    left,
                    Rect {
                        row: 1,
                        column: 1,
                        rows: 3,
                        columns: 6,
                    },
                ),
                (
                    right,
                    Rect {
                        row: 1,
                        column: 9,
                        rows: 3,
                        columns: 7,
                    },
                ),
            ]
        );
        let left_screen = Screen::new(3, 6).unwrap();
        let right_screen = Screen::new(3, 7).unwrap();
        let view = compose_with_titles(
            &layout,
            &[(left, &left_screen), (right, &right_screen)],
            None,
            &[(left, "left"), (right, "right")],
            &[left],
        )
        .unwrap();
        let top: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(top.contains("left"));
        assert!(top.contains("right"));
        assert_eq!(view.row(0).unwrap()[0].character, '┌');
        assert_eq!(view.row(4).unwrap()[16].character, '┘');
        assert_eq!(
            view.row(0).unwrap()[8].style,
            pane_border_style(true, false, false)
        );
        assert_eq!(
            view.row(0).unwrap()[0].style,
            pane_border_style(false, false, true)
        );
    }

    #[test]
    fn tiny_canvas_omits_outer_borders_without_losing_content() {
        let layout = Layout::new(1, 2).unwrap();
        let mut child = Screen::new(1, 2).unwrap();
        child.print('o');
        child.print('k');
        let view = compose(&layout, &[(layout.active(), &child)]).unwrap();
        assert_eq!(view.dimensions(), child.dimensions());
        assert_eq!(view.row(0).unwrap()[0].style.foreground, DEFAULT_FOREGROUND);
        assert_eq!(view.row(0).unwrap()[0].style.background, DEFAULT_BACKGROUND);
        assert_eq!(
            view.row(0)
                .unwrap()
                .iter()
                .map(|cell| cell.character)
                .collect::<String>(),
            "ok"
        );
        assert_eq!(view.cursor(), child.cursor());
    }

    #[test]
    fn pending_bell_is_shown_in_the_pane_title() {
        let layout = Layout::new(5, 20).unwrap();
        let pane = layout.active();
        let screen = Screen::new(3, 18).unwrap();
        let view = compose_with_titles(
            &layout,
            &[(pane, &screen)],
            None,
            &[(pane, "shell")],
            &[pane],
        )
        .unwrap();
        let top: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(top.contains("shell [!]"));
    }
}
