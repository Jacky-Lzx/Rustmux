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
    let before = parsed(b"\x1b[?1;2004h\x1b[31mabcdefgh");
    let mut after = parsed(b"\x1b[?1;2004h\x1b[31mabcdefgh\x1b=");
    assert!(after.application_keypad());
    after.set_application_keypad(false);
    assert_eq!(after, before);
    assert!(!parsed(b"\x1b=\x1b>").application_keypad());
    let combined = parsed(b"\x1b[?1;2004;25h\x1b=");
    assert!(combined.application_keypad() && combined.bracketed_paste());
}

#[test]
fn global_mode_survives_grid_cursor_and_resize_but_not_resets() {
    let mut screen = parsed(b"\x1b7\x1b[?1049h\x1b=\x1b[?1049l\x1b8");
    assert!(screen.application_keypad());
    screen.resize(4, 10).unwrap();
    assert!(screen.application_keypad());
    for reset in [b"\x1b[!p".as_slice(), b"\x1bc"] {
        let screen = parsed(&[b"\x1b=\x1b7\x1b[?1049h", reset, b"\x1b[?1049l\x1b8"].concat());
        assert!(!screen.application_keypad());
    }
}

#[test]
fn unrelated_and_cancelled_escape_sequences_are_ignored() {
    for input in [
        b"=".as_slice(),
        b"\x1b =",
        b"\x1b(=",
        b"\x1b\x18=",
        b"\x1b]ignored\x1b=\x07",
    ] {
        assert!(!parsed(input).application_keypad());
    }
}

#[test]
fn rendering_synchronizes_mode_without_mutating_model() {
    let mut screen = Screen::new(2, 5).unwrap();
    for enabled in [true, false] {
        screen.set_application_keypad(enabled);
        let before = screen.clone();
        let mut output = Vec::new();
        render(&screen, &mut output).unwrap();
        let sequence = if enabled { b"\x1b=" } else { b"\x1b>" };
        assert!(output.windows(sequence.len()).any(|w| w == sequence));
        assert_eq!(screen, before);
        let mut replay = Screen::new(2, 5).unwrap();
        Parser::new().advance(&mut replay, &output);
        assert_eq!(replay, screen);
    }
}
