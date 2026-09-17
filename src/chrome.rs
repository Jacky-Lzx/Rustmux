//! Top-row window chrome, composed separately from child terminal state.
use crate::{
    screen::{EraseMode, Screen},
    style::{Color, Style},
};
use std::io;
use unicode_width::UnicodeWidthChar;

const PANE_BAR_ROWS: u16 = 1;
const MIN_PANE_ROWS: u16 = 1;
const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
const MANTLE: Color = Color::Rgb(0x18, 0x18, 0x25);
const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);
const BLUE: Color = Color::Rgb(0x89, 0xb4, 0xfa);
const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);

pub(crate) fn pane_rows(outer_rows: u16) -> u16 {
    outer_rows.saturating_sub(PANE_BAR_ROWS).max(MIN_PANE_ROWS)
}

pub(crate) fn bar_style(active: bool) -> Style {
    // Catppuccin Mocha: explicit RGB keeps chrome independent of indexed palettes.
    Style {
        foreground: if active { BASE } else { SUBTEXT0 },
        background: if active { BLUE } else { MANTLE },
        bold: active,
        ..Style::default()
    }
}

pub(crate) fn separator_style(history: bool) -> Style {
    Style {
        foreground: if history { PEACH } else { SUBTEXT0 },
        background: MANTLE,
        bold: history,
        ..Style::default()
    }
}

/// Set up the UI row on a clone, never on the child's actual grid.
pub(crate) fn prepare_row(screen: &mut Screen, style: Style) {
    screen.set_origin_mode(false);
    screen.set_insert_mode(false);
    screen.set_auto_wrap(false);
    screen.designate_character_set(false, false);
    screen.select_character_set(false);
    screen.set_style(style);
    screen.position(0, 0);
    screen.erase_line(EraseMode::All);
}

pub(crate) fn clipped(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for character in text.chars().filter(|c| !c.is_control()) {
        let size = character.width().unwrap_or(0);
        if used + size > width {
            break;
        }
        if size == 0 && result.is_empty() {
            continue;
        }
        result.push(character);
        used += size;
    }
    result
}

pub(crate) fn compose(
    child: &Screen,
    outer_rows: u16,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
) -> io::Result<Screen> {
    let mut screen = child.clone();
    if outer_rows <= 1 {
        return Ok(screen);
    }
    let (_, columns) = screen.dimensions();
    screen.prepend_display_row()?;
    screen.save_cursor();
    prepare_row(&mut screen, bar_style(false));
    let labels: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            clipped(
                &format!(
                    " {}{}:{} ",
                    if index == active { "*" } else { "" },
                    index + 1,
                    name
                ),
                24,
            )
        })
        .collect();
    let widths: Vec<_> = labels
        .iter()
        .map(|s| s.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>())
        .collect();
    let active_width = widths.get(active).copied().unwrap_or(0).min(columns);
    let session = session_name
        .map(|name| clipped(&format!(" [{name}] "), columns.saturating_sub(active_width)))
        .unwrap_or_default();
    let session_width = session
        .chars()
        .map(|character| character.width().unwrap_or(0))
        .sum::<usize>();
    for character in session.chars() {
        screen.print(character);
    }
    let available = columns.saturating_sub(session_width);
    let mut start = 0;
    while start < active && widths[start..=active].iter().sum::<usize>() > available {
        start += 1;
    }
    let mut remaining = available;
    for (index, label) in labels.iter().enumerate().skip(start) {
        screen.set_style(bar_style(index == active));
        for character in clipped(label, remaining).chars() {
            screen.print(character);
            remaining -= character.width().unwrap_or(0);
        }
        if remaining == 0 {
            break;
        }
    }
    screen.restore_cursor();
    Ok(screen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn bar_preserves_child_rows_cursor_and_modes() {
        let mut child = Screen::new(3, 40).unwrap();
        Parser::new().advance(&mut child, b"content\x1b[2;3r\x1b[?6h\x1b(0\x1b[?2004h");
        let before = child.clone();
        let view = compose(&child, 4, None, &["first".into(), "中文".into()], 1).unwrap();
        assert_eq!(child, before);
        for row in 0..3 {
            assert_eq!(view.row(row + 1), child.row(row));
        }
        assert_eq!(view.cursor(), (child.cursor().0 + 1, child.cursor().1));
        assert_eq!(view.bracketed_paste(), child.bracketed_paste());
        assert_eq!(view.cursor_shape(), child.cursor_shape());
        let bar: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|c| c.width != 0)
            .map(|c| c.character)
            .collect();
        assert!(bar.contains("1:first"));
        assert!(bar.contains("*2:中文"));
    }

    #[test]
    fn narrow_bar_keeps_active_label_visible_and_does_not_split_wide_glyphs() {
        for columns in [1, 2, 4, 7, 12] {
            let child = Screen::new(1, columns).unwrap();
            let view = compose(
                &child,
                2,
                None,
                &["very long".into(), "中e\u{301}\x1b[31m".into()],
                1,
            )
            .unwrap();
            let row = view.row(0).unwrap();
            assert_eq!(row[0].style, bar_style(true));
            for (index, cell) in row.iter().enumerate() {
                if cell.width == 2 {
                    assert!(index + 1 < columns);
                    assert_eq!(row[index + 1].width, 0);
                }
                assert!(!cell.character.is_control());
            }
        }
        assert_eq!(pane_rows(1), 1);
        assert_eq!(pane_rows(24), 23);
        let child = Screen::new(1, 10).unwrap();
        assert_eq!(
            compose(&child, 1, None, &["hidden".into()], 0).unwrap(),
            child
        );
    }

    #[test]
    fn named_session_precedes_windows_without_hiding_the_active_label() {
        let child = Screen::new(2, 30).unwrap();
        let view = compose(
            &child,
            3,
            Some("personal"),
            &["first".into(), "editor".into()],
            1,
        )
        .unwrap();
        let bar: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(bar.starts_with(" [personal] "));
        assert!(bar.contains("*2:editor"));

        let narrow = Screen::new(2, 9).unwrap();
        let view = compose(&narrow, 3, Some("personal"), &["first".into()], 0).unwrap();
        let bar: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(bar.contains("*1:first"), "bar was {bar:?}");
        assert!(!bar.contains("personal"));
    }
}
