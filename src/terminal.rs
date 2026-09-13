//! Single-pane input forwarding and model-based terminal rendering.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, poll};
use nix::sys::termios::{self, SetArg, Termios};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};

use crate::parser::MAX_REPLY_BYTES;
use crate::pty::PtyShell;
use crate::{parser::Parser, render::render, screen::Screen};

// Bound pending keyboard input to 64 KiB; output retains at most one frame.
const LIMIT: usize = 64 * 1024;
const POLL_TIMEOUT_MILLIS: u16 = 50;
const MAX_CELLS: usize = 64 * 1024;
const MAX_FRAME: usize = 16 * 1024 * 1024;
const FRAME_INTERVAL: Duration = Duration::from_millis(6);

// \x1b is ESC; ESC [ introduces a control sequence. For private modes (? prefix),
// h enables a mode and l (lowercase L) disables it.
// ?1049h saves the cursor and switches to a cleared alternate screen buffer.
const ENTER: &[u8] = b"\x1b[?1049h";
// Reset common display modes on exit, in sequence:
// ?1l: restore normal cursor-key encoding (disable application cursor keys).
// ?2004l: disable bracketed paste (the markers around pasted input).
// ?1000l: disable basic mouse button reporting.
// ?1002l: disable mouse motion reporting while a button is held.
// ?1003l: disable reporting of all mouse motion.
// ?1006l: disable SGR mouse report encoding.
// 0m: reset text attributes, including colors and bold.
// ?25h: show the cursor.
// ?1049l: return to the main screen buffer and restore the saved cursor.
// These are baseline resets, not a snapshot of the previous display modes.
// Raw mode and other termios attributes are restored separately.
const LEAVE: &[u8] =
    b"\x1b[?1l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[0m\x1b[?25h\x1b[?1049l";

/// Run on the controlling terminal during single-threaded program startup.
/// Returns the shell exit code, or 128 + signal for termination by signal.
/// Input and output must be terminals. Raw mode is restored before returning.
pub fn run(shell_path: &OsStr) -> io::Result<u8> {
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
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(device)?;
    let size = window_size(&file)?;
    // Start the shell before changing the outer terminal, so exec failures
    // cannot leave it raw. Signal registration below creates no worker threads.
    check_size(size.ws_row, size.ws_col)?;
    let screen = Screen::new(usize::from(size.ws_row), usize::from(size.ws_col))?;
    let mut shell = PtyShell::spawn(shell_path, size.ws_row, size.ws_col)?;
    let master = shell.master_fd().expect("new PTY is open");
    let flags = OFlag::from_bits_truncate(fcntl(master, FcntlArg::F_GETFL)?);
    fcntl(master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    let signals = Signals::install()?;
    let mut terminal = Terminal::enter(file)?;
    let result = forward(&mut terminal.file, &mut shell, &signals, screen);
    // Restore the user's terminal before potentially blocking child cleanup.
    let restored = terminal.restore();
    drop(shell);
    match result {
        Err(error) => Err(error),
        Ok(code) => restored.map(|()| code),
    }
}

fn window_size(file: &File) -> io::Result<nix::pty::Winsize> {
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

struct Terminal {
    file: File,
    original: Termios,
    active: bool,
}

impl Terminal {
    fn enter(file: File) -> io::Result<Self> {
        let original = termios::tcgetattr(&file)?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        let mut terminal = Self {
            file,
            original,
            active: true,
        };
        termios::tcsetattr(&terminal.file, SetArg::TCSANOW, &raw)?;
        terminal.control(ENTER)?;
        Ok(terminal)
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        // Always attempt termios restoration, independently of output failures.
        let modes = termios::tcsetattr(&self.file, SetArg::TCSANOW, &self.original);
        let screen = self.control(LEAVE);
        if modes.is_ok() && screen.is_ok() {
            self.active = false;
        }
        modes?;
        screen
    }

    fn control(&mut self, mut bytes: &[u8]) -> io::Result<()> {
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
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let mut fds = [PollFd::new(self.file.as_fd(), PollFlags::POLLOUT)];
                    match poll(&mut fds, POLL_TIMEOUT_MILLIS) {
                        Ok(_) | Err(Errno::EINTR) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

struct Signals {
    pending: Arc<AtomicUsize>,
    resize: Arc<AtomicBool>,
    ids: Vec<signal_hook::SigId>,
}

impl Signals {
    fn install() -> io::Result<Self> {
        let mut signals = Self {
            pending: Arc::new(AtomicUsize::new(0)),
            // Re-read after installing the handler to cover changes since spawn.
            resize: Arc::new(AtomicBool::new(true)),
            ids: Vec::new(),
        };
        for signal in [SIGHUP, SIGTERM, SIGINT, SIGQUIT] {
            signals.ids.push(signal_hook::flag::register_usize(
                signal,
                signals.pending.clone(),
                signal as usize,
            )?);
        }
        signals.ids.push(signal_hook::flag::register(
            SIGWINCH,
            signals.resize.clone(),
        )?);
        Ok(signals)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

fn exit_code(status: ExitStatus) -> u8 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)) as u8
}

fn check_size(rows: u16, columns: u16) -> io::Result<()> {
    let cells = usize::from(rows) * usize::from(columns);
    if cells == 0 || cells > MAX_CELLS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal dimensions must be nonzero and at most 65536 cells",
        ));
    }
    Ok(())
}

struct FrameWriter<'a>(&'a mut VecDeque<u8>);
impl Write for FrameWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME - self.0.len() {
            return Err(io::Error::other("rendered frame exceeds output limit"));
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn forward(
    terminal: &mut File,
    shell: &mut PtyShell,
    signals: &Signals,
    mut screen: Screen,
) -> io::Result<u8> {
    let mut parser = Parser::new();
    let mut to_shell = VecDeque::new();
    let mut to_terminal = VecDeque::new();
    let mut dirty = true;
    let mut next_frame = Instant::now();
    let mut eof = false;
    let mut eof_at = None;
    let mut status = None;
    loop {
        let received = signals.pending.load(Ordering::Relaxed);
        if received != 0 {
            return Ok((128 + received) as u8);
        }
        if status.is_none() {
            status = shell.try_wait()?;
        }
        if signals.resize.swap(false, Ordering::Relaxed) {
            let size = window_size(terminal)?;
            if size.ws_row != 0 && size.ws_col != 0 {
                check_size(size.ws_row, size.ws_col)?;
                screen.resize(usize::from(size.ws_row), usize::from(size.ws_col))?;
                if status.is_none() && !eof {
                    shell.resize(size.ws_row, size.ws_col)?;
                }
                dirty = true;
            }
        }
        if dirty && to_terminal.is_empty() && (eof || Instant::now() >= next_frame) {
            render(&screen, &mut FrameWriter(&mut to_terminal))?;
            dirty = false;
            next_frame = Instant::now() + FRAME_INTERVAL;
        }
        if eof {
            if !dirty
                && to_terminal.is_empty()
                && let Some(status) = status
            {
                return Ok(exit_code(status));
            }
            if status.is_none()
                && eof_at.is_some_and(|time: Instant| time.elapsed() > Duration::from_secs(1))
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "shell kept running after PTY closed",
                ));
            }
        }
        let mut outer_events = PollFlags::empty();
        let mut inner_events = PollFlags::empty();
        if status.is_none() && !eof && to_shell.len() < LIMIT {
            outer_events |= PollFlags::POLLIN;
        }
        if !to_terminal.is_empty() {
            outer_events |= PollFlags::POLLOUT;
        }
        // Backpressure reaches the child: don't accumulate frames or consume
        // unbounded child output while the previous frame is still being sent.
        // Reserve worst-case reply space before reading child output. A single
        // byte can finish a query retained from a previous read.
        let reply_read_limit = if status.is_some() {
            8192 // No live child to receive replies; drain its final output.
        } else {
            (LIMIT - to_shell.len()) / MAX_REPLY_BYTES
        };
        if !eof && to_terminal.is_empty() && reply_read_limit != 0 {
            inner_events |= PollFlags::POLLIN;
        }
        if status.is_none() && !eof && !to_shell.is_empty() {
            inner_events |= PollFlags::POLLOUT;
        }
        let timeout = if dirty && to_terminal.is_empty() {
            next_frame
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 50) as u16
        } else {
            50
        };
        let (outer, inner) = {
            let mut fds = vec![PollFd::new(terminal.as_fd(), outer_events)];
            if !inner_events.is_empty() {
                fds.push(PollFd::new(
                    shell.master_fd().expect("PTY stays open during rendering"),
                    inner_events,
                ));
            }
            match poll(&mut fds, timeout) {
                Err(Errno::EINTR) => continue,
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
            (
                fds[0].revents().unwrap_or(PollFlags::empty()),
                fds.get(1)
                    .and_then(PollFd::revents)
                    .unwrap_or(PollFlags::empty()),
            )
        };
        if outer.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "controlling terminal disconnected",
            ));
        }
        if inner.contains(PollFlags::POLLNVAL) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "invalid PTY descriptor",
            ));
        }
        if outer.contains(PollFlags::POLLOUT) {
            send(terminal, &mut to_terminal)?;
        }
        let readable =
            inner.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR);
        if !eof && inner_events.contains(PollFlags::POLLIN) {
            if readable {
                let mut bytes = [0; 8192];
                let read_limit = bytes.len().min(reply_read_limit);
                match shell.read(&mut bytes[..read_limit]) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        parser.advance_with_replies(&mut screen, &bytes[..n], &mut |reply| {
                            if status.is_none() {
                                to_shell.extend(reply);
                            }
                        });
                        debug_assert!(to_shell.len() <= LIMIT);
                        dirty = true;
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                        ) => {}
                    Err(e) => return Err(e),
                }
            } else if status.is_some() {
                // Descendants retaining the slave must not delay direct-child exit.
                eof = true;
            }
            if eof {
                parser.finish(&mut screen);
                dirty = true; // Always flush the final model before normal exit.
                eof_at = Some(Instant::now());
            }
        }
        if !eof && status.is_none() {
            if inner.contains(PollFlags::POLLOUT) {
                send(shell, &mut to_shell)?;
            }
            if outer.contains(PollFlags::POLLIN) && receive(terminal, &mut to_shell)? {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "terminal input ended",
                ));
            }
        }
    }
}

fn receive(reader: &mut impl Read, pending: &mut VecDeque<u8>) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    let capacity = buffer.len().min(LIMIT - pending.len());
    match reader.read(&mut buffer[..capacity]) {
        Ok(0) => Ok(true),
        Ok(n) => {
            pending.extend(&buffer[..n]);
            Ok(false)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

fn send(writer: &mut impl Write, pending: &mut VecDeque<u8>) -> io::Result<()> {
    let bytes = pending.as_slices().0;
    if bytes.is_empty() {
        return Ok(());
    }
    match writer.write(bytes) {
        Ok(0) => Err(io::ErrorKind::WriteZero.into()),
        Ok(n) => {
            pending.drain(..n);
            Ok(())
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_and_screen_limits_reject_without_growing_output() {
        assert!(check_size(256, 256).is_ok());
        assert!(check_size(257, 256).is_err());
        assert!(check_size(0, 80).is_err());
        let mut pending = VecDeque::from(vec![0; MAX_FRAME]);
        assert!(FrameWriter(&mut pending).write(&[1]).is_err());
        assert_eq!(pending.len(), MAX_FRAME);
        assert_eq!(pending.back(), Some(&0));
    }

    #[test]
    fn terminal_modes_restore_when_an_operation_returns_an_error() {
        let pair = nix::pty::openpty(None, None).unwrap();
        let mut original = termios::tcgetattr(&pair.slave).unwrap();
        let observer = pair.slave.try_clone().unwrap();
        let result: io::Result<()> = (|| {
            let terminal = Terminal::enter(pair.slave.into())?;
            assert!(
                !termios::tcgetattr(&terminal.file)?
                    .local_flags
                    .contains(termios::LocalFlags::ICANON)
            );
            Err(io::Error::other("injected operation failure"))
        })();
        assert!(result.is_err());
        let mut restored = termios::tcgetattr(observer).unwrap();
        original.local_flags.remove(termios::LocalFlags::PENDIN);
        restored.local_flags.remove(termios::LocalFlags::PENDIN);
        assert_eq!(restored.input_flags, original.input_flags);
        assert_eq!(restored.output_flags, original.output_flags);
        assert_eq!(restored.control_flags, original.control_flags);
        assert_eq!(restored.local_flags, original.local_flags);
        assert_eq!(restored.control_chars, original.control_chars);
        assert_eq!(
            termios::cfgetispeed(&restored),
            termios::cfgetispeed(&original)
        );
        assert_eq!(
            termios::cfgetospeed(&restored),
            termios::cfgetospeed(&original)
        );
    }

    #[test]
    fn partial_writes_and_retryable_errors_preserve_pending_bytes() {
        struct Sink {
            calls: usize,
            bytes: Vec<u8>,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                match self.calls {
                    1 => Err(io::ErrorKind::Interrupted.into()),
                    2 => Err(io::ErrorKind::WouldBlock.into()),
                    _ => {
                        let count = bytes.len().min(2);
                        self.bytes.extend_from_slice(&bytes[..count]);
                        Ok(count)
                    }
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let original = "中文 input".as_bytes();
        let mut pending = VecDeque::from(original.to_vec());
        let mut sink = Sink {
            calls: 0,
            bytes: Vec::new(),
        };
        for _ in 0..2 {
            send(&mut sink, &mut pending).unwrap();
            assert_eq!(pending.iter().copied().collect::<Vec<_>>(), original);
        }
        while !pending.is_empty() {
            send(&mut sink, &mut pending).unwrap();
        }
        assert_eq!(sink.bytes, original);
    }

    #[test]
    fn reads_obey_queue_capacity_and_distinguish_would_block_from_eof() {
        let mut pending = VecDeque::from(vec![0; LIMIT - 2]);
        let mut input = &b"abc"[..];
        assert!(!receive(&mut input, &mut pending).unwrap());
        assert_eq!(pending.len(), LIMIT);
        assert_eq!(input, b"c");
        struct Paused;
        impl Read for Paused {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        pending.clear();
        assert!(!receive(&mut Paused, &mut pending).unwrap());
        assert!(receive(&mut io::empty(), &mut pending).unwrap());
    }
}
