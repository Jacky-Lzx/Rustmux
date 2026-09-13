use rustmux::{parser::Parser, screen::Screen};
#[test]
fn split_sequences_keep_parsing_and_reply_while_painting_is_paused() {
    let input = b"\x1b[?2026hTEXT\x1b[?2026$p\x1b[?2026l\x1b[?2026$p";
    for split in 0..=input.len() {
        let mut screen = Screen::new(3, 8).unwrap();
        let mut parser = Parser::new();
        let mut replies = Vec::new();
        parser.advance_with_replies(&mut screen, &input[..split], &mut |r| {
            replies.extend_from_slice(r)
        });
        parser.advance_with_replies(&mut screen, &input[split..], &mut |r| {
            replies.extend_from_slice(r)
        });
        assert_eq!(replies, b"\x1b[?2026;1$y\x1b[?2026;2$y");
        assert_eq!(screen.cursor(), (0, 4));
        assert!(!screen.synchronized_output());
    }
}
#[test]
fn global_mode_survives_saves_and_model_resize_but_resets_clear_it() {
    let mut screen = Screen::new(3, 8).unwrap();
    Parser::new().advance(&mut screen, b"\x1b7\x1b[?1049h\x1b[?2026h\x1b[?1049l\x1b8");
    screen.resize(4, 10).unwrap();
    assert!(screen.synchronized_output());
    screen.soft_reset();
    assert!(!screen.synchronized_output());
    screen.set_synchronized_output(true);
    screen.reset();
    assert!(!screen.synchronized_output());
}
#[test]
fn malformed_requests_do_not_enable_synchronization() {
    for input in [
        b"\x1b[2026h".as_slice(),
        b"\x1b[?2026:1h",
        b"\x1b[?2026 h",
        b"\x1b]ignored\x1b[?2026h\x07",
    ] {
        let mut screen = Screen::new(3, 8).unwrap();
        Parser::new().advance(&mut screen, input);
        assert!(!screen.synchronized_output());
    }
}
