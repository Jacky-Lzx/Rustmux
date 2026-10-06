use rustmux::{
    layout::{Layout, SplitAxis},
    pane_view::compose,
    parser::Parser,
    render::{Renderer, render},
    screen::{EraseMode, Screen},
    style::Cell,
};
use std::io::{self, Write};

const CLOSE: &[u8] = b"\x1b]8;;\x1b\\";
const URI: &str = "https://example.test/path;a?b=1#part";
fn open(id: &str, uri: &str) -> String {
    format!("\x1b]8;{id};{uri}\x1b\\")
}
fn linked(cell: &Cell) -> bool {
    cell.hyperlink.is_some()
}
fn draw(screen: &Screen) -> Vec<u8> {
    let mut output = Vec::new();
    render(screen, &mut output).unwrap();
    output
}
fn contains(bytes: &[u8], part: &[u8]) -> bool {
    bytes.windows(part.len()).any(|w| w == part)
}

#[test]
fn every_split_of_long_st_and_bel_commands_preserves_linked_glyphs() {
    let uri = format!("https://example.test/{}", "x".repeat(500));
    for terminator in ["\x1b\\", "\x07"] {
        let input = format!("\x1b]8;id=source;{uri}{terminator}中e\u{301}\x1b]8;;{terminator}X");
        for split in 0..=input.len() {
            let mut screen = Screen::new(1, 8).unwrap();
            let mut parser = Parser::new();
            parser.advance(&mut screen, &input.as_bytes()[..split]);
            parser.advance(&mut screen, &input.as_bytes()[split..]);
            let row = screen.row(0).unwrap();
            assert_eq!(row[0].hyperlink.as_ref().unwrap().uri(), uri);
            assert_eq!(row[0].hyperlink, row[1].hyperlink);
            assert_eq!(row[0].hyperlink, row[2].hyperlink);
            assert_eq!(row[2].combining, ['\u{301}']);
            assert!(!linked(&row[3]));
        }
    }
}

#[test]
fn explicit_grouping_is_pane_local_and_anonymous_openings_are_independent() {
    let mut left = Screen::new(1, 8).unwrap();
    let mut right = Screen::new(1, 8).unwrap();
    let mut parser = Parser::new();
    let input = format!(
        "{}A\x1b]8;;\x1b\\{}B{}C{}D{}E",
        open("id=shared", URI),
        open("future=value:id=shared", URI),
        open("", URI),
        open("id=", URI),
        open("id=shared", "file:///other")
    );
    parser.advance(&mut left, input.as_bytes());
    Parser::new().advance(
        &mut right,
        format!("{}A", open("id=shared", URI)).as_bytes(),
    );
    let row = left.row(0).unwrap();
    assert_eq!(row[0].hyperlink, row[1].hyperlink);
    assert_ne!(row[0].hyperlink, row[2].hyperlink);
    assert_ne!(row[2].hyperlink, row[3].hyperlink);
    assert_ne!(row[0].hyperlink, row[4].hyperlink);
    assert_ne!(row[0].hyperlink, right.row(0).unwrap()[0].hyperlink);
    assert_eq!(left.clone(), left);
}

#[test]
fn invalid_commands_close_the_previous_link_and_never_emit_payloads() {
    let mut cases = vec![
        "id=a:id=b;https://invalid".into(),
        "bad;https://invalid".into(),
        ";https://invalid/with space".into(),
        ";https://invalid/é".into(),
        ";https://invalid/\x1b[31m".into(),
        ";https://invalid/\x7f".into(),
        ";https://invalid/\n".into(),
        "missing-separator".into(),
    ];
    cases.push(format!("{};https://invalid", "p".repeat(257)));
    cases.push(format!(";{}", "u".repeat(2084)));
    cases.push(format!(";{}", "u".repeat(100_000)));
    for payload in cases {
        let mut screen = Screen::new(1, 5).unwrap();
        let mut parser = Parser::new();
        parser.advance(
            &mut screen,
            format!("{}A\x1b]8;{payload}\x1b\\B", open("", URI)).as_bytes(),
        );
        let row = screen.row(0).unwrap();
        assert!(linked(&row[0]), "{payload:?}");
        assert_eq!(row[1].character, 'B');
        assert!(!linked(&row[1]), "{payload:?}");
        let output = draw(&screen);
        assert!(!contains(&output, b"https://invalid"));
    }
}

#[test]
fn maximum_lengths_are_accepted_and_unrelated_osc_reply_bound_is_unchanged() {
    let mut screen = Screen::new(1, 5).unwrap();
    let mut parser = Parser::new();
    let uri = "u".repeat(2083);
    let id = format!("id={}", "x".repeat(253));
    parser.advance(&mut screen, format!("{}A", open(&id, &uri)).as_bytes());
    assert_eq!(
        screen.row(0).unwrap()[0].hyperlink.as_ref().unwrap().uri(),
        uri
    );
    let mut reply = Vec::new();
    parser.advance_with_replies(
        &mut screen,
        format!("\x1b]4;0;?{}\x1b\\B", ";0;?".repeat(100)).as_bytes(),
        &mut |bytes| reply.extend_from_slice(bytes),
    );
    assert!(reply.is_empty());
    assert!(linked(&screen.row(0).unwrap()[1]));
    assert_eq!(rustmux::parser::MAX_REPLY_BYTES, 420);
}

#[test]
fn cancellation_does_not_apply_an_unfinished_command() {
    let mut screen = Screen::new(1, 5).unwrap();
    let mut parser = Parser::new();
    parser.advance(
        &mut screen,
        format!("{}A\x1b]8;;https://ignored\x18B", open("", URI)).as_bytes(),
    );
    assert_eq!(
        screen.row(0).unwrap()[0].hyperlink,
        screen.row(0).unwrap()[1].hyperlink
    );
}

#[test]
fn link_is_independent_of_sgr_and_saved_cursor_but_resets_close_it() {
    let mut screen = Screen::new(2, 12).unwrap();
    let mut parser = Parser::new();
    parser.advance(
        &mut screen,
        format!("{}A\x1b7\x1b[0mB", open("", URI)).as_bytes(),
    );
    assert_eq!(
        screen.row(0).unwrap()[0].hyperlink,
        screen.row(0).unwrap()[1].hyperlink
    );
    parser.advance(&mut screen, b"\x1b]8;;\x1b\\\x1b8C");
    assert!(!linked(&screen.row(0).unwrap()[1]));
    parser.advance(
        &mut screen,
        format!("{}D\x1b[!pE", open("", URI)).as_bytes(),
    );
    assert!(linked(&screen.row(0).unwrap()[2]));
    assert!(!linked(&screen.row(0).unwrap()[3]));
    parser.advance(&mut screen, format!("{}\x1bcF", open("", URI)).as_bytes());
    assert!(screen.row(0).unwrap().iter().all(|c| !linked(c)));
}

#[test]
fn buffer_switch_closes_active_link_and_retains_linked_primary_cells() {
    let mut screen = Screen::new(2, 10).unwrap();
    let mut parser = Parser::new();
    parser.advance(
        &mut screen,
        format!("{}A\x1b[?1049hB", open("id=a", URI)).as_bytes(),
    );
    assert!(!linked(&screen.row(0).unwrap()[0]));
    parser.advance(
        &mut screen,
        format!("{}C\x1b[?1049lD", open("id=b", URI)).as_bytes(),
    );
    assert!(linked(&screen.row(0).unwrap()[0]));
    assert!(!linked(&screen.row(0).unwrap()[1]));
}

#[test]
fn edits_move_links_and_erasure_does_not_inherit_them() {
    let mut screen = Screen::new(1, 10).unwrap();
    Parser::new().advance(&mut screen, format!("{}A中BC", open("", URI)).as_bytes());
    let id = screen.row(0).unwrap()[0].hyperlink.clone();
    screen.move_to(0, 0);
    screen.insert_characters(1);
    assert!(!linked(&screen.row(0).unwrap()[0]));
    assert_eq!(screen.row(0).unwrap()[1].hyperlink, id);
    screen.move_to(0, 2);
    screen.delete_characters(2);
    assert_eq!(screen.row(0).unwrap()[2].character, 'B');
    assert_eq!(screen.row(0).unwrap()[2].hyperlink, id);
    screen.erase_line(EraseMode::All);
    assert!(screen.row(0).unwrap().iter().all(|c| !linked(c)));
}

#[test]
fn late_variation_selector_preserves_link_identity_on_both_wide_cells() {
    let mut screen = Screen::new(2, 5).unwrap();
    let mut parser = Parser::new();
    parser.advance(&mut screen, format!("{}❤", open("", URI)).as_bytes());
    parser.advance(&mut screen, CLOSE);
    parser.advance(&mut screen, "\u{fe0f}".as_bytes());
    let row = screen.row(0).unwrap();
    assert_eq!(row[0].width, 2);
    assert_eq!(row[0].hyperlink, row[1].hyperlink);
    assert!(linked(&row[1]));
}

#[test]
fn history_reflow_and_resize_preserve_cells_and_current_link() {
    let mut screen = Screen::new(2, 4).unwrap();
    let mut parser = Parser::new();
    parser.advance(
        &mut screen,
        format!("{}abcdefghijkl", open("id=wrap", URI)).as_bytes(),
    );
    let id = screen.history_row(0).unwrap()[0].hyperlink.clone();
    screen.resize(3, 3).unwrap();
    parser.advance(&mut screen, b"m");
    let all: Vec<_> = (0..screen.history_len())
        .flat_map(|r| screen.history_row(r).unwrap())
        .chain((0..3).flat_map(|r| screen.row(r).unwrap()))
        .filter(|c| c.character != ' ')
        .collect();
    let text: String = all.iter().map(|c| c.character).collect();
    assert_eq!(text, "abcdefghijklm");
    assert!(all.iter().all(|c| c.hyperlink == id));
}

#[test]
fn pane_composition_keeps_links_on_content_and_off_borders() {
    let mut layout = Layout::new(7, 15).unwrap();
    let left = layout.active();
    let right = layout.split_active(SplitAxis::Columns).unwrap();
    let mut screens = Vec::new();
    for id in [left, right] {
        let (_, rect) = layout
            .content_geometry()
            .panes
            .into_iter()
            .find(|(p, _)| *p == id)
            .unwrap();
        let mut screen = Screen::new(rect.rows.into(), rect.columns.into()).unwrap();
        Parser::new().advance(
            &mut screen,
            format!("{}LINK", open("id=same", URI)).as_bytes(),
        );
        screens.push(screen);
    }
    let frame = compose(&layout, &[(left, &screens[0]), (right, &screens[1])]).unwrap();
    let mut linked_count = 0;
    for cell in (0..7).flat_map(|r| frame.row(r).unwrap()) {
        if linked(cell) {
            linked_count += 1;
            assert!("LINK".contains(cell.character));
        }
    }
    assert_eq!(linked_count, 8);
}

#[test]
fn partial_redraw_observes_link_only_changes_and_closes_every_run() {
    let mut screen = Screen::new(2, 80).unwrap();
    let mut renderer = Renderer::default();
    renderer.render(&screen, &mut Vec::new()).unwrap();
    let mut parser = Parser::new();
    parser.advance(
        &mut screen,
        format!("\x1b[1;20H{}X", open("id=first", URI)).as_bytes(),
    );
    let mut first = Vec::new();
    renderer.render(&screen, &mut first).unwrap();
    assert!(contains(&first, b"\x1b[1;20H\x1b]8;id=rmx-"));
    assert!(contains(&first, b"X\x1b]8;;\x1b\\\x1b[0m"));
    parser.advance(
        &mut screen,
        format!("\x1b[1;20H{}X", open("id=second", URI)).as_bytes(),
    );
    let mut second = Vec::new();
    renderer.render(&screen, &mut second).unwrap();
    assert_ne!(first, second);
    assert!(contains(&second, b"X\x1b]8;;\x1b\\"));
    parser.advance(&mut screen, b"\x1b]8;;\x1b\\\x1b[1;20HX");
    let mut plain = Vec::new();
    renderer.render(&screen, &mut plain).unwrap();
    assert!(contains(&plain, b"\x1b[1;20HX"));
    assert!(!contains(&plain, b"\x1b]8;id="));
}

#[test]
fn error_invalidates_cache_and_next_frame_closes_a_partially_open_link() {
    struct Broken(usize);
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::Error::other("broken output"));
            }
            let n = self.0.min(bytes.len());
            self.0 -= n;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut screen = Screen::new(1, 4).unwrap();
    Parser::new().advance(&mut screen, format!("{}AB", open("", URI)).as_bytes());
    let mut renderer = Renderer::default();
    let first = draw(&screen);
    let start = first
        .windows(b"\x1b]8;id=".len())
        .position(|w| w == b"\x1b]8;id=")
        .unwrap();
    assert!(renderer.render(&screen, &mut Broken(start + 12)).is_err());
    let mut repaired = Vec::new();
    renderer.render(&screen, &mut repaired).unwrap();
    assert!(repaired.starts_with(CLOSE));
    assert_eq!(repaired, first);
}

#[test]
fn excessive_link_output_falls_back_to_text_and_repaints_when_budget_recovers() {
    let mut screen = Screen::new(1, 1200).unwrap();
    let mut parser = Parser::new();
    let uri = "u".repeat(2083);
    let mut renderer = Renderer::default();
    parser.advance(
        &mut screen,
        format!("{}X", open("id=single", &uri)).as_bytes(),
    );
    let initial = draw(&screen);
    assert!(contains(&initial, uri.as_bytes()));
    renderer.render(&screen, &mut Vec::new()).unwrap();
    let mut input = String::from("\x1b[H");
    for _ in 0..600 {
        input.push_str(&open("", &uri));
        input.push_str("X\x1b]8;;\x1b\\X");
    }
    parser.advance(&mut screen, input.as_bytes());
    let mut fallback = Vec::new();
    renderer.render(&screen, &mut fallback).unwrap();
    assert!(contains(&fallback, &vec![b'X'; 1200]));
    assert!(!contains(&fallback, b"\x1b]8;id="));
    parser.advance(&mut screen, b"\x1b]8;;\x1b\\\x1b[H\x1b[K");
    parser.advance(
        &mut screen,
        format!("{}X", open("id=single", &uri)).as_bytes(),
    );
    let mut recovered = Vec::new();
    renderer.render(&screen, &mut recovered).unwrap();
    assert!(contains(&recovered, uri.as_bytes()));
    assert!(contains(&recovered, b"\x1b[1;1H"));
}
