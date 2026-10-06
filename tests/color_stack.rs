use rustmux::{
    layout::{Layout, SplitAxis},
    pane_view::compose,
    parser::Parser,
    render::Renderer,
    screen::Screen,
    style::Color,
};

const PUSH: &[u8] = b"\x1b]30001\x1b\\";
const POP: &[u8] = b"\x1b]30101\x1b\\";
fn advance(screen: &mut Screen, input: &[u8]) {
    Parser::new().advance(screen, input);
}
fn replies(screen: &mut Screen, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    Parser::new().advance_with_replies(screen, input, &mut |reply| output.extend_from_slice(reply));
    output
}
fn foreground(screen: &mut Screen) -> Vec<u8> {
    replies(screen, b"\x1b]10;?\x1b\\")
}
fn rgb_reply(code: u8, color: (u8, u8, u8)) -> String {
    format!(
        "\x1b]{code};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
        u16::from(color.0) * 257,
        u16::from(color.1) * 257,
        u16::from(color.2) * 257
    )
}
fn set_fg(screen: &mut Screen, color: u8) {
    advance(screen, format!("\x1b]10;#{color:02x}0000\x1b\\").as_bytes());
}
fn queries() -> Vec<u8> {
    let mut bytes = b"\x1b]10;?;?;?\x1b\\".to_vec();
    for index in 0..256 {
        bytes.extend_from_slice(format!("\x1b]4;{index};?\x1b\\").as_bytes());
    }
    bytes
}
fn palette_color(seed: u8, index: u8) -> (u8, u8, u8) {
    (
        seed.wrapping_add(index),
        index.wrapping_mul(3),
        seed.wrapping_mul(7),
    )
}
fn set_profile(screen: &mut Screen, seed: u8) {
    advance(
        screen,
        format!("\x1b]10;#{seed:02x}0001;#00{seed:02x}02;#0003{seed:02x}\x1b\\").as_bytes(),
    );
    for index in 0..256 {
        let (r, g, b) = palette_color(seed, index as u8);
        advance(
            screen,
            format!("\x1b]4;{index};#{r:02x}{g:02x}{b:02x}\x1b\\").as_bytes(),
        );
    }
}
fn expected_profile(seed: u8) -> Vec<u8> {
    let mut text =
        rgb_reply(10, (seed, 0, 1)) + &rgb_reply(11, (0, seed, 2)) + &rgb_reply(12, (0, 3, seed));
    for index in 0..256 {
        let (r, g, b) = palette_color(seed, index as u8);
        text.push_str(&format!(
            "\x1b]4;{index};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
            u16::from(r) * 257,
            u16::from(g) * 257,
            u16::from(b) * 257
        ));
    }
    text.into_bytes()
}

#[test]
fn nested_stack_restores_every_supported_color_without_replies_or_text_changes() {
    let mut screen = Screen::new(2, 8).unwrap();
    advance(&mut screen, b"\x1b[31mTEXT\x1b[2;3H");
    let cells = screen.row(0).unwrap().to_vec();
    let cursor = screen.cursor();
    set_profile(&mut screen, 11);
    assert!(replies(&mut screen, PUSH).is_empty());
    set_profile(&mut screen, 22);
    advance(&mut screen, PUSH);
    set_profile(&mut screen, 33);
    assert!(replies(&mut screen, POP).is_empty());
    assert_eq!(replies(&mut screen, &queries()), expected_profile(22));
    advance(&mut screen, POP);
    assert_eq!(replies(&mut screen, &queries()), expected_profile(11));
    assert_eq!(screen.row(0).unwrap(), cells);
    assert_eq!(screen.cursor(), cursor);
    assert_eq!(screen.style().foreground, Color::Indexed(1));
}

#[test]
fn every_chunk_boundary_and_both_terminators_preserve_stack_operations() {
    for terminator in ["\x1b\\", "\x07"] {
        let input = format!(
            "\x1b]10;#112233{terminator}\x1b]30001{terminator}\x1b]10;#aabbcc{terminator}\x1b]30101{terminator}\x1b]10;?{terminator}X"
        );
        let expected = format!("\x1b]10;rgb:1111/2222/3333{terminator}").into_bytes();
        for split in 0..=input.len() {
            let mut screen = Screen::new(1, 4).unwrap();
            let mut parser = Parser::new();
            let mut output = Vec::new();
            parser.advance_with_replies(&mut screen, &input.as_bytes()[..split], &mut |r| {
                output.extend_from_slice(r)
            });
            parser.advance_with_replies(&mut screen, &input.as_bytes()[split..], &mut |r| {
                output.extend_from_slice(r)
            });
            assert_eq!(output, expected);
            assert_eq!(screen.row(0).unwrap()[0].character, 'X');
        }
    }
}

#[test]
fn overflow_retains_the_newest_32_profiles_and_underflow_is_a_noop() {
    let mut screen = Screen::new(1, 4).unwrap();
    let initial = screen.clone();
    advance(&mut screen, POP);
    assert_eq!(screen, initial);
    for value in 0..35 {
        set_fg(&mut screen, value);
        advance(&mut screen, PUSH);
    }
    set_fg(&mut screen, 255);
    for value in (3..35).rev() {
        advance(&mut screen, POP);
        assert_eq!(
            foreground(&mut screen),
            rgb_reply(10, (value, 0, 0)).as_bytes()
        );
    }
    let empty = screen.clone();
    for _ in 0..100 {
        advance(&mut screen, POP);
    }
    assert_eq!(screen, empty);
}

#[test]
fn malformed_cancelled_and_unfinished_commands_do_not_create_stack_entries() {
    let mut cases: Vec<Vec<u8>> = vec![
        b"\x1b]30001;\x1b\\".to_vec(),
        b"\x1b]30001;1\x07".to_vec(),
        b"\x1b]030001\x1b\\".to_vec(),
        b"\x1b]30001\x18".to_vec(),
        b"\x1b]30001\x1a".to_vec(),
        b"\x1b]30001\x1bX\x1b\\".to_vec(),
        b"\x1b]30001".to_vec(),
    ];
    cases.push(format!("\x1b]30001{}\x1b\\", "0".repeat(10000)).into_bytes());
    for input in cases {
        let mut screen = Screen::new(1, 4).unwrap();
        let mut parser = Parser::new();
        set_fg(&mut screen, 1);
        parser.advance(&mut screen, &input);
        parser.finish(&mut screen);
        set_fg(&mut screen, 2);
        advance(&mut screen, POP);
        assert_eq!(foreground(&mut screen), rgb_reply(10, (2, 0, 0)).as_bytes());
    }
}

#[test]
fn malformed_pop_does_not_consume_a_valid_snapshot() {
    for malformed in [
        b"\x1b]30101;1\x07".as_slice(),
        b"\x1b]30101\x18",
        b"\x1b]30101\x1bX\x1b\\",
    ] {
        let mut screen = Screen::new(1, 4).unwrap();
        set_fg(&mut screen, 1);
        advance(&mut screen, PUSH);
        set_fg(&mut screen, 2);
        advance(&mut screen, malformed);
        assert_eq!(foreground(&mut screen), rgb_reply(10, (2, 0, 0)).as_bytes());
        advance(&mut screen, POP);
        assert_eq!(foreground(&mut screen), rgb_reply(10, (1, 0, 0)).as_bytes());
    }
}

#[test]
fn main_and_alternate_buffers_share_one_pane_color_stack() {
    let mut screen = Screen::new(2, 8).unwrap();
    set_profile(&mut screen, 1);
    advance(&mut screen, PUSH);
    advance(&mut screen, b"\x1b[?1049h");
    set_profile(&mut screen, 2);
    advance(&mut screen, PUSH);
    advance(&mut screen, b"\x1b[?1049l");
    set_profile(&mut screen, 3);
    advance(&mut screen, POP);
    assert_eq!(replies(&mut screen, &queries()), expected_profile(2));
    advance(&mut screen, POP);
    assert_eq!(replies(&mut screen, &queries()), expected_profile(1));
}

#[test]
fn soft_reset_retains_stack_and_hard_reset_discards_saved_profiles() {
    let mut screen = Screen::new(2, 8).unwrap();
    set_fg(&mut screen, 1);
    advance(&mut screen, PUSH);
    set_fg(&mut screen, 2);
    advance(&mut screen, b"\x1b[!p");
    advance(&mut screen, POP);
    assert_eq!(foreground(&mut screen), rgb_reply(10, (1, 0, 0)).as_bytes());
    advance(&mut screen, PUSH);
    set_fg(&mut screen, 3);
    advance(&mut screen, b"\x1bc");
    advance(&mut screen, POP);
    assert_eq!(foreground(&mut screen), rgb_reply(10, (3, 0, 0)).as_bytes());
}

#[test]
fn sgr_and_cursor_restore_do_not_pop_or_replace_color_stack() {
    let mut screen = Screen::new(2, 8).unwrap();
    set_fg(&mut screen, 1);
    advance(&mut screen, b"\x1b7");
    advance(&mut screen, PUSH);
    set_fg(&mut screen, 2);
    advance(&mut screen, b"\x1b[0m\x1b8");
    assert_eq!(foreground(&mut screen), rgb_reply(10, (2, 0, 0)).as_bytes());
    advance(&mut screen, POP);
    assert_eq!(foreground(&mut screen), rgb_reply(10, (1, 0, 0)).as_bytes());
}

#[test]
fn width_reflow_height_resize_and_failed_resize_preserve_pending_profiles() {
    for dimensions in [(4, 8), (2, 5), (3, 12), (0, 8)] {
        let mut screen = Screen::new(2, 8).unwrap();
        set_profile(&mut screen, 1);
        advance(&mut screen, PUSH);
        set_profile(&mut screen, 2);
        advance(&mut screen, b"abcdefghijklmn");
        let before = screen.clone();
        let result = screen.resize(dimensions.0, dimensions.1);
        if dimensions.0 == 0 {
            assert!(result.is_err());
            assert_eq!(screen, before);
        } else {
            result.unwrap();
        }
        advance(&mut screen, POP);
        assert_eq!(replies(&mut screen, &queries()), expected_profile(1));
    }
}

#[test]
fn cloned_render_snapshots_do_not_consume_the_live_stack() {
    let mut screen = Screen::new(2, 8).unwrap();
    set_fg(&mut screen, 1);
    advance(&mut screen, PUSH);
    set_fg(&mut screen, 2);
    let mut snapshot = screen.clone();
    advance(&mut snapshot, POP);
    assert_eq!(
        foreground(&mut snapshot),
        rgb_reply(10, (1, 0, 0)).as_bytes()
    );
    assert_eq!(foreground(&mut screen), rgb_reply(10, (2, 0, 0)).as_bytes());
    advance(&mut screen, POP);
    assert_eq!(foreground(&mut screen), rgb_reply(10, (1, 0, 0)).as_bytes());
}

#[test]
fn pane_color_stacks_are_isolated_and_pop_repaints_existing_cells() {
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
        set_profile(&mut screen, 1);
        advance(&mut screen, b"\x1b[31mP\x1b[0mD");
        advance(&mut screen, PUSH);
        set_profile(&mut screen, 2);
        screens.push(screen);
    }
    let compose_frame = |screens: &[Screen]| {
        compose(&layout, &[(left, &screens[0]), (right, &screens[1])]).unwrap()
    };
    let before = compose_frame(&screens);
    let mut renderer = Renderer::default();
    renderer.render(&before, &mut Vec::new()).unwrap();
    let cells = screens[0].row(0).unwrap().to_vec();
    advance(&mut screens[0], POP);
    assert_eq!(screens[0].row(0).unwrap(), cells);
    assert_eq!(replies(&mut screens[1], &queries()), expected_profile(2));
    let after = compose_frame(&screens);
    let mut output = Vec::new();
    renderer.render(&after, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("38;2;2;3;7"), "{text:?}"); // Palette 1 of seed 1.
    assert!(text.contains("38;2;1;0;1"), "{text:?}"); // Restored default foreground.
    assert!(!text.contains("\x1b]30001"));
    assert!(!text.contains("\x1b]30101"));
}
