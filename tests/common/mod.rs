use rustmux::screen::Screen;

// ANSI rendering preserves displayed cells, cursor and forwarded input modes,
// not the provenance of padding spaces, history or other parser-only metadata.
pub fn assert_rendered_screen_eq(actual: &Screen, expected: &Screen) {
    assert_eq!(actual.dimensions(), expected.dimensions());
    for row in 0..expected.dimensions().0 {
        assert_eq!(actual.row(row), expected.row(row));
    }
    assert_eq!(actual.cursor(), expected.cursor());
    assert_eq!(actual.cursor_visible(), expected.cursor_visible());
    assert_eq!(actual.cursor_shape(), expected.cursor_shape());
    assert_eq!(
        actual.application_cursor_keys(),
        expected.application_cursor_keys()
    );
    assert_eq!(actual.application_keypad(), expected.application_keypad());
    assert_eq!(
        actual.backarrow_sends_backspace(),
        expected.backarrow_sends_backspace()
    );
    assert_eq!(actual.bracketed_paste(), expected.bracketed_paste());
    assert_eq!(actual.focus_reporting(), expected.focus_reporting());
    assert_eq!(actual.mouse_tracking(), expected.mouse_tracking());
    assert_eq!(actual.sgr_mouse(), expected.sgr_mouse());
}
