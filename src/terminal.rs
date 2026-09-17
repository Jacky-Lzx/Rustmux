//! Multi-window PTY polling, prefix input and model-based terminal rendering.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, poll};
#[cfg(test)]
use nix::sys::termios;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};

use crate::pane::{INPUT_LIMIT as LIMIT, MAX_CELLS, Pane};
use crate::{
    chrome::{compose, pane_rows},
    layout::{Direction, PaneId, Rect, SplitAxis},
    pane_set::PaneSet,
    pane_view,
    prompt::{EditResult, PromptKind, WindowPrompt},
    render::Renderer,
    screen::{MouseTracking, Screen},
    session::{
        SessionEndpoint,
        frontend::{ConnectionState, ServerFrontend},
        handshake::{self, ServerPeer},
    },
    terminal_device::{TerminalDevice, window_size},
    window::{WindowId, Windows},
};

// Bound pending keyboard input to 64 KiB; output retains at most one frame.
const POLL_TIMEOUT_MILLIS: u16 = 50;
const MAX_LEGACY_MOUSE_COORDINATE: usize = 223;
const MAX_MOUSE_SEQUENCE_BYTES: usize = 64;
const MAX_FRAME: usize = 16 * 1024 * 1024;
const SYNC_TIMEOUT: Duration = Duration::from_secs(1);
const FRAME_INTERVAL: Duration = Duration::from_millis(6);
const PANE_DRAG_RESIZE_INTERVAL: Duration = Duration::from_millis(33);

/// Run on the controlling terminal during single-threaded program startup.
/// Returns the shell exit code, or 128 + signal for termination by signal.
/// Input and output must be terminals. Raw mode is restored before returning.
pub fn run(shell_path: &OsStr) -> io::Result<u8> {
    let file = TerminalDevice::open_controlling()?;
    let size = window_size(&file)?;
    // Start the shell before changing the outer terminal, so exec failures
    // cannot leave it raw. Signal registration below creates no worker threads.
    let mut session = TerminalSession::new(shell_path, size.ws_row, size.ws_col, None)?;
    let signals = Signals::install()?;
    let mut terminal = LocalFrontend::enter(file, signals.resize.clone())?;
    let result = session.attach(&mut terminal, &signals);
    // Restore the user's terminal before potentially blocking child cleanup.
    let restored = terminal.restore();
    drop(session);
    match result {
        Err(error) => Err(error),
        Ok(ForwardExit::Process(code)) => restored.map(|()| code),
        Ok(ForwardExit::Detached | ForwardExit::Disconnected) => {
            restored?;
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal input ended",
            ))
        }
    }
}

/// Run one persistent session, preserving panes while clients detach and reconnect.
pub fn serve_session(
    shell_path: &OsStr,
    name: &crate::session::SessionName,
    endpoint: &SessionEndpoint,
    mut peer: ServerPeer,
) -> io::Result<u8> {
    let (rows, columns) = peer.size();
    let mut session = TerminalSession::new(shell_path, rows, columns, Some(name.as_str()))?;
    let signals = Signals::install()?;
    loop {
        let mut frontend = ServerFrontend::new(peer);
        match session.attach(&mut frontend, &signals)? {
            ForwardExit::Process(code) => {
                if !frontend.has_pending_output() {
                    frontend.send_exit(i32::from(code))?;
                }
                return Ok(code);
            }
            ForwardExit::Detached | ForwardExit::Disconnected => {}
        }
        drop(frontend);

        loop {
            match session.wait_for_client(endpoint, &signals)? {
                DetachedEvent::Process(code) => return Ok(code),
                DetachedEvent::Client(stream) => match handshake::server(stream) {
                    Ok(next) => {
                        peer = next;
                        break;
                    }
                    // A malformed or abandoned connection belongs to that client;
                    // it must not terminate the existing panes.
                    Err(_) => continue,
                },
            }
        }
    }
}

/// State that must survive one frontend disconnect and a later attachment.
struct TerminalSession {
    shell_path: OsString,
    session_name: Option<String>,
    windows: Windows<PaneSet<Pane>>,
    outer_rows: u16,
    closed: Option<crate::closed_pane::ClosedPane>,
}

impl TerminalSession {
    fn new(
        shell_path: &OsStr,
        rows: u16,
        columns: u16,
        session_name: Option<&str>,
    ) -> io::Result<Self> {
        check_size(rows, columns)?;
        let mut windows = Windows::default();
        windows.create(
            "shell".into(),
            spawn_window(shell_path, None, pane_rows(rows), columns)?,
        )?;
        Ok(Self {
            shell_path: shell_path.to_owned(),
            session_name: session_name.map(str::to_owned),
            windows,
            outer_rows: rows,
            closed: None,
        })
    }

    fn attach(
        &mut self,
        frontend: &mut impl Frontend,
        signals: &Signals,
    ) -> io::Result<ForwardExit> {
        forward(
            frontend,
            &mut self.windows,
            signals,
            &self.shell_path,
            self.session_name.as_deref(),
            &mut self.outer_rows,
            &mut self.closed,
        )
    }

    /// Keep every PTY live while waiting for the next session client.
    fn wait_for_client(
        &mut self,
        endpoint: &SessionEndpoint,
        signals: &Signals,
    ) -> io::Result<DetachedEvent> {
        loop {
            let received = signals.pending.load(Ordering::Relaxed);
            if received != 0 {
                return Ok(DetachedEvent::Process((128 + received) as u8));
            }
            if let Some(saved) = self.closed.as_mut()
                && !saved.service()?
            {
                self.closed = None;
            }

            for window in self.windows.iter_mut() {
                for (_, pane) in window.content_mut().iter_mut() {
                    let (shell, _, _, state) = pane.parts_mut();
                    if state.status.is_none() {
                        state.status = shell.try_wait()?;
                    }
                }
            }
            if let Some(code) = self.remove_finished_detached()? {
                return Ok(DetachedEvent::Process(code));
            }

            let mut interests = Vec::new();
            let (listener_events, pane_events) = {
                let mut fds = vec![PollFd::new(endpoint.listener().as_fd(), PollFlags::POLLIN)];
                for window in self.windows.iter() {
                    for (pane_id, pane) in window.content().iter() {
                        let mut flags = PollFlags::empty();
                        if pane.io().reply_read_limit() != 0 {
                            flags |= PollFlags::POLLIN;
                        }
                        if !pane.io().eof
                            && pane.io().status.is_none()
                            && !pane.io().to_shell.is_empty()
                        {
                            flags |= PollFlags::POLLOUT;
                        }
                        if !flags.is_empty() {
                            interests.push((window.id(), pane_id, flags));
                            fds.push(PollFd::new(
                                pane.shell().master_fd().expect("live PTY"),
                                flags,
                            ));
                        }
                    }
                }
                if let Some(saved) = self.closed.as_ref() {
                    let pane = saved.pane.as_ref().unwrap();
                    let mut flags = PollFlags::empty();
                    if pane.io().reply_read_limit() != 0 {
                        flags |= PollFlags::POLLIN;
                    }
                    if !pane.io().to_shell.is_empty() {
                        flags |= PollFlags::POLLOUT;
                    }
                    if !flags.is_empty() {
                        fds.push(PollFd::new(pane.shell().master_fd().unwrap(), flags));
                    }
                }
                match poll(&mut fds, POLL_TIMEOUT_MILLIS) {
                    Ok(_) | Err(Errno::EINTR) => {}
                    Err(error) => return Err(error.into()),
                }
                (
                    fds[0].revents().unwrap_or(PollFlags::empty()),
                    fds[1..]
                        .iter()
                        .take(interests.len())
                        .map(|fd| fd.revents().unwrap_or(PollFlags::empty()))
                        .collect::<Vec<_>>(),
                )
            };

            for ((window_id, pane_id, requested), ready) in interests.into_iter().zip(pane_events) {
                service_pane(
                    self.windows
                        .get_mut(window_id)
                        .unwrap()
                        .content_mut()
                        .get_mut(pane_id)
                        .expect("polled pane exists"),
                    requested,
                    ready,
                )?;
            }

            if listener_events.contains(PollFlags::POLLNVAL) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "invalid session listener descriptor",
                ));
            }
            if listener_events.intersects(PollFlags::POLLERR | PollFlags::POLLHUP) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "session listener failed",
                ));
            }
            if listener_events.contains(PollFlags::POLLIN) {
                match endpoint.listener().accept() {
                    Ok((stream, _)) => return Ok(DetachedEvent::Client(stream)),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }

    fn remove_finished_detached(&mut self) -> io::Result<Option<u8>> {
        let mut finished = Vec::new();
        for window in self.windows.iter() {
            for (pane_id, pane) in window.content().iter() {
                let state = pane.io();
                if state.eof {
                    if let Some(status) = state.status {
                        finished.push((window.id(), pane_id, exit_code(status)));
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
        for (window_id, pane_id, code) in finished {
            let panes = self
                .windows
                .get_mut(window_id)
                .expect("finished pane owns a window")
                .content_mut();
            if panes.iter().len() == 1 {
                if self.windows.iter().len() == 1 {
                    return Ok(Some(code));
                }
                drop(self.windows.close(window_id)?);
            } else {
                drop(panes.close(pane_id)?);
                panes.synchronize_sizes()?;
            }
        }
        Ok(None)
    }
}

trait Frontend {
    fn poll_fd(&self) -> BorrowedFd<'_>;
    fn take_resize(&mut self) -> io::Result<Option<nix::pty::Winsize>>;
    fn can_receive(&self) -> bool;
    fn drain_input(&mut self, pending: &mut VecDeque<u8>);
    fn receive(&mut self, pending: &mut VecDeque<u8>) -> io::Result<ConnectionState>;
    fn send(&mut self, pending: &mut VecDeque<u8>) -> io::Result<()>;
}

struct LocalFrontend {
    terminal: TerminalDevice,
    resize: Arc<AtomicBool>,
}

impl LocalFrontend {
    fn enter(file: File, resize: Arc<AtomicBool>) -> io::Result<Self> {
        Ok(Self {
            terminal: TerminalDevice::enter(file)?,
            resize,
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        self.terminal.restore()
    }
}

impl Frontend for LocalFrontend {
    fn poll_fd(&self) -> BorrowedFd<'_> {
        self.terminal.file().as_fd()
    }

    fn take_resize(&mut self) -> io::Result<Option<nix::pty::Winsize>> {
        if self.resize.swap(false, Ordering::Relaxed) {
            let size = self.terminal.size()?;
            Ok((size.ws_row != 0 && size.ws_col != 0).then_some(size))
        } else {
            Ok(None)
        }
    }

    fn can_receive(&self) -> bool {
        true
    }

    fn drain_input(&mut self, _pending: &mut VecDeque<u8>) {}

    fn receive(&mut self, pending: &mut VecDeque<u8>) -> io::Result<ConnectionState> {
        if receive(self.terminal.file_mut(), pending)? {
            Ok(ConnectionState::Disconnected)
        } else {
            Ok(ConnectionState::Attached)
        }
    }

    fn send(&mut self, pending: &mut VecDeque<u8>) -> io::Result<()> {
        send(self.terminal.file_mut(), pending)
    }
}

impl Frontend for ServerFrontend {
    fn poll_fd(&self) -> BorrowedFd<'_> {
        self.poll_fd()
    }

    fn take_resize(&mut self) -> io::Result<Option<nix::pty::Winsize>> {
        Ok(self.take_resize())
    }

    fn can_receive(&self) -> bool {
        self.state() == ConnectionState::Attached
            && self.buffered_input_len() < crate::session::protocol::MAX_FRAME_BYTES
    }

    fn drain_input(&mut self, pending: &mut VecDeque<u8>) {
        self.drain_input(pending, LIMIT);
    }

    fn receive(&mut self, pending: &mut VecDeque<u8>) -> io::Result<ConnectionState> {
        let state = self.receive()?;
        self.drain_input(pending, LIMIT);
        Ok(state)
    }

    fn send(&mut self, pending: &mut VecDeque<u8>) -> io::Result<()> {
        self.send_output(pending)
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
    SelectPane(PaneId),
    ResizeSeparator(usize, i32),
    FinishSeparatorResize,
    Last,
    Close,
    ClosePane,
    MoveLeft,
    MoveRight,
    Split(SplitAxis),
    NextPane,
    BreakPane,
    JoinPane,
    ToggleZoom,
    UndoClose,
    History,
    HistoryEditor,
    LastCommandEditor,
    FocusPane(Direction),
    ResizePane(Direction),
    SwapPaneNext,
    SwapPanePrevious,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum InputMode {
    #[default]
    Locked,
    Normal,
}

#[derive(Default)]
struct WindowInput {
    mode: InputMode,
    paste: bool,
    tail: VecDeque<u8>,
    mouse: Vec<u8>,
    mouse_since: Option<Instant>,
    pane_height: usize,
    pane_top: usize,
    pane_left: usize,
    pane_width: usize,
    mouse_tracking: MouseTracking,
    bar_enabled: bool,
    bar_press: bool,
    pane_press: bool,
    window_hitboxes: Vec<(usize, usize, usize)>,
    active_pane: Option<PaneId>,
    pane_hitboxes: Vec<(PaneId, Rect)>,
    separator_hitboxes: Vec<(usize, SplitAxis, Rect)>,
    pane_drag: Option<PaneDrag>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PaneDrag {
    separator: usize,
    axis: SplitAxis,
    position: usize,
}

impl WindowInput {
    // Hold only candidate mouse reports. Escape alone is released after 30ms;
    // completed non-mouse sequences are forwarded as soon as they are known.
    fn feed(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        if self.paste
            || (self.mouse.is_empty()
                && (!(self.mouse_tracking != MouseTracking::Off
                    || self.bar_enabled
                    || self.pane_hitboxes.len() > 1)
                    || byte != 27))
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
            self.mode = InputMode::Locked;
            let release = (bytes.starts_with(b"\x1b[<") && bytes.last() == Some(&b'm'))
                || (bytes.starts_with(b"\x1b[M")
                    && bytes[3]
                        .checked_sub(32)
                        .is_some_and(|button| button & 0x63 == 3));
            if let Some(mut drag) = self.pane_drag {
                if release {
                    self.pane_drag = None;
                    output.push(WindowKey::FinishSeparatorResize);
                    return;
                }
                if left_mouse_drag_motion(&bytes) {
                    let position = match drag.axis {
                        SplitAxis::Columns => column,
                        SplitAxis::Rows => row,
                    };
                    let delta = position as i32 - drag.position as i32;
                    drag.position = position;
                    self.pane_drag = Some(drag);
                    if delta != 0 {
                        output.push(WindowKey::ResizeSeparator(drag.separator, delta));
                    }
                    return;
                }
                return;
            }
            if release && self.bar_press {
                self.bar_press = false;
                return;
            }
            if !release && left_mouse_press(&bytes) {
                let layout_row = row.saturating_sub(1 + usize::from(self.bar_enabled));
                let layout_column = column.saturating_sub(1);
                if let Some((separator, axis, _)) =
                    self.separator_hitboxes.iter().find(|(_, _, rect)| {
                        layout_row >= usize::from(rect.row)
                            && layout_row < usize::from(rect.row + rect.rows)
                            && layout_column >= usize::from(rect.column)
                            && layout_column < usize::from(rect.column + rect.columns)
                    })
                {
                    self.pane_drag = Some(PaneDrag {
                        separator: *separator,
                        axis: *axis,
                        position: match axis {
                            SplitAxis::Columns => column,
                            SplitAxis::Rows => row,
                        },
                    });
                    return;
                }
            }
            if release && self.pane_press {
                self.pane_press = false;
                return;
            }
            if self.bar_enabled && row == 1 && !release {
                if let Some(action) = bar_scroll(&bytes) {
                    output.push(action);
                } else if left_mouse_press(&bytes) {
                    self.bar_press = true;
                    if let Some((_, _, index)) = self
                        .window_hitboxes
                        .iter()
                        .find(|(start, end, _)| column >= *start && column < *end)
                    {
                        output.push(WindowKey::Select(*index));
                    }
                }
                return;
            }
            if !release
                && left_mouse_press(&bytes)
                && let Some((id, _)) = self.pane_hitboxes.iter().find(|(_, rect)| {
                    let row = row.saturating_sub(1 + usize::from(self.bar_enabled));
                    let column = column.saturating_sub(1);
                    row >= usize::from(rect.row)
                        && row < usize::from(rect.row + rect.rows)
                        && column >= usize::from(rect.column)
                        && column < usize::from(rect.column + rect.columns)
                })
                && Some(*id) != self.active_pane
            {
                self.pane_press = true;
                output.push(WindowKey::SelectPane(*id));
                return;
            }
            if self.mouse_tracking == MouseTracking::Off {
                return;
            }
            if mouse_motion(&bytes)
                && match self.mouse_tracking {
                    MouseTracking::Off | MouseTracking::Button => true,
                    MouseTracking::Drag => !motion_has_button(&bytes),
                    MouseTracking::Any => false,
                }
            {
                return;
            }
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
        } else if self.mode == InputMode::Normal {
            self.mode = InputMode::Locked;
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
                b'!' => output.push(WindowKey::BreakPane),
                b'm' => output.push(WindowKey::JoinPane),
                b'o' => output.push(WindowKey::NextPane),
                b'Z' => output.push(WindowKey::ToggleZoom),
                b'z' => output.push(WindowKey::UndoClose),
                b'[' => output.push(WindowKey::History),
                b'E' => output.push(WindowKey::HistoryEditor),
                b'e' => output.push(WindowKey::LastCommandEditor),
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
            self.mode = InputMode::Normal;
        } else {
            output.push(WindowKey::Byte(byte));
        }
    }
}

fn left_mouse_press(bytes: &[u8]) -> bool {
    mouse_button(bytes).is_some_and(|button| button & 0b1110_0011 == 0)
}

fn mouse_button(bytes: &[u8]) -> Option<u16> {
    if bytes.starts_with(b"\x1b[<") && bytes.last() == Some(&b'M') {
        bytes[3..]
            .iter()
            .position(|&byte| byte == b';')
            .and_then(|length| std::str::from_utf8(&bytes[3..3 + length]).ok())
            .and_then(|button| button.parse().ok())
    } else if bytes.starts_with(b"\x1b[M") {
        bytes
            .get(3)
            .and_then(|button| button.checked_sub(32))
            .map(u16::from)
    } else {
        None
    }
}

fn bar_scroll(bytes: &[u8]) -> Option<WindowKey> {
    match mouse_button(bytes)? & 0b1100_0011 {
        64 => Some(WindowKey::Previous),
        65 => Some(WindowKey::Next),
        _ => None,
    }
}

fn mouse_motion(bytes: &[u8]) -> bool {
    mouse_button(bytes).is_some_and(|button| button & 32 != 0)
}

fn motion_has_button(bytes: &[u8]) -> bool {
    mouse_button(bytes).is_some_and(|button| matches!(button & 0b1110_0011, 32..=34))
}

fn left_mouse_drag_motion(bytes: &[u8]) -> bool {
    mouse_button(bytes).is_some_and(|button| button & 0b1110_0011 == 32)
}

fn spawn_window(
    shell: &OsStr,
    directory: Option<&Path>,
    rows: u16,
    columns: u16,
) -> io::Result<PaneSet<Pane>> {
    let (content_rows, content_columns) = pane_content_dimensions(rows, columns);
    PaneSet::new(
        rows,
        columns,
        Pane::spawn_in(shell, directory, content_rows, content_columns)?,
    )
}

fn pane_content_dimensions(rows: u16, columns: u16) -> (u16, u16) {
    (
        if rows >= 3 { rows - 2 } else { rows },
        if columns >= 3 { columns - 2 } else { columns },
    )
}

fn active_directory(windows: &Windows<PaneSet<Pane>>) -> Option<PathBuf> {
    windows
        .active()
        .unwrap()
        .content()
        .active()
        .inherited_directory()
}

fn submits_command(byte: u8, bracketed_paste: bool) -> bool {
    matches!(byte, b'\r' | b'\n') && !bracketed_paste
}

fn spawn_editor_window(text: &str, rows: u16, columns: u16) -> io::Result<PaneSet<Pane>> {
    let (content_rows, content_columns) = pane_content_dimensions(rows, columns);
    PaneSet::new(
        rows,
        columns,
        Pane::spawn_editor(text, content_rows, content_columns)?,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForwardExit {
    Process(u8),
    Detached,
    Disconnected,
}

enum DetachedEvent {
    Client(UnixStream),
    Process(u8),
}

fn frontend_exit(state: ConnectionState, input: &VecDeque<u8>) -> Option<ForwardExit> {
    if !input.is_empty() {
        return None;
    }
    match state {
        ConnectionState::Attached => None,
        ConnectionState::Detached => Some(ForwardExit::Detached),
        ConnectionState::Disconnected => Some(ForwardExit::Disconnected),
    }
}

fn service_pane(pane: &mut Pane, requested: PollFlags, ready: PollFlags) -> io::Result<()> {
    if ready.contains(PollFlags::POLLNVAL) {
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "invalid PTY descriptor",
        ));
    }
    let reply_read_limit = pane.io().reply_read_limit();
    let readable = ready.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR);
    if !pane.io().eof && requested.contains(PollFlags::POLLIN) {
        if readable {
            let mut bytes = [0; 8192];
            let read_limit = bytes.len().min(reply_read_limit);
            match pane.shell_mut().read(&mut bytes[..read_limit]) {
                Ok(0) => pane.parts_mut().3.eof = true,
                Ok(count) => {
                    let mut replies = Vec::new();
                    pane.process_output(&bytes[..count], &mut |reply| {
                        replies.extend_from_slice(reply);
                    });
                    let state = pane.parts_mut().3;
                    if state.status.is_none() {
                        state.to_shell.extend(replies);
                    }
                    debug_assert!(state.to_shell.len() <= LIMIT);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error),
            }
        } else if pane.io().status.is_some() {
            // Descendants retaining the slave must not delay direct-child exit.
            pane.parts_mut().3.eof = true;
        }
        if pane.io().eof {
            pane.finish_output();
        }
    }
    if !pane.io().eof && pane.io().status.is_none() && ready.contains(PollFlags::POLLOUT) {
        let (shell, _, _, state) = pane.parts_mut();
        send(shell, &mut state.to_shell)?;
    }
    Ok(())
}

fn forward(
    frontend: &mut impl Frontend,
    windows: &mut Windows<PaneSet<Pane>>,
    signals: &Signals,
    shell_path: &OsStr,
    session_name: Option<&str>,
    outer_rows: &mut u16,
    closed: &mut Option<crate::closed_pane::ClosedPane>,
) -> io::Result<ForwardExit> {
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
    let mut connection = ConnectionState::Attached;
    let mut pane_resize_pending: Option<(WindowId, Instant)> = None;
    loop {
        frontend.drain_input(&mut input);
        if connection != ConnectionState::Attached {
            // No peer can consume an old physical frame. Dropping it also lets
            // history-mode input that preceded Detach continue in order.
            to_terminal.clear();
        }
        if let Some(exit) = frontend_exit(connection, &input) {
            return Ok(exit);
        }
        let received = signals.pending.load(Ordering::Relaxed);
        if received != 0 {
            return Ok(ForwardExit::Process((128 + received) as u8));
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
                    if sole_pane && windows.iter().len() == 1 {
                        // run() restores the terminal, then cleans up visible and hidden shells.
                        return Ok(ForwardExit::Process(0));
                    }
                    windows
                        .get_mut(id)
                        .unwrap()
                        .content_mut()
                        .get_mut(pane_id)
                        .unwrap()
                        .stop_for_hide()?;
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
                        return Ok(ForwardExit::Process(0));
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
        if history
            .as_ref()
            .is_some_and(|view| view.escape_expired(Instant::now()))
        {
            let exited = history.as_mut().unwrap().expire_escape();
            if exited {
                history = None;
                keys = WindowInput::default();
            }
            renderer.invalidate();
            force_redraw = true;
        }
        if history
            .as_mut()
            .is_some_and(|view| view.expire_copy_status(Instant::now()))
        {
            renderer.invalidate();
            force_redraw = true;
        }
        if history
            .as_mut()
            .is_some_and(|view| view.expire_drag_scroll(Instant::now()))
        {
            renderer.invalidate();
            force_redraw = true;
        }
        let active = windows.active().expect("at least one window").id();
        let resize = if let Some(size) = frontend.take_resize()? {
            check_size(size.ws_row, size.ws_col)?;
            *outer_rows = size.ws_row;
            if history.take().is_some() {
                input.clear();
                keys = WindowInput::default();
            }
            renderer.invalidate();
            force_redraw = true;
            Some(size)
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
                    let mut sizes = layout.tiled_content_geometry().panes;
                    if layout.is_zoomed() {
                        let visible = layout.content_geometry().panes[0];
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
            pane_resize_pending = None;
        }
        if pane_resize_pending.is_some_and(|(_, due)| Instant::now() >= due) {
            let (id, _) = pane_resize_pending.take().unwrap();
            if let Some(window) = windows.get_mut(id) {
                window.content_mut().synchronize_sizes()?;
                renderer.invalidate();
                force_redraw = true;
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
                    && pane_resize_pending.is_none()
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
                    let titles: Vec<_> = panes
                        .iter()
                        .map(|(pane_id, pane)| (pane_id, pane.terminal_title()))
                        .collect();
                    let content = pane_view::compose_with_titles(
                        panes.layout(),
                        &screens,
                        history.as_ref().map(|_| focused),
                        &titles,
                    )?;
                    let mut view = compose(
                        &content,
                        *outer_rows,
                        session_name,
                        &names,
                        active_index,
                        keys.mode == InputMode::Normal,
                    )?;
                    if let Some(history) = &history
                        && *outer_rows > 1
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
                        if let Some(column) = history.query_cursor(view.dimensions().1) {
                            view.position(0, column);
                            view.set_cursor_visible(true);
                            view.set_cursor_shape(crate::screen::CursorShape::SteadyBar);
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
                        return Ok(ForwardExit::Process(code));
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
                if keys.mode == InputMode::Normal {
                    keys.mode = InputMode::Locked;
                    state.to_shell.push_back(2);
                    bar_dirty = true;
                    force_redraw = true;
                }
                state.to_shell.extend(keys.take_mouse());
            }
        }
        // Decode in input order. Bytes preceding a switch remain queued for the
        // old child; following bytes target the newly selected one.
        while close_requested.is_none() && !input.is_empty() {
            if let Some(view) = &mut history {
                // Finish any pending frame or OSC before accepting more history
                // input. Repeated copy keys cannot grow the output queue unbounded.
                if !to_terminal.is_empty() {
                    break;
                }
                let exited = view.feed(input.pop_front().unwrap());
                let copy = view.take_copy();
                if exited {
                    history = None;
                    keys = WindowInput::default();
                }
                if let Some(sequence) = copy {
                    to_terminal.extend(sequence);
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
                            PromptKind::MovePane => {
                                // Numbers refer to the IDs shown when opening the prompt,
                                // so a background exit cannot silently retarget the move.
                                let target = editor
                                    .text
                                    .trim()
                                    .parse::<usize>()
                                    .ok()
                                    .and_then(|number| number.checked_sub(1))
                                    .and_then(|index| editor.destinations.get(index))
                                    .copied();
                                let source = windows.active().unwrap().id();
                                let result = target
                                    .ok_or_else(|| io::Error::other("invalid window number"))
                                    .and_then(|target| {
                                        windows.join_active_pane(target, SplitAxis::Columns)
                                    });
                                match result {
                                    Ok(true) => {
                                        if let Some(source) = windows.get_mut(source) {
                                            source.content_mut().synchronize_sizes()?;
                                        }
                                        windows
                                            .active_mut()
                                            .unwrap()
                                            .content_mut()
                                            .synchronize_sizes()?;
                                        bar_dirty = true;
                                    }
                                    Ok(false) => {}
                                    Err(_) => {
                                        if to_terminal.is_empty() {
                                            to_terminal.push_back(7);
                                        }
                                    }
                                }
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
                .content_geometry()
                .panes
                .into_iter()
                .find(|(id, _)| *id == set.layout().active())
                .unwrap()
                .1;
            keys.pane_top = usize::from(*outer_rows > 1) + usize::from(rect.row);
            keys.pane_left = usize::from(rect.column);
            keys.pane_width = usize::from(rect.columns);
            keys.mouse_tracking = pane.screen().mouse_tracking();
            keys.bar_enabled = *outer_rows > 1;
            if keys.mouse.is_empty() && input.front() == Some(&27) {
                let active = windows.active().unwrap().id();
                let names: Vec<_> = windows
                    .iter()
                    .map(|window| window.name().to_owned())
                    .collect();
                let active_index = windows
                    .iter()
                    .position(|window| window.id() == active)
                    .unwrap();
                let columns = windows.active().unwrap().content().layout().dimensions().1;
                keys.window_hitboxes = crate::chrome::window_hitboxes(
                    usize::from(columns),
                    session_name,
                    &names,
                    active_index,
                    keys.mode == InputMode::Normal,
                );
                keys.active_pane = Some(set.layout().active());
                keys.pane_hitboxes = pane_view::hitboxes(set.layout());
                keys.separator_hitboxes = set.layout().separator_hitboxes();
            }
            actions.clear();
            let input_mode = keys.mode;
            keys.feed(input.pop_front().unwrap(), &mut actions);
            if keys.mode != input_mode {
                bar_dirty = true;
                force_redraw = true;
            }
            for action in actions.drain(..) {
                let old = (
                    windows.active().unwrap().id(),
                    windows.active().unwrap().content().layout().active(),
                );
                match action {
                    WindowKey::Byte(byte) => {
                        let pane = windows.active_mut().unwrap().content_mut().active_mut();
                        if submits_command(byte, keys.paste) {
                            pane.command_submitted();
                        }
                        pane.parts_mut().3.to_shell.push_back(byte);
                    }
                    WindowKey::Split(axis) => {
                        let directory = active_directory(windows);
                        let panes = windows.active_mut().unwrap().content_mut();
                        match panes.split_with(axis, |_, rect| {
                            Pane::spawn_in(
                                shell_path,
                                directory.as_deref(),
                                rect.rows,
                                rect.columns,
                            )
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
                        if let Some(view) = &mut history {
                            view.set_origin(keys.pane_top, keys.pane_left);
                        }
                        keys = WindowInput::default();
                        if history.is_some() {
                            renderer.invalidate();
                            force_redraw = true;
                        }
                    }
                    WindowKey::HistoryEditor => {
                        let screen = windows.active().unwrap().content().active().screen();
                        if windows.iter().len() == MAX_WINDOWS || screen.is_alternate() {
                            if to_terminal.is_empty() {
                                to_terminal.push_back(7);
                            }
                            continue;
                        }
                        let text = crate::history_view::export_text(screen);
                        let (rows, columns) =
                            windows.active().unwrap().content().layout().dimensions();
                        match spawn_editor_window(&text, rows, columns) {
                            Ok(pane) => {
                                windows.create("history".into(), pane)?;
                            }
                            Err(_) => {
                                if to_terminal.is_empty() {
                                    to_terminal.push_back(7);
                                }
                            }
                        }
                    }
                    WindowKey::LastCommandEditor => {
                        let text = windows
                            .active()
                            .unwrap()
                            .content()
                            .active()
                            .last_command_output();
                        if windows.iter().len() == MAX_WINDOWS || text.is_none() {
                            if to_terminal.is_empty() {
                                to_terminal.push_back(7);
                            }
                            continue;
                        }
                        let (rows, columns) =
                            windows.active().unwrap().content().layout().dimensions();
                        match spawn_editor_window(text.as_deref().unwrap(), rows, columns) {
                            Ok(pane) => {
                                windows.create("output".into(), pane)?;
                            }
                            Err(_) => {
                                if to_terminal.is_empty() {
                                    to_terminal.push_back(7);
                                }
                            }
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
                    WindowKey::JoinPane => {
                        prompt = Some(WindowPrompt::move_pane(
                            windows.iter().map(|window| window.id()).collect(),
                        ));
                        renderer.invalidate();
                        force_redraw = true;
                    }
                    WindowKey::BreakPane => {
                        if windows.iter().len() == MAX_WINDOWS {
                            if to_terminal.is_empty() {
                                to_terminal.push_back(7);
                            }
                            continue;
                        }
                        let source = windows.active().unwrap().id();
                        match windows.break_active_pane() {
                            Ok(Some(_)) => {
                                windows
                                    .get_mut(source)
                                    .unwrap()
                                    .content_mut()
                                    .synchronize_sizes()?;
                                bar_dirty = true;
                                renderer.invalidate();
                                force_redraw = true;
                            }
                            Ok(None) => {}
                            Err(_) => {
                                if to_terminal.is_empty() {
                                    to_terminal.push_back(7);
                                }
                            }
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
                    WindowKey::SelectPane(id) => {
                        windows.active_mut().unwrap().content_mut().select(id)?;
                    }
                    WindowKey::ResizeSeparator(index, delta) => {
                        let window = windows.active_mut().unwrap();
                        if window.content_mut().resize_separator(index, delta) {
                            pane_resize_pending.get_or_insert_with(|| {
                                (window.id(), Instant::now() + PANE_DRAG_RESIZE_INTERVAL)
                            });
                        }
                    }
                    WindowKey::FinishSeparatorResize => {
                        if let Some((id, _)) = pane_resize_pending.take()
                            && let Some(window) = windows.get_mut(id)
                        {
                            window.content_mut().synchronize_sizes()?;
                            renderer.invalidate();
                            force_redraw = true;
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
                        let directory = active_directory(windows);
                        match spawn_window(shell_path, directory.as_deref(), rows, columns) {
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
        if let Some(exit) = frontend_exit(connection, &input) {
            return Ok(exit);
        }
        // A changed focus needs a frame before returning to a blocking poll.
        if (force_redraw || close_requested.is_some())
            && to_terminal.is_empty()
            && pane_resize_pending.is_none()
        {
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
        if connection == ConnectionState::Attached && input.len() < LIMIT && frontend.can_receive()
        {
            outer_events |= PollFlags::POLLIN;
        }
        if connection == ConnectionState::Attached && !to_terminal.is_empty() {
            outer_events |= PollFlags::POLLOUT;
        }
        let mut timeout = if (active_dirty || bar_dirty) && !active_paused && to_terminal.is_empty()
        {
            next_frame
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 50) as u16
        } else {
            50
        };
        if let Some((_, due)) = pane_resize_pending {
            timeout = timeout.min(
                due.saturating_duration_since(Instant::now())
                    .as_millis()
                    .clamp(1, u128::from(POLL_TIMEOUT_MILLIS)) as u16,
            );
        }
        let mut interests = Vec::new();
        let (outer, events) = {
            let mut fds = vec![PollFd::new(frontend.poll_fd(), outer_events)];
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
        if outer.contains(PollFlags::POLLNVAL) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "invalid frontend descriptor",
            ));
        }
        if connection == ConnectionState::Attached
            && outer.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
        {
            connection = frontend.receive(&mut input)?;
        }
        if connection == ConnectionState::Attached && outer.contains(PollFlags::POLLOUT) {
            frontend.send(&mut to_terminal)?;
        }
        // One bounded read/write per pane per iteration prevents a busy background
        // process from starving the other panes, keyboard or signal handling.
        for ((id, pane_id, inner_events), inner) in interests.into_iter().zip(events) {
            service_pane(
                windows
                    .get_mut(id)
                    .unwrap()
                    .content_mut()
                    .get_mut(pane_id)
                    .expect("polled pane exists"),
                inner_events,
                inner,
            )?;
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
    use crate::session::{
        handshake::{self, ClientPeer, ServerPeer},
        protocol::{ClientMessage, ServerMessage},
    };
    use std::os::unix::{fs::PermissionsExt, net::UnixStream};
    use std::thread;

    fn socket_peers(rows: u16, columns: u16) -> (ClientPeer, ServerPeer) {
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || handshake::server(server_stream).unwrap());
        let client = handshake::client(client_stream, rows, columns).unwrap();
        (client, server.join().unwrap())
    }

    fn socket_frontend(rows: u16, columns: u16) -> (ClientPeer, ServerFrontend) {
        let (client, server) = socket_peers(rows, columns);
        (client, ServerFrontend::new(server))
    }

    fn send_client_messages(client: &mut ClientPeer, messages: &[ClientMessage]) {
        let bytes: Vec<_> = messages
            .iter()
            .flat_map(|message| message.encode().unwrap())
            .collect();
        client.stream_mut().write_all(&bytes).unwrap();
    }

    fn test_signals() -> Signals {
        Signals {
            pending: Arc::new(AtomicUsize::new(0)),
            resize: Arc::new(AtomicBool::new(false)),
            ids: Vec::new(),
        }
    }

    #[test]
    fn socket_frontend_defers_detach_until_prior_input_is_consumed() {
        let (mut client, mut frontend) = socket_frontend(24, 80);
        send_client_messages(
            &mut client,
            &[
                ClientMessage::Input(b"ordered".to_vec()),
                ClientMessage::Detach,
            ],
        );

        let mut input = VecDeque::new();
        let state = Frontend::receive(&mut frontend, &mut input).unwrap();
        assert_eq!(state, ConnectionState::Detached);
        assert_eq!(input, b"ordered".to_vec());
        assert_eq!(frontend_exit(state, &input), None);
        input.clear();
        assert_eq!(frontend_exit(state, &input), Some(ForwardExit::Detached));
    }

    #[test]
    fn terminal_session_preserves_shell_across_socket_attachments() {
        let mut session = TerminalSession::new(OsStr::new("/bin/sh"), 24, 80, None).unwrap();
        let signals = test_signals();
        let (mut first_client, mut first_frontend) = socket_frontend(24, 80);
        send_client_messages(
            &mut first_client,
            &[
                ClientMessage::Input(b"RUSTMUX_ATTACH_TEST=kept\n".to_vec()),
                ClientMessage::Detach,
            ],
        );
        assert_eq!(
            session.attach(&mut first_frontend, &signals).unwrap(),
            ForwardExit::Detached
        );
        assert_eq!(session.windows.iter().len(), 1);

        let (mut second_client, mut second_frontend) = socket_frontend(30, 90);
        send_client_messages(
            &mut second_client,
            &[ClientMessage::Input(
                b"test \"$RUSTMUX_ATTACH_TEST\" = kept; exit $?\n".to_vec(),
            )],
        );
        assert_eq!(
            session.attach(&mut second_frontend, &signals).unwrap(),
            ForwardExit::Process(0)
        );
        assert_eq!(session.outer_rows, 30);
    }

    #[test]
    fn detached_session_drains_output_while_waiting_for_a_client() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = crate::session::SessionName::new(format!(
            "detached-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        let endpoint = SessionEndpoint::bind(&name).unwrap();
        let mut session = TerminalSession::new(OsStr::new("/bin/sh"), 24, 80, None).unwrap();
        let signals = test_signals();

        let (mut first_client, mut first_frontend) = socket_frontend(24, 80);
        send_client_messages(
            &mut first_client,
            &[
                ClientMessage::Input(b"printf 'detached-marker\\n'\n".to_vec()),
                ClientMessage::Detach,
            ],
        );
        assert_eq!(
            session.attach(&mut first_frontend, &signals).unwrap(),
            ForwardExit::Detached
        );

        let path = endpoint.path().to_owned();
        let connector = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            handshake::client(UnixStream::connect(path).unwrap(), 30, 90).unwrap()
        });
        let stream = match session.wait_for_client(&endpoint, &signals).unwrap() {
            DetachedEvent::Client(stream) => stream,
            DetachedEvent::Process(code) => panic!("shell exited with {code}"),
        };
        let peer = handshake::server(stream).unwrap();
        let mut second_client = connector.join().unwrap();
        let text = crate::history_view::export_text(
            session
                .windows
                .active()
                .unwrap()
                .content()
                .active()
                .screen(),
        );
        assert!(text.contains("detached-marker"), "screen was {text:?}");

        let mut second_frontend = ServerFrontend::new(peer);
        send_client_messages(
            &mut second_client,
            &[ClientMessage::Input(b"exit\n".to_vec())],
        );
        assert_eq!(
            session.attach(&mut second_frontend, &signals).unwrap(),
            ForwardExit::Process(0)
        );
    }

    #[test]
    fn session_server_reports_the_final_status_to_its_client() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = crate::session::SessionName::new(format!(
            "serve-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        let endpoint = SessionEndpoint::bind(&name).unwrap();
        let (mut client, server) = socket_peers(24, 80);
        send_client_messages(&mut client, &[ClientMessage::Input(b"exit 7\n".to_vec())]);

        assert_eq!(
            serve_session(OsStr::new("/bin/sh"), &name, &endpoint, server).unwrap(),
            7
        );
        let mut status = None;
        let mut bytes = [0; 8192];
        loop {
            match client.stream_mut().read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    for message in client.decode(&bytes[..count]).unwrap() {
                        if let ServerMessage::Exit { status: value } = message {
                            status = Some(value);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => panic!("client read failed: {error}"),
            }
        }
        assert_eq!(status, Some(7));
    }

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
            let terminal =
                LocalFrontend::enter(pair.slave.into(), Arc::new(AtomicBool::new(false)))?;
            assert!(
                !termios::tcgetattr(terminal.terminal.file())?
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

    #[test]
    fn pane_service_tracks_prompt_markers_across_output_scrolling() {
        let mut shell = tempfile::NamedTempFile::new().unwrap();
        shell
            .write_all(
                b"#!/bin/sh\nprintf '\\033[3;1H\\033]133;A\\a\\r\\nSTATUS\\r\\n> \\033]133;B\\a'\nsleep 5\n",
            )
            .unwrap();
        let mut permissions = shell.as_file().metadata().unwrap().permissions();
        permissions.set_mode(0o700);
        shell.as_file().set_permissions(permissions).unwrap();

        let mut pane = Pane::spawn(shell.path(), 3, 8).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while pane.io().prompt_start != Some((0, 0)) {
            service_pane(&mut pane, PollFlags::POLLIN, PollFlags::POLLIN).unwrap();
            assert!(Instant::now() < deadline, "prompt marker was not parsed");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pane.screen().cursor(), (2, 2));
        pane.shell_mut().terminate().unwrap();
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
    fn prefix_enters_normal_mode_until_one_command_is_decoded() {
        let mut decoder = WindowInput::default();
        let mut actions = Vec::new();
        assert_eq!(decoder.mode, InputMode::Locked);
        decoder.feed(2, &mut actions);
        assert!(actions.is_empty());
        assert_eq!(decoder.mode, InputMode::Normal);
        decoder.feed(b'n', &mut actions);
        assert_eq!(actions, [WindowKey::Next]);
        assert_eq!(decoder.mode, InputMode::Locked);
    }

    #[test]
    fn only_newlines_outside_bracketed_paste_submit_commands() {
        let mut decoder = WindowInput::default();
        let mut actions = Vec::new();
        let mut submissions = 0;
        for &byte in b"\x1b[200~first\nsecond\x1b[201~\r" {
            actions.clear();
            decoder.feed(byte, &mut actions);
            submissions += actions
                .iter()
                .filter(|action| {
                    matches!(action, WindowKey::Byte(byte) if submits_command(*byte, decoder.paste))
                })
                .count();
        }
        assert_eq!(submissions, 1);
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
            decode(b"\x02%\x02\"\x02o\x02Z\x02[\x02E\x02e\x02h\x02j\x02k\x02l"),
            vec![
                WindowKey::Split(SplitAxis::Columns),
                WindowKey::Split(SplitAxis::Rows),
                WindowKey::NextPane,
                WindowKey::ToggleZoom,
                WindowKey::History,
                WindowKey::HistoryEditor,
                WindowKey::LastCommandEditor,
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
    fn break_pane_shortcut_requires_prefix_and_respects_paste() {
        assert_eq!(
            decode(b"!\x02!"),
            vec![WindowKey::Byte(b'!'), WindowKey::BreakPane]
        );
        let bytes = b"\x1b[200~\x02!\x1b[201~";
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
    fn join_pane_key_requires_prefix_and_respects_paste() {
        assert_eq!(
            decode(b"m\x02m"),
            vec![WindowKey::Byte(b'm'), WindowKey::JoinPane]
        );
        let bytes = b"\x1b[200~\x02m1\r\x1b[201~";
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
            mouse_tracking: MouseTracking::Button,
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
            mouse_tracking: MouseTracking::Button,
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
            mouse_tracking: MouseTracking::Button,
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

    #[test]
    fn bar_clicks_select_windows_without_reaching_a_child() {
        let mut keys = WindowInput {
            pane_height: 23,
            pane_width: 80,
            pane_top: 1,
            bar_enabled: true,
            window_hitboxes: vec![(1, 11, 0), (11, 22, 1)],
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<0;12;1M\x1b[<0;12;1m" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::Select(1)]);
        assert_eq!(keys.mode, InputMode::Locked);

        output.clear();
        keys.mode = InputMode::Normal;
        for &byte in b"\x1b[M %!\x1b[M#%!" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::Select(0)]);
        assert_eq!(keys.mode, InputMode::Locked);

        output.clear();
        for &byte in b"\x1b[<0;30;1M\x1b[<0;30;1m\x1b[<0;12;2M\x1b[<0;12;2m" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());
    }

    #[test]
    fn bar_scrolls_through_windows_without_starting_a_click() {
        let mut keys = WindowInput {
            pane_height: 23,
            pane_width: 80,
            pane_top: 1,
            bar_enabled: true,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<64;40;1M\x1b[<69;40;1M\x1b[M`(!\x1b[Ma(!" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            [
                WindowKey::Previous,
                WindowKey::Next,
                WindowKey::Previous,
                WindowKey::Next
            ]
        );
        assert!(!keys.bar_press);

        output.clear();
        for &byte in b"\x1b[<64;40;2M\x1b[<64;40;1m" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());
    }

    #[test]
    fn inactive_pane_click_selects_it_and_consumes_the_release() {
        let mut layout = crate::layout::Layout::new(23, 80).unwrap();
        let left = layout.active();
        let right = layout.split_active(SplitAxis::Columns).unwrap();
        let mut keys = WindowInput {
            pane_height: 21,
            pane_width: 38,
            pane_top: 2,
            pane_left: 41,
            bar_enabled: true,
            active_pane: Some(right),
            pane_hitboxes: pane_view::hitboxes(&layout),
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<0;2;3M\x1b[<0;2;3m" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::SelectPane(left)]);
        assert!(!keys.pane_press);

        output.clear();
        for &byte in b"\x1b[<2;2;3M\x1b[<0;42;3M" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());
    }

    #[test]
    fn separator_drag_is_locked_by_index_and_child_motion_is_filtered() {
        let mut layout = crate::layout::Layout::new(23, 80).unwrap();
        layout.split_active(SplitAxis::Columns).unwrap();
        let mut keys = WindowInput {
            pane_height: 21,
            pane_width: 38,
            pane_top: 2,
            pane_left: 41,
            bar_enabled: true,
            active_pane: Some(layout.active()),
            pane_hitboxes: pane_view::hitboxes(&layout),
            separator_hitboxes: layout.separator_hitboxes(),
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<0;40;5M\x1b[<32;45;5M\x1b[<0;45;5m" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            [
                WindowKey::ResizeSeparator(0, 5),
                WindowKey::FinishSeparatorResize,
            ]
        );
        assert!(keys.pane_drag.is_none());

        keys.mouse_tracking = MouseTracking::Button;
        keys.separator_hitboxes.clear();
        output.clear();
        for &byte in b"\x1b[<32;50;5M" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());
        assert!(motion_has_button(b"\x1b[<32;50;5M"));
        assert!(!motion_has_button(b"\x1b[<35;50;5M"));
    }
}
