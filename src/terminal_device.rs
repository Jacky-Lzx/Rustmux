//! Raw-mode access to the user's controlling terminal.

use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, poll};
use nix::sys::termios::{self, SetArg, Termios};

const POLL_TIMEOUT_MILLIS: u16 = 50;

// \x1b is ESC; ESC [ introduces a control sequence. For private modes (? prefix),
// h enables a mode and l (lowercase L) disables it.
// ?1049h saves the cursor and switches to a cleared alternate screen buffer.
const ENTER: &[u8] = b"\x1b[?1049h";
// Reset common display modes on exit, in sequence:
// CSI 0 SP q: reset cursor shape (the space is part of DECSCUSR).
// ESC >: restore numeric keypad encoding (disable application keypad).
// ?1l: restore normal cursor-key encoding (disable application cursor keys).
// ?2004l: disable bracketed paste (the markers around pasted input).
// ?1000l: disable basic mouse button reporting.
// ?1002l: disable mouse motion reporting while a button is held.
// ?1003l: disable reporting of all mouse motion.
// ?1004l: disable focus-in/focus-out event reporting.
// ?1006l: disable SGR mouse report encoding.
// 0m: reset text attributes, including colors and bold.
// ?25h: show the cursor.
// ?1049l: return to the main screen buffer and restore the saved cursor.
// These are baseline resets, not a snapshot of the previous display modes.
// Raw mode and other termios attributes are restored separately.
const LEAVE: &[u8] =
    b"\x1b[0 q\x1b>\x1b[?1l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l\x1b[0m\x1b[?25h\x1b[?1049l";

/// One nonblocking file description for terminal input and output.
pub(crate) struct TerminalDevice {
    file: File,
    original: Termios,
    active: bool,
}

impl TerminalDevice {
    pub(crate) fn open_controlling() -> io::Result<File> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stdin and stdout must be terminals",
            ));
        }
        let device = nix::unistd::ttyname(io::stdin())?;
        if device != nix::unistd::ttyname(io::stdout())? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stdin and stdout must use the same terminal",
            ));
        }
        // Open the actual device: macOS cannot poll the /dev/tty indirection.
        // A separate open description avoids changing the parent's file flags.
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK)
            .open(device)
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
