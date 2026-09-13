use rustmux::{parser::Parser, screen::Screen};

fn parsed(input: &str) -> Screen {
    let bytes = input.as_bytes();
    let mut expected = Screen::new(3, 4).unwrap();
    Parser::new().advance(&mut expected, bytes);
    for split in 0..=bytes.len() {
        let mut screen = Screen::new(3, 4).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &bytes[..split]);
        parser.advance(&mut screen, &bytes[split..]);
        assert_eq!(screen, expected, "split {split}");
    }
    let mut screen = Screen::new(3, 4).unwrap();
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
fn disabled_wrap_overwrites_last_column_and_reenable_keeps_edge_state() {
    let screen = parsed("\x1b[?7labcdef");
    assert_eq!(row(&screen, 0), "abcf");
    assert_eq!(row(&screen, 1), "    ");
    assert_eq!(screen.cursor(), (0, 3));
    assert!(!screen.auto_wrap());
    assert!(!screen.wrap_pending());
    let screen = parsed("abcd\x1b[?7lX\x1b[?7hY");
    assert_eq!(row(&screen, 0), "abcX");
    assert_eq!(row(&screen, 1), "Y   ");
    assert!(screen.auto_wrap());
}

#[test]
fn wide_glyphs_and_combining_suffixes_remain_valid_without_wrap() {
    let screen = parsed("\x1b[?7lab中\u{301}");
    let cells = screen.row(0).unwrap();
    assert_eq!(
        (cells[2].character, cells[2].width, cells[3].width),
        ('中', 2, 0)
    );
    assert_eq!(cells[2].combining, vec!['\u{301}']);
    let screen = parsed("\x1b[?7labcd\u{301}文");
    assert_eq!(row(&screen, 0), "abcd");
    assert_eq!(screen.row(0).unwrap()[3].combining, vec!['\u{301}']);
    assert_eq!(screen.cursor(), (0, 3));
    let screen = parsed("\x1b[?7lab中X");
    assert_eq!(row(&screen, 0), "ab X");
    let mut one = Screen::new(1, 1).unwrap();
    Parser::new().advance(&mut one, "\x1b[?7l中\u{301}X".as_bytes());
    assert_eq!(row(&one, 0), "X");
    assert!(!one.wrap_pending());
}

#[test]
fn explicit_controls_and_insert_mode_work_without_implicit_scroll() {
    let screen = parsed("HEAD\x1b[2;3r\x1b[3;1H\x1b[?7labcdef");
    assert_eq!(row(&screen, 0), "HEAD");
    assert_eq!(row(&screen, 1), "    ");
    assert_eq!(row(&screen, 2), "abcf");
    let screen = parsed("\x1b[?7labcd\rX\nY");
    assert_eq!(row(&screen, 0), "Xbcd");
    assert_eq!(row(&screen, 1), " Y  ");
    let screen = parsed("abcd\x1b[1;3H\x1b[4h\x1b[?7lXYZ");
    assert_eq!(row(&screen, 0), "abXZ");
    assert_eq!(row(&screen, 1), "    ");
}

#[test]
fn cursor_saves_and_alternate_exit_restore_wrap_mode() {
    let screen = parsed("abcd\x1b7\x1b[?7lX\x1b8Y");
    assert!(screen.auto_wrap());
    assert_eq!(row(&screen, 1), "Y   ");
    let screen = parsed("\x1b[?7labcd\x1b7\x1b[?7h\x1b8X");
    assert!(!screen.auto_wrap());
    assert_eq!(row(&screen, 0), "abcX");
    let screen = parsed("\x1b[?7labcd\x1b[?1049h\x1b[?7h\x1b[?1049lX");
    assert!(!screen.auto_wrap());
    assert_eq!(row(&screen, 0), "abcX");
}

#[test]
fn resize_retains_modes_but_cancels_saved_edge_state() {
    let mut screen = parsed("\x1b[?7labcd\x1b7\x1b[?1049h\x1b[?7h");
    let mut parser = Parser::new();
    screen.resize(4, 6).unwrap();
    assert!(screen.auto_wrap());
    parser.advance(&mut screen, b"\x1b[?1049l\x1b8");
    assert!(!screen.auto_wrap());
    parser.advance(&mut screen, b"\x1b[?7hX");
    assert_eq!(row(&screen, 0), "abcX  ");
    assert_eq!(screen.cursor(), (0, 4));
    let before = screen.clone();
    screen.resize(4, 6).unwrap();
    assert_eq!(screen, before);
    assert!(screen.resize(0, 6).is_err());
    assert_eq!(screen, before);
}

#[test]
fn only_valid_private_mode_commands_change_auto_wrap() {
    for command in ["\x1b[7l", "\x1b[?7:1l", "\x1b[?999999999999999999999999l"] {
        let screen = parsed(&format!("abcd{command}X"));
        assert!(screen.auto_wrap());
        assert_eq!(row(&screen, 1), "X   ");
    }
    let screen = parsed("\x1b[?25;7labcdX");
    assert!(!screen.auto_wrap());
    assert!(!screen.cursor_visible());
    assert_eq!(row(&screen, 0), "abcX");
}
