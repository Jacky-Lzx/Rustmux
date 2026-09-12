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
fn shrink_discards_bottom_and_right_without_reflow_or_resurrection() {
    let mut screen = screen(3, 4, "abcdefghijkl");
    screen.resize(2, 2).unwrap();
    assert_eq!(text(&screen, 0), "ab");
    assert_eq!(text(&screen, 1), "ef");
    assert_eq!(screen.cursor(), (1, 1));
    assert!(!screen.wrap_pending());
    screen.resize(3, 4).unwrap();
    assert_eq!(text(&screen, 0), "ab  ");
    assert_eq!(text(&screen, 1), "ef  ");
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
