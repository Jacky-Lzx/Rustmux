use rustmux::{parser::Parser, screen::Screen};

fn parsed(input: &[u8]) -> Screen {
    let mut expected = Screen::new(3, 24).unwrap();
    Parser::new().advance(&mut expected, input);
    for split in 0..=input.len() {
        let mut screen = Screen::new(3, 24).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &input[..split]);
        parser.advance(&mut screen, &input[split..]);
        assert_eq!(screen, expected, "split {split}");
    }
    let mut screen = Screen::new(3, 24).unwrap();
    let mut parser = Parser::new();
    for byte in input {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}

#[test]
fn defaults_counts_and_edges_do_not_erase_or_wrap() {
    for (sequence, column) in [
        ("\t", 8),
        ("\x1b[I", 8),
        ("\x1b[0I", 8),
        ("\x1b[2I", 16),
        ("\x1b[99I", 23),
        ("\x1b[99I\x1b[Z", 16),
        ("\x1b[99I\x1b[2Z", 8),
        ("\x1b[99I\x1b[99Z", 0),
    ] {
        let screen = parsed(format!("abcdefghijklmnopqrstuvwx\x1b[H{sequence}").as_bytes());
        assert_eq!(screen.cursor(), (0, column));
        assert_eq!(
            screen
                .row(0)
                .unwrap()
                .iter()
                .map(|c| c.character)
                .collect::<String>(),
            "abcdefghijklmnopqrstuvwx"
        );
        assert!(!screen.wrap_pending());
    }
    let screen = parsed(format!("\x1b[{}I\x1b[{}Z", usize::MAX, usize::MAX).as_bytes());
    assert_eq!(screen.cursor(), (0, 0));
}

#[test]
fn custom_stops_current_clear_and_clear_all() {
    let setup = "\x1b[3g\x1b[4G\x1bH\x1b[12G\x1bH\x1b[H";
    let screen = parsed(format!("{setup}\tA\tB").as_bytes());
    assert_eq!(screen.row(0).unwrap()[3].character, 'A');
    assert_eq!(screen.row(0).unwrap()[11].character, 'B');
    let screen = parsed(format!("{setup}\x1b[4G\x1b[g\x1b[H\t").as_bytes());
    assert_eq!(screen.cursor(), (0, 11));
    let screen = parsed(format!("{setup}\x1b[3g\t").as_bytes());
    assert_eq!(screen.cursor(), (0, 23));
}

#[test]
fn setting_clearing_and_invalid_commands_preserve_pending_wrap() {
    for suffix in [
        "\x1bH",
        "\x1b[g",
        "\x1b[3g",
        "\x1b[1g",
        "\x1b[?3g",
        "\x1b[0;3g",
        "\x1b[3:0g",
    ] {
        let screen = parsed(format!("abcdefghijklmnopqrstuvwx{suffix}").as_bytes());
        assert!(screen.wrap_pending());
        assert_eq!(screen.cursor(), (0, 23));
    }
    let before = parsed(b"abcdefghijklmnopqrstuvwx");
    for suffix in [
        "\x1b[1;2I",
        "\x1b[?2Z",
        "\x1b[1:2I",
        "\x1b[999999999999999999999999Z",
    ] {
        let mut bytes = b"abcdefghijklmnopqrstuvwx".to_vec();
        bytes.extend_from_slice(suffix.as_bytes());
        assert_eq!(parsed(&bytes), before);
    }
}

#[test]
fn stops_are_global_and_resize_preserves_only_retained_columns() {
    let mut screen =
        parsed(b"\x1b[3g\x1b[4G\x1bH\x1b7\x1b[?1049h\x1b[12G\x1bH\x1b[?1049l\x1b8\x1b[H\x1b[2I");
    assert_eq!(screen.cursor(), (0, 11));
    screen.resize(4, 10).unwrap();
    screen.move_to(0, 0);
    screen.tab();
    assert_eq!(screen.cursor(), (0, 3));
    screen.resize(4, 24).unwrap();
    screen.tab();
    assert_eq!(screen.cursor(), (0, 16)); // Removed stop 11 does not return; new columns get defaults.
    let before = screen.clone();
    screen.resize(4, 24).unwrap();
    assert_eq!(screen, before);
    assert!(screen.resize(0, 24).is_err());
    assert_eq!(screen, before);
}

#[test]
fn tab_movement_respects_origin_and_leaves_wide_cells_intact() {
    let screen = parsed("\x1b[2;3r\x1b[?6h\x1b[3g\x1b[2G\x1bH\x1b[H中\x1b[H\t".as_bytes());
    assert_eq!(screen.cursor(), (1, 1));
    assert_eq!(screen.row(1).unwrap()[0].width, 2);
    assert_eq!(screen.row(1).unwrap()[1].width, 0);
    let mut one = Screen::new(1, 1).unwrap();
    Parser::new().advance(&mut one, b"X\x1bH\t\x1b[Z\x1b[3g");
    assert_eq!(one.cursor(), (0, 0));
    assert!(!one.wrap_pending());
    assert_eq!(one.row(0).unwrap()[0].character, 'X');
}
