use rustmux::parser::Parser;
use rustmux::screen::Screen;
use rustmux::style::Color;

fn invariant(screen: &Screen) {
    for row in 0..screen.dimensions().0 {
        let cells = screen.row(row).unwrap();
        for (column, cell) in cells.iter().enumerate() {
            assert!(cell.combining.len() <= 16);
            match cell.width {
                0 => {
                    assert!(column > 0);
                    assert_eq!(cells[column - 1].width, 2);
                    assert_eq!(cell.style, cells[column - 1].style);
                    assert!(cell.combining.is_empty());
                }
                1 => {}
                2 => {
                    assert!(column + 1 < cells.len());
                    assert_eq!(cells[column + 1].width, 0);
                }
                _ => panic!("invalid cell width"),
            }
        }
    }
}

fn text(screen: &Screen, row: usize) -> String {
    screen
        .row(row)
        .unwrap()
        .iter()
        .filter(|cell| cell.width != 0)
        .flat_map(|cell| std::iter::once(cell.character).chain(cell.combining.iter().copied()))
        .collect()
}

fn parse(rows: usize, columns: usize, input: &[u8]) -> Screen {
    let mut expected = Screen::new(rows, columns).unwrap();
    let mut parser = Parser::new();
    parser.advance(&mut expected, input);
    parser.finish(&mut expected);
    invariant(&expected);
    for split in 0..=input.len() {
        let mut screen = Screen::new(rows, columns).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        invariant(&screen);
        parser.advance(&mut screen, &input[split..]);
        parser.finish(&mut screen);
        invariant(&screen);
        assert_eq!(screen, expected, "split at {split}");
    }
    let mut screen = Screen::new(rows, columns).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
        invariant(&screen);
    }
    parser.finish(&mut screen);
    assert_eq!(screen, expected);
    expected
}

#[test]
fn utf8_scalars_and_wide_cells_preserve_styles() {
    let screen = parse(2, 8, "é\x1b[31m中😀\x1b[0mZ".as_bytes());
    assert_eq!(text(&screen, 0), "é中😀Z  ");
    assert_eq!(screen.cursor(), (0, 6));
    let cells = screen.row(0).unwrap();
    assert_eq!(cells[1].width, 2);
    assert_eq!(cells[2].width, 0);
    assert_eq!(cells[1].style.foreground, Color::Indexed(1));
    assert_eq!(cells[5].style.foreground, Color::Default);
}

#[test]
fn wide_wrap_scroll_and_single_column_policy() {
    let screen = parse(2, 4, "abc中".as_bytes());
    assert_eq!(text(&screen, 0), "abc ");
    assert_eq!(text(&screen, 1), "中  ");
    assert_eq!(screen.cursor(), (1, 2));
    let screen = parse(1, 4, "中文A".as_bytes());
    assert_eq!(text(&screen, 0), "A   ");
    let screen = parse(2, 1, "中A".as_bytes());
    assert_eq!(text(&screen, 0), "�");
    assert_eq!(text(&screen, 1), "A");
}

#[test]
fn overwriting_either_half_repairs_the_whole_glyph() {
    for (column, expected) in [(1, "X   "), (2, " X  ")] {
        let screen = parse(1, 4, format!("中\x1b[1;{column}HX").as_bytes());
        assert_eq!(text(&screen, 0), expected);
    }
    let screen = parse(1, 4, "中文\x1b[1;2H界".as_bytes());
    assert_eq!(text(&screen, 0), " 界 ");
}

#[test]
fn partial_erase_never_leaves_half_a_wide_glyph() {
    for command in ["K", "J"] {
        let screen = parse(1, 4, format!("A中B\x1b[1;3H\x1b[{command}").as_bytes());
        assert_eq!(text(&screen, 0), "A   ");
    }
    for command in ["1K", "1J"] {
        let screen = parse(1, 4, format!("A中B\x1b[1;2H\x1b[{command}").as_bytes());
        assert_eq!(text(&screen, 0), "   B");
    }
}

#[test]
fn combining_suffixes_stay_with_base_even_at_pending_wrap() {
    let screen = parse(1, 4, "\u{301}e\u{301}\x1b[31m中\u{302}".as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].combining, ['\u{301}']);
    assert_eq!(screen.row(0).unwrap()[1].combining, ['\u{302}']);
    assert_eq!(screen.cursor(), (0, 3));
    let screen = parse(1, 2, "中\x1b[31m\u{301}".as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].combining, ['\u{301}']);
    assert_eq!(screen.row(0).unwrap()[0].style.foreground, Color::Default);
    assert!(screen.wrap_pending());
    let input = format!("e{}", "\u{301}".repeat(100));
    let screen = parse(1, 4, input.as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].combining.len(), 16);
}

#[test]
fn malformed_utf8_matches_lossy_replacement_and_does_not_swallow_escape() {
    for input in [
        b"\xe0\x80\x80".as_slice(),
        b"\xed\xa0\x80",
        b"\xf4\x90\x80\x80",
        b"\xe4\xb8",
        b"\xffA\xc2B",
        b"\xf0\x9fX",
    ] {
        let expected = String::from_utf8_lossy(input);
        let screen = parse(1, 12, input);
        assert_eq!(text(&screen, 0), format!("{expected:<12}"));
    }
    let screen = parse(1, 5, b"\xe4\x1b[31mX");
    assert_eq!(text(&screen, 0), "�X   ");
    assert_eq!(
        screen.row(0).unwrap()[1].style.foreground,
        Color::Indexed(1)
    );
    let screen = parse(1, 5, "A\x1b]中\x07B".as_bytes());
    assert_eq!(text(&screen, 0), "AB   ");
}

#[test]
fn finish_replaces_truncated_text_once_and_resets_sequence_state() {
    let mut screen = Screen::new(1, 8).unwrap();
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"\xe4\xb8");
    assert_eq!(screen.cursor(), (0, 0));
    parser.finish(&mut screen);
    parser.finish(&mut screen);
    assert_eq!(text(&screen, 0), "�       ");
    parser.advance(&mut screen, b"\x1b[31");
    parser.finish(&mut screen);
    parser.advance(&mut screen, b"X");
    assert_eq!(text(&screen, 0), "�X      ");
    assert_eq!(screen.style().foreground, Color::Default);
}
