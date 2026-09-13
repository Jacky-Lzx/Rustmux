use rustmux::{parser::Parser, screen::Screen, style::Style};
const MODES: &str =
    "\x1b[2;4r\x1b[?6h\x1b[?7;25l\x1b[4h\x1b[31;44;1m\x1b)0\x0e\x1b[3g\x1b[5G\x1bH\x1b[2;8HX\x1b7";

fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(4, 8).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(4, 8).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected);
    }
    let mut screen = Screen::new(4, 8).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}
#[test]
fn resets_modes_but_preserves_cells_cursor_and_custom_tabs() {
    let before = parsed(MODES.as_bytes());
    let mut after = parsed(format!("{MODES}\x1b[!p").as_bytes());
    for r in 0..4 {
        assert_eq!(after.row(r), before.row(r));
    }
    assert_eq!(after.cursor(), before.cursor());
    assert!(after.cursor_visible());
    assert!(after.auto_wrap());
    assert!(!after.origin_mode());
    assert!(!after.insert_mode());
    assert!(!after.wrap_pending());
    assert_eq!(after.style(), Style::default());
    assert_eq!(after.scroll_region(), (0, 3));
    Parser::new().advance(&mut after, b"\x1b[H\tq");
    assert_eq!(after.row(0).unwrap()[4].character, 'q');
    assert_eq!(after.cursor(), (0, 5));
}
#[test]
fn saved_cursor_becomes_home_with_default_modes() {
    let screen = parsed(format!("{MODES}\x1b[!p\x1b8q").as_bytes());
    assert_eq!(screen.cursor(), (0, 1));
    assert_eq!(screen.row(0).unwrap()[0].character, 'q');
    assert_eq!(screen.row(0).unwrap()[0].style, Style::default());
}
#[test]
fn alternate_stays_active_and_main_snapshot_survives() {
    let mut main = parsed(MODES.as_bytes());
    // These modes are global rather than part of the mode-1049 snapshot.
    main.set_insert_mode(false);
    main.set_cursor_visible(true);
    let input = format!("{MODES}\x1b[?1049hALT\x1b[!p");
    let mut screen = parsed(input.as_bytes());
    assert!(screen.is_alternate());
    Parser::new().advance(&mut screen, b"\x1b[?1049l");
    assert_eq!(screen, main);
}
#[test]
fn only_exact_soft_reset_form_is_accepted() {
    let before = parsed(MODES.as_bytes());
    for suffix in [
        "\x1b[p",
        "\x1b[0!p",
        "\x1b[? !p",
        "\x1b[!!p",
        "\x1b[!0p",
        "\x1b[!m",
        "\x1b[!\x18",
        "\x1b]payload\x1b[!p\x07",
    ] {
        assert_eq!(
            parsed(format!("{MODES}{suffix}").as_bytes()),
            before,
            "{suffix:?}"
        );
    }
}
#[test]
fn repeated_reset_after_resize_and_queries_keep_position() {
    let mut screen = parsed(MODES.as_bytes());
    screen.resize(2, 6).unwrap();
    let position = screen.cursor();
    screen.soft_reset();
    let before = screen.clone();
    screen.soft_reset();
    assert_eq!(screen, before);
    assert_eq!(screen.cursor(), position);
    assert_eq!(screen.dimensions(), (2, 6));
    let mut replies = Vec::new();
    Parser::new().advance_with_replies(&mut screen, b"\x1b[6n", &mut |r| {
        replies.extend_from_slice(r)
    });
    assert_eq!(replies, b"\x1b[2;6R");
    let mut one = Screen::new(1, 1).unwrap();
    Parser::new().advance(&mut one, b"X\x1b[!p");
    assert_eq!(one.row(0).unwrap()[0].character, 'X');
    assert!(!one.wrap_pending());
}
