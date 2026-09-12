use rustmux::parser::Parser;
use rustmux::screen::Screen;
use rustmux::style::{Cell, Color, Style};

fn parsed(rows: usize, columns: usize, input: &[u8]) -> Screen {
    let mut expected = Screen::new(rows, columns).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(rows, columns).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected, "split at {split}");
    }
    let mut screen = Screen::new(rows, columns).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}

#[test]
fn attributes_and_individual_resets_are_saved_per_cell() {
    let screen = parsed(
        1,
        5,
        b"\x1b[1;2;3;4;5;7;8;9;31;44mA\x1b[22;23;24;25;27;28;29;39;49mB\x1b[31;;1mC\x1b[mD",
    );
    let row = screen.row(0).unwrap();
    assert_eq!(
        row[0],
        Cell {
            character: 'A',
            style: Style {
                foreground: Color::Indexed(1),
                background: Color::Indexed(4),
                bold: true,
                dim: true,
                italic: true,
                underline: true,
                blink: true,
                inverse: true,
                hidden: true,
                strikethrough: true,
            }
        }
    );
    assert_eq!(row[1].style, Style::default());
    assert_eq!(
        row[2].style,
        Style {
            bold: true,
            ..Style::default()
        }
    );
    assert_eq!(row[3].style, Style::default());
    assert_eq!(screen.style(), Style::default());
}

#[test]
fn all_sixteen_palette_colors_are_distinct_from_default() {
    for index in 0..16u8 {
        let (fg, bg) = if index < 8 {
            (30 + index, 40 + index)
        } else {
            (90 + index - 8, 100 + index - 8)
        };
        let screen = parsed(1, 3, format!("\x1b[{fg};{bg}mX\x1b[39;49mY").as_bytes());
        assert_eq!(
            screen.row(0).unwrap()[0].style,
            Style {
                foreground: Color::Indexed(index),
                background: Color::Indexed(index),
                ..Style::default()
            }
        );
        assert_eq!(screen.row(0).unwrap()[1].style, Style::default());
    }
}

#[test]
fn extended_colors_consume_components_and_preserve_other_attributes() {
    let screen = parsed(
        1,
        4,
        b"\x1b[1;38;5;255;48;2;0;127;255mA\x1b[38;2;1;2;3;48;5;0mB\x1b[58;2;1;2;3;999mC",
    );
    assert_eq!(
        screen.row(0).unwrap()[0].style,
        Style {
            foreground: Color::Indexed(255),
            background: Color::Rgb(0, 127, 255),
            bold: true,
            ..Style::default()
        }
    );
    let style = Style {
        foreground: Color::Rgb(1, 2, 3),
        background: Color::Indexed(0),
        bold: true,
        ..Style::default()
    };
    assert_eq!(screen.row(0).unwrap()[1].style, style);
    // Unsupported underline color still consumes its RGB values, not dim/italic.
    assert_eq!(screen.row(0).unwrap()[2].style, style);
}

#[test]
fn malformed_color_groups_and_parameter_overflow_do_not_partially_apply() {
    for params in [
        "1;38",
        "1;38;2;1;2",
        "1;38;2;;2;3",
        "1;38;5;256",
        "0;48;2;1;2;999",
        "1;38;9;2",
        "1;99999999999999999999999999999999",
    ] {
        let screen = parsed(1, 3, format!("\x1b[32mA\x1b[{params}mB").as_bytes());
        assert_eq!(
            screen.row(0).unwrap()[0].style,
            screen.row(0).unwrap()[1].style,
            "{params}"
        );
        assert!(!screen.style().bold);
    }
    let accepted = std::iter::repeat_n("1", 32).collect::<Vec<_>>().join(";");
    let screen = parsed(1, 2, format!("\x1b[{accepted}mA").as_bytes());
    assert!(screen.style().bold);
    let rejected = format!("{accepted};31");
    let screen = parsed(1, 2, format!("\x1b[{rejected}mA").as_bytes());
    assert_eq!(screen.style(), Style::default());
}

#[test]
fn sgr_keeps_pending_wrap_and_overwrite_only_changes_target_cell() {
    let screen = parsed(2, 3, b"\x1b[31mabc\x1b[32mD\x1b[1;2H\x1b[0mX");
    let row = screen.row(0).unwrap();
    assert_eq!(row[0].style.foreground, Color::Indexed(1));
    assert_eq!(
        row[1],
        Cell {
            character: 'X',
            style: Style::default()
        }
    );
    assert_eq!(row[2].style.foreground, Color::Indexed(1));
    assert_eq!(
        screen.row(1).unwrap()[0],
        Cell {
            character: 'D',
            style: Style {
                foreground: Color::Indexed(2),
                ..Style::default()
            }
        }
    );
    let screen = parsed(1, 3, b"abc\x1b[1m");
    assert_eq!(screen.cursor(), (0, 2));
    assert!(screen.wrap_pending());
    assert_eq!(screen.row(0).unwrap()[2].style, Style::default());
}

#[test]
fn erased_and_scrolled_blanks_use_background_without_decorations() {
    let blue_blank = Cell {
        character: ' ',
        style: Style {
            background: Color::Indexed(4),
            ..Style::default()
        },
    };
    for command in ["2K", "2J"] {
        let screen = parsed(1, 3, format!("abc\x1b[1;7;31;44m\x1b[{command}").as_bytes());
        assert_eq!(screen.row(0).unwrap(), &[blue_blank; 3]);
        assert!(screen.style().bold && screen.style().inverse);
    }
    let screen = parsed(2, 3, b"\x1b[31mabc\r\n\x1b[32mdef\x1b[1;44m\n");
    assert_eq!(
        screen.row(0).unwrap()[0],
        Cell {
            character: 'd',
            style: Style {
                foreground: Color::Indexed(2),
                ..Style::default()
            }
        }
    );
    assert_eq!(screen.row(1).unwrap(), &[blue_blank; 3]);
    assert_eq!(screen.cursor(), (1, 2));
}

#[test]
fn colon_colors_preserve_group_boundaries() {
    for rgb in ["38:2:255:0:0", "38:2::255:0:0", "38:2:0:255:0:0"] {
        let screen = parsed(1, 2, format!("\x1b[1;{rgb};48:2:0:51:0;4:3mX").as_bytes());
        assert_eq!(
            screen.row(0).unwrap()[0].style,
            Style {
                foreground: Color::Rgb(255, 0, 0),
                background: Color::Rgb(0, 51, 0),
                bold: true,
                ..Style::default()
            }
        );
    }
    let screen = parsed(1, 2, b"\x1b[38:5:123;48:5:45;58:2::1:2:3;3mX");
    assert_eq!(screen.style().foreground, Color::Indexed(123));
    assert_eq!(screen.style().background, Color::Indexed(45));
    assert!(screen.style().italic);
}

#[test]
fn malformed_colon_colors_are_atomic_and_do_not_affect_cursor_commands() {
    for params in [
        "38:2:255:0",
        "38:2:256:0:0",
        "38:2::1::3",
        "38:5:",
        "38:2:1:2:3:4:5",
        "38:2:1;2;3",
        "38;2:1:2:3",
        "38:2:1:255:0:0",
    ] {
        let screen = parsed(1, 4, format!("\x1b[32m\x1b[1;{params}mX").as_bytes());
        assert_eq!(
            screen.style(),
            Style {
                foreground: Color::Indexed(2),
                ..Style::default()
            },
            "{params}"
        );
    }
    let screen = parsed(2, 4, b"A\x1b[2:2HX");
    assert_eq!(screen.row(0).unwrap()[1].character, 'X');
}
