use rustmux::{
    parser::Parser,
    render::{Renderer, render},
    screen::{MouseTracking, Screen},
};
fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(3, 8).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut actual = Screen::new(3, 8).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut actual, &input[..split]);
        parser.advance(&mut actual, &input[split..]);
        assert_eq!(actual, expected);
    }
    expected
}
#[test]
fn tracking_is_exclusive_and_encoding_is_independent() {
    for (mode, tracking) in [
        (1000, MouseTracking::Button),
        (1002, MouseTracking::Drag),
        (1003, MouseTracking::Any),
    ] {
        let mut screen =
            parsed(format!("\x1b[?1004;2004h\x1b[31mabcdefgh\x1b[?{mode};1006h").as_bytes());
        assert_eq!(screen.mouse_tracking(), tracking);
        assert!(screen.sgr_mouse());
        screen.set_mouse_tracking(MouseTracking::Off);
        screen.set_sgr_mouse(false);
        assert_eq!(screen, parsed(b"\x1b[?1004;2004h\x1b[31mabcdefgh"));
    }
    let screen = parsed(b"\x1b[?1000;1002;1003;1006h\x1b[?1006l");
    assert_eq!(screen.mouse_tracking(), MouseTracking::Any);
    assert!(!screen.sgr_mouse());
    let screen = parsed(b"\x1b[?1003;1006h\x1b[?1000l");
    assert_eq!(screen.mouse_tracking(), MouseTracking::Off);
    assert!(screen.sgr_mouse());
}
#[test]
fn global_state_survives_saves_resize_and_soft_reset_but_not_ris() {
    let mut screen = parsed(b"\x1b7\x1b[?1049h\x1b[?1002;1006h\x1b[?1049l\x1b8\x1b[!p");
    screen.resize(4, 10).unwrap();
    assert_eq!(screen.mouse_tracking(), MouseTracking::Drag);
    assert!(screen.sgr_mouse());
    screen.reset();
    assert_eq!(screen.mouse_tracking(), MouseTracking::Off);
    assert!(!screen.sgr_mouse());
}
#[test]
fn malformed_modes_are_ignored_and_queries_match_exclusive_state() {
    for input in [
        b"\x1b[1000h".as_slice(),
        b"\x1b[?1003:1h",
        b"\x1b[?1006 h",
        b"\x1b]x\x1b[?1000h\x07",
    ] {
        assert_eq!(parsed(input), Screen::new(3, 8).unwrap());
    }
    let mut screen = parsed(b"\x1b[?1000;1002;1006h");
    let mut bytes = Vec::new();
    Parser::new().advance_with_replies(
        &mut screen,
        b"\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1006$p",
        &mut |r| bytes.extend_from_slice(r),
    );
    assert_eq!(
        bytes,
        b"\x1b[?1000;2$y\x1b[?1002;1$y\x1b[?1003;2$y\x1b[?1006;1$y"
    );
}
#[test]
fn renderer_replays_state_and_only_resynchronizes_changes() {
    let mut renderer = Renderer::default();
    for input in [
        b"\x1b[?1000h".as_slice(),
        b"\x1b[?1002;1006h",
        b"\x1b[?1003h",
        b"",
    ] {
        let screen = parsed(input);
        let mut bytes = Vec::new();
        render(&screen, &mut bytes).unwrap();
        assert_eq!(parsed(&bytes), screen);
        bytes.clear();
        renderer.render(&screen, &mut bytes).unwrap();
        assert!(bytes.windows(8).any(|w| w == b"\x1b[?1000l"));
        bytes.clear();
        renderer.render(&screen, &mut bytes).unwrap();
        assert!(!bytes.windows(6).any(|w| w == b"\x1b[?100"));
    }
}
