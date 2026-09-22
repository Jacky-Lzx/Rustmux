use rustmux::parser::{MAX_REPLY_BYTES, Parser};
use rustmux::screen::Screen;
use rustmux::style::{Cell, Color, Style, UnderlineStyle};

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

fn replies(input: &[u8]) -> (Screen, Vec<u8>) {
    let run = |split: usize| {
        let mut screen = Screen::new(2, 4).unwrap();
        let mut parser = Parser::new();
        let mut output = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            parser.advance_with_replies(&mut screen, chunk, &mut |reply| {
                assert!(reply.len() <= MAX_REPLY_BYTES);
                output.extend_from_slice(reply);
            });
        }
        (screen, output)
    };
    let expected = run(input.len());
    for split in 0..=input.len() {
        assert_eq!(run(split), expected, "split {split}");
    }
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
                underline_color: Color::Default,
                bold: true,
                dim: true,
                italic: true,
                underline: UnderlineStyle::Single,
                blink: true,
                inverse: true,
                hidden: true,
                strikethrough: true,
            },
            ..Cell::default()
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
fn status_string_reports_the_complete_current_style() {
    let (screen, output) = replies(
        b"\x1bP$qm\x1b\\\x1b[1;2;3;4:3;5;7;8;9;38;5;255;48;2;1;2;3;58;5;4m\x1bP$qm\x1b\\\x1b[0m\x1bP$qm\x1b\\",
    );
    assert_eq!(screen.style(), Style::default());
    assert_eq!(
        output,
        b"\x1bP1$r0m\x1b\\\x1bP1$r0;1;2;3;4:3;5;7;8;9;38;5;255;48;2;1;2;3;58;5;4m\x1b\\\x1bP1$r0m\x1b\\"
    );
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
fn extended_colors_apply_to_foreground_background_and_underline() {
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
    assert_eq!(
        screen.row(0).unwrap()[2].style,
        Style {
            underline_color: Color::Rgb(1, 2, 3),
            ..style
        }
    );
}

#[test]
fn malformed_color_groups_and_parameter_overflow_do_not_partially_apply() {
    for params in [
        "1;38",
        "1;38;2;1;2",
        "1;38;2;;2;3",
        "1;38;5;256",
        "1;58;5;256",
        "1;58;2;1;2",
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
            style: Style::default(),
            ..Cell::default()
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
            },
            ..Cell::default()
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
        ..Cell::default()
    };
    for command in ["2K", "2J"] {
        let screen = parsed(1, 3, format!("abc\x1b[1;7;31;44m\x1b[{command}").as_bytes());
        assert_eq!(screen.row(0).unwrap(), &vec![blue_blank.clone(); 3]);
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
            },
            ..Cell::default()
        }
    );
    assert_eq!(screen.row(1).unwrap(), &vec![blue_blank.clone(); 3]);
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
                underline: UnderlineStyle::Curly,
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
fn underline_styles_and_colors_are_independent_and_reset_separately() {
    let screen = parsed(
        1,
        9,
        b"\x1b[58;5;123;4mA\x1b[4:2mB\x1b[4:3;58:2::1:2:3mC\x1b[4:4mD\x1b[4:5mE\x1b[24mF\x1b[4:1;59mG\x1b[4:0mH\x1b[21mI",
    );
    let row = screen.row(0).unwrap();
    assert_eq!(row[0].style.underline, UnderlineStyle::Single);
    assert_eq!(row[0].style.underline_color, Color::Indexed(123));
    assert_eq!(row[1].style.underline, UnderlineStyle::Double);
    assert_eq!(row[2].style.underline, UnderlineStyle::Curly);
    assert_eq!(row[2].style.underline_color, Color::Rgb(1, 2, 3));
    assert_eq!(row[3].style.underline, UnderlineStyle::Dotted);
    assert_eq!(row[4].style.underline, UnderlineStyle::Dashed);
    assert_eq!(row[5].style.underline, UnderlineStyle::None);
    assert_eq!(row[5].style.underline_color, Color::Rgb(1, 2, 3));
    assert_eq!(row[6].style.underline, UnderlineStyle::Single);
    assert_eq!(row[6].style.underline_color, Color::Default);
    assert_eq!(row[7].style.underline, UnderlineStyle::None);
    assert_eq!(row[8].style.underline, UnderlineStyle::Double);
}

#[test]
fn unknown_underline_substyles_are_ignored_as_complete_groups() {
    let screen = parsed(1, 3, b"\x1b[31;4mA\x1b[4:6;32mB\x1b[4:;33mC");
    let row = screen.row(0).unwrap();
    assert_eq!(row[0].style.underline, UnderlineStyle::Single);
    assert_eq!(row[1].style.underline, UnderlineStyle::Single);
    assert_eq!(row[1].style.foreground, Color::Indexed(2));
    assert_eq!(row[2].style.underline, UnderlineStyle::Single);
    assert_eq!(row[2].style.foreground, Color::Indexed(3));
}

#[test]
fn malformed_colon_colors_are_atomic_and_do_not_affect_cursor_commands() {
    for params in [
        "38:2:255:0",
        "38:2:256:0:0",
        "38:2::1::3",
        "38:5:",
        "58:2::1:2",
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
