//! Window chrome, composed separately from child terminal state.
use crate::{
    screen::{EraseMode, MouseTracking, Screen},
    style::{Color, Style},
};
use std::io;
use unicode_width::UnicodeWidthChar;

const TOP_BAR_ROWS: u16 = 1;
const BOTTOM_BAR_ROWS: u16 = 1;
const MIN_PANE_ROWS: u16 = 1;
const POWERLINE_RIGHT: char = '';
const BADGE_TEXT: Color = Color::Rgb(0x11, 0x11, 0x1b);
const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);
const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);

pub(crate) fn pane_rows(outer_rows: u16) -> u16 {
    let chrome_rows = TOP_BAR_ROWS + u16::from(footer_enabled(outer_rows)) * BOTTOM_BAR_ROWS;
    outer_rows.saturating_sub(chrome_rows).max(MIN_PANE_ROWS)
}

pub(crate) fn footer_enabled(outer_rows: u16) -> bool {
    outer_rows >= TOP_BAR_ROWS + BOTTOM_BAR_ROWS + MIN_PANE_ROWS
}

pub(crate) fn bar_style(active: bool) -> Style {
    // Match main's Catppuccin Mocha powerline badges.
    Style {
        foreground: BADGE_TEXT,
        background: if active { GREEN } else { TEXT },
        bold: true,
        ..Style::default()
    }
}

fn bar_background_style() -> Style {
    Style {
        foreground: TEXT,
        background: BASE,
        ..Style::default()
    }
}

fn shortcut_key_style() -> Style {
    Style {
        foreground: BADGE_TEXT,
        background: PEACH,
        bold: true,
        ..Style::default()
    }
}

fn shortcut_label_style() -> Style {
    Style {
        foreground: SUBTEXT0,
        background: BASE,
        ..Style::default()
    }
}

fn separator_style(foreground: Color, background: Color) -> Style {
    Style {
        foreground,
        background,
        ..Style::default()
    }
}

pub(crate) fn pane_border_style(active: bool, history: bool) -> Style {
    Style {
        foreground: if history {
            PEACH
        } else if active {
            GREEN
        } else {
            SUBTEXT0
        },
        bold: active || history,
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

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

fn print(screen: &mut Screen, text: &str) {
    for character in text.chars() {
        screen.print(character);
    }
}

#[derive(Clone, Copy)]
struct ShortcutHint {
    key: &'static str,
    label: &'static str,
    actions: &'static [(usize, u8)],
}

const LOCKED_SHORTCUTS: &[ShortcutHint] = &[
    ShortcutHint {
        key: "Ctrl-B",
        label: "Commands",
        actions: &[(0, 2)],
    },
    ShortcutHint {
        key: "Ctrl-B Ctrl-W",
        label: "Sessions",
        actions: &[(0, 23)],
    },
];

const NORMAL_SHORTCUTS: &[ShortcutHint] = &[
    ShortcutHint {
        key: "c",
        label: "New",
        actions: &[(0, b'c')],
    },
    ShortcutHint {
        key: "%",
        label: "Split →",
        actions: &[(0, b'%')],
    },
    ShortcutHint {
        key: "\"",
        label: "Split ↓",
        actions: &[(0, b'"')],
    },
    ShortcutHint {
        key: "h/j/k/l",
        label: "Focus",
        actions: &[(0, b'h'), (2, b'j'), (4, b'k'), (6, b'l')],
    },
    ShortcutHint {
        key: "n/p",
        label: "Window",
        actions: &[(0, b'n'), (2, b'p')],
    },
    ShortcutHint {
        key: "Z",
        label: "Zoom",
        actions: &[(0, b'Z')],
    },
    ShortcutHint {
        key: "?",
        label: "Help",
        actions: &[(0, b'?')],
    },
];

fn shortcut_width(hint: ShortcutHint) -> usize {
    display_width(hint.key) + display_width(hint.label) + 3
}

fn visible_shortcuts(columns: usize, normal: bool, session: bool) -> Vec<ShortcutHint> {
    let shortcuts = if normal {
        NORMAL_SHORTCUTS
    } else if session {
        LOCKED_SHORTCUTS
    } else {
        &LOCKED_SHORTCUTS[..1]
    };
    let mut visible = Vec::new();
    let mut remaining = columns;
    if normal {
        let (help, primary) = shortcuts
            .split_last()
            .expect("normal shortcuts include help");
        let help_width = shortcut_width(*help);
        if help_width <= remaining {
            remaining -= help_width;
            for hint in primary {
                let width = shortcut_width(*hint);
                if width > remaining {
                    break;
                }
                visible.push(*hint);
                remaining -= width;
            }
            visible.push(*help);
            return visible;
        }
    }
    for hint in shortcuts {
        let width = shortcut_width(*hint);
        if width > remaining {
            break;
        }
        visible.push(*hint);
        remaining -= width;
    }
    visible
}

fn draw_shortcuts(screen: &mut Screen, row: usize, columns: usize, normal: bool, session: bool) {
    screen.set_origin_mode(false);
    screen.set_insert_mode(false);
    screen.set_auto_wrap(false);
    screen.designate_character_set(false, false);
    screen.select_character_set(false);
    screen.set_style(bar_background_style());
    screen.position(row, 0);
    screen.erase_line(EraseMode::All);
    for hint in visible_shortcuts(columns, normal, session) {
        screen.set_style(shortcut_key_style());
        print(screen, " ");
        print(screen, hint.key);
        print(screen, " ");
        screen.set_style(shortcut_label_style());
        print(screen, hint.label);
        print(screen, " ");
    }
}

/// Return one-based, half-open footer targets. Specific characters in grouped
/// keys precede the whole-hint fallback, so clicking `p` in `n/p` selects `p`
/// while its label and padding select the first displayed key.
pub(crate) fn footer_hitboxes(
    columns: usize,
    normal: bool,
    session: bool,
) -> Vec<(usize, usize, u8)> {
    let mut hitboxes = Vec::new();
    let mut used = 0;
    for hint in visible_shortcuts(columns, normal, session) {
        let width = shortcut_width(hint);
        for (offset, action) in hint.actions {
            let column = used + 2 + offset;
            hitboxes.push((column, column + 1, *action));
        }
        if let Some((_, action)) = hint.actions.first() {
            hitboxes.push((used + 1, used + width + 1, *action));
        }
        used += width;
    }
    hitboxes
}

fn powerline_label(index: usize, name: &str) -> String {
    format!(" {} {} ", index + 1, name)
}

fn powerline_width(label: &str) -> usize {
    display_width(label) + 2
}

fn draw_powerline_segment(screen: &mut Screen, label: &str, active: bool, remaining: &mut usize) {
    if *remaining < 3 {
        return;
    }
    let background = if active { GREEN } else { TEXT };
    screen.set_style(separator_style(BASE, background));
    screen.print(POWERLINE_RIGHT);
    *remaining -= 1;

    let label = clipped(label, remaining.saturating_sub(1));
    let label_width = display_width(&label);
    screen.set_style(bar_style(active));
    print(screen, &label);
    *remaining -= label_width;

    screen.set_style(separator_style(background, BASE));
    screen.print(POWERLINE_RIGHT);
    *remaining -= 1;
}

fn mode_label(normal: bool) -> &'static str {
    if normal { " NORMAL " } else { " LOCKED " }
}

fn draw_mode(screen: &mut Screen, columns: usize, width: usize, normal: bool) {
    if width < 3 {
        return;
    }
    let background = if normal { GREEN } else { RED };
    screen.position(0, columns - width);
    screen.set_style(separator_style(BASE, background));
    screen.print(POWERLINE_RIGHT);
    let label = clipped(mode_label(normal), width - 2);
    screen.set_style(Style {
        foreground: BADGE_TEXT,
        background,
        bold: true,
        ..Style::default()
    });
    print(screen, &label);
    screen.set_style(separator_style(background, BASE));
    screen.print(POWERLINE_RIGHT);
}

struct BarLayout {
    labels: Vec<String>,
    session: String,
    session_width: usize,
    label_columns: usize,
    start: usize,
    mode_width: usize,
}

fn bar_layout(
    columns: usize,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
    normal_mode: bool,
) -> BarLayout {
    let labels: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| powerline_label(index, name))
        .collect();
    let widths: Vec<_> = labels.iter().map(|label| powerline_width(label)).collect();
    let active_width = widths.get(active).copied().unwrap_or(0).min(columns);
    let full_mode_width = powerline_width(mode_label(normal_mode));
    let mode_width = if columns >= active_width.saturating_add(full_mode_width) {
        full_mode_width
    } else {
        0
    };
    let label_columns = columns.saturating_sub(mode_width);
    let session = session_name
        .map(|name| {
            clipped(
                &format!(" Rustmux ({name}) "),
                label_columns.saturating_sub(active_width),
            )
        })
        .unwrap_or_default();
    let session_width = display_width(&session);
    let available = label_columns.saturating_sub(session_width);
    let mut start = 0;
    while start < active && widths[start..=active].iter().sum::<usize>() > available {
        start += 1;
    }
    BarLayout {
        labels,
        session,
        session_width,
        label_columns,
        start,
        mode_width,
    }
}

pub(crate) fn window_hitboxes(
    columns: usize,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
    normal_mode: bool,
) -> Vec<(usize, usize, usize)> {
    let layout = bar_layout(columns, session_name, names, active, normal_mode);
    let mut hitboxes = Vec::new();
    let mut used = layout.session_width;
    let mut remaining = layout.label_columns.saturating_sub(used);
    for (index, label) in layout.labels.iter().enumerate().skip(layout.start) {
        if remaining < 3 {
            break;
        }
        let label_width = display_width(&clipped(label, remaining - 2));
        let segment_width = label_width + 2;
        hitboxes.push((used + 1, used + segment_width + 1, index));
        used += segment_width;
        remaining -= segment_width;
    }
    hitboxes
}

pub(crate) fn compose(
    child: &Screen,
    outer_rows: u16,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
    normal_mode: bool,
) -> io::Result<Screen> {
    let mut screen = child.clone();
    if outer_rows <= 1 {
        return Ok(screen);
    }
    let (_, columns) = screen.dimensions();
    if screen.mouse_tracking() != MouseTracking::Any {
        // The chrome needs drag reports for pane separators. WindowInput filters
        // motion back down to the tracking mode requested by the active child.
        screen.set_mouse_tracking(MouseTracking::Drag);
    }
    screen.prepend_display_row()?;
    let footer = footer_enabled(outer_rows);
    if footer {
        screen.append_display_row()?;
    }
    screen.save_cursor();
    prepare_row(&mut screen, bar_background_style());
    let layout = bar_layout(columns, session_name, names, active, normal_mode);
    screen.set_style(Style {
        bold: true,
        ..bar_background_style()
    });
    print(&mut screen, &layout.session);
    let mut remaining = layout.label_columns.saturating_sub(layout.session_width);
    for (index, label) in layout.labels.iter().enumerate().skip(layout.start) {
        draw_powerline_segment(&mut screen, label, index == active, &mut remaining);
        if remaining < 3 {
            break;
        }
    }
    if layout.mode_width != 0 {
        draw_mode(&mut screen, columns, layout.mode_width, normal_mode);
    }
    if footer {
        let row = screen.dimensions().0 - 1;
        draw_shortcuts(
            &mut screen,
            row,
            columns,
            normal_mode,
            session_name.is_some(),
        );
    }
    screen.restore_cursor();
    Ok(screen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn pane_borders_use_main_colors_without_an_opaque_background() {
        assert_eq!(
            pane_border_style(true, false),
            Style {
                foreground: GREEN,
                background: Color::Default,
                bold: true,
                ..Style::default()
            }
        );
        assert_eq!(pane_border_style(false, false).foreground, SUBTEXT0);
        assert_eq!(pane_border_style(false, true).foreground, PEACH);
        for style in [
            pane_border_style(false, false),
            pane_border_style(false, true),
        ] {
            assert_eq!(style.background, Color::Default);
        }
    }

    #[test]
    fn bar_preserves_child_rows_cursor_and_modes() {
        let mut child = Screen::new(3, 60).unwrap();
        Parser::new().advance(&mut child, b"content\x1b[2;3r\x1b[?6h\x1b(0\x1b[?2004h");
        let before = child.clone();
        let view = compose(&child, 5, None, &["first".into(), "中文".into()], 1, false).unwrap();
        assert_eq!(child, before);
        assert_eq!(view.dimensions(), (5, 60));
        for row in 0..3 {
            assert_eq!(view.row(row + 1), child.row(row));
        }
        assert_eq!(view.cursor(), (child.cursor().0 + 1, child.cursor().1));
        assert_eq!(view.bracketed_paste(), child.bracketed_paste());
        assert_eq!(view.cursor_shape(), child.cursor_shape());
        assert_eq!(child.mouse_tracking(), MouseTracking::Off);
        assert_eq!(view.mouse_tracking(), MouseTracking::Drag);
        let bar: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|c| c.width != 0)
            .map(|c| c.character)
            .collect();
        assert!(bar.contains("1 first"));
        assert!(bar.contains("2 中文"));
        assert!(bar.contains(POWERLINE_RIGHT));
        let row = view.row(0).unwrap();
        assert_eq!(row[0].style, separator_style(BASE, TEXT));
        assert_eq!(row[1].style, bar_style(false));
        assert_eq!(row[10].style, separator_style(TEXT, BASE));
        assert_eq!(row[11].style, separator_style(BASE, GREEN));
        assert_eq!(row[12].style, bar_style(true));
        let footer: String = view
            .row(4)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(footer.starts_with(" Ctrl-B Commands "));
        assert!(!footer.contains("Ctrl-B Ctrl-W Sessions"));
        assert_eq!(view.row(4).unwrap()[0].style, shortcut_key_style());

        let mut mouse_child = Screen::new(2, 20).unwrap();
        mouse_child.set_mouse_tracking(MouseTracking::Drag);
        mouse_child.set_sgr_mouse(true);
        let view = compose(&mouse_child, 4, None, &["shell".into()], 0, false).unwrap();
        assert_eq!(view.mouse_tracking(), MouseTracking::Drag);
        assert!(view.sgr_mouse());

        mouse_child.set_mouse_tracking(MouseTracking::Any);
        let view = compose(&mouse_child, 4, None, &["shell".into()], 0, false).unwrap();
        assert_eq!(view.mouse_tracking(), MouseTracking::Any);
    }

    #[test]
    fn narrow_bar_keeps_active_label_visible_and_does_not_split_wide_glyphs() {
        for columns in [3, 4, 7, 12] {
            let child = Screen::new(1, columns).unwrap();
            let view = compose(
                &child,
                2,
                None,
                &["very long".into(), "中e\u{301}\x1b[31m".into()],
                1,
                false,
            )
            .unwrap();
            let row = view.row(0).unwrap();
            assert_eq!(row[0].character, POWERLINE_RIGHT);
            assert_eq!(row[0].style, separator_style(BASE, GREEN));
            for (index, cell) in row.iter().enumerate() {
                if cell.width == 2 {
                    assert!(index + 1 < columns);
                    assert_eq!(row[index + 1].width, 0);
                }
                assert!(!cell.character.is_control());
            }
        }
        assert_eq!(pane_rows(1), 1);
        assert_eq!(pane_rows(2), 1);
        assert_eq!(pane_rows(3), 1);
        assert_eq!(pane_rows(24), 22);
        let child = Screen::new(1, 10).unwrap();
        assert_eq!(
            compose(&child, 1, None, &["hidden".into()], 0, false).unwrap(),
            child
        );
    }

    #[test]
    fn named_session_precedes_windows_without_hiding_the_active_label() {
        let child = Screen::new(2, 50).unwrap();
        let view = compose(
            &child,
            4,
            Some("personal"),
            &["first".into(), "editor".into()],
            1,
            false,
        )
        .unwrap();
        let bar: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(bar.starts_with(" Rustmux (personal) "));
        assert!(bar.contains("2 editor"));

        let narrow = Screen::new(1, 9).unwrap();
        let view = compose(&narrow, 3, Some("personal"), &["first".into()], 0, false).unwrap();
        let bar: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(bar.contains("1 fir"), "bar was {bar:?}");
        assert!(!bar.contains("personal"));
    }

    #[test]
    fn mode_badge_is_right_aligned_and_yields_to_the_active_window() {
        let child = Screen::new(2, 40).unwrap();
        let locked = compose(&child, 4, None, &["shell".into()], 0, false).unwrap();
        let normal = compose(&child, 4, None, &["shell".into()], 0, true).unwrap();
        let locked_row = locked.row(0).unwrap();
        let normal_row = normal.row(0).unwrap();
        assert_eq!(locked_row[30].style, separator_style(BASE, RED));
        assert_eq!(normal_row[30].style, separator_style(BASE, GREEN));
        assert_eq!(locked_row[31].character, ' ');
        assert_eq!(locked_row[32].character, 'L');
        assert_eq!(normal_row[32].character, 'N');

        let narrow = Screen::new(1, 8).unwrap();
        let view = compose(&narrow, 3, None, &["shell".into()], 0, true).unwrap();
        let bar: String = view
            .row(0)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(bar.contains("1 she"), "bar was {bar:?}");
        assert!(!bar.contains("NORMAL"));
    }

    #[test]
    fn footer_changes_with_mode_and_keeps_hints_atomic() {
        let child = Screen::new(1, 80).unwrap();
        let locked = compose(&child, 3, None, &["shell".into()], 0, false).unwrap();
        let normal = compose(&child, 3, None, &["shell".into()], 0, true).unwrap();
        let text = |screen: &Screen| {
            screen
                .row(2)
                .unwrap()
                .iter()
                .filter(|cell| cell.width != 0)
                .map(|cell| cell.character)
                .collect::<String>()
        };
        assert!(text(&locked).starts_with(" Ctrl-B Commands "));
        assert!(!text(&locked).contains("Sessions"));
        assert!(text(&normal).starts_with(" c New  % Split →  \" Split ↓ "));

        let named = compose(&child, 3, Some("work"), &["shell".into()], 0, false).unwrap();
        assert!(text(&named).contains("Ctrl-B Ctrl-W Sessions"));

        let narrow = Screen::new(1, 20).unwrap();
        let normal = compose(&narrow, 3, None, &["shell".into()], 0, true).unwrap();
        let footer: String = normal
            .row(2)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(footer.starts_with(" c New  ? Help "));
        assert!(!footer.contains("Split ↓"));
        assert!(!footer.contains("Focus"));
    }

    #[test]
    fn window_hitboxes_follow_the_rendered_segments_only() {
        let names = vec!["first".into(), "second".into(), "third".into()];
        assert_eq!(
            window_hitboxes(80, None, &names, 0, false),
            vec![(1, 12, 0), (12, 24, 1), (24, 35, 2)]
        );

        let session = window_hitboxes(80, Some("work"), &names, 0, false);
        assert_eq!(session[0], (17, 28, 0));
        assert!(session.iter().all(|(_, end, _)| *end <= 71));

        let narrow = window_hitboxes(12, None, &names, 2, false);
        assert_eq!(narrow.len(), 1);
        assert_eq!(narrow[0].2, 2);
    }

    #[test]
    fn footer_hitboxes_follow_visible_hints_and_split_grouped_keys() {
        let locked = footer_hitboxes(80, false, true);
        assert!(
            locked
                .iter()
                .any(|&(start, end, key)| { start == 1 && end == 18 && key == 2 })
        );
        assert!(locked.iter().any(|&(_, _, key)| key == 23));
        assert!(
            !footer_hitboxes(80, false, false)
                .iter()
                .any(|&(_, _, key)| key == 23)
        );

        let normal = footer_hitboxes(80, true, true);
        let action_at = |column| {
            normal
                .iter()
                .find(|(start, end, _)| column >= *start && column < *end)
                .map(|(_, _, action)| *action)
        };
        assert_eq!(action_at(2), Some(b'c'));
        assert_eq!(action_at(31), Some(b'h'));
        assert_eq!(action_at(33), Some(b'j'));
        assert_eq!(action_at(39), Some(b'h'));
        assert_eq!(action_at(46), Some(b'n'));
        assert_eq!(action_at(48), Some(b'p'));

        let narrow = footer_hitboxes(20, true, true);
        assert!(narrow.iter().any(|&(_, _, key)| key == b'c'));
        assert!(narrow.iter().any(|&(_, _, key)| key == b'?'));
        assert!(!narrow.iter().any(|&(_, _, key)| key == b'%'));
        assert!(!narrow.iter().any(|&(_, _, key)| key == b'"'));
    }
}
