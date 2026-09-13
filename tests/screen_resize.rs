use rustmux::{
    parser::Parser,
    screen::Screen,
    style::{Color, Style},
};

fn screen(rows: usize, columns: usize, input: &str) -> Screen {
    let mut screen = Screen::new(rows, columns).unwrap();
    Parser::new().advance(&mut screen, input.as_bytes());
    screen
}

fn text(screen: &Screen, row: usize) -> String {
    screen
        .row(row)
        .unwrap()
        .iter()
        .filter(|c| c.width != 0)
        .flat_map(|c| std::iter::once(c.character).chain(c.combining.iter().copied()))
        .collect()
}

#[test]
fn grow_preserves_cells_styles_and_combining_suffixes() {
    let mut screen = screen(2, 4, "\x1b[31m中e\u{301}\x1b[44m");
    let old = screen.row(0).unwrap().to_vec();
    screen.resize(3, 6).unwrap();
    assert_eq!(&screen.row(0).unwrap()[..4], old);
    assert_eq!(text(&screen, 0), "中e\u{301}   ");
    assert_eq!(screen.cursor(), (0, 3));
    let blank = &screen.row(2).unwrap()[0];
    assert_eq!(blank.character, ' ');
    assert_eq!(
        blank.style,
        Style {
            background: Color::Indexed(4),
            ..Style::default()
        }
    );
    assert_eq!(screen.style().foreground, Color::Indexed(1));
}

#[test]
fn shrink_keeps_cursor_row_and_archives_top_without_width_reflow() {
    let mut screen = screen(3, 4, "abcdefghijkl");
    screen.resize(2, 2).unwrap();
    assert_eq!(text(&screen, 0), "ef");
    assert_eq!(text(&screen, 1), "ij");
    assert_eq!(screen.cursor(), (1, 1));
    assert_eq!(screen.history_len(), 1);
    assert_eq!(
        screen
            .history_row(0)
            .unwrap()
            .iter()
            .map(|c| c.character)
            .collect::<String>(),
        "abcd"
    );
    assert!(!screen.wrap_pending());
    screen.resize(3, 4).unwrap();
    assert_eq!(text(&screen, 0), "ef  ");
    assert_eq!(text(&screen, 1), "ij  ");
    assert_eq!(text(&screen, 2), "    ");
}

#[test]
fn clipped_wide_character_is_fully_removed() {
    let mut screen = screen(1, 4, "A中B\x1b[44m");
    screen.resize(1, 2).unwrap();
    assert_eq!(text(&screen, 0), "A ");
    assert_eq!(screen.row(0).unwrap()[1].width, 1);
    assert_eq!(
        screen.row(0).unwrap()[1].style.background,
        Color::Indexed(4)
    );
    let mut screen = self::screen(1, 4, "中AB");
    screen.resize(1, 1).unwrap();
    assert_eq!(text(&screen, 0), " ");
    screen.print('X');
    assert_eq!(text(&screen, 0), "X");
}

#[test]
fn alternate_and_saved_main_resize_with_independent_backgrounds() {
    let mut screen = screen(2, 4, "\x1b[41mMAIN\x1b[?1049h\x1b[H\x1b[44mALT\x1b[2;4H");
    screen.resize(3, 6).unwrap();
    assert!(screen.is_alternate());
    assert_eq!(text(&screen, 0), "ALT   ");
    assert_eq!(
        screen.row(2).unwrap()[0].style.background,
        Color::Indexed(4)
    );
    screen.resize(1, 3).unwrap();
    assert_eq!(screen.cursor(), (0, 2));
    screen.leave_alternate();
    assert_eq!(text(&screen, 0), "MAI");
    assert_eq!(screen.cursor(), (0, 2));
    assert_eq!(screen.style().background, Color::Indexed(1));
    assert!(!screen.wrap_pending());
    screen.resize(2, 5).unwrap();
    assert_eq!(
        screen.row(1).unwrap()[0].style.background,
        Color::Indexed(1)
    );
    screen.enter_alternate();
    assert_eq!(text(&screen, 0), "     ");
    assert_eq!(text(&screen, 1), "     ");
}

#[test]
fn unchanged_or_invalid_sizes_preserve_all_state() {
    let mut screen = screen(1, 4, "MAIN\x1b[?1049h\x1b[HALT!");
    let original = screen.clone();
    screen.resize(1, 4).unwrap();
    assert_eq!(screen, original);
    // usize::MAX cells deterministically exceeds Vec's capacity limit.
    for (rows, columns) in [(0, 4), (4, 0), (usize::MAX, 2), (1, usize::MAX)] {
        assert!(screen.resize(rows, columns).is_err());
        assert_eq!(screen, original);
    }
    screen.leave_alternate();
    assert!(screen.wrap_pending());
    assert_eq!(text(&screen, 0), "MAIN");
}

#[test]
fn height_shrink_archives_styled_rows_and_translates_saved_primary_cursor() {
    let mut screen = screen(4, 5, "\x1b[31m中e\u{301}\r\nBBBB\r\nCCCC\x1b7\r\nDDDD");
    let first = screen.row(0).unwrap().to_vec();
    let second = screen.row(1).unwrap().to_vec();
    let snapshot = screen.clone();
    screen.resize(2, 5).unwrap();
    assert_eq!(screen.history_row(0), Some(first.as_slice()));
    assert_eq!(screen.history_row(1), Some(second.as_slice()));
    assert_eq!(text(&screen, 0), "CCCC ");
    assert_eq!(text(&screen, 1), "DDDD ");
    assert_eq!(screen.cursor(), (1, 4));
    Parser::new().advance(&mut screen, b"\x1b8");
    assert_eq!(screen.cursor(), (0, 4));
    assert_eq!(snapshot.history_len(), 0);
    assert_eq!(snapshot.row(0), Some(first.as_slice()));
    screen.resize(4, 5).unwrap();
    assert_eq!(screen.history_len(), 2);
    assert_eq!(text(&screen, 2), "     "); // Growth does not pull history back yet.
}

#[test]
fn resize_archives_inactive_primary_but_never_alternate_rows() {
    let mut screen = screen(
        4,
        4,
        "AAAA\r\nBBBB\r\nCCCC\x1b7\r\nDDDD\x1b[?1049h\x1b[HALT\x1b[4;1H",
    );
    screen.resize(2, 4).unwrap();
    assert!(screen.is_alternate());
    assert_eq!(text(&screen, 0), "ALT ");
    assert_eq!(screen.history_len(), 2);
    screen.leave_alternate();
    assert_eq!(text(&screen, 0), "CCCC");
    assert_eq!(text(&screen, 1), "DDDD");
    assert_eq!(screen.cursor(), (1, 3));
    Parser::new().advance(&mut screen, b"\x1b8");
    assert_eq!(screen.cursor(), (0, 3));
}

#[test]
fn cursor_already_visible_keeps_origin_and_repeated_shrink_does_not_duplicate_history() {
    let mut screen = screen(4, 4, "AAAA\r\nBBBB\r\nCCCC\r\nDDDD\x1b[H");
    screen.resize(2, 4).unwrap();
    assert_eq!(text(&screen, 0), "AAAA");
    assert_eq!(text(&screen, 1), "BBBB");
    assert_eq!(screen.history_len(), 0);
    screen.position(1, 0);
    screen.resize(1, 4).unwrap();
    assert_eq!(text(&screen, 0), "BBBB");
    assert_eq!(screen.history_len(), 1);
    let before = screen.clone();
    screen.resize(1, 4).unwrap();
    assert_eq!(screen, before);
    assert!(screen.resize(0, 4).is_err());
    assert_eq!(screen, before);
}
