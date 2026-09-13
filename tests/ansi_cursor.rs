use rustmux::{parser::Parser, screen::Screen};
fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(4, 8).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut actual = Screen::new(4, 8).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut actual, &input[..split]);
        parser.advance(&mut actual, &input[split..]);
        assert_eq!(actual, expected);
    }
    expected
}
#[test]
fn ansi_and_dec_forms_share_full_state_and_the_same_save_slot() {
    let setup = b"\x1b[2;4r\x1b[?6h\x1b[31m\x1b(0abcdefgh";
    let changed = b"\x1b[?6l\x1b[?7l\x1b[0m\x1b(B\x1b[H";
    for (save, restore) in [
        (b"\x1b[s".as_slice(), b"\x1b[u".as_slice()),
        (b"\x1b7", b"\x1b[u"),
        (b"\x1b[s", b"\x1b8"),
    ] {
        let actual = parsed(&[setup.as_slice(), save, changed, restore].concat());
        let expected = parsed(&[setup.as_slice(), b"\x1b7", changed, b"\x1b8"].concat());
        assert_eq!(actual, expected);
        assert!(actual.origin_mode() && actual.auto_wrap() && actual.wrap_pending());
    }
    let actual = parsed(b"\x1b[2;3H\x1b[s\x1b[3;4H\x1b7\x1b[H\x1b[u");
    assert_eq!(actual.cursor(), (2, 3));
}
#[test]
fn repeated_restore_and_alternate_slots_work_without_reverting_global_modes() {
    let screen = parsed(b"\x1b[2;3H\x1b[s\x1b[?1049h\x1b[3;4H\x1b[s\x1b[H\x1b[u\x1b[u\x1b[?25l\x1b[?1049l\x1b[H\x1b[u");
    assert_eq!(screen.cursor(), (1, 2));
    assert!(!screen.cursor_visible());
    assert_eq!(parsed(b"abc\x1b[u"), parsed(b"abc"));
}
#[test]
fn saved_position_is_clamped_by_resize_and_reset_clears_it() {
    let mut screen = parsed(b"\x1b[4;8H\x1b[s");
    screen.resize(2, 3).unwrap();
    Parser::new().advance(&mut screen, b"\x1b[H\x1b[u");
    assert_eq!(screen.cursor(), (1, 2));
    assert!(!screen.wrap_pending());
    Parser::new().advance(&mut screen, b"\x1bcX\x1b[u");
    assert_eq!(screen.cursor(), (0, 1));
}
#[test]
fn parameterized_private_and_intermediate_forms_cannot_touch_the_save_slot() {
    for sequence in [
        b"\x1b[0s".as_slice(),
        b"\x1b[1;2s",
        b"\x1b[?s",
        b"\x1b[ s",
        b"\x1b[>1s",
        b"\x1b[0:0s",
        b"\x1b[0u",
        b"\x1b[97u",
        b"\x1b[>1u",
        b"\x1b[<u",
        b"\x1b[ u",
        b"\x1b]x\x1b[s\x07",
    ] {
        let screen = parsed(&[b"\x1b[2;3H\x1b[s\x1b[3;4H".as_slice(), sequence].concat());
        assert_eq!(screen.cursor(), (2, 3));
        let mut screen = screen;
        Parser::new().advance(&mut screen, b"\x1b[u");
        assert_eq!(screen.cursor(), (1, 2));
    }
}
