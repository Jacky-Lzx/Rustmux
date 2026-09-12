use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::{Pid, getsid, tcgetpgrp};
use rustmux::pty::PtyShell;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn exited(shell: &mut PtyShell) -> std::process::ExitStatus {
    let end = deadline();
    loop {
        if let Some(status) = shell.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < end, "shell did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fd_count() -> usize {
    std::fs::read_dir(if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    })
    .unwrap()
    .count()
}

// One test keeps descriptor-count assertions isolated from concurrent PTY tests.
#[test]
fn shell_lifecycle_and_failures() {
    // Exercise the fd 0/1/2 allocation case in a separate process so the test
    // harness and other tests keep their own standard descriptors.
    if std::env::var_os("RUSTMUX_TEST_CLOSED_STDIO").is_some() {
        let diagnostic =
            std::fs::File::from(std::io::stderr().as_fd().try_clone_to_owned().unwrap());
        std::panic::set_hook(Box::new(move |info| {
            let _ = writeln!(&diagnostic, "{info}");
        }));
        for fd in 0..=2 {
            nix::unistd::close(fd).unwrap();
        }
        let mut shell = PtyShell::spawn("/bin/sh", 24, 80).unwrap();
        // Wait for the shell to finish its startup termios changes before
        // sending input; interactive shells may flush queued input on startup.
        let fd = shell.master_fd().unwrap();
        assert!(
            fd.as_raw_fd() >= 3,
            "master must not occupy a standard stream"
        );
        assert!(
            FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).unwrap())
                .contains(FdFlag::FD_CLOEXEC)
        );
        let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL).unwrap());
        fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
        let end = deadline();
        loop {
            let mut buffer = [0; 1024];
            match shell.read(&mut buffer) {
                Ok(n) if n > 0 => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                other => panic!("shell startup output: {other:?}"),
            }
            assert!(Instant::now() < end, "shell prompt timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        shell.write_all(b"exit 9\n").unwrap();
        let end = deadline();
        loop {
            let mut buffer = [0; 1024];
            match shell.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < end, "shell output did not close");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("shell output: {e}"),
            }
        }
        assert_eq!(exited(&mut shell).code(), Some(9));
        return;
    }
    let helper = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "shell_lifecycle_and_failures"])
        .env("RUSTMUX_TEST_CLOSED_STDIO", "1")
        .output()
        .unwrap();
    assert!(
        helper.status.success(),
        "closed-stdio helper failed: {:?}",
        helper
    );
    let before = fd_count();
    for _ in 0..16 {
        assert_eq!(
            PtyShell::spawn("/rustmux-test-no-such-shell", 24, 80)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(PtyShell::spawn("/", 24, 80).is_err());
    }
    assert_eq!(fd_count(), before, "failed spawn leaked descriptors");
    assert_eq!(
        PtyShell::spawn("/bin/sh", 0, 80).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );

    let mut shell = PtyShell::spawn("/bin/sh", 31, 97).unwrap();
    let pid = Pid::from_raw(shell.id() as i32);
    assert_eq!(
        getsid(Some(pid)).unwrap(),
        pid,
        "shell must lead its own session"
    );
    let fd = shell.master_fd().unwrap();
    assert_eq!(
        tcgetpgrp(fd).unwrap(),
        pid,
        "shell must own the terminal foreground group"
    );
    assert!(
        FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).unwrap())
            .contains(FdFlag::FD_CLOEXEC)
    );
    // Make test reads nonblocking so regressions fail within a deadline.
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL).unwrap());
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
    shell.write_all(b"test -t 0 && test -t 1 && test -t 2 && test -c /dev/tty && stty size < /dev/tty; exit 7\n").unwrap();
    let mut output = Vec::new();
    let end = deadline();
    loop {
        let mut bytes = [0; 4096];
        match shell.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&bytes[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < end,
                    "PTY output timed out: {:?}",
                    String::from_utf8_lossy(&output)
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => panic!("PTY read failed: {e}"),
        }
    }
    assert!(
        String::from_utf8_lossy(&output).contains("31 97"),
        "{:?}",
        String::from_utf8_lossy(&output)
    );
    assert_eq!(exited(&mut shell).code(), Some(7));
    assert_eq!(shell.wait().unwrap().code(), Some(7));
    assert_eq!(waitpid(pid, Some(WaitPidFlag::WNOHANG)), Err(Errno::ECHILD));
    drop(shell);

    let mut shell = PtyShell::spawn("/bin/sh", 24, 80).unwrap();
    assert_eq!(
        shell.resize(0, 80).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(
        shell.resize(24, 0).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    let status = shell.terminate().unwrap();
    assert_eq!(
        shell.resize(24, 80).unwrap_err().kind(),
        std::io::ErrorKind::NotConnected
    );
    assert!(!status.success());
    assert!(shell.master_fd().is_none());
    assert_eq!(shell.terminate().unwrap(), status);
    assert_eq!(
        shell.write(b"x").unwrap_err().kind(),
        std::io::ErrorKind::NotConnected
    );
    drop(shell);

    let shell = PtyShell::spawn("/bin/sh", 24, 80).unwrap();
    let pid = Pid::from_raw(shell.id() as i32);
    let raw = shell.master_fd().unwrap().as_raw_fd();
    drop(shell);
    assert_eq!(waitpid(pid, Some(WaitPidFlag::WNOHANG)), Err(Errno::ECHILD));
    // SAFETY: querying an integer descriptor does not dereference memory.
    assert_eq!(unsafe { nix::libc::fcntl(raw, nix::libc::F_GETFD) }, -1);
    assert_eq!(Errno::last(), Errno::EBADF);
    assert_eq!(
        fd_count(),
        before,
        "successful lifecycle leaked descriptors"
    );
}
