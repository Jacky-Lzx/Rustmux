use rustmux::{parser::Parser, screen::Screen};

fn feed(screen: &mut Screen, text: &str) {
    Parser::new().advance(screen, text.as_bytes());
}

#[test]
fn only_actual_autowrap_marks_a_continuation() {
    let mut screen = Screen::new(4, 4).unwrap();
    feed(&mut screen, "abcd");
    assert_eq!(screen.row_continued(1), Some(false));
    feed(&mut screen, "e\u{301}");
    assert_eq!(screen.row_continued(1), Some(true));
    feed(&mut screen, "\r\nf");
    assert_eq!(screen.row_continued(2), Some(false));
    assert_eq!(screen.row_continued(4), None);
    feed(&mut screen, "\x1b[?7l\x1b[3;4Hxyz");
    assert_eq!(screen.row_continued(3), Some(false));
    feed(&mut screen, "\x1b[?7h\x1b[3;4H中");
    assert_eq!(screen.row_continued(3), Some(true));
}

#[test]
fn history_and_height_round_trip_preserve_continuations() {
    let mut screen = Screen::new(2, 3).unwrap();
    feed(&mut screen, "abcdefg");
    assert_eq!(screen.history_row_continued(0), Some(false));
    assert_eq!(screen.row_continued(0), Some(true));
    assert_eq!(screen.row_continued(1), Some(true));
    let snapshot = screen.clone();
    screen.resize(1, 3).unwrap();
    assert_eq!(screen.history_row_continued(1), Some(true));
    screen.resize(3, 3).unwrap();
    assert_eq!(screen.history_len(), 0);
    assert_eq!(screen.row_continued(0), Some(false));
    assert_eq!(screen.row_continued(1), Some(true));
    assert_eq!(screen.row_continued(2), Some(true));
    assert_eq!(snapshot.history_len(), 1);
    screen.resize(3, 2).unwrap();
    assert!((0..3).all(|row| screen.row_continued(row) == Some(true)));
}

#[test]
fn one_row_scrolling_and_alternate_state_keep_their_own_flags() {
    let mut screen = Screen::new(1, 2).unwrap();
    feed(&mut screen, "abcde");
    assert_eq!(screen.history_row_continued(0), Some(false));
    assert_eq!(screen.history_row_continued(1), Some(true));
    assert_eq!(screen.row_continued(0), Some(true));
    feed(&mut screen, "\x1b[?1049h");
    assert_eq!(screen.row_continued(0), Some(false));
    feed(&mut screen, "xyz");
    assert_eq!(screen.row_continued(0), Some(true));
    assert_eq!(screen.history_len(), 2);
    feed(&mut screen, "\x1b[?1049l\x1b[!p");
    assert_eq!(screen.row_continued(0), Some(true));
    feed(&mut screen, "\x1bc");
    assert_eq!(screen.row_continued(0), Some(false));
    assert_eq!(screen.history_row_continued(0), None);
}

#[test]
fn edits_and_partial_scrolls_break_connections_across_changed_boundaries() {
    let mut screen = Screen::new(4, 2).unwrap();
    feed(&mut screen, "abcdefg");
    assert_eq!(screen.row_continued(1), Some(true));
    feed(&mut screen, "\x1b[H\x1b[M");
    assert_eq!(screen.row_continued(0), Some(false));
    assert_eq!(screen.row_continued(1), Some(true));
    feed(&mut screen, "\x1b[1;3r\x1b[S");
    assert_eq!(screen.row_continued(0), Some(false));
    assert_eq!(screen.row_continued(3), Some(false));
    feed(&mut screen, "\x1b[r\x1b[Habcdefg\x1b[2;1H\x1b[K");
    assert_eq!(screen.row_continued(1), Some(false));
    assert_eq!(screen.row_continued(2), Some(false));
}

#[test]
fn insertion_mode_does_not_erase_new_autowrap_provenance() {
    let mut screen = Screen::new(2, 2).unwrap();
    feed(&mut screen, "\x1b[4habc");
    assert_eq!(screen.row_continued(1), Some(true));
}
