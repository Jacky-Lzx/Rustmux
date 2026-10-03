//! Per-session interface themes and independent fallback child terminal colors.

use crate::style::Color;

pub(crate) const DEFAULT_FOREGROUND_RGB: (u8, u8, u8) = (0xcd, 0xd6, 0xf4);
pub(crate) const DEFAULT_BACKGROUND_RGB: (u8, u8, u8) = (0x1e, 0x1e, 0x2e);
pub(crate) const DEFAULT_CURSOR_RGB: (u8, u8, u8) = (0xf5, 0xe0, 0xdc);

#[cfg(test)]
pub(crate) const DEFAULT_FOREGROUND: Color = Color::Rgb(
    DEFAULT_FOREGROUND_RGB.0,
    DEFAULT_FOREGROUND_RGB.1,
    DEFAULT_FOREGROUND_RGB.2,
);
#[cfg(test)]
pub(crate) const DEFAULT_BACKGROUND: Color = Color::Rgb(
    DEFAULT_BACKGROUND_RGB.0,
    DEFAULT_BACKGROUND_RGB.1,
    DEFAULT_BACKGROUND_RGB.2,
);

/// XTerm's conventional 256-color table: 16 ANSI colors, a 6x6x6 color cube,
/// then 24 grayscale entries. Pane-local OSC 4 overrides start from this table.
pub(crate) fn default_palette_color(index: u8) -> (u8, u8, u8) {
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match index {
        0..=15 => ANSI[usize::from(index)],
        16..=231 => {
            let offset = index - 16;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (
                component(offset / 36),
                component(offset / 6 % 6),
                component(offset % 6),
            )
        }
        232..=255 => {
            let value = 8 + (index - 232) * 10;
            (value, value, value)
        }
    }
}

use serde::Deserialize;
use std::collections::BTreeMap;

pub(crate) type Rgb = (u8, u8, u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Theme {
    pub badge_text: Rgb,
    pub background: Rgb,
    pub surface: Rgb,
    pub surface_highlight: Rgb,
    pub border: Rgb,
    pub muted: Rgb,
    pub foreground: Rgb,
    pub error: Rgb,
    pub accent: Rgb,
    pub orange: Rgb,
    pub warning: Rgb,
    pub blue: Rgb,
    pub secondary: Rgb,
    pub key: Rgb,
    pub purple: Rgb,
    pub teal: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            badge_text: (17, 17, 27),
            background: (30, 30, 46),
            surface: (49, 50, 68),
            surface_highlight: (69, 71, 90),
            border: (108, 112, 134),
            muted: (166, 173, 200),
            foreground: (205, 214, 244),
            error: (243, 139, 168),
            accent: (166, 227, 161),
            orange: (250, 179, 135),
            warning: (249, 226, 175),
            blue: (137, 180, 250),
            secondary: (180, 190, 254),
            key: (245, 194, 231),
            purple: (203, 166, 247),
            teal: (148, 226, 213),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThemeConfig {
    #[serde(default)]
    preset: Preset,
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Preset {
    #[default]
    Mocha,
    Light,
}

impl ThemeConfig {
    pub(crate) fn resolve(self) -> Result<Theme, String> {
        let mut theme = match self.preset {
            Preset::Mocha => Theme::default(),
            Preset::Light => Theme {
                badge_text: (255, 255, 255),
                background: (245, 246, 250),
                surface: (226, 230, 239),
                surface_highlight: (195, 202, 216),
                border: (103, 112, 132),
                muted: (85, 94, 114),
                foreground: (36, 43, 59),
                error: (174, 37, 58),
                accent: (24, 110, 74),
                orange: (165, 72, 17),
                warning: (135, 94, 0),
                blue: (38, 88, 174),
                secondary: (86, 73, 170),
                key: (163, 46, 119),
                purple: (113, 55, 170),
                teal: (16, 108, 117),
            },
        };
        for (name, value) in self.colors {
            let field = match name.as_str() {
                "badge_text" => &mut theme.badge_text,
                "background" => &mut theme.background,
                "surface" => &mut theme.surface,
                "surface_highlight" => &mut theme.surface_highlight,
                "border" => &mut theme.border,
                "muted" => &mut theme.muted,
                "foreground" => &mut theme.foreground,
                "error" => &mut theme.error,
                "accent" => &mut theme.accent,
                "orange" => &mut theme.orange,
                "warning" => &mut theme.warning,
                "blue" => &mut theme.blue,
                "secondary" => &mut theme.secondary,
                "key" => &mut theme.key,
                "purple" => &mut theme.purple,
                "teal" => &mut theme.teal,
                _ => return Err(format!("unknown theme color '{name}'")),
            };
            *field = parse_color(&value)
                .ok_or_else(|| format!("theme color '{name}' must be #RRGGBB, got '{value}'"))?;
        }
        Ok(theme)
    }
}

fn parse_color(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        u8::from_str_radix(&hex[..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..], 16).ok()?,
    ))
}

/// Convert interface colors without changing child terminal defaults.
pub(crate) const fn rgb((r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

impl Theme {
    pub(crate) fn report(self) -> BTreeMap<String, String> {
        [
            ("badge_text", self.badge_text),
            ("background", self.background),
            ("surface", self.surface),
            ("surface_highlight", self.surface_highlight),
            ("border", self.border),
            ("muted", self.muted),
            ("foreground", self.foreground),
            ("error", self.error),
            ("accent", self.accent),
            ("orange", self.orange),
            ("warning", self.warning),
            ("blue", self.blue),
            ("secondary", self.secondary),
            ("key", self.key),
            ("purple", self.purple),
            ("teal", self.teal),
        ]
        .into_iter()
        .map(|(name, (r, g, b))| (name.to_owned(), format!("#{r:02x}{g:02x}{b:02x}")))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reported_color_accepts_an_override_and_round_trips() {
        for name in Theme::default().report().keys() {
            let source = format!("[colors]\n{name} = '#01AbEF'");
            let theme = toml::from_str::<ThemeConfig>(&source)
                .unwrap()
                .resolve()
                .unwrap();
            assert_eq!(theme.report()[name], "#01abef");
            assert_eq!(
                theme
                    .report()
                    .iter()
                    .filter(|(key, value)| *value != &Theme::default().report()[*key])
                    .count(),
                1
            );
        }
    }

    #[test]
    fn interface_composition_is_local_and_preserves_child_cells_and_terminal_colors() {
        use crate::{layout::Layout, screen::Screen, style::Style};
        let layout = Layout::new(7, 80).unwrap();
        let (id, rect) = layout.content_geometry().panes[0];
        let mut child = Screen::new(rect.rows.into(), rect.columns.into()).unwrap();
        child.set_style(Style {
            foreground: Color::Rgb(201, 5, 19),
            background: Color::Rgb(4, 5, 6),
            ..Style::default()
        });
        child.print('X');
        let original = child.clone();
        let theme = toml::from_str::<ThemeConfig>("preset='light'\n[colors]\naccent='#010203'")
            .unwrap()
            .resolve()
            .unwrap();
        let frame =
            crate::pane_view::compose_themed(&layout, &[(id, &child)], None, &[], &[], theme)
                .unwrap();
        assert_eq!(frame.row(0).unwrap()[0].style.foreground, rgb(theme.accent));
        let view = crate::chrome::compose_with_mode(
            theme,
            &frame,
            9,
            None,
            &["shell".into()],
            0,
            crate::chrome::FooterMode::Locked,
            crate::config::Shortcuts::default(),
        )
        .unwrap();
        assert_eq!(
            view.row(0).unwrap()[79].style.background,
            rgb(theme.background)
        );
        let child_row = usize::from(rect.row) + 1;
        let column = usize::from(rect.column);
        assert_eq!(
            view.row(child_row).unwrap()[column],
            original.row(0).unwrap()[0]
        );
        assert_eq!(child, original);
        let default_view = crate::chrome::compose_with_mode(
            Theme::default(),
            &frame,
            9,
            None,
            &["shell".into()],
            0,
            crate::chrome::FooterMode::Locked,
            crate::config::Shortcuts::default(),
        )
        .unwrap();
        assert_eq!(
            default_view.row(0).unwrap()[79].style.background,
            rgb(Theme::default().background)
        );
        let colors = crate::terminal_colors::TerminalColors::default();
        assert_eq!(colors.foreground, DEFAULT_FOREGROUND_RGB);
        assert_eq!(colors.background, DEFAULT_BACKGROUND_RGB);
    }

    #[test]
    fn help_prompt_and_history_footer_use_the_supplied_theme() {
        use crate::{screen::Screen, style::Style};
        let theme = toml::from_str::<ThemeConfig>("preset='light'\n[colors]\nbackground='#010203'\nkey='#040506'\nsecondary='#070809'\npurple='#0a0b0c'\norange='#0d0e0f'").unwrap().resolve().unwrap();
        let mut child = Screen::new(24, 80).unwrap();
        child.set_style(Style {
            foreground: Color::Rgb(101, 102, 103),
            ..Style::default()
        });
        child.print('X');
        let original = child.clone();
        let mut help = crate::shortcut_help::ShortcutHelp::new(false);
        let help_view = help.overlay_themed(&child, theme);
        let contains_foreground = |screen: &Screen, color| {
            (0..screen.dimensions().0).any(|row| {
                screen
                    .row(row)
                    .unwrap()
                    .iter()
                    .any(|cell| cell.style.foreground == rgb(color))
            })
        };
        for color in [theme.key, theme.secondary, theme.purple] {
            assert!(contains_foreground(&help_view, color));
        }
        assert_eq!(help_view.row(0).unwrap(), original.row(0).unwrap());
        let prompt = crate::prompt::WindowPrompt::close();
        let prompt_view = prompt.overlay_themed(&child, theme);
        assert!(contains_foreground(&prompt_view, theme.key));
        assert!(contains_foreground(&prompt_view, theme.secondary));
        assert_eq!(prompt_view.row(1).unwrap(), original.row(1).unwrap());
        let mut footer_view = child.clone();
        crate::chrome::draw_history_footer(theme, &mut footer_view, "search", &[("/", "Search")]);
        assert_eq!(
            footer_view.row(23).unwrap()[0].style.background,
            rgb(theme.orange)
        );
        assert_eq!(
            footer_view.row(23).unwrap()[9].style.background,
            rgb(theme.background)
        );
        assert!(contains_foreground(&footer_view, theme.key));
        assert_eq!(child, original);
    }

    #[test]
    fn presets_accept_partial_overrides_without_changing_other_colors() {
        let config: ThemeConfig =
            toml::from_str("preset = 'light'\n[colors]\naccent = '#01Abef'\n").unwrap();
        let theme = config.resolve().unwrap();
        assert_eq!(theme.accent, (1, 171, 239));
        assert_eq!(theme.background, (245, 246, 250));
        assert_eq!(ThemeConfig::default().resolve().unwrap(), Theme::default());
    }

    #[test]
    fn theme_rejects_unknown_options_and_malformed_colors() {
        for source in [
            "preset = 'unknown'",
            "presett = 'light'",
            "[colors]\naccent = 123",
        ] {
            assert!(toml::from_str::<ThemeConfig>(source).is_err());
        }
        for source in [
            "[colors]\nacent = '#112233'",
            "[colors]\naccent = '#abc'",
            "[colors]\naccent = '112233'",
            "[colors]\naccent = '#gg0000'",
            "[colors]\naccent = '#é1234'",
        ] {
            assert!(
                toml::from_str::<ThemeConfig>(source)
                    .unwrap()
                    .resolve()
                    .is_err()
            );
        }
    }
}
