use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl},
    sys::wait::{WaitPidFlag, waitpid},
    unistd::Pid,
};
use rustmux::{pane::Pane, window::Windows};
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
}
