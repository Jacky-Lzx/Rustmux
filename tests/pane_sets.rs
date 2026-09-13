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
