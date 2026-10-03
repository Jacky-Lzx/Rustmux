use rustmux::{
    layout::{Layout, SplitAxis},
    pane_view::compose,
    parser::{MAX_REPLY_BYTES, Parser},
    render::Renderer,
    screen::Screen,
};

fn parsed(input: &[u8]) -> (Screen, Vec<u8>) {
    let run = |split| {
        let mut screen = Screen::new(4, 16).unwrap();
        let mut parser = Parser::new();
        let mut output = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            parser.advance_with_replies(&mut screen, chunk, &mut |reply| {
                assert!(reply.len() <= MAX_REPLY_BYTES);
                output.extend_from_slice(reply);
            });
        }
        (screen, output)
    };
    let expected = run(input.len());
    for split in 0..=input.len() {
        assert_eq!(run(split), expected, "split at {split}");
    }
    expected
}

#[test]
fn fragmented_osc_set_push_pop_query_and_bel_termination_are_stateful() {
    let (screen, output) = parsed(b"TEXT\x1b]22;pointer\x07\x1b]22;>wait,crosshair\x1b\\\x1b]22;=text\x1b\\\x1b]22;<ignored\x1b\\\x1b]22;?__current__,pointer,unknown\x07");
    assert_eq!(screen.pointer_shape(), Some("wait"));
    assert_eq!(output, b"\x1b]22;wait,1,0\x1b\\");
    assert_eq!(screen.cursor(), (0, 4));
    assert_eq!(screen.row(0).unwrap()[0].character, 'T');
    let (screen, output) = parsed(b"\x1b]22;?__current__,__default__,__grabbed__\x1b\\");
    assert_eq!(screen.pointer_shape(), None);
    assert_eq!(output, b"\x1b]22;0,default,default\x1b\\");
}

#[test]
fn malformed_and_overlong_sequences_are_consumed_without_mutating_or_replying() {
    for bad in [
        b"\x1b]22;bogus\x1b\\".to_vec(),
        b"\x1b]22;pointer,wait\x1b\\".to_vec(),
        b"\x1b]22;=Pointer\x1b\\".to_vec(),
        b"\x1b]22;\xff\x1b\\".to_vec(),
        [b"\x1b]22;>".as_slice(), &vec![b'x'; 100_000], b"\x1b\\"].concat(),
        [b"\x1b]22;?".as_slice(), &vec![b','; 100_000], b"\x1b\\"].concat(),
    ] {
        let mut screen = Screen::new(4, 16).unwrap();
        let mut parser = Parser::new();
        parser.advance(&mut screen, b"\x1b]22;pointer\x1b\\");
        let before = screen.clone();
        let mut replies = Vec::new();
        parser.advance_with_replies(&mut screen, &bad, &mut |reply| {
            replies.extend_from_slice(reply)
        });
        assert!(replies.is_empty());
        assert_eq!(screen, before);
        parser.advance(&mut screen, b"OK");
        assert_eq!(screen.row(0).unwrap()[0].character, 'O');
    }
    // A maximum-size support query remains inside the existing per-byte bound.
    let (_, replies) = parsed(&[b"\x1b]22;?".as_slice(), &[b','; 60], b"\x1b\\"].concat());
    assert_eq!(replies.iter().filter(|b| **b == b'0').count(), 61);
}

#[test]
fn main_and_alternate_stacks_survive_resize_and_saved_cursor_but_reset_together() {
    for mode in [47, 1047, 1049] {
        let (mut screen, _) = parsed(
            format!("\x1b]22;>pointer,wait\x1b\\\x1b7\x1b[?{mode}h\x1b]22;>crosshair,text\x1b\\")
                .as_bytes(),
        );
        screen.resize(6, 20).unwrap();
        assert_eq!(screen.pointer_shape(), Some("text"));
        let mut parser = Parser::new();
        parser.advance(
            &mut screen,
            format!("\x1b[?{mode}l\x1b8\x1b]22;<\x1b\\").as_bytes(),
        );
        assert_eq!(screen.pointer_shape(), Some("pointer"));
        parser.advance(
            &mut screen,
            format!("\x1b[?{mode}h\x1b]22;<\x1b\\").as_bytes(),
        );
        assert_eq!(screen.pointer_shape(), Some("crosshair"));
        for reset in [b"\x1b[!p".as_slice(), b"\x1bc"] {
            let mut reset_screen = screen.clone();
            parser.advance(&mut reset_screen, reset);
            assert_eq!(reset_screen.pointer_shape(), None);
            parser.advance(&mut reset_screen, b"\x1b[?1049l\x1b]22;<\x1b\\");
            assert_eq!(reset_screen.pointer_shape(), None);
            parser.advance(&mut reset_screen, b"\x1b[?1049h\x1b]22;<\x1b\\");
            assert_eq!(reset_screen.pointer_shape(), None);
        }
    }
}

#[test]
fn selected_pane_owns_rendered_shape_and_renderer_emits_sets_only_on_changes() {
    let mut layout = Layout::new(6, 40).unwrap();
    let left = layout.active();
    let right = layout.split_active(SplitAxis::Columns).unwrap();
    let screens: Vec<_> = layout
        .content_geometry()
        .panes
        .iter()
        .map(|(id, rect)| {
            let mut screen = Screen::new(rect.rows.into(), rect.columns.into()).unwrap();
            let shape = if *id == left { "pointer" } else { "wait" };
            Parser::new().advance(&mut screen, format!("\x1b]22;>{shape}\x1b\\").as_bytes());
            (*id, screen)
        })
        .collect();
    let originals = screens.clone();
    let references: Vec<_> = screens.iter().map(|(id, screen)| (*id, screen)).collect();
    let mut renderer = Renderer::default();
    let mut output = Vec::new();
    for (id, shape) in [(right, "wait"), (left, "pointer"), (right, "wait")] {
        layout.select(id).unwrap();
        let view = compose(&layout, &references).unwrap();
        assert_eq!(view.pointer_shape(), Some(shape));
        output.clear();
        renderer.render(&view, &mut output).unwrap();
        assert!(
            output
                .windows(shape.len() + 7)
                .any(|bytes| bytes == format!("\x1b]22;{shape}\x1b\\").as_bytes())
        );
        output.clear();
        renderer.render(&view, &mut output).unwrap();
        assert!(!output.windows(5).any(|bytes| bytes == b"\x1b]22;"));
    }
    assert_eq!(screens, originals);
    output.clear();
    renderer
        .render(&Screen::new(6, 40).unwrap(), &mut output)
        .unwrap();
    assert!(output.windows(7).any(|bytes| bytes == b"\x1b]22;\x1b\\"));
    assert!(!output.windows(6).any(|bytes| bytes == b"\x1b]22;>"));
}
