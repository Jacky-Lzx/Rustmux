//! Shared colors for Rustmux-owned chrome and pane defaults.

use crate::style::Color;

pub(crate) const DEFAULT_FOREGROUND_RGB: (u8, u8, u8) = (0xcd, 0xd6, 0xf4);
pub(crate) const DEFAULT_BACKGROUND_RGB: (u8, u8, u8) = (0x1e, 0x1e, 0x2e);

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
