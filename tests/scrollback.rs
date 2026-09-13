use rustmux::{
    parser::Parser,
    screen::{SCROLLBACK_MAX_CELLS, SCROLLBACK_MAX_LINES, Screen},
    style::Color,
};

fn feed(screen: &mut Screen, bytes: &[u8]) {
    Parser::new().advance(screen, bytes);
}

fn history_text(screen: &Screen, index: usize) -> String {
    screen
        .history_row(index)
        .unwrap()
        .iter()
        .map(|cell| cell.character)
        .collect()
}

#[test]
fn full_screen_scroll_retains_exact_cells_in_order() {
    let mut screen = Screen::new(2, 5).unwrap();
    feed(&mut screen, "\x1b[31m中e\u{301}\r\nnext".as_bytes());
    let first = screen.row(0).unwrap().to_vec();
    let second = screen.row(1).unwrap().to_vec();
    assert_eq!(screen.history_len(), 0);
    feed(&mut screen, b"\r\n");
    assert_eq!(screen.history_row(0), Some(first.as_slice()));
    assert_eq!(
        screen.history_row(0).unwrap()[0].style.foreground,
        Color::Indexed(1)
    );
    assert_eq!(screen.history_row(0).unwrap()[1].width, 0);
    assert_eq!(screen.history_row(0).unwrap()[2].combining, vec!['\u{301}']);
    screen.scroll_up(usize::MAX);
    assert_eq!(screen.history_row(1), Some(second.as_slice()));
    assert_eq!(screen.history_len(), 3);
    assert!(screen.history_row(3).is_none());
}

#[test]
fn delayed_wrap_and_explicit_scroll_capture_but_editing_does_not() {
    let mut screen = Screen::new(1, 3).unwrap();
    feed(&mut screen, b"abc");
    assert_eq!(screen.history_len(), 0);
    feed(&mut screen, b"d");
    assert_eq!(history_text(&screen, 0), "abc");
    feed(&mut screen, b"\x1b[S");
    assert_eq!(history_text(&screen, 1), "d  ");
    let history = screen.clone();
    feed(&mut screen, b"\x1b[T\x1b[M\x1b[L\x1b[2J\x1bM");
    assert_eq!(screen.history_len(), history.history_len());
    assert_eq!(screen.history_row(0), history.history_row(0));
}

#[test]
fn partial_regions_and_alternate_output_do_not_enter_primary_history() {
    let mut screen = Screen::new(3, 4).unwrap();
    feed(&mut screen, b"main\r\nline\r\nlast\r\n");
    let first = screen.history_row(0).unwrap().to_vec();
    feed(&mut screen, b"\x1b[2;3r\x1b[3;1H\n\x1b[S");
    assert_eq!(screen.history_len(), 1);
    feed(&mut screen, b"\x1b[?1049hALT\x1b[99S\x1b[?1049l");
    assert_eq!(screen.history_len(), 1);
    assert_eq!(screen.history_row(0), Some(first.as_slice()));
}

#[test]
fn resize_preserves_original_history_widths_and_snapshot_isolation() {
    let mut screen = Screen::new(1, 4).unwrap();
    feed(&mut screen, b"abcd\r\n");
    let snapshot = screen.clone();
    screen.resize(2, 2).unwrap();
    feed(&mut screen, b"ef\r\ngh\r\n");
    assert_eq!(history_text(&screen, 0), "abcd");
    assert_eq!(history_text(&screen, 1), "ef");
    assert_eq!(snapshot.history_len(), 1);
    assert_eq!(history_text(&snapshot, 0), "abcd");
    let before = screen.clone();
    assert!(screen.resize(0, 2).is_err());
    assert_eq!(screen, before);
    screen.soft_reset();
    assert_eq!(screen.history_len(), 2);
    screen.clear_history();
    assert_eq!(screen.history_len(), 0);
    assert_eq!(screen.row(0), before.row(0));
    assert_eq!(snapshot.history_len(), 1);
    screen.scroll_up(1);
    assert_eq!(screen.history_len(), 1);
    screen.reset();
    assert_eq!(screen.history_len(), 0);
}

#[test]
fn line_and_cell_limits_evict_oldest_complete_rows() {
    let mut narrow = Screen::new(1, 1).unwrap();
    for index in 0..SCROLLBACK_MAX_LINES + 4 {
        narrow.print(char::from(b'a' + (index % 26) as u8));
        narrow.scroll_up(1);
    }
    assert_eq!(narrow.history_len(), SCROLLBACK_MAX_LINES);
    assert_eq!(history_text(&narrow, 0), "e");

    let mut wide = Screen::new(1, 100).unwrap();
    for _ in 0..SCROLLBACK_MAX_CELLS / 100 + 10 {
        wide.scroll_up(1);
    }
    assert_eq!(wide.history_len(), SCROLLBACK_MAX_CELLS / 100);
    let mut cells: usize = (0..wide.history_len())
        .map(|i| wide.history_row(i).unwrap().len())
        .sum();
    assert!(cells <= SCROLLBACK_MAX_CELLS);
    wide.resize(1, 1).unwrap();
    wide.scroll_up(1);
    cells += 1;
    assert_eq!(
        (0..wide.history_len())
            .map(|i| wide.history_row(i).unwrap().len())
            .sum::<usize>(),
        cells
    );

    wide.resize(1, SCROLLBACK_MAX_CELLS + 1).unwrap();
    wide.scroll_up(1);
    assert_eq!(wide.history_len(), 0);
}
