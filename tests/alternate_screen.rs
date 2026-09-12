use rustmux::{parser::Parser, screen::Screen, style::Color};

fn text(screen: &Screen, row: usize) -> String {
    screen
        .row(row)
        .unwrap()
        .iter()
        .filter(|c| c.width != 0)
        .flat_map(|c| std::iter::once(c.character).chain(c.combining.iter().copied()))
        .collect()
}

fn parse(input: &[u8]) -> Screen {
    let mut expected = Screen::new(3, 6).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(3, 6).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected, "split {split}");
    }
    let mut screen = Screen::new(3, 6).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}

#[test]
fn entry_clears_alternate_and_keeps_coordinates_and_style() {
    let screen = parse(b"main\x1b[2;3H\x1b[31;44m\x1b[?1049h");
    assert!(screen.is_alternate());
    assert_eq!(screen.cursor(), (1, 2));
    assert_eq!(screen.style().foreground, Color::Indexed(1));
    for row in 0..3 {
        assert_eq!(text(&screen, row), "      ");
        assert!(
            screen
                .row(row)
                .unwrap()
                .iter()
                .all(|c| c.style.background == Color::Indexed(4))
        );
    }
}

#[test]
fn exit_restores_unicode_styles_cursor_and_pending_wrap() {
    let main = "\x1b[31m中文e\u{301}X";
    let expected = parse(main.as_bytes());
    assert!(expected.wrap_pending());
    let screen = parse(
        format!("{main}\x1b[?1049h\x1b[H\x1b[0mALT\r\n1\r\n2\r\n3\x1b[2J\x1b[?1049l").as_bytes(),
    );
    assert_eq!(screen, expected);
    assert!(!screen.is_alternate());
}

#[test]
fn duplicate_modes_are_idempotent_and_reentry_is_fresh() {
    let screen = parse(b"M\x1b[?1049h\x1b[Hkeep\x1b[?1049h");
    assert_eq!(text(&screen, 0), "keep  ");
    let screen = parse(b"M\x1b[?1049h\x1b[Hkeep\x1b[?1049h\x1b[?1049l\x1b[?1049l");
    assert_eq!(screen, parse(b"M"));
    let screen = parse(b"M\x1b[?1049hkeep\x1b[?1049l\x1b[?1049h");
    assert!(screen.is_alternate());
    assert_eq!(screen.cursor(), (0, 1));
    assert_eq!(text(&screen, 0), "      ");
}

#[test]
fn only_supported_well_formed_private_modes_switch_screens() {
    for sequence in [
        "\x1b[1049h",
        "\x1b[?1049m",
        "\x1b[?47h",
        "\x1b[??1049h",
        "\x1b[1?1049h",
        "\x1b[?1049:h",
        "\x1b[?1049 h",
        "\x1b[?999999999999999999999999999999h",
    ] {
        assert_eq!(parse(format!("M{sequence}").as_bytes()), parse(b"M"));
    }
    let screen = parse(b"M\x1b[?25;1049h\x1b[Halt\x1b[?1049;25l");
    assert_eq!(screen, parse(b"M"));
}

#[test]
fn switching_after_a_split_wide_character_preserves_main_cells() {
    let screen = parse("中\x1b[?1049h\x1b[H界\u{301}\x1b[?1049lX".as_bytes());
    assert_eq!(text(&screen, 0), "中X   ");
    assert_eq!(screen.row(0).unwrap()[0].width, 2);
    assert_eq!(screen.row(0).unwrap()[1].width, 0);
    assert!(screen.row(0).unwrap()[0].combining.is_empty());
}
