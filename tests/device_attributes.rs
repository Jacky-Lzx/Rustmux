use rustmux::{
    parser::{MAX_REPLY_BYTES, Parser},
    screen::Screen,
};
fn replies(input: &[u8]) -> Vec<u8> {
    let mut expected = None;
    for split in 0..=input.len() {
        let mut parser = Parser::new();
        let mut screen = Screen::new(3, 8).unwrap();
        parser.advance(&mut screen, b"\x1b[31mabcdefgh\x1b[?2026h");
        let before = screen.clone();
        let mut bytes = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            parser.advance_with_replies(&mut screen, chunk, &mut |r| {
                assert!(r.len() <= MAX_REPLY_BYTES);
                bytes.extend_from_slice(r);
            });
        }
        assert_eq!(screen, before);
        if let Some(expected) = &expected {
            assert_eq!(&bytes, expected);
        } else {
            expected = Some(bytes);
        }
    }
    expected.unwrap()
}
#[test]
fn primary_and_legacy_requests_reply_in_order_without_changing_state() {
    assert_eq!(
        replies(b"\x1b[c\x1b[0c\x1bZ\x1b[5n"),
        b"\x1b[?1;0c\x1b[?1;0c\x1b[?1;0c\x1b[0n"
    );
}
#[test]
fn unsupported_variants_malformed_queries_and_response_echoes_are_silent() {
    for input in [
        b"\x1b[1c".as_slice(),
        b"\x1b[>c",
        b"\x1b[=c",
        b"\x1b[?c",
        b"\x1b[0;0c",
        b"\x1b[0:0c",
        b"\x1b[0 c",
        b"\x1b[0$c",
        b"\x1b[999999999999999999999999c",
        b"\x1b[?1;0c",
        b"\x1b Z",
        b"\x1b]ignored\x1b[c\x07",
        b"\x1bPignored\x1bZ\x1b\\",
    ] {
        assert!(replies(input).is_empty());
    }
}
#[test]
fn controls_cancellation_and_eof_follow_existing_parser_rules() {
    assert_eq!(replies(b"\x1b[0\x07c\x1b\x07Z"), b"\x1b[?1;0c\x1b[?1;0c");
    let mut parser = Parser::new();
    let mut screen = Screen::new(3, 8).unwrap();
    let mut output = Vec::new();
    parser.advance_with_replies(&mut screen, b"\x1b[0\x18c\x1b\x1aZ", &mut |r| {
        output.extend_from_slice(r)
    });
    assert!(output.is_empty());
    parser.advance(&mut screen, b"\x1b[0");
    parser.finish(&mut screen);
    parser.advance_with_replies(&mut screen, b"c", &mut |r| output.extend_from_slice(r));
    assert!(output.is_empty());
    parser.advance_with_replies(&mut screen, b"\x1b[c", &mut |r| output.extend_from_slice(r));
    assert_eq!(output, b"\x1b[?1;0c");
}
#[test]
fn bytewise_requests_fit_the_single_final_byte_reply_budget() {
    let mut parser = Parser::new();
    let mut screen = Screen::new(3, 8).unwrap();
    let mut output = Vec::new();
    for byte in b"\x1b[0c\x1bZ" {
        parser.advance_with_replies(&mut screen, &[*byte], &mut |r| {
            assert!(r.len() <= MAX_REPLY_BYTES);
            output.extend_from_slice(r);
        });
    }
    assert_eq!(output, b"\x1b[?1;0c\x1b[?1;0c");
}
