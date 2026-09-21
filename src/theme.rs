//! Shared colors for Rustmux-owned chrome and pane defaults.

use crate::style::Color;

pub(crate) const DEFAULT_FOREGROUND_RGB: (u8, u8, u8) = (0xcd, 0xd6, 0xf4);
pub(crate) const DEFAULT_BACKGROUND_RGB: (u8, u8, u8) = (0x1e, 0x1e, 0x2e);
pub(crate) const DEFAULT_CURSOR_RGB: (u8, u8, u8) = (0xf5, 0xe0, 0xdc);

pub(crate) const DEFAULT_FOREGROUND: Color = Color::Rgb(
    DEFAULT_FOREGROUND_RGB.0,
    DEFAULT_FOREGROUND_RGB.1,
    DEFAULT_FOREGROUND_RGB.2,
);
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
