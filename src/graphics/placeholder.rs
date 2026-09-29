//! Decode Kitty Unicode image-placeholder cells without drawing pixels.
//! Table source: https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders
//! and its linked rowcolumn-diacritics.txt (Unicode 6.0 ordering).

use crate::style::{Cell, Color};

pub const PLACEHOLDER_CHAR: char = '\u{10eeee}';

// Values are their indices, not the Unicode code points. Keep the complete
// protocol table so row/column 0..296 and the high image-ID byte are exact.
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PlaceholderCell {
    pub image_id: u32,
    pub placement_id: Option<u32>,
    pub row: u32,
    pub column: u32,
}

fn diacritic_value(character: char) -> Option<u32> {
    DIACRITICS
        .binary_search(&(character as u32))
        .ok()
        .and_then(|index| u32::try_from(index).ok())
}

fn color_id(color: Color) -> Option<u32> {
    match color {
        Color::Default => None,
        Color::Indexed(index) => Some(u32::from(index)),
        Color::Rgb(red, green, blue) => {
            Some((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue))
        }
    }
}

/// Decode a physical screen row left-to-right. Missing diacritics inherit only
/// from the immediately preceding placeholder with identical foreground and
/// underline colors. A new row marker without a column starts at column zero;
/// an omitted high image-ID byte starts at zero unless it can be inherited.
/// Invalid or incomplete cells produce None, never a guessed image reference.
pub fn decode_row(cells: &[Cell]) -> Vec<Option<PlaceholderCell>> {
    let mut decoded = Vec::with_capacity(cells.len());
    let mut previous: Option<(PlaceholderCell, Color, Color)> = None;
    for cell in cells {
        let current = (|| {
            if cell.character != PLACEHOLDER_CHAR || cell.width != 1 {
                return None;
            }
            let low_id = color_id(cell.style.foreground)?;
            if cell.combining.len() > 3 {
                return None;
            }
            let mut marks = [None; 3];
            for (index, &character) in cell.combining.iter().enumerate() {
                marks[index] = Some(diacritic_value(character)?);
            }
            let left = previous
                .as_ref()
                .filter(|(_, foreground, underline)| {
                    *foreground == cell.style.foreground && *underline == cell.style.underline_color
                })
                .map(|(reference, _, _)| *reference);
            let row = marks[0].or_else(|| left.map(|cell| cell.row))?;
            let column = match marks[1] {
                Some(column) => column,
                None if marks[0].is_none() => left?.column.checked_add(1)?,
                None => left
                    .filter(|cell| cell.row == row)
                    .and_then(|cell| cell.column.checked_add(1))
                    .unwrap_or(0),
            };
            let high = match marks[2] {
                Some(high) => u8::try_from(high).ok()?,
                None => left
                    .filter(|cell| cell.row == row && cell.column.checked_add(1) == Some(column))
                    .map_or(0, |cell| (cell.image_id >> 24) as u8),
            };
            let image_id = low_id | (u32::from(high) << 24);
            if image_id == 0 {
                return None;
            }
            Some(PlaceholderCell {
                image_id,
                placement_id: color_id(cell.style.underline_color).filter(|id| *id != 0),
                row,
                column,
            })
        })();
        previous =
            current.map(|reference| (reference, cell.style.foreground, cell.style.underline_color));
        decoded.push(current);
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parser::Parser, screen::Screen, style::Style};

    fn cell(marks: &[char], foreground: Color, underline_color: Color) -> Cell {
        Cell {
            character: PLACEHOLDER_CHAR,
            combining: marks.to_vec(),
            style: Style {
                foreground,
                underline_color,
                ..Style::default()
            },
            ..Cell::default()
        }
    }

    #[test]
    fn protocol_diacritic_table_has_expected_endpoints() {
        assert_eq!(diacritic_value('\u{0305}'), Some(0));
        assert_eq!(diacritic_value('\u{030d}'), Some(1));
        assert_eq!(diacritic_value('\u{1d244}'), Some(296));
        assert_eq!(diacritic_value('x'), None);
        assert!(DIACRITICS.is_sorted());
    }

    #[test]
    fn explicit_and_compact_protocol_examples_decode() {
        let fg = Color::Indexed(42);
        let none = Color::Default;
        let rows = [
            vec![
                cell(&['\u{0305}', '\u{0305}'], fg, none),
                cell(&['\u{0305}', '\u{030d}'], fg, none),
            ],
            vec![cell(&['\u{030d}'], fg, none), cell(&[], fg, none)],
        ];
        assert_eq!(
            decode_row(&rows[0]),
            vec![
                Some(PlaceholderCell {
                    image_id: 42,
                    placement_id: None,
                    row: 0,
                    column: 0
                }),
                Some(PlaceholderCell {
                    image_id: 42,
                    placement_id: None,
                    row: 0,
                    column: 1
                }),
            ]
        );
        assert_eq!(
            decode_row(&rows[1]),
            vec![
                Some(PlaceholderCell {
                    image_id: 42,
                    placement_id: None,
                    row: 1,
                    column: 0
                }),
                Some(PlaceholderCell {
                    image_id: 42,
                    placement_id: None,
                    row: 1,
                    column: 1
                }),
            ]
        );
    }

    #[test]
    fn rgb_id_high_byte_and_placement_color_decode() {
        let foreground = Color::Rgb(0, 0, 42);
        let underline = Color::Rgb(0, 1, 2);
        let cells = [
            cell(&['\u{0305}', '\u{0305}', '\u{030e}'], foreground, underline),
            cell(&[], foreground, underline),
        ];
        assert_eq!(
            decode_row(&cells),
            vec![
                Some(PlaceholderCell {
                    image_id: 0x0200_002a,
                    placement_id: Some(0x0102),
                    row: 0,
                    column: 0
                }),
                Some(PlaceholderCell {
                    image_id: 0x0200_002a,
                    placement_id: Some(0x0102),
                    row: 0,
                    column: 1
                }),
            ]
        );
    }

    #[test]
    fn malformed_marks_and_changed_colors_do_not_inherit() {
        let fg = Color::Indexed(42);
        let valid = cell(&['\u{0305}', '\u{0305}', '\u{030e}'], fg, Color::Default);
        let invalid = cell(&['x'], fg, Color::Default);
        let other_color = cell(&[], Color::Indexed(43), Color::Default);
        let no_high_inheritance = cell(
            &['\u{0305}', '\u{030d}'],
            Color::Indexed(43),
            Color::Default,
        );
        assert_eq!(
            decode_row(&[valid, invalid, other_color, no_high_inheritance]),
            vec![
                Some(PlaceholderCell {
                    image_id: 0x0200_002a,
                    placement_id: None,
                    row: 0,
                    column: 0
                }),
                None,
                None,
                Some(PlaceholderCell {
                    image_id: 43,
                    placement_id: None,
                    row: 0,
                    column: 1
                }),
            ]
        );
    }

    #[test]
    fn full_table_coordinates_and_invalid_high_byte_are_bounded() {
        let foreground = Color::Indexed(7);
        let last = char::from_u32(DIACRITICS[296]).unwrap();
        let high_overflow = char::from_u32(DIACRITICS[256]).unwrap();
        let cells = [
            cell(&[last, last], foreground, Color::Indexed(9)),
            cell(
                &['\u{0305}', '\u{0305}', high_overflow],
                foreground,
                Color::Default,
            ),
        ];
        assert_eq!(
            decode_row(&cells),
            vec![
                Some(PlaceholderCell {
                    image_id: 7,
                    placement_id: Some(9),
                    row: 296,
                    column: 296,
                }),
                None,
            ]
        );
    }

    #[test]
    fn underline_color_change_breaks_implicit_coordinates() {
        let fg = Color::Indexed(42);
        let cells = [
            cell(&['\u{0305}', '\u{0305}'], fg, Color::Indexed(1)),
            cell(&[], fg, Color::Indexed(2)),
            cell(&['\u{0305}'], fg, Color::Indexed(2)),
        ];
        assert_eq!(decode_row(&cells)[1], None);
        assert_eq!(decode_row(&cells)[2].unwrap().column, 0);
    }

    #[test]
    fn parser_retains_placeholder_scalar_marks_and_colors() {
        let mut screen = Screen::new(1, 3).unwrap();
        let mut parser = Parser::new();
        parser.advance(
            &mut screen,
            "\x1b[38;5;42m\u{10eeee}\u{0305}\u{0305}\u{10eeee}".as_bytes(),
        );
        let row = screen.row(0).unwrap();
        assert_eq!(row[0].character, PLACEHOLDER_CHAR);
        assert_eq!(row[0].combining, ['\u{0305}', '\u{0305}']);
        assert_eq!(row[1].character, PLACEHOLDER_CHAR);
        assert_eq!(decode_row(row)[0].unwrap().image_id, 42);
        assert_eq!(decode_row(row)[1].unwrap().column, 1);
    }
}
