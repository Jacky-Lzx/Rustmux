use rustmux::{
    parser::Parser,
    render::render,
    screen::Screen,
    style::{Color, Style},
};

fn parsed(input: &str) -> Screen {
    let bytes = input.as_bytes();
    let mut expected = Screen::new(3, 8).unwrap();
    Parser::new().advance(&mut expected, bytes);
    for split in 0..=bytes.len() {
        let mut screen = Screen::new(3, 8).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &bytes[..split]);
        parser.advance(&mut screen, &bytes[split..]);
        assert_eq!(screen, expected, "split {split}");
    }
    let mut screen = Screen::new(3, 8).unwrap();
    let mut parser = Parser::new();
    for byte in bytes {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}

fn text(screen: &Screen, row: usize) -> String {
    screen
        .row(row)
        .unwrap()
        .iter()
        .map(|c| c.character)
        .collect()
}

#[test]
fn basic_operations_preserve_cursor_and_neighboring_rows() {
    for (command, expected) in [("@", "ab cdefg"), ("P", "abdefgh "), ("X", "ab defgh")] {
        for count in ["", "0", "1"] {
            let screen = parsed(&format!(
                "\x1b[2;3rTOP\x1b[2;1Habcdefgh\x1b[3;1HBOTTOM\x1b[2;3H\x1b[{count}{command}"
            ));
            assert_eq!(text(&screen, 1), expected);
            assert_eq!(text(&screen, 0), "TOP     ");
            assert_eq!(text(&screen, 2), "BOTTOM  ");
            assert_eq!(screen.cursor(), (1, 2));
            assert!(!screen.wrap_pending());
        }
        let screen = parsed(&format!("\x1b[2;3rabcdefgh\x1b[1;3H\x1b[{command}"));
        assert_eq!(text(&screen, 0), expected);
    }
}

#[test]
fn multiple_columns_large_counts_and_background_blanks() {
    for (command, expected) in [("2@", "ab  cdef"), ("2P", "abefgh  "), ("2X", "ab  efgh")] {
        assert_eq!(
            text(&parsed(&format!("abcdefgh\x1b[1;3H\x1b[{command}")), 0),
            expected
        );
    }
    for command in ["@", "P", "X"] {
        let screen = parsed(&format!(
            "abcdefgh\x1b[1;3H\x1b[1;31;44m\x1b[{}{command}",
            usize::MAX
        ));
        assert_eq!(text(&screen, 0), "ab      ");
        assert!(screen.style().bold);
        for cell in &screen.row(0).unwrap()[2..] {
            assert_eq!(
                cell.style,
                Style {
                    background: Color::Indexed(4),
                    ..Style::default()
                }
            );
        }
    }
}

#[test]
fn split_wide_glyphs_are_cleared_without_changing_shift_count() {
    for (input, expected) in [
        ("A中BCDEF\x1b[1;3H\x1b[@", "A   BCDE"),
        ("A中BCDEF\x1b[1;3H\x1b[P", "A BCDEF "),
        ("A中BCDEF\x1b[1;2H\x1b[P", "A BCDEF "),
        ("A中BCDEF\x1b[1;3H\x1b[X", "A  BCDEF"),
        ("ABCDEF中\x1b[H\x1b[@", " ABCDEF "),
    ] {
        assert_eq!(text(&parsed(input), 0), expected, "{input:?}");
    }
}

#[test]
fn intact_unicode_cells_keep_styles_and_suffixes_when_shifted() {
    for (command, column) in [("@", 2), ("P", 0)] {
        let screen = parsed(&format!(
            "A\x1b[38:2:1:2:3m中e\u{301}XYZ\x1b[H\x1b[{command}"
        ));
        let row = screen.row(0).unwrap();
        assert_eq!(
            (
                row[column].character,
                row[column].width,
                row[column + 1].width
            ),
            ('中', 2, 0)
        );
        assert_eq!(row[column].style.foreground, Color::Rgb(1, 2, 3));
        assert_eq!(row[column + 2].combining, vec!['\u{301}']);
        let mut frame = Vec::new();
        render(&screen, &mut frame).unwrap();
        let mut replay = Screen::new(3, 8).unwrap();
        Parser::new().advance(&mut replay, &frame);
        for row in 0..3 {
            assert_eq!(screen.row(row), replay.row(row));
        }
    }
}

#[test]
fn all_wide_boundaries_preserve_cell_pair_invariants() {
    for command in ["@", "P", "X"] {
        for column in 1..=8 {
            for count in 1..=9 {
                let screen = parsed(&format!(
                    "中e\u{301}文ABZ\x1b[1;{column}H\x1b[{count}{command}"
                ));
                let row = screen.row(0).unwrap();
                for (i, cell) in row.iter().enumerate() {
                    match cell.width {
                        0 => assert!(i > 0 && row[i - 1].width == 2),
                        2 => assert!(i + 1 < row.len() && row[i + 1].width == 0),
                        1 => {}
                        _ => panic!("invalid cell width"),
                    }
                }
            }
        }
    }
}

#[test]
fn malformed_commands_are_atomic_and_zero_model_count_is_noop() {
    let before = parsed("abcdefgh");
    for command in ["@", "P", "X"] {
        for params in ["?1", "1;2", "1:2", "99999999999999999999999999"] {
            assert_eq!(parsed(&format!("abcdefgh\x1b[{params}{command}")), before);
        }
        let screen = parsed(&format!("abcdefgh\x1b[{command}Z"));
        assert_eq!(text(&screen, 0), "abcdefgZ");
        assert_eq!(text(&screen, 1), "        ");
        let mut one = Screen::new(1, 1).unwrap();
        Parser::new().advance(&mut one, format!("X\x1b[{command}").as_bytes());
        assert_eq!(text(&one, 0), " ");
        assert!(!one.wrap_pending());
    }
    let mut screen = before.clone();
    screen.insert_characters(0);
    screen.delete_characters(0);
    screen.erase_characters(0);
    assert_eq!(screen, before);
    let mut screen = before.clone();
    Parser::new().advance(
        &mut screen,
        b"\x1b[?1049h\x1b[Htest\x1b[H\x1b[@\x1b[P\x1b[X\x1b[?1049l",
    );
    assert_eq!(screen, before);
}
