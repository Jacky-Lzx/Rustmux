use rustmux::{parser::Parser, screen::Screen, style::Color};

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

fn row(screen: &Screen, r: usize) -> String {
    screen
        .row(r)
        .unwrap()
        .iter()
        .map(|cell| cell.character)
        .collect()
}

#[test]
fn insert_and_replace_modes_are_distinct_and_toggling_does_not_move() {
    let screen = parsed("abcdefgh\x1b[1;3H\x1b[4hXY\x1b[4lZ");
    assert_eq!(row(&screen, 0), "abXYZdef");
    assert!(!screen.insert_mode());
    assert_eq!(screen.cursor(), (0, 5));
    let screen = parsed("abcdefgh\x1b[4h\x1b[4h");
    assert!(screen.insert_mode());
    assert_eq!(screen.cursor(), (0, 7));
    assert!(screen.wrap_pending());
    assert_eq!(row(&screen, 0), "abcdefgh");
}

#[test]
fn wide_characters_shift_by_columns_and_combining_marks_do_not_shift() {
    let screen = parsed("abcdefgh\x1b[1;2H\x1b[4h\x1b[38:2:1:2:3m中e\u{301}");
    assert_eq!(row(&screen, 0), "a中 ebcde");
    let cells = screen.row(0).unwrap();
    assert_eq!((cells[1].width, cells[2].width), (2, 0));
    assert_eq!(cells[3].combining, vec!['\u{301}']);
    assert_eq!(cells[1].style.foreground, Color::Rgb(1, 2, 3));
    assert_eq!(cells[4].style.foreground, Color::Default);
    let screen = parsed("A中BCDEF\x1b[1;3H\x1b[4hX");
    assert_eq!(row(&screen, 0), "A X BCDE");
    let screen = parsed("ABCDEF中\x1b[H\x1b[4hX");
    assert_eq!(row(&screen, 0), "XABCDEF ");
}

#[test]
fn insertion_happens_after_wrap_and_region_scrolling() {
    let screen = parsed("HEADER\x1b[2;1Habcdefgh\x1b[3;1H12345678\x1b[2;3r\x1b[3;8H\x1b[4hXY");
    assert_eq!(row(&screen, 0), "HEADER  ");
    assert_eq!(row(&screen, 1), "1234567X");
    assert_eq!(row(&screen, 2), "Y       ");
    let screen = parsed("abcdefgh\x1b[2;1H12345678\x1b[1;8H\x1b[4h中");
    assert_eq!(row(&screen, 0), "abcdefg ");
    assert_eq!(row(&screen, 1), "中 123456");
    let mut one = Screen::new(1, 1).unwrap();
    Parser::new().advance(&mut one, "\x1b[4h中X".as_bytes());
    assert_eq!(row(&one, 0), "X");
    assert!(one.wrap_pending());
}

#[test]
fn mode_is_global_across_cursor_saves_alternate_and_resize() {
    let mut screen = parsed("\x1b[4h\x1b7\x1b[4l\x1b8");
    assert!(!screen.insert_mode());
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"\x1b[?1049h\x1b[4h\x1b[?1049l");
    assert!(screen.insert_mode());
    screen.resize(4, 5).unwrap();
    assert!(screen.insert_mode());
    let before = screen.clone();
    assert!(screen.resize(0, 5).is_err());
    assert_eq!(screen, before);
}

#[test]
fn private_malformed_and_unknown_modes_do_not_enable_insertion() {
    for command in [
        "\x1b[?4h",
        "\x1b[4:1h",
        "\x1b[999999999999999999999999h",
        "\x1b[h",
        "\x1b[99h",
    ] {
        let screen = parsed(&format!("abcdefgh\x1b[1;3H{command}X"));
        assert_eq!(row(&screen, 0), "abXdefgh");
        assert!(!screen.insert_mode());
    }
    let screen = parsed("abcdefgh\x1b[1;3H\x1b[99;4hX\x1b[4;99lY");
    assert_eq!(row(&screen, 0), "abXYdefg");
    assert!(!screen.insert_mode());
}

#[test]
fn explicit_character_editing_does_not_double_insert_or_change_mode() {
    let screen = parsed("abcdefgh\x1b[1;3H\x1b[4h\x1b[@\x1b[P\x1b[X");
    assert_eq!(row(&screen, 0), "ab defg ");
    assert!(screen.insert_mode());
    assert_eq!(screen.cursor(), (0, 2));
}
