//! Pointer overrides belong to the composed display, never the child screen.
use super::*;

impl WindowInput {
    pub(super) fn footer_mode(&self) -> FooterMode {
        match self.mode {
            InputMode::Normal => FooterMode::Normal,
            InputMode::Pane => FooterMode::Pane,
            InputMode::Resize => FooterMode::Resize,
            InputMode::Move => FooterMode::Move,
            InputMode::Tab => FooterMode::Tab,
            InputMode::Session => FooterMode::Session,
            InputMode::Locked | InputMode::History => FooterMode::Locked,
        }
    }

    pub(super) fn decorate_hover(
        &self,
        view: &mut Screen,
        layout: &crate::layout::Layout,
        bar: &[(usize, usize, usize)],
        footer: &[(usize, usize, u8)],
        outer_rows: u16,
    ) {
        let separators = layout.separator_hitboxes();
        let has_bar = outer_rows > 1 && !bar.is_empty();
        let has_footer = footer_enabled(outer_rows) && !footer.is_empty();
        if has_bar || has_footer || !separators.is_empty() {
            // Collect hover reports, but WindowInput still filters them against
            // the real child's Off/Button/Drag/Any mode before forwarding.
            view.set_mouse_tracking(MouseTracking::Any);
        }
        let shape = if self.pane_drag.is_some() {
            Some("grabbing")
        } else if let Some((column, row)) = self.pointer_position {
            if column == 0 || column > view.dimensions().1 || row == 0 || row > view.dimensions().0
            {
                None
            } else if (has_bar && row == 1 && hit(bar, column))
                || (has_footer && row == usize::from(outer_rows) && hit(footer, column))
            {
                Some("pointer")
            } else {
                // Rectangles are zero-based layout coordinates; mouse reports
                // are one-based physical coordinates including the top bar.
                row.checked_sub(1 + usize::from(outer_rows > 1))
                    .and_then(|layout_row| {
                        separators.iter().find_map(|(_, axis, rect)| {
                            let layout_column = column - 1;
                            (layout_row >= usize::from(rect.row)
                                && layout_row < usize::from(rect.row + rect.rows)
                                && layout_column >= usize::from(rect.column)
                                && layout_column < usize::from(rect.column + rect.columns))
                            .then_some(match axis {
                                SplitAxis::Columns => "ew-resize",
                                SplitAxis::Rows => "ns-resize",
                            })
                        })
                    })
            }
        } else {
            None
        };
        if let Some(shape) = shape {
            view.apply_pointer(shape, &mut |_| unreachable!("display pointer set replied"));
        }
    }
}

fn hit<T>(hitboxes: &[(usize, usize, T)], column: usize) -> bool {
    hitboxes
        .iter()
        .any(|(start, end, _)| column >= *start && column < *end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::Layout, parser::Parser};

    fn child() -> Screen {
        let mut child = Screen::new(20, 80).unwrap();
        Parser::new().advance(&mut child, b"\x1b]22;>wait,text\x1b\\");
        child
    }

    fn decorate(keys: &WindowInput, layout: &Layout, child: &Screen) -> Screen {
        let mut view = child.clone();
        keys.decorate_hover(&mut view, layout, &[(2, 12, 0)], &[(30, 40, 2)], 20);
        view
    }

    #[test]
    fn only_actual_controls_override_and_child_stacks_remain_unchanged() {
        let layout = Layout::new(18, 80).unwrap();
        let mut child = child();
        let before = child.clone();
        for (position, expected) in [
            ((2, 1), "pointer"),
            ((11, 1), "pointer"),
            ((12, 1), "text"),
            ((1, 1), "text"),
            ((30, 20), "pointer"),
            ((39, 20), "pointer"),
            ((40, 20), "text"),
            ((30, 19), "text"),
            ((0, 1), "text"),
            ((81, 1), "text"),
            ((2, 21), "text"),
            ((2, 0), "text"),
        ] {
            let keys = WindowInput {
                pointer_position: Some(position),
                ..Default::default()
            };
            let view = decorate(&keys, &layout, &child);
            assert_eq!(view.pointer_shape(), Some(expected), "{position:?}");
            assert_eq!(view.mouse_tracking(), MouseTracking::Any);
            assert_eq!(view.sgr_mouse(), child.sgr_mouse());
        }
        assert_eq!(child, before);
        Parser::new().advance(&mut child, b"\x1b]22;<\x1b\\");
        assert_eq!(child.pointer_shape(), Some("wait"));
    }

    #[test]
    fn separators_use_current_geometry_and_zoom_hides_handles() {
        for (axis, shape) in [
            (SplitAxis::Columns, "ew-resize"),
            (SplitAxis::Rows, "ns-resize"),
        ] {
            let mut layout = Layout::new(18, 80).unwrap();
            layout.split_active(axis).unwrap();
            let rect = layout.separator_hitboxes()[0].2;
            let keys = WindowInput {
                pointer_position: Some((usize::from(rect.column) + 1, usize::from(rect.row) + 2)),
                ..Default::default()
            };
            let child = child();
            assert_eq!(
                decorate(&keys, &layout, &child).pointer_shape(),
                Some(shape)
            );
            layout.toggle_zoom();
            assert_eq!(
                decorate(&keys, &layout, &child).pointer_shape(),
                Some("text")
            );
            layout.toggle_zoom();
            layout.resize_separator(0, 2);
            assert_eq!(
                decorate(&keys, &layout, &child).pointer_shape(),
                Some("text")
            );
        }
    }

    #[test]
    fn dragging_overrides_position_and_absent_controls_leave_tracking_unchanged() {
        let layout = Layout::new(18, 80).unwrap();
        let child = child();
        let keys = WindowInput {
            pane_drag: Some(PaneDrag {
                separator: 0,
                axis: SplitAxis::Columns,
                position: 40,
            }),
            ..Default::default()
        };
        assert_eq!(
            decorate(&keys, &layout, &child).pointer_shape(),
            Some("grabbing")
        );
        let mut view = child.clone();
        WindowInput::default().decorate_hover(&mut view, &layout, &[], &[], 1);
        assert_eq!(view, child);
        // A footer hidden by a short terminal cannot gain a hover hitbox.
        let mut view = child.clone();
        WindowInput {
            pointer_position: Some((30, 2)),
            ..Default::default()
        }
        .decorate_hover(&mut view, &layout, &[], &[(30, 40, 2)], 2);
        assert_eq!(view, child);
    }

    #[test]
    fn complete_hover_reports_preserve_modes_and_only_any_motion_reaches_child() {
        for mode in [
            InputMode::Normal,
            InputMode::Pane,
            InputMode::Resize,
            InputMode::Move,
            InputMode::Tab,
            InputMode::Session,
        ] {
            let mut keys = WindowInput {
                mode,
                bar_enabled: true,
                ..Default::default()
            };
            let mut actions = Vec::new();
            for &byte in b"\x1b[<35;4;3M" {
                keys.feed(byte, &mut actions);
            }
            assert_eq!(keys.mode, mode);
            assert_eq!(keys.pointer_position, Some((4, 3)));
            assert!(actions.is_empty());
        }
        for tracking in [
            MouseTracking::Off,
            MouseTracking::Button,
            MouseTracking::Drag,
            MouseTracking::Any,
        ] {
            let mut keys = WindowInput {
                bar_enabled: true,
                mouse_tracking: tracking,
                pane_top: 1,
                pane_height: 18,
                pane_width: 80,
                ..Default::default()
            };
            let mut actions = Vec::new();
            for &byte in b"\x1b[<35;4;3M" {
                keys.feed(byte, &mut actions);
            }
            let expected: Vec<_> = if tracking == MouseTracking::Any {
                b"\x1b[<35;4;2M"
                    .iter()
                    .copied()
                    .map(WindowKey::Byte)
                    .collect()
            } else {
                Vec::new()
            };
            assert_eq!(actions, expected);
        }
    }

    #[test]
    fn legacy_fragments_and_paste_do_not_create_spurious_hover_positions() {
        let mut keys = WindowInput {
            bar_enabled: true,
            ..Default::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x1b[M" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(keys.pointer_position, None);
        for byte in [32 + 35, 32 + 4, 32 + 3] {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(keys.pointer_position, Some((4, 3)));
        assert!(actions.is_empty());
        for &byte in b"\x1b[200~\x1b[<35;10;10M\x1b[201~" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(keys.pointer_position, Some((4, 3)));
        assert!(!keys.paste);
    }
}
