use rustmux::{parser::Parser, render::render, screen::Screen, style::Color};

fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(5, 4).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(5, 4).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected, "split {split}");
    }
    let mut screen = Screen::new(5, 4).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}

fn rows(screen: &Screen) -> Vec<String> {
    (0..screen.dimensions().0)
        .map(|r| screen.row(r).unwrap().iter().map(|c| c.character).collect())
        .collect()
}

const GRID: &str = "\x1b[1;1HAAAA\x1b[2;1HBBBB\x1b[3;1HCCCC\x1b[4;1HDDDD\x1b[5;1HEEEE";

#[test]
fn index_and_wrap_preserve_header_footer_and_styles() {
    for suffix in ["\n", "\x1bD", "X"] {
        let screen = parsed(format!("{GRID}\x1b[2;4r\x1b[4;4H\x1b[44;1mQ{suffix}").as_bytes());
        assert_eq!(
            rows(&screen),
            if suffix == "X" {
                vec!["AAAA", "CCCC", "DDDQ", "X   ", "EEEE"]
            } else {
                vec!["AAAA", "CCCC", "DDDQ", "    ", "EEEE"]
            }
        );
        let blank = &screen.row(3).unwrap()[2];
        assert_eq!(blank.style.background, Color::Indexed(4));
        assert_eq!(blank.style.foreground, Color::Default);
        assert!(!blank.style.bold);
        assert!(!screen.wrap_pending());
    }
}

#[test]
fn reverse_index_and_next_line_respect_margins_and_columns() {
    let screen = parsed(format!("{GRID}\x1b[2;4r\x1b[2;3H\x1bM").as_bytes());
    assert_eq!(rows(&screen), ["AAAA", "    ", "BBBB", "CCCC", "EEEE"]);
    assert_eq!(screen.cursor(), (1, 2));
    let screen = parsed(format!("{GRID}\x1b[2;4r\x1b[4;3H\x1bE").as_bytes());
    assert_eq!(rows(&screen), ["AAAA", "CCCC", "DDDD", "    ", "EEEE"]);
    assert_eq!(screen.cursor(), (3, 0));
    let screen = parsed(format!("{GRID}\x1b[2;4r\x1b[5;3H\n\x1b[1;3H\x1bM").as_bytes());
    assert_eq!(rows(&screen), ["AAAA", "BBBB", "CCCC", "DDDD", "EEEE"]);
    assert_eq!(screen.cursor(), (0, 2));
}

#[test]
fn margin_defaults_invalid_input_and_relative_movement() {
    for command in ["\x1b[r", "\x1b[;r", "\x1b[0;0r", "\x1b[1;5r"] {
        let screen = parsed(format!("abcd\x1b[2;4r\x1b[3;2H{command}").as_bytes());
        assert_eq!(screen.scroll_region(), (0, 4));
        assert_eq!(screen.cursor(), (0, 0));
        assert!(!screen.wrap_pending());
    }
    for command in [
        "\x1b[3;3r",
        "\x1b[4;2r",
        "\x1b[1;6r",
        "\x1b[2;4;1r",
        "\x1b[2:4r",
        "\x1b[?2;4r",
        "\x1b[999999999999999999999999;4r",
    ] {
        let screen = parsed(format!("\x1b[2;4r\x1b[3;1Habcd{command}").as_bytes());
        assert_eq!(screen.scroll_region(), (1, 3));
        assert_eq!(screen.cursor(), (2, 3));
        assert!(screen.wrap_pending());
    }
    for (position, motion, expected) in [
        ("3;2", "99A", (1, 1)),
        ("3;2", "99B", (3, 1)),
        ("1;2", "99A", (0, 1)),
        ("5;2", "99B", (4, 1)),
    ] {
        let screen = parsed(format!("\x1b[2;4r\x1b[{position}H\x1b[{motion}").as_bytes());
        assert_eq!(screen.cursor(), expected);
    }
}

#[test]
fn alternate_margins_are_isolated_and_resize_resets_both() {
    let mut screen = parsed(b"\x1b[2;4r\x1b7\x1b[?1049h\x1b[1;3r\x1b[?1049h");
    assert_eq!(screen.scroll_region(), (0, 2));
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"\x1b[?1049l");
    assert_eq!(screen.scroll_region(), (1, 3));
    parser.advance(&mut screen, b"\x1b[?1049h");
    assert_eq!(screen.scroll_region(), (0, 4));
    parser.advance(&mut screen, b"\x1b[2;3r");
    let before = screen.clone();
    screen.resize(5, 4).unwrap();
    assert_eq!(screen, before);
    assert!(screen.resize(0, 4).is_err());
    assert_eq!(screen, before);
    screen.resize(3, 6).unwrap();
    assert_eq!(screen.scroll_region(), (0, 2));
    parser.advance(&mut screen, b"\x1b[?1049l\x1b8");
    assert_eq!(screen.scroll_region(), (0, 2));
    // DECSC/DECRC restores the cursor, not scrolling margins.
    parser.advance(&mut screen, b"\x1b[1;2r\x1b8");
    assert_eq!(screen.scroll_region(), (0, 1));
}

#[test]
fn scrolling_moves_complete_unicode_cells_and_rendered_frames() {
    let screen = parsed("\x1b[2;4r\x1b[3;1H\x1b[38:2:255:0:0m中e\u{301}\x1b[4;1H\n".as_bytes());
    let row = screen.row(1).unwrap();
    assert_eq!((row[0].character, row[0].width, row[1].width), ('中', 2, 0));
    assert_eq!(row[2].combining, vec!['\u{301}']);
    assert_eq!(row[0].style.foreground, Color::Rgb(255, 0, 0));
    let mut frame = Vec::new();
    render(&screen, &mut frame).unwrap();
    let mut replay = Screen::new(5, 4).unwrap();
    Parser::new().advance(&mut replay, &frame);
    for row in 0..5 {
        assert_eq!(screen.row(row), replay.row(row));
    }
}

#[test]
fn a_single_row_screen_can_scroll_both_directions() {
    let mut screen = Screen::new(1, 1).unwrap();
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"\x1b[rX\x1bM");
    assert_eq!(rows(&screen), [" "]);
    assert!(!screen.wrap_pending());
    parser.advance(&mut screen, b"Y\x1bD");
    assert_eq!(rows(&screen), [" "]);
    assert_eq!(screen.scroll_region(), (0, 0));
}
