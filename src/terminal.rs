//! Multi-window PTY polling, prefix input and model-based terminal rendering.

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
use nix::poll::{PollFd, PollFlags, poll};
use nix::sys::termios::{self, SetArg, Termios};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};

use crate::pane::{INPUT_LIMIT as LIMIT, MAX_CELLS, Pane};
use crate::{render::Renderer, screen::Screen, window::Windows};

// Bound pending keyboard input to 64 KiB; output retains at most one frame.
const POLL_TIMEOUT_MILLIS: u16 = 50;
const MAX_FRAME: usize = 16 * 1024 * 1024;
const SYNC_TIMEOUT: Duration = Duration::from_secs(1);
const FRAME_INTERVAL: Duration = Duration::from_millis(6);

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
    let mut windows = Windows::default();
    windows.create(
        "1".into(),
        Pane::spawn(shell_path, size.ws_row, size.ws_col)?,
    )?;
    let signals = Signals::install()?;
    let mut terminal = Terminal::enter(file)?;
    let result = forward(&mut terminal.file, &mut windows, &signals, shell_path);
    // Restore the user's terminal before potentially blocking child cleanup.
    let restored = terminal.restore();
    drop(windows);
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

// Bound an observed synchronized batch even if the child stalls or repeats h.
// Timeout resets the model mode, so subsequent queries report ordinary output.
fn synchronized_pause(
    screen: &mut Screen,
    since: &mut Option<Instant>,
    now: Instant,
    eof: bool,
) -> bool {
    if eof || !screen.synchronized_output() {
        *since = None;
        if eof {
            screen.set_synchronized_output(false);
        }
        return false;
    }
    let started = *since.get_or_insert(now);
    if now.saturating_duration_since(started) >= SYNC_TIMEOUT {
        screen.set_synchronized_output(false);
        *since = None;
        false
    } else {
        true
    }
}

// Bound total resident grids and descriptors before starting another process.
const MAX_WINDOWS: usize = 16;

#[derive(Debug, PartialEq, Eq)]
enum WindowKey {
    Byte(u8),
    Create,
    Next,
    Previous,
}

#[derive(Default)]
struct WindowInput {
    prefix: bool,
    paste: bool,
    tail: VecDeque<u8>,
}

impl WindowInput {
    fn feed(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        self.tail.push_back(byte);
        if self.tail.len() > 6 {
            self.tail.pop_front();
        }
        let was_paste = self.paste;
        if self.tail.iter().copied().eq(b"\x1b[200~".iter().copied()) {
            self.paste = true;
        }
        if self.tail.iter().copied().eq(b"\x1b[201~".iter().copied()) {
            self.paste = false;
        }
        if was_paste {
            output.push(WindowKey::Byte(byte));
        } else if self.prefix {
            self.prefix = false;
            match byte {
                b'c' => output.push(WindowKey::Create),
                b'n' => output.push(WindowKey::Next),
                b'p' => output.push(WindowKey::Previous),
                2 => output.push(WindowKey::Byte(2)),
                _ => {
                    output.push(WindowKey::Byte(2));
                    output.push(WindowKey::Byte(byte));
                }
            }
        } else if byte == 2 {
            self.prefix = true;
        } else {
            output.push(WindowKey::Byte(byte));
        }
    }
}

fn forward(
    terminal: &mut File,
    windows: &mut Windows<Pane>,
    signals: &Signals,
    shell_path: &OsStr,
) -> io::Result<u8> {
    let mut renderer = Renderer::default();
    let mut to_terminal = VecDeque::new();
    let mut input = VecDeque::new();
    let mut keys = WindowInput::default();
    let mut actions = Vec::new();
    let mut next_frame = Instant::now();
    let mut force_redraw = true;
    loop {
        let received = signals.pending.load(Ordering::Relaxed);
        if received != 0 {
            return Ok((128 + received) as u8);
        }
        let active = windows.active().expect("at least one window").id();
        let resize = if signals.resize.swap(false, Ordering::Relaxed) {
            let size = window_size(terminal)?;
            if size.ws_row != 0 && size.ws_col != 0 {
                check_size(size.ws_row, size.ws_col)?;
                renderer.invalidate();
                force_redraw = true;
                Some(size)
            } else {
                None
            }
        } else {
            None
        };
        let mut active_paused = false;
        let mut finished = Vec::new();
        for window in windows.iter_mut() {
            let id = window.id();
            let (shell, _, screen, state) = window.content_mut().parts_mut();
            if state.status.is_none() {
                state.status = shell.try_wait()?;
            }
            if let Some(size) = resize {
                screen.resize(usize::from(size.ws_row), usize::from(size.ws_col))?;
                screen.set_synchronized_output(false);
                state.synchronized_since = None;
                if state.status.is_none() && !state.eof {
                    shell.resize(size.ws_row, size.ws_col)?;
                }
                state.dirty = true;
            }
            let paused = synchronized_pause(
                screen,
                &mut state.synchronized_since,
                Instant::now(),
                state.eof,
            );
            if id == active {
                active_paused = paused;
                if (state.dirty || force_redraw)
                    && (!paused || force_redraw)
                    && to_terminal.is_empty()
                    && (state.eof || force_redraw || Instant::now() >= next_frame)
                {
                    renderer.render(screen, &mut FrameWriter(&mut to_terminal))?;
                    state.dirty = false;
                    force_redraw = false;
                    next_frame = Instant::now() + FRAME_INTERVAL;
                }
            }
            if state.eof {
                if let Some(status) = state.status {
                    if id != active || (!state.dirty && to_terminal.is_empty()) {
                        finished.push((id, exit_code(status)));
                    }
                } else if state
                    .eof_at
                    .is_some_and(|time| time.elapsed() > Duration::from_secs(1))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "shell kept running after PTY closed",
                    ));
                }
            }
        }
        if !finished.is_empty() {
            for (id, code) in finished {
                if windows.iter().len() == 1 {
                    return Ok(code);
                }
                let was_active = windows.active().unwrap().id() == id;
                drop(windows.close(id)?);
                if was_active {
                    // Do not deliver pending keystrokes from a dead window to its successor.
                    input.clear();
                    keys = WindowInput::default();
                    renderer.invalidate();
                    force_redraw = true;
                }
            }
            continue;
        }
        // Decode in input order. Bytes preceding a switch remain queued for the
        // old child; following bytes target the newly selected one.
        while !input.is_empty() {
            let pane = windows.active().unwrap().content();
            if !pane.io().accepts_input() || pane.io().to_shell.len() > LIMIT - 2 {
                break;
            }
            actions.clear();
            keys.feed(input.pop_front().unwrap(), &mut actions);
            for action in actions.drain(..) {
                let old = windows.active().unwrap().id();
                match action {
                    WindowKey::Byte(byte) => windows
                        .active_mut()
                        .unwrap()
                        .content_mut()
                        .parts_mut()
                        .3
                        .to_shell
                        .push_back(byte),
                    WindowKey::Next => {
                        windows.select_next();
                    }
                    WindowKey::Previous => {
                        windows.select_previous();
                    }
                    WindowKey::Create => {
                        if windows.iter().len() == MAX_WINDOWS {
                            if to_terminal.is_empty() {
                                to_terminal.push_back(7);
                            }
                            continue;
                        }
                        let (rows, columns) =
                            windows.active().unwrap().content().screen().dimensions();
                        match Pane::spawn(shell_path, rows as u16, columns as u16) {
                            Ok(pane) => {
                                windows.create("shell".into(), pane)?;
                            }
                            Err(_) => {
                                if to_terminal.is_empty() {
                                    to_terminal.push_back(7);
                                }
                            }
                        }
                    }
                }
                if windows.active().unwrap().id() != old {
                    renderer.invalidate();
                    force_redraw = true;
                }
            }
        }
        // A changed focus needs a frame before returning to a blocking poll.
        if force_redraw && to_terminal.is_empty() {
            continue;
        }
        let active = windows.active().unwrap().id();
        let active_io = windows.active().unwrap().content().io();
        let mut outer_events = PollFlags::empty();
        if input.len() < LIMIT {
            outer_events |= PollFlags::POLLIN;
        }
        if !to_terminal.is_empty() {
            outer_events |= PollFlags::POLLOUT;
        }
        let timeout = if active_io.dirty && !active_paused && to_terminal.is_empty() {
            next_frame
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 50) as u16
        } else {
            50
        };
        let mut interests = Vec::new();
        let (outer, events) = {
            let mut fds = vec![PollFd::new(terminal.as_fd(), outer_events)];
            for window in windows.iter() {
                let pane = window.content();
                let state = pane.io();
                let mut flags = PollFlags::empty();
                // Background grids do not produce physical frames; keep draining them.
                if state.reply_read_limit() != 0
                    && (window.id() != active || to_terminal.is_empty())
                {
                    flags |= PollFlags::POLLIN;
                }
                if !state.eof && state.status.is_none() && !state.to_shell.is_empty() {
                    flags |= PollFlags::POLLOUT;
                }
                if !flags.is_empty() {
                    interests.push((window.id(), flags));
                    fds.push(PollFd::new(
                        pane.shell().master_fd().expect("live PTY"),
                        flags,
                    ));
                }
            }
            match poll(&mut fds, timeout) {
                Err(Errno::EINTR) => continue,
                Err(e) => return Err(e.into()),
                Ok(_) => {}
            }
            (
                fds[0].revents().unwrap_or(PollFlags::empty()),
                fds[1..]
                    .iter()
                    .map(|fd| fd.revents().unwrap_or(PollFlags::empty()))
                    .collect::<Vec<_>>(),
            )
        };
        if outer.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "controlling terminal disconnected",
            ));
        }
        if outer.contains(PollFlags::POLLOUT) {
            send(terminal, &mut to_terminal)?;
        }
        // One bounded read/write per pane per iteration prevents a busy background
        // process from starving the other panes, keyboard or signal handling.
        for ((id, inner_events), inner) in interests.into_iter().zip(events) {
            if inner.contains(PollFlags::POLLNVAL) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "invalid PTY descriptor",
                ));
            }
            let (shell, parser, screen, state) =
                windows.get_mut(id).unwrap().content_mut().parts_mut();
            let reply_read_limit = state.reply_read_limit();
            let readable =
                inner.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR);
            if !state.eof && inner_events.contains(PollFlags::POLLIN) {
                if readable {
                    let mut bytes = [0; 8192];
                    let read_limit = bytes.len().min(reply_read_limit);
                    match shell.read(&mut bytes[..read_limit]) {
                        Ok(0) => state.eof = true,
                        Ok(n) => {
                            parser.advance_with_replies(screen, &bytes[..n], &mut |reply| {
                                if state.status.is_none() {
                                    state.to_shell.extend(reply);
                                }
                            });
                            debug_assert!(state.to_shell.len() <= LIMIT);
                            state.dirty = true;
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                            ) => {}
                        Err(e) => return Err(e),
                    }
                } else if state.status.is_some() {
                    // Descendants retaining the slave must not delay direct-child exit.
                    state.eof = true;
                }
                if state.eof {
                    parser.finish(screen);
                    state.dirty = true; // Always flush the final model before normal exit.
                    state.eof_at = Some(Instant::now());
                }
            }
            if !state.eof && state.status.is_none() && inner.contains(PollFlags::POLLOUT) {
                send(shell, &mut state.to_shell)?;
            }
        }
        if outer.contains(PollFlags::POLLIN) && receive(terminal, &mut input)? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal input ended",
            ));
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

#[cfg(test)]
mod synchronized_tests {
    use super::*;

    #[test]
    fn timeout_repeated_enable_and_eof_release_pending_frames() {
        let mut screen = Screen::new(2, 8).unwrap();
        let mut since = None;
        let now = Instant::now();
        assert!(!synchronized_pause(&mut screen, &mut since, now, false));
        screen.set_synchronized_output(true);
        assert!(synchronized_pause(&mut screen, &mut since, now, false));
        screen.set_synchronized_output(true);
        assert!(synchronized_pause(
            &mut screen,
            &mut since,
            now + SYNC_TIMEOUT / 2,
            false
        ));
        assert!(!synchronized_pause(
            &mut screen,
            &mut since,
            now + SYNC_TIMEOUT,
            false
        ));
        assert!(!screen.synchronized_output());
        screen.set_synchronized_output(true);
        assert!(synchronized_pause(
            &mut screen,
            &mut since,
            now + SYNC_TIMEOUT,
            false
        ));
        assert!(!synchronized_pause(
            &mut screen,
            &mut since,
            now + SYNC_TIMEOUT,
            true
        ));
        assert!(!screen.synchronized_output());
    }

    #[test]
    fn explicit_end_allows_a_new_batch() {
        let mut screen = Screen::new(2, 8).unwrap();
        let mut since = None;
        let now = Instant::now();
        screen.set_synchronized_output(true);
        assert!(synchronized_pause(&mut screen, &mut since, now, false));
        screen.set_synchronized_output(false);
        assert!(!synchronized_pause(&mut screen, &mut since, now, false));
        screen.set_synchronized_output(true);
        assert!(synchronized_pause(
            &mut screen,
            &mut since,
            now + SYNC_TIMEOUT,
            false
        ));
    }
}

#[cfg(test)]
mod window_input_tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<WindowKey> {
        let mut decoder = WindowInput::default();
        let mut result = Vec::new();
        for &byte in bytes {
            decoder.feed(byte, &mut result);
        }
        result
    }

    #[test]
    fn prefix_commands_literal_prefix_and_unknown_keys() {
        assert_eq!(
            decode(b"a\x02c\x02n\x02p\x02\x02\x02z"),
            vec![
                WindowKey::Byte(b'a'),
                WindowKey::Create,
                WindowKey::Next,
                WindowKey::Previous,
                WindowKey::Byte(2),
                WindowKey::Byte(2),
                WindowKey::Byte(b'z')
            ]
        );
    }

    #[test]
    fn bracketed_paste_and_utf8_are_forwarded_byte_for_byte() {
        let bytes = "\x1b[200~中文\x02c\x02n\x02p\x02\x02\x1b[201~".as_bytes();
        assert_eq!(
            decode(bytes),
            bytes
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
        let mut input = bytes.to_vec();
        input.extend(b"\x02c");
        assert_eq!(decode(&input).last(), Some(&WindowKey::Create));
        assert_eq!(decode(b"\x1b"), vec![WindowKey::Byte(27)]);
    }
}
