mod common;
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
    expected
}

#[test]
fn toggles_do_not_change_display_or_other_input_modes() {
    let before = parsed(b"\x1b[?1;66;2004h\x1b[31mabcdefgh");
    let mut after = parsed(b"\x1b[?1;66;67;2004h\x1b[31mabcdefgh");
    assert!(after.backarrow_sends_backspace());
    after.set_backarrow_sends_backspace(false);
    assert_eq!(after, before);
    assert!(!parsed(b"\x1b[?67h\x1b[?67l").backarrow_sends_backspace());
}

#[test]
fn global_mode_survives_grid_cursor_and_resize_but_not_resets() {
    let mut screen = parsed(b"\x1b7\x1b[?1049h\x1b[?67h\x1b[?1049l\x1b8");
    assert!(screen.backarrow_sends_backspace());
    screen.resize(4, 10).unwrap();
    assert!(screen.backarrow_sends_backspace());
    for reset in [b"\x1b[!p".as_slice(), b"\x1bc"] {
        let screen = parsed(&[b"\x1b[?67h\x1b7\x1b[?1049h", reset, b"\x1b[?1049l\x1b8"].concat());
        assert!(!screen.backarrow_sends_backspace());
    }
}

#[test]
fn malformed_and_nonprivate_requests_are_ignored() {
    for input in [
        b"\x1b[67h".as_slice(),
        b"\x1b[?67:1h",
        b"\x1b[?67 h",
        b"\x1b[?67\x18h",
        b"\x1b]ignored\x1b[?67h\x07",
    ] {
        assert!(!parsed(input).backarrow_sends_backspace());
    }
}

#[test]
fn rendering_synchronizes_mode_without_mutating_model() {
    let mut screen = Screen::new(2, 5).unwrap();
    for enabled in [true, false] {
        screen.set_backarrow_sends_backspace(enabled);
        let before = screen.clone();
        let mut output = Vec::new();
        render(&screen, &mut output).unwrap();
        let sequence = if enabled {
            b"\x1b[?67h".as_slice()
        } else {
            b"\x1b[?67l".as_slice()
        };
        assert!(
            output
                .windows(sequence.len())
                .any(|window| window == sequence)
        );
        assert_eq!(screen, before);
        let mut replay = Screen::new(2, 5).unwrap();
        Parser::new().advance(&mut replay, &output);
        common::assert_rendered_screen_eq(&replay, &screen);
    }
}
