use rustmux::{
    parser::{MAX_REPLY_BYTES, Parser},
    screen::Screen,
};

fn run(screen: &mut Screen, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    Parser::new().advance_with_replies(screen, input, &mut |reply| output.extend_from_slice(reply));
    output
}
fn osc(values: &str) -> Vec<u8> {
    format!("\x1b]21;{values}\x1b\\").into_bytes()
}
fn reply(values: &str) -> Vec<u8> {
    osc(values)
}

#[test]
fn mixed_operations_are_ordered_at_every_split_and_terminator() {
    for terminator in ["\x07", "\x1b\\"] {
        let input = format!("abc\x1b]21;foreground=#123456;foreground=?;1=#aabbcc;1=?{terminator}");
        let expected =
            format!("\x1b]21;foreground=rgb:12/34/56;1=rgb:aa/bb/cc{terminator}").into_bytes();
        for split in 0..=input.len() {
            let mut screen = Screen::new(8, 20).unwrap();
            let mut parser = Parser::new();
            let mut output = Vec::new();
            for chunk in [&input.as_bytes()[..split], &input.as_bytes()[split..]] {
                let before = output.len();
                parser
                    .advance_with_replies(&mut screen, chunk, &mut |r| output.extend_from_slice(r));
                assert!(output.len() - before <= chunk.len() * MAX_REPLY_BYTES);
            }
            assert_eq!(output, expected);
            assert_eq!(screen.cursor(), (0, 3));
        }
    }
}

#[test]
fn all_palette_entries_share_state_with_legacy_commands() {
    let mut screen = Screen::new(2, 4).unwrap();
    for index in 0..256 {
        let request = format!("{index}=#123456;{index}=?");
        assert_eq!(
            run(&mut screen, &osc(&request)),
            reply(&format!("{index}=rgb:12/34/56"))
        );
        assert_eq!(
            run(&mut screen, format!("\x1b]4;{index};?\x07").as_bytes()),
            format!("\x1b]4;{index};rgb:1212/3434/5656\x07").as_bytes()
        );
        assert!(run(&mut screen, &osc(&index.to_string())).is_empty());
    }
}

#[test]
fn queries_do_not_modify_screen_and_unknown_names_are_encoded() {
    let mut screen = Screen::new(3, 8).unwrap();
    run(&mut screen, b"abc\x1b[31m\x1b[?7l");
    let before = screen.clone();
    let unknown = "selection_background=?;nonsense;cursor_text=#123456";
    assert_eq!(
        run(&mut screen, &osc(unknown)),
        reply("unknown=c2VsZWN0aW9uX2JhY2tncm91bmQ;unknown=bm9uc2Vuc2U;unknown=Y3Vyc29yX3RleHQ")
    );
    run(
        &mut screen,
        &osc("foreground=?;background=?;cursor=?;255=?"),
    );
    assert_eq!(screen, before);
}

#[test]
fn resets_resume_inheritance_and_stack_restores_structured_setters() {
    let mut screen = Screen::new(3, 8).unwrap();
    let original = run(&mut screen, &osc("foreground=?;background=?;cursor=?;1=?"));
    run(&mut screen, b"\x1b]30001\x07");
    for field in ["foreground", "background", "cursor", "1"] {
        run(&mut screen, &osc(&format!("{field}=rgb:f/00/123")));
    }
    assert_eq!(
        run(&mut screen, &osc("cursor=?")),
        reply("cursor=rgb:ff/00/12")
    );
    screen.resize(7, 16).unwrap();
    run(&mut screen, b"\x1b[?1049h\x1b[!p\x1b]30101\x07");
    assert_eq!(
        run(&mut screen, &osc("foreground=?;background=?;cursor=?;1=?")),
        original
    );
    for field in ["foreground", "background", "cursor", "1"] {
        run(&mut screen, &osc(&format!("{field}=#123456")));
        run(&mut screen, &osc(field));
    }
    assert_eq!(
        run(&mut screen, &osc("foreground=?;background=?;cursor=?;1=?")),
        original
    );
}

#[test]
fn malformed_supported_fields_are_atomic_no_ops() {
    for field in [
        "",
        "foreground=",
        "foreground=red",
        "1=bad",
        "cursor=#12",
        "background=#éabc",
        "foreground=?=x",
        "foreground=#ffffff;",
        "=x",
        "foreground=rgb:1/2/3/4",
        "cursor=\x7f",
    ] {
        let mut screen = Screen::new(2, 4).unwrap();
        let before = screen.clone();
        let input = osc(&format!("1=#123456;1=?;{field}"));
        assert!(run(&mut screen, &input).is_empty(), "{field}");
        assert_eq!(screen, before, "{field}");
    }
}

#[test]
fn cancellation_unfinished_and_overflow_never_apply_partial_state() {
    for ending in [b"\x18".as_slice(), b"\x1a", b"\x1bX\x07", b"", b"\x1b"] {
        let mut screen = Screen::new(2, 4).unwrap();
        let before = screen.clone();
        let mut input = b"\x1b]21;foreground=#123456".to_vec();
        input.extend_from_slice(ending);
        assert!(run(&mut screen, &input).is_empty());
        assert_eq!(screen, before);
    }
    let mut screen = Screen::new(2, 4).unwrap();
    let before = screen.clone();
    let input = format!("\x1b]21;foreground=#123456;{}\x07", "z".repeat(1024 * 1024));
    assert!(run(&mut screen, input.as_bytes()).is_empty());
    assert_eq!(screen, before);
    assert_eq!(run(&mut screen, b"\x1b[5n"), b"\x1b[0n");
}

#[test]
fn maximum_expansion_fits_existing_per_byte_reply_reservation() {
    assert_eq!(MAX_REPLY_BYTES, 420);
    // 64-byte payload: 31 unsupported one-byte fields, with no query values.
    let input = osc(&vec!["x"; 31].join(";"));
    let mut screen = Screen::new(2, 4).unwrap();
    let mut parser = Parser::new();
    let mut count = 0;
    for byte in input {
        let mut emitted = 0;
        parser.advance_with_replies(&mut screen, &[byte], &mut |r| {
            emitted += r.len();
            count += r.len();
        });
        assert!(emitted <= MAX_REPLY_BYTES);
    }
    assert_eq!(count, 4 + 31 * 11 + 2);
}

#[test]
fn structured_setters_recolor_existing_cells_only_in_their_pane() {
    use rustmux::{
        layout::{Layout, SplitAxis},
        pane_view::compose,
        render::Renderer,
    };
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
        run(&mut screen, b"\x1b[31mP\x1b[0mD");
        screens.push(screen);
    }
    let compose_frame = |screens: &[Screen]| {
        compose(&layout, &[(left, &screens[0]), (right, &screens[1])]).unwrap()
    };
    let mut renderer = Renderer::default();
    renderer
        .render(&compose_frame(&screens), &mut Vec::new())
        .unwrap();
    let cells = screens[0].row(0).unwrap().to_vec();
    let neighbor = screens[1].clone();
    run(
        &mut screens[0],
        &osc("foreground=#010203;background=#040506;1=#070809"),
    );
    assert_eq!(screens[0].row(0).unwrap(), cells);
    assert_eq!(screens[1], neighbor);
    let mut output = Vec::new();
    renderer
        .render(&compose_frame(&screens), &mut output)
        .unwrap();
    let text = String::from_utf8(output).unwrap();
    for sgr in ["38;2;7;8;9", "38;2;1;2;3", "48;2;4;5;6"] {
        assert!(text.contains(sgr), "{text:?}");
    }
    assert!(!text.contains("\x1b]21;"));
}
