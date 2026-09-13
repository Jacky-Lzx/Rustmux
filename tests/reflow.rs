use rustmux::{
    parser::Parser,
    screen::{SCROLLBACK_MAX_CELLS, SCROLLBACK_MAX_LINES, Screen},
    style::Cell,
};

fn feed(screen: &mut Screen, text: &str) {
    Parser::new().advance(screen, text.as_bytes());
}

fn content(screen: &Screen) -> Vec<Cell> {
    let mut result = Vec::new();
    for row in 0..screen.history_len() {
        result.extend_from_slice(
            &screen.history_row(row).unwrap()[..screen.history_row_used_columns(row).unwrap()],
        );
    }
    for row in 0..screen.dimensions().0 {
        result
            .extend_from_slice(&screen.row(row).unwrap()[..screen.row_used_columns(row).unwrap()]);
    }
    result
}

#[test]
fn widths_round_trip_text_styles_spaces_combining_and_wide_glyphs() {
    let mut screen = Screen::new(4, 8).unwrap();
    feed(&mut screen, "\x1b[31mab  中e\u{301}FGHIJK  末尾");
    let original = content(&screen);
    let snapshot = screen.clone();
    for width in [2, 7, 3, 12, 4, 8, 2, 8] {
        screen.resize(4, width).unwrap();
        assert_eq!(content(&screen), original, "width {width}");
        let (row, column) = screen.cursor();
        assert!(row < 4 && column < width);
    }
    assert_eq!(content(&snapshot), original);
    feed(&mut screen, "!");
    let mut expected = original;
    expected.push(Cell {
        character: '!',
        style: snapshot.style(),
        ..Default::default()
    });
    assert_eq!(content(&screen), expected);
}

#[test]
fn hard_newlines_stay_separate_and_pending_wrap_maps_to_insertion_point() {
    let mut screen = Screen::new(4, 4).unwrap();
    feed(&mut screen, "ab  \r\ncd");
    screen.resize(4, 8).unwrap();
    assert_eq!(screen.row_used_columns(0), Some(4));
    assert_eq!(screen.row_used_columns(1), Some(2));
    assert_eq!(screen.row_continued(1), Some(false));
    let mut full = Screen::new(2, 4).unwrap();
    feed(&mut full, "abcd\x1b7");
    full.resize(2, 2).unwrap();
    assert!(full.wrap_pending());
    assert_eq!(full.cursor(), (1, 1));
    feed(&mut full, "\x1b[H\x1b8e");
    assert_eq!(
        content(&full)
            .iter()
            .map(|c| c.character)
            .collect::<String>(),
        "abcde"
    );
}

#[test]
fn alternate_content_is_clipped_while_hidden_main_reflows() {
    let mut screen = Screen::new(3, 8).unwrap();
    feed(&mut screen, "abcdefghijk\x1b7");
    let original = content(&screen);
    feed(&mut screen, "\x1b[?1049h\x1b[HALTERNATE");
    screen.resize(3, 4).unwrap();
    assert!(screen.is_alternate());
    assert_eq!(
        screen
            .row(0)
            .unwrap()
            .iter()
            .map(|c| c.character)
            .collect::<String>(),
        "ALTE"
    );
    feed(&mut screen, "\x1b[?1049l\x1b8!");
    let actual = content(&screen);
    assert_eq!(&actual[..original.len()], original);
    assert_eq!(actual.last().unwrap().character, '!');
}

#[test]
fn one_column_replaces_wide_glyph_and_limits_history_after_expansion() {
    let mut screen = Screen::new(2, 8).unwrap();
    feed(&mut screen, "中A");
    screen.resize(2, 1).unwrap();
    assert_eq!(
        content(&screen)
            .iter()
            .map(|c| c.character)
            .collect::<String>(),
        "�A"
    );
    let mut screen = Screen::new(2, 80).unwrap();
    feed(&mut screen, &"x".repeat(20_000));
    screen.resize(2, 1).unwrap();
    assert!(screen.history_len() <= SCROLLBACK_MAX_LINES);
    let cells: usize = (0..screen.history_len())
        .map(|row| screen.history_row(row).unwrap().len())
        .sum();
    assert!(cells <= SCROLLBACK_MAX_CELLS);
    assert_eq!(content(&screen).len(), SCROLLBACK_MAX_LINES + 2);
    let before = screen.clone();
    assert!(screen.resize(2, 0).is_err());
    assert_eq!(screen, before);
}

#[test]
fn clearing_below_wrapped_output_preserves_width_round_trip() {
    let mut screen = Screen::new(6, 8).unwrap();
    feed(&mut screen, "abcdefgh\r\n");
    for width in [4, 8, 2, 8] {
        screen.resize(6, width).unwrap();
        // Shell prompt redraw: erase from the following blank line to the end.
        feed(&mut screen, "\x1b[J");
    }
    assert_eq!(screen.row_used_columns(0), Some(8));
    assert_eq!(
        screen
            .row(0)
            .unwrap()
            .iter()
            .map(|cell| cell.character)
            .collect::<String>(),
        "abcdefgh"
    );
    assert_eq!(screen.row_used_columns(1), Some(0));
}
