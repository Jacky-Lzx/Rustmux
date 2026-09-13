use rustmux::{
    parser::Parser,
    screen::Screen,
    style::{Color, Style},
};

const GRID: &str = "\x1b[1;1HAAAA\x1b[2;1HBBBB\x1b[3;1HCCCC\x1b[4;1HDDDD\x1b[5;1HEEEE\x1b[2;4r";

fn parsed(input: &str) -> Screen {
    let bytes = input.as_bytes();
    let mut expected = Screen::new(5, 4).unwrap();
    Parser::new().advance(&mut expected, bytes);
    for split in 0..=bytes.len() {
        let mut screen = Screen::new(5, 4).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, &bytes[..split]);
        parser.advance(&mut screen, &bytes[split..]);
        assert_eq!(screen, expected, "split {split}");
    }
    let mut screen = Screen::new(5, 4).unwrap();
    let mut parser = Parser::new();
    for byte in bytes {
        parser.advance(&mut screen, &[*byte]);
    }
    assert_eq!(screen, expected);
    expected
}

fn rows(screen: &Screen) -> Vec<String> {
    (0..screen.dimensions().0)
        .map(|r| screen.row(r).unwrap().iter().map(|c| c.character).collect())
        .collect()
}

#[test]
fn insert_delete_start_at_cursor_and_preserve_surrounding_rows() {
    for (command, expected) in [
        ("L", ["AAAA", "BBBB", "    ", "CCCC", "EEEE"]),
        ("M", ["AAAA", "BBBB", "DDDD", "    ", "EEEE"]),
    ] {
        for count in ["", "0", "1"] {
            let screen = parsed(&format!("{GRID}\x1b[3;3H\x1b[{count}{command}"));
            assert_eq!(rows(&screen), expected);
            assert_eq!(screen.cursor(), (2, 0));
            assert!(!screen.wrap_pending());
        }
    }
}

#[test]
fn explicit_scroll_uses_whole_region_even_with_cursor_outside() {
    for (command, expected) in [
        ("S", ["AAAA", "CCCC", "DDDD", "    ", "EEEE"]),
        ("T", ["AAAA", "    ", "BBBB", "CCCC", "EEEE"]),
    ] {
        for count in ["", "0", "1"] {
            let screen = parsed(&format!("{GRID}\x1b[5;3H\x1b[{count}{command}"));
            assert_eq!(rows(&screen), expected);
            assert_eq!(screen.cursor(), (4, 2));
        }
    }
}

#[test]
fn counts_clamp_and_blanks_use_only_writing_background() {
    for command in ["L", "M", "S", "T"] {
        let screen = parsed(&format!(
            "{GRID}\x1b[2;4H\x1b[1;31;44m\x1b[{}{command}Z",
            usize::MAX
        ));
        assert_eq!(screen.row(0).unwrap()[0].character, 'A');
        assert_eq!(screen.row(4).unwrap()[0].character, 'E');
        for row in 2..=3 {
            for cell in screen.row(row).unwrap() {
                assert_eq!(cell.character, ' ');
                assert_eq!(
                    cell.style,
                    Style {
                        background: Color::Indexed(4),
                        ..Style::default()
                    }
                );
            }
        }
        let column = if matches!(command, "L" | "M") { 0 } else { 3 };
        assert_eq!(screen.row(1).unwrap()[column].character, 'Z');
        assert!(screen.row(1).unwrap()[column].style.bold);
    }
    for command in ["2L", "2M"] {
        let screen = parsed(&format!("{GRID}\x1b[3;1H\x1b[{command}"));
        assert_eq!(rows(&screen), ["AAAA", "BBBB", "    ", "    ", "EEEE"]);
    }
}

#[test]
fn outside_region_and_malformed_commands_do_not_mutate_state() {
    for row in [1, 5] {
        let prefix = format!("{GRID}\x1b[{row};4HX");
        let before = parsed(&prefix);
        for command in ["L", "M"] {
            assert_eq!(parsed(&format!("{prefix}\x1b[{command}")), before);
        }
    }
    let prefix = format!("{GRID}\x1b[3;4HX");
    let before = parsed(&prefix);
    for command in ["L", "M", "S", "T"] {
        for params in ["?1", "1;2", "1:2", "99999999999999999999999999"] {
            assert_eq!(parsed(&format!("{prefix}\x1b[{params}{command}")), before);
        }
    }
    let mut zero = before.clone();
    zero.insert_lines(0);
    zero.delete_lines(0);
    zero.scroll_up(0);
    zero.scroll_down(0);
    assert_eq!(zero, before);
}

#[test]
fn line_edits_move_unicode_styles_and_do_not_touch_main_grid() {
    let screen = parsed(&format!(
        "{GRID}\x1b[3;1H\x1b[38:2:255:0:0m中e\u{301}\x1b[2;1H\x1b[M"
    ));
    let row = screen.row(1).unwrap();
    assert_eq!((row[0].character, row[0].width, row[1].width), ('中', 2, 0));
    assert_eq!(row[2].combining, vec!['\u{301}']);
    assert_eq!(row[0].style.foreground, Color::Rgb(255, 0, 0));
    let before = parsed(GRID);
    let after = parsed(&format!(
        "{GRID}\x1b[?1049hhello\x1b[2;4r\x1b[3;2H\x1b[L\x1b[M\x1b[S\x1b[T\x1b[?1049l"
    ));
    assert_eq!(after, before);
}

#[test]
fn single_cell_and_pending_wrap_remain_valid() {
    for command in ["L", "M", "S", "T"] {
        let screen = parsed(&format!("{GRID}\x1b[3;4HX\x1b[{command}Y"));
        assert_eq!(
            screen.row(2).unwrap()[if matches!(command, "L" | "M") { 0 } else { 3 }].character,
            'Y'
        );
        let mut one = Screen::new(1, 1).unwrap();
        Parser::new().advance(&mut one, format!("X\x1b[{command}").as_bytes());
        assert_eq!(rows(&one), [" "]);
        assert_eq!(one.cursor(), (0, 0));
        assert!(!one.wrap_pending());
    }
}
