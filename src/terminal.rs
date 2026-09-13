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
use crate::{
    chrome::{compose, pane_rows},
    layout::{Direction, SplitAxis},
    pane_set::PaneSet,
    pane_view,
    prompt::{EditResult, PromptKind, WindowPrompt},
    render::Renderer,
    screen::Screen,
    window::Windows,
};

// Bound pending keyboard input to 64 KiB; output retains at most one frame.
const POLL_TIMEOUT_MILLIS: u16 = 50;
const MAX_LEGACY_MOUSE_COORDINATE: usize = 223;
const MAX_MOUSE_SEQUENCE_BYTES: usize = 64;
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
        "shell".into(),
        spawn_window(shell_path, pane_rows(size.ws_row), size.ws_col)?,
    )?;
    let signals = Signals::install()?;
    let mut terminal = Terminal::enter(file)?;
    let mut closed = None;
    let result = forward(
        &mut terminal.file,
        &mut windows,
        &signals,
        shell_path,
        size.ws_row,
        &mut closed,
    );
    // Restore the user's terminal before potentially blocking child cleanup.
    let restored = terminal.restore();
    drop(closed);
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
pub(crate) const MAX_WINDOWS: usize = 16;

#[derive(Debug, PartialEq, Eq)]
enum WindowKey {
    Byte(u8),
    Create,
    Next,
    Previous,
    Rename,
    Select(usize),
    Last,
    Close,
    ClosePane,
    MoveLeft,
    MoveRight,
    Split(SplitAxis),
    NextPane,
    ToggleZoom,
    UndoClose,
    History,
    FocusPane(Direction),
    ResizePane(Direction),
    SwapPaneNext,
    SwapPanePrevious,
}

#[derive(Default)]
struct WindowInput {
    prefix: bool,
    paste: bool,
    tail: VecDeque<u8>,
    mouse: Vec<u8>,
    mouse_since: Option<Instant>,
    pane_height: usize,
    pane_top: usize,
    pane_left: usize,
    pane_width: usize,
    mouse_enabled: bool,
}

impl WindowInput {
    // Hold only candidate mouse reports. Escape alone is released after 30ms;
    // completed non-mouse sequences are forwarded as soon as they are known.
    fn feed(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        if self.paste
            || (self.mouse.is_empty() && (!self.mouse_enabled || byte != 27 || self.prefix))
        {
            self.plain(byte, output);
            return;
        }
        self.mouse_since.get_or_insert_with(Instant::now);
        self.mouse.push(byte);
        let len = self.mouse.len();
        let pending = match self.mouse.as_slice() {
            [27] | [27, b'['] => true,
            [27, b'[', b'M', ..] => len < 6,
            [27, b'[', b'<', rest @ ..] => {
                rest.last().is_none_or(|b| !matches!(b, b'M' | b'm'))
                    && len < MAX_MOUSE_SEQUENCE_BYTES
            }
            _ => false,
        };
        if pending {
            return;
        }
        let coordinates = match self.mouse.as_slice() {
            [27, b'[', b'M', _, column, row] => column
                .checked_sub(32)
                .zip(row.checked_sub(32))
                .map(|(x, y)| (usize::from(x), usize::from(y))),
            [27, b'[', b'<', rest @ ..] if matches!(rest.last(), Some(b'M' | b'm')) => {
                std::str::from_utf8(&rest[..rest.len() - 1])
                    .ok()
                    .and_then(|text| {
                        let parts: Vec<_> = text.split(';').collect();
                        if parts.len() == 3
                            && parts
                                .iter()
                                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                        {
                            parts[1]
                                .parse::<usize>()
                                .ok()
                                .zip(parts[2].parse::<usize>().ok())
                        } else {
                            None
                        }
                    })
            }
            _ => None,
        };
        let mut bytes = self.take_mouse();
        if let Some((column, row)) = coordinates {
            let release = (bytes.starts_with(b"\x1b[<") && bytes.last() == Some(&b'm'))
                || (bytes.starts_with(b"\x1b[M")
                    && bytes[3]
                        .checked_sub(32)
                        .is_some_and(|button| button & 0x63 == 3));
            let child_row = row.saturating_sub(self.pane_top);
            let child_column = column.saturating_sub(self.pane_left);
            if (child_row == 0
                || child_row > self.pane_height
                || child_column == 0
                || child_column > self.pane_width)
                && !release
            {
                return;
            }
            // Translate physical coordinates to the active pane. A release outside
            // still ends a drag at the nearest content edge.
            let child_row = child_row.clamp(1, self.pane_height.max(1));
            let child_column = child_column.clamp(1, self.pane_width.max(1));
            if bytes.starts_with(b"\x1b[<") {
                let terminator = *bytes.last().unwrap();
                let separator = bytes.iter().position(|&byte| byte == b';').unwrap();
                bytes.truncate(separator + 1);
                bytes.extend_from_slice(format!("{child_column};{child_row}").as_bytes());
                bytes.push(terminator);
            } else {
                bytes[4] = 32 + child_column.min(MAX_LEGACY_MOUSE_COORDINATE) as u8;
                bytes[5] = 32 + child_row.min(MAX_LEGACY_MOUSE_COORDINATE) as u8;
            }
        }
        for byte in bytes {
            self.plain(byte, output);
        }
    }

    fn mouse_expired(&self) -> bool {
        self.mouse_since
            .is_some_and(|start| start.elapsed() >= Duration::from_millis(30))
    }

    fn take_mouse(&mut self) -> Vec<u8> {
        self.mouse_since = None;
        std::mem::take(&mut self.mouse)
    }

    fn plain(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
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
                b'\t' => output.push(WindowKey::Last),
                b'&' => output.push(WindowKey::Close),
                b'x' => output.push(WindowKey::ClosePane),
                b'<' => output.push(WindowKey::MoveLeft),
                b'>' => output.push(WindowKey::MoveRight),
                b'%' => output.push(WindowKey::Split(SplitAxis::Columns)),
                b'"' => output.push(WindowKey::Split(SplitAxis::Rows)),
                b'{' => output.push(WindowKey::SwapPanePrevious),
                b'}' => output.push(WindowKey::SwapPaneNext),
                b'o' => output.push(WindowKey::NextPane),
                b'Z' => output.push(WindowKey::ToggleZoom),
                b'z' => output.push(WindowKey::UndoClose),
                b'[' => output.push(WindowKey::History),
                8 => output.push(WindowKey::ResizePane(Direction::Left)),
                10 => output.push(WindowKey::ResizePane(Direction::Down)),
                11 => output.push(WindowKey::ResizePane(Direction::Up)),
                12 => output.push(WindowKey::ResizePane(Direction::Right)),
                b'h' => output.push(WindowKey::FocusPane(Direction::Left)),
                b'j' => output.push(WindowKey::FocusPane(Direction::Down)),
                b'k' => output.push(WindowKey::FocusPane(Direction::Up)),
                b'l' => output.push(WindowKey::FocusPane(Direction::Right)),
                b',' => output.push(WindowKey::Rename),
                b'1'..=b'9' => output.push(WindowKey::Select(usize::from(byte - b'1'))),
                b'0' => output.push(WindowKey::Select(9)),
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

fn spawn_window(shell: &OsStr, rows: u16, columns: u16) -> io::Result<PaneSet<Pane>> {
    PaneSet::new(rows, columns, Pane::spawn(shell, rows, columns)?)
}

fn forward(
    terminal: &mut File,
    windows: &mut Windows<PaneSet<Pane>>,
    signals: &Signals,
    shell_path: &OsStr,
    mut outer_rows: u16,
    closed: &mut Option<crate::closed_pane::ClosedPane>,
) -> io::Result<u8> {
    let mut renderer = Renderer::default();
    let mut to_terminal = VecDeque::new();
    let mut input = VecDeque::new();
    let mut keys = WindowInput::default();
    let mut actions = Vec::new();
    let mut next_frame = Instant::now();
    let mut force_redraw = true;
    let mut bar_dirty = false;
    let mut prompt: Option<WindowPrompt> = None;
    let mut history: Option<crate::history_view::HistoryView> = None;
    let mut close_requested = None;
    loop {
        let received = signals.pending.load(Ordering::Relaxed);
        if received != 0 {
            return Ok((128 + received) as u8);
        }
        if let Some(saved) = closed.as_mut()
            && !saved.service()?
        {
            *closed = None;
        }
        if close_requested.is_some() && to_terminal.is_empty() {
            let (id, pane_id) = close_requested.take().unwrap();
            // Finish the already encoded physical frame before changing ownership.
            if windows.get(id).is_some() {
                // A pane request names its stable identity, never a position that
                // could refer to another child after layout changes.
                if let Some(pane_id) = pane_id
                    && windows.get(id).unwrap().content().get(pane_id).is_none()
                {
                    continue;
                }
                if let Some(pane_id) = pane_id {
                    let window = windows.get(id).unwrap();
                    let before = window.content().layout().clone();
                    let name = window.name().to_owned();
                    let sole_pane = window.content().iter().len() == 1;
                    // Keep a visible input target so the last close remains undoable.
                    let replacement = if sole_pane && windows.iter().len() == 1 {
                        let (rows, columns) = before.dimensions();
                        Some(spawn_window(shell_path, rows, columns)?)
                    } else {
                        None
                    };
                    windows
                        .get_mut(id)
                        .unwrap()
                        .content_mut()
                        .get_mut(pane_id)
                        .unwrap()
                        .stop_for_hide()?;
                    if let Some(replacement) = replacement {
                        windows.create("shell".into(), replacement)?;
                    }
                    let (mut pane, after) = if sole_pane {
                        (windows.close(id)?.into_content().into_single(), None)
                    } else {
                        let panes = windows.get_mut(id).unwrap().content_mut();
                        let pane = panes.close(pane_id)?;
                        panes.synchronize_sizes()?;
                        (pane, Some(panes.layout().clone()))
                    };
                    pane.parts_mut().3.to_shell.clear();
                    *closed = Some(crate::closed_pane::ClosedPane {
                        pane: Some(pane),
                        window: id,
                        name,
                        before,
                        after,
                        id: pane_id,
                    });
                } else {
                    if windows.iter().len() == 1 {
                        return Ok(0);
                    }
                    for (_, pane) in windows.get_mut(id).unwrap().content_mut().iter_mut() {
                        pane.shell_mut().terminate()?;
                    }
                    drop(windows.close(id)?);
                }
                bar_dirty = true;
                input.clear();
                keys = WindowInput::default();
                prompt = None;
                renderer.invalidate();
                force_redraw = true;
                continue;
            }
        }
        if prompt
            .as_ref()
            .is_some_and(|prompt| prompt.cancel_due(Instant::now()))
        {
            prompt = None;
            renderer.invalidate();
            force_redraw = true;
        }
        let active = windows.active().expect("at least one window").id();
        let resize = if signals.resize.swap(false, Ordering::Relaxed) {
            let size = window_size(terminal)?;
            if size.ws_row != 0 && size.ws_col != 0 {
                check_size(size.ws_row, size.ws_col)?;
                outer_rows = size.ws_row;
                if history.take().is_some() {
                    input.clear();
                    keys = WindowInput::default();
                }
                renderer.invalidate();
                force_redraw = true;
                Some(size)
            } else {
                None
            }
        } else {
            None
        };
        // Observe exits before preparing resizes, so dead panes need no PTY ioctl.
        for window in windows.iter_mut() {
            for (_, pane) in window.content_mut().iter_mut() {
                let (shell, _, _, state) = pane.parts_mut();
                if state.status.is_none() {
                    state.status = shell.try_wait()?;
                }
            }
        }
        if let Some(size) = resize {
            for window in windows.iter_mut() {
                window
                    .content_mut()
                    .resize(pane_rows(size.ws_row), size.ws_col)?;
            }
            let sizes: Vec<_> = windows
                .iter()
                .map(|window| {
                    let layout = window.content().layout();
                    let mut sizes = layout.tiled_geometry().panes;
                    if layout.is_zoomed() {
                        let visible = layout.geometry().panes[0];
                        *sizes.iter_mut().find(|(id, _)| *id == visible.0).unwrap() = visible;
                    }
                    (window.id(), sizes)
                })
                .collect();
            // Prepare all destinations across all windows before changing any PTY.
            let mut prepared = Vec::new();
            for window in windows.iter_mut() {
                let rectangles = &sizes.iter().find(|(id, _)| *id == window.id()).unwrap().1;
                for (pane_id, pane) in window.content_mut().iter_mut() {
                    let rect = rectangles.iter().find(|(id, _)| *id == pane_id).unwrap().1;
                    prepared.push(pane.prepare_resize(rect.rows, rect.columns)?);
                }
            }
            for resize in prepared {
                resize.commit()?;
            }
        }
        let names: Vec<_> = windows
            .iter()
            .map(|window| window.name().to_owned())
            .collect();
        let active_index = windows
            .iter()
            .position(|window| window.id() == active)
            .unwrap();
        let mut active_paused = false;
        let mut finished = Vec::new();
        for window in windows.iter_mut() {
            let id = window.id();
            let panes = window.content_mut();
            let zoomed = panes.layout().is_zoomed();
            let focused = panes.layout().active();
            let mut paused = false;
            let mut eof = false;
            let mut dirty = false;
            for (pane_id, pane) in panes.iter_mut() {
                let (_, _, screen, state) = pane.parts_mut();
                let pane_paused = synchronized_pause(
                    screen,
                    &mut state.synchronized_since,
                    Instant::now(),
                    state.eof,
                );
                if (!zoomed || pane_id == focused)
                    && !(id == active && pane_id == focused && history.is_some())
                {
                    paused |= pane_paused;
                    eof |= state.eof;
                    dirty |= state.dirty;
                }
            }
            if id == active {
                if panes.active().io().eof && (prompt.is_some() || history.is_some()) {
                    history = None;
                    prompt = None;
                    renderer.invalidate();
                    force_redraw = true;
                }
                active_paused = paused;
                if close_requested.is_none()
                    && (dirty || force_redraw || bar_dirty)
                    && (!paused || force_redraw)
                    && to_terminal.is_empty()
                    && (eof || force_redraw || Instant::now() >= next_frame)
                {
                    let historical = history.as_ref().map(|view| view.render()).transpose()?;
                    let screens: Vec<_> = panes
                        .iter()
                        .map(|(pane_id, pane)| {
                            let screen = if pane_id == focused {
                                historical.as_ref().unwrap_or(pane.screen())
                            } else {
                                pane.screen()
                            };
                            (pane_id, screen)
                        })
                        .collect();
                    let content = pane_view::compose(panes.layout(), &screens)?;
                    let mut view = compose(&content, outer_rows, &names, active_index)?;
                    if let Some(history) = &history
                        && outer_rows > 1
                    {
                        crate::chrome::prepare_row(&mut view, crate::chrome::bar_style(true));
                        for character in crate::chrome::clipped(
                            &history.label(view.dimensions().1),
                            view.dimensions().1,
                        )
                        .chars()
                        {
                            view.print(character);
                        }
                    }
                    if let Some(prompt) = &prompt {
                        renderer
                            .render(&prompt.overlay(&view), &mut FrameWriter(&mut to_terminal))?;
                    } else {
                        renderer.render(&view, &mut FrameWriter(&mut to_terminal))?;
                    }
                    for (pane_id, pane) in panes.iter_mut() {
                        if !zoomed || pane_id == focused {
                            pane.parts_mut().3.dirty = false;
                        }
                    }
                    bar_dirty = false;
                    force_redraw = false;
                    next_frame = Instant::now() + FRAME_INTERVAL;
                }
            }
            for (pane_id, pane) in panes.iter() {
                let state = pane.io();
                if state.eof {
                    if let Some(status) = state.status {
                        if id != active
                            || (zoomed && pane_id != focused)
                            || (!state.dirty && to_terminal.is_empty())
                        {
                            finished.push((id, pane_id, exit_code(status)));
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
        }
        if !finished.is_empty() {
            bar_dirty = true;
            for (id, pane_id, code) in finished {
                let was_active = windows.active().unwrap().id() == id;
                let panes = windows
                    .get_mut(id)
                    .expect("finished pane owns a window")
                    .content_mut();
                let was_focused = panes.layout().active() == pane_id;
                if panes.iter().len() == 1 {
                    if windows.iter().len() == 1 {
                        return Ok(code);
                    }
                    drop(windows.close(id)?);
                } else {
                    drop(panes.close(pane_id)?);
                    panes.synchronize_sizes()?;
                }
                if was_active {
                    if history.take().is_some() {
                        input.clear();
                        keys = WindowInput::default();
                    }
                    if was_focused {
                        input.clear();
                        keys = WindowInput::default();
                        prompt = None;
                    }
                    renderer.invalidate();
                    force_redraw = true;
                }
            }
            continue;
        }
        // A lone Escape or incomplete report must not remain held indefinitely.
        if prompt.is_none() && keys.mouse_expired() {
            let pane = windows.active_mut().unwrap().content_mut().active_mut();
            let (_, _, _, state) = pane.parts_mut();
            if state.accepts_input() && state.to_shell.len() <= LIMIT - 64 {
                state.to_shell.extend(keys.take_mouse());
            }
        }
        // Decode in input order. Bytes preceding a switch remain queued for the
        // old child; following bytes target the newly selected one.
        while close_requested.is_none() && !input.is_empty() {
            if let Some(view) = &mut history {
                if view.feed(input.pop_front().unwrap()) {
                    history = None;
                    keys = WindowInput::default();
                }
                force_redraw = true;
                continue;
            }
            if let Some(editor) = &mut prompt {
                let result = editor.feed(input.pop_front().unwrap(), Instant::now());
                match result {
                    EditResult::Save => {
                        match editor.kind {
                            PromptKind::Rename => {
                                let name = editor.text.clone();
                                windows.rename(windows.active().unwrap().id(), name)?;
                            }
                            PromptKind::Close if editor.text == "yes" => {
                                close_requested = Some((windows.active().unwrap().id(), None));
                            }
                            PromptKind::ClosePane if editor.text == "yes" => {
                                let window = windows.active().unwrap();
                                close_requested =
                                    Some((window.id(), Some(window.content().layout().active())));
                            }
                            PromptKind::Close | PromptKind::ClosePane => {}
                        }
                        prompt = None;
                        keys = WindowInput::default();
                        renderer.invalidate();
                    }
                    EditResult::Cancel => {
                        prompt = None;
                        keys = WindowInput::default();
                        renderer.invalidate();
                    }
                    EditResult::Continue => {}
                }
                force_redraw = true;
                continue;
            }
            let pane = windows.active().unwrap().content().active();
            if !pane.io().accepts_input() || pane.io().to_shell.len() > LIMIT - 64 {
                break;
            }
            keys.pane_height = pane.screen().dimensions().0;
            let set = windows.active().unwrap().content();
            let rect = set
                .layout()
                .geometry()
                .panes
                .into_iter()
                .find(|(id, _)| *id == set.layout().active())
                .unwrap()
                .1;
            keys.pane_top = usize::from(outer_rows > 1) + usize::from(rect.row);
            keys.pane_left = usize::from(rect.column);
            keys.pane_width = usize::from(rect.columns);
            keys.mouse_enabled =
                pane.screen().mouse_tracking() != crate::screen::MouseTracking::Off;
            actions.clear();
            keys.feed(input.pop_front().unwrap(), &mut actions);
            for action in actions.drain(..) {
                let old = (
                    windows.active().unwrap().id(),
                    windows.active().unwrap().content().layout().active(),
                );
                match action {
                    WindowKey::Byte(byte) => windows
                        .active_mut()
                        .unwrap()
                        .content_mut()
                        .active_mut()
                        .parts_mut()
                        .3
                        .to_shell
                        .push_back(byte),
                    WindowKey::Split(axis) => {
                        let panes = windows.active_mut().unwrap().content_mut();
                        match panes.split_with(axis, |_, rect| {
                            Pane::spawn(shell_path, rect.rows, rect.columns)
                        }) {
                            Ok(_) => panes.synchronize_sizes()?,
                            Err(_) => {
                                if to_terminal.is_empty() {
                                    to_terminal.push_back(7);
                                }
                            }
                        }
                    }
                    WindowKey::SwapPaneNext | WindowKey::SwapPanePrevious => {
                        let panes = windows.active_mut().unwrap().content_mut();
                        let changed = if action == WindowKey::SwapPaneNext {
                            panes.swap_active_next()
                        } else {
                            panes.swap_active_previous()
                        };
                        if changed {
                            panes.synchronize_sizes()?;
                            renderer.invalidate();
                            force_redraw = true;
                        }
                    }
                    WindowKey::ResizePane(direction) => {
                        let panes = windows.active_mut().unwrap().content_mut();
                        if panes.resize_active(direction) {
                            panes.synchronize_sizes()?;
                            renderer.invalidate();
                            force_redraw = true;
                        }
                    }
                    WindowKey::History => {
                        history = crate::history_view::HistoryView::new(
                            windows.active().unwrap().content().active().screen(),
                        );
                        keys = WindowInput::default();
                        if history.is_some() {
                            renderer.invalidate();
                            force_redraw = true;
                        }
                    }
                    WindowKey::UndoClose => {
                        if let Some(mut saved) = closed.take() {
                            if let Err(error) = saved.restore(windows) {
                                if saved.pane.is_none() {
                                    return Err(error);
                                }
                                *closed = Some(saved);
                                if to_terminal.is_empty() {
                                    to_terminal.push_back(7);
                                }
                            } else {
                                renderer.invalidate();
                                force_redraw = true;
                            }
                        }
                    }
                    WindowKey::ToggleZoom => {
                        let panes = windows.active_mut().unwrap().content_mut();
                        let was_zoomed = panes.layout().is_zoomed();
                        if panes.toggle_zoom() != was_zoomed {
                            panes.synchronize_sizes()?;
                            renderer.invalidate();
                            force_redraw = true;
                        }
                    }
                    WindowKey::NextPane => {
                        let panes = windows.active_mut().unwrap().content_mut();
                        let ids: Vec<_> = panes
                            .layout()
                            .tiled_geometry()
                            .panes
                            .into_iter()
                            .map(|(id, _)| id)
                            .collect();
                        let index = ids
                            .iter()
                            .position(|id| *id == panes.layout().active())
                            .unwrap();
                        panes.select(ids[(index + 1) % ids.len()])?;
                    }
                    WindowKey::FocusPane(direction) => {
                        windows
                            .active_mut()
                            .unwrap()
                            .content_mut()
                            .select_direction(direction);
                    }
                    WindowKey::ClosePane => {
                        prompt = Some(WindowPrompt::close_pane());
                        renderer.invalidate();
                        force_redraw = true;
                    }
                    WindowKey::Close => {
                        prompt = Some(WindowPrompt::close());
                        renderer.invalidate();
                        force_redraw = true;
                    }
                    WindowKey::Rename => {
                        prompt = Some(WindowPrompt::new(windows.active().unwrap().name()));
                        renderer.invalidate();
                        force_redraw = true;
                    }
                    WindowKey::Select(position) => {
                        // Bar numbers are current positions, not stable WindowIds.
                        let target = windows.iter().nth(position).map(|window| window.id());
                        if let Some(id) = target {
                            windows.select(id)?;
                        }
                    }
                    WindowKey::MoveLeft => {
                        bar_dirty |= windows.move_active_left();
                    }
                    WindowKey::MoveRight => {
                        bar_dirty |= windows.move_active_right();
                    }
                    WindowKey::Last => {
                        windows.select_last();
                    }
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
                            windows.active().unwrap().content().layout().dimensions();
                        match spawn_window(shell_path, rows, columns) {
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
                if (
                    windows.active().unwrap().id(),
                    windows.active().unwrap().content().layout().active(),
                ) != old
                {
                    windows
                        .active_mut()
                        .unwrap()
                        .content_mut()
                        .synchronize_sizes()?;
                    renderer.invalidate();
                    force_redraw = true;
                }
            }
        }
        // A changed focus needs a frame before returning to a blocking poll.
        if (force_redraw || close_requested.is_some()) && to_terminal.is_empty() {
            continue;
        }
        let active = windows.active().unwrap().id();
        let active_set = windows.active().unwrap().content();
        let active_dirty = active_set.iter().any(|(id, pane)| {
            (!active_set.layout().is_zoomed() || id == active_set.layout().active())
                && !(history.is_some() && id == active_set.layout().active())
                && pane.io().dirty
        });
        let mut outer_events = PollFlags::empty();
        if input.len() < LIMIT {
            outer_events |= PollFlags::POLLIN;
        }
        if !to_terminal.is_empty() {
            outer_events |= PollFlags::POLLOUT;
        }
        let timeout = if (active_dirty || bar_dirty) && !active_paused && to_terminal.is_empty() {
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
                for (pane_id, pane) in window.content().iter() {
                    let state = pane.io();
                    let mut flags = PollFlags::empty();
                    if state.reply_read_limit() != 0
                        && (window.id() != active || to_terminal.is_empty())
                    {
                        flags |= PollFlags::POLLIN;
                    }
                    if !state.eof && state.status.is_none() && !state.to_shell.is_empty() {
                        flags |= PollFlags::POLLOUT;
                    }
                    if !flags.is_empty() {
                        // Pane IDs are collection-local; retain the owning window ID too.
                        interests.push((window.id(), pane_id, flags));
                        fds.push(PollFd::new(
                            pane.shell().master_fd().expect("live PTY"),
                            flags,
                        ));
                    }
                }
            }
            // Wake immediately for hidden PTY traffic too. Its bounded I/O runs
            // at the top of the next loop and does not schedule a visible frame.
            if let Some(saved) = closed.as_ref() {
                let pane = saved.pane.as_ref().unwrap();
                let mut flags = PollFlags::empty();
                if pane.io().reply_read_limit() > 0 {
                    flags |= PollFlags::POLLIN;
                }
                if !pane.io().to_shell.is_empty() {
                    flags |= PollFlags::POLLOUT;
                }
                if !flags.is_empty() {
                    fds.push(PollFd::new(pane.shell().master_fd().unwrap(), flags));
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
                    .take(interests.len()) // Hidden readiness is serviced on the next turn.
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
        for ((id, pane_id, inner_events), inner) in interests.into_iter().zip(events) {
            if inner.contains(PollFlags::POLLNVAL) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "invalid PTY descriptor",
                ));
            }
            let (shell, parser, screen, state) = windows
                .get_mut(id)
                .unwrap()
                .content_mut()
                .get_mut(pane_id)
                .expect("polled pane exists")
                .parts_mut();
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
            decode(b"a\x02c\x02n\x02p\x02\t\x02&\x02<\x02>\x02\x02\x02q"),
            vec![
                WindowKey::Byte(b'a'),
                WindowKey::Create,
                WindowKey::Next,
                WindowKey::Previous,
                WindowKey::Last,
                WindowKey::Close,
                WindowKey::MoveLeft,
                WindowKey::MoveRight,
                WindowKey::Byte(2),
                WindowKey::Byte(2),
                WindowKey::Byte(b'q')
            ]
        );
    }

    #[test]
    fn split_and_pane_focus_shortcuts_are_decoded() {
        assert_eq!(
            decode(b"\x02%\x02\"\x02o\x02Z\x02[\x02h\x02j\x02k\x02l"),
            vec![
                WindowKey::Split(SplitAxis::Columns),
                WindowKey::Split(SplitAxis::Rows),
                WindowKey::NextPane,
                WindowKey::ToggleZoom,
                WindowKey::History,
                WindowKey::FocusPane(Direction::Left),
                WindowKey::FocusPane(Direction::Down),
                WindowKey::FocusPane(Direction::Up),
                WindowKey::FocusPane(Direction::Right),
            ]
        );
    }

    #[test]
    fn manual_resize_shortcuts_require_prefix_and_respect_paste() {
        let keys = b"\x02\x08\x02\x0a\x02\x0b\x02\x0c";
        assert_eq!(
            decode(keys),
            vec![
                WindowKey::ResizePane(Direction::Left),
                WindowKey::ResizePane(Direction::Down),
                WindowKey::ResizePane(Direction::Up),
                WindowKey::ResizePane(Direction::Right),
            ]
        );
        let plain = b"\x08\x0a\x0b\x0c";
        assert_eq!(
            decode(plain),
            plain
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
        let mut pasted = b"\x1b[200~".to_vec();
        pasted.extend_from_slice(keys);
        pasted.extend_from_slice(b"\x1b[201~");
        assert_eq!(
            decode(&pasted),
            pasted
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn swap_shortcuts_are_local_only_outside_bracketed_paste() {
        assert_eq!(
            decode(b"{}\x02{\x02}"),
            vec![
                WindowKey::Byte(b'{'),
                WindowKey::Byte(b'}'),
                WindowKey::SwapPanePrevious,
                WindowKey::SwapPaneNext,
            ]
        );
        let pasted = b"\x1b[200~\x02{\x02}\x1b[201~";
        assert_eq!(
            decode(pasted),
            pasted
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pane_close_shortcut_requires_prefix_and_is_ignored_in_paste() {
        assert_eq!(
            decode(b"x\x02x"),
            vec![WindowKey::Byte(b'x'), WindowKey::ClosePane]
        );
        let pasted = b"\x1b[200~\x02xyes\r\x1b[201~";
        assert_eq!(
            decode(pasted),
            pasted
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn undo_and_zoom_use_distinct_keys_and_paste_never_invokes_them() {
        assert_eq!(
            decode(b"\x02z\x02Z"),
            vec![WindowKey::UndoClose, WindowKey::ToggleZoom]
        );
        let bytes = b"\x1b[200~\x02z\x02Z\x1b[201~";
        assert_eq!(
            decode(bytes),
            bytes
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn numeric_shortcuts_consume_only_the_prefixed_digit() {
        for (digit, position) in (b'1'..=b'9').zip(0..9).chain([(b'0', 9)]) {
            assert_eq!(
                decode(&[digit, 2, digit, b'x']),
                vec![
                    WindowKey::Byte(digit),
                    WindowKey::Select(position),
                    WindowKey::Byte(b'x')
                ]
            );
        }
    }

    #[test]
    fn bracketed_paste_and_utf8_are_forwarded_byte_for_byte() {
        let bytes = "\x1b[200~中文\x02c\x02n\x02p\x021\x020\x02\t\x02&\x02<\x02>\x02%\x02o\x02Z\x02[\x02h\x02j\x02k\x02l\x02\x02\x1b[201~"
            .as_bytes();
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
        let mut keys = WindowInput {
            mouse_enabled: true,
            ..WindowInput::default()
        };
        keys.feed(27, &mut Vec::new());
        assert_eq!(keys.take_mouse(), vec![27]);
    }
    #[test]
    fn hidden_bar_keeps_one_row_mouse_coordinates() {
        let mut keys = WindowInput {
            pane_height: 1,
            pane_width: 80,
            pane_top: 0,
            mouse_enabled: true,
            ..WindowInput::default()
        };
        let bytes = b"\x1b[<0;2;1M\x1b[<0;2;1m\x1b[M !!\x1b[M#!!";
        let mut output = Vec::new();
        for &byte in bytes {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            bytes
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn bar_mouse_presses_are_ignored_but_releases_finish_child_drags() {
        let mut keys = WindowInput {
            pane_height: 23,
            pane_width: 80,
            pane_top: 1,
            mouse_enabled: true,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<0;2;1M\x1b[<0;2;1m\x1b[<0;2;24M\x1b[M !!\x1b[M#!!\x1b[M !8" {
            keys.feed(byte, &mut output);
        }
        let expected = b"\x1b[<0;2;1m\x1b[<0;2;23M\x1b[M#!!\x1b[M !7";
        assert_eq!(
            output,
            expected
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
        assert_eq!(decode(b"\x1b"), vec![WindowKey::Byte(27)]);
    }
}
