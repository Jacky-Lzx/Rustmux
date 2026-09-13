use rustmux::{
    parser::Parser,
    render::{Renderer, render},
    screen::Screen,
};
fn frame(renderer: &mut Renderer, screen: &Screen) -> Vec<u8> {
    let mut bytes = Vec::new();
    renderer.render(screen, &mut bytes).unwrap();
    bytes
}
fn assert_grid(screen: &Screen, replay: &Screen) {
    for row in 0..screen.dimensions().0 {
        assert_eq!(screen.row(row), replay.row(row));
    }
    assert_eq!(screen.cursor(), replay.cursor());
    assert_eq!(screen.cursor_visible(), replay.cursor_visible());
    assert_eq!(screen.cursor_shape(), replay.cursor_shape());
}
#[test]
fn one_changed_row_and_cursor_only_frames_preserve_untouched_rows() {
    let mut renderer = Renderer::default();
    let mut screen = Screen::new(24, 80).unwrap();
    let mut replay = screen.clone();
    let mut parser = Parser::new();
    parser.advance(&mut replay, &frame(&mut renderer, &screen));
    Parser::new().advance(&mut screen, b"\x1b[12;1Hchanged");
    let bytes = frame(&mut renderer, &screen);
    assert!(bytes.windows(7).any(|w| w == b"changed"));
    assert!(!bytes.windows(6).any(|w| w == b"\x1b[1;1"));
    let mut full = Vec::new();
    render(&screen, &mut full).unwrap();
    assert!(bytes.len() * 5 < full.len());
    parser.advance(&mut replay, &bytes);
    assert_grid(&screen, &replay);
    Parser::new().advance(&mut screen, b"\x1b[3;4H\x1b[?25l");
    let bytes = frame(&mut renderer, &screen);
    assert_eq!(bytes.iter().filter(|&&byte| byte == b'H').count(), 1);
    parser.advance(&mut replay, &bytes);
    assert_grid(&screen, &replay);
}
#[test]
fn styles_wide_cells_combining_erase_scroll_and_alternate_replay_correctly() {
    let mut renderer = Renderer::default();
    let mut screen = Screen::new(4, 8).unwrap();
    let mut replay = screen.clone();
    let mut parser = Parser::new();
    for input in [
        "中e\u{301}",
        "\x1b[H\x1b[31m中e\u{301}",
        "\x1b[1;2HX",
        "\x1b[2J",
        "\x1b[4;1Hbottom\nnext",
        "\x1b[?1049hOTHER",
        "\x1b[?1049l",
    ] {
        Parser::new().advance(&mut screen, input.as_bytes());
        parser.advance(&mut replay, &frame(&mut renderer, &screen));
        assert_grid(&screen, &replay);
    }
}
#[test]
fn resize_invalidation_and_failed_output_force_full_repaint() {
    let mut renderer = Renderer::default();
    let mut screen = Screen::new(2, 4).unwrap();
    frame(&mut renderer, &screen);
    screen.resize(3, 5).unwrap();
    let mut full = Vec::new();
    render(&screen, &mut full).unwrap();
    let bytes = frame(&mut renderer, &screen);
    // Mode synchronization may be omitted; every new row must still be present.
    for row in 1..=3 {
        assert!(
            bytes
                .windows(6)
                .any(|w| w == format!("\x1b[{row};1H").as_bytes())
        );
    }
    renderer.invalidate();
    assert_eq!(frame(&mut renderer, &screen), full);
    Parser::new().advance(&mut screen, b"abc");
    let mut short = [0u8; 12];
    assert!(renderer.render(&screen, &mut short.as_mut_slice()).is_err());
    // Even reverting to the old model must repaint after a partial write.
    Parser::new().advance(&mut screen, b"\x1b[2J\x1b[H");
    full.clear();
    render(&screen, &mut full).unwrap();
    assert_eq!(frame(&mut renderer, &screen), full);
}
