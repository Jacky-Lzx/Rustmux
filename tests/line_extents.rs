use rustmux::{parser::Parser, screen::Screen};

fn feed(screen: &mut Screen, text: &str) {
    Parser::new().advance(screen, text.as_bytes());
}

#[test]
fn spaces_are_content_but_unwritten_cells_and_cursor_motion_are_not() {
    let mut s = Screen::new(3, 8).unwrap();
    assert_eq!(s.row_used_columns(0), Some(0));
    feed(&mut s, "a  ");
    assert_eq!(s.row_used_columns(0), Some(3));
    feed(&mut s, "\x1b[2;7H");
    assert_eq!(s.row_used_columns(1), Some(0));
    feed(&mut s, "X");
    assert_eq!(s.row_used_columns(1), Some(7));
    feed(&mut s, "\x1b[3;1He\u{301}");
    assert_eq!(s.row_used_columns(2), Some(1));
    assert_eq!(s.row_used_columns(3), None);
}

#[test]
fn early_wide_wrap_padding_is_excluded_even_with_background_color() {
    let mut s = Screen::new(2, 4).unwrap();
    feed(&mut s, "\x1b[44mabc中");
    assert_eq!(s.row_used_columns(0), Some(3));
    assert_eq!(s.row_used_columns(1), Some(2));
    assert_eq!(s.row_continued(1), Some(true));
    let mut s = Screen::new(2, 4).unwrap();
    feed(&mut s, "ab  c");
    assert_eq!(s.row_used_columns(0), Some(4));
    assert_eq!(s.row_used_columns(1), Some(1));
}

#[test]
fn erasure_updates_extent_and_preserves_colored_backgrounds() {
    let mut s = Screen::new(2, 8).unwrap();
    feed(&mut s, "abc   \x1b[1;4H\x1b[K");
    assert_eq!(s.row_used_columns(0), Some(3));
    feed(&mut s, "\x1b[1;2H\x1b[X");
    assert_eq!(s.row_used_columns(0), Some(3));
    feed(&mut s, "\x1b[44m\x1b[2J");
    assert_eq!(s.row_used_columns(0), Some(8));
    assert_eq!(s.row_used_columns(1), Some(8));
    feed(&mut s, "\x1b[0m\x1b[2J");
    assert_eq!(s.row_used_columns(0), Some(0));
    assert_eq!(s.row_used_columns(1), Some(0));
}

#[test]
fn character_and_line_shifts_move_lengths_without_retaining_erased_wide_halves() {
    let mut s = Screen::new(3, 8).unwrap();
    feed(&mut s, "abc \x1b[1;2H\x1b[2@");
    assert_eq!(s.row_used_columns(0), Some(6));
    feed(&mut s, "\x1b[2P");
    assert_eq!(s.row_used_columns(0), Some(4));
    feed(&mut s, "\x1b[2;1HX\x1b[H\x1b[L");
    assert_eq!(s.row_used_columns(0), Some(0));
    assert_eq!(s.row_used_columns(1), Some(4));
    assert_eq!(s.row_used_columns(2), Some(1));
    feed(&mut s, "\x1b[M");
    assert_eq!(s.row_used_columns(0), Some(4));
    let mut s = Screen::new(1, 4).unwrap();
    feed(&mut s, "A中\x1b[1;3H\x1b[P");
    assert_eq!(s.row_used_columns(0), Some(1));
}

#[test]
fn history_resize_and_alternate_preserve_lengths_with_snapshot_isolation() {
    let mut s = Screen::new(2, 4).unwrap();
    feed(&mut s, "ab  cd");
    s.resize(1, 4).unwrap();
    assert_eq!(s.history_row_used_columns(0), Some(4));
    assert_eq!(s.row_used_columns(0), Some(2));
    let snapshot = s.clone();
    s.resize(2, 4).unwrap();
    assert_eq!(s.row_used_columns(0), Some(4));
    assert_eq!(s.row_used_columns(1), Some(2));
    assert_eq!(s.history_row_used_columns(0), None);
    assert_eq!(snapshot.history_row_used_columns(0), Some(4));
    feed(&mut s, "\x1b[?1049h");
    assert_eq!(s.row_used_columns(0), Some(0));
    feed(&mut s, "\x1b[?1049l\x1b[!p");
    assert_eq!(s.row_used_columns(0), Some(4));
    let before = s.clone();
    assert!(s.resize(0, 4).is_err());
    assert_eq!(s, before);
    feed(&mut s, "\x1bc");
    assert_eq!(s.row_used_columns(0), Some(0));
    let mut s = Screen::new(1, 4).unwrap();
    feed(&mut s, "\x1b[?1049hA中");
    s.resize(1, 2).unwrap();
    assert_eq!(s.row_used_columns(0), Some(1));
}

#[test]
fn restoring_full_width_row_uses_its_extent_not_the_current_writing_background() {
    let mut s = Screen::new(1, 4).unwrap();
    feed(&mut s, "ab\r\n\x1b[44mX");
    assert_eq!(s.history_row_used_columns(0), Some(2));
    s.resize(2, 4).unwrap();
    assert_eq!(s.row_used_columns(0), Some(2));
}
