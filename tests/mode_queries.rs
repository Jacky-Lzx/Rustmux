use rustmux::{
    parser::{MAX_REPLY_BYTES, Parser},
    screen::Screen,
};

fn replies(screen: &mut Screen, input: &[u8]) -> Vec<u8> {
    let before = screen.clone();
    let mut expected = Vec::new();
    Parser::new().advance_with_replies(screen, input, &mut |r| expected.extend_from_slice(r));
    for split in 0..=input.len() {
        let mut actual = before.clone();
        let mut parser = Parser::new();
        let mut bytes = Vec::new();
        parser.advance_with_replies(&mut actual, &input[..split], &mut |r| {
            bytes.extend_from_slice(r)
        });
        parser.advance_with_replies(&mut actual, &input[split..], &mut |r| {
            bytes.extend_from_slice(r)
        });
        assert_eq!(actual, *screen);
        assert_eq!(bytes, expected);
    }
    expected
}

#[test]
fn supported_modes_report_both_states_without_mutation() {
    for (prefix, mode) in [
        ("", 4),
        ("?", 1),
        ("?", 6),
        ("?", 7),
        ("?", 25),
        ("?", 1004),
        ("?", 1049),
        ("?", 2004),
    ] {
        let mut screen = Screen::new(3, 8).unwrap();
        for (command, status) in [('h', 1), ('l', 2)] {
            Parser::new().advance(
                &mut screen,
                format!("\x1b[{prefix}{mode}{command}").as_bytes(),
            );
            let before = screen.clone();
            assert_eq!(
                replies(&mut screen, format!("\x1b[{prefix}{mode}$p").as_bytes()),
                format!("\x1b[{prefix}{mode};{status}$y").as_bytes()
            );
            assert_eq!(screen, before);
        }
    }
}

#[test]
fn queries_observe_stream_order_and_reset_state() {
    let mut screen = Screen::new(3, 8).unwrap();
    assert_eq!(
        replies(
            &mut screen,
            b"\x1b[?2004$p\x1b[?2004h\x1b[?2004$p\x1b[!p\x1b[?2004$p\x1bc\x1b[?2004$p\x1b[5n"
        ),
        b"\x1b[?2004;2$y\x1b[?2004;1$y\x1b[?2004;1$y\x1b[?2004;2$y\x1b[0n"
    );
}

#[test]
fn unsupported_and_default_modes_report_zero_in_their_own_namespace() {
    let mut screen = Screen::new(3, 8).unwrap();
    for (prefix, mode) in [
        ("", 1),
        ("?", 4),
        ("?", 66),
        ("?", 1005),
        ("?", 2027),
        ("", 0),
        ("?", usize::MAX),
    ] {
        let response = replies(&mut screen, format!("\x1b[{prefix}{mode}$p").as_bytes());
        assert_eq!(response, format!("\x1b[{prefix}{mode};0$y").as_bytes());
        assert!(response.len() <= MAX_REPLY_BYTES);
    }
    assert_eq!(
        replies(&mut screen, b"\x1b[$p\x1b[?$p"),
        b"\x1b[0;0$y\x1b[?0;0$y"
    );
}

#[test]
fn malformed_queries_are_silent_and_cannot_change_modes() {
    for input in [
        b"\x1b[?7;25$p".as_slice(),
        b"\x1b[?7:1$p",
        b"\x1b[?7$$p",
        b"\x1b[?7$1p",
        b"\x1b[?7$h",
        b"\x1b[7 $p",
        b"\x1b[>7$p",
        b"\x1b[?999999999999999999999999$p",
        b"\x1b[?7$\x18",
        b"\x1b]ignore\x1b[?7$p\x07",
        b"\x1bPignore\x1b[?7$p\x1b\\",
    ] {
        let mut screen = Screen::new(3, 8).unwrap();
        let before = screen.clone();
        assert!(replies(&mut screen, input).is_empty());
        assert_eq!(screen, before);
    }
}

#[test]
fn c0_controls_execute_and_incomplete_queries_are_discarded_at_eof() {
    let mut screen = Screen::new(3, 8).unwrap();
    assert_eq!(replies(&mut screen, b"\x1b[?7$\n p"), b"");
    assert_eq!(screen.cursor(), (1, 0));
    assert_eq!(replies(&mut screen, b"\x1b[?7$\x07p"), b"\x1b[?7;1$y");
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"\x1b[?7$");
    parser.finish(&mut screen);
    let mut bytes = Vec::new();
    parser.advance_with_replies(&mut screen, b"p", &mut |r| bytes.extend_from_slice(r));
    assert!(bytes.is_empty());
}
