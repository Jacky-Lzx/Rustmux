//! Window chrome, composed separately from child terminal state.
use crate::{
    screen::{EraseMode, MouseTracking, Screen},
    style::{Color, Style},
    theme::{DEFAULT_BACKGROUND, DEFAULT_FOREGROUND},
};
use std::io;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const TOP_BAR_ROWS: u16 = 1;
const BOTTOM_BAR_ROWS: u16 = 1;
const MIN_PANE_ROWS: u16 = 1;
const POWERLINE_RIGHT: char = '';
const HISTORY_MINIMUM_STATUS_COLUMNS: usize = 24;
const BADGE_TEXT: Color = Color::Rgb(0x11, 0x11, 0x1b);
const BASE: Color = DEFAULT_BACKGROUND;
const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);
const TEXT: Color = DEFAULT_FOREGROUND;
const PINK: Color = Color::Rgb(0xf5, 0xc2, 0xe7);
const LAVENDER: Color = Color::Rgb(0xb4, 0xbe, 0xfe);
const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
const HISTORY_MODE: &str = " HISTORY ";

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
        foreground: PINK,
        background: BASE,
        bold: true,
        ..Style::default()
    }
}

fn shortcut_label_style() -> Style {
    Style {
        foreground: BADGE_TEXT,
        background: LAVENDER,
        bold: true,
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

pub(crate) fn pane_border_style(active: bool, history: bool, bell: bool) -> Style {
    Style {
        foreground: if history {
            PEACH
        } else if active {
            GREEN
        } else if bell {
            PEACH
        } else {
            SUBTEXT0
        },
        bold: active || history || bell,
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

const LOCKED_SHORTCUTS: &[ShortcutHint] = &[ShortcutHint {
    key: "Ctrl-B",
    label: "Commands",
    actions: &[(0, 2)],
}];

const SESSION_SHORTCUT: ShortcutHint = ShortcutHint {
    key: "Ctrl-W",
    label: "Sessions",
    actions: &[(0, 23)],
};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FooterMode {
    Locked,
    Normal,
    Pane,
}

const PANE_SHORTCUTS: &[ShortcutHint] = &[
    ShortcutHint {
        key: "b",
        label: "Break",
        actions: &[(0, b'b')],
    },
    ShortcutHint {
        key: "r",
        label: "Split →",
        actions: &[(0, b'r')],
    },
    ShortcutHint {
        key: "d",
        label: "Split ↓",
        actions: &[(0, b'd')],
    },
    ShortcutHint {
        key: "h/j/k/l",
        label: "Focus",
        actions: &[(0, b'h'), (2, b'j'), (4, b'k'), (6, b'l')],
    },
    ShortcutHint {
        key: "f",
        label: "Zoom",
        actions: &[(0, b'f')],
    },
    ShortcutHint {
        key: "x",
        label: "Close",
        actions: &[(0, b'x')],
    },
];

fn pane_action(key: u8) -> Option<crate::config::PaneAction> {
    use crate::config::PaneAction;
    Some(match key {
        b'b' => PaneAction::Break,
        b'r' => PaneAction::SplitRight,
        b'd' => PaneAction::SplitDown,
        b'h' => PaneAction::FocusLeft,
        b'j' => PaneAction::FocusDown,
        b'k' => PaneAction::FocusUp,
        b'l' => PaneAction::FocusRight,
        b'f' => PaneAction::Zoom,
        b'x' => PaneAction::Close,
        _ => return None,
    })
}

fn displayed_key(key: u8) -> String {
    match key {
        9 => "Tab".to_owned(),
        13 => "Enter".to_owned(),
        27 => "Esc".to_owned(),
        1..=26 => format!("Ctrl-{}", char::from(b'A' + key - 1)),
        _ => char::from(key).to_string(),
    }
}

fn pane_hint_key(hint: ShortcutHint, shortcuts: crate::config::Shortcuts) -> String {
    hint.actions
        .iter()
        .filter_map(|(_, key)| shortcuts.pane_key(pane_action(*key)?).map(displayed_key))
        .collect::<Vec<_>>()
        .join("/")
}

fn hint_key_for_mode(
    hint: ShortcutHint,
    mode: FooterMode,
    shortcuts: crate::config::Shortcuts,
) -> String {
    if mode == FooterMode::Pane {
        pane_hint_key(hint, shortcuts)
    } else {
        hint_key(hint, shortcuts)
    }
}

fn hint_key(hint: ShortcutHint, shortcuts: crate::config::Shortcuts) -> String {
    if hint.key == "n/p" {
        format!(
            "{}/{}",
            char::from(shortcuts.key_for(b'n')),
            char::from(shortcuts.key_for(b'p'))
        )
    } else if hint.actions.len() == 1 && matches!(hint.actions[0].1, b'c' | b'%' | b'"') {
        char::from(shortcuts.key_for(hint.actions[0].1)).to_string()
    } else {
        hint.key.to_owned()
    }
}

fn shortcut_width(
    hint: ShortcutHint,
    mode: FooterMode,
    shortcuts: crate::config::Shortcuts,
) -> usize {
    display_width(&hint_key_for_mode(hint, mode, shortcuts)) + display_width(hint.label) + 6
}

fn visible_shortcuts(
    columns: usize,
    mode: FooterMode,
    session: bool,
    bindings: crate::config::Shortcuts,
) -> Vec<ShortcutHint> {
    let shortcuts = match mode {
        FooterMode::Normal => NORMAL_SHORTCUTS,
        FooterMode::Pane => PANE_SHORTCUTS,
        FooterMode::Locked => LOCKED_SHORTCUTS,
    };
    let mut visible = Vec::new();
    let mut remaining = columns;
    if mode == FooterMode::Normal {
        let (help, primary) = shortcuts
            .split_last()
            .expect("normal shortcuts include help");
        let help_width = shortcut_width(*help, mode, bindings);
        if help_width <= remaining {
            remaining -= help_width;
            let session_hint = session
                .then_some(SESSION_SHORTCUT)
                .filter(|hint| shortcut_width(*hint, mode, bindings) <= remaining);
            if let Some(hint) = session_hint {
                remaining -= shortcut_width(hint, mode, bindings);
            }
            for hint in primary {
                if !hint
                    .actions
                    .iter()
                    .all(|(_, action)| bindings.action_is_active(*action))
                {
                    continue;
                }
                let width = shortcut_width(*hint, mode, bindings);
                if width > remaining {
                    break;
                }
                visible.push(*hint);
                remaining -= width;
            }
            visible.extend(session_hint);
            visible.push(*help);
            return visible;
        }
    }
    for hint in shortcuts {
        if !hint.actions.iter().all(|(_, action)| match mode {
            FooterMode::Pane => pane_action(*action)
                .and_then(|action| bindings.pane_key(action))
                .is_some(),
            _ => bindings.action_is_active(*action),
        }) {
            continue;
        }
        let width = shortcut_width(*hint, mode, bindings);
        if width > remaining {
            break;
        }
        visible.push(*hint);
        remaining -= width;
    }
    visible
}

fn draw_shortcut_segment(
    screen: &mut Screen,
    hint: ShortcutHint,
    mode: FooterMode,
    bindings: crate::config::Shortcuts,
) {
    draw_key_label_segment(screen, &hint_key_for_mode(hint, mode, bindings), hint.label);
}

fn draw_key_label_segment(screen: &mut Screen, key: &str, label: &str) {
    screen.set_style(shortcut_key_style());
    print(screen, " ");
    print(screen, key);
    print(screen, " ");
    screen.set_style(separator_style(BASE, LAVENDER));
    screen.print(POWERLINE_RIGHT);
    screen.set_style(shortcut_label_style());
    print(screen, " ");
    print(screen, label);
    print(screen, " ");
    screen.set_style(separator_style(LAVENDER, BASE));
    screen.print(POWERLINE_RIGHT);
}

/// Return one-based, half-open footer targets. Specific characters in grouped
/// keys precede the whole-hint fallback, so clicking `p` in `n/p` selects `p`
/// while its label and padding select the first displayed key.
#[cfg(test)]
pub(crate) fn footer_hitboxes(
    columns: usize,
    normal: bool,
    session: bool,
) -> Vec<(usize, usize, u8)> {
    footer_hitboxes_with_shortcuts(
        columns,
        normal,
        session,
        crate::config::Shortcuts::default(),
    )
}

#[cfg(test)]
pub(crate) fn footer_hitboxes_with_shortcuts(
    columns: usize,
    normal: bool,
    session: bool,
    bindings: crate::config::Shortcuts,
) -> Vec<(usize, usize, u8)> {
    footer_hitboxes_for_mode(
        columns,
        if normal {
            FooterMode::Normal
        } else {
            FooterMode::Locked
        },
        session,
        bindings,
    )
}

pub(crate) fn footer_hitboxes_for_mode(
    columns: usize,
    mode: FooterMode,
    session: bool,
    bindings: crate::config::Shortcuts,
) -> Vec<(usize, usize, u8)> {
    let mut hitboxes = Vec::new();
    let shortcuts = footer_shortcuts(columns, mode, session, bindings);
    let mut used = footer_mode_width(columns, mode) + usize::from(!shortcuts.is_empty());
    for hint in shortcuts {
        let width = shortcut_width(hint, mode, bindings);
        let mut key_offset = 0;
        for (index, (_, action)) in hint.actions.iter().enumerate() {
            if index != 0 {
                key_offset += 1;
            }
            let action = if mode == FooterMode::Pane {
                pane_action(*action).and_then(|action| bindings.pane_key(action))
            } else {
                Some(*action)
            };
            if let Some(action) = action {
                let width = display_width(&displayed_key(action));
                let width = if mode == FooterMode::Pane { width } else { 1 };
                let column = used + 2 + key_offset;
                hitboxes.push((column, column + width, action));
                key_offset += width;
            }
        }
        if let Some((_, action)) = hint.actions.first() {
            let action = if mode == FooterMode::Pane {
                pane_action(*action).and_then(|action| bindings.pane_key(action))
            } else {
                Some(*action)
            };
            if let Some(action) = action {
                hitboxes.push((used + 1, used + width + 1, action));
            }
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

fn draw_colored_powerline_segment(
    screen: &mut Screen,
    label: &str,
    background: Color,
    next_background: Color,
    remaining: &mut usize,
) {
    if *remaining < 3 {
        return;
    }
    screen.set_style(separator_style(BASE, background));
    screen.print(POWERLINE_RIGHT);
    *remaining -= 1;

    let label = clipped(label, remaining.saturating_sub(1));
    let label_width = display_width(&label);
    screen.set_style(Style {
        foreground: BADGE_TEXT,
        background,
        bold: true,
        ..Style::default()
    });
    print(screen, &label);
    *remaining -= label_width;

    screen.set_style(separator_style(background, next_background));
    screen.print(POWERLINE_RIGHT);
    *remaining -= 1;
}

fn draw_powerline_segment(screen: &mut Screen, label: &str, active: bool, remaining: &mut usize) {
    draw_colored_powerline_segment(
        screen,
        label,
        if active { GREEN } else { TEXT },
        BASE,
        remaining,
    );
}

fn mode_label(mode: FooterMode) -> &'static str {
    match mode {
        FooterMode::Normal => " NORMAL ",
        FooterMode::Pane => " PANE ",
        FooterMode::Locked => " LOCKED ",
    }
}

fn footer_mode_width(columns: usize, mode: FooterMode) -> usize {
    display_width(mode_label(mode)).min(columns)
}

fn footer_shortcuts(
    columns: usize,
    mode: FooterMode,
    session: bool,
    bindings: crate::config::Shortcuts,
) -> Vec<ShortcutHint> {
    let remaining = columns
        .saturating_sub(footer_mode_width(columns, mode))
        .saturating_sub(1);
    visible_shortcuts(remaining, mode, session, bindings)
}

fn draw_footer(
    screen: &mut Screen,
    row: usize,
    columns: usize,
    mode: FooterMode,
    session: bool,
    bindings: crate::config::Shortcuts,
) {
    screen.set_origin_mode(false);
    screen.set_insert_mode(false);
    screen.set_auto_wrap(false);
    screen.designate_character_set(false, false);
    screen.select_character_set(false);
    screen.set_style(bar_background_style());
    screen.position(row, 0);
    screen.erase_line(EraseMode::All);
    let mode_width = footer_mode_width(columns, mode);
    let shortcuts = footer_shortcuts(columns, mode, session, bindings);
    screen.set_style(Style {
        foreground: BADGE_TEXT,
        background: match mode {
            FooterMode::Normal => GREEN,
            FooterMode::Pane => LAVENDER,
            FooterMode::Locked => RED,
        },
        bold: true,
        ..Style::default()
    });
    print(screen, &clipped(mode_label(mode), mode_width));
    if !shortcuts.is_empty() {
        screen.set_style(bar_background_style());
        print(screen, " ");
    }
    for hint in shortcuts {
        draw_shortcut_segment(screen, hint, mode, bindings);
    }
}

fn history_hint_width((key, label): (&str, &str)) -> usize {
    key.width() + label.width() + 6
}

fn visible_history_hints<'a>(
    columns: usize,
    hints: &'a [(&'a str, &'a str)],
) -> Vec<(&'a str, &'a str)> {
    let available = history_footer_content_columns(columns);
    let minimum_status = available.min(HISTORY_MINIMUM_STATUS_COLUMNS);
    let mut remaining = available.saturating_sub(minimum_status);
    let mut visible = Vec::new();
    for hint in hints {
        let width = history_hint_width(*hint);
        if width > remaining.saturating_sub(1) {
            break;
        }
        visible.push(*hint);
        remaining -= width;
    }
    visible
}

pub(crate) fn history_footer_status_columns(columns: usize, hints: &[(&str, &str)]) -> usize {
    let available = history_footer_content_columns(columns);
    let hints = visible_history_hints(columns, hints);
    let hint_width: usize = hints.iter().copied().map(history_hint_width).sum();
    available
        .saturating_sub(hint_width)
        .saturating_sub(usize::from(!hints.is_empty()))
}

pub(crate) fn draw_history_footer(
    screen: &mut Screen,
    status: &str,
    hints: &[(&str, &str)],
) -> Option<usize> {
    let (rows, columns) = screen.dimensions();
    if rows < 3 || columns == 0 {
        return None;
    }
    let row = rows - 1;
    screen.save_cursor();
    screen.set_origin_mode(false);
    screen.set_insert_mode(false);
    screen.set_auto_wrap(false);
    screen.designate_character_set(false, false);
    screen.select_character_set(false);
    screen.set_style(bar_background_style());
    screen.position(row, 0);
    screen.erase_line(EraseMode::All);
    screen.set_style(Style {
        foreground: BADGE_TEXT,
        background: PEACH,
        bold: true,
        ..Style::default()
    });
    let mode = clipped(HISTORY_MODE, columns);
    print(screen, &mode);
    let used = mode.width();
    let content_start = used + usize::from(used < columns);
    if used < columns {
        screen.set_style(bar_background_style());
        print(screen, " ");
        let visible_hints = visible_history_hints(columns, hints);
        let status_columns = history_footer_status_columns(columns, hints);
        print(screen, &clipped(status, status_columns));
        if !visible_hints.is_empty() {
            print(screen, " ");
        }
        for (key, label) in visible_hints {
            draw_key_label_segment(screen, key, label);
        }
    }
    screen.restore_cursor();
    Some(content_start.min(columns.saturating_sub(1)))
}

pub(crate) fn history_footer_content_columns(columns: usize) -> usize {
    columns.saturating_sub(HISTORY_MODE.width() + 1)
}

struct BarLayout {
    labels: Vec<String>,
    session: String,
    session_width: usize,
    start: usize,
}

fn bar_layout(
    columns: usize,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
) -> BarLayout {
    let labels: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| powerline_label(index, name))
        .collect();
    let widths: Vec<_> = labels.iter().map(|label| powerline_width(label)).collect();
    let active_width = widths.get(active).copied().unwrap_or(0).min(columns);
    let session = session_name
        .map(|name| {
            clipped(
                &format!(" Rustmux ({name}) "),
                columns.saturating_sub(active_width),
            )
        })
        .unwrap_or_default();
    let session_width = display_width(&session);
    let available = columns.saturating_sub(session_width);
    let mut start = 0;
    while start < active && widths[start..=active].iter().sum::<usize>() > available {
        start += 1;
    }
    BarLayout {
        labels,
        session,
        session_width,
        start,
    }
}

pub(crate) fn window_hitboxes(
    columns: usize,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
) -> Vec<(usize, usize, usize)> {
    let layout = bar_layout(columns, session_name, names, active);
    let mut hitboxes = Vec::new();
    let mut used = layout.session_width;
    let mut remaining = columns.saturating_sub(used);
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

pub(crate) fn active_window_name_cursor_column(
    columns: usize,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
) -> Option<usize> {
    let layout = bar_layout(columns, session_name, names, active);
    let mut used = layout.session_width;
    let mut remaining = columns.saturating_sub(used);
    for (index, label) in layout.labels.iter().enumerate().skip(layout.start) {
        if remaining < 3 {
            break;
        }
        let visible = clipped(label, remaining - 2);
        let label_width = display_width(&visible);
        if index == active {
            let prefix = clipped(
                &format!(" {} {}", index + 1, names.get(index)?),
                label_width,
            );
            let offset = display_width(&prefix).min(label_width.saturating_sub(1));
            return Some((used + 1 + offset).min(columns.saturating_sub(1)));
        }
        let segment_width = label_width + 2;
        used += segment_width;
        remaining -= segment_width;
    }
    None
}

#[cfg(test)]
pub(crate) fn compose(
    child: &Screen,
    outer_rows: u16,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
    normal_mode: bool,
) -> io::Result<Screen> {
    compose_with_shortcuts(
        child,
        outer_rows,
        session_name,
        names,
        active,
        normal_mode,
        crate::config::Shortcuts::default(),
    )
}

#[cfg(test)]
pub(crate) fn compose_with_shortcuts(
    child: &Screen,
    outer_rows: u16,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
    normal_mode: bool,
    shortcuts: crate::config::Shortcuts,
) -> io::Result<Screen> {
    compose_with_mode(
        child,
        outer_rows,
        session_name,
        names,
        active,
        if normal_mode {
            FooterMode::Normal
        } else {
            FooterMode::Locked
        },
        shortcuts,
    )
}

pub(crate) fn compose_with_mode(
    child: &Screen,
    outer_rows: u16,
    session_name: Option<&str>,
    names: &[String],
    active: usize,
    mode: FooterMode,
    shortcuts: crate::config::Shortcuts,
) -> io::Result<Screen> {
    let mut screen = child.clone();
    if screen.is_alternate()
        && screen.alternate_scroll()
        && screen.mouse_tracking() == MouseTracking::Off
    {
        // Rustmux translates wheel reports to cursor keys. Ask for SGR button
        // reports even when there is no chrome row to request drag tracking.
        screen.set_sgr_mouse(true);
        screen.set_mouse_tracking(MouseTracking::Button);
    }
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
    let layout = bar_layout(columns, session_name, names, active);
    screen.set_style(Style {
        bold: true,
        ..bar_background_style()
    });
    print(&mut screen, &layout.session);
    let mut remaining = columns.saturating_sub(layout.session_width);
    for (index, label) in layout.labels.iter().enumerate().skip(layout.start) {
        draw_powerline_segment(&mut screen, label, index == active, &mut remaining);
        if remaining < 3 {
            break;
        }
    }
    if footer {
        let row = screen.dimensions().0 - 1;
        draw_footer(
            &mut screen,
            row,
            columns,
            mode,
            session_name.is_some(),
            shortcuts,
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
            pane_border_style(true, false, false),
            Style {
                foreground: GREEN,
                background: Color::Default,
                bold: true,
                ..Style::default()
            }
        );
        assert_eq!(pane_border_style(false, false, false).foreground, SUBTEXT0);
        assert_eq!(pane_border_style(false, true, false).foreground, PEACH);
        assert_eq!(pane_border_style(false, false, true).foreground, PEACH);
        assert_eq!(pane_border_style(true, false, true).foreground, GREEN);
        for style in [
            pane_border_style(false, false, false),
            pane_border_style(false, true, false),
            pane_border_style(false, false, true),
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
        assert!(footer.contains("LOCKED"));
        assert!(footer.contains("Ctrl-B"));
        assert!(footer.contains("Commands"));
        assert!(!footer.contains("Ctrl-B Ctrl-W Sessions"));
        let footer_row = view.row(4).unwrap();
        assert_eq!(footer_row[0].style.background, RED);
        assert!(footer_row[0].style.bold);
        assert_eq!(footer_row[8].style, bar_background_style());
        assert_eq!(footer_row[9].style, shortcut_key_style());
        assert_eq!(footer_row[10].style, shortcut_key_style());
        assert_eq!(footer_row[17].style, separator_style(BASE, LAVENDER));
        assert_eq!(footer_row[18].style, shortcut_label_style());

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
    fn alternate_scroll_requests_outer_mouse_reports_without_changing_child() {
        let mut child = Screen::new(2, 20).unwrap();
        Parser::new().advance(&mut child, b"\x1b[?1007h\x1b[?1049h");
        let before = child.clone();

        let hidden = compose(&child, 1, None, &["shell".into()], 0, false).unwrap();
        assert_eq!(hidden.mouse_tracking(), MouseTracking::Button);
        assert!(hidden.sgr_mouse());

        let visible = compose(&child, 4, None, &["shell".into()], 0, false).unwrap();
        assert_eq!(visible.mouse_tracking(), MouseTracking::Drag);
        assert!(visible.sgr_mouse());
        assert_eq!(child, before);

        child.set_mouse_tracking(MouseTracking::Any);
        child.set_sgr_mouse(false);
        let tracked = compose(&child, 4, None, &["shell".into()], 0, false).unwrap();
        assert_eq!(tracked.mouse_tracking(), MouseTracking::Any);
        assert!(!tracked.sgr_mouse());
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
    fn rectangular_mode_badge_is_left_aligned_and_leaves_the_top_bar_free() {
        let child = Screen::new(2, 40).unwrap();
        let locked = compose(&child, 4, None, &["shell".into()], 0, false).unwrap();
        let normal = compose(&child, 4, None, &["shell".into()], 0, true).unwrap();
        let locked_row = locked.row(3).unwrap();
        let normal_row = normal.row(3).unwrap();
        assert_eq!(locked_row[0].style.background, RED);
        assert_eq!(normal_row[0].style.background, GREEN);
        assert_eq!(locked_row[0].character, ' ');
        assert_eq!(locked_row[1].character, 'L');
        assert_eq!(normal_row[1].character, 'N');
        assert_ne!(locked_row[0].character, POWERLINE_RIGHT);
        let top: String = locked
            .row(0)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(top.contains("1 shell"));
        assert!(!top.contains("LOCKED"));

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
        assert!(text(&locked).contains("LOCKED"));
        assert!(text(&locked).contains("Ctrl-B"));
        assert!(text(&locked).contains("Commands"));
        assert!(!text(&locked).contains("Sessions"));
        assert!(text(&normal).contains("NORMAL"));
        assert!(text(&normal).contains('c'));
        assert!(text(&normal).contains("New"));
        assert!(text(&normal).contains("Split →"));
        assert!(text(&normal).contains("Split ↓"));

        let named_locked = compose(&child, 3, Some("work"), &["shell".into()], 0, false).unwrap();
        assert!(!text(&named_locked).contains("Ctrl-W"));
        let named = compose(&child, 3, Some("work"), &["shell".into()], 0, true).unwrap();
        assert!(text(&named).contains("Ctrl-W"));
        assert!(text(&named).contains("Sessions"));

        let narrow = Screen::new(1, 20).unwrap();
        let normal = compose(&narrow, 3, None, &["shell".into()], 0, true).unwrap();
        let footer: String = normal
            .row(2)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(footer.contains("NORMAL"));
        assert!(footer.contains('?'));
        assert!(footer.contains("Help"));
        assert!(!footer.contains("New"));
        assert!(!footer.contains("Split ↓"));
        assert!(!footer.contains("Focus"));

        let mut history = compose(&child, 3, None, &["shell".into()], 0, false).unwrap();
        let saved_top = history.row(0).unwrap().to_vec();
        let saved_cursor = history.cursor();
        let hints = &[("/?", "Search"), ("q", "Exit")];
        assert_eq!(draw_history_footer(&mut history, "2/40", hints), Some(10));
        assert_eq!(history.row(0).unwrap(), saved_top);
        assert_eq!(history.cursor(), saved_cursor);
        let footer = history.row(2).unwrap();
        let text: String = footer
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(text.starts_with(" HISTORY  2/40"));
        assert!(text.contains("/?"));
        assert!(text.contains("Search"));
        assert!(text.contains("q"));
        assert!(text.contains("Exit"));
        assert_eq!(footer[0].style.background, PEACH);
        assert_eq!(footer[9].style.background, BASE);
        let search = text.find("Search").unwrap();
        assert_eq!(footer[search].style.background, LAVENDER);
        assert_eq!(history_footer_content_columns(80), 70);
        assert_eq!(history_footer_content_columns(8), 0);
        assert_eq!(history_footer_status_columns(80, hints), 44);

        let narrow_child = Screen::new(1, 40).unwrap();
        let mut narrow = compose(&narrow_child, 3, None, &["shell".into()], 0, false).unwrap();
        draw_history_footer(&mut narrow, "Search /still-in-history", hints);
        let narrow_text: String = narrow
            .row(2)
            .unwrap()
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.character)
            .collect();
        assert!(narrow_text.contains("Search /still-in-history"));
        assert!(!narrow_text.contains("/?"));
    }

    #[test]
    fn window_hitboxes_follow_the_rendered_segments_only() {
        let names = vec!["first".into(), "second".into(), "third".into()];
        assert_eq!(
            active_window_name_cursor_column(80, None, &names, 0),
            Some(9)
        );
        assert_eq!(
            window_hitboxes(80, None, &names, 0),
            vec![(1, 12, 0), (12, 24, 1), (24, 35, 2)]
        );

        assert_eq!(
            active_window_name_cursor_column(80, Some("work"), &names, 0),
            Some(25)
        );
        let session = window_hitboxes(80, Some("work"), &names, 0);
        assert_eq!(session[0], (17, 28, 0));
        assert!(session.iter().all(|(_, end, _)| *end <= 71));

        let cursor = active_window_name_cursor_column(12, None, &names, 2).unwrap();
        assert!(cursor < 12);
        let narrow = window_hitboxes(12, None, &names, 2);
        assert_eq!(narrow.len(), 1);
        assert_eq!(narrow[0].2, 2);

        assert_eq!(
            active_window_name_cursor_column(80, None, &[String::new()], 0),
            Some(4)
        );
    }

    #[test]
    fn footer_hitboxes_follow_visible_hints_and_split_grouped_keys() {
        let locked = footer_hitboxes(80, false, true);
        assert!(
            locked
                .iter()
                .any(|&(start, end, key)| { start == 10 && end == 30 && key == 2 })
        );
        assert!(
            !locked.iter().any(|&(_, _, key)| key == 23),
            "Ctrl-W appears only after entering NORMAL mode"
        );

        let normal = footer_hitboxes(120, true, false);
        let action_at = |column| {
            normal
                .iter()
                .find(|(start, end, _)| column >= *start && column < *end)
                .map(|(_, _, action)| *action)
        };
        assert_eq!(action_at(11), Some(b'c'));
        assert_eq!(action_at(49), Some(b'h'));
        assert_eq!(action_at(51), Some(b'j'));
        assert_eq!(action_at(57), Some(b'h'));
        assert_eq!(action_at(67), Some(b'n'));
        assert_eq!(action_at(69), Some(b'p'));
        assert_eq!(action_at(82), Some(b'Z'));
        assert_eq!(action_at(93), Some(b'?'));

        let session = footer_hitboxes(120, true, true);
        let session_action_at = |column| {
            session
                .iter()
                .find(|(start, end, _)| column >= *start && column < *end)
                .map(|(_, _, action)| *action)
        };
        assert_eq!(session_action_at(82), Some(23));
        assert_eq!(session_action_at(102), Some(b'?'));

        let narrow = footer_hitboxes(20, true, true);
        assert!(!narrow.iter().any(|&(_, _, key)| key == b'c'));
        assert!(narrow.iter().any(|&(_, _, key)| key == b'?'));
        assert!(!narrow.iter().any(|&(_, _, key)| key == b'%'));
        assert!(!narrow.iter().any(|&(_, _, key)| key == b'"'));
    }

    #[test]
    fn configured_footer_keys_change_labels_not_click_actions() {
        let bindings = crate::config::Shortcuts::test_keys(*b"NRD");
        let child = Screen::new(20, 100).unwrap();
        let view =
            compose_with_shortcuts(&child, 22, None, &["shell".into()], 0, true, bindings).unwrap();
        let footer: String = view
            .row(21)
            .unwrap()
            .iter()
            .map(|cell| cell.character)
            .collect();
        assert!(footer.contains("N"));
        assert!(footer.contains("R"));
        assert!(footer.contains("D"));
        assert!(
            footer_hitboxes_with_shortcuts(100, true, false, bindings)
                .iter()
                .any(|&(_, _, action)| action == b'c')
        );
    }
}
