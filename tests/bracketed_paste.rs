use rustmux::{parser::Parser, render::render, screen::Screen};
fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(3, 8).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(3, 8).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected);
    }
    let mut screen = Screen::new(3, 8).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}
#[test]
fn mode_changes_preserve_text_style_cursor_and_pending_wrap() {
    let before = parsed(b"\x1b[31mabcdefgh");
    let mut after = parsed(b"\x1b[31mabcdefgh\x1b[?2004h");
    assert!(after.bracketed_paste());
    after.set_bracketed_paste(false);
    assert_eq!(after, before);
    assert!(!parsed(b"\x1b[?2004h\x1b[?2004l").bracketed_paste());
}
#[test]
fn mode_is_global_preserved_by_soft_reset_and_cleared_by_ris() {
    let mut screen = parsed(b"\x1b7\x1b[?1049h\x1b[?2004h\x1b[?1049l\x1b8\x1b[!p");
    assert!(screen.bracketed_paste());
    screen.resize(4, 10).unwrap();
    assert!(screen.bracketed_paste());
    screen.reset();
    assert!(!screen.bracketed_paste());
}
#[test]
fn renderer_synchronizes_outer_mode_and_replay_preserves_it() {
    let mut screen = Screen::new(3, 8).unwrap();
    for enabled in [true, false] {
        screen.set_bracketed_paste(enabled);
        let before = screen.clone();
        let mut output = Vec::new();
        render(&screen, &mut output).unwrap();
        let sequence = if enabled {
            b"\x1b[?2004h"
        } else {
            b"\x1b[?2004l"
        };
        assert!(output.windows(sequence.len()).any(|w| w == sequence));
        assert_eq!(screen, before);
        let mut replay = Screen::new(3, 8).unwrap();
        Parser::new().advance(&mut replay, &output);
        assert_eq!(screen, replay);
    }
}
#[test]
fn malformed_and_nonprivate_modes_do_not_enable_paste() {
    for input in [
        b"\x1b[2004h".as_slice(),
        b"\x1b[?2004:1h",
        b"\x1b[?999999999999999999999h",
        b"\x1b]text\x1b[?2004h\x07",
    ] {
        assert!(!parsed(input).bracketed_paste());
    }
    let screen = parsed(b"\x1b[?25;2004h");
    assert!(screen.bracketed_paste());
    assert!(screen.cursor_visible());
}
