use rustmux::{parser::Parser, screen::Screen, window::Windows};
use std::{cell::Cell, rc::Rc};

#[test]
fn creation_selection_and_wrapping_keep_stable_ids() {
    let mut windows = Windows::default();
    assert!(windows.active().is_none());
    assert!(windows.select_next().is_none());
    assert!(windows.select_previous().is_none());
    let a = windows.create("one".into(), 1).unwrap();
    assert_eq!(windows.select_next(), Some(a));
    assert_eq!(windows.select_previous(), Some(a));
    let b = windows.create("two".into(), 2).unwrap();
    let c = windows.create("three".into(), 3).unwrap();
    assert_eq!(windows.active().unwrap().id(), c);
    assert_eq!(windows.select_next(), Some(a));
    assert_eq!(windows.select_previous(), Some(c));
    windows.select(b).unwrap();
    windows.rename(a, "中文窗口".into()).unwrap();
    assert_eq!(windows.get(a).unwrap().name(), "中文窗口");
    assert_eq!(windows.active().unwrap().id(), b);
    assert_eq!(
        windows.iter().map(|w| w.id()).collect::<Vec<_>>(),
        vec![a, b, c]
    );
    windows.close(a).unwrap();
    assert_eq!(windows.active().unwrap().id(), b);
    for result in [windows.select(a), windows.rename(a, "stale".into())] {
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }
    assert!(windows.close(a).is_err());
    assert_eq!(windows.active().unwrap().id(), b);
    let d = windows.create("one".into(), 4).unwrap();
    assert!(d.get() > c.get());
}

#[test]
fn every_removal_preserves_focus_or_selects_the_nearest_survivor() {
    for active in 0..3 {
        for removed in 0..3 {
            let mut windows = Windows::default();
            let ids: Vec<_> = (0..3)
                .map(|i| windows.create(i.to_string(), i).unwrap())
                .collect();
            windows.select(ids[active]).unwrap();
            assert_eq!(windows.close(ids[removed]).unwrap().into_content(), removed);
            let expected = if active != removed {
                active
            } else if removed == 2 {
                1
            } else {
                removed + 1
            };
            assert_eq!(windows.active().unwrap().id(), ids[expected]);
            for &id in &ids {
                if windows.get(id).is_some() {
                    windows.close(id).unwrap();
                }
            }
            assert!(windows.active().is_none());
            assert!(windows.active_mut().is_none());
            assert!(windows.select_next().is_none());
            let new = windows.create(String::new(), 9).unwrap();
            assert!(new.get() > ids[2].get());
            assert_eq!(windows.active().unwrap().id(), new);
        }
    }
}

#[test]
fn switching_preserves_split_parser_input_and_background_screen_state() {
    let mut windows = Windows::default();
    let a = windows
        .create("a".into(), (Parser::new(), Screen::new(2, 20).unwrap()))
        .unwrap();
    let b = windows
        .create("b".into(), (Parser::new(), Screen::new(2, 20).unwrap()))
        .unwrap();
    let (parser, screen) = windows.get_mut(a).unwrap().content_mut();
    parser.advance(screen, &[0xe4, 0xb8]); // First two bytes of 中.
    let (parser, screen) = windows.active_mut().unwrap().content_mut();
    parser.advance(screen, b"B\x1b[?2004h");
    windows.select(a).unwrap();
    let (parser, screen) = windows.active_mut().unwrap().content_mut();
    parser.advance(screen, &[0xad]);
    assert_eq!(screen.row(0).unwrap()[0].character, '中');
    assert!(!screen.bracketed_paste());
    let (parser, screen) = windows.get_mut(b).unwrap().content_mut();
    parser.advance(screen, b"G");
    assert_eq!(windows.active().unwrap().id(), a);
    windows.select(b).unwrap();
    let (_, screen) = windows.active().unwrap().content();
    assert!(screen.bracketed_paste());
    assert_eq!(screen.row(0).unwrap()[0].character, 'B');
    assert_eq!(screen.row(0).unwrap()[1].character, 'G');
}

#[test]
fn close_returns_ownership_without_dropping_other_windows() {
    struct Resource(Rc<Cell<usize>>);
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let drops = Rc::new(Cell::new(0));
    let mut windows = Windows::default();
    let a = windows.create("a".into(), Resource(drops.clone())).unwrap();
    windows.create("b".into(), Resource(drops.clone())).unwrap();
    windows.select(a).unwrap();
    windows.select_next();
    windows.rename(a, "renamed".into()).unwrap();
    let removed = windows.close(a).unwrap();
    assert_eq!(drops.get(), 0);
    drop(removed);
    assert_eq!(drops.get(), 1);
    drop(windows);
    assert_eq!(drops.get(), 2);
}

#[test]
fn last_window_tracks_identity_and_toggles_without_losing_history_on_noops() {
    let mut windows = Windows::default();
    assert_eq!(windows.select_last(), None);
    let a = windows.create("a".into(), ()).unwrap();
    assert_eq!(windows.select_last(), None);
    let b = windows.create("b".into(), ()).unwrap();
    let c = windows.create("c".into(), ()).unwrap();
    windows.select(a).unwrap();
    windows.select(a).unwrap();
    windows.rename(c, "renamed".into()).unwrap();
    windows.close(b).unwrap(); // Positions change; history still points to C.
    assert_eq!(windows.select_last(), Some(c));
    assert_eq!(windows.select_last(), Some(a));
    assert!(windows.select(b).is_err());
    assert_eq!(windows.select_last(), Some(c));
    windows.close(a).unwrap(); // The remembered target is gone.
    assert_eq!(windows.select_last(), None);
    assert_eq!(windows.active().unwrap().id(), c);
}

#[test]
fn last_window_handles_cyclic_selection_and_automatic_close_fallback() {
    let mut windows = Windows::default();
    let a = windows.create("a".into(), ()).unwrap();
    let b = windows.create("b".into(), ()).unwrap();
    let c = windows.create("c".into(), ()).unwrap();
    windows.select_next(); // C -> A
    assert_eq!(windows.select_last(), Some(c));
    windows.select_previous(); // C -> B
    assert_eq!(windows.select_last(), Some(c));
    windows.close(c).unwrap(); // Automatic fallback is B, already the remembered target.
    assert_eq!(windows.active().unwrap().id(), b);
    assert_eq!(windows.select_last(), None);
    windows.select(a).unwrap();
    windows.close(a).unwrap();
    windows.close(b).unwrap();
    windows.create("new".into(), ()).unwrap();
    assert_eq!(windows.select_last(), None);
}
