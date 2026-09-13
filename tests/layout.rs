use rustmux::layout::{Layout, MAX_PANES, Rect, SplitAxis};

fn assert_partition(layout: &Layout) {
    let (rows, columns) = layout.dimensions();
    let geometry = layout.geometry();
    assert_eq!(geometry.separators.len() + 1, geometry.panes.len());
    assert!(geometry.panes.iter().any(|(id, _)| *id == layout.active()));
    let mut covered = vec![false; usize::from(rows) * usize::from(columns)];
    for rect in geometry
        .panes
        .iter()
        .map(|(_, rect)| rect)
        .chain(&geometry.separators)
    {
        assert!(rect.rows > 0 && rect.columns > 0);
        assert!(u32::from(rect.row) + u32::from(rect.rows) <= u32::from(rows));
        assert!(u32::from(rect.column) + u32::from(rect.columns) <= u32::from(columns));
        for row in rect.row..rect.row + rect.rows {
            for column in rect.column..rect.column + rect.columns {
                let cell = usize::from(row) * usize::from(columns) + usize::from(column);
                assert!(
                    !covered[cell],
                    "overlapping pane/separator at {row},{column}"
                );
                covered[cell] = true;
            }
        }
    }
    assert!(
        covered.into_iter().all(|cell| cell),
        "unassigned content cells"
    );
}

#[test]
fn nested_splits_have_exact_rectangles_and_preserve_ids_on_resize() {
    let mut layout = Layout::new(7, 11).unwrap();
    let first = layout.active();
    let right = layout.split_active(SplitAxis::Columns).unwrap();
    layout.select(first).unwrap();
    let bottom = layout.split_active(SplitAxis::Rows).unwrap();
    assert_eq!(
        layout.geometry().panes,
        vec![
            (
                first,
                Rect {
                    row: 0,
                    column: 0,
                    rows: 3,
                    columns: 5
                }
            ),
            (
                bottom,
                Rect {
                    row: 4,
                    column: 0,
                    rows: 3,
                    columns: 5
                }
            ),
            (
                right,
                Rect {
                    row: 0,
                    column: 6,
                    rows: 7,
                    columns: 5
                }
            ),
        ]
    );
    assert_eq!(layout.minimum_size(), (3, 3));
    for rows in 3..12 {
        for columns in 3..18 {
            layout.resize(rows, columns).unwrap();
            assert_partition(&layout);
            assert_eq!(layout.active(), bottom);
            assert_eq!(
                layout
                    .geometry()
                    .panes
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>(),
                vec![first, bottom, right]
            );
        }
    }
}

#[test]
fn resize_respects_asymmetric_subtree_minima_and_rejects_without_mutation() {
    let mut layout = Layout::new(1, 7).unwrap();
    let first = layout.active();
    layout.split_active(SplitAxis::Columns).unwrap();
    layout.select(first).unwrap();
    layout.split_active(SplitAxis::Columns).unwrap();
    assert_eq!(layout.minimum_size(), (1, 5));
    layout.resize(1, 5).unwrap(); // Equal root halves would leave the left subtree too small.
    assert_eq!(
        layout
            .geometry()
            .panes
            .iter()
            .map(|(_, r)| r.column)
            .collect::<Vec<_>>(),
        vec![0, 2, 4]
    );
    assert_partition(&layout);
    let before = layout.clone();
    for (rows, columns) in [(0, 5), (1, 0), (1, 4)] {
        assert!(layout.resize(rows, columns).is_err());
        assert_eq!(layout, before);
    }
    layout.resize(1, 5).unwrap();
    assert_eq!(layout, before);
}

#[test]
fn failed_splits_do_not_change_focus_or_consume_ids() {
    assert!(Layout::new(0, 1).is_err());
    assert!(Layout::new(1, 0).is_err());
    let mut layout = Layout::new(2, 2).unwrap();
    let before = layout.clone();
    for axis in [SplitAxis::Rows, SplitAxis::Columns] {
        assert!(layout.split_active(axis).is_err());
        assert_eq!(layout, before);
    }
    layout.resize(3, 3).unwrap();
    let second = layout.split_active(SplitAxis::Columns).unwrap();
    assert_eq!(second.get(), 1);
    let third = layout.split_active(SplitAxis::Rows).unwrap();
    assert_eq!(third.get(), 2);
    assert_partition(&layout);
}

#[test]
fn pane_cap_bounds_tree_and_maximum_dimensions_do_not_overflow() {
    let mut layout = Layout::new(128, 128).unwrap();
    for _ in 1..MAX_PANES {
        let (id, rect) = layout
            .geometry()
            .panes
            .into_iter()
            .max_by_key(|(_, r)| u32::from(r.rows) * u32::from(r.columns))
            .unwrap();
        layout.select(id).unwrap();
        layout
            .split_active(if rect.columns >= rect.rows {
                SplitAxis::Columns
            } else {
                SplitAxis::Rows
            })
            .unwrap();
    }
    assert_partition(&layout);
    let before = layout.clone();
    assert!(layout.split_active(SplitAxis::Rows).is_err());
    assert_eq!(layout, before);
    layout.resize(u16::MAX, u16::MAX).unwrap();
    let geometry = layout.geometry();
    let area: u64 = geometry
        .panes
        .iter()
        .map(|(_, r)| r)
        .chain(&geometry.separators)
        .map(|r| u64::from(r.rows) * u64::from(r.columns))
        .sum();
    assert_eq!(area, u64::from(u16::MAX).pow(2));
}
