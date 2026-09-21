use rustmux::{
    parser::{MAX_REPLY_BYTES, Parser},
    screen::Screen,
};

fn replies(input: &[u8]) -> (Screen, Vec<u8>) {
    let run = |split: usize| {
        let mut screen = Screen::new(8, 12).unwrap();
        let mut parser = Parser::new();
        let mut replies = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            let before = replies.len();
            parser.advance_with_replies(&mut screen, chunk, &mut |reply| {
                replies.extend_from_slice(reply)
            });
            assert!(replies.len() - before <= chunk.len() * MAX_REPLY_BYTES);
        }
        (screen, replies)
    };
    let expected = run(input.len());
    for split in 0..=input.len() {
        assert_eq!(run(split), expected);
    }
    let mut screen = Screen::new(8, 12).unwrap();
    let mut parser = Parser::new();
    let mut output = Vec::new();
    for byte in input {
        parser.advance_with_replies(&mut screen, &[*byte], &mut |reply| {
            assert!(reply.len() <= MAX_REPLY_BYTES);
            output.extend_from_slice(reply);
        });
    }
    assert_eq!((screen, output), expected);
    expected
}

#[test]
fn status_and_cursor_replies_are_ordered_and_do_not_change_screen() {
    let (screen, output) = replies(b"abc\x1b[5n\x1b[6n\x1b[?6n\x1b[4;9H\x1b[6n\x1b[?6n");
    assert_eq!(output, b"\x1b[0n\x1b[1;4R\x1b[?1;4R\x1b[4;9R\x1b[?4;9R");
    let mut expected = Screen::new(8, 12).unwrap();
    Parser::new().advance(&mut expected, b"abc\x1b[4;9H");
    assert_eq!(screen, expected);
    let (screen, output) = replies(b"abcdefghijkl\x1b[6n");
    assert!(screen.wrap_pending());
    assert_eq!(output, b"\x1b[1;12R");
}

#[test]
fn cursor_report_uses_current_origin_and_active_grid() {
    let (_, output) = replies(
        b"\x1b[3;7r\x1b[?6h\x1b[2;5H\x1b[6n\x1b[?6n\x1b[?1049h\x1b[H\x1b[6n\x1b[?6n\x1b[?1049l\x1b[6n\x1b[?6n",
    );
    assert_eq!(
        output,
        b"\x1b[2;5R\x1b[?2;5R\x1b[1;1R\x1b[?1;1R\x1b[2;5R\x1b[?2;5R"
    );
}

#[test]
fn malformed_unsupported_and_string_queries_produce_no_reply() {
    let (_, output) = replies(b"\x1b[n\x1b[0n\x1b[?n\x1b[?0n\x1b[?5n\x1b[?6;1n\x1b[?6:1n\x1b[5;6n\x1b[6:1n\x1b[999999999999999999999999n\x1b[?999999999999999999999999n\x1b]text\x1b[6n\x07\x1b[6\x18n");
    assert!(output.is_empty());
    let (_, output) = replies(b"\xff\x1b[6n");
    assert_eq!(output, b"\x1b[1;2R");
}

#[test]
fn default_color_queries_use_matching_terminators_and_are_ordered() {
    let (screen, output) = replies(b"before\x1b]10;?\x07middle\x1b]11;?\x1b\\after\x1b[5n");
    assert_eq!(
        output,
        b"\x1b]10;rgb:cdcd/d6d6/f4f4\x07\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[0n"
    );
    let mut expected = Screen::new(8, 12).unwrap();
    Parser::new().advance(&mut expected, b"beforemiddleafter");
    assert_eq!(screen, expected);
}

#[test]
fn default_colors_are_stateful_resettable_and_survive_ris() {
    let (_, output) = replies(
        b"\x1b]10;#010203\x1b\\\x1b]11;rgb:1111/2222/3333\x07\
          \x1b]10;?\x1b\\\x1b]11;?\x07\
          \x1bc\x1b]10;?\x1b\\\x1b]11;?\x07\
          \x1b]110\x1b\\\x1b]111\x07\x1b]10;?\x1b\\\x1b]11;?\x07",
    );
    assert_eq!(
        output,
        b"\x1b]10;rgb:0101/0202/0303\x1b\\\x1b]11;rgb:1111/2222/3333\x07\
          \x1b]10;rgb:0101/0202/0303\x1b\\\x1b]11;rgb:1111/2222/3333\x07\
          \x1b]10;rgb:cdcd/d6d6/f4f4\x1b\\\x1b]11;rgb:1e1e/1e1e/2e2e\x07"
    );
}

#[test]
fn invalid_default_color_setters_are_atomic() {
    for invalid in [
        "#12345",
        "#gg0000",
        "rgb:/2/3",
        "rgb:1/2",
        "rgb:1/2/3/4",
        "rgb:12345/2/3",
        "red",
        "#010203;?",
    ] {
        let input = format!("\x1b]10;#123456\x1b\\\x1b]10;{invalid}\x07\x1b]10;?\x1b\\");
        assert_eq!(
            replies(input.as_bytes()).1,
            b"\x1b]10;rgb:1212/3434/5656\x1b\\"
        );
    }
}

#[test]
fn rgb_components_scale_from_one_to_four_hex_digits() {
    let (_, output) = replies(b"\x1b]10;rgb:f/80/0000\x07\x1b]10;?\x1b\\");
    assert_eq!(output, b"\x1b]10;rgb:ffff/8080/0000\x1b\\");

    let overlong = format!("\x1b]11;#{}\x07\x1b]11;?\x1b\\", "f".repeat(80));
    assert_eq!(
        replies(overlong.as_bytes()).1,
        b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\"
    );
}

#[test]
fn unsupported_malformed_and_cancelled_color_queries_do_not_reply() {
    for input in [
        b"\x1b]9;?\x07".as_slice(),
        b"\x1b]10\x07",
        b"\x1b]10;?;?\x07",
        b"\x1b]11;?\x18",
        b"\x1b]11;?\x1bX\x1b\\",
        b"\x1b]11;?\x1b\x07",
        b"\x1bP10;?\x1b\\",
    ] {
        assert!(
            replies(input).1.is_empty(),
            "unexpected reply for {input:?}"
        );
    }
}

#[test]
fn split_final_byte_is_budgeted_and_finish_discards_incomplete_queries() {
    let mut screen = Screen::new(1, 1).unwrap();
    let mut parser = Parser::new();
    let mut output = Vec::new();
    parser.advance_with_replies(&mut screen, b"\x1b[6", &mut |reply| {
        output.extend_from_slice(reply)
    });
    assert!(output.is_empty());
    parser.advance_with_replies(&mut screen, b"n", &mut |reply| {
        output.extend_from_slice(reply)
    });
    assert_eq!(output, b"\x1b[1;1R");
    parser.advance_with_replies(&mut screen, b"\x1b[?6", &mut |reply| {
        output.extend_from_slice(reply)
    });
    assert_eq!(output, b"\x1b[1;1R");
    parser.advance_with_replies(&mut screen, b"n", &mut |reply| {
        output.extend_from_slice(reply)
    });
    assert_eq!(output, b"\x1b[1;1R\x1b[?1;1R");
    parser.advance(&mut screen, b"\x1b[6");
    parser.finish(&mut screen);
    output.clear();
    parser.advance_with_replies(&mut screen, b"n", &mut |reply| {
        output.extend_from_slice(reply)
    });
    assert!(output.is_empty());
}
