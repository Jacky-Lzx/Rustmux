use rustmux::{parser::Parser, render::render, screen::Screen, style::Color};
fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(4, 40).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(4, 40).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected);
    }
    let mut screen = Screen::new(4, 40).unwrap();
    let mut parser = Parser::new();
    for byte in input {
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
        .filter(|c| c.width != 0)
        .map(|c| c.character)
        .collect::<String>()
        .trim_end()
        .into()
}
#[test]
fn g0_graphics_draws_box_and_ascii_designation_restores_letters() {
    let screen = parsed(b"\x1b(0lqqk\r\nx  x\r\nmqqj\x1b(Bqx");
    assert_eq!(text(&screen, 0), "┌──┐");
    assert_eq!(text(&screen, 1), "│  │");
    assert_eq!(text(&screen, 2), "└──┘qx");
}
#[test]
fn g1_requires_so_and_si_returns_to_g0() {
    let screen = parsed(b"\x1b)0q\x0eqx\x0fqx");
    assert_eq!(text(&screen, 0), "q─│qx");
    let screen = parsed(b"\x1b(0\x1b)Bq\x0eq\x0fq");
    assert_eq!(text(&screen, 0), "─q─");
}
#[test]
fn symbols_utf8_and_styles_survive_rendering() {
    let screen = parsed("\x1b(0\x1b[31m\x60afgnojklmtuvwyz{|}~中\u{301}".as_bytes());
    assert_eq!(text(&screen, 0), "◆▒°±┼⎺┘┐┌└├┤┴┬≤≥π≠£·中");
    assert_eq!(
        screen.row(0).unwrap()[0].style.foreground,
        Color::Indexed(1)
    );
    let mut frame = Vec::new();
    render(&screen, &mut frame).unwrap();
    let mut replay = Screen::new(4, 40).unwrap();
    Parser::new().advance(&mut replay, &frame);
    for row in 0..4 {
        assert_eq!(replay.row(row), screen.row(row));
    }
}
#[test]
fn saved_and_alternate_character_sets_are_restored_and_resize_retains_them() {
    let mut screen = parsed(b"\x1b)0\x0e\x1b7\x0f\x1b)B\x1b8q");
    assert_eq!(text(&screen, 0), "─");
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"\x1b[?1049h\x1b)B\x0f\x1b[?1049lq");
    assert_eq!(text(&screen, 0), "──");
    screen.resize(5, 42).unwrap();
    parser.advance(&mut screen, b"x");
    assert_eq!(text(&screen, 0), "──│");
}
#[test]
fn cancelled_unknown_and_string_designations_do_not_leak_or_change_sets() {
    for prefix in [
        b"\x1b(A".as_slice(),
        b"\x1b(\x18",
        b"\x1b(\x1b(B",
        b"\x1b(%0",
        b"\x1b]payload\x1b(0\x0e\x07",
    ] {
        let mut input = prefix.to_vec();
        input.extend_from_slice(b"qx");
        assert_eq!(text(&parsed(&input), 0), "qx");
    }
    let screen = parsed(b"\x1b)0\x1b[3\x0eGq");
    assert_eq!(text(&screen, 0), "  ─");
}
#[test]
fn switching_sets_preserves_pending_wrap_and_insert_mode_uses_mapped_width() {
    let mut screen = Screen::new(2, 2).unwrap();
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"ab\x1b)0\x0e");
    assert!(screen.wrap_pending());
    parser.advance(&mut screen, b"q");
    assert_eq!(text(&screen, 0), "ab");
    assert_eq!(text(&screen, 1), "─");
    let screen = parsed(b"abc\x1b[H\x1b[4h\x1b(0q");
    assert_eq!(text(&screen, 0), "─abc");
}
