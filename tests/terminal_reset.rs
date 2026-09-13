use rustmux::{parser::Parser, render::render, screen::Screen};

// Exercise every persisted family of screen state before resetting.
const DIRTY: &str = "main中e\u{301}\x1b[1;31;44m\x1b[2;3r\x1b[?6;25l\x1b[4h\x1b[3g\x1b[4G\x1bH\x1b)0\x0e\x1b7\x1b[?1049h\x1b[2;4r\x1b[?6h\x1b[?7l\x1b(0\x1b[Hqx\x1b7";

fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(4, 16).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(4, 16).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected);
    }
    let mut screen = Screen::new(4, 16).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}

#[test]
fn ris_restores_every_model_field_from_main_or_alternate() {
    for suffix in ["", "\x1b[?1049l"] {
        let input = format!("{DIRTY}{suffix}\x1bc");
        assert_eq!(parsed(input.as_bytes()), Screen::new(4, 16).unwrap());
    }
}

#[test]
fn old_saved_state_cannot_return_and_parsing_continues_after_reset() {
    let screen = parsed(format!("{DIRTY}\x1bc\x1b8\x1b[?1049lq\tX").as_bytes());
    let mut expected = Screen::new(4, 16).unwrap();
    Parser::new().advance(&mut expected, b"q\tX");
    assert_eq!(screen, expected);
    let screen = parsed(format!("{DIRTY}\x1bc\x1b[?1049h\x1b8\x1b[?1049l").as_bytes());
    assert_eq!(screen, Screen::new(4, 16).unwrap());
}

#[test]
fn reset_preserves_resized_dimensions_and_is_repeatable() {
    let mut screen = parsed(DIRTY.as_bytes());
    screen.resize(2, 7).unwrap();
    screen.reset();
    assert_eq!(screen, Screen::new(2, 7).unwrap());
    screen.reset();
    assert_eq!(screen, Screen::new(2, 7).unwrap());
    let mut one = Screen::new(1, 1).unwrap();
    Parser::new().advance(&mut one, b"X\x1bc");
    assert_eq!(one, Screen::new(1, 1).unwrap());
}

#[test]
fn only_ris_resets_and_cancelled_or_string_sequences_are_ignored() {
    let before = parsed(DIRTY.as_bytes());
    for suffix in [
        "\x1b[c",
        "\x1b c",
        "\x1b]title\x1bc\x07",
        "\x1bPdata\x1bc\x1b\\",
        "\x1b\x18",
    ] {
        assert_eq!(
            parsed(format!("{DIRTY}{suffix}").as_bytes()),
            before,
            "{suffix:?}"
        );
    }
    // RIS restarts an incomplete CSI; malformed UTF-8 before it does not survive.
    assert_eq!(parsed(b"\xff\x1b[999\x1bc"), Screen::new(4, 16).unwrap());
}

#[test]
fn queries_and_renderer_observe_reset_state_immediately() {
    let mut screen = parsed(DIRTY.as_bytes());
    let mut parser = Parser::new();
    let mut replies = Vec::new();
    parser.advance_with_replies(&mut screen, b"\x1bc\x1b[6n\x1b[5n", &mut |r| {
        replies.extend_from_slice(r)
    });
    assert_eq!(replies, b"\x1b[1;1R\x1b[0n");
    let mut frame = Vec::new();
    render(&screen, &mut frame).unwrap();
    let mut replay = Screen::new(4, 16).unwrap();
    Parser::new().advance(&mut replay, &frame);
    assert_eq!(replay, screen);
}
