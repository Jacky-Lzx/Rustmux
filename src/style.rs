//! Stored text attributes. The renderer emits symbolic palette/default colors.

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub foreground: Color,
    pub background: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

/// A stored character with its width and zero-width suffix. The style is a snapshot:
/// subsequent SGR commands cannot restyle existing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub character: char,
    /// 1 for ordinary cells, 2 for a wide leader, 0 for its trailing placeholder.
    pub width: u8,
    /// At most 16 zero-width scalars; only leaders carry suffixes.
    pub combining: Vec<char>,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            character: ' ',
            width: 1,
            combining: Vec::new(),
            style: Style::default(),
        }
    }
}

impl Style {
    /// Apply semicolon SGR parameters atomically. Empty parameters mean reset.
    /// Unknown simple codes are ignored; malformed extended colors reject the
    /// command so their components cannot be mistaken for unrelated attributes.
    pub(crate) fn sgr(mut self, params: &[Option<usize>]) -> Option<Self> {
        let mut i = 0;
        while i < params.len() {
            match params[i].unwrap_or(0) {
                0 => self = Self::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                code @ 30..=37 => self.foreground = Color::Indexed((code - 30) as u8),
                code @ 40..=47 => self.background = Color::Indexed((code - 40) as u8),
                code @ 90..=97 => self.foreground = Color::Indexed((code - 90 + 8) as u8),
                code @ 100..=107 => self.background = Color::Indexed((code - 100 + 8) as u8),
                39 => self.foreground = Color::Default,
                49 => self.background = Color::Default,
                code @ (38 | 48 | 58) => {
                    let component =
                        |index| u8::try_from(params.get(index).copied().flatten()?).ok();
                    let color = match params.get(i + 1).copied().flatten()? {
                        5 => {
                            let color = Color::Indexed(component(i + 2)?);
                            i += 2;
                            color
                        }
                        2 => {
                            let color =
                                Color::Rgb(component(i + 2)?, component(i + 3)?, component(i + 4)?);
                            i += 4;
                            color
                        }
                        _ => return None,
                    };
                    match code {
                        38 => self.foreground = color,
                        48 => self.background = color,
                        // Underline color is unsupported, but consume its group.
                        _ => {}
                    }
                }
                _ => {}
            }
            i += 1;
        }
        Some(self)
    }
}
