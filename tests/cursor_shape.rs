mod common;
use rustmux::{
    parser::{MAX_REPLY_BYTES, Parser},
    render::render,
    screen::{CursorShape, Screen},
};

fn replies(input: &[u8]) -> (Screen, Vec<u8>) {
    let run = |split: usize| {
        let mut screen = Screen::new(3, 8).unwrap();
        let mut parser = Parser::new();
        let mut output = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            parser.advance_with_replies(&mut screen, chunk, &mut |reply| {
                assert!(reply.len() <= MAX_REPLY_BYTES);
                output.extend_from_slice(reply)
            });
        }
        (screen, output)
    };
    let expected = run(input.len());
    for split in 0..=input.len() {
        assert_eq!(run(split), expected);
    }
    expected
}

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
fn all_shapes_and_default_parse_without_changing_other_state() {
    for (parameter, shape) in [
        ("", CursorShape::BlinkingBlock),
        ("0", CursorShape::BlinkingBlock),
        ("1", CursorShape::BlinkingBlock),
        ("2", CursorShape::SteadyBlock),
        ("3", CursorShape::BlinkingUnderline),
        ("4", CursorShape::SteadyUnderline),
        ("5", CursorShape::BlinkingBar),
        ("6", CursorShape::SteadyBar),
    ] {
        let before = parsed(b"\x1b[31mabcdefgh\x1b[?25l");
        let mut after = parsed(format!("\x1b[31mabcdefgh\x1b[?25l\x1b[{parameter} q").as_bytes());
        assert_eq!(after.cursor_shape(), shape);
        after.set_cursor_shape(CursorShape::default());
        assert_eq!(before, after);
    }
}

#[test]
fn global_shape_survives_save_grid_and_resize_but_resets_to_block() {
    let mut screen = parsed(b"\x1b7\x1b[?1049h\x1b[6 q\x1b[?1049l\x1b8");
    screen.resize(4, 10).unwrap();
    assert_eq!(screen.cursor_shape(), CursorShape::SteadyBar);
    for reset in [b"\x1b[!p".as_slice(), b"\x1bc"] {
        assert_eq!(
            parsed(&[b"\x1b[6 q\x1b7\x1b[?1049h", reset, b"\x1b[?1049l\x1b8"].concat())
                .cursor_shape(),
            CursorShape::BlinkingBlock
        );
    }
}

#[test]
fn malformed_commands_do_not_change_shape_or_leak_into_other_commands() {
    for input in [
        b"\x1b[6q".as_slice(),
        b"\x1b[?6 q",
        b"\x1b[6; q",
        b"\x1b[6:1 q",
        b"\x1b[6  q",
        b"\x1b[ 6q",
        b"\x1b[6!q",
        b"\x1b[7 q",
        b"\x1b[999999999999999999999999 q",
        b"\x1b[6 \x18q",
        b"\x1b]ignored\x1b[6 q\x07",
    ] {
        assert_eq!(parsed(input).cursor_shape(), CursorShape::BlinkingBlock);
    }
    assert_eq!(parsed(b"\x1b[6 m").style(), parsed(b"").style());
    assert_eq!(
        parsed(b"\x1b[6 \x07q").cursor_shape(),
        CursorShape::SteadyBar
    );
}

#[test]
fn frames_emit_shape_even_when_hidden_and_replay_without_mutation() {
    for code in 1..=6 {
        let screen = parsed(format!("\x1b[{code} q\x1b[?25l").as_bytes());
        let before = screen.clone();
        let mut bytes = Vec::new();
        render(&screen, &mut bytes).unwrap();
        let sequence = format!("\x1b[{code} q");
        assert!(
            bytes
                .windows(sequence.len())
                .any(|w| w == sequence.as_bytes())
        );
        assert_eq!(screen, before);
        let mut replay = Screen::new(3, 8).unwrap();
        Parser::new().advance(&mut replay, &bytes);
        common::assert_rendered_screen_eq(&replay, &screen);
    }
}

#[test]
fn status_string_reports_the_current_cursor_style() {
    let (screen, output) = replies(b"\x1bP$q q\x1b\\\x1b[6 q\x1bP$q q\x1b\\\x1b[!p\x1bP$q q\x1b\\");
    assert_eq!(screen.cursor_shape(), CursorShape::BlinkingBlock);
    assert_eq!(
        output,
        b"\x1bP1$r1 q\x1b\\\x1bP1$r6 q\x1b\\\x1bP1$r1 q\x1b\\"
    );
}

#[test]
fn unsupported_malformed_and_cancelled_status_strings_are_bounded() {
    for input in [
        b"\x1bP$q\"q\x1b\\".as_slice(),
        b"\x1bP$q\x1b\\",
        b"\x1bP$q qx\x1b\\",
        b"\x1bP$q q\x07\x1b\\",
    ] {
        assert_eq!(replies(input).1, b"\x1bP0$r\x1b\\");
    }
    for input in [
        b"\x1bP+q q\x1b\\".as_slice(),
        b"\x1bP1$r6 q\x1b\\",
        b"\x1bP$q q\x18",
        b"\x1bP$q q\x1bX\x1b\\",
    ] {
        assert!(replies(input).1.is_empty());
    }

    let oversized = [b"\x1bP$q".as_slice(), &[b'x'; 64], b"\x1b\\"].concat();
    assert!(replies(&oversized).1.is_empty());
}
