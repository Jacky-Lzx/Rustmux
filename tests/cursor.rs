use rustmux::{parser::Parser, render::render, screen::Screen, style::Color};

fn parse(input: &[u8]) -> Screen {
    let mut expected = Screen::new(2, 4).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(2, 4).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected);
    }
    expected
}

#[test]
fn saved_attributes_and_pending_wrap_restore_without_undoing_text() {
    let screen = parse(b"\x1b[31mabcd\x1b7\x1b[H\x1b[32mX\x1b8Y");
    assert_eq!(screen.row(0).unwrap()[0].character, 'X');
    assert_eq!(
        screen.row(0).unwrap()[0].style.foreground,
        Color::Indexed(2)
    );
    assert_eq!(screen.row(1).unwrap()[0].character, 'Y');
    assert_eq!(
        screen.row(1).unwrap()[0].style.foreground,
        Color::Indexed(1)
    );
    assert_eq!(screen.cursor(), (1, 1));
}

#[test]
fn save_slot_is_replaced_and_restore_without_save_is_noop() {
    assert_eq!(parse(b"ab\x1b8").cursor(), (0, 2));
    assert_eq!(parse(b"a\x1b7b\x1b7c\x1b8\x1b8").cursor(), (0, 2));
}

#[test]
fn alternate_save_does_not_overwrite_main_or_1049_snapshot() {
    let screen = parse(b"a\x1b7b\x1b[?1049h\x1b[H\x1b7\x1b[2;3H\x1b8X\x1b[?1049l");
    assert_eq!(screen.cursor(), (0, 2));
    let mut screen = screen;
    screen.restore_cursor();
    assert_eq!(screen.cursor(), (0, 1));
    screen.enter_alternate();
    screen.move_to(1, 3);
    screen.restore_cursor(); // discarded alternate save must not survive re-entry
    assert_eq!(screen.cursor(), (1, 3));
}

#[test]
fn resize_clamps_all_saved_positions_and_preserves_visibility() {
    let mut screen = parse(b"abcd\x1b7\x1b[?1049h\x1b[2;4H\x1b7\x1b[?25l");
    screen.resize(1, 2).unwrap();
    screen.restore_cursor();
    assert_eq!(screen.cursor(), (0, 1));
    assert!(!screen.wrap_pending());
    assert!(!screen.cursor_visible());
    screen.leave_alternate();
    screen.restore_cursor();
    assert_eq!(screen.cursor(), (0, 1));
    assert!(!screen.wrap_pending());
    assert!(!screen.cursor_visible());
}

#[test]
fn renderer_obeys_global_visibility_and_rejects_nonprivate_variants() {
    let screen = parse(b"a\x1b7\x1b[?25l\x1b8");
    assert!(!screen.cursor_visible());
    let mut frame = Vec::new();
    render(&screen, &mut frame).unwrap();
    assert!(frame.ends_with(b"\x1b[1;2H\x1b[?25l"));
    assert!(!frame.windows(6).any(|bytes| bytes == b"\x1b[?25h"));
    assert!(parse(b"\x1b[25l").cursor_visible());
    assert!(parse(b"\x1b[?25l\x1b[?25;999h").cursor_visible());
}
