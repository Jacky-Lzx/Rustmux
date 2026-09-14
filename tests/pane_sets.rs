use rustmux::{
    layout::{Direction, SplitAxis},
    pane_set::PaneSet,
};
use std::{cell::RefCell, io, rc::Rc};

#[derive(Debug)]
struct Content {
    value: usize,
    drops: Rc<RefCell<Vec<usize>>>,
}

impl Drop for Content {
    fn drop(&mut self) {
        self.drops.borrow_mut().push(self.value);
    }
}

fn assert_membership<T>(panes: &PaneSet<T>) {
    let geometry = panes.layout().tiled_geometry();
    assert_eq!(panes.iter().len(), geometry.panes.len());
    for (id, _) in geometry.panes {
        assert!(panes.get(id).is_some());
    }
}

#[test]
fn split_failure_preserves_layout_zoom_contents_and_does_not_consume_id() {
    let mut panes = PaneSet::new(3, 3, String::from("first")).unwrap();
    let first = panes.layout().active();
    let second = panes
        .split_with(SplitAxis::Columns, |id, rect| {
            assert_eq!(id.get(), 1);
            assert_eq!(
                (rect.row, rect.column, rect.rows, rect.columns),
                (0, 2, 3, 1)
            );
            Ok("second".into())
        })
        .unwrap();
    panes.toggle_zoom();
    let before = panes.layout().clone();
    assert!(
        panes
            .split_with(SplitAxis::Columns, |_, _| -> io::Result<String> {
                panic!("invalid split must not invoke factory")
            })
            .is_err()
    );
    assert_eq!(panes.layout(), &before);
    let error = panes
        .split_with(SplitAxis::Rows, |id, _| -> io::Result<String> {
            assert_eq!(id.get(), 2);
            Err(io::Error::other("spawn failed"))
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "spawn failed");
    assert_eq!(panes.layout(), &before);
    assert_eq!(panes.get(first).unwrap(), "first");
    assert_eq!(panes.get(second).unwrap(), "second");
    let third = panes
        .split_with(SplitAxis::Rows, |id, rect| {
            assert_eq!(id.get(), 2);
            assert_eq!((rect.rows, rect.columns), (1, 1));
            Ok("third".into())
        })
        .unwrap();
    assert_eq!(panes.layout().active(), third);
    assert!(!panes.layout().is_zoomed());
    assert_membership(&panes);
}

#[test]
fn close_transfers_nonclone_contents_and_errors_never_drop_live_values() {
    let drops = Rc::new(RefCell::new(Vec::new()));
    let mut panes = PaneSet::new(
        9,
        13,
        Content {
            value: 0,
            drops: drops.clone(),
        },
    )
    .unwrap();
    let first = panes.layout().active();
    let second = panes
        .split_with(SplitAxis::Columns, |_, _| {
            Ok(Content {
                value: 1,
                drops: drops.clone(),
            })
        })
        .unwrap();
    let third = panes
        .split_with(SplitAxis::Rows, |_, _| {
            Ok(Content {
                value: 2,
                drops: drops.clone(),
            })
        })
        .unwrap();
    let removed = panes.close(second).unwrap();
    assert!(drops.borrow().is_empty());
    assert_eq!(removed.value, 1);
    assert_eq!(panes.layout().active(), third);
    assert_membership(&panes);
    assert!(panes.close(second).is_err());
    assert!(panes.select(second).is_err());
    assert!(panes.get_mut(second).is_none());
    assert!(drops.borrow().is_empty());
    drop(removed);
    assert_eq!(*drops.borrow(), vec![1]);
    drop(panes.close(third).unwrap());
    assert_eq!(panes.layout().active(), first);
    let before = panes.layout().clone();
    assert!(panes.close(first).is_err());
    assert_eq!(panes.layout(), &before);
    assert_eq!(*drops.borrow(), vec![1, 2]);
    drop(panes);
    assert_eq!(*drops.borrow(), vec![1, 2, 0]);
}

#[test]
fn hidden_contents_remain_mutable_and_geometry_operations_preserve_them() {
    let mut panes = PaneSet::new(9, 13, vec![1]).unwrap();
    let first = panes.layout().active();
    let second = panes
        .split_with(SplitAxis::Columns, |_, _| Ok(vec![2]))
        .unwrap();
    panes.toggle_zoom();
    panes.get_mut(first).unwrap().push(3);
    assert_eq!(panes.select_direction(Direction::Left), Some(first));
    assert_eq!(panes.active(), &[1, 3]);
    panes.active_mut().push(4);
    for (_, content) in panes.iter_mut() {
        content.push(5);
    }
    let before = panes.layout().clone();
    assert!(panes.resize(1, 1).is_err());
    assert_eq!(panes.layout(), &before);
    panes.resize(4, 7).unwrap();
    assert!(panes.layout().is_zoomed());
    assert_eq!(panes.get(first).unwrap(), &[1, 3, 4, 5]);
    assert_eq!(panes.get(second).unwrap(), &[2, 5]);
    assert_membership(&panes);
}

#[test]
fn swaps_keep_nonclone_contents_and_creation_order_until_explicit_close() {
    let drops = Rc::new(RefCell::new(Vec::new()));
    let mut panes = PaneSet::new(
        7,
        15,
        Content {
            value: 1,
            drops: drops.clone(),
        },
    )
    .unwrap();
    let first = panes.layout().active();
    let second = panes
        .split_with(SplitAxis::Columns, |_, _| {
            Ok(Content {
                value: 2,
                drops: drops.clone(),
            })
        })
        .unwrap();
    let original = panes.layout().clone();
    assert!(panes.swap_active_next());
    assert_eq!(panes.layout().geometry().panes[0].0, second);
    assert_eq!(panes.layout().active(), second);
    assert_eq!(panes.active().value, 2);
    assert_eq!(panes.get(first).unwrap().value, 1);
    assert_eq!(
        panes.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        vec![first, second]
    );
    assert_membership(&panes);
    assert!(drops.borrow().is_empty());
    assert!(panes.swap_active_previous());
    assert_eq!(panes.layout(), &original);
    drop(panes.close(first).unwrap());
    assert_eq!(*drops.borrow(), vec![1]);
    assert_eq!(panes.active().value, 2);
    drop(panes);
    assert_eq!(*drops.borrow(), vec![1, 2]);
}

#[test]
fn breaking_pane_into_window_moves_content_and_preserves_source_geometry() {
    use rustmux::window::Windows;
    let drops = Rc::new(RefCell::new(Vec::new()));
    let mut panes = PaneSet::new(
        7,
        15,
        Content {
            value: 1,
            drops: drops.clone(),
        },
    )
    .unwrap();
    let first = panes.layout().active();
    panes
        .split_with(SplitAxis::Columns, |_, _| {
            Ok(Content {
                value: 2,
                drops: drops.clone(),
            })
        })
        .unwrap();
    panes
        .split_with(SplitAxis::Rows, |_, _| {
            Ok(Content {
                value: 3,
                drops: drops.clone(),
            })
        })
        .unwrap();
    panes.resize_active(Direction::Right);
    let mut expected = panes.layout().clone();
    expected.close(expected.active()).unwrap();
    panes.toggle_zoom();
    let mut windows = Windows::default();
    let source = windows.create("work".into(), panes).unwrap();
    let destination = windows.break_active_pane().unwrap().unwrap();
    let remaining = windows.get(source).unwrap().content();
    assert_eq!(remaining.layout(), &expected);
    assert_eq!(remaining.get(first).unwrap().value, 1);
    let moved = windows.get(destination).unwrap();
    assert_eq!(moved.name(), "work");
    assert_eq!(moved.content().active().value, 3);
    assert_eq!(moved.content().iter().len(), 1);
    assert_eq!(moved.content().layout().dimensions(), (7, 15));
    assert!(!moved.content().layout().is_zoomed());
    assert_eq!(windows.active().unwrap().id(), destination);
    assert!(windows.break_active_pane().unwrap().is_none());
    assert_eq!(windows.iter().len(), 2);
    windows.select_last();
    assert_eq!(windows.active().unwrap().id(), source);
    assert!(drops.borrow().is_empty());
    drop(windows);
    let mut actual = drops.borrow().clone();
    actual.sort();
    assert_eq!(actual, vec![1, 2, 3]);
}

#[test]
fn join_moves_nonclone_contents_both_directions_without_stopping_owners() {
    use rustmux::window::Windows;
    let drops = Rc::new(RefCell::new(Vec::new()));
    let make = |value| Content {
        value,
        drops: drops.clone(),
    };
    let mut source = PaneSet::new(7, 15, make(1)).unwrap();
    source
        .split_with(SplitAxis::Columns, |_, _| Ok(make(2)))
        .unwrap();
    source.toggle_zoom();
    let mut target = PaneSet::new(7, 15, make(3)).unwrap();
    target
        .split_with(SplitAxis::Rows, |_, _| Ok(make(4)))
        .unwrap();
    target.toggle_zoom();
    let mut windows = Windows::default();
    let source_id = windows.create("source".into(), source).unwrap();
    let target_id = windows.create("target".into(), target).unwrap();
    windows.select(source_id).unwrap();
    assert!(
        windows
            .join_active_pane(target_id, SplitAxis::Columns)
            .unwrap()
    );
    assert_eq!(windows.active().unwrap().id(), target_id);
    assert_eq!(windows.active().unwrap().name(), "target");
    assert_eq!(windows.active().unwrap().content().active().value, 2);
    assert!(!windows.active().unwrap().content().layout().is_zoomed());
    assert_eq!(windows.get(source_id).unwrap().content().iter().len(), 1);
    assert!(
        !windows
            .get(source_id)
            .unwrap()
            .content()
            .layout()
            .is_zoomed()
    );
    assert_eq!(windows.get(target_id).unwrap().content().iter().len(), 3);
    assert!(
        windows
            .join_active_pane(source_id, SplitAxis::Rows)
            .unwrap()
    );
    assert_eq!(windows.active().unwrap().content().active().value, 2);
    windows.select_last();
    assert_eq!(windows.active().unwrap().id(), target_id);
    assert!(drops.borrow().is_empty());
    for window in windows.iter() {
        assert_membership(window.content());
    }
    drop(windows);
    let mut actual = drops.borrow().clone();
    actual.sort();
    assert_eq!(actual, vec![1, 2, 3, 4]);
}

#[test]
fn join_rejects_bad_targets_atomically_and_removes_only_an_empty_source() {
    use rustmux::window::Windows;
    let mut windows = Windows::default();
    let source = windows
        .create("source".into(), PaneSet::new(5, 9, 1).unwrap())
        .unwrap();
    let target = windows
        .create("target".into(), PaneSet::new(5, 2, 2).unwrap())
        .unwrap();
    windows.select(source).unwrap();
    let before_source = windows.get(source).unwrap().content().layout().clone();
    let before_target = windows.get(target).unwrap().content().layout().clone();
    assert!(
        windows
            .join_active_pane(target, SplitAxis::Columns)
            .is_err()
    );
    assert!(
        !windows
            .join_active_pane(source, SplitAxis::Columns)
            .unwrap()
    );
    assert_eq!(windows.active().unwrap().id(), source);
    assert_eq!(
        windows.get(source).unwrap().content().layout(),
        &before_source
    );
    assert_eq!(
        windows.get(target).unwrap().content().layout(),
        &before_target
    );
    windows
        .get_mut(target)
        .unwrap()
        .content_mut()
        .resize(5, 9)
        .unwrap();
    assert!(
        windows
            .join_active_pane(target, SplitAxis::Columns)
            .unwrap()
    );
    assert_eq!(windows.iter().len(), 1);
    assert!(windows.get(source).is_none());
    assert_eq!(windows.active().unwrap().id(), target);
    assert_eq!(*windows.active().unwrap().content().active(), 1);
    assert_eq!(
        windows.active().unwrap().content().layout().active().get(),
        1
    );
    assert!(windows.join_active_pane(source, SplitAxis::Rows).is_err());
    assert_eq!(windows.iter().len(), 1);
    assert_membership(windows.active().unwrap().content());
}
