use rustmux::parser::Parser;
use rustmux::screen::Screen;
use rustmux::style::Color;

fn invariant(screen: &Screen) {
    for row in 0..screen.dimensions().0 {
        let cells = screen.row(row).unwrap();
        for (column, cell) in cells.iter().enumerate() {
            assert!(cell.combining.len() <= 16);
            match cell.width {
                0 => {
                    assert!(column > 0);
                    assert_eq!(cells[column - 1].width, 2);
                    assert_eq!(cell.style, cells[column - 1].style);
                    assert!(cell.combining.is_empty());
                }
                1 => {}
                2 => {
                    assert!(column + 1 < cells.len());
                    assert_eq!(cells[column + 1].width, 0);
                }
                _ => panic!("invalid cell width"),
            }
        }
    }
}

fn text(screen: &Screen, row: usize) -> String {
    screen
        .row(row)
        .unwrap()
        .iter()
        .filter(|cell| cell.width != 0)
        .flat_map(|cell| std::iter::once(cell.character).chain(cell.combining.iter().copied()))
        .collect()
}

fn parse(rows: usize, columns: usize, input: &[u8]) -> Screen {
    let mut expected = Screen::new(rows, columns).unwrap();
    let mut parser = Parser::new();
    parser.advance(&mut expected, input);
    parser.finish(&mut expected);
    invariant(&expected);
    for split in 0..=input.len() {
        let mut screen = Screen::new(rows, columns).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        invariant(&screen);
        parser.advance(&mut screen, &input[split..]);
        parser.finish(&mut screen);
        invariant(&screen);
        assert_eq!(screen, expected, "split at {split}");
    }
    let mut screen = Screen::new(rows, columns).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
        invariant(&screen);
    }
    parser.finish(&mut screen);
    assert_eq!(screen, expected);
    expected
}

#[test]
fn zwj_emoji_components_share_one_cell_across_every_read_boundary() {
    for emoji in ["👩‍💻", "👨‍👩‍👧‍👦", "👩🏽‍💻", "🧑🏻‍🤝‍🧑🏿", "🏳️‍🌈", "❤️‍🔥"]
    {
        let screen = parse(2, 16, format!("{emoji}X").as_bytes());
        let cells = screen.row(0).unwrap();
        let mut scalars = emoji.chars();
        assert_eq!(cells[0].character, scalars.next().unwrap(), "{emoji}");
        assert_eq!(cells[0].combining, scalars.collect::<Vec<_>>(), "{emoji}");
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[2].character, 'X');
        assert_eq!(screen.cursor(), (0, 3));
    }
}

#[test]
fn zwj_components_keep_the_leaders_style_and_hyperlink() {
    let input = "\x1b[31m\x1b]8;;https://example.test/first\x1b\\👩\u{200d}\x1b[32m\x1b]8;;https://example.test/second\x1b\\💻X".as_bytes();
    // Each Screen allocates different external hyperlink IDs, so compare the
    // retained metadata rather than requiring identity across independent grids.
    for split in 0..=input.len() {
        let mut screen = Screen::new(2, 16).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        invariant(&screen);
        let cells = screen.row(0).unwrap();
        assert_eq!(cells[0].combining, ['\u{200d}', '💻']);
        assert_eq!(cells[0].style.foreground, Color::Indexed(1));
        assert_eq!(cells[2].style.foreground, Color::Indexed(2));
        assert_ne!(cells[0].hyperlink, cells[2].hyperlink);
        assert_eq!(cells[1].hyperlink, cells[0].hyperlink);
        assert_eq!(
            cells[0].hyperlink.as_ref().unwrap().uri(),
            "https://example.test/first"
        );
        assert_eq!(
            cells[2].hyperlink.as_ref().unwrap().uri(),
            "https://example.test/second"
        );
    }
}

#[test]
fn zwj_suffixes_do_not_trigger_wrap_or_insert_additional_columns() {
    let screen = parse(2, 4, "ab👩‍💻X".as_bytes());
    assert_eq!(text(&screen, 0), "ab👩‍💻");
    assert_eq!(text(&screen, 1), "X   ");
    assert_eq!(screen.cursor(), (1, 1));
    let screen = parse(2, 4, "\x1b[?7lab👩‍💻".as_bytes());
    assert_eq!(text(&screen, 0), "ab👩‍💻");
    assert_eq!(screen.cursor(), (0, 3));
    let screen = parse(2, 8, "abcdef\r\x1b[4h👩‍💻".as_bytes());
    assert_eq!(text(&screen, 0), "👩‍💻abcdef");
    let screen = parse(2, 1, "👩‍💻".as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].character, '�');
    assert_eq!(screen.row(1).unwrap()[0].character, '�');
    assert_eq!(text(&screen, 0), "�\u{200d}");
}

#[test]
fn invalid_zwj_components_and_late_text_presentation_do_not_hide_columns() {
    for input in ["A‍💻", "👩‍A", "👩‍\u{fe0e}💻", "👩‍\u{200d}💻"] {
        let screen = parse(2, 16, input.as_bytes());
        assert!(
            screen
                .row(0)
                .unwrap()
                .iter()
                .filter(|c| c.width != 0)
                .any(|c| c.character == 'A' || c.character == '💻')
        );
        assert!(screen.cursor().1 >= 3, "{input}");
    }
    // Text-default components requiring a following VS16 are a documented
    // boundary of this increment; keep their separately occupied columns.
    let screen = parse(2, 16, "👩‍❤️".as_bytes());
    assert_eq!(screen.cursor(), (0, 4));
    assert_eq!(screen.row(0).unwrap()[2].character, '❤');
    let screen = parse(2, 16, "\x1b[31m👩‍💻\x1b[32m\u{fe0e}X".as_bytes());
    assert_eq!(screen.cursor(), (0, 4));
    assert_eq!(screen.row(0).unwrap()[0].combining, ['\u{200d}']);
    assert_eq!(screen.row(0).unwrap()[2].character, '💻');
    assert_eq!(screen.row(0).unwrap()[2].combining, ['\u{fe0e}']);
    assert_eq!(
        screen.row(0).unwrap()[2].style.foreground,
        Color::Indexed(1)
    );
    assert_eq!(
        screen.row(0).unwrap()[3].style.foreground,
        Color::Indexed(2)
    );
    let screen = parse(2, 4, "ab👩‍💻\u{fe0e}X".as_bytes());
    assert_eq!(text(&screen, 0), "ab👩\u{200d}");
    assert_eq!(text(&screen, 1), "💻\u{fe0e}X  ");
    let screen = parse(2, 4, "\x1b[?7lab👩‍💻\u{fe0e}".as_bytes());
    assert_eq!(text(&screen, 0), "ab👩‍💻");
    let screen = parse(2, 8, "abcdef\r\x1b[4h👩‍💻\u{fe0e}".as_bytes());
    assert_eq!(text(&screen, 0), "👩‍💻\u{fe0e}abcde");
}

#[test]
fn zwj_overwrite_reflow_and_storage_limit_preserve_cell_invariants() {
    for column in [1, 2] {
        let screen = parse(2, 8, format!("👩‍💻\x1b[1;{column}HX").as_bytes());
        assert!(screen.row(0).unwrap()[0].combining.is_empty());
        assert!(screen.row(0).unwrap()[1].combining.is_empty());
    }
    let mut screen = parse(2, 12, "👩‍💻👨‍👩‍👧‍👦AB".as_bytes());
    screen.resize(4, 3).unwrap();
    invariant(&screen);
    assert_eq!(screen.row(0).unwrap()[0].combining, ['\u{200d}', '💻']);
    assert_eq!(
        screen.row(1).unwrap()[0].combining,
        ['\u{200d}', '👩', '\u{200d}', '👧', '\u{200d}', '👦']
    );
    let sequence = format!("👩{}X", "\u{200d}💻".repeat(20));
    let screen = parse(4, 20, sequence.as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].combining.len(), 16);
    assert_ne!(
        screen.row(0).unwrap()[0].combining.last(),
        Some(&'\u{200d}')
    );
}

#[test]
fn emoji_modifiers_and_flag_pairs_share_a_cell_span() {
    let screen = parse(2, 20, "\x1b[31m👍\x1b[32m🏽X🇨🇳🇯🇵Z".as_bytes());
    assert_eq!(screen.cursor(), (0, 8));
    let cells = screen.row(0).unwrap();
    assert_eq!(cells[0].combining, ['🏽']);
    assert_eq!(cells[0].width, 2);
    assert_eq!(cells[0].style.foreground, Color::Indexed(1));
    assert_eq!(cells[2].character, 'X');
    assert_eq!(cells[2].style.foreground, Color::Indexed(2));
    assert_eq!(cells[3].character, '🇨');
    assert_eq!(cells[3].combining, ['🇳']);
    assert_eq!(cells[3].width, 2);
    assert_eq!(cells[5].combining, ['🇵']);
    assert_eq!(cells[7].character, 'Z');
}

#[test]
fn emoji_sequences_wrap_and_grow_in_insert_mode() {
    let screen = parse(2, 4, "ab👍🏽X".as_bytes());
    assert_eq!(text(&screen, 0), "ab👍🏽");
    assert_eq!(text(&screen, 1), "X   ");
    let screen = parse(2, 4, "abc🇨🇳X".as_bytes());
    assert_eq!(text(&screen, 0), "abc ");
    assert_eq!(text(&screen, 1), "🇨🇳X ");
    assert_eq!(screen.cursor(), (1, 3));
    let screen = parse(2, 8, "abcdef\r\x1b[4h🇨🇳".as_bytes());
    assert_eq!(text(&screen, 0), "🇨🇳abcdef");
    let screen = parse(2, 1, "🇨🇳".as_bytes());
    assert_eq!(text(&screen, 0), "�");
    let screen = parse(2, 4, "\x1b[?7labc🇨🇳".as_bytes());
    assert_eq!(text(&screen, 0), "abc🇨");
    assert_eq!(screen.cursor(), (0, 3));
    for modifier in ['🏻', '🏼', '🏽', '🏾', '🏿'] {
        let screen = parse(2, 8, format!("☝{modifier}X").as_bytes());
        assert_eq!(screen.cursor(), (0, 3));
        assert_eq!(screen.row(0).unwrap()[0].combining, [modifier]);
    }
}

#[test]
fn invalid_modifiers_and_unpaired_indicators_remain_separate() {
    let screen = parse(2, 20, "A🏽👍🏽🏿🇨🇳🇯".as_bytes());
    assert!(screen.row(0).unwrap()[0].combining.is_empty());
    assert_eq!(screen.row(0).unwrap()[1].character, '🏽');
    assert_eq!(screen.row(0).unwrap()[3].combining, ['🏽']);
    assert_eq!(screen.row(0).unwrap()[5].character, '🏿');
    assert_eq!(screen.row(0).unwrap()[9].character, '🇯');
    assert!(screen.row(0).unwrap()[9].combining.is_empty());
    // An unrelated zero-width suffix cannot turn two indicators into a flag.
    let screen = parse(2, 8, "🇨\u{301}🇳".as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].combining, ['\u{301}']);
    assert_eq!(screen.row(0).unwrap()[1].character, '🇳');
}

#[test]
fn emoji_overwrite_and_reflow_keep_sequences_whole() {
    for column in [1, 2] {
        let screen = parse(2, 8, format!("👍🏽🇨🇳\x1b[1;{column}HX").as_bytes());
        assert!(screen.row(0).unwrap()[0].combining.is_empty());
        assert!(screen.row(0).unwrap()[1].combining.is_empty());
    }
    let mut screen = parse(2, 8, "👍🏽🇨🇳AB".as_bytes());
    screen.resize(3, 3).unwrap();
    invariant(&screen);
    assert_eq!(screen.row(0).unwrap()[0].combining, ['🏽']);
    assert_eq!(screen.row(1).unwrap()[0].combining, ['🇳']);
}

#[test]
fn emoji_selectors_update_width_cursor_and_preserve_style() {
    let screen = parse(2, 16, "\x1b[31m⚠\x1b[32m\u{fe0f}work".as_bytes());
    assert_eq!(screen.cursor(), (0, 6));
    let cells = screen.row(0).unwrap();
    assert_eq!(cells[0].width, 2);
    assert_eq!(cells[1].width, 0);
    assert_eq!(cells[0].combining, ['\u{fe0f}']);
    assert_eq!(cells[0].style.foreground, Color::Indexed(1));
    assert_eq!(cells[1].style, cells[0].style);
    assert_eq!(cells[2].character, 'w');
    assert_eq!(cells[2].style.foreground, Color::Indexed(2));

    let screen = parse(2, 8, "☕\u{fe0e}X".as_bytes());
    assert_eq!(screen.cursor(), (0, 2));
    assert_eq!(screen.row(0).unwrap()[0].width, 1);
    assert_eq!(screen.row(0).unwrap()[1].character, 'X');
    let screen = parse(2, 8, "A\u{fe0f}中\u{fe0e}".as_bytes());
    assert_eq!(screen.cursor(), (0, 3));
    assert_eq!(screen.row(0).unwrap()[0].width, 1);
    assert_eq!(screen.row(0).unwrap()[1].width, 2);
}

#[test]
fn emoji_selector_growth_handles_margins_and_insert_mode() {
    let screen = parse(2, 4, "ab⚠\u{fe0f}X".as_bytes());
    assert_eq!(text(&screen, 0), "ab⚠\u{fe0f}");
    assert_eq!(text(&screen, 1), "X   ");
    let screen = parse(2, 4, "abc⚠\u{fe0f}X".as_bytes());
    assert_eq!(text(&screen, 0), "abc ");
    assert_eq!(text(&screen, 1), "⚠\u{fe0f}X ");
    assert_eq!(screen.cursor(), (1, 3));
    let screen = parse(2, 4, "\x1b[2;1Habc⚠\u{fe0f}X".as_bytes());
    assert_eq!(text(&screen, 0), "abc ");
    assert_eq!(text(&screen, 1), "⚠\u{fe0f}X ");
    let screen = parse(2, 4, "\x1b[?7labc⚠\u{fe0f}".as_bytes());
    assert_eq!(text(&screen, 0), "abc⚠");
    assert_eq!(screen.row(0).unwrap()[3].width, 1);
    let screen = parse(2, 1, "⚠\u{fe0f}".as_bytes());
    assert_eq!(text(&screen, 0), "�");

    let screen = parse(2, 8, "abcdef\r\x1b[4h⚠\u{fe0f}".as_bytes());
    assert_eq!(text(&screen, 0), "⚠\u{fe0f}abcdef");
    assert_eq!(screen.cursor(), (0, 2));
    let screen = parse(2, 4, "ab☕\u{fe0e}X".as_bytes());
    assert_eq!(text(&screen, 0), "ab☕\u{fe0e}X");
    assert!(screen.wrap_pending());
}

#[test]
fn utf8_scalars_and_wide_cells_preserve_styles() {
    let screen = parse(2, 8, "é\x1b[31m中😀\x1b[0mZ".as_bytes());
    assert_eq!(text(&screen, 0), "é中😀Z  ");
    assert_eq!(screen.cursor(), (0, 6));
    let cells = screen.row(0).unwrap();
    assert_eq!(cells[1].width, 2);
    assert_eq!(cells[2].width, 0);
    assert_eq!(cells[1].style.foreground, Color::Indexed(1));
    assert_eq!(cells[5].style.foreground, Color::Default);
}

#[test]
fn wide_wrap_scroll_and_single_column_policy() {
    let screen = parse(2, 4, "abc中".as_bytes());
    assert_eq!(text(&screen, 0), "abc ");
    assert_eq!(text(&screen, 1), "中  ");
    assert_eq!(screen.cursor(), (1, 2));
    let screen = parse(1, 4, "中文A".as_bytes());
    assert_eq!(text(&screen, 0), "A   ");
    let screen = parse(2, 1, "中A".as_bytes());
    assert_eq!(text(&screen, 0), "�");
    assert_eq!(text(&screen, 1), "A");
}

#[test]
fn overwriting_either_half_repairs_the_whole_glyph() {
    for (column, expected) in [(1, "X   "), (2, " X  ")] {
        let screen = parse(1, 4, format!("中\x1b[1;{column}HX").as_bytes());
        assert_eq!(text(&screen, 0), expected);
    }
    let screen = parse(1, 4, "中文\x1b[1;2H界".as_bytes());
    assert_eq!(text(&screen, 0), " 界 ");
}

#[test]
fn partial_erase_never_leaves_half_a_wide_glyph() {
    for command in ["K", "J"] {
        let screen = parse(1, 4, format!("A中B\x1b[1;3H\x1b[{command}").as_bytes());
        assert_eq!(text(&screen, 0), "A   ");
    }
    for command in ["1K", "1J"] {
        let screen = parse(1, 4, format!("A中B\x1b[1;2H\x1b[{command}").as_bytes());
        assert_eq!(text(&screen, 0), "   B");
    }
}

#[test]
fn combining_suffixes_stay_with_base_even_at_pending_wrap() {
    let screen = parse(1, 4, "\u{301}e\u{301}\x1b[31m中\u{302}".as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].combining, ['\u{301}']);
    assert_eq!(screen.row(0).unwrap()[1].combining, ['\u{302}']);
    assert_eq!(screen.cursor(), (0, 3));
    let screen = parse(1, 2, "中\x1b[31m\u{301}".as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].combining, ['\u{301}']);
    assert_eq!(screen.row(0).unwrap()[0].style.foreground, Color::Default);
    assert!(screen.wrap_pending());
    let input = format!("e{}", "\u{301}".repeat(100));
    let screen = parse(1, 4, input.as_bytes());
    assert_eq!(screen.row(0).unwrap()[0].combining.len(), 16);
}

#[test]
fn malformed_utf8_matches_lossy_replacement_and_does_not_swallow_escape() {
    for input in [
        b"\xe0\x80\x80".as_slice(),
        b"\xed\xa0\x80",
        b"\xf4\x90\x80\x80",
        b"\xe4\xb8",
        b"\xffA\xc2B",
        b"\xf0\x9fX",
    ] {
        let expected = String::from_utf8_lossy(input);
        let screen = parse(1, 12, input);
        assert_eq!(text(&screen, 0), format!("{expected:<12}"));
    }
    let screen = parse(1, 5, b"\xe4\x1b[31mX");
    assert_eq!(text(&screen, 0), "�X   ");
    assert_eq!(
        screen.row(0).unwrap()[1].style.foreground,
        Color::Indexed(1)
    );
    let screen = parse(1, 5, "A\x1b]中\x07B".as_bytes());
    assert_eq!(text(&screen, 0), "AB   ");
}

#[test]
fn finish_replaces_truncated_text_once_and_resets_sequence_state() {
    let mut screen = Screen::new(1, 8).unwrap();
    let mut parser = Parser::new();
    parser.advance(&mut screen, b"\xe4\xb8");
    assert_eq!(screen.cursor(), (0, 0));
    parser.finish(&mut screen);
    parser.finish(&mut screen);
    assert_eq!(text(&screen, 0), "�       ");
    parser.advance(&mut screen, b"\x1b[31");
    parser.finish(&mut screen);
    parser.advance(&mut screen, b"X");
    assert_eq!(text(&screen, 0), "�X      ");
    assert_eq!(screen.style().foreground, Color::Default);
}
