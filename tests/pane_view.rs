use rustmux::{
    layout::{Layout, SplitAxis},
    pane_view::compose,
    parser::Parser,
    render::Renderer,
    screen::Screen,
};

#[test]
fn composition_preserves_cells_maps_cursor_and_replays_through_renderer() {
    let mut layout = Layout::new(7, 13).unwrap();
    let left = layout.active();
    let top = layout.split_active(SplitAxis::Columns).unwrap();
    let bottom = layout.split_active(SplitAxis::Rows).unwrap();
    let mut a = Screen::new(5, 4).unwrap();
    let mut b = Screen::new(1, 5).unwrap();
    let mut c = Screen::new(2, 5).unwrap();
    Parser::new().advance(&mut a, "\x1b[31m中e\u{301}".as_bytes());
    Parser::new().advance(&mut b, b"TOP\x1b[?25l");
    Parser::new().advance(
        &mut c,
        b"\x1b[38;2;12;34;56mBOT\x1b[?2004h\x1b[?1h\x1b[2;2H",
    );
    let originals = (a.clone(), b.clone(), c.clone());
    let frame = compose(&layout, &[(bottom, &c), (left, &a), (top, &b)]).unwrap();
    for row in 0..5 {
        assert_eq!(&frame.row(row + 1).unwrap()[1..5], a.row(row).unwrap());
    }
    assert_eq!(&frame.row(4).unwrap()[7..12], c.row(0).unwrap());
    assert_eq!(frame.row(2).unwrap()[5].character, '│');
    assert_eq!(frame.row(2).unwrap()[6].character, '└');
    assert_eq!(frame.row(3).unwrap()[6].character, '┌');
    assert_eq!(frame.cursor(), (5, 8));
    assert!(frame.cursor_visible());
    assert!(frame.bracketed_paste());
    assert!(frame.application_cursor_keys());
    assert_eq!((a.clone(), b.clone(), c.clone()), originals);
    let mut renderer = Renderer::default();
    let mut outer = Screen::new(7, 13).unwrap();
    let mut parser = Parser::new();
    for view in [frame, {
        layout.select(left).unwrap();
        compose(&layout, &[(left, &a), (top, &b), (bottom, &c)]).unwrap()
    }] {
        let mut bytes = Vec::new();
        renderer.render(&view, &mut bytes).unwrap();
        parser.advance(&mut outer, &bytes);
        for row in 0..7 {
            assert_eq!(outer.row(row), view.row(row));
        }
        assert_eq!(outer.cursor(), view.cursor());
        assert_eq!(outer.bracketed_paste(), view.bracketed_paste());
    }
}

#[test]
fn rejects_missing_duplicate_unknown_and_wrong_size_screens_without_mutation() {
    let mut layout = Layout::new(5, 9).unwrap();
    let first = layout.active();
    let second = layout.split_active(SplitAxis::Columns).unwrap();
    let a = Screen::new(3, 2).unwrap();
    let wrong = Screen::new(3, 5).unwrap();
    assert!(compose(&layout, &[(first, &a)]).is_err());
    assert!(compose(&layout, &[(first, &a), (first, &a), (second, &a)]).is_err());
    assert!(compose(&layout, &[(first, &a), (second, &wrong)]).is_err());
    layout.close(second).unwrap();
    assert!(compose(&layout, &[(first, &wrong), (second, &a)]).is_err());
    let huge = Layout::new(257, 256).unwrap();
    assert!(compose(&huge, &[]).is_err());
    assert_eq!(a.dimensions(), (3, 2));
}

#[test]
fn zoom_needs_only_the_visible_full_size_screen() {
    let mut layout = Layout::new(5, 9).unwrap();
    let first = layout.active();
    let second = layout.split_active(SplitAxis::Columns).unwrap();
    layout.toggle_zoom();
    let mut full = Screen::new(3, 7).unwrap();
    Parser::new().advance(&mut full, b"ZOOM\x1b[2;3H");
    let frame = compose(&layout, &[(second, &full)]).unwrap();
    for row in 0..3 {
        assert_eq!(&frame.row(row + 1).unwrap()[1..8], full.row(row).unwrap());
    }
    assert_eq!(frame.cursor(), (full.cursor().0 + 1, full.cursor().1 + 1));
    let hidden = Screen::new(3, 3).unwrap();
    assert!(compose(&layout, &[(first, &hidden), (second, &full)]).is_ok());
    layout.toggle_zoom();
    assert!(compose(&layout, &[(first, &hidden), (second, &full)]).is_err());
}

#[test]
fn nested_panes_keep_independent_adjacent_borders() {
    for axis in [SplitAxis::Rows, SplitAxis::Columns] {
        let mut layout = Layout::new(7, 7).unwrap();
        let first = layout.active();
        let second = layout.split_active(axis).unwrap();
        layout
            .split_active(match axis {
                SplitAxis::Rows => SplitAxis::Columns,
                SplitAxis::Columns => SplitAxis::Rows,
            })
            .unwrap();
        let screens: Vec<_> = layout
            .content_geometry()
            .panes
            .iter()
            .map(|(id, r)| (*id, Screen::new(r.rows.into(), r.columns.into()).unwrap()))
            .collect();
        let refs: Vec<_> = screens.iter().map(|(id, s)| (*id, s)).collect();
        let frame = compose(&layout, &refs).unwrap();
        match axis {
            SplitAxis::Columns => {
                assert_eq!(frame.row(2).unwrap()[2].character, '│');
                assert_eq!(frame.row(2).unwrap()[3].character, '└');
                assert_eq!(frame.row(3).unwrap()[2].character, '│');
                assert_eq!(frame.row(3).unwrap()[3].character, '┌');
            }
            SplitAxis::Rows => {
                assert_eq!(frame.row(2).unwrap()[2].character, '─');
                assert_eq!(frame.row(2).unwrap()[3].character, '─');
                assert_eq!(frame.row(3).unwrap()[2].character, '┐');
                assert_eq!(frame.row(3).unwrap()[3].character, '┌');
            }
        }
        assert!(screens.iter().any(|(id, _)| *id == first));
        assert!(screens.iter().any(|(id, _)| *id == second));
    }
}

#[test]
fn composing_taller_canvas_does_not_restore_history_or_shift_cursor() {
    let mut layout = Layout::new(7, 6).unwrap();
    let top = layout.active();
    let bottom = layout.split_active(SplitAxis::Rows).unwrap();
    let a = Screen::new(1, 4).unwrap();
    let mut b = Screen::new(2, 4).unwrap();
    Parser::new().advance(&mut b, b"OLD1\r\nOLD2\r\nLIVE");
    let snapshot = b.clone();
    let view = compose(&layout, &[(top, &a), (bottom, &b)]).unwrap();
    assert_eq!(view.cursor(), (5, 4));
    assert_eq!(&view.row(4).unwrap()[1..5], b.row(0).unwrap());
    assert_eq!(&view.row(5).unwrap()[1..5], b.row(1).unwrap());
    assert_eq!(view.history_len(), b.history_len());
    assert_eq!(b, snapshot);
}
