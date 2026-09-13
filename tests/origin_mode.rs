use rustmux::{parser::Parser, render::render, screen::Screen};

fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(6, 8).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(6, 8).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected, "split {split}");
    }
    let mut screen = Screen::new(6, 8).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}

#[test]
fn origin_toggle_homes_and_cup_uses_region_coordinates() {
    for (suffix, cursor, mode) in [
        ("", (1, 0), true),
        ("\x1b[2;3H", (2, 2), true),
        ("\x1b[999;999f", (4, 7), true),
        ("\x1b[0;0H", (1, 0), true),
        ("\x1b[3;4H\x1b[?6h", (1, 0), true),
        ("\x1b[?6l", (0, 0), false),
        ("\x1b[3;4r", (2, 0), true),
    ] {
        let screen = parsed(format!("\x1b[2;5r\x1b[?6h{suffix}").as_bytes());
        assert_eq!(screen.cursor(), cursor);
        assert_eq!(screen.origin_mode(), mode);
        assert!(!screen.wrap_pending());
    }
    let screen = parsed(b"\x1b[2;5r\x1b[?6hX\x1b[?6lY");
    assert_eq!(screen.row(1).unwrap()[0].character, 'X');
    assert_eq!(screen.row(0).unwrap()[0].character, 'Y');
}

#[test]
fn row_column_and_next_previous_line_commands_use_correct_bounds() {
    for (suffix, cursor) in [
        ("\x1b[99A", (1, 3)),
        ("\x1b[99B", (4, 3)),
        ("\x1b[99E", (4, 0)),
        ("\x1b[99F", (1, 0)),
        ("\x1b[E", (3, 0)),
        ("\x1b[0F", (1, 0)),
        ("\x1b[6G", (2, 5)),
        ("\x1b[`", (2, 0)),
        ("\x1b[99d", (4, 3)),
        ("\x1b[0d", (1, 3)),
    ] {
        let screen = parsed(format!("\x1b[2;5r\x1b[?6h\x1b[2;4H{suffix}").as_bytes());
        assert_eq!(screen.cursor(), cursor, "{suffix:?}");
    }
    assert_eq!(parsed(b"\x1b[2;5r\x1b[6d").cursor(), (5, 0));
}

#[test]
fn saved_origin_restores_and_clamps_against_changed_margins() {
    let screen = parsed(b"\x1b[2;5r\x1b[?6h\x1b[4;8HX\x1b7\x1b[?6l\x1b8");
    assert!(screen.origin_mode());
    assert_eq!(screen.cursor(), (4, 7));
    assert!(screen.wrap_pending());
    let screen = parsed(b"\x1b[2;5r\x1b[?6h\x1b[4;8HX\x1b7\x1b[2;3r\x1b8");
    assert_eq!(screen.cursor(), (2, 7));
    assert!(screen.origin_mode());
    assert!(!screen.wrap_pending());
    assert_eq!(screen.scroll_region(), (1, 2));
}

#[test]
fn alternate_and_resize_preserve_saved_origin() {
    let mut screen = parsed(b"\x1b[2;5r\x1b[?6h\x1b[2;3H\x1b[?1049h\x1b[?6l\x1b[?1049l");
    assert!(screen.origin_mode());
    assert_eq!(screen.cursor(), (2, 2));
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"\x1b7\x1b[?1049h\x1b[?6l");
    screen.resize(3, 4).unwrap();
    assert!(!screen.origin_mode());
    parser.advance(&mut screen, b"\x1b[?1049l\x1b8");
    assert!(screen.origin_mode());
    assert_eq!(screen.scroll_region(), (0, 2));
    assert_eq!(screen.cursor(), (2, 2));
    let before = screen.clone();
    screen.resize(3, 4).unwrap();
    assert_eq!(screen, before);
    assert!(screen.resize(0, 4).is_err());
    assert_eq!(screen, before);
}

#[test]
fn malformed_commands_do_not_change_origin_or_cursor() {
    let prefix = b"\x1b[2;5r\x1b[?6h\x1b[2;8HX";
    let before = parsed(prefix);
    for suffix in [
        "\x1b[6l",
        "\x1b[?6:1l",
        "\x1b[?999999999999999999999999l",
        "\x1b[1;2G",
        "\x1b[?2d",
        "\x1b[1:2E",
        "\x1b[1;2F",
    ] {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(suffix.as_bytes());
        assert_eq!(parsed(&bytes), before);
    }
}

#[test]
fn rendering_uses_physical_rows_and_single_row_origin_is_valid() {
    let screen = parsed(b"\x1b[2;5r\x1b[?6h\x1b[2;3HX");
    let mut frame = Vec::new();
    render(&screen, &mut frame).unwrap();
    let mut replay = Screen::new(6, 8).unwrap();
    Parser::new().advance(&mut replay, &frame);
    assert_eq!(replay.cursor(), screen.cursor());
    for row in 0..6 {
        assert_eq!(replay.row(row), screen.row(row));
    }
    let mut one = Screen::new(1, 1).unwrap();
    Parser::new().advance(&mut one, b"\x1b[?6h\x1b[99;99HX\x1b[99E");
    assert_eq!(one.cursor(), (0, 0));
    assert!(!one.wrap_pending());
}
