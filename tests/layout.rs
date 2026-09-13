use rustmux::layout::{Direction, Layout, MAX_PANES, Rect, SplitAxis};

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

#[test]
fn closing_promotes_sibling_subtree_and_preserves_its_ids() {
    let mut layout = Layout::new(7, 11).unwrap();
    let left = layout.active();
    let top = layout.split_active(SplitAxis::Columns).unwrap();
    let bottom = layout.split_active(SplitAxis::Rows).unwrap();
    assert_eq!(layout.close(left).unwrap(), bottom);
    assert_eq!(
        layout.geometry().panes,
        vec![
            (
                top,
                Rect {
                    row: 0,
                    column: 0,
                    rows: 3,
                    columns: 11
                }
            ),
            (
                bottom,
                Rect {
                    row: 4,
                    column: 0,
                    rows: 3,
                    columns: 11
                }
            ),
        ]
    );
    assert_eq!(layout.minimum_size(), (3, 1));
    assert_partition(&layout);
    assert_eq!(layout.close(bottom).unwrap(), top);
    assert_eq!(layout.minimum_size(), (1, 1));
    assert_eq!(layout.geometry().separators, vec![]);
    assert_eq!(
        layout.geometry().panes[0].1,
        Rect {
            row: 0,
            column: 0,
            rows: 7,
            columns: 11
        }
    );
    let before = layout.clone();
    assert_eq!(
        layout.close(left).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(layout, before);
    assert!(layout.close(top).is_err());
    assert_eq!(layout, before);
    let new = layout.split_active(SplitAxis::Columns).unwrap();
    assert!(new.get() > bottom.get());
    layout.resize(1, 3).unwrap();
    assert_partition(&layout);
}

#[test]
fn every_nested_removal_obeys_focus_order_and_preserves_partition() {
    let mut original = Layout::new(31, 41).unwrap();
    for axis in [
        SplitAxis::Columns,
        SplitAxis::Rows,
        SplitAxis::Columns,
        SplitAxis::Rows,
    ] {
        original.split_active(axis).unwrap();
    }
    let ids: Vec<_> = original
        .geometry()
        .panes
        .iter()
        .map(|(id, _)| *id)
        .collect();
    for &active in &ids {
        for (index, &removed) in ids.iter().enumerate() {
            let mut layout = original.clone();
            layout.select(active).unwrap();
            let expected = if active != removed {
                active
            } else {
                ids.get(index + 1)
                    .copied()
                    .unwrap_or_else(|| ids[index - 1])
            };
            assert_eq!(layout.close(removed).unwrap(), expected);
            assert_eq!(layout.active(), expected);
            assert_eq!(
                layout
                    .geometry()
                    .panes
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>(),
                ids.iter()
                    .copied()
                    .filter(|id| *id != removed)
                    .collect::<Vec<_>>()
            );
            assert_partition(&layout);
            let (rows, columns) = layout.minimum_size();
            layout.resize(rows, columns).unwrap();
            assert_partition(&layout);
            let before = layout.clone();
            assert!(layout.select(removed).is_err());
            assert!(layout.close(removed).is_err());
            assert_eq!(layout, before);
        }
    }
}

#[test]
fn closing_at_capacity_allows_another_split_without_reusing_ids() {
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
            .split_active(if rect.rows > rect.columns {
                SplitAxis::Rows
            } else {
                SplitAxis::Columns
            })
            .unwrap();
    }
    let removed = layout.active();
    layout.close(removed).unwrap();
    let (id, rect) = layout
        .geometry()
        .panes
        .into_iter()
        .max_by_key(|(_, r)| u32::from(r.rows) * u32::from(r.columns))
        .unwrap();
    layout.select(id).unwrap();
    let new = layout
        .split_active(if rect.rows > rect.columns {
            SplitAxis::Rows
        } else {
            SplitAxis::Columns
        })
        .unwrap();
    assert_eq!(new.get(), MAX_PANES as u64);
    assert_eq!(layout.geometry().panes.len(), MAX_PANES);
    assert_partition(&layout);
}

#[test]
fn zoom_is_a_reversible_view_and_selection_can_reach_hidden_panes() {
    let mut layout = Layout::new(9, 13).unwrap();
    let single = layout.clone();
    assert!(!layout.toggle_zoom());
    assert_eq!(layout, single);
    let first = layout.active();
    layout.split_active(SplitAxis::Columns).unwrap();
    layout.split_active(SplitAxis::Rows).unwrap();
    let before = layout.clone();
    let tiled = layout.geometry();
    assert!(layout.toggle_zoom());
    assert!(layout.is_zoomed());
    assert_eq!(layout.tiled_geometry(), tiled);
    assert_eq!(
        layout.geometry().panes,
        vec![(
            layout.active(),
            Rect {
                row: 0,
                column: 0,
                rows: 9,
                columns: 13,
            }
        )]
    );
    assert_partition(&layout);
    assert!(!layout.toggle_zoom());
    assert_eq!(layout, before);
    assert!(layout.toggle_zoom());
    layout.select(first).unwrap();
    assert!(layout.is_zoomed());
    assert_eq!(layout.geometry().panes[0].0, first);
    assert_eq!(layout.tiled_geometry(), tiled);
    layout.toggle_zoom();
    assert_eq!(layout.geometry(), tiled);
}

#[test]
fn zoom_resize_preserves_tree_and_requires_space_for_restoration() {
    let mut layout = Layout::new(9, 13).unwrap();
    layout.split_active(SplitAxis::Columns).unwrap();
    layout.split_active(SplitAxis::Rows).unwrap();
    let mut unzoomed = layout.clone();
    layout.toggle_zoom();
    let before = layout.clone();
    assert!(layout.resize(1, 1).is_err());
    assert_eq!(layout, before);
    for (rows, columns) in [(3, 3), (7, 19), (20, 30)] {
        layout.resize(rows, columns).unwrap();
        unzoomed.resize(rows, columns).unwrap();
        assert!(layout.is_zoomed());
        assert_eq!(layout.tiled_geometry(), unzoomed.geometry());
        assert_partition(&layout);
    }
    layout.toggle_zoom();
    assert_eq!(layout, unzoomed);
}

#[test]
fn structural_changes_exit_zoom_only_after_success() {
    let mut layout = Layout::new(3, 3).unwrap();
    let first = layout.active();
    let second = layout.split_active(SplitAxis::Columns).unwrap();
    layout.toggle_zoom();
    let before = layout.clone();
    // The full-area zoom is wide enough, but the actual tiled leaf is only one column.
    assert!(layout.split_active(SplitAxis::Columns).is_err());
    assert_eq!(layout, before);
    let third = layout.split_active(SplitAxis::Rows).unwrap();
    assert!(!layout.is_zoomed());
    assert_eq!(layout.active(), third);
    assert_eq!(layout.geometry().panes.len(), 3);
    assert_partition(&layout);
    layout.toggle_zoom();
    assert_eq!(layout.close(first).unwrap(), third); // A hidden pane can close.
    assert!(!layout.is_zoomed());
    assert_partition(&layout);
    layout.toggle_zoom();
    let before = layout.clone();
    assert!(layout.close(first).is_err());
    assert!(layout.select(first).is_err());
    assert_eq!(layout, before);
    assert_eq!(layout.close(third).unwrap(), second);
    assert!(!layout.is_zoomed());
    assert!(!layout.toggle_zoom());
    assert_partition(&layout);
}

#[test]
fn directional_selection_uses_overlap_and_preserves_geometry_and_zoom() {
    let mut layout = Layout::new(15, 17).unwrap();
    let left = layout.active();
    let right_top = layout.split_active(SplitAxis::Columns).unwrap();
    let right_middle = layout.split_active(SplitAxis::Rows).unwrap();
    let right_bottom = layout.split_active(SplitAxis::Rows).unwrap();
    layout.select(left).unwrap();
    let tiled = layout.tiled_geometry();
    assert_eq!(layout.select_direction(Direction::Right), Some(right_top)); // Aligned top edge.
    assert_eq!(layout.select_direction(Direction::Down), Some(right_middle)); // Nearest edge.
    assert_eq!(layout.select_direction(Direction::Down), Some(right_bottom));
    let before = layout.clone();
    assert_eq!(layout.select_direction(Direction::Down), None);
    assert_eq!(layout, before);
    assert_eq!(layout.select_direction(Direction::Left), Some(left));
    assert_eq!(layout.tiled_geometry(), tiled);
    layout.toggle_zoom();
    assert_eq!(layout.select_direction(Direction::Right), Some(right_top));
    assert!(layout.is_zoomed());
    assert_eq!(layout.geometry().panes[0].0, right_top);
    assert_eq!(layout.tiled_geometry(), tiled);
    layout.resize(9, 11).unwrap();
    layout.close(right_middle).unwrap();
    assert_eq!(layout.select_direction(Direction::Down), Some(right_bottom));
    assert_partition(&layout);
}

#[test]
fn directional_ties_are_deterministic_and_single_panes_do_not_wrap() {
    let mut layout = Layout::new(7, 7).unwrap();
    let top = layout.active();
    for direction in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        let before = layout.clone();
        assert_eq!(layout.select_direction(direction), None);
        assert_eq!(layout, before);
    }
    let bottom_left = layout.split_active(SplitAxis::Rows).unwrap();
    let bottom_right = layout.split_active(SplitAxis::Columns).unwrap();
    layout.select(top).unwrap();
    assert_eq!(layout.select_direction(Direction::Down), Some(bottom_left));
    assert_eq!(
        layout.select_direction(Direction::Right),
        Some(bottom_right)
    );
    assert_eq!(layout.select_direction(Direction::Up), Some(top));
}

#[test]
fn directional_focus_prefers_aligned_edges_over_an_extra_cell() {
    // Exercise both equal halves and the extra cell assigned to the second half.
    for size in [15, 16, 17, 18] {
        for (axis, cross_axis, forward, backward) in [
            (
                SplitAxis::Columns,
                SplitAxis::Rows,
                Direction::Right,
                Direction::Left,
            ),
            (
                SplitAxis::Rows,
                SplitAxis::Columns,
                Direction::Down,
                Direction::Up,
            ),
        ] {
            let mut layout = Layout::new(size, size).unwrap();
            let first = layout.active();
            let aligned = layout.split_active(axis).unwrap();
            layout.split_active(cross_axis).unwrap();
            layout.select(first).unwrap();
            assert_eq!(layout.select_direction(forward), Some(aligned));

            // Mirror the arrangement to cover left and up as well.
            let mut layout = Layout::new(size, size).unwrap();
            let aligned = layout.active();
            let second = layout.split_active(axis).unwrap();
            layout.select(aligned).unwrap();
            layout.split_active(cross_axis).unwrap();
            layout.select(second).unwrap();
            assert_eq!(layout.select_direction(backward), Some(aligned));
        }
    }
}

#[test]
fn manual_resize_moves_separator_and_retains_ratio_across_outer_resize() {
    let mut layout = Layout::new(7, 11).unwrap();
    let left = layout.active();
    let right = layout.split_active(SplitAxis::Columns).unwrap();
    assert!(layout.resize_active(Direction::Right));
    assert_eq!(layout.geometry().panes[0].1.columns, 6);
    assert_eq!(layout.geometry().panes[1].1.columns, 4);
    assert_eq!(layout.active(), right);
    let original = layout.geometry();
    layout.resize(7, 21).unwrap();
    assert_eq!(layout.geometry().panes[0].1.columns, 12);
    assert_eq!(layout.geometry().panes[1].1.columns, 8);
    layout.resize(1, 3).unwrap();
    assert_partition(&layout);
    layout.resize(7, 11).unwrap();
    assert_eq!(layout.geometry(), original);
    layout.select(left).unwrap();
    assert!(layout.resize_active(Direction::Left));
    assert_eq!(layout.geometry().panes[0].1.columns, 5);
    assert_eq!(layout.active(), left);
}

#[test]
fn nearest_matching_separator_stops_at_minimum_without_moving_ancestor() {
    let mut layout = Layout::new(9, 31).unwrap();
    let left = layout.active();
    layout.split_active(SplitAxis::Columns).unwrap();
    layout.select(left).unwrap();
    layout.split_active(SplitAxis::Columns).unwrap();
    let outer = layout.geometry().separators[0];
    assert!(layout.resize_active(Direction::Right));
    assert_eq!(layout.geometry().panes[0].1.columns, 8);
    assert_eq!(layout.geometry().separators[0], outer);
    for _ in 0..30 {
        layout.resize_active(Direction::Right);
    }
    assert_eq!(layout.geometry().panes[1].1.columns, 1);
    let before = layout.clone();
    assert!(!layout.resize_active(Direction::Right));
    assert!(!layout.resize_active(Direction::Up));
    assert_eq!(layout, before);
    assert_eq!(layout.geometry().separators[0], outer);
    assert_partition(&layout);
}

#[test]
fn manual_resize_respects_nested_minima_and_zoom_and_preserves_partition() {
    let mut layout = Layout::new(15, 31).unwrap();
    let left = layout.active();
    let right = layout.split_active(SplitAxis::Columns).unwrap();
    layout.split_active(SplitAxis::Columns).unwrap();
    layout.select(left).unwrap();
    layout.split_active(SplitAxis::Rows).unwrap();
    let focused = layout.active();
    for direction in [
        Direction::Left,
        Direction::Down,
        Direction::Right,
        Direction::Up,
    ] {
        for _ in 0..40 {
            layout.resize_active(direction);
            assert_partition(&layout);
            assert_eq!(layout.active(), focused);
        }
    }
    assert!(layout.geometry().panes.iter().any(|(id, _)| *id == right));
    layout.toggle_zoom();
    let before = layout.clone();
    for direction in [
        Direction::Left,
        Direction::Down,
        Direction::Right,
        Direction::Up,
    ] {
        assert!(!layout.resize_active(direction));
        assert_eq!(layout, before);
    }
    layout.toggle_zoom();
    for rows in 3..17 {
        for columns in 5..35 {
            layout.resize(rows, columns).unwrap();
            assert_partition(&layout);
        }
    }
}

#[test]
fn manual_resize_is_noop_for_single_pane_and_safe_at_maximum_dimension() {
    let mut layout = Layout::new(u16::MAX, u16::MAX).unwrap();
    let before = layout.clone();
    assert!(!layout.resize_active(Direction::Left));
    assert_eq!(layout, before);
    layout.split_active(SplitAxis::Columns).unwrap();
    assert!(layout.resize_active(Direction::Right));
    assert_eq!(layout.geometry().panes[0].1.columns, 32768);
    layout.resize(1, 3).unwrap();
    layout.resize(u16::MAX, u16::MAX).unwrap();
    assert_eq!(layout.geometry().panes[0].1.columns, 32768);
}
