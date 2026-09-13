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
    let mut after = parsed(b"\x1b[31mabcdefgh\x1b[?1004h");
    assert!(after.focus_reporting());
    after.set_focus_reporting(false);
    assert_eq!(after, before);
    assert!(!parsed(b"\x1b[?1004h\x1b[?1004l").focus_reporting());
}
#[test]
fn mode_is_global_preserved_by_soft_reset_and_cleared_by_ris() {
    let mut screen = parsed(b"\x1b7\x1b[?1049h\x1b[?1004h\x1b[?1049l\x1b8\x1b[!p");
    assert!(screen.focus_reporting());
    screen.resize(4, 10).unwrap();
    assert!(screen.focus_reporting());
    screen.reset();
    assert!(!screen.focus_reporting());
}
#[test]
fn renderer_synchronizes_outer_mode_and_replay_preserves_it() {
    let mut screen = Screen::new(3, 8).unwrap();
    for enabled in [true, false] {
        screen.set_focus_reporting(enabled);
        let before = screen.clone();
        let mut output = Vec::new();
        render(&screen, &mut output).unwrap();
        let sequence = if enabled {
            b"\x1b[?1004h"
        } else {
            b"\x1b[?1004l"
        };
        assert!(output.windows(sequence.len()).any(|w| w == sequence));
        assert_eq!(screen, before);
        let mut replay = Screen::new(3, 8).unwrap();
        Parser::new().advance(&mut replay, &output);
        assert_eq!(screen, replay);
    }
}
#[test]
fn malformed_and_nonprivate_modes_do_not_enable_focus() {
    for input in [
        b"\x1b[1004h".as_slice(),
        b"\x1b[?1004:1h",
        b"\x1b[?999999999999999999999h",
        b"\x1b]text\x1b[?1004h\x07",
    ] {
        assert!(!parsed(input).focus_reporting());
    }
    let screen = parsed(b"\x1b[?25;1004h");
    assert!(screen.focus_reporting());
    assert!(screen.cursor_visible());
}

#[test]
fn ordered_frames_synchronize_only_on_changes_and_retry_after_failure() {
    use rustmux::render::Renderer;
    use std::io;
    let mut renderer = Renderer::default();
    let mut screen = Screen::new(3, 8).unwrap();
    for enabled in [false, true, false] {
        screen.set_focus_reporting(enabled);
        // Failed output must not update the cached mode.
        let mut short = [0u8; 1];
        assert!(renderer.render(&screen, &mut short.as_mut_slice()).is_err());
        let mut bytes = Vec::new();
        renderer.render(&screen, &mut bytes).unwrap();
        let sequence = if enabled {
            b"\x1b[?1004h"
        } else {
            b"\x1b[?1004l"
        };
        assert!(bytes.windows(sequence.len()).any(|w| w == sequence));
        screen.print('x');
        bytes.clear();
        renderer.render(&screen, &mut bytes).unwrap();
        assert!(!bytes.windows(7).any(|w| w == b"\x1b[?1004"));
    }
    // A fresh output stream always synchronizes, including the disabled state.
    let mut fresh = Renderer::default();
    fresh.render(&screen, &mut io::sink()).unwrap();
}
