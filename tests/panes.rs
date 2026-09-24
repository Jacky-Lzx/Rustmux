use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl},
    sys::wait::{WaitPidFlag, waitpid},
    unistd::Pid,
};
use rustmux::{layout::SplitAxis, pane::Pane, pane_set::PaneSet, window::Windows};
use std::{
    io::{Read, Write},
    thread,
    time::{Duration, Instant},
};

fn text(pane: &Pane) -> String {
    (0..pane.screen().dimensions().0)
        .flat_map(|row| pane.screen().row(row).unwrap())
        .map(|cell| cell.character)
        .collect()
}

fn read_once(pane: &mut Pane) -> bool {
    let mut buffer = [0; 4096];
    match pane.shell_mut().read(&mut buffer) {
        Ok(0) => {
            pane.finish_output();
            false
        }
        Ok(n) => {
            pane.process_output(&buffer[..n], &mut |_| panic!("unexpected child query"));
            true
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            false
        }
        Err(error) => panic!("PTY read: {error}"),
    }
}

fn until(pane: &mut Pane, condition: impl Fn(&Pane) -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !condition(pane) {
        read_once(pane);
        assert!(Instant::now() < end, "output timed out: {}", text(pane));
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn kitty_transfer_callbacks_and_partial_commands_stay_with_their_pane() {
    let mut left = Pane::spawn("/bin/sh", 6, 40).unwrap();
    let mut right = Pane::spawn("/bin/sh", 6, 40).unwrap();
    let mut left_images = Vec::new();
    let mut right_images = Vec::new();
    left.process_output_with_graphics(
        b"left\x1b_Ga=T,f=32,s=1,v=1,i=7,m=1;AQ",
        &mut |_| {},
        &mut |image| left_images.push(image),
    );
    right.process_output_with_graphics(
        b"right\x1b_Ga=T,f=24,s=1,v=1,i=7;BAUG\x1b\\",
        &mut |_| {},
        &mut |image| right_images.push(image),
    );
    left.process_output_with_graphics(
        b"ID\x1b\\middle\x1b_Gm=0;BA==\x1b\\end",
        &mut |_| {},
        &mut |image| left_images.push(image),
    );
    assert_eq!(left_images.len(), 1);
    assert_eq!(left_images[0].data, [1, 2, 3, 4]);
    assert_eq!(left_images[0].control(b'i'), Some(b"7".as_slice()));
    assert_eq!(right_images.len(), 1);
    assert_eq!(right_images[0].data, [4, 5, 6]);
    assert!(text(&left).starts_with("leftmiddleend"), "{}", text(&left));
    assert!(text(&right).starts_with("right"), "{}", text(&right));

    // The ordinary runtime path does not retain or surface image data.
    left.process_output(b"\x1b_Gf=100;U0VDUkVU\x1b\\tail", &mut |_| {});
    assert!(!text(&left).contains("SECRET"));
    assert!(text(&left).contains("tail"));

    // Switching to the non-capturing path abandons a partial transfer.
    left.process_output_with_graphics(b"\x1b_Gf=100,m=1;QUJD\x1b\\", &mut |_| {}, &mut |image| {
        left_images.push(image)
    });
    left.process_output(b"plain", &mut |_| {});
    left.process_output_with_graphics(b"\x1b_Gm=0;RA==\x1b\\", &mut |_| {}, &mut |image| {
        left_images.push(image)
    });
    assert_eq!(left_images.len(), 1);
}

#[test]
fn kitty_image_data_is_opt_in_and_isolated_per_pane() {
    let mut left = Pane::spawn("/bin/sh", 6, 40).unwrap();
    let mut right = Pane::spawn("/bin/sh", 6, 40).unwrap();
    left.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,m=1;QUJD\x1b\\", &mut |_| {});
    right.process_output_with_image_store(b"\x1b_Ga=t,f=100,i=7;RA==\x1b\\", &mut |_| {});
    left.process_output_with_image_store(b"\x1b_Gm=0;RA==\x1b\\", &mut |_| {});
    assert_eq!(left.image_store().get(7).unwrap().data, b"ABCD");
    assert_eq!(right.image_store().get(7).unwrap().data, b"D");

    left.image_store_mut().remove(7);
    assert!(left.image_store().is_empty());
    assert_eq!(right.image_store().get(7).unwrap().data, b"D");
    right.process_output(b"\x1b_Ga=t,f=100,i=8;QQ==\x1b\\", &mut |_| {});
    assert!(right.image_store().get(8).is_none());
    right.finish_output();
    assert!(right.image_store().is_empty());
}

#[test]
fn kitty_placement_lifecycle_commands_stay_with_their_pane() {
    let mut left = Pane::spawn("/bin/sh", 6, 40).unwrap();
    let mut right = Pane::spawn("/bin/sh", 6, 40).unwrap();
    for pane in [&mut left, &mut right] {
        pane.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,p=1;QQ==\x1b\\", &mut |_| {});
    }
    left.process_output_with_image_store(b"\x1b_Ga=p,i=7,p=2\x1b\\", &mut |_| {});
    left.process_output_with_image_store(b"\x1b_Ga=d,d=I,i=7,p=1\x1b\\", &mut |_| {});
    assert_eq!(left.image_store().placements().count(), 1);
    assert_eq!(right.image_store().placements().count(), 1);
    assert!(left.image_store().get(7).is_some());
    left.process_output_with_image_store(b"\x1b_Ga=d,d=I,i=7", &mut |_| {});
    left.process_output_with_image_store(b"\x1b\\", &mut |_| {});
    assert!(left.image_store().get(7).is_none());
    assert!(right.image_store().get(7).is_some());
}

#[test]
fn kitty_delete_aborts_an_unfinished_transfer() {
    let mut pane = Pane::spawn("/bin/sh", 6, 40).unwrap();
    pane.process_output_with_image_store(b"\x1b_Ga=T,f=100,i=7,m=1;QUJD\x1b\\", &mut |_| {});
    pane.process_output_with_image_store(b"\x1b_Ga=d,d=I,i=7\x1b\\", &mut |_| {});
    pane.process_output_with_image_store(b"\x1b_Gm=0;RA==\x1b\\", &mut |_| {});
    assert!(pane.image_store().is_empty());
    assert_eq!(pane.image_store().placements().count(), 0);
}

#[test]
fn real_windows_keep_processes_and_terminal_state_isolated() {
    assert!(Pane::spawn("/definitely/missing/rustmux-shell", 24, 80).is_err());
    assert!(Pane::spawn("/bin/sh", 0, 80).is_err());
    assert!(Pane::spawn("/bin/sh", 257, 256).is_err());
    let mut windows = Windows::default();
    let a = windows
        .create("a".into(), Pane::spawn("/bin/sh", 24, 80).unwrap())
        .unwrap();
    let b = windows
        .create("b".into(), Pane::spawn("/bin/sh", 24, 80).unwrap())
        .unwrap();
    for id in [a, b] {
        let pane = windows.get_mut(id).unwrap().content_mut();
        let flags = fcntl(pane.shell().master_fd().unwrap(), FcntlArg::F_GETFL).unwrap();
        assert!(OFlag::from_bits_truncate(flags).contains(OFlag::O_NONBLOCK));
        // Wait for the initial prompt before commands: shell startup may flush input.
        until(pane, |p| !text(p).trim().is_empty());
    }
    let a_pid = windows.get(a).unwrap().content().shell().id();
    let b_pid = windows.get(b).unwrap().content().shell().id();
    assert_ne!(a_pid, b_pid);
    windows.select(a).unwrap();
    for (id, suffix) in [(a, "A"), (b, "B")] {
        let pane = windows.get_mut(id).unwrap().content_mut();
        pane.shell_mut()
            .write_all(
                format!("stty -echo; printf '\\033[2J\\033[H%s%s\\n' READY_ {suffix}\n").as_bytes(),
            )
            .unwrap();
        until(pane, |p| text(p).starts_with(&format!("READY_{suffix}")));
    }
    assert_eq!(windows.active().unwrap().id(), a);
    for _ in 0..4 {
        windows.select_next();
    }
    assert_eq!(windows.get(a).unwrap().content().shell().id(), a_pid);
    assert_eq!(windows.get(b).unwrap().content().shell().id(), b_pid);
    // Each parser retains partial input and produces replies from its own grid.
    let pane = windows.get_mut(a).unwrap().content_mut();
    pane.process_output(b"\x1b[3;4H\x1b[?2004h\x1b[6", &mut |_| {});
    let mut replies = Vec::new();
    pane.process_output(b"n", &mut |reply| replies.extend_from_slice(reply));
    assert_eq!(replies, b"\x1b[3;4R");
    assert!(pane.screen().bracketed_paste());
    assert!(!windows.get(b).unwrap().content().screen().bracketed_paste());
    let removed = windows.close(a).unwrap();
    assert!(
        windows
            .get_mut(b)
            .unwrap()
            .content_mut()
            .shell_mut()
            .try_wait()
            .unwrap()
            .is_none()
    );
    drop(removed);
    assert_eq!(
        waitpid(Pid::from_raw(a_pid as i32), Some(WaitPidFlag::WNOHANG)),
        Err(Errno::ECHILD)
    );
    let pane = windows.active_mut().unwrap().content_mut();
    pane.shell_mut()
        .write_all(b"printf '\\033[2J\\033[H%s%s\\n' STILL_ ALIVE\n")
        .unwrap();
    until(pane, |p| text(p).starts_with("STILL_ALIVE"));
    assert_eq!(pane.shell().id(), b_pid);
    pane.shell_mut().terminate().unwrap();
    prepared_resizes_preserve_state_and_update_the_real_pty();
    layout_sizes_reach_real_children_and_survive_zoom_and_close();
}

fn prepared_resizes_preserve_state_and_update_the_real_pty() {
    let mut pane = Pane::spawn("/bin/sh", 12, 40).unwrap();
    until(&mut pane, |p| !text(p).trim().is_empty());
    pane.process_output(b"\x1b[2J\x1b[Hkept\x1b[?2026h", &mut |_| {});
    let before = pane.screen().clone();
    assert!(pane.prepare_resize(0, 40).is_err());
    assert!(pane.prepare_resize(257, 256).is_err());
    drop(pane.prepare_resize(8, 30).unwrap());
    assert_eq!(pane.screen(), &before);
    pane.shell_mut()
        .write_all(b"stty -echo; printf '\\033[2J\\033[HOLD:'; stty size\n")
        .unwrap();
    until(&mut pane, |p| text(p).contains("OLD:12 40"));
    // An incomplete parser sequence must survive screen preparation and commit.
    pane.process_output(b"\x1b[2J\x1b[Hkept\x1b[?2026h\xe4\xb8", &mut |_| {});
    pane.prepare_resize(8, 30).unwrap().commit().unwrap();
    assert_eq!(pane.screen().dimensions(), (8, 30));
    assert!(!pane.screen().synchronized_output());
    pane.process_output(b"\xad", &mut |_| {});
    assert!(text(&pane).starts_with("kept中"));
    pane.shell_mut()
        .write_all(b"printf '\\033[2J\\033[HNEW:'; stty size\n")
        .unwrap();
    until(&mut pane, |p| text(p).contains("NEW:8 30"));
    pane.process_output(b"\x1b[?2026h", &mut |_| {});
    pane.prepare_resize(8, 30).unwrap().commit().unwrap();
    assert!(!pane.screen().synchronized_output());
    pane.process_output(b"\x1b[?2026h", &mut |_| {});
    pane.shell_mut().terminate().unwrap();
    let before = pane.screen().clone();
    assert!(pane.prepare_resize(6, 20).unwrap().commit().is_err());
    assert_eq!(pane.screen(), &before); // Closed master must not commit prepared cells.
    pane.finish_output();
    pane.prepare_resize(6, 20).unwrap().commit().unwrap(); // Known EOF skips ioctl.
    assert_eq!(pane.screen().dimensions(), (6, 20));
}

fn expect_size(pane: &mut Pane, marker: &str, rows: u16, columns: u16) {
    pane.shell_mut()
        .write_all(format!("stty -echo; printf '\\033[2J\\033[H{marker}:'; stty size\n").as_bytes())
        .unwrap();
    until(pane, |p| {
        text(p).contains(&format!("{marker}:{rows} {columns}"))
    });
    assert_eq!(
        pane.screen().dimensions(),
        (usize::from(rows), usize::from(columns))
    );
}

fn layout_sizes_reach_real_children_and_survive_zoom_and_close() {
    let mut panes = PaneSet::new(12, 41, Pane::spawn("/bin/sh", 10, 39).unwrap()).unwrap();
    let first = panes.layout().active();
    until(panes.active_mut(), |p| !text(p).trim().is_empty());
    let first_pid = panes.active().shell().id();
    let second = panes
        .split_with(SplitAxis::Columns, |_, rect| {
            Pane::spawn("/bin/sh", rect.rows, rect.columns)
        })
        .unwrap();
    until(panes.active_mut(), |p| !text(p).trim().is_empty());
    let second_pid = panes.active().shell().id();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(first).unwrap(), "SPLIT_A", 10, 18);
    expect_size(panes.get_mut(second).unwrap(), "SPLIT_B", 10, 19);
    panes
        .get_mut(first)
        .unwrap()
        .process_output(b"\x1b[?2026h", &mut |_| {});
    panes.select(first).unwrap();
    panes.synchronize_sizes().unwrap();
    assert!(panes.active().screen().synchronized_output()); // Focus alone isn't a resize.
    panes.toggle_zoom();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(first).unwrap(), "ZOOM_A", 10, 39);
    expect_size(panes.get_mut(second).unwrap(), "HIDDEN_B", 10, 19);
    panes.select(second).unwrap();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(first).unwrap(), "HIDDEN_A", 10, 18);
    expect_size(panes.get_mut(second).unwrap(), "ZOOM_B", 10, 39);
    panes.resize(9, 31).unwrap();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(first).unwrap(), "RESIZE_A", 7, 13);
    expect_size(panes.get_mut(second).unwrap(), "RESIZE_B", 7, 29);
    panes.toggle_zoom();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.get_mut(second).unwrap(), "UNZOOM_B", 7, 14);
    // The geometry-only API permits this size; process synchronization rejects it
    // before altering any owned screen or terminal.
    panes.resize(257, 256).unwrap();
    assert!(panes.synchronize_sizes().is_err());
    assert_eq!(panes.get(first).unwrap().screen().dimensions(), (7, 13));
    assert_eq!(panes.get(second).unwrap().screen().dimensions(), (7, 14));
    panes.resize(9, 31).unwrap();
    assert_eq!(panes.get(first).unwrap().shell().id(), first_pid);
    assert_eq!(panes.get(second).unwrap().shell().id(), second_pid);
    let removed = panes.close(first).unwrap();
    panes.synchronize_sizes().unwrap();
    expect_size(panes.active_mut(), "SURVIVOR", 7, 29);
    assert_eq!(panes.active().shell().id(), second_pid);
    drop(removed);
    assert_eq!(
        waitpid(Pid::from_raw(first_pid as i32), Some(WaitPidFlag::WNOHANG)),
        Err(Errno::ECHILD)
    );
    // Exited panes still get model geometry without attempting to resize a closed PTY.
    panes.active_mut().shell_mut().terminate().unwrap();
    panes.resize(6, 21).unwrap();
    panes.synchronize_sizes().unwrap();
    assert_eq!(panes.active().screen().dimensions(), (4, 19));
}
