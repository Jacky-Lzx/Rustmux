use rustmux::{parser::Parser, render::Renderer, screen::Screen};
fn paint(renderer: &mut Renderer, screen: &Screen) -> Vec<u8> {
    let mut bytes = Vec::new();
    renderer.render(screen, &mut bytes).unwrap();
    bytes
}
fn changes(before: &str, after: &str, columns: usize) -> Vec<u8> {
    let mut screen = Screen::new(2, columns).unwrap();
    let mut replay = screen.clone();
    let mut renderer = Renderer::default();
    let mut parser = Parser::new();
    Parser::new().advance(&mut screen, before.as_bytes());
    parser.advance(&mut replay, &paint(&mut renderer, &screen));
    Parser::new().advance(&mut screen, after.as_bytes());
    let bytes = paint(&mut renderer, &screen);
    parser.advance(&mut replay, &bytes);
    for row in 0..2 {
        assert_eq!(screen.row(row), replay.row(row));
    }
    assert_eq!(screen.cursor(), replay.cursor());
    bytes
}
#[test]
fn single_cell_uses_its_column_and_preserves_neighboring_text() {
    let bytes = changes("abcdefghijklmnopqrst", "\x1b[1;10HX", 20);
    assert!(
        bytes
            .windows(b"\x1b[1;10HX".len())
            .any(|w| w == b"\x1b[1;10HX")
    );
    assert!(!bytes.windows(3).any(|w| w == b"abc"));
}
#[test]
fn separate_spans_and_fragmented_row_fallback() {
    let bytes = changes(&"a".repeat(80), "\x1b[1;2HX\x1b[1;70HY", 80);
    assert!(bytes.windows(b"\x1b[1;2H".len()).any(|w| w == b"\x1b[1;2H"));
    assert!(
        bytes
            .windows(b"\x1b[1;70H".len())
            .any(|w| w == b"\x1b[1;70H")
    );
    let bytes = changes("aaaaaaaaaaaaaaaaaaaa", "\x1b[Hbab ab ab ab ab ab ab", 20);
    assert!(bytes.windows(6).any(|w| w == b"\x1b[1;1H"));
}
#[test]
fn wide_old_and_new_boundaries_and_combining_style_and_blanks_replay() {
    for (before, after) in [
        ("中abc", "\x1b[1;2HX"),
        ("a中bc", "\x1b[H中"),
        ("中中中", "\x1b[1;2H中文"),
        ("abcdef中", "\x1b[1;8HX"),
        ("e\u{301}X", "\x1b[He"),
        ("eX", "\x1b[He\u{301}"),
        ("abcdefgh", "\x1b[1;4H\x1b[48;2;1;2;3m \x1b[K"),
        ("abcdefgh", "\x1b[1;4H\x1b[31md"),
    ] {
        changes(before, after, 8);
    }
}
#[test]
fn deterministic_edit_stream_matches_full_model_after_every_frame() {
    let mut screen = Screen::new(6, 20).unwrap();
    let mut replay = screen.clone();
    let mut renderer = Renderer::default();
    let mut parser = Parser::new();
    let mut rng = 7u64;
    for _ in 0..600 {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let row = (rng >> 32) % 6 + 1;
        let col = (rng >> 40) % 20 + 1;
        let edit = [
            "中",
            "X",
            "e\u{301}",
            " ",
            "\x1b[K",
            "\x1b[2P",
            "\x1b[2@",
            "\x1b[31mZ",
            "\x1b[0mZ",
            "\x1b[2J",
            "\n",
        ][rng as usize % 11];
        Parser::new().advance(&mut screen, format!("\x1b[{row};{col}H{edit}").as_bytes());
        parser.advance(&mut replay, &paint(&mut renderer, &screen));
        for row in 0..6 {
            assert_eq!(screen.row(row), replay.row(row));
        }
        assert_eq!(screen.cursor(), replay.cursor());
    }
}

#[test]
fn short_gaps_are_rewritten_as_one_span() {
    let bytes = changes(&"a".repeat(80), "\x1b[1;2HX\x1b[1;5HY\x1b[1;8HZ", 80);
    assert!(bytes.windows(13).any(|w| w == b"\x1b[1;2HXaaYaaZ"));
    assert!(!bytes.windows(6).any(|w| w == b"\x1b[1;5H"));
    assert!(!bytes.windows(6).any(|w| w == b"\x1b[1;8H"));
}

#[test]
fn equal_cost_gap_keeps_separate_spans() {
    let bytes = changes(&"a".repeat(80), "\x1b[1;2HX\x1b[1;9HY", 80);
    assert!(bytes.windows(7).any(|w| w == b"\x1b[1;2HX"));
    assert!(bytes.windows(7).any(|w| w == b"\x1b[1;9HY"));
    assert!(!bytes.windows(8).any(|w| w == b"XaaaaaaY"));
}

#[test]
fn short_but_expensive_styled_gap_is_not_rewritten() {
    let before = format!("{}\x1b[1;3H\x1b[38;2;10;20;30ma\x1b[0m", "a".repeat(80));
    let bytes = changes(&before, "\x1b[1;2HX\x1b[1;4HY", 80);
    assert!(bytes.windows(7).any(|w| w == b"\x1b[1;4HY"));
    assert!(!bytes.windows(8).any(|w| w == b"10;20;30"));
}

#[test]
fn wide_and_combining_gaps_preserve_complete_glyphs() {
    for (before, after, expected) in [
        ("a中a", "\x1b[1;1HX\x1b[1;4HY", "X中Y"),
        ("ae\u{301}a", "\x1b[1;1HX\x1b[1;3HY", "Xe\u{301}Y"),
    ] {
        let bytes = changes(before, after, 80);
        assert!(
            bytes
                .windows(expected.len())
                .any(|w| w == expected.as_bytes())
        );
    }
}
