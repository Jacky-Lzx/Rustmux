//! Raw-mode access to the user's controlling terminal.

use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, poll};
use nix::sys::termios::{self, SetArg, Termios};

const POLL_TIMEOUT_MILLIS: u16 = 50;

// \x1b is ESC; ESC [ introduces a control sequence. For private modes (? prefix),
// h enables a mode and l (lowercase L) disables it.
// ?1049h saves the cursor and switches to a cleared alternate screen buffer.
const ENTER: &[u8] = b"\x1b[?1049h\x1b]22;\x1b\\";
// Reset common display modes on exit, in sequence:
// CSI 0 SP q: reset cursor shape (the space is part of DECSCUSR).
// ESC >: restore numeric keypad encoding (disable application keypad).
// ?1l: restore normal cursor-key encoding (disable application cursor keys).
// ?67l: make the Backspace key send DEL rather than BS.
// CSI = 0 u: disable Kitty progressive keyboard enhancements.
// ?2004l: disable bracketed paste (the markers around pasted input).
// ?1000l: disable basic mouse button reporting.
// ?1002l: disable mouse motion reporting while a button is held.
// ?1003l: disable reporting of all mouse motion.
// ?1004l: disable focus-in/focus-out event reporting.
// ?1006l: disable SGR mouse report encoding.
// OSC 112 ST: restore the terminal's configured cursor color.
// OSC 22 ST: reset the alternate-screen pointer before leaving it.
// 0m: reset text attributes, including colors and bold.
// ?25h: show the cursor.
// ?1049l: return to the main screen buffer and restore the saved cursor.
// These are baseline resets, not a snapshot of the previous display modes.
// Raw mode and other termios attributes are restored separately.
const LEAVE: &[u8] =
    b"\x1b[0 q\x1b>\x1b[?1l\x1b[?67l\x1b[=0u\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l\x1b]112\x1b\\\x1b]22;\x1b\\\x1b[0m\x1b[?25h\x1b[?1049l";

/// One nonblocking file description for terminal input and output.
pub(crate) struct TerminalDevice {
    file: File,
    original: Termios,
    active: bool,
}

impl TerminalDevice {
    pub(crate) fn open_controlling() -> io::Result<File> {
        open_terminal(io::stdin().as_fd(), io::stdout().as_fd())
    }

    pub(crate) fn enter(file: File) -> io::Result<Self> {
        let original = termios::tcgetattr(&file)?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        let mut terminal = Self {
            file,
            original,
            active: true,
        };
        termios::tcsetattr(&terminal.file, SetArg::TCSANOW, &raw)?;
        terminal.write_all(ENTER)?;
        Ok(terminal)
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    pub(crate) fn size(&self) -> io::Result<nix::pty::Winsize> {
        window_size(&self.file)
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // Always attempt termios restoration, independently of output failures.
        let modes = termios::tcsetattr(&self.file, SetArg::TCSANOW, &self.original);
        let screen = self.write_all(LEAVE);
        if modes.is_ok() && screen.is_ok() {
            self.active = false;
        }
        modes?;
        screen
    }

    pub(crate) fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(500);
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "terminal control output stalled",
                ));
            }
            match self.file.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut fds = [PollFd::new(self.file.as_fd(), PollFlags::POLLOUT)];
                    match poll(&mut fds, POLL_TIMEOUT_MILLIS) {
                        Ok(_) | Err(Errno::EINTR) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

fn open_terminal(input: BorrowedFd<'_>, output: BorrowedFd<'_>) -> io::Result<File> {
    if !input.is_terminal() || !output.is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdin and stdout must be terminals",
        ));
    }
    let identity = nix::sys::stat::fstat(input)
        .map_err(|error| terminal_error("inspect stdin terminal", error))?;
    let output_identity = nix::sys::stat::fstat(output)
        .map_err(|error| terminal_error("inspect stdout terminal", error))?;
    if identity.st_dev != output_identity.st_dev || identity.st_rdev != output_identity.st_rdev {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdin and stdout must use the same terminal",
        ));
    }
    let device = terminal_path(input)
        .map_err(|error| terminal_error("resolve stdin terminal path", error))?;
    // Opening a PTY master creates a different terminal, even if its device
    // number matches. Frontend I/O must use the existing slave instead.
    if device == std::path::Path::new("/dev/ptmx")
        || device == std::path::Path::new("/dev/pts/ptmx")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stdin must use a terminal slave, not a PTY master",
        ));
    }
    // Open the actual device: macOS cannot poll the /dev/tty indirection.
    // A separate open description avoids changing the parent's file flags.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(&device)
        .map_err(|error| terminal_error(&format!("open terminal {}", device.display()), error))?;
    let opened = nix::sys::stat::fstat(&file)
        .map_err(|error| terminal_error("inspect reopened terminal", error))?;
    if !file.is_terminal() || opened.st_dev != identity.st_dev || opened.st_rdev != identity.st_rdev
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reopened terminal does not match stdin",
        ));
    }
    Ok(file)
}

fn terminal_error(operation: &str, error: impl Into<io::Error>) -> io::Error {
    let error = error.into();
    io::Error::new(error.kind(), format!("{operation}: {error}"))
}

#[cfg(target_os = "macos")]
fn terminal_path(file: BorrowedFd<'_>) -> io::Result<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    // Darwin's ttyname_r scans /dev; failed device lookup also returns ERANGE.
    // F_GETPATH resolves the open descriptor without that directory scan.
    let mut path = [0u8; nix::libc::PATH_MAX as usize];
    // SAFETY: file is live; F_GETPATH receives PATH_MAX bytes of writable storage.
    if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETPATH, path.as_mut_ptr()) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    let path = CStr::from_bytes_until_nul(&path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "unterminated terminal path"))?;
    Ok(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

#[cfg(not(target_os = "macos"))]
fn terminal_path(file: BorrowedFd<'_>) -> io::Result<PathBuf> {
    Ok(nix::unistd::ttyname(file)?)
}

impl Drop for TerminalDevice {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub(crate) fn window_size(file: &impl AsRawFd) -> io::Result<nix::pty::Winsize> {
    let mut size = nix::pty::Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: file is live and size points to writable Winsize storage.
    if unsafe { nix::libc::ioctl(file.as_raw_fd(), nix::libc::TIOCGWINSZ, &mut size) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::pty::openpty;

    #[test]
    fn rejects_redirected_input_or_output() {
        let pty = openpty(None, None).unwrap();
        let file = tempfile::tempfile().unwrap();
        for (input, output) in [
            (file.as_fd(), pty.slave.as_fd()),
            (pty.slave.as_fd(), file.as_fd()),
        ] {
            let error = open_terminal(input, output).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(error.to_string(), "stdin and stdout must be terminals");
        }
    }

    #[test]
    fn rejects_different_terminal_devices() {
        let first = openpty(None, None).unwrap();
        let second = openpty(None, None).unwrap();
        let error = open_terminal(first.slave.as_fd(), second.slave.as_fd()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            error.to_string(),
            "stdin and stdout must use the same terminal"
        );
    }

    #[test]
    fn reopened_slave_routes_io_without_changing_parent_flags() {
        let pty = openpty(None, None).unwrap();
        let output = nix::unistd::dup(&pty.slave).unwrap();
        let flags = fcntl(&pty.slave, FcntlArg::F_GETFL).unwrap();
        let mut raw = termios::tcgetattr(&pty.slave).unwrap();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(&pty.slave, SetArg::TCSANOW, &raw).unwrap();
        let mut file = open_terminal(pty.slave.as_fd(), output.as_fd()).unwrap();
        assert_eq!(fcntl(&pty.slave, FcntlArg::F_GETFL).unwrap(), flags);
        assert_eq!(fcntl(&output, FcntlArg::F_GETFL).unwrap(), flags);
        assert!(
            OFlag::from_bits_truncate(fcntl(&file, FcntlArg::F_GETFL).unwrap())
                .contains(OFlag::O_NONBLOCK)
        );
        let mut bytes = [0; 64];
        assert_eq!(
            file.read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        nix::unistd::write(&pty.master, b"input").unwrap();
        let mut read = [PollFd::new(file.as_fd(), PollFlags::POLLIN)];
        assert_eq!(poll(&mut read, 1000u16).unwrap(), 1);
        let length = file.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"input");
        file.write_all(b"output").unwrap();
        let mut read = [PollFd::new(pty.master.as_fd(), PollFlags::POLLIN)];
        assert_eq!(poll(&mut read, 1000u16).unwrap(), 1);
        let length = nix::unistd::read(&pty.master, &mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"output");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rejects_master_instead_of_opening_a_new_pty() {
        let pty = openpty(None, None).unwrap();
        let error = open_terminal(pty.master.as_fd(), pty.master.as_fd()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("terminal slave"));
    }
}
