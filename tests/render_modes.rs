use rustmux::{
    parser::Parser,
    render::{Renderer, render},
    screen::Screen,
};

fn frame(renderer: &mut Renderer, screen: &Screen) -> Vec<u8> {
    let mut bytes = Vec::new();
    renderer.render(screen, &mut bytes).unwrap();
    bytes
}

fn assert_modes(actual: &Screen, expected: &Screen) {
    assert_eq!(actual.bracketed_paste(), expected.bracketed_paste());
    assert_eq!(
        actual.application_cursor_keys(),
        expected.application_cursor_keys()
    );
    assert_eq!(actual.application_keypad(), expected.application_keypad());
    assert_eq!(actual.cursor_shape(), expected.cursor_shape());
    assert_eq!(actual.focus_reporting(), expected.focus_reporting());
    assert_eq!(actual.mouse_tracking(), expected.mouse_tracking());
    assert_eq!(actual.sgr_mouse(), expected.sgr_mouse());
}

#[test]
fn unchanged_modes_are_omitted_even_when_text_changes() {
    let mut screen = Screen::new(2, 80).unwrap();
    let mut renderer = Renderer::default();
    frame(&mut renderer, &screen);
    // Cursor hide, style reset, final reset and cursor/visibility restoration remain.
    assert_eq!(
        frame(&mut renderer, &screen),
        b"\x1b[?25l\x1b[0m\x1b[0m\x1b[1;1H\x1b[?25h"
    );
    Parser::new().advance(&mut screen, b"X");
    let bytes = frame(&mut renderer, &screen);
    for mode in [b"\x1b[?2004".as_slice(), b"\x1b[?1l", b"\x1b>", b" q"] {
        assert!(!bytes.windows(mode.len()).any(|w| w == mode));
    }
}

#[test]
fn individual_mode_changes_and_resets_replay_without_redundant_commands() {
    let mut screen = Screen::new(2, 80).unwrap();
    let mut replay = screen.clone();
    let mut renderer = Renderer::default();
    let mut parser = Parser::new();
    parser.advance(&mut replay, &frame(&mut renderer, &screen));
    for input in [
        b"\x1b[?2004h".as_slice(),
        b"\x1b[?2004l",
        b"\x1b[?1h",
        b"\x1b[?1l",
        b"\x1b=",
        b"\x1b>",
        b"\x1b[5 q",
        b"\x1b[0 q",
        b"\x1b[?1004h",
        b"\x1b[?1004l",
        b"\x1b[?1003;1006h",
        b"\x1b[?1003;1006l",
        b"\x1b[?2004;1;1004h\x1b=\x1b[6 q",
        b"\x1bc",
    ] {
        Parser::new().advance(&mut screen, input);
        let changed = frame(&mut renderer, &screen);
        parser.advance(&mut replay, &changed);
        assert_modes(&replay, &screen);
        let unchanged = frame(&mut renderer, &screen);
        assert!(changed.len() > unchanged.len());
        assert_eq!(unchanged.len(), 26);
    }
}

#[test]
fn every_failed_prefix_invalidates_all_modes() {
    let initial = Screen::new(2, 80).unwrap();
    let mut screen = initial.clone();
    Parser::new().advance(&mut screen, b"\x1b[?2004;1;1004;1003;1006h\x1b=\x1b[6 qX");
    let mut renderer = Renderer::default();
    frame(&mut renderer, &initial);
    let changed = frame(&mut renderer, &screen);
    let mut full = Vec::new();
    render(&screen, &mut full).unwrap();
    for limit in 0..changed.len() {
        let mut renderer = Renderer::default();
        frame(&mut renderer, &initial);
        let mut short = vec![0; limit];
        assert!(renderer.render(&screen, &mut short.as_mut_slice()).is_err());
        assert_eq!(frame(&mut renderer, &screen), full);
    }
    renderer.invalidate();
    assert_eq!(frame(&mut renderer, &screen), full);
}
