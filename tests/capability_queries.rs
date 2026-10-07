use rustmux::{
    parser::{MAX_REPLY_BYTES, Parser},
    screen::Screen,
};

fn collect(input: &[u8]) -> Vec<u8> {
    let mut expected = None;
    for split in 0..=input.len() {
        let mut parser = Parser::new();
        let mut screen = Screen::new(3, 8).unwrap();
        parser.advance(&mut screen, b"\x1b[31mabcdefgh\x1b[?2026h");
        let before = screen.clone();
        let mut output = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            parser.advance_with_replies(&mut screen, chunk, &mut |reply| {
                output.extend_from_slice(reply)
            });
        }
        assert_eq!(screen, before);
        if let Some(expected) = &expected {
            assert_eq!(&output, expected, "split {split}");
        } else {
            expected = Some(output);
        }
    }
    let mut parser = Parser::new();
    let mut screen = Screen::new(3, 8).unwrap();
    let mut bytewise = Vec::new();
    for byte in input {
        let start = bytewise.len();
        parser.advance_with_replies(&mut screen, &[*byte], &mut |reply| {
            bytewise.extend_from_slice(reply)
        });
        assert!(bytewise.len() - start <= MAX_REPLY_BYTES);
    }
    let expected = expected.unwrap();
    assert_eq!(bytewise, expected);
    expected
}

#[test]
fn supported_names_and_hex_case_reply_in_order() {
    assert_eq!(
        collect(b"\x1bP+q436f;636f6c6f7273;524742\x1b\\"),
        b"\x1bP1+r436f=323536;636f6c6f7273=323536;524742=38\x1b\\"
    );
    assert_eq!(
        collect(b"\x1bP+q436F;636F6C6F7273\x1b\\"),
        b"\x1bP1+r436F=323536;636F6C6F7273=323536\x1b\\"
    );
}

#[test]
fn first_unknown_or_malformed_name_ends_the_list() {
    for name in ["", "TN", "name", "co", "RGBx", "Tc", "kcuu1", "setrgbf"] {
        let encoded: String = name.bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            collect(format!("\x1bP+q{encoded};436f\x1b\\").as_bytes()),
            b"\x1bP0+r\x1b\\"
        );
    }
    for name in ["4", "43gg", "00", " 436f", "436f=323536", "436f\x07"] {
        assert_eq!(
            collect(format!("\x1bP+q{name}\x1b\\").as_bytes()),
            b"\x1bP0+r\x1b\\"
        );
        assert_eq!(
            collect(format!("\x1bP+q524742;{name};436f\x1b\\").as_bytes()),
            b"\x1bP1+r524742=38\x1b\\"
        );
    }
}

#[test]
fn maximum_query_and_single_completion_byte_stay_inside_reply_budget() {
    // 64 retained bytes exactly: +q, twelve Co names, and unknown 00.
    let names = ["436f"; 12].join(";");
    let request = format!("\x1bP+q{names};00\x1b\\");
    let values = ["436f=323536"; 12].join(";");
    assert_eq!(
        collect(request.as_bytes()),
        format!("\x1bP1+r{values}\x1b\\").as_bytes()
    );
    assert!(collect(format!("\x1bP+q{names};000\x1b\\").as_bytes()).is_empty());
}

#[test]
fn unrelated_controls_echoes_cancelled_and_unfinished_queries_are_silent() {
    for request in [
        b"\x1bP+p436f\x1b\\".as_slice(),
        b"\x1bP+Q436f\x1b\\",
        b"\x1bP +q436f\x1b\\",
        b"\x1bP1+r436f=323536\x1b\\",
        b"\x1bP0+r\x1b\\",
        b"\x1bP+q436f\x18",
        b"\x1bP+q436f\x1a",
        b"\x1bP+q436f\x1b[5n\x1b\\",
        b"\x1bP+q436f\x07",
        b"\x1bP+q436f",
        b"\x1bP+q436f\x1b",
        b"\x1b]ignored\x1bP+q436f\x1b\\",
    ] {
        assert!(collect(request).is_empty(), "{request:?}");
    }
    assert_eq!(collect(b"\x1bP+q436f\x18\x1b[5n"), b"\x1b[0n");
    let mut parser = Parser::new();
    let mut screen = Screen::new(3, 8).unwrap();
    parser.advance(&mut screen, b"\x1bP+q436f\x1b");
    parser.finish(&mut screen);
    let mut replies = Vec::new();
    parser.advance_with_replies(&mut screen, b"\x1b\\\x1b[5n", &mut |r| {
        replies.extend_from_slice(r)
    });
    assert_eq!(replies, b"\x1b[0n");
}

#[test]
fn oversized_dcs_discards_then_recovers_without_retaining_the_payload() {
    let mut parser = Parser::new();
    let mut screen = Screen::new(3, 8).unwrap();
    let before = screen.clone();
    parser.advance(&mut screen, b"\x1bP+q");
    for _ in 0..1024 {
        parser.advance(&mut screen, &[b'4'; 1024]);
    }
    let mut output = Vec::new();
    parser.advance_with_replies(&mut screen, b"\x1b\\\x1bP+q436f\x1b\\", &mut |r| {
        output.extend_from_slice(r)
    });
    assert_eq!(output, b"\x1bP1+r436f=323536\x1b\\");
    assert_eq!(screen, before);
}

#[test]
fn display_only_replay_does_not_leave_a_deferred_reply() {
    let mut parser = Parser::new();
    let mut screen = Screen::new(3, 8).unwrap();
    parser.advance(&mut screen, b"\x1bP+q436f\x1b\\");
    let mut output = Vec::new();
    parser.advance_with_replies(&mut screen, b"\x1b[5n", &mut |r| {
        output.extend_from_slice(r)
    });
    assert_eq!(output, b"\x1b[0n");
}

#[test]
fn capabilities_remain_local_and_stable_across_buffers_resize_and_reset() {
    for setup in [
        b"\x1b[31mabcdefgh".as_slice(),
        b"\x1b[?1049h\x1b[48;2;1;2;3mabcdefgh\x1b[?2026h",
        b"\x1b[?1049h\x1b[!p",
        b"\x1b[?1049h\x1bc",
    ] {
        let mut parser = Parser::new();
        let mut screen = Screen::new(3, 8).unwrap();
        parser.advance(&mut screen, setup);
        screen.resize(4, 10).unwrap();
        let before = screen.clone();
        let mut output = Vec::new();
        parser.advance_with_replies(&mut screen, b"\x1bP+q436f;524742\x1b\\", &mut |r| {
            output.extend_from_slice(r)
        });
        assert_eq!(output, b"\x1bP1+r436f=323536;524742=38\x1b\\");
        assert_eq!(screen, before);
    }
}
