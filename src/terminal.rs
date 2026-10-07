//! Multi-window PTY polling, prefix input and model-based terminal rendering.

mod hover;

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
    chrome::{
        FooterMode, compose_with_layout, footer_enabled, footer_enabled_for_layout,
        pane_rows_for_layout,
    },
    graphics::outer::KittyOverlays,
    graphics_capability::{GraphicsCapabilityProbe, GraphicsSupport, OuterImageReplies},
    graphics_shared_memory_output::SharedPixels,
    graphics_store::CellPixelSize,
    layout::{Direction, PaneId, Rect, SplitAxis},
    pane_set::PaneSet,
    pane_view,
    prompt::{EditResult, PromptKind, WindowPrompt},
    render::{Renderer, frame::FrameWriter},
    screen::{MouseTracking, Screen},
    session::{
        SessionEndpoint,
        frontend::{ConnectionState, ServerFrontend},
        handshake::{self, ServerPeer},
    },
    terminal_colors::{ColorProbe, TerminalColors},
    terminal_device::{TerminalDevice, window_size},
    window::{WindowId, Windows},
};

// Bound pending keyboard input to 64 KiB; output retains at most one frame.
const POLL_TIMEOUT_MILLIS: u16 = 50;
const MAX_LEGACY_MOUSE_COORDINATE: usize = 223;
const MAX_MOUSE_SEQUENCE_BYTES: usize = 64;
const SYNC_TIMEOUT: Duration = Duration::from_secs(1);
const FRAME_INTERVAL: Duration = Duration::from_millis(6);
const PANE_DRAG_RESIZE_INTERVAL: Duration = Duration::from_millis(33);
const GRAPHICS_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const GRAPHICS_PROBE_IMAGE_ID: std::num::NonZeroU32 = std::num::NonZeroU32::new(31).unwrap();
const SHM_PROBE_IMAGE_ID: std::num::NonZeroU32 = std::num::NonZeroU32::new(32).unwrap();

/// Run on the controlling terminal during single-threaded program startup.
/// Returns the shell exit code, or 128 + signal for termination by signal.
/// Input and output must be terminals. Raw mode is restored before returning.
pub fn run(
    shell_path: &OsStr,
    notifications: crate::config::Notifications,
    scrollback_lines: usize,
    shortcuts: crate::config::Shortcuts,
) -> io::Result<u8> {
    run_inner(
        shell_path,
        notifications,
        scrollback_lines,
        shortcuts,
        false,
        None,
    )
}

/// Run a local session with the configured pane exit policy.
pub fn run_configured(config: &crate::config::Config) -> io::Result<u8> {
    run_inner(
        config.shell(),
        config.notifications(),
        config.scrollback_lines(),
        config.shortcuts(),
        config.remain_on_exit(),
        Some(config),
    )
}

fn run_inner(
    shell_path: &OsStr,
    notifications: crate::config::Notifications,
    scrollback_lines: usize,
    shortcuts: crate::config::Shortcuts,
    remain_on_exit: bool,
    config: Option<&crate::config::Config>,
) -> io::Result<u8> {
    let file = TerminalDevice::open_controlling()?;
    let size = window_size(&file)?;
    // Start the shell before changing the outer terminal, so exec failures
    // cannot leave it raw. Signal registration below creates no worker threads.
    let mut session = TerminalSession::from_snapshot(
        SessionContext {
            shell_path,
            session_name: None,
            notifications,
            scrollback_lines,
            shortcuts,
            compact: config.is_some_and(|config| config.compact()),
        },
        size.ws_row,
        size.ws_col,
        None,
        false,
    )?;
    session.remain_on_exit = remain_on_exit;
    session.default_mode =
        config.map_or_else(crate::config::DefaultMode::default, |c| c.default_mode());
    session.reload = config
        .map(crate::config::reload::Reload::new)
        .transpose()?
        .flatten();
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
    peer: ServerPeer,
    notifications: crate::config::Notifications,
    scrollback_lines: usize,
    shortcuts: crate::config::Shortcuts,
) -> io::Result<u8> {
    serve_inner(
        SessionContext {
            compact: false,
            shell_path,
            session_name: Some(name.as_str()),
            notifications,
            scrollback_lines,
            shortcuts,
        },
        name,
        endpoint,
        peer,
        None,
        Bootstrap {
            options: crate::config::PersistenceOptions::default(),
            remain_on_exit: false,
            config: None,
        },
    )
}

pub(crate) fn serve_configured_session(
    config: &crate::config::Config,
    name: &crate::session::SessionName,
    endpoint: &SessionEndpoint,
    peer: ServerPeer,
    snapshot: Option<crate::persistence::Snapshot>,
) -> io::Result<u8> {
    serve_inner(
        SessionContext {
            compact: config.compact(),
            shell_path: config.shell(),
            session_name: Some(name.as_str()),
            notifications: config.notifications(),
            scrollback_lines: config.scrollback_lines(),
            shortcuts: config.shortcuts(),
        },
        name,
        endpoint,
        peer,
        snapshot,
        Bootstrap {
            options: config.persistence(),
            remain_on_exit: config.remain_on_exit(),
            config: Some(config),
        },
    )
}

struct Bootstrap<'a> {
    options: crate::config::PersistenceOptions,
    remain_on_exit: bool,
    config: Option<&'a crate::config::Config>,
}

fn serve_inner(
    context: SessionContext<'_>,
    name: &crate::session::SessionName,
    endpoint: &SessionEndpoint,
    mut peer: ServerPeer,
    snapshot: Option<crate::persistence::Snapshot>,
    bootstrap: Bootstrap<'_>,
) -> io::Result<u8> {
    let Bootstrap {
        options,
        remain_on_exit,
        config,
    } = bootstrap;
    let (rows, columns) = peer.size();
    let shortcuts = context.shortcuts;
    let mut session = TerminalSession::from_snapshot(
        context.clone(),
        rows,
        columns,
        snapshot.as_ref(),
        options.save_scrollback,
    )?;
    session.persistence = Some(crate::session::snapshot::SnapshotService::bind(
        name,
        options,
        context.scrollback_lines,
    )?);
    session.remain_on_exit = remain_on_exit;
    session.default_mode =
        config.map_or_else(crate::config::DefaultMode::default, |c| c.default_mode());
    session.rename = Some(endpoint.rename_identity());
    let mut control = crate::control::Service::bind(name)?;
    control.track_identity(endpoint.rename_identity());
    session.control = Some(control);
    session.reload = config
        .map(crate::config::reload::Reload::new)
        .transpose()?
        .flatten();
    let signals = Signals::install()?;
    let result = (|| {
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
            session.finish_saves(true);
            drop(frontend);

            loop {
                match session.wait_for_client(endpoint, &signals)? {
                    DetachedEvent::Process(code) => return Ok(code),
                    DetachedEvent::Client(stream) => {
                        match handshake::server_with_keybinds(
                            stream,
                            shortcuts.locked_entry_key(),
                            !shortcuts.clear_defaults(),
                        ) {
                            Ok(next) => {
                                peer = next;
                                break;
                            }
                            // A malformed or abandoned connection belongs to that client;
                            // it must not terminate the existing panes.
                            Err(_) => continue,
                        }
                    }
                }
            }
        }
    })();
    session.finish_saves(true);
    result
}

/// State that must survive one frontend disconnect and a later attachment.
struct TerminalSession {
    compact: bool,
    remain_on_exit: bool,
    default_mode: crate::config::DefaultMode,
    shell_path: OsString,
    session_name: Option<String>,
    windows: Windows<PaneSet<Pane>>,
    outer_rows: u16,
    cell_pixels: Option<CellPixelSize>,
    graphics_support: Option<GraphicsSupport>,
    inherited_colors: Arc<TerminalColors>,
    notifications: crate::config::Notifications,
    scrollback_lines: usize,
    shortcuts: crate::config::Shortcuts,
    closed: Option<crate::closed_pane::ClosedPane>,
    persistence: Option<crate::session::snapshot::SnapshotService>,
    control: Option<crate::control::Service>,
    rename: Option<crate::session::rename::Identity>,
    reload: Option<crate::config::reload::Reload>,
}

#[derive(Clone)]
struct SessionContext<'a> {
    compact: bool,
    shell_path: &'a OsStr,
    session_name: Option<&'a str>,
    notifications: crate::config::Notifications,
    scrollback_lines: usize,
    shortcuts: crate::config::Shortcuts,
}

struct AttachmentCapabilities<'a> {
    remain_on_exit: bool,
    default_mode: crate::config::DefaultMode,
    reload: Option<&'a mut crate::config::reload::Reload>,
    control: Option<&'a mut crate::control::Service>,
    rename: Option<&'a crate::session::rename::Identity>,
    persistence: Option<&'a mut crate::session::snapshot::SnapshotService>,
    cell_pixels: &'a mut Option<CellPixelSize>,
    graphics_support: &'a mut Option<GraphicsSupport>,
    inherited_colors: &'a mut Arc<TerminalColors>,
}

impl TerminalSession {
    #[cfg(test)]
    fn new(
        shell_path: &OsStr,
        rows: u16,
        columns: u16,
        session_name: Option<&str>,
        notifications: crate::config::Notifications,
        scrollback_lines: usize,
        shortcuts: crate::config::Shortcuts,
    ) -> io::Result<Self> {
        Self::from_snapshot(
            SessionContext {
                compact: false,
                shell_path,
                session_name,
                notifications,
                scrollback_lines,
                shortcuts,
            },
            rows,
            columns,
            None,
            false,
        )
    }

    fn from_snapshot(
        context: SessionContext<'_>,
        rows: u16,
        columns: u16,
        snapshot: Option<&crate::persistence::Snapshot>,
        restore_history: bool,
    ) -> io::Result<Self> {
        let SessionContext {
            compact,
            shell_path,
            session_name,
            notifications,
            scrollback_lines,
            shortcuts,
        } = context;
        check_size(rows, columns)?;
        let windows = match snapshot {
            Some(snapshot) => snapshot.restore(
                shell_path,
                rows,
                columns,
                notifications.clone(),
                scrollback_lines,
                restore_history,
                compact,
            )?,
            None => {
                let mut windows = Windows::default();
                windows.create(
                    "shell".into(),
                    spawn_window(
                        shell_path,
                        None,
                        pane_rows_for_layout(rows, compact),
                        columns,
                        notifications.clone(),
                        scrollback_lines,
                    )?,
                )?;
                windows
            }
        };
        Ok(Self {
            compact,
            remain_on_exit: false,
            default_mode: crate::config::DefaultMode::default(),
            shell_path: shell_path.to_owned(),
            session_name: session_name.map(str::to_owned),
            windows,
            outer_rows: rows,
            cell_pixels: None,
            graphics_support: None,
            inherited_colors: Arc::new(TerminalColors::default()),
            notifications,
            scrollback_lines,
            shortcuts,
            closed: None,
            persistence: None,
            control: None,
            rename: None,
            reload: None,
        })
    }

    fn finish_saves(&mut self, checkpoint: bool) {
        if let Some(service) = self.persistence.as_mut() {
            service.finish(&self.windows, self.outer_rows, checkpoint);
        }
    }

    fn attach(
        &mut self,
        frontend: &mut impl Frontend,
        signals: &Signals,
    ) -> io::Result<ForwardExit> {
        // A reconnecting frontend may have a different physical cell size.
        // Its initial resize will replace this before active panes are read.
        self.cell_pixels = None;
        self.graphics_support = None;
        let result = forward(
            frontend,
            &mut self.windows,
            signals,
            SessionContext {
                compact: self.compact,
                shell_path: &self.shell_path,
                session_name: self.session_name.as_deref(),
                notifications: self.notifications.clone(),
                scrollback_lines: self.scrollback_lines,
                shortcuts: self.shortcuts,
            },
            &mut self.outer_rows,
            AttachmentCapabilities {
                remain_on_exit: self.remain_on_exit,
                default_mode: self.default_mode,
                reload: self.reload.as_mut(),
                control: self.control.as_mut(),
                rename: self.rename.as_ref(),
                persistence: self.persistence.as_mut(),
                cell_pixels: &mut self.cell_pixels,
                graphics_support: &mut self.graphics_support,
                inherited_colors: &mut self.inherited_colors,
            },
            &mut self.closed,
        );
        if let Some(identity) = self.rename.as_ref() {
            self.session_name = Some(identity.name().as_str().into());
        }
        if let Some(config) = self.reload.as_ref().map(|reload| reload.current().clone()) {
            self.import_config(&config);
        }
        // A peer can disappear after a controller requests detach but before
        // the control frame is written. A named server keeps its panes alive.
        if self.rename.is_some() && result.as_ref().is_err_and(transport_closed) {
            return Ok(ForwardExit::Disconnected);
        }
        result
    }

    fn import_config(&mut self, config: &crate::config::Config) {
        self.compact = config.compact();
        self.default_mode = config.default_mode();
        self.shell_path = config.shell().clone();
        self.shortcuts = config.shortcuts();
        self.notifications = config.notifications();
        self.scrollback_lines = config.scrollback_lines();
        self.remain_on_exit = config.remain_on_exit();
    }

    fn reload_detached(&mut self) -> io::Result<()> {
        if let Some(reload) = self.reload.as_mut() {
            reload.poll();
        }
        if let Some(config) = self.reload.as_mut().and_then(|reload| reload.take()) {
            if self.compact != config.compact() {
                if let Err(error) =
                    validate_chrome_resize(&self.windows, self.outer_rows, config.compact())
                {
                    self.reload.as_mut().unwrap().reject(error);
                    return Ok(());
                }
                resize_chrome(&mut self.windows, self.outer_rows, config.compact())?;
            }
            apply_config(
                &config,
                &mut self.windows,
                &mut self.closed,
                self.persistence.as_mut(),
            );
            self.import_config(&config);
            self.reload.as_mut().unwrap().commit(config);
        }
        Ok(())
    }

    /// Keep every PTY live while waiting for the next session client.
    fn wait_for_client(
        &mut self,
        endpoint: &SessionEndpoint,
        signals: &Signals,
    ) -> io::Result<DetachedEvent> {
        loop {
            self.reload_detached()?;
            if let Some(service) = self.control.as_mut() {
                let refresh = service.tick(|request| {
                    if let crate::control::Request::DisconnectSession { server_pid } = request {
                        return Err(io::Error::other(
                            if server_pid != std::process::id() as i32 {
                                "session server changed; refresh and retry"
                            } else {
                                "session has no attached client"
                            },
                        ));
                    }
                    if let crate::control::Request::RenameSession {
                        source,
                        name,
                        server_pid,
                    } = request
                    {
                        if server_pid != std::process::id() as i32 {
                            return Err(io::Error::other(
                                "session server changed; refresh and retry",
                            ));
                        }
                        return self
                            .rename
                            .as_ref()
                            .ok_or_else(|| io::Error::other("rename unavailable"))?
                            .rename(
                                &source,
                                &name,
                                self.persistence
                                    .as_mut()
                                    .ok_or_else(|| io::Error::other("snapshots unavailable"))?,
                            );
                    }
                    control::handle(
                        request,
                        &mut self.windows,
                        SessionContext {
                            compact: self.compact,
                            shell_path: &self.shell_path,
                            session_name: self.session_name.as_deref(),
                            notifications: self.notifications.clone(),
                            scrollback_lines: self.scrollback_lines,
                            shortcuts: self.shortcuts,
                        },
                        self.outer_rows,
                        self.remain_on_exit,
                        self.reload.as_ref(),
                    )
                });
                if refresh {
                    for window in self.windows.iter_mut() {
                        window.content_mut().synchronize_sizes()?;
                    }
                }
            }
            if let Some(identity) = self.rename.as_ref() {
                self.session_name = Some(identity.name().as_str().into());
            }
            if let Some(service) = self.persistence.as_mut() {
                service.tick(&self.windows, self.outer_rows);
            }
            let received = signals.pending.load(Ordering::Relaxed);
            if received != 0 {
                return Ok(DetachedEvent::Process((128 + received) as u8));
            }
            if let Some(saved) = self.closed.as_mut()
                && !saved.service(self.cell_pixels)?
            {
                self.closed = None;
            }

            for window in self.windows.iter_mut() {
                for (_, pane) in window.content_mut().iter_mut() {
                    let retained = pane.retain_after_exit(self.remain_on_exit);
                    pane.observe_exit(retained)?;
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
                let pane = self
                    .windows
                    .get_mut(window_id)
                    .unwrap()
                    .content_mut()
                    .get_mut(pane_id)
                    .expect("polled pane exists");
                service_pane(
                    pane,
                    requested,
                    ready,
                    self.cell_pixels,
                    false,
                    ClipboardPolicy {
                        read: self
                            .reload
                            .as_ref()
                            .is_some_and(|r| r.current().clipboard_read()),
                        ..ClipboardPolicy::default()
                    },
                )?;
                // Completion while detached retains activity, never delivery for a later client.
                let _ = pane.take_command_reminder();
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
                        if !pane.retain_after_exit(self.remain_on_exit) {
                            finished.push((window.id(), pane_id, exit_code(status)));
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
    fn open_session_manager(&mut self) -> io::Result<bool>;
    fn detach_client(&mut self) -> io::Result<bool>;
    fn renamed(&mut self, _name: &str) -> io::Result<bool> {
        Ok(true)
    }
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

    fn open_session_manager(&mut self) -> io::Result<bool> {
        Ok(false)
    }

    fn detach_client(&mut self) -> io::Result<bool> {
        Ok(false)
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

    fn open_session_manager(&mut self) -> io::Result<bool> {
        self.send_session_manager()?;
        Ok(true)
    }

    fn detach_client(&mut self) -> io::Result<bool> {
        self.send_detach()?;
        Ok(true)
    }
    fn renamed(&mut self, name: &str) -> io::Result<bool> {
        self.send_renamed(name)
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
    RespawnPane,
    MoveLeft,
    MoveRight,
    Split(SplitAxis),
    NextPane,
    BreakPane,
    MovePanePreviousWindow,
    MovePaneNextWindow,
    JoinPane,
    ToggleZoom,
    UndoClose,
    History,
    HistoryEditor,
    LastCommandEditor,
    SessionManager,
    Detach,
    Help,
    FocusPane(Direction),
    ResizePane(Direction),
    MovePane(Direction),
    SwapPaneNext,
    SwapPanePrevious,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum InputMode {
    #[default]
    Locked,
    Normal,
    Pane,
    Resize,
    Move,
    Tab,
    Session,
    History,
}

impl InputMode {
    fn binding_mode(self) -> crate::config::BindingMode {
        use crate::config::BindingMode;
        match self {
            Self::Locked => BindingMode::Locked,
            Self::Normal => BindingMode::Normal,
            Self::Pane => BindingMode::Pane,
            Self::Resize => BindingMode::Resize,
            Self::Move => BindingMode::Move,
            Self::Tab => BindingMode::Tab,
            Self::Session => BindingMode::Session,
            Self::History => BindingMode::History,
        }
    }
}

#[derive(Default)]
struct WindowInput {
    mode: InputMode,
    shortcuts: crate::config::Shortcuts,
    session_available: bool,
    paste: bool,
    tail: VecDeque<u8>,
    mouse: Vec<u8>,
    mouse_since: Option<Instant>,
    pointer_position: Option<(usize, usize)>,
    pane_height: usize,
    pane_top: usize,
    pane_left: usize,
    pane_width: usize,
    mouse_tracking: MouseTracking,
    alternate_scroll: bool,
    application_cursor_keys: bool,
    kitty_keyboard_flags: u8,
    bar_enabled: bool,
    footer_row: Option<usize>,
    bar_press: bool,
    footer_press: bool,
    pane_press: bool,
    window_hitboxes: Vec<(usize, usize, usize)>,
    footer_hitboxes: Vec<(usize, usize, u8)>,
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

impl From<crate::config::HistoryMode> for InputMode {
    fn from(mode: crate::config::HistoryMode) -> Self {
        use crate::config::HistoryMode;
        match mode {
            HistoryMode::Locked => Self::Locked,
            HistoryMode::Normal => Self::Normal,
            HistoryMode::Pane => Self::Pane,
            HistoryMode::Resize => Self::Resize,
            HistoryMode::Move => Self::Move,
            HistoryMode::Tab => Self::Tab,
            HistoryMode::Session => Self::Session,
        }
    }
}

fn history_exit_input(
    view: &mut crate::history_view::HistoryView,
    shortcuts: crate::config::Shortcuts,
) -> WindowInput {
    WindowInput {
        mode: view
            .take_mode()
            .unwrap_or(crate::config::HistoryMode::Locked)
            .into(),
        shortcuts,
        ..WindowInput::default()
    }
}

impl WindowInput {
    fn default_input_mode(mode: crate::config::DefaultMode, session_available: bool) -> InputMode {
        use crate::config::DefaultMode;
        match mode {
            DefaultMode::Locked => InputMode::Locked,
            DefaultMode::Normal => InputMode::Normal,
            DefaultMode::Pane => InputMode::Pane,
            DefaultMode::Resize => InputMode::Resize,
            DefaultMode::Move => InputMode::Move,
            DefaultMode::Tab => InputMode::Tab,
            DefaultMode::Session if session_available => InputMode::Session,
            // The unnamed foreground entry point has no manager or detach target.
            DefaultMode::Session => InputMode::Locked,
        }
    }

    fn new(
        shortcuts: crate::config::Shortcuts,
        default_mode: crate::config::DefaultMode,
        session_available: bool,
    ) -> Self {
        Self {
            mode: Self::default_input_mode(default_mode, session_available),
            shortcuts,
            session_available,
            ..Self::default()
        }
    }

    fn can_reload(&self) -> bool {
        self.mode == InputMode::Locked
            && !self.paste
            && self.mouse.is_empty()
            && self.pane_drag.is_none()
            && !self.bar_press
            && !self.footer_press
            && !self.pane_press
            && !(1..6).any(|count| {
                self.tail
                    .iter()
                    .rev()
                    .take(count)
                    .copied()
                    .eq(b"\x1b[200~"[..count].iter().rev().copied())
            })
    }
    // Hold only candidate mouse reports. Escape alone is released after 30ms;
    // completed non-mouse sequences are forwarded as soon as they are known.
    fn feed(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        if self.paste
            || (self.mouse.is_empty()
                && (!(self.kitty_keyboard_flags != 0
                    || self.mouse_tracking != MouseTracking::Off
                    || self.alternate_scroll
                    || self.bar_enabled
                    || self.footer_row.is_some()
                    || self.pane_hitboxes.len() > 1
                    || (self.mode == InputMode::Normal && self.shortcuts.has_normal_arrows())
                    || matches!(
                        self.mode,
                        InputMode::Pane
                            | InputMode::Resize
                            | InputMode::Move
                            | InputMode::Tab
                            | InputMode::Session
                    ))
                    || byte != 27))
        {
            self.plain(byte, output);
            return;
        }
        self.mouse_since.get_or_insert_with(Instant::now);
        self.mouse.push(byte);
        let len = self.mouse.len();
        let pending = match self.mouse.as_slice() {
            [27] | [27, b'['] | [27, b'O'] => true,
            [27, b'[', b'M', ..] => len < 6,
            [27, b'[', b'<', rest @ ..] => {
                rest.last().is_none_or(|b| !matches!(b, b'M' | b'm' | b'u'))
                    && len < MAX_MOUSE_SEQUENCE_BYTES
            }
            [27, b'[', rest @ ..] => {
                rest.last().is_none_or(|byte| !(0x40..=0x7e).contains(byte))
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
                            && parts[0].parse::<u16>().is_ok()
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
        if self.mode == InputMode::Normal
            && let Some((direction, event_type)) = arrow_key_event(&bytes)
            && self.shortcuts.normal_arrow_binding(direction).is_some()
        {
            if event_type != 3 {
                self.normal_arrow_shortcut(direction, output);
            }
            return;
        }
        if let Some(key) = kitty_key_event(&bytes) {
            if key.shortcut_byte() == Some(self.shortcuts.locked_entry_key()) {
                if key.event_type != 3 {
                    self.plain(self.shortcuts.locked_entry_key(), output);
                }
                return;
            }
            if self.mode == InputMode::Locked
                && key
                    .shortcut_byte()
                    .is_some_and(|byte| self.shortcuts.enters_history_locked(byte))
            {
                if key.event_type != 3 {
                    output.push(WindowKey::History);
                }
                return;
            }
            if self.mode != InputMode::Locked {
                if key.event_type == 3 {
                    return;
                }
                if let Some(byte) = key.shortcut_byte() {
                    match self.mode {
                        InputMode::Normal => self.shortcut(byte, output),
                        InputMode::Pane => self.pane_shortcut(byte, output),
                        InputMode::Resize => self.resize_shortcut(byte, output),
                        InputMode::Move => self.move_shortcut(byte, output),
                        InputMode::Tab => self.tab_shortcut(byte, output),
                        InputMode::Session => self.session_shortcut(byte, output),
                        InputMode::History => {} // HistoryView owns local input while the snapshot is open.
                        InputMode::Locked => unreachable!(),
                    }
                } else {
                    if self.mode == InputMode::Normal {
                        self.mode = InputMode::Locked;
                        output.push(WindowKey::Byte(self.shortcuts.locked_entry_key()));
                        output.extend(bytes.into_iter().map(WindowKey::Byte));
                    }
                }
                return;
            }
        }
        if matches!(
            self.mode,
            InputMode::Pane
                | InputMode::Resize
                | InputMode::Move
                | InputMode::Tab
                | InputMode::Session
        ) && coordinates.is_none()
            && bytes.starts_with(b"\x1b")
        {
            if bytes == b"\x1b[200~" {
                // A paste belongs to the child, not to this modal keymap.
                self.mode = InputMode::Locked;
                self.paste = true;
                self.tail.clear();
                output.extend(bytes.into_iter().map(WindowKey::Byte));
                return;
            }
            if let Some((direction, event_type)) = arrow_key_event(&bytes)
                && event_type != 3
            {
                match self.mode {
                    InputMode::Pane => self.pane_arrow_shortcut(direction, output),
                    InputMode::Resize => self.resize_arrow_shortcut(direction, output),
                    InputMode::Move => self.move_arrow_shortcut(direction, output),
                    InputMode::Tab => self.tab_arrow_shortcut(direction, output),
                    InputMode::Session => {}
                    _ => unreachable!(),
                }
            }
            // Unknown escape sequences stay local to the active mode. In particular,
            // never pass an unbound arrow's trailing bytes to the child.
            return;
        }
        if let Some((column, row)) = coordinates {
            self.pointer_position = Some((column, row));
            // Unpressed motion is observational. It must not execute shortcuts
            // or leave a Rustmux input mode just because hover reporting is on.
            let hovering = mouse_motion(&bytes) && !motion_has_button(&bytes);
            if hovering && self.mode != InputMode::Locked {
                return;
            }
            let release = (bytes.starts_with(b"\x1b[<") && bytes.last() == Some(&b'm'))
                || (bytes.starts_with(b"\x1b[M")
                    && bytes[3]
                        .checked_sub(32)
                        .is_some_and(|button| button & 0x63 == 3));
            if self.footer_press {
                if release {
                    self.footer_press = false;
                }
                return;
            }
            let footer_mode = self.mode;
            if !hovering {
                self.mode = InputMode::Locked;
            }
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
            if self.footer_row == Some(row) {
                if !release && left_mouse_press(&bytes) {
                    self.footer_press = true;
                    if let Some((_, _, action)) = self
                        .footer_hitboxes
                        .iter()
                        .find(|(start, end, _)| column >= *start && column < *end)
                        .copied()
                    {
                        if self
                            .shortcuts
                            .uses_binding_hints(footer_mode.binding_mode())
                        {
                            self.mode = footer_mode;
                            let key = crate::config::HistoryKey::from_footer_code(action);
                            if let crate::config::HistoryKey::Byte(byte) = key {
                                match footer_mode {
                                    InputMode::Normal => self.shortcut(byte, output),
                                    InputMode::Pane => self.pane_shortcut(byte, output),
                                    InputMode::Resize => self.resize_shortcut(byte, output),
                                    InputMode::Move => self.move_shortcut(byte, output),
                                    InputMode::Tab => self.tab_shortcut(byte, output),
                                    InputMode::Session => self.session_shortcut(byte, output),
                                    InputMode::Locked
                                        if self.shortcuts.enters_history_locked(byte) =>
                                    {
                                        output.push(WindowKey::History)
                                    }
                                    InputMode::Locked
                                        if byte == self.shortcuts.locked_entry_key() =>
                                    {
                                        self.mode = InputMode::Normal
                                    }
                                    _ => {}
                                }
                            } else if let Some(direction) = match key {
                                crate::config::HistoryKey::Up => Some(Direction::Up),
                                crate::config::HistoryKey::Down => Some(Direction::Down),
                                crate::config::HistoryKey::Left => Some(Direction::Left),
                                crate::config::HistoryKey::Right => Some(Direction::Right),
                                _ => None,
                            } {
                                match footer_mode {
                                    InputMode::Normal => {
                                        self.normal_arrow_shortcut(direction, output)
                                    }
                                    InputMode::Pane => self.pane_arrow_shortcut(direction, output),
                                    InputMode::Resize => {
                                        self.resize_arrow_shortcut(direction, output)
                                    }
                                    InputMode::Move => self.move_arrow_shortcut(direction, output),
                                    InputMode::Tab => self.tab_arrow_shortcut(direction, output),
                                    _ => {}
                                }
                            }
                        } else if matches!(
                            footer_mode,
                            InputMode::Pane
                                | InputMode::Resize
                                | InputMode::Move
                                | InputMode::Tab
                                | InputMode::Session
                        ) {
                            self.mode = footer_mode;
                            match footer_mode {
                                InputMode::Pane => self.pane_shortcut(action, output),
                                InputMode::Resize => self.resize_shortcut(action, output),
                                InputMode::Move => self.move_shortcut(action, output),
                                InputMode::Tab => self.tab_shortcut(action, output),
                                InputMode::Session => self.session_shortcut(action, output),
                                _ => unreachable!(),
                            }
                        } else if action == 2 {
                            self.mode = InputMode::Normal;
                        } else if action == 20 && self.shortcuts.tab_entry_key().is_some() {
                            self.mode = InputMode::Tab;
                        } else if action == 15
                            && self.session_available
                            && self.shortcuts.session_entry_key().is_some()
                        {
                            self.mode = InputMode::Session;
                        } else if action == 23 {
                            self.mode = InputMode::Locked;
                            output.push(WindowKey::SessionManager);
                        } else {
                            self.mode = InputMode::Locked;
                            if let Some(command) = shortcut_action(action) {
                                output.push(command);
                            }
                        }
                    }
                }
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
            let child_row = row.saturating_sub(self.pane_top);
            let child_column = column.saturating_sub(self.pane_left);
            let inside = child_row != 0
                && child_row <= self.pane_height
                && child_column != 0
                && child_column <= self.pane_width;
            if self.mouse_tracking == MouseTracking::Off {
                if self.alternate_scroll
                    && inside
                    && let Some(sequence) =
                        alternate_scroll_key(&bytes, self.application_cursor_keys)
                {
                    output.extend(sequence.iter().copied().map(WindowKey::Byte));
                }
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
            if !inside && !release {
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
        } else {
            match self.mode {
                InputMode::Normal => self.shortcut(byte, output),
                InputMode::Pane => self.pane_shortcut(byte, output),
                InputMode::Resize => self.resize_shortcut(byte, output),
                InputMode::Move => self.move_shortcut(byte, output),
                InputMode::Tab => self.tab_shortcut(byte, output),
                InputMode::Session => self.session_shortcut(byte, output),
                InputMode::History => {} // HistoryView consumes all snapshot input.
                InputMode::Locked if self.shortcuts.enters_history_locked(byte) => {
                    output.push(WindowKey::History);
                }
                InputMode::Locked if byte == self.shortcuts.locked_entry_key() => {
                    self.mode = InputMode::Normal;
                }
                InputMode::Locked => output.push(WindowKey::Byte(byte)),
            }
        }
    }

    fn shortcut(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        if self.session_available && !self.shortcuts.clear_defaults() && byte == b'd' {
            self.mode = InputMode::Locked;
            output.push(WindowKey::Detach);
            return;
        }
        if self.session_available && self.shortcuts.resolve(byte) == Some(23) {
            self.mode = InputMode::Locked;
            output.push(WindowKey::SessionManager);
            return;
        }
        if self.shortcuts.enters_pane(byte) {
            self.mode = InputMode::Pane;
            return;
        }
        if self.shortcuts.enters_resize(byte) {
            self.mode = InputMode::Resize;
            return;
        }
        if self.shortcuts.enters_move(byte) {
            self.mode = InputMode::Move;
            return;
        }
        if self.shortcuts.enters_tab(byte) {
            self.mode = InputMode::Tab;
            return;
        }
        if self.session_available && self.shortcuts.enters_session(byte) {
            self.mode = InputMode::Session;
            return;
        }
        self.mode = InputMode::Locked;
        if self.shortcuts.exits_normal(byte) {
            return;
        }
        if byte == self.shortcuts.locked_entry_key() {
            output.push(WindowKey::Byte(byte));
            return;
        }
        if byte == 2 {
            output.push(WindowKey::Byte(self.shortcuts.locked_entry_key()));
            output.push(WindowKey::Byte(byte));
            return;
        }
        if let Some(action) = self.shortcuts.resolve(byte).and_then(shortcut_action) {
            output.push(action);
        } else {
            output.push(WindowKey::Byte(self.shortcuts.locked_entry_key()));
            output.push(WindowKey::Byte(byte));
        }
    }

    fn normal_arrow_shortcut(&mut self, direction: Direction, output: &mut Vec<WindowKey>) {
        let Some(binding) = self.shortcuts.normal_arrow_binding(direction) else {
            return;
        };
        self.mode = if binding.stay {
            InputMode::Normal
        } else {
            InputMode::Locked
        };
        output.push(WindowKey::FocusPane(binding.direction));
    }

    fn pane_shortcut(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        let Some(binding) = self.shortcuts.pane_binding(byte) else {
            return;
        };
        self.pane_binding_action(binding.action, binding.stay, output);
    }

    fn pane_arrow_shortcut(&mut self, direction: Direction, output: &mut Vec<WindowKey>) {
        let Some(binding) = self.shortcuts.pane_arrow_binding(direction) else {
            return;
        };
        self.pane_binding_action(binding.action, binding.stay, output);
    }

    fn pane_binding_action(
        &mut self,
        action: crate::config::PaneAction,
        stay: bool,
        output: &mut Vec<WindowKey>,
    ) {
        use crate::config::PaneAction;
        self.mode = if stay {
            InputMode::Pane
        } else {
            InputMode::Locked
        };
        match action {
            PaneAction::Help => {
                self.mode = InputMode::Pane;
                output.push(WindowKey::Help);
            }
            PaneAction::Break => output.push(WindowKey::BreakPane),
            PaneAction::MovePreviousWindow => output.push(WindowKey::MovePanePreviousWindow),
            PaneAction::MoveNextWindow => output.push(WindowKey::MovePaneNextWindow),
            PaneAction::SplitRight => output.push(WindowKey::Split(SplitAxis::Columns)),
            PaneAction::SplitDown => output.push(WindowKey::Split(SplitAxis::Rows)),
            PaneAction::FocusLeft => output.push(WindowKey::FocusPane(Direction::Left)),
            PaneAction::FocusDown => output.push(WindowKey::FocusPane(Direction::Down)),
            PaneAction::FocusUp => output.push(WindowKey::FocusPane(Direction::Up)),
            PaneAction::FocusRight => output.push(WindowKey::FocusPane(Direction::Right)),
            PaneAction::Next => output.push(WindowKey::NextPane),
            PaneAction::Zoom => output.push(WindowKey::ToggleZoom),
            PaneAction::Close => output.push(WindowKey::ClosePane),
            PaneAction::Respawn => output.push(WindowKey::RespawnPane),
            PaneAction::History => output.push(WindowKey::History),
            PaneAction::Normal => self.mode = InputMode::Normal,
            PaneAction::Resize => self.mode = InputMode::Resize,
            PaneAction::Move => self.mode = InputMode::Move,
            PaneAction::Tab => self.mode = InputMode::Tab,
            PaneAction::Session if self.session_available => self.mode = InputMode::Session,
            PaneAction::Session => self.mode = InputMode::Pane,
            PaneAction::Locked => self.mode = InputMode::Locked,
        }
    }

    fn resize_shortcut(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        let Some(binding) = self.shortcuts.resize_binding(byte) else {
            return;
        };
        self.resize_action(binding.action, output);
    }

    fn resize_arrow_shortcut(&mut self, direction: Direction, output: &mut Vec<WindowKey>) {
        let Some(action) = self.shortcuts.resize_arrow_action(direction) else {
            return;
        };
        self.resize_action(action, output);
    }

    fn resize_action(&mut self, action: crate::config::ResizeAction, output: &mut Vec<WindowKey>) {
        use crate::config::ResizeAction;
        match action {
            ResizeAction::Help => {
                self.mode = InputMode::Resize;
                output.push(WindowKey::Help);
            }
            ResizeAction::Resize(direction) => output.push(WindowKey::ResizePane(direction)),
            ResizeAction::History => output.push(WindowKey::History),
            ResizeAction::Normal => self.mode = InputMode::Normal,
            ResizeAction::Pane => self.mode = InputMode::Pane,
            ResizeAction::Move => self.mode = InputMode::Move,
            ResizeAction::Tab => self.mode = InputMode::Tab,
            ResizeAction::Session if self.session_available => self.mode = InputMode::Session,
            ResizeAction::Session => {}
            ResizeAction::Locked => self.mode = InputMode::Locked,
        }
    }

    fn move_shortcut(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        let Some(binding) = self.shortcuts.move_binding(byte) else {
            return;
        };
        self.move_action(binding.action, output);
    }

    fn move_arrow_shortcut(&mut self, direction: Direction, output: &mut Vec<WindowKey>) {
        let Some(action) = self.shortcuts.move_arrow_action(direction) else {
            return;
        };
        self.move_action(action, output);
    }

    fn move_action(&mut self, action: crate::config::MoveAction, output: &mut Vec<WindowKey>) {
        use crate::config::MoveAction;
        match action {
            MoveAction::Help => {
                self.mode = InputMode::Move;
                output.push(WindowKey::Help);
            }
            MoveAction::Move(direction) => output.push(WindowKey::MovePane(direction)),
            MoveAction::History => output.push(WindowKey::History),
            MoveAction::Normal => self.mode = InputMode::Normal,
            MoveAction::Pane => self.mode = InputMode::Pane,
            MoveAction::Resize => self.mode = InputMode::Resize,
            MoveAction::Tab => self.mode = InputMode::Tab,
            MoveAction::Session if self.session_available => self.mode = InputMode::Session,
            MoveAction::Session => {}
            MoveAction::Locked => self.mode = InputMode::Locked,
        }
    }

    fn tab_shortcut(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        let Some(binding) = self.shortcuts.tab_binding(byte) else {
            return;
        };
        self.tab_binding_action(binding.action, binding.stay, output);
    }

    fn tab_arrow_shortcut(&mut self, direction: Direction, output: &mut Vec<WindowKey>) {
        let Some(binding) = self.shortcuts.tab_arrow_binding(direction) else {
            return;
        };
        self.tab_binding_action(binding.action, binding.stay, output);
    }

    fn tab_binding_action(
        &mut self,
        action: crate::config::TabAction,
        stay: bool,
        output: &mut Vec<WindowKey>,
    ) {
        use crate::config::TabAction;
        self.mode = if stay {
            InputMode::Tab
        } else {
            InputMode::Locked
        };
        match action {
            TabAction::Next => output.push(WindowKey::Next),
            TabAction::Previous => output.push(WindowKey::Previous),
            TabAction::MoveLeft => output.push(WindowKey::MoveLeft),
            TabAction::MoveRight => output.push(WindowKey::MoveRight),
            TabAction::New => output.push(WindowKey::Create),
            TabAction::Rename => output.push(WindowKey::Rename),
            TabAction::Close => output.push(WindowKey::Close),
            TabAction::Select(index) => output.push(WindowKey::Select(index - 1)),
            TabAction::Help => output.push(WindowKey::Help),
            TabAction::History => output.push(WindowKey::History),
            TabAction::Normal => self.mode = InputMode::Normal,
            TabAction::Pane => self.mode = InputMode::Pane,
            TabAction::Resize => self.mode = InputMode::Resize,
            TabAction::Move => self.mode = InputMode::Move,
            TabAction::Session if self.session_available => self.mode = InputMode::Session,
            TabAction::Session => self.mode = InputMode::Tab,
            TabAction::Locked => self.mode = InputMode::Locked,
        }
    }

    fn session_shortcut(&mut self, byte: u8, output: &mut Vec<WindowKey>) {
        let Some(binding) = self.shortcuts.session_binding(byte) else {
            return;
        };
        use crate::config::SessionAction;
        match binding.action {
            SessionAction::Detach => {
                self.mode = InputMode::Locked;
                output.push(WindowKey::Detach);
            }
            SessionAction::Manager => {
                self.mode = InputMode::Locked;
                output.push(WindowKey::SessionManager);
            }
            SessionAction::Help => output.push(WindowKey::Help),
            SessionAction::History => output.push(WindowKey::History),
            SessionAction::Normal => self.mode = InputMode::Normal,
            SessionAction::Pane => self.mode = InputMode::Pane,
            SessionAction::Resize => self.mode = InputMode::Resize,
            SessionAction::Move => self.mode = InputMode::Move,
            SessionAction::Tab => self.mode = InputMode::Tab,
            SessionAction::Locked => self.mode = InputMode::Locked,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KittyKeyEvent {
    codepoint: u32,
    shifted: Option<u32>,
    base_layout: Option<u32>,
    modifiers: u16,
    event_type: u8,
}

impl KittyKeyEvent {
    fn shortcut_byte(self) -> Option<u8> {
        if self.modifiers & !(1 | 4) != 0 {
            return None;
        }
        let codepoint = if self.modifiers & 1 != 0 {
            self.shifted.or_else(|| {
                self.base_layout.map(|codepoint| {
                    u8::try_from(codepoint)
                        .ok()
                        .filter(u8::is_ascii)
                        .map_or(codepoint, |byte| u32::from(byte.to_ascii_uppercase()))
                })
            })
        } else {
            self.base_layout
        }
        .unwrap_or(self.codepoint);
        if self.modifiers & 4 != 0 {
            let byte = u8::try_from(codepoint).ok()?;
            return byte.is_ascii().then_some(byte.to_ascii_uppercase() & 0x1f);
        }
        u8::try_from(codepoint).ok().filter(u8::is_ascii)
    }
}

fn kitty_key_event(sequence: &[u8]) -> Option<KittyKeyEvent> {
    let parameters = sequence.strip_prefix(b"\x1b[")?.strip_suffix(b"u")?;
    let text = std::str::from_utf8(parameters).ok()?;
    let mut fields = text.split(';');
    let mut key = fields.next()?.split(':');
    let codepoint = key.next()?.parse().ok()?;
    let parse_optional = |value: Option<&str>| match value {
        Some("") | None => Some(None),
        Some(value) => value.parse().ok().map(Some),
    };
    let shifted = parse_optional(key.next())?;
    let base_layout = parse_optional(key.next())?;
    if key.next().is_some() {
        return None;
    }
    let mut modifier_field = fields.next().unwrap_or("").split(':');
    let encoded_modifiers = match modifier_field.next() {
        Some("") | None => 1,
        Some(value) => value.parse::<u16>().ok()?,
    };
    let modifiers = encoded_modifiers.checked_sub(1)?;
    let event_type = match modifier_field.next() {
        Some("") | None => 1,
        Some(value) => value.parse().ok()?,
    };
    if modifier_field.next().is_some() || !matches!(event_type, 1..=3) {
        return None;
    }
    let _text = fields.next();
    if fields.next().is_some() {
        return None;
    }
    Some(KittyKeyEvent {
        codepoint,
        shifted,
        base_layout,
        modifiers,
        event_type,
    })
}

fn arrow_key_event(sequence: &[u8]) -> Option<(Direction, u8)> {
    let (parameters, key) = if let Some(rest) = sequence.strip_prefix(b"\x1b[") {
        let (&key, parameters) = rest.split_last()?;
        (parameters, key)
    } else {
        let key = *sequence.strip_prefix(b"\x1bO")?.first()?;
        if sequence.len() != 3 {
            return None;
        }
        (&[][..], key)
    };
    let direction = match key {
        b'A' => Direction::Up,
        b'B' => Direction::Down,
        b'C' => Direction::Right,
        b'D' => Direction::Left,
        _ => return None,
    };
    let event_type = match parameters {
        b"" | b"1" | b"1;1" | b"1;1:1" => 1,
        b"1;1:2" => 2,
        b"1;1:3" => 3,
        _ => return None,
    };
    Some((direction, event_type))
}

fn shortcut_action(byte: u8) -> Option<WindowKey> {
    Some(match byte {
        b'c' => WindowKey::Create,
        b'n' => WindowKey::Next,
        b'p' => WindowKey::Previous,
        b'\t' => WindowKey::Last,
        b'&' => WindowKey::Close,
        b'x' => WindowKey::ClosePane,
        b'R' => WindowKey::RespawnPane,
        b'<' => WindowKey::MoveLeft,
        b'>' => WindowKey::MoveRight,
        b'%' => WindowKey::Split(SplitAxis::Columns),
        b'"' => WindowKey::Split(SplitAxis::Rows),
        b'{' => WindowKey::SwapPanePrevious,
        b'}' => WindowKey::SwapPaneNext,
        b'!' => WindowKey::BreakPane,
        b'm' => WindowKey::JoinPane,
        b'o' => WindowKey::NextPane,
        b'Z' => WindowKey::ToggleZoom,
        b'z' => WindowKey::UndoClose,
        b'[' => WindowKey::History,
        b'E' => WindowKey::HistoryEditor,
        b'e' => WindowKey::LastCommandEditor,
        b'?' => WindowKey::Help,
        8 => WindowKey::ResizePane(Direction::Left),
        10 => WindowKey::ResizePane(Direction::Down),
        11 => WindowKey::ResizePane(Direction::Up),
        12 => WindowKey::ResizePane(Direction::Right),
        b'h' => WindowKey::FocusPane(Direction::Left),
        b'j' => WindowKey::FocusPane(Direction::Down),
        b'k' => WindowKey::FocusPane(Direction::Up),
        b'l' => WindowKey::FocusPane(Direction::Right),
        b',' => WindowKey::Rename,
        b'1'..=b'9' => WindowKey::Select(usize::from(byte - b'1')),
        b'0' => WindowKey::Select(9),
        2 => WindowKey::Byte(2),
        _ => return None,
    })
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

fn alternate_scroll_key(bytes: &[u8], application: bool) -> Option<&'static [u8]> {
    match (mouse_button(bytes)? & 0b1100_0011, application) {
        (64, false) => Some(b"\x1b[A"),
        (65, false) => Some(b"\x1b[B"),
        (64, true) => Some(b"\x1bOA"),
        (65, true) => Some(b"\x1bOB"),
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
    notifications: crate::config::Notifications,
    scrollback_lines: usize,
) -> io::Result<PaneSet<Pane>> {
    let (content_rows, content_columns) = pane_content_dimensions(rows, columns);
    PaneSet::new(
        rows,
        columns,
        Pane::spawn_in(
            shell,
            directory,
            content_rows,
            content_columns,
            notifications,
            scrollback_lines,
        )?,
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

fn active_focus(windows: &Windows<PaneSet<Pane>>) -> (WindowId, PaneId) {
    let window = windows.active().expect("at least one window");
    (window.id(), window.content().layout().active())
}

fn queue_focus_event(pane: &mut Pane, focused: bool) {
    if !pane.screen().focus_reporting() {
        return;
    }
    let state = pane.parts_mut().3;
    let sequence = if focused { b"\x1b[I" } else { b"\x1b[O" };
    if state.accepts_input() && state.to_shell.len() <= LIMIT - sequence.len() {
        state.to_shell.extend(sequence);
    }
}

fn queue_focus_transition(
    windows: &mut Windows<PaneSet<Pane>>,
    old: (WindowId, PaneId),
    new: (WindowId, PaneId),
) {
    if old == new {
        return;
    }
    if let Some(pane) = windows
        .get_mut(old.0)
        .and_then(|window| window.content_mut().get_mut(old.1))
    {
        queue_focus_event(pane, false);
    }
    if let Some(pane) = windows
        .get_mut(new.0)
        .and_then(|window| window.content_mut().get_mut(new.1))
    {
        queue_focus_event(pane, true);
    }
}

fn submits_command(byte: u8, bracketed_paste: bool) -> bool {
    matches!(byte, b'\r' | b'\n') && !bracketed_paste
}

fn spawn_editor_window(
    text: &str,
    rows: u16,
    columns: u16,
    scrollback_lines: usize,
) -> io::Result<PaneSet<Pane>> {
    let (content_rows, content_columns) = pane_content_dimensions(rows, columns);
    PaneSet::new(
        rows,
        columns,
        Pane::spawn_editor(text, content_rows, content_columns, scrollback_lines)?,
    )
}

/// Open only the editor staged by a History binding; existing spawn/cleanup owns its file.
fn dispatch_history_editor(
    editor: Option<(&'static str, String)>,
    windows: &mut Windows<PaneSet<Pane>>,
    scrollback_lines: usize,
    to_terminal: &mut VecDeque<u8>,
) -> io::Result<()> {
    let Some((name, text)) = editor else {
        return Ok(());
    };
    if windows.iter().len() == MAX_WINDOWS {
        if to_terminal.is_empty() {
            to_terminal.push_back(7);
        }
        return Ok(());
    }
    let old = active_focus(windows);
    let (rows, columns) = windows.active().unwrap().content().layout().dimensions();
    match spawn_editor_window(&text, rows, columns, scrollback_lines) {
        Ok(pane) => {
            windows.create(name.into(), pane)?;
            queue_focus_transition(windows, old, active_focus(windows));
            windows
                .active_mut()
                .unwrap()
                .content_mut()
                .synchronize_sizes()?;
        }
        Err(_) if to_terminal.is_empty() => to_terminal.push_back(7),
        Err(_) => {}
    }
    Ok(())
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

fn transport_closed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
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

fn window_names(windows: &Windows<PaneSet<Pane>>) -> Vec<String> {
    windows
        .iter()
        .map(|window| {
            let mut name = window.name().to_owned();
            if window
                .content()
                .iter()
                .any(|(_, pane)| pane.io().bell_pending)
            {
                name.push_str(" [!]");
            }
            name
        })
        .collect()
}

#[derive(Default)]
struct ClipboardPolicy {
    attached: bool,
    read: bool,
    file_transfer: bool,
    drag_source: bool,
    drop_target: bool,
    write: bool,
}

fn service_pane(
    pane: &mut Pane,
    requested: PollFlags,
    ready: PollFlags,
    cell_pixels: Option<CellPixelSize>,
    answer_graphics: bool,
    clipboard: ClipboardPolicy,
) -> io::Result<()> {
    pane.parts_mut().2.configure_rich_clipboard(clipboard.read);
    pane.configure_clipboard(clipboard.attached && clipboard.write);
    pane.configure_rich_clipboard(clipboard.attached.then_some(clipboard.read));
    pane.configure_rich_clipboard_write(clipboard.attached.then_some(clipboard.write));
    pane.configure_file_transfer(clipboard.attached.then_some(clipboard.file_transfer));
    pane.configure_drag_source(clipboard.attached.then_some(clipboard.drag_source));
    pane.configure_drop_target(
        clipboard.attached.then_some(clipboard.drop_target),
        clipboard.drag_source,
    );
    pane.track_command_application();
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
                    pane.process_output_for_runtime(
                        &bytes[..count],
                        &mut |reply| replies.extend_from_slice(reply),
                        cell_pixels,
                        answer_graphics,
                    );
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

struct RuntimeConfig {
    compact: bool,
    default_mode: crate::config::DefaultMode,
    clipboard_write: bool,
    clipboard_read: bool,
    file_transfer: bool,
    drag_source: bool,
    drop_target: bool,
    theme: crate::theme::Theme,
    mouse_hover_cursor: bool,
    shell: OsString,
    notifications: crate::config::Notifications,
    scrollback_lines: usize,
    shortcuts: crate::config::Shortcuts,
    remain_on_exit: bool,
}
impl RuntimeConfig {
    fn update(&mut self, config: &crate::config::Config) {
        self.compact = config.compact();
        self.default_mode = config.default_mode();
        self.theme = config.theme();
        self.mouse_hover_cursor = config.mouse_hover_cursor();
        self.clipboard_write = config.clipboard_write();
        self.clipboard_read = config.clipboard_read();
        self.file_transfer = config.file_transfer();
        self.drag_source = config.drag_source();
        self.drop_target = config.drop_target();
        self.shell = config.shell().clone();
        self.notifications = config.notifications();
        self.scrollback_lines = config.scrollback_lines();
        self.shortcuts = config.shortcuts();
        self.remain_on_exit = config.remain_on_exit();
    }
}

fn validate_chrome_resize(
    windows: &Windows<PaneSet<Pane>>,
    outer_rows: u16,
    compact: bool,
) -> io::Result<()> {
    for window in windows.iter() {
        let set = window.content();
        set.validate_resize(
            pane_rows_for_layout(outer_rows, compact),
            set.layout().dimensions().1,
        )?;
    }
    Ok(())
}

fn resize_chrome(
    windows: &mut Windows<PaneSet<Pane>>,
    outer_rows: u16,
    compact: bool,
) -> io::Result<()> {
    for window in windows.iter_mut() {
        let set = window.content_mut();
        set.resize(
            pane_rows_for_layout(outer_rows, compact),
            set.layout().dimensions().1,
        )?;
        set.synchronize_sizes()?;
    }
    Ok(())
}

fn apply_config(
    config: &crate::config::Config,
    windows: &mut Windows<PaneSet<Pane>>,
    closed: &mut Option<crate::closed_pane::ClosedPane>,
    persistence: Option<&mut crate::session::snapshot::SnapshotService>,
) {
    for window in windows.iter_mut() {
        for (_, pane) in window.content_mut().iter_mut() {
            pane.configure_notifications(config.notifications());
            if !config.drop_target() {
                pane.configure_drop_target(Some(false), config.drag_source());
            }
            if !config.drag_source() {
                pane.configure_drag_source(Some(false));
            }
            if !config.file_transfer() {
                pane.configure_file_transfer(Some(false));
            }
            pane.parts_mut()
                .2
                .configure_rich_clipboard(config.clipboard_read());
            if !config.clipboard_read() {
                pane.configure_rich_clipboard(Some(false));
            }
            if !config.clipboard_write() {
                pane.configure_clipboard(false);
                pane.configure_rich_clipboard_write(Some(false));
            }
        }
    }
    if let Some(pane) = closed.as_mut().and_then(|saved| saved.pane.as_mut()) {
        pane.configure_notifications(config.notifications());
        pane.configure_clipboard(false);
        pane.configure_rich_clipboard(None);
        pane.configure_rich_clipboard_write(None);
        pane.configure_file_transfer(None);
        pane.configure_drag_source(None);
        pane.configure_drop_target(None, false);
        pane.parts_mut().2.configure_rich_clipboard(false);
    }
    if let Some(service) = persistence {
        service.configure(config.persistence(), config.scrollback_lines());
    }
}

fn forward(
    frontend: &mut impl Frontend,
    windows: &mut Windows<PaneSet<Pane>>,
    signals: &Signals,
    context: SessionContext<'_>,
    outer_rows: &mut u16,
    capabilities: AttachmentCapabilities<'_>,
    closed: &mut Option<crate::closed_pane::ClosedPane>,
) -> io::Result<ForwardExit> {
    let AttachmentCapabilities {
        remain_on_exit,
        default_mode,
        mut reload,
        mut control,
        rename,
        mut persistence,
        cell_pixels,
        graphics_support,
        inherited_colors,
    } = capabilities;
    let mut session_name = context.session_name.map(str::to_owned);
    let mut renamed_notice: Option<String> = None;
    // A new attachment cannot complete a write captured for the previous client.
    for window in windows.iter_mut() {
        for (_, pane) in window.content_mut().iter_mut() {
            pane.configure_clipboard(false);
            pane.configure_rich_clipboard(None);
            pane.configure_rich_clipboard_write(None);
            pane.configure_file_transfer(None);
            pane.configure_drag_source(None);
            pane.configure_drop_target(None, false);
        }
    }
    let mut runtime = RuntimeConfig {
        compact: context.compact,
        default_mode,
        clipboard_read: reload
            .as_ref()
            .is_some_and(|r| r.current().clipboard_read()),
        file_transfer: reload.as_ref().is_some_and(|r| r.current().file_transfer()),
        drag_source: reload.as_ref().is_some_and(|r| r.current().drag_source()),
        drop_target: reload.as_ref().is_some_and(|r| r.current().drop_target()),
        clipboard_write: reload
            .as_ref()
            .is_some_and(|r| r.current().clipboard_write()),
        mouse_hover_cursor: reload
            .as_ref()
            .is_some_and(|r| r.current().mouse_hover_cursor()),
        theme: reload
            .as_ref()
            .map_or_else(crate::theme::Theme::default, |r| r.current().theme()),
        shell: context.shell_path.to_owned(),
        notifications: context.notifications,
        scrollback_lines: context.scrollback_lines,
        shortcuts: context.shortcuts,
        remain_on_exit,
    };
    let shortcuts = runtime.shortcuts;
    let mut renderer = Renderer::default();
    let mut kitty_overlays = KittyOverlays::default();
    let mut notification_ids = crate::notification::Ids::default();
    let mut outer_image_replies = OuterImageReplies::default();
    let mut rich_clipboard = crate::rich_clipboard::Router::default();
    let mut file_transfer = crate::file_transfer::Router::default();
    let mut drag_source = crate::drag_source::Router::default();
    let mut drop_target = crate::drop_target::Router::default();
    let mut graphics_ready = false;
    let mut to_terminal = VecDeque::new();
    let mut color_probe = Some(ColorProbe::new());
    *inherited_colors = color_probe.as_ref().unwrap().colors();
    to_terminal.extend(color_probe.as_ref().unwrap().request_bytes());
    let probe = GraphicsCapabilityProbe::new(GRAPHICS_PROBE_IMAGE_ID);
    to_terminal.extend(probe.request_bytes());
    let mut graphics_probe = Some(probe);
    let mut graphics_probe_deadline = None;
    let mut shm_probe_started = false;
    let mut shm_probe: Option<GraphicsCapabilityProbe> = None;
    let mut shm_probe_object: Option<SharedPixels> = None;
    let mut shm_probe_deadline = None;
    let mut shm_support = None;
    let mut input = VecDeque::new();
    let mut keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
    let mut actions = Vec::new();
    let mut next_frame = Instant::now();
    let mut force_redraw = true;
    let mut bar_dirty = false;
    let mut prompt: Option<WindowPrompt> = None;
    let mut history: Option<crate::history_view::HistoryView> = None;
    let mut help: Option<crate::shortcut_help::ShortcutHelp> = None;
    let mut help_return_mode = InputMode::Locked;
    let mut help_action_mode = InputMode::Locked;
    let mut close_requested = None;
    let mut connection = ConnectionState::Attached;
    let mut session_manager_requested = false;
    let mut detach_requested = false;
    let mut client_detach_pending = false;
    let mut pane_resize_pending: Option<(WindowId, Instant)> = None;
    let mut pending_outer_resize = None;
    let mut save_error = None;
    let mut reload_error = None;
    loop {
        if connection == ConnectionState::Detached {
            // The client has stopped sending input but still drains final output.
            // Preserve preceding input and flush clipboard aborts before acknowledging.
            client_detach_pending = true;
            detach_requested = true;
            connection = ConnectionState::Attached;
        }
        if let Some(service) = reload.as_deref_mut() {
            service.poll();
            if service.pending()
                && input.is_empty()
                && keys.can_reload()
                && history.is_none()
                && help.is_none()
                && prompt.is_none()
                && let Some(config) = service.take()
            {
                let validation = if runtime.compact != config.compact() {
                    validate_chrome_resize(windows, *outer_rows, config.compact())
                } else {
                    Ok(())
                };
                match validation {
                    Err(error) => service.reject(error),
                    Ok(()) => {
                        if runtime.compact != config.compact() {
                            resize_chrome(windows, *outer_rows, config.compact())?;
                        }
                        apply_config(&config, windows, closed, persistence.as_deref_mut());
                        runtime.update(&config);
                        keys.shortcuts = runtime.shortcuts;
                        service.commit(config);
                        renderer.invalidate();
                        force_redraw = true;
                        bar_dirty = true;
                    }
                }
            }
            let error = service.error().map(str::to_owned);
            if reload_error != error {
                reload_error = error;
                force_redraw = true;
            }
        }
        let shell_path = runtime.shell.as_os_str();
        let notifications = runtime.notifications.clone();
        let scrollback_lines = runtime.scrollback_lines;
        let shortcuts = runtime.shortcuts;
        let default_mode = runtime.default_mode;
        let remain_on_exit = runtime.remain_on_exit;
        let context = SessionContext {
            compact: runtime.compact,
            shell_path,
            session_name: session_name.as_deref(),
            notifications: notifications.clone(),
            scrollback_lines,
            shortcuts,
        };
        if let Some(service) = control.as_mut() {
            let old = active_focus(windows);
            if service.tick(|request| {
                if let crate::control::Request::DisconnectSession { server_pid } = request {
                    if server_pid != std::process::id() as i32 {
                        return Err(io::Error::other(
                            "session server changed; refresh and retry",
                        ));
                    }
                    if connection != ConnectionState::Attached {
                        return Err(io::Error::other("session has no attached client"));
                    }
                    detach_requested = true;
                    return Ok(String::new());
                }
                if let crate::control::Request::RenameSession {
                    source,
                    name,
                    server_pid,
                } = request
                {
                    if server_pid != std::process::id() as i32 {
                        return Err(io::Error::other(
                            "session server changed; refresh and retry",
                        ));
                    }
                    return rename
                        .ok_or_else(|| io::Error::other("rename unavailable"))?
                        .rename(
                            &source,
                            &name,
                            persistence
                                .as_deref_mut()
                                .ok_or_else(|| io::Error::other("snapshots unavailable"))?,
                        );
                }
                control::handle(
                    request,
                    windows,
                    context.clone(),
                    *outer_rows,
                    remain_on_exit,
                    reload.as_deref(),
                )
            }) {
                queue_focus_transition(windows, old, active_focus(windows));
                // Script selection or resizing can change an inactive window.
                // Keep all child sizes synchronized, and propagate I/O failure to
                // the normal runtime cleanup rather than a controller reply.
                for window in windows.iter_mut() {
                    window.content_mut().synchronize_sizes()?;
                }
                history = None;
                help = None;
                prompt = None;
                keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
                pane_resize_pending = None;
                renderer.invalidate();
                force_redraw = true;
                bar_dirty = true;
            }
        }
        if let Some(identity) = rename {
            let latest = identity.name().as_str().to_owned();
            if session_name.as_ref() != Some(&latest) {
                session_name = Some(latest.clone());
                renamed_notice = Some(latest);
                renderer.invalidate();
                force_redraw = true;
                bar_dirty = true;
            }
        }
        if let Some(service) = persistence.as_mut() {
            service.tick(windows, *outer_rows);
            let current = service.error().map(str::to_owned);
            if save_error != current {
                save_error = current;
                force_redraw = true;
            }
        }
        if graphics_probe_deadline.is_none() && to_terminal.is_empty() {
            graphics_probe_deadline = Some(Instant::now() + GRAPHICS_PROBE_TIMEOUT);
        }
        if graphics_probe_deadline.is_some_and(|deadline| Instant::now() >= deadline)
            && let Some(mut probe) = graphics_probe.take()
        {
            let mut released = Vec::new();
            *graphics_support = probe.finish(&mut released);
            input.extend(released);
        }
        if shm_probe_deadline.is_some_and(|deadline| Instant::now() >= deadline)
            && let Some(mut probe) = shm_probe.take()
        {
            let mut released = Vec::new();
            shm_support = probe.finish(&mut released);
            drop(shm_probe_object.take());
            input.extend(released);
        }
        let mut expired = Vec::new();
        rich_clipboard.tick(
            Instant::now(),
            runtime.clipboard_read,
            runtime.clipboard_write,
            |owner| rich_clipboard_live(windows, owner),
            &mut expired,
            &mut to_terminal,
        );
        file_transfer.tick(
            Instant::now(),
            runtime.file_transfer && connection == ConnectionState::Attached,
            |owner| rich_clipboard_live(windows, owner),
        );
        drag_source.tick(
            Instant::now(),
            runtime.drag_source
                && connection == ConnectionState::Attached
                && !detach_requested
                && !session_manager_requested,
            keys.mode == InputMode::Locked
                && prompt.is_none()
                && history.is_none()
                && help.is_none(),
            drag_source_view(windows, *outer_rows, *cell_pixels),
            |owner| rich_clipboard_live(windows, owner),
        );
        drop_target.tick(
            Instant::now(),
            runtime.drop_target
                && connection == ConnectionState::Attached
                && !detach_requested
                && !session_manager_requested,
            keys.mode == InputMode::Locked
                && prompt.is_none()
                && history.is_none()
                && help.is_none(),
            &drop_target_views(windows, *outer_rows, *cell_pixels),
            |owner| rich_clipboard_live(windows, owner),
        );
        drop_target.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
        drop_target.pump(&mut to_terminal, LIMIT);
        drag_source.pump(&mut to_terminal, LIMIT);
        file_transfer.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
        file_transfer.pump(Instant::now(), &mut to_terminal, LIMIT);
        let old_input_len = input.len();
        input.extend(expired);
        let paste_enabled = connection == ConnectionState::Attached
            && runtime.clipboard_read
            && keys.mode == InputMode::Locked
            && prompt.is_none()
            && history.is_none()
            && help.is_none();
        rich_clipboard.tick_paste(Instant::now(), paste_enabled, |owner| {
            rich_paste_live(windows, owner)
        });
        rich_clipboard.set_paste_target(rich_paste_target(windows, paste_enabled));
        rich_clipboard.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
        // Released prefixes have already passed the clipboard framer, but still
        // belong to the downstream color/graphics probes, not keyboard input.
        let rich_input_len = input.len();
        if rich_clipboard.can_receive()
            && file_transfer.can_receive()
            && drag_source.can_receive()
            && drop_target.can_receive()
        {
            frontend.drain_input(&mut input);
        }
        filter_rich_clipboard_input(
            &mut rich_clipboard,
            &mut file_transfer,
            &mut drag_source,
            &mut drop_target,
            (
                &drop_target_views(windows, *outer_rows, *cell_pixels),
                drag_source_view(windows, *outer_rows, *cell_pixels),
            ),
            &mut input,
            rich_input_len,
        );
        filter_color_probe_input(
            &mut color_probe,
            &mut input,
            old_input_len,
            inherited_colors,
        );
        filter_graphics_probe_input(
            &mut graphics_probe,
            &mut input,
            old_input_len,
            graphics_support,
        );
        filter_graphics_probe_input(&mut shm_probe, &mut input, old_input_len, &mut shm_support);
        finish_color_probe_at_barrier(&mut color_probe, &graphics_probe, &mut input);
        force_redraw |= inherit_pane_colors(windows, closed, history.as_mut(), inherited_colors);
        if !graphics_ready && *graphics_support == Some(GraphicsSupport::Supported) {
            graphics_ready = true;
            force_redraw = true;
        }
        if graphics_ready
            && !shm_probe_started
            && graphics_probe
                .as_ref()
                .is_none_or(GraphicsCapabilityProbe::complete)
        {
            shm_probe_started = true;
            if let Ok(object) = SharedPixels::create(&[0, 0, 0]) {
                to_terminal.extend(object.query_command(SHM_PROBE_IMAGE_ID.get()));
                to_terminal.extend(b"\x1b[c");
                shm_probe = Some(GraphicsCapabilityProbe::new_strict(SHM_PROBE_IMAGE_ID));
                shm_probe_object = Some(object);
            }
        }
        if shm_probe.is_some() && shm_probe_deadline.is_none() && to_terminal.is_empty() {
            shm_probe_deadline = Some(Instant::now() + GRAPHICS_PROBE_TIMEOUT);
        }
        if shm_probe
            .as_ref()
            .is_some_and(GraphicsCapabilityProbe::complete)
        {
            shm_probe = None;
            drop(shm_probe_object.take());
        }
        if !kitty_overlays.shared_memory_supported()
            && shm_support == Some(GraphicsSupport::Supported)
        {
            kitty_overlays.enable_shared_memory(&mut to_terminal)?;
            force_redraw = true;
        }
        if connection != ConnectionState::Attached {
            // No peer can consume an old physical frame. Dropping it also lets
            // history-mode input that preceded Detach continue in order.
            to_terminal.clear();
        }
        if let Some(exit) = frontend_exit(connection, &input) {
            drop_target.cancel_all();
            drop_target.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
            rich_clipboard.cancel(&mut to_terminal, |owner, bytes| {
                deliver_rich_clipboard(windows, owner, bytes)
            });
            return Ok(exit);
        }
        let received = signals.pending.load(Ordering::Relaxed);
        if received != 0 {
            file_transfer.cancel_all();
            let _ = flush_protocol_exit(
                frontend,
                &mut file_transfer,
                &mut drag_source,
                &mut drop_target,
                &mut to_terminal,
            );
            return Ok(ForwardExit::Process((128 + received) as u8));
        }
        if close_requested.is_some() && to_terminal.is_empty() {
            let (id, pane_id) = close_requested.take().unwrap();
            // Finish the already encoded physical frame before changing ownership.
            if windows.get(id).is_some() {
                let old = active_focus(windows);
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
                        file_transfer.cancel_all();
                        flush_protocol_exit(
                            frontend,
                            &mut file_transfer,
                            &mut drag_source,
                            &mut drop_target,
                            &mut to_terminal,
                        )?;
                        return Ok(ForwardExit::Process(0));
                    }
                    let pane = windows
                        .get_mut(id)
                        .unwrap()
                        .content_mut()
                        .get_mut(pane_id)
                        .unwrap();
                    rich_clipboard.forget(pane.rich_clipboard_owner(), &mut to_terminal);
                    pane.stop_for_hide()?;
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
                        file_transfer.cancel_all();
                        flush_protocol_exit(
                            frontend,
                            &mut file_transfer,
                            &mut drag_source,
                            &mut drop_target,
                            &mut to_terminal,
                        )?;
                        return Ok(ForwardExit::Process(0));
                    }
                    for (_, pane) in windows.get_mut(id).unwrap().content_mut().iter_mut() {
                        rich_clipboard.forget(pane.rich_clipboard_owner(), &mut to_terminal);
                        pane.shell_mut().terminate()?;
                    }
                    drop(windows.close(id)?);
                }
                bar_dirty = true;
                queue_focus_transition(windows, old, active_focus(windows));
                input.clear();
                keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
                prompt = None;
                help = None;
                renderer.invalidate();
                force_redraw = true;
                continue;
            }
        }
        if prompt
            .as_ref()
            .is_some_and(|prompt| prompt.cancel_due(Instant::now()))
        {
            let return_to_tab = prompt.as_ref().is_some_and(|editor| {
                editor.kind == PromptKind::Rename && keys.mode == InputMode::Tab
            });
            prompt = None;
            keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
            if return_to_tab {
                keys.mode = InputMode::Tab;
            }
            renderer.invalidate();
            force_redraw = true;
        }
        if to_terminal.is_empty()
            && history
                .as_ref()
                .is_some_and(|view| view.escape_expired(Instant::now()))
        {
            let view = history.as_mut().unwrap();
            let exited = view.expire_escape();
            let editor = view.take_editor();
            if view.take_help() {
                help_return_mode = InputMode::History;
                help_action_mode = InputMode::History;
                help = Some(crate::shortcut_help::ShortcutHelp::for_mode(
                    session_name.is_some(),
                    shortcuts,
                    crate::config::BindingMode::History,
                ));
            }
            if let Some(copy) = view.take_copy() {
                rich_clipboard.cancel(&mut to_terminal, |owner, bytes| {
                    deliver_rich_clipboard(windows, owner, bytes)
                });
                to_terminal.extend(copy);
            }
            if exited {
                keys = history_exit_input(history.as_mut().unwrap(), shortcuts);
                history = None;
            }
            dispatch_history_editor(editor, windows, scrollback_lines, &mut to_terminal)?;
            renderer.invalidate();
            force_redraw = true;
        }
        if help
            .as_ref()
            .is_some_and(|help| help.escape_expired(Instant::now()))
        {
            help = None;
            keys = WindowInput {
                mode: help_return_mode,
                shortcuts,
                ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
            };
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
        let staged_resize = pending_outer_resize.take();
        let resize = if let Some(size) = frontend.take_resize()?.or(staged_resize) {
            check_size(size.ws_row, size.ws_col)?;
            *outer_rows = size.ws_row;
            *cell_pixels = CellPixelSize::from_terminal_size(
                size.ws_row,
                size.ws_col,
                size.ws_xpixel,
                size.ws_ypixel,
            );
            if history.take().is_some() {
                help = None;
                input.clear();
                keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
            }
            renderer.invalidate();
            force_redraw = true;
            Some(size)
        } else {
            None
        };
        // Observe exits before preparing resizes, so dead panes need no PTY ioctl.
        for window in windows.iter_mut() {
            let focused = window.content().layout().active();
            let active_window = window.id() == active;
            for (id, pane) in window.content_mut().iter_mut() {
                let retained = pane.retain_after_exit(remain_on_exit);
                if pane.observe_exit(retained)? {
                    bar_dirty = true;
                    if active_window && id == focused && pane.retain_after_exit(remain_on_exit) {
                        // The stopped process cannot end a partial paste/mouse
                        // sequence. Retained panes still need Rustmux shortcuts.
                        keys = WindowInput {
                            mode: if history.is_some() {
                                InputMode::History
                            } else {
                                WindowInput::default_input_mode(
                                    default_mode,
                                    session_name.is_some(),
                                )
                            },
                            shortcuts,
                            ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
                        };
                    }
                }
            }
        }
        if let Some(size) = resize {
            for window in windows.iter_mut() {
                window.content_mut().resize(
                    pane_rows_for_layout(size.ws_row, runtime.compact),
                    size.ws_col,
                )?;
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
                    prepared.push(pane.prepare_resize_with_cell_pixels(
                        rect.rows,
                        rect.columns,
                        *cell_pixels,
                    )?);
                }
            }
            for resize in prepared {
                resize.commit()?;
            }
            pane_resize_pending = None;
        }
        // New panes can already match their layout's character dimensions,
        // so PaneSet::synchronize_sizes may have skipped their first ioctl.
        // Keep their PTY pixel fields current before servicing child output.
        for window in windows.iter_mut() {
            for (_, pane) in window.content_mut().iter_mut() {
                if pane.io().status.is_none() && !pane.io().eof {
                    pane.sync_pty_cell_pixels(*cell_pixels)?;
                }
            }
        }
        // Service the hidden undo pane after consuming an outer resize, so it
        // never interprets new graphics output using stale physical cells.
        if color_probe.is_none()
            && let Some(saved) = closed.as_mut()
            && !saved.service(*cell_pixels)?
        {
            *closed = None;
        }
        if pane_resize_pending.is_some_and(|(_, due)| Instant::now() >= due) {
            let (id, _) = pane_resize_pending.take().unwrap();
            if let Some(window) = windows.get_mut(id) {
                window.content_mut().synchronize_sizes()?;
                renderer.invalidate();
                force_redraw = true;
            }
        }
        let active_index = windows
            .iter()
            .position(|window| window.id() == active)
            .unwrap();
        if connection == ConnectionState::Attached {
            let state = windows
                .active_mut()
                .unwrap()
                .content_mut()
                .active_mut()
                .parts_mut()
                .3;
            if state.bell_pending {
                state.bell_pending = false;
                bar_dirty = true;
            }
        }
        let mut names = window_names(windows);
        if let Some(editor) = prompt.as_ref().filter(|editor| editor.is_rename()) {
            names[active_index].clone_from(&editor.text);
        }
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
                if panes.active().io().eof
                    && !panes.active().retain_after_exit(remain_on_exit)
                    && (prompt.is_some() || history.is_some() || help.is_some())
                {
                    history = None;
                    prompt = None;
                    help = None;
                    renderer.invalidate();
                    force_redraw = true;
                }
                active_paused = paused;
                let deferred_due = kitty_overlays
                    .next_deferred_retry()
                    .is_some_and(|due| Instant::now() >= due);
                if color_probe.is_none()
                    && close_requested.is_none()
                    && !session_manager_requested
                    && !detach_requested
                    && (dirty || force_redraw || bar_dirty || deferred_due)
                    && pane_resize_pending.is_none()
                    && (!paused || force_redraw)
                    && to_terminal.is_empty()
                    && (eof || force_redraw || deferred_due || Instant::now() >= next_frame)
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
                    let owned_titles: Vec<_> = panes
                        .iter()
                        .map(|(id, pane)| {
                            (
                                id,
                                if pane.retain_after_exit(remain_on_exit) {
                                    pane.display_title()
                                } else {
                                    pane.terminal_title().into()
                                },
                            )
                        })
                        .collect();
                    let titles: Vec<_> = owned_titles
                        .iter()
                        .map(|(id, title)| (*id, title.as_ref()))
                        .collect();
                    let bells: Vec<_> = panes
                        .iter()
                        .filter_map(|(pane_id, pane)| pane.io().bell_pending.then_some(pane_id))
                        .collect();
                    let content = pane_view::compose_themed(
                        panes.layout(),
                        &screens,
                        history.as_ref().map(|_| focused),
                        &titles,
                        &bells,
                        runtime.theme,
                    )?;
                    let mut view = compose_with_layout(
                        runtime.theme,
                        &content,
                        *outer_rows,
                        session_name.as_deref(),
                        &names,
                        active_index,
                        keys.footer_mode(),
                        shortcuts,
                        runtime.compact,
                    )?;
                    if panes.active().retain_after_exit(remain_on_exit)
                        && (panes.active().io().status.is_some() || panes.active().io().eof)
                    {
                        view.set_mouse_tracking(if *outer_rows > 1 {
                            crate::screen::MouseTracking::Drag
                        } else {
                            crate::screen::MouseTracking::Off
                        });
                        view.set_sgr_mouse(*outer_rows > 1);
                        view.clear_pointer_shapes();
                        view.set_bracketed_paste(false);
                        view.set_rich_clipboard_paste(false);
                        view.set_focus_reporting(false);
                        view.set_kitty_keyboard_flags(0, 1);
                        view.set_application_cursor_keys(false);
                        view.set_application_keypad(false);
                        if history.is_none() && prompt.is_none() && help.is_none() {
                            view.set_cursor_visible(false);
                        }
                    }
                    if keys.mode != InputMode::Locked
                        || prompt.is_some()
                        || history.is_some()
                        || help.is_some()
                    {
                        // Rustmux-owned modes expect ordinary key bytes. Keep the
                        // child's requested flags intact and restore them when the
                        // local mode closes.
                        view.set_kitty_keyboard_flags(0, 1);
                        view.set_rich_clipboard_paste(false);
                    }
                    if !runtime.compact
                        && prompt.as_ref().is_some_and(|editor| editor.is_rename())
                        && *outer_rows > 1
                        && let Some(column) = crate::chrome::active_window_name_cursor_column(
                            view.dimensions().1,
                            session_name.as_deref(),
                            &names,
                            active_index,
                        )
                    {
                        view.position(0, column);
                        view.set_cursor_visible(true);
                        view.set_cursor_shape(crate::screen::CursorShape::SteadyBar);
                    }
                    if let Some(history) = &history {
                        let (rows, columns) = view.dimensions();
                        if footer_enabled_for_layout(*outer_rows, runtime.compact) {
                            let owned_hints = history.footer_hints();
                            let hints: Vec<_> = owned_hints
                                .iter()
                                .map(|(key, label)| (key.as_str(), label.as_str()))
                                .collect();
                            let hints = hints.as_slice();
                            let status_columns =
                                crate::chrome::history_footer_status_columns(columns, hints);
                            let (status, cursor) = history.footer_status(status_columns);
                            if let Some(start) = crate::chrome::draw_history_footer(
                                runtime.theme,
                                &mut view,
                                &status,
                                hints,
                            ) && let Some(column) = cursor
                            {
                                view.position(rows - 1, start + column);
                                view.set_cursor_visible(true);
                                view.set_cursor_shape(crate::screen::CursorShape::SteadyBar);
                            }
                        } else if *outer_rows > 1 {
                            crate::chrome::prepare_row(
                                &mut view,
                                crate::chrome::bar_style(runtime.theme, true),
                            );
                            for character in
                                crate::chrome::clipped(&history.label(columns), columns).chars()
                            {
                                view.print(character);
                            }
                            if let Some(column) = history.query_cursor(columns) {
                                view.position(0, column);
                                view.set_cursor_visible(true);
                                view.set_cursor_shape(crate::screen::CursorShape::SteadyBar);
                            }
                        }
                    }
                    let status_error = reload_error
                        .as_ref()
                        .map(|error| format!("Config reload failed: {error}"))
                        .or_else(|| {
                            save_error
                                .as_ref()
                                .map(|error| format!("Save failed: {error}"))
                        });
                    if let Some(error) = &status_error
                        && prompt.is_none()
                        && history.is_none()
                        && help.is_none()
                        && (footer_enabled_for_layout(*outer_rows, runtime.compact)
                            || (runtime.compact && *outer_rows > 1))
                    {
                        let (rows, columns) = view.dimensions();
                        view.save_cursor();
                        view.position(if runtime.compact { 0 } else { rows - 1 }, 0);
                        view.set_style(crate::style::Style {
                            foreground: crate::theme::rgb(runtime.theme.error),
                            background: crate::theme::rgb(runtime.theme.background),
                            ..crate::style::Style::default()
                        });
                        view.erase_line(crate::screen::EraseMode::All);
                        let mut used = 0;
                        for character in error.chars() {
                            let width =
                                unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
                            if used + width > columns {
                                break;
                            }
                            view.print(character);
                            used += width;
                        }
                        view.restore_cursor();
                    }
                    if runtime.mouse_hover_cursor
                        && prompt.is_none()
                        && help.is_none()
                        && history.is_none()
                    {
                        let bar = if runtime.compact && status_error.is_some() {
                            Vec::new()
                        } else {
                            crate::chrome::window_hitboxes_for_layout(
                                view.dimensions().1,
                                session_name.as_deref(),
                                &names,
                                active_index,
                                keys.footer_mode(),
                                runtime.compact,
                            )
                        };
                        let footer = if status_error.is_none() && !runtime.compact {
                            crate::chrome::footer_hitboxes_for_mode(
                                view.dimensions().1,
                                keys.footer_mode(),
                                session_name.is_some(),
                                shortcuts,
                            )
                        } else {
                            Vec::new()
                        };
                        keys.decorate_hover(&mut view, panes.layout(), &bar, &footer, *outer_rows);
                    }
                    if let Some(prompt) = &prompt {
                        renderer.render(
                            &prompt.overlay_themed_layout(&view, runtime.theme, runtime.compact),
                            &mut FrameWriter(&mut to_terminal),
                        )?;
                    } else if let Some(help) = &mut help {
                        renderer.render(
                            &help.overlay_themed(&view, runtime.theme),
                            &mut FrameWriter(&mut to_terminal),
                        )?;
                    } else {
                        renderer.render(&view, &mut FrameWriter(&mut to_terminal))?;
                    }
                    if graphics_ready && prompt.is_none() && help.is_none() && history.is_none() {
                        if let Some(cell) = *cell_pixels {
                            kitty_overlays.render(
                                id,
                                panes,
                                cell,
                                *outer_rows,
                                view.cursor(),
                                &mut to_terminal,
                            )?;
                        } else {
                            kitty_overlays.clear(&mut to_terminal)?;
                        }
                    } else {
                        kitty_overlays.clear(&mut to_terminal)?;
                    }
                    for (pane_id, pane) in panes.iter_mut() {
                        if !zoomed || pane_id == focused {
                            pane.parts_mut().3.dirty = false;
                        }
                    }
                    bar_dirty = false;
                    force_redraw = kitty_overlays.needs_retry();
                    next_frame = Instant::now() + FRAME_INTERVAL;
                }
            }
            for (pane_id, pane) in panes.iter() {
                let state = pane.io();
                if state.eof {
                    if let Some(status) = state.status {
                        if (id != active
                            || (zoomed && pane_id != focused)
                            || (!state.dirty && to_terminal.is_empty()))
                            && !pane.retain_after_exit(remain_on_exit)
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
            let old = active_focus(windows);
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
                        flush_protocol_exit(
                            frontend,
                            &mut file_transfer,
                            &mut drag_source,
                            &mut drop_target,
                            &mut to_terminal,
                        )?;
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
                        keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
                    }
                    if was_focused {
                        input.clear();
                        keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
                        prompt = None;
                    }
                    renderer.invalidate();
                    force_redraw = true;
                }
            }
            queue_focus_transition(windows, old, active_focus(windows));
            continue;
        }
        // A lone Escape or incomplete report must not remain held indefinitely.
        if prompt.is_none() && help.is_none() && keys.mouse_expired() {
            let pane = windows.active_mut().unwrap().content_mut().active_mut();
            let (_, _, _, state) = pane.parts_mut();
            if state.accepts_input() && state.to_shell.len() <= LIMIT - 64 {
                let pending = keys.take_mouse();
                let cancel_normal =
                    keys.mode == InputMode::Normal && pending == [27] && shortcuts.exits_normal(27);
                let cancel_pane = keys.mode == InputMode::Pane
                    && pending == [27]
                    && shortcuts
                        .pane_binding(27)
                        .is_some_and(|binding| binding.action == crate::config::PaneAction::Locked);
                let was_local_mode = matches!(
                    keys.mode,
                    InputMode::Pane
                        | InputMode::Resize
                        | InputMode::Move
                        | InputMode::Tab
                        | InputMode::Session
                );
                if keys.mode == InputMode::Normal {
                    keys.mode = InputMode::Locked;
                    if !cancel_normal {
                        state.to_shell.push_back(2);
                    }
                    bar_dirty = true;
                    force_redraw = true;
                }
                if matches!(
                    keys.mode,
                    InputMode::Pane
                        | InputMode::Resize
                        | InputMode::Move
                        | InputMode::Tab
                        | InputMode::Session
                ) {
                    keys.mode = InputMode::Locked;
                    bar_dirty = true;
                    force_redraw = true;
                }
                if !cancel_normal && !cancel_pane && !was_local_mode {
                    state.to_shell.extend(pending);
                }
            }
        }
        // Decode in input order. Bytes preceding a switch remain queued for the
        // old child; following bytes target the newly selected one.
        while close_requested.is_none() && !input.is_empty() {
            if help.is_none()
                && let Some(view) = &mut history
            {
                // Finish any pending frame or OSC before accepting more history
                // input. Repeated copy keys cannot grow the output queue unbounded.
                if !to_terminal.is_empty() {
                    break;
                }
                let exited = view.feed(input.pop_front().unwrap());
                let copy = view.take_copy();
                let editor = view.take_editor();
                if view.take_help() {
                    help_return_mode = InputMode::History;
                    help_action_mode = InputMode::History;
                    help = Some(crate::shortcut_help::ShortcutHelp::for_mode(
                        session_name.is_some(),
                        shortcuts,
                        crate::config::BindingMode::History,
                    ));
                    renderer.invalidate();
                }
                if exited {
                    keys = history_exit_input(view, shortcuts);
                    history = None;
                    renderer.invalidate();
                    bar_dirty = true;
                }
                if let Some(sequence) = copy {
                    rich_clipboard.cancel(&mut to_terminal, |owner, bytes| {
                        deliver_rich_clipboard(windows, owner, bytes)
                    });
                    to_terminal.extend(sequence);
                }
                dispatch_history_editor(editor, windows, scrollback_lines, &mut to_terminal)?;
                force_redraw = true;
                continue;
            }
            if let Some(editor) = &mut prompt {
                let return_to_tab =
                    editor.kind == PromptKind::Rename && keys.mode == InputMode::Tab;
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
                        keys = WindowInput {
                            mode: if return_to_tab {
                                InputMode::Tab
                            } else {
                                WindowInput::default_input_mode(
                                    default_mode,
                                    session_name.is_some(),
                                )
                            },
                            shortcuts,
                            ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
                        };
                        renderer.invalidate();
                    }
                    EditResult::Cancel => {
                        prompt = None;
                        keys = WindowInput {
                            mode: if return_to_tab {
                                InputMode::Tab
                            } else {
                                WindowInput::default_input_mode(
                                    default_mode,
                                    session_name.is_some(),
                                )
                            },
                            shortcuts,
                            ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
                        };
                        renderer.invalidate();
                    }
                    EditResult::Continue => {}
                }
                force_redraw = true;
                continue;
            }
            let help_action = if let Some(help_view) = &mut help {
                match help_view.feed(input.pop_front().unwrap(), Instant::now()) {
                    crate::shortcut_help::HelpEvent::Continue => {
                        force_redraw = true;
                        continue;
                    }
                    crate::shortcut_help::HelpEvent::Redraw => {
                        renderer.invalidate();
                        force_redraw = true;
                        continue;
                    }
                    crate::shortcut_help::HelpEvent::Binding(key) => {
                        help = None;
                        keys = WindowInput {
                            mode: help_action_mode,
                            shortcuts,
                            ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
                        };
                        let sequence = match key {
                            crate::config::HistoryKey::Byte(byte) => vec![byte],
                            _ => key.sequence().to_vec(),
                        };
                        for byte in sequence.into_iter().rev() {
                            input.push_front(byte);
                        }
                        renderer.invalidate();
                        force_redraw = true;
                        continue;
                    }
                    crate::shortcut_help::HelpEvent::Close => {
                        help = None;
                        keys = WindowInput {
                            mode: help_return_mode,
                            shortcuts,
                            ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
                        };
                        renderer.invalidate();
                        force_redraw = true;
                        continue;
                    }
                    crate::shortcut_help::HelpEvent::Action(23) => Some(WindowKey::SessionManager),
                    crate::shortcut_help::HelpEvent::Action(15)
                        if session_name.is_some() && shortcuts.session_entry_key().is_some() =>
                    {
                        help = None;
                        keys = WindowInput {
                            mode: InputMode::Session,
                            shortcuts,
                            ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
                        };
                        renderer.invalidate();
                        force_redraw = true;
                        continue;
                    }
                    crate::shortcut_help::HelpEvent::Action(2) => {
                        Some(WindowKey::Byte(shortcuts.locked_entry_key()))
                    }
                    crate::shortcut_help::HelpEvent::Action(byte) => shortcut_action(byte),
                }
            } else {
                None
            };
            if help_action.is_some() {
                help = None;
                // Legacy Normal Help rows represent the same one-shot Locked
                // action chains as their physical Normal bindings.
                keys = WindowInput {
                    mode: InputMode::Locked,
                    ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
                };
                renderer.invalidate();
                force_redraw = true;
            }
            let pane = windows.active().unwrap().content().active();
            let stopped = pane.io().eof || pane.io().status.is_some();
            if !(stopped && pane.retain_after_exit(remain_on_exit))
                && (!pane.io().accepts_input() || pane.io().to_shell.len() > LIMIT - 64)
            {
                break;
            }
            keys.shortcuts = shortcuts;
            keys.session_available = session_name.is_some();
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
            keys.mouse_tracking = if stopped {
                crate::screen::MouseTracking::Off
            } else {
                pane.screen().mouse_tracking()
            };
            keys.alternate_scroll =
                !stopped && pane.screen().is_alternate() && pane.screen().alternate_scroll();
            keys.application_cursor_keys = !stopped && pane.screen().application_cursor_keys();
            keys.kitty_keyboard_flags = if stopped {
                0
            } else {
                pane.screen().kitty_keyboard_flags()
            };
            keys.bar_enabled = *outer_rows > 1;
            keys.footer_row = footer_enabled_for_layout(*outer_rows, runtime.compact)
                .then_some(usize::from(*outer_rows));
            if help_action.is_none() && keys.mouse.is_empty() && input.front() == Some(&27) {
                let active = windows.active().unwrap().id();
                let names = window_names(windows);
                let active_index = windows
                    .iter()
                    .position(|window| window.id() == active)
                    .unwrap();
                let columns = windows.active().unwrap().content().layout().dimensions().1;
                keys.window_hitboxes = crate::chrome::window_hitboxes_for_layout(
                    usize::from(columns),
                    session_name.as_deref(),
                    &names,
                    active_index,
                    keys.footer_mode(),
                    runtime.compact,
                );
                keys.footer_hitboxes = crate::chrome::footer_hitboxes_for_mode(
                    usize::from(columns),
                    keys.footer_mode(),
                    session_name.is_some(),
                    shortcuts,
                );
                if runtime.compact || reload_error.is_some() || save_error.is_some() {
                    // Error text replaces the footer hints; it has no actions.
                    keys.footer_hitboxes.clear();
                    if runtime.compact && (reload_error.is_some() || save_error.is_some()) {
                        keys.window_hitboxes.clear();
                    }
                }
                keys.active_pane = Some(set.layout().active());
                keys.pane_hitboxes = pane_view::hitboxes(set.layout());
                keys.separator_hitboxes = set.layout().separator_hitboxes();
            }
            actions.clear();
            let action_mode = keys.mode;
            if let Some(action) = help_action {
                actions.push(action);
            } else {
                let input_mode = keys.mode;
                let pointer_before = (keys.pointer_position, keys.pane_drag);
                keys.feed(input.pop_front().unwrap(), &mut actions);
                if runtime.mouse_hover_cursor
                    && pointer_before != (keys.pointer_position, keys.pane_drag)
                {
                    // Hover is observational: respect frame pacing and an
                    // application's synchronized-output batch. Local actions
                    // may still explicitly force their own UI transitions.
                    bar_dirty = true;
                }
                if keys.mode != input_mode {
                    bar_dirty = true;
                    force_redraw = true;
                }
            }
            for action in actions.drain(..) {
                let old = active_focus(windows);
                match action {
                    WindowKey::Byte(byte) => {
                        let pane = windows.active_mut().unwrap().content_mut().active_mut();
                        if !pane.io().accepts_input() {
                            continue;
                        }
                        if submits_command(byte, keys.paste) {
                            pane.command_submitted();
                        }
                        pane.parts_mut().3.to_shell.push_back(byte);
                    }
                    WindowKey::SessionManager => {
                        input.clear();
                        keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
                        session_manager_requested = true;
                    }
                    WindowKey::Detach => {
                        input.clear();
                        keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
                        detach_requested = true;
                    }
                    WindowKey::Help => {
                        let mode = action_mode.binding_mode();
                        help_action_mode = action_mode;
                        help_return_mode = if mode != crate::config::BindingMode::Normal {
                            action_mode
                        } else {
                            InputMode::Locked
                        };
                        help = Some(crate::shortcut_help::ShortcutHelp::for_mode(
                            session_name.is_some(),
                            shortcuts,
                            mode,
                        ));
                        keys = WindowInput::new(shortcuts, default_mode, session_name.is_some());
                        renderer.invalidate();
                        force_redraw = true;
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
                                notifications.clone(),
                                scrollback_lines,
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
                    WindowKey::MovePane(direction) => {
                        let panes = windows.active_mut().unwrap().content_mut();
                        if panes.move_active(direction) {
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
                        keys = WindowInput {
                            mode: if history.is_some() {
                                InputMode::History
                            } else {
                                WindowInput::default_input_mode(
                                    default_mode,
                                    session_name.is_some(),
                                )
                            },
                            shortcuts,
                            ..WindowInput::new(shortcuts, default_mode, session_name.is_some())
                        };
                        if let Some(view) = &mut history {
                            view.set_shortcuts(shortcuts, session_name.is_some());
                            view.set_last_output(
                                windows
                                    .active()
                                    .unwrap()
                                    .content()
                                    .active()
                                    .last_command_output(),
                            );
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
                        match spawn_editor_window(&text, rows, columns, scrollback_lines) {
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
                        match spawn_editor_window(
                            text.as_deref().unwrap(),
                            rows,
                            columns,
                            scrollback_lines,
                        ) {
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
                    WindowKey::MovePanePreviousWindow | WindowKey::MovePaneNextWindow => {
                        let source = windows.active().unwrap().id();
                        let offset = if action == WindowKey::MovePaneNextWindow {
                            1
                        } else {
                            -1
                        };
                        match windows.move_active_pane_relative(offset) {
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
                                renderer.invalidate();
                                force_redraw = true;
                            }
                            Ok(false) | Err(_) => {
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
                    WindowKey::RespawnPane => {
                        let pane = windows.active_mut().unwrap().content_mut().active_mut();
                        if pane
                            .respawn(
                                shell_path,
                                None,
                                None,
                                notifications.clone(),
                                scrollback_lines,
                            )
                            .is_err()
                        {
                            if to_terminal.is_empty() {
                                to_terminal.push_back(7);
                            }
                        } else {
                            history = None;
                            input.clear();
                            prompt = None;
                            help = None;
                            keys =
                                WindowInput::new(shortcuts, default_mode, session_name.is_some());
                            renderer.invalidate();
                            force_redraw = true;
                            bar_dirty = true;
                        }
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
                        match spawn_window(
                            shell_path,
                            directory.as_deref(),
                            rows,
                            columns,
                            notifications.clone(),
                            scrollback_lines,
                        ) {
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
                let new = active_focus(windows);
                if new != old {
                    queue_focus_transition(windows, old, new);
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
        // Yazi chooses its drag component on mouse press. A frontend read can
        // contain both that press and the following OSC 72 gesture; deliver
        // queued keyboard/pointer input before the extracted drag notification.
        // If input is blocked on the child's queue, retain the notification too.
        if input.is_empty() {
            drag_source.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
        }
        if to_terminal.is_empty()
            && let Some(name) = renamed_notice.as_deref()
            && frontend.renamed(name)?
        {
            renamed_notice = None;
        }
        if session_manager_requested
            && !detach_requested
            && renamed_notice.is_none()
            && to_terminal.is_empty()
        {
            rich_clipboard.cancel(&mut to_terminal, |owner, bytes| {
                deliver_rich_clipboard(windows, owner, bytes)
            });
            drop_target.cancel_all();
            drop_target.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
            drop_target.pump(&mut to_terminal, LIMIT);
            drag_source.cancel_all();
            drag_source.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
            drag_source.pump(&mut to_terminal, LIMIT);
            file_transfer.cancel_all();
            file_transfer.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
            file_transfer.pump(Instant::now(), &mut to_terminal, LIMIT);
            if !to_terminal.is_empty() {
                continue;
            }
            if frontend.open_session_manager()? {
                return Ok(ForwardExit::Detached);
            }
            session_manager_requested = false;
        }
        if detach_requested && renamed_notice.is_none() && to_terminal.is_empty() {
            rich_clipboard.cancel(&mut to_terminal, |owner, bytes| {
                deliver_rich_clipboard(windows, owner, bytes)
            });
            drop_target.cancel_all();
            drop_target.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
            drop_target.pump(&mut to_terminal, LIMIT);
            drag_source.cancel_all();
            drag_source.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
            drag_source.pump(&mut to_terminal, LIMIT);
            file_transfer.cancel_all();
            file_transfer.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
            file_transfer.pump(Instant::now(), &mut to_terminal, LIMIT);
            if !to_terminal.is_empty() {
                continue;
            }
            match frontend.detach_client() {
                Ok(true) => return Ok(ForwardExit::Detached),
                Err(error) if client_detach_pending && transport_closed(&error) => {
                    return Ok(ForwardExit::Detached);
                }
                Err(error) => return Err(error),
                Ok(false) => {}
            }
            detach_requested = false;
        }
        if let Some(exit) = frontend_exit(connection, &input) {
            drop_target.cancel_all();
            drop_target.drain(|owner, bytes| deliver_rich_clipboard(windows, owner, bytes));
            rich_clipboard.cancel(&mut to_terminal, |owner, bytes| {
                deliver_rich_clipboard(windows, owner, bytes)
            });
            return Ok(exit);
        }
        // A changed focus needs a frame before returning to a blocking poll.
        if color_probe.is_none()
            && (force_redraw || close_requested.is_some())
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
        if !detach_requested && !session_manager_requested {
            rich_clipboard.pump(Instant::now(), &mut to_terminal, LIMIT);
        }
        let mut outer_events = PollFlags::empty();
        if connection == ConnectionState::Attached
            && !session_manager_requested
            && !detach_requested
            && input.len() < LIMIT
            && frontend.can_receive()
            && rich_clipboard.can_receive()
            && file_transfer.can_receive()
            && drag_source.can_receive()
            && drop_target.can_receive()
        {
            outer_events |= PollFlags::POLLIN;
        }
        if connection == ConnectionState::Attached
            && (!to_terminal.is_empty() || renamed_notice.is_some())
        {
            outer_events |= PollFlags::POLLOUT;
        }
        let mut timeout = if color_probe.is_none()
            && (active_dirty || bar_dirty)
            && !active_paused
            && to_terminal.is_empty()
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
        if !active_paused && let Some(due) = kitty_overlays.next_deferred_retry() {
            timeout = timeout.min(
                due.saturating_duration_since(Instant::now())
                    .as_millis()
                    .clamp(1, u128::from(POLL_TIMEOUT_MILLIS)) as u16,
            );
        }
        if let Some(due) = outer_image_replies.next_expiry() {
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
                    // Child startup color queries must observe the inherited
                    // table, not race the outer-terminal discovery replies.
                    if color_probe.is_none()
                        && rich_clipboard.can_receive()
                        && file_transfer.can_receive()
                        && drag_source.can_receive()
                        && drop_target.can_receive()
                        && !drag_source.probing()
                        && !drop_target.probing()
                        && state.reply_read_limit() != 0
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
                if color_probe.is_none() && pane.io().reply_read_limit() > 0 {
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
            let old_input_len = input.len();
            connection = frontend.receive(&mut input)?;
            let paste_enabled = connection == ConnectionState::Attached
                && runtime.clipboard_read
                && keys.mode == InputMode::Locked
                && prompt.is_none()
                && history.is_none()
                && help.is_none();
            rich_clipboard.tick_paste(Instant::now(), paste_enabled, |owner| {
                rich_paste_live(windows, owner)
            });
            rich_clipboard.set_paste_target(rich_paste_target(windows, paste_enabled));
            // Consume rich-clipboard responses before other probes can retain
            // and later release their bytes into focused keyboard input.
            filter_rich_clipboard_input(
                &mut rich_clipboard,
                &mut file_transfer,
                &mut drag_source,
                &mut drop_target,
                (
                    &drop_target_views(windows, *outer_rows, *cell_pixels),
                    drag_source_view(windows, *outer_rows, *cell_pixels),
                ),
                &mut input,
                old_input_len,
            );
            filter_color_probe_input(
                &mut color_probe,
                &mut input,
                old_input_len,
                inherited_colors,
            );
            filter_graphics_probe_input(
                &mut graphics_probe,
                &mut input,
                old_input_len,
                graphics_support,
            );
            filter_graphics_probe_input(
                &mut shm_probe,
                &mut input,
                old_input_len,
                &mut shm_support,
            );
            finish_color_probe_at_barrier(&mut color_probe, &graphics_probe, &mut input);
            force_redraw |=
                inherit_pane_colors(windows, closed, history.as_mut(), inherited_colors);
            if input.len() > old_input_len {
                let raw: Vec<_> = input.drain(old_input_len..).collect();
                let mut passthrough = Vec::new();
                let failed = outer_image_replies.advance(&raw, &mut passthrough);
                input.extend(passthrough);
                for image_id in failed {
                    force_redraw |= kitty_overlays.invalidate_cached_image(image_id);
                }
            }
        }
        let mut expired_reply = Vec::new();
        outer_image_replies.expire(Instant::now(), &mut expired_reply);
        input.extend(expired_reply);
        if connection == ConnectionState::Attached
            && outer.contains(PollFlags::POLLOUT)
            && let Err(error) = frontend.send(&mut to_terminal)
        {
            if client_detach_pending && transport_closed(&error) {
                rich_clipboard.cancel(&mut to_terminal, |owner, bytes| {
                    deliver_rich_clipboard(windows, owner, bytes)
                });
                return Ok(ForwardExit::Detached);
            }
            return Err(error);
        }
        // A resize may have arrived in the same poll as pane output. Defer
        // those pane reads until the new grid is applied on the next turn;
        // never size an image using cells from the previous frontend frame.
        if let Some(size) = frontend.take_resize()? {
            pending_outer_resize = Some(size);
            *cell_pixels = None;
            continue;
        }
        // One bounded read/write per pane per iteration prevents a busy background
        // process from starving the other panes, keyboard or signal handling.
        for ((id, pane_id, mut inner_events), inner) in interests.into_iter().zip(events) {
            if !rich_clipboard.can_receive()
                || !file_transfer.can_receive()
                || !drag_source.can_receive()
                || !drop_target.can_receive()
            {
                inner_events.remove(PollFlags::POLLIN);
            }
            let window = windows.get_mut(id).unwrap();
            let pane = window
                .content_mut()
                .get_mut(pane_id)
                .expect("polled pane exists");
            let bell_was_pending = pane.io().bell_pending;
            let paste_generation = pane.screen().rich_paste_generation();
            service_pane(
                pane,
                inner_events,
                inner,
                *cell_pixels,
                connection == ConnectionState::Attached
                    && *graphics_support == Some(GraphicsSupport::Supported)
                    && cell_pixels.is_some(),
                ClipboardPolicy {
                    attached: connection == ConnectionState::Attached,
                    read: runtime.clipboard_read,
                    write: runtime.clipboard_write,
                    file_transfer: runtime.file_transfer,
                    drag_source: runtime.drag_source,
                    drop_target: runtime.drop_target,
                },
            )?;
            // Reset followed by set in one PTY read still revokes old events.
            if paste_generation != pane.screen().rich_paste_generation() {
                rich_clipboard.forget_paste(pane.rich_clipboard_owner());
            }
            while let Some(request) = pane.take_drop_target() {
                drop_target.request(
                    pane.rich_clipboard_owner(),
                    request,
                    Instant::now(),
                    cell_pixels.is_some(),
                );
            }
            while let Some(request) = pane.take_drag_source() {
                drag_source.request(pane.rich_clipboard_owner(), request, Instant::now());
            }
            while let Some(request) = pane.take_file_transfer() {
                file_transfer.request(pane.rich_clipboard_owner(), request, Instant::now());
            }
            while let Some(request) = pane.take_rich_clipboard() {
                rich_clipboard.request(
                    pane.rich_clipboard_owner(),
                    request,
                    Instant::now(),
                    &mut to_terminal,
                    LIMIT,
                );
            }
            if let Some(copy) = pane.take_clipboard()
                && connection == ConnectionState::Attached
                && runtime.clipboard_write
                && rich_clipboard.idle()
                && copy.len() <= LIMIT.saturating_sub(to_terminal.len())
            {
                to_terminal.extend(copy);
            }
            let reminder = pane.take_command_reminder();
            bar_dirty |= !bell_was_pending && pane.io().bell_pending;
            if let Some(reminder) = reminder
                && connection == ConnectionState::Attached
            {
                if reminder.bell {
                    to_terminal.push_back(7);
                }
                if reminder.desktop
                    && let Some(identifier) = notification_ids.allocate()
                {
                    let pane_id = pane.control_id();
                    let title = pane.terminal_title().to_owned();
                    let message = crate::notification::encode(
                        &identifier,
                        window.name(),
                        pane_id,
                        &title,
                        reminder.duration,
                    );
                    // Desktop delivery is best effort; keep the terminal output queue bounded.
                    if message.len() <= LIMIT.saturating_sub(to_terminal.len()) {
                        to_terminal.extend(message);
                    }
                }
            }
        }
    }
}

// Normal EOF can follow the child's finish in the same PTY read. Flush that
// complete tail (or cancel an unfinished transfer) before delivering exit.
fn flush_protocol_exit(
    frontend: &mut impl Frontend,
    files: &mut crate::file_transfer::Router,
    drag: &mut crate::drag_source::Router,
    drop: &mut crate::drop_target::Router,
    pending: &mut VecDeque<u8>,
) -> io::Result<()> {
    drag.cancel_all();
    drop.cancel_all();
    if !files.has_activity() && !drag.has_outgoing() && !drop.has_outgoing() {
        return Ok(());
    }
    files.prepare_exit();
    let deadline = Instant::now() + Duration::from_millis(500);
    while files.has_outgoing() || drag.has_outgoing() || drop.has_outgoing() || !pending.is_empty()
    {
        drag.pump(pending, LIMIT);
        drop.pump(pending, LIMIT);
        files.pump(Instant::now(), pending, LIMIT);
        frontend.send(pending)?;
        if !files.has_outgoing()
            && !drag.has_outgoing()
            && !drop.has_outgoing()
            && pending.is_empty()
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "protocol output stalled during exit",
            ));
        }
        let mut descriptors = [PollFd::new(frontend.poll_fd(), PollFlags::POLLOUT)];
        match poll(&mut descriptors, 50u16) {
            Ok(_) | Err(Errno::EINTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn filter_rich_clipboard_input(
    router: &mut crate::rich_clipboard::Router,
    files: &mut crate::file_transfer::Router,
    drag: &mut crate::drag_source::Router,
    drop: &mut crate::drop_target::Router,
    views: (
        &[crate::drag_source::View],
        Option<crate::drag_source::View>,
    ),
    input: &mut VecDeque<u8>,
    old_len: usize,
) {
    if input.len() > old_len {
        let raw: Vec<_> = input.drain(old_len..).collect();
        let mut pass = Vec::new();
        let now = Instant::now();
        router.advance_with_ipc(&raw, &mut pass, now, &mut |protocol, body| match protocol {
            crate::terminal_ipc::Protocol::File => files.response(body, now),
            crate::terminal_ipc::Protocol::Drag => {
                drag.response(body, now, views.1);
                drop.response(body, now, views.0);
            }
            crate::terminal_ipc::Protocol::Clipboard => unreachable!(),
        });
        input.extend(pass);
    }
}
fn drop_target_views(
    windows: &Windows<PaneSet<Pane>>,
    outer_rows: u16,
    cell_pixels: Option<CellPixelSize>,
) -> Vec<crate::drag_source::View> {
    let Some(pixels) = cell_pixels else {
        return Vec::new();
    };
    let Some(window) = windows.active() else {
        return Vec::new();
    };
    let panes = window.content();
    panes
        .layout()
        .content_geometry()
        .panes
        .into_iter()
        .filter_map(|(id, mut rect)| {
            let pane = panes.get(id)?;
            if !pane.io().accepts_input() {
                return None;
            }
            rect.row = rect.row.checked_add(u16::from(outer_rows > 1))?;
            Some(crate::drag_source::View {
                owner: pane.rich_clipboard_owner(),
                rect,
                pixels: (pixels.width(), pixels.height()),
            })
        })
        .collect()
}
fn drag_source_view(
    windows: &Windows<PaneSet<Pane>>,
    outer_rows: u16,
    cell_pixels: Option<CellPixelSize>,
) -> Option<crate::drag_source::View> {
    let pixels = cell_pixels?;
    let panes = windows.active()?.content();
    let pane = panes.active();
    if !pane.io().accepts_input() {
        return None;
    }
    let mut rect = panes
        .layout()
        .content_geometry()
        .panes
        .into_iter()
        .find(|(id, _)| *id == panes.layout().active())?
        .1;
    rect.row = rect.row.checked_add(u16::from(outer_rows > 1))?;
    Some(crate::drag_source::View {
        owner: pane.rich_clipboard_owner(),
        rect,
        pixels: (pixels.width(), pixels.height()),
    })
}
fn rich_paste_live(windows: &Windows<PaneSet<Pane>>, owner: crate::rich_clipboard::Owner) -> bool {
    windows.iter().any(|window| {
        window.content().iter().any(|(_, pane)| {
            pane.rich_clipboard_owner() == owner
                && pane.io().accepts_input()
                && pane.screen().rich_clipboard_paste()
        })
    })
}
fn rich_paste_target(
    windows: &Windows<PaneSet<Pane>>,
    enabled: bool,
) -> Option<crate::rich_clipboard::Owner> {
    let pane = windows.active()?.content().active();
    (enabled && pane.io().accepts_input() && pane.screen().rich_clipboard_paste())
        .then(|| pane.rich_clipboard_owner())
}
fn rich_clipboard_live(
    windows: &Windows<PaneSet<Pane>>,
    owner: crate::rich_clipboard::Owner,
) -> bool {
    windows.iter().any(|window| {
        window
            .content()
            .iter()
            .any(|(_, pane)| pane.rich_clipboard_owner() == owner && pane.io().accepts_input())
    })
}
fn deliver_rich_clipboard(
    windows: &mut Windows<PaneSet<Pane>>,
    owner: crate::rich_clipboard::Owner,
    bytes: &[u8],
) -> bool {
    for window in windows.iter_mut() {
        for (_, pane) in window.content_mut().iter_mut() {
            if pane.rich_clipboard_owner() == owner && pane.io().accepts_input() {
                let state = pane.parts_mut().3;
                if bytes.len() > LIMIT.saturating_sub(state.to_shell.len()) {
                    return false;
                }
                state.to_shell.extend(bytes);
                return true;
            }
        }
    }
    true // The old process is gone; discard its reply rather than routing to focus.
}

fn inherit_pane_colors(
    windows: &mut Windows<PaneSet<Pane>>,
    closed: &mut Option<crate::closed_pane::ClosedPane>,
    history: Option<&mut crate::history_view::HistoryView>,
    colors: &Arc<TerminalColors>,
) -> bool {
    let mut changed = false;
    for window in windows.iter_mut() {
        for (_, pane) in window.content_mut().iter_mut() {
            let (_, _, screen, state) = pane.parts_mut();
            if screen.inherit_colors(colors) {
                state.dirty = true;
                changed = true;
            }
        }
    }
    if let Some(pane) = closed.as_mut().and_then(|closed| closed.pane.as_mut()) {
        pane.parts_mut().2.inherit_colors(colors);
    }
    if let Some(history) = history {
        changed |= history.inherit_colors(colors);
    }
    changed
}

fn filter_color_probe_input(
    probe: &mut Option<ColorProbe>,
    input: &mut VecDeque<u8>,
    old_len: usize,
    colors: &mut Arc<TerminalColors>,
) {
    let Some(probe) = probe.as_mut() else {
        return;
    };
    let raw: Vec<_> = input.drain(old_len..).collect();
    let mut forwarded = Vec::with_capacity(raw.len());
    if probe.advance(&raw, &mut forwarded) {
        *colors = probe.colors();
    }
    input.extend(forwarded);
}

fn finish_color_probe_at_barrier(
    color_probe: &mut Option<ColorProbe>,
    graphics_probe: &Option<GraphicsCapabilityProbe>,
    input: &mut VecDeque<u8>,
) {
    if graphics_probe
        .as_ref()
        .is_none_or(GraphicsCapabilityProbe::complete)
        && let Some(mut probe) = color_probe.take()
    {
        let mut released = Vec::new();
        probe.finish(&mut released);
        input.extend(released);
    }
}

fn filter_graphics_probe_input(
    probe: &mut Option<GraphicsCapabilityProbe>,
    input: &mut VecDeque<u8>,
    old_len: usize,
    support: &mut Option<GraphicsSupport>,
) {
    let Some(probe) = probe.as_mut() else {
        return;
    };
    if input.len() == old_len {
        return;
    }
    let raw: Vec<u8> = input.drain(old_len..).collect();
    let mut forwarded = Vec::with_capacity(raw.len());
    if let Some(decision) = probe.advance(&raw, &mut forwarded) {
        *support = Some(decision);
    }
    input.extend(forwarded);
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
    use crate::render::frame::MAX_FRAME;

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

    fn socket_frontend_with_size(size: nix::pty::Winsize) -> (ClientPeer, ServerFrontend) {
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || handshake::server(server_stream).unwrap());
        let client = handshake::client_with_size(client_stream, size).unwrap();
        (client, ServerFrontend::new(server.join().unwrap()))
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
    fn graphics_probe_filters_only_new_input_and_preserves_queued_bytes() {
        let mut probe = Some(GraphicsCapabilityProbe::new(GRAPHICS_PROBE_IMAGE_ID));
        let mut support = None;
        let mut input = VecDeque::from(b"queued".to_vec());
        let old_len = input.len();
        input.extend(b"\x1b_Gi=31;O");
        filter_graphics_probe_input(&mut probe, &mut input, old_len, &mut support);
        assert_eq!(input, b"queued".to_vec());
        assert_eq!(support, None);

        let old_len = input.len();
        input.extend(b"K\x1b\\\x1b[?1;2ckey");
        filter_graphics_probe_input(&mut probe, &mut input, old_len, &mut support);
        assert_eq!(input, b"queuedkey".to_vec());
        assert_eq!(support, Some(GraphicsSupport::Supported));
    }

    #[test]
    fn focus_transition_queues_events_only_for_reporting_panes() {
        let mut windows = Windows::default();
        let window = windows
            .create(
                "first".into(),
                spawn_window(
                    OsStr::new("/bin/sh"),
                    None,
                    24,
                    80,
                    crate::config::Notifications::default(),
                    crate::config::DEFAULT_SCROLLBACK_LINES,
                )
                .unwrap(),
            )
            .unwrap();
        let first = windows.active().unwrap().content().layout().active();
        windows
            .active_mut()
            .unwrap()
            .content_mut()
            .active_mut()
            .parts_mut()
            .2
            .set_focus_reporting(true);

        let second = windows
            .active_mut()
            .unwrap()
            .content_mut()
            .split_with(SplitAxis::Columns, |_, rect| {
                Pane::spawn(OsStr::new("/bin/sh"), rect.rows, rect.columns)
            })
            .unwrap();
        windows
            .active_mut()
            .unwrap()
            .content_mut()
            .active_mut()
            .parts_mut()
            .2
            .set_focus_reporting(true);

        queue_focus_transition(&mut windows, (window, first), (window, second));
        assert_eq!(
            windows
                .get(window)
                .unwrap()
                .content()
                .get(first)
                .unwrap()
                .io()
                .to_shell,
            b"\x1b[O"
        );
        assert_eq!(
            windows
                .get(window)
                .unwrap()
                .content()
                .get(second)
                .unwrap()
                .io()
                .to_shell,
            b"\x1b[I"
        );

        windows
            .get_mut(window)
            .unwrap()
            .content_mut()
            .get_mut(first)
            .unwrap()
            .parts_mut()
            .3
            .to_shell
            .clear();
        let second_pane = windows
            .get_mut(window)
            .unwrap()
            .content_mut()
            .get_mut(second)
            .unwrap();
        second_pane.parts_mut().3.to_shell.clear();
        second_pane.parts_mut().2.set_focus_reporting(false);
        queue_focus_transition(&mut windows, (window, second), (window, first));
        assert!(
            windows
                .get(window)
                .unwrap()
                .content()
                .get(second)
                .unwrap()
                .io()
                .to_shell
                .is_empty()
        );
        assert_eq!(
            windows
                .get(window)
                .unwrap()
                .content()
                .get(first)
                .unwrap()
                .io()
                .to_shell,
            b"\x1b[I"
        );
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
        let mut session = TerminalSession::new(
            OsStr::new("/bin/sh"),
            24,
            80,
            None,
            crate::config::Notifications::default(),
            crate::config::DEFAULT_SCROLLBACK_LINES,
            crate::config::Shortcuts::default(),
        )
        .unwrap();
        let signals = test_signals();
        let (mut first_client, mut first_frontend) = socket_frontend_with_size(nix::pty::Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 800,
            ws_ypixel: 480,
        });
        send_client_messages(
            &mut first_client,
            &[
                ClientMessage::Input(b"\x1b_Gi=31;OK\x1b\\\x1b[?1;2c".to_vec()),
                ClientMessage::Input(b"RUSTMUX_ATTACH_TEST=kept\n".to_vec()),
                ClientMessage::Detach,
            ],
        );
        assert_eq!(
            session.attach(&mut first_frontend, &signals).unwrap(),
            ForwardExit::Detached
        );
        assert_eq!(session.windows.iter().len(), 1);
        assert_eq!(session.cell_pixels, CellPixelSize::new(10, 20));
        assert_eq!(session.graphics_support, Some(GraphicsSupport::Supported));

        let (mut second_client, mut second_frontend) =
            socket_frontend_with_size(nix::pty::Winsize {
                ws_row: 30,
                ws_col: 90,
                ws_xpixel: 900,
                ws_ypixel: 600,
            });
        send_client_messages(
            &mut second_client,
            &[
                ClientMessage::Resize {
                    rows: 30,
                    columns: 90,
                    pixel_width: 901,
                    pixel_height: 600,
                },
                ClientMessage::Input(b"test \"$RUSTMUX_ATTACH_TEST\" = kept; exit $?\n".to_vec()),
            ],
        );
        // A real client drains redraws while sending a resize. Without this,
        // the socket fills before the server can finish the shell command.
        let drain = thread::spawn(move || {
            let mut bytes = [0; 8192];
            loop {
                match second_client.stream_mut().read(&mut bytes) {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("client output drain failed: {error}"),
                }
            }
        });
        assert_eq!(
            session.attach(&mut second_frontend, &signals).unwrap(),
            ForwardExit::Process(0)
        );
        drop(second_frontend);
        drain.join().unwrap();
        assert_eq!(session.outer_rows, 30);
        assert_eq!(session.cell_pixels, None);
        assert_eq!(session.graphics_support, None);
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
        let mut session = TerminalSession::new(
            OsStr::new("/bin/sh"),
            24,
            80,
            None,
            crate::config::Notifications::default(),
            crate::config::DEFAULT_SCROLLBACK_LINES,
            crate::config::Shortcuts::default(),
        )
        .unwrap();
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
        // Real clients read queries and redraws concurrently with shell input.
        let drain = drain_client_output(second_client);
        assert_eq!(
            session.attach(&mut second_frontend, &signals).unwrap(),
            ForwardExit::Process(0)
        );
        drop(second_frontend);
        drain.join().unwrap();
    }

    #[test]
    fn incomplete_client_frame_disconnects_without_terminating_named_panes() {
        let name =
            crate::session::SessionName::new(format!("truncated-{}", std::process::id())).unwrap();
        let endpoint = SessionEndpoint::bind(&name).unwrap();
        let mut session = TerminalSession::new(
            OsStr::new("/bin/sh"),
            24,
            80,
            Some(name.as_str()),
            crate::config::Notifications::default(),
            crate::config::DEFAULT_SCROLLBACK_LINES,
            crate::config::Shortcuts::default(),
        )
        .unwrap();
        session.rename = Some(endpoint.rename_identity());
        let pid = session
            .windows
            .active()
            .unwrap()
            .content()
            .active()
            .shell()
            .id();
        let (mut client, mut frontend) = socket_frontend(24, 80);
        client.stream_mut().write_all(&[0, 0]).unwrap();
        client
            .stream_mut()
            .shutdown(std::net::Shutdown::Write)
            .unwrap();
        assert_eq!(
            session.attach(&mut frontend, &test_signals()).unwrap(),
            ForwardExit::Disconnected
        );
        let pane = session
            .windows
            .active_mut()
            .unwrap()
            .content_mut()
            .active_mut();
        assert_eq!(pane.shell().id(), pid);
        assert!(pane.parts_mut().0.try_wait().unwrap().is_none());
    }

    fn drain_client_output(mut client: ClientPeer) -> thread::JoinHandle<Vec<ServerMessage>> {
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut messages = Vec::new();
            let mut bytes = [0; 8192];
            loop {
                assert!(Instant::now() < deadline, "client output did not finish");
                match client.stream_mut().read(&mut bytes) {
                    Ok(0) => return messages,
                    Ok(count) => messages.extend(client.decode(&bytes[..count]).unwrap()),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("client read failed: {error}"),
                }
            }
        })
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
        let drain = drain_client_output(client);

        assert_eq!(
            serve_session(
                OsStr::new("/bin/sh"),
                &name,
                &endpoint,
                server,
                crate::config::Notifications::default(),
                crate::config::DEFAULT_SCROLLBACK_LINES,
                crate::config::Shortcuts::default(),
            )
            .unwrap(),
            7
        );
        let messages = drain.join().unwrap();
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, ServerMessage::Exit { status: 7 }))
        );
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
        // Linux rejects executing a file while another descriptor still has it
        // open for writing. TempPath keeps automatic cleanup without retaining
        // the NamedTempFile handle across the spawn.
        let shell = shell.into_temp_path();

        let mut pane = Pane::spawn(shell.as_os_str(), 3, 8).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while pane.io().prompt_start != Some((0, 0)) {
            service_pane(
                &mut pane,
                PollFlags::POLLIN,
                PollFlags::POLLIN,
                None,
                false,
                ClipboardPolicy::default(),
            )
            .unwrap();
            assert!(Instant::now() < deadline, "prompt marker was not parsed");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pane.screen().cursor(), (2, 2));
        pane.shell_mut().terminate().unwrap();
    }

    #[test]
    fn pane_service_retains_kitty_images_with_or_without_cell_pixels() {
        let mut shell = tempfile::NamedTempFile::new().unwrap();
        shell
            .write_all(
                b"#!/bin/sh\nprintf '\\033_Ga=T,f=32,s=1,v=1,i=7,p=1,C=1;AQIDBA==\\033\\\\'\nsleep 5\n",
            )
            .unwrap();
        let mut permissions = shell.as_file().metadata().unwrap().permissions();
        permissions.set_mode(0o700);
        shell.as_file().set_permissions(permissions).unwrap();
        let shell = shell.into_temp_path();

        for cell_pixels in [None, CellPixelSize::new(1, 1)] {
            let mut pane = Pane::spawn(shell.as_os_str(), 2, 2).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while pane.image_store().get(7).is_none() {
                service_pane(
                    &mut pane,
                    PollFlags::POLLIN,
                    PollFlags::POLLIN,
                    cell_pixels,
                    false,
                    ClipboardPolicy::default(),
                )
                .unwrap();
                assert!(Instant::now() < deadline, "Kitty image was not stored");
                thread::sleep(Duration::from_millis(5));
            }
            let geometry = pane
                .image_store()
                .placements()
                .next()
                .unwrap()
                .geometry
                .unwrap();
            assert_eq!(geometry.columns, cell_pixels.map(|_| 1));
            assert_eq!(geometry.rows, cell_pixels.map(|_| 1));
            let snapshot = pane
                .compose_image_snapshot(CellPixelSize::new(1, 1).unwrap())
                .unwrap();
            assert_eq!(&snapshot.pixels[..4], &[1, 2, 3, 4]);
            pane.shell_mut().terminate().unwrap();
        }
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
    fn pane_mode_runs_configured_actions_and_keeps_focus_mode() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.pane]
h = { actions = ["focus-left"] }
r = { actions = ["new-pane-right", { action = "switch-mode", mode = "locked" }] }
p = { actions = [{ action = "switch-mode", mode = "normal" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        let mut keys = WindowInput {
            shortcuts,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x02\x10h?" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::FocusPane(Direction::Left)]);
        assert_eq!(keys.mode, InputMode::Pane);
        keys.feed(b'r', &mut actions);
        assert_eq!(actions.last(), Some(&WindowKey::Split(SplitAxis::Columns)));
        assert_eq!(keys.mode, InputMode::Locked);
        for &byte in b"\x02\x10p" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(keys.mode, InputMode::Normal);
        keys.feed(b'n', &mut actions);
        assert_eq!(actions.last(), Some(&WindowKey::Next));
        for &byte in b"\x02\x10" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(keys.mode, InputMode::Pane);
        keys.feed(27, &mut actions);
        assert_eq!(keys.mode, InputMode::Pane);
        assert_eq!(keys.mouse, [27]); // Escape waits briefly for a possible arrow report.
    }

    #[test]
    fn pane_arrow_aliases_accept_legacy_and_kitty_sequences_without_leaking() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.pane]
left = { actions = ["focus-left"] }
down = { actions = ["focus-down"] }
up = { actions = ["focus-up"] }
right = { actions = ["focus-right"] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        let mut keys = WindowInput {
            mode: InputMode::Pane,
            shortcuts,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for sequence in [
            &b"\x1b[D"[..],
            &b"\x1bOB"[..],
            &b"\x1b[1A"[..],
            &b"\x1b[1;1C"[..],
            &b"\x1b[1;1:2D"[..],
        ] {
            for &byte in sequence {
                keys.feed(byte, &mut actions);
            }
        }
        assert_eq!(
            actions,
            [
                WindowKey::FocusPane(Direction::Left),
                WindowKey::FocusPane(Direction::Down),
                WindowKey::FocusPane(Direction::Up),
                WindowKey::FocusPane(Direction::Right),
                WindowKey::FocusPane(Direction::Left),
            ]
        );
        assert_eq!(keys.mode, InputMode::Pane);
        for sequence in [&b"\x1b[1;2D"[..], &b"\x1b[1;1:3A"[..], &b"\x1b[9~"[..]] {
            for &byte in sequence {
                keys.feed(byte, &mut actions);
            }
        }
        assert_eq!(actions.len(), 5);
        assert_eq!(keys.mode, InputMode::Pane);
        for &byte in b"\x1b[200~text\x1b[201~" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(keys.mode, InputMode::Locked);
        assert_eq!(
            actions[5..]
                .iter()
                .filter_map(|action| match action {
                    WindowKey::Byte(byte) => Some(*byte),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            b"\x1b[200~text\x1b[201~"
        );
    }

    #[test]
    fn pane_footer_click_uses_configured_key_and_stay_semantics() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.pane]
h = { actions = ["focus-left"] }
l = { actions = ["focus-right"] }
j = { actions = ["focus-down"] }
k = { actions = ["focus-up"] }
r = { actions = ["new-pane-right", { action = "switch-mode", mode = "locked" }] }
"#,
        );
        let boxes =
            crate::chrome::footer_hitboxes_for_mode(120, FooterMode::Pane, false, shortcuts);
        let focus = boxes
            .iter()
            .find(|(_, _, key)| *key == b'h')
            .copied()
            .unwrap()
            .0;
        let split = boxes
            .iter()
            .find(|(_, _, key)| *key == b'r')
            .copied()
            .unwrap()
            .0;
        let mut keys = WindowInput {
            mode: InputMode::Pane,
            shortcuts,
            footer_row: Some(24),
            footer_hitboxes: boxes,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for byte in format!("\x1b[<0;{focus};24M\x1b[<0;{focus};24m").bytes() {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::FocusPane(Direction::Left)]);
        assert_eq!(keys.mode, InputMode::Pane);
        for byte in format!("\x1b[<0;{split};24M\x1b[<0;{split};24m").bytes() {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(actions.last(), Some(&WindowKey::Split(SplitAxis::Columns)));
        assert_eq!(keys.mode, InputMode::Locked);
    }

    #[test]
    fn pane_window_move_footer_hint_dispatches_the_clicked_direction() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.pane]
"[" = { actions = ["move-pane-previous-window", { action = "switch-mode", mode = "locked" }] }
"]" = { actions = ["move-pane-next-window", { action = "switch-mode", mode = "locked" }] }
"#,
        );
        let hitboxes =
            crate::chrome::footer_hitboxes_for_mode(200, FooterMode::Pane, false, shortcuts);
        for (key, expected) in [
            (b'[', WindowKey::MovePanePreviousWindow),
            (b']', WindowKey::MovePaneNextWindow),
        ] {
            let column = hitboxes
                .iter()
                .find(|(_, _, action)| *action == key)
                .unwrap()
                .0;
            let mut keys = WindowInput {
                mode: InputMode::Pane,
                shortcuts,
                footer_row: Some(24),
                footer_hitboxes: hitboxes.clone(),
                ..WindowInput::default()
            };
            let mut actions = Vec::new();
            for byte in format!("\x1b[<0;{column};24M\x1b[<0;{column};24m").bytes() {
                keys.feed(byte, &mut actions);
            }
            assert_eq!(actions, [expected]);
            assert_eq!(keys.mode, InputMode::Locked);
        }
    }

    #[test]
    fn normal_arrows_dispatch_complete_sequences_and_preserve_mode_policy() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
left={actions=["focus-up"]}
right={actions=["focus-left",{action="switch-mode",mode="locked"}]}
"#,
        );
        let mut keys = WindowInput {
            mode: InputMode::Normal,
            shortcuts,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        // CSI, SS3, Kitty press/repeat/release, including fragmented byte feeds.
        for byte in b"\x1b[D\x1bOD\x1b[1;1:1D\x1b[1;1:2D\x1b[1;1:3D" {
            keys.feed(*byte, &mut actions);
        }
        assert_eq!(
            actions,
            (0..4)
                .map(|_| WindowKey::FocusPane(Direction::Up))
                .collect::<Vec<_>>()
        );
        assert_eq!(keys.mode, InputMode::Normal);
        assert!(keys.mouse.is_empty());
        actions.clear();
        for byte in b"\x1b[C" {
            keys.feed(*byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::FocusPane(Direction::Left)]);
        assert_eq!(keys.mode, InputMode::Locked);
        actions.clear();
        for byte in b"\x1b[D" {
            keys.feed(*byte, &mut actions);
        }
        assert_eq!(
            actions,
            b"\x1b[D"
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
        // Normal's unbound arrows retain the existing literal-prefix fallback.
        keys.mode = InputMode::Normal;
        actions.clear();
        for byte in b"\x1b[A" {
            keys.feed(*byte, &mut actions);
        }
        assert_eq!(
            actions,
            b"\x02\x1b[A"
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
        assert_eq!(keys.mode, InputMode::Locked);
        keys.mode = InputMode::Normal;
        actions.clear();
        for byte in b"\x1b[1;2D" {
            keys.feed(*byte, &mut actions);
        }
        assert!(actions.iter().all(|a| matches!(a, WindowKey::Byte(_))));
    }

    #[test]
    fn normal_arrow_footer_click_uses_the_same_binding_without_display_metadata() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            "[keybinds.normal]\nleft={actions=['focus-up']}\nright={actions=['focus-left',{action='switch-mode',mode='locked'}]}",
        );
        let hitboxes =
            crate::chrome::footer_hitboxes_for_mode(300, FooterMode::Normal, false, shortcuts);
        for (key, direction, mode) in [
            (
                crate::config::HistoryKey::Left,
                Direction::Up,
                InputMode::Normal,
            ),
            (
                crate::config::HistoryKey::Right,
                Direction::Left,
                InputMode::Locked,
            ),
        ] {
            let column = hitboxes
                .iter()
                .find(|(_, _, action)| *action == key.footer_code())
                .unwrap()
                .0;
            let mut keys = WindowInput {
                mode: InputMode::Normal,
                shortcuts,
                footer_row: Some(24),
                footer_hitboxes: hitboxes.clone(),
                ..WindowInput::default()
            };
            let mut actions = Vec::new();
            for byte in format!("\x1b[<0;{column};24M\x1b[<0;{column};24m").bytes() {
                keys.feed(byte, &mut actions);
            }
            assert_eq!(actions, [WindowKey::FocusPane(direction)]);
            assert_eq!(keys.mode, mode);
        }
    }

    #[test]
    fn resize_mode_keeps_directional_actions_local_until_a_transition() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
r = { actions = [{ action = "switch-mode", mode = "resize" }] }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.resize]
h = { actions = ["resize-pane-left"] }
l = { actions = ["resize-pane-right"] }
left = { actions = ["resize-pane-left"] }
right = { actions = ["resize-pane-right"] }
r = { actions = [{ action = "switch-mode", mode = "normal" }] }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        let mut keys = WindowInput {
            shortcuts,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x02rhl\x1b[D\x1bOC\x1b[1;2D" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(
            actions,
            [
                WindowKey::ResizePane(Direction::Left),
                WindowKey::ResizePane(Direction::Right),
                WindowKey::ResizePane(Direction::Left),
                WindowKey::ResizePane(Direction::Right),
            ]
        );
        assert_eq!(keys.mode, InputMode::Resize);
        keys.feed(b'r', &mut actions);
        assert_eq!(keys.mode, InputMode::Normal);
        keys.feed(16, &mut actions);
        assert_eq!(keys.mode, InputMode::Pane);
        keys.mode = InputMode::Resize;
        keys.feed(16, &mut actions);
        assert_eq!(keys.mode, InputMode::Pane);
        assert_eq!(actions.len(), 4);
    }

    #[test]
    fn resize_footer_click_uses_configured_direction_and_stays_in_mode() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
r = { actions = [{ action = "switch-mode", mode = "resize" }] }
[keybinds.resize]
h = { actions = ["resize-pane-left"] }
j = { actions = ["resize-pane-down"] }
k = { actions = ["resize-pane-up"] }
l = { actions = ["resize-pane-right"] }
r = { actions = [{ action = "switch-mode", mode = "normal" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        let boxes =
            crate::chrome::footer_hitboxes_for_mode(120, FooterMode::Resize, false, shortcuts);
        let column = boxes
            .iter()
            .find(|(_, _, action)| *action == b'l')
            .unwrap()
            .0;
        let mut keys = WindowInput {
            mode: InputMode::Resize,
            shortcuts,
            footer_row: Some(24),
            footer_hitboxes: boxes,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for byte in format!("\x1b[<0;{column};24M\x1b[<0;{column};24m").bytes() {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::ResizePane(Direction::Right)]);
        assert_eq!(keys.mode, InputMode::Resize);
    }

    #[test]
    fn move_mode_handles_keys_arrows_and_transitions_without_forwarding() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
[keybinds.move]
h = { actions = ["move-pane-left"] }
l = { actions = ["move-pane-right"] }
left = { actions = ["move-pane-left"] }
right = { actions = ["move-pane-right"] }
m = { actions = [{ action = "switch-mode", mode = "normal" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        let mut keys = WindowInput {
            shortcuts,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x02\x0dhl\x1b[D\x1bOC\x1b[1;2D" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(
            actions,
            [
                WindowKey::MovePane(Direction::Left),
                WindowKey::MovePane(Direction::Right),
                WindowKey::MovePane(Direction::Left),
                WindowKey::MovePane(Direction::Right),
            ]
        );
        assert_eq!(keys.mode, InputMode::Move);
        keys.feed(b'm', &mut actions);
        assert_eq!(keys.mode, InputMode::Normal);
        keys.feed(13, &mut actions);
        assert_eq!(keys.mode, InputMode::Move);
        keys.move_shortcut(27, &mut actions);
        assert_eq!(keys.mode, InputMode::Locked);
        assert_eq!(actions.len(), 4);
    }

    #[test]
    fn move_footer_click_dispatches_configured_direction() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
[keybinds.move]
h = { actions = ["move-pane-left"], display = "always" }
j = { actions = ["move-pane-down"], display = "always" }
k = { actions = ["move-pane-up"], display = "always" }
l = { actions = ["move-pane-right"], display = "always" }
m = { actions = [{ action = "switch-mode", mode = "normal" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        let boxes =
            crate::chrome::footer_hitboxes_for_mode(120, FooterMode::Move, false, shortcuts);
        let column = boxes
            .iter()
            .find(|(_, _, action)| *action == b'l')
            .unwrap()
            .0;
        let mut keys = WindowInput {
            mode: InputMode::Move,
            shortcuts,
            footer_row: Some(24),
            footer_hitboxes: boxes,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for byte in format!("\x1b[<0;{column};24M\x1b[<0;{column};24m").bytes() {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::MovePane(Direction::Right)]);
        assert_eq!(keys.mode, InputMode::Move);
    }

    #[test]
    fn tab_mode_dispatches_window_actions_and_keeps_navigation_modal() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
[keybinds.tab]
h = { actions = ["previous-window"] }
l = { actions = ["next-window"] }
left = { actions = ["previous-window"] }
right = { actions = ["next-window"] }
"<" = { actions = ["move-window-left"] }
">" = { actions = ["move-window-right"] }
3 = { actions = [{ action = "go-to-window", index = 3 }] }
n = { actions = ["new-window", { action = "switch-mode", mode = "locked" }] }
r = { actions = ["rename-window"] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        let mut keys = WindowInput {
            shortcuts,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x02\x14hl\x1b[D\x1bOC\x1b[1;2D<>3r" {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(
            actions,
            [
                WindowKey::Previous,
                WindowKey::Next,
                WindowKey::Previous,
                WindowKey::Next,
                WindowKey::MoveLeft,
                WindowKey::MoveRight,
                WindowKey::Select(2),
                WindowKey::Rename,
            ]
        );
        assert_eq!(keys.mode, InputMode::Tab);
        keys.feed(b'n', &mut actions);
        assert_eq!(keys.mode, InputMode::Locked);
        assert_eq!(actions.last(), Some(&WindowKey::Create));
    }

    #[test]
    fn tab_footer_click_dispatches_visible_window_key() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
[keybinds.tab]
h = { actions = ["previous-window"], display = "always" }
l = { actions = ["next-window"], display = "always" }
"<" = { actions = ["move-window-left"], display = "always" }
">" = { actions = ["move-window-right"], display = "always" }
n = { actions = ["new-window", { action = "switch-mode", mode = "locked" }] }
"#,
        );
        let boxes = crate::chrome::footer_hitboxes_for_mode(120, FooterMode::Tab, false, shortcuts);
        let column = boxes
            .iter()
            .find(|(_, _, action)| *action == b'l')
            .unwrap()
            .0;
        let mut keys = WindowInput {
            mode: InputMode::Tab,
            shortcuts,
            footer_row: Some(24),
            footer_hitboxes: boxes,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for byte in format!("\x1b[<0;{column};24M\x1b[<0;{column};24m").bytes() {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::Next]);
        assert_eq!(keys.mode, InputMode::Tab);
    }

    #[test]
    fn normal_footer_tab_hint_enters_mode_when_clicked() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
"#,
        );
        let boxes =
            crate::chrome::footer_hitboxes_for_mode(80, FooterMode::Normal, false, shortcuts);
        let column = boxes.iter().find(|(_, _, action)| *action == 20).unwrap().0;
        let mut keys = WindowInput {
            mode: InputMode::Normal,
            shortcuts,
            footer_row: Some(24),
            footer_hitboxes: boxes,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for byte in format!("\x1b[<0;{column};24M\x1b[<0;{column};24m").bytes() {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(keys.mode, InputMode::Tab);
        assert!(actions.is_empty());
    }

    #[test]
    fn supported_modes_switch_directly_without_sending_keys_to_child() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.pane]
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
[keybinds.tab]
"Ctrl r" = { actions = [{ action = "switch-mode", mode = "resize" }] }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.resize]
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
[keybinds.move]
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
"Ctrl r" = { actions = [{ action = "switch-mode", mode = "resize" }] }
"#,
        );
        let mut keys = WindowInput {
            shortcuts,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        let hops = [
            (2, InputMode::Normal),
            (16, InputMode::Pane),
            (20, InputMode::Tab),
            (18, InputMode::Resize),
            (13, InputMode::Move),
            (20, InputMode::Tab),
            (16, InputMode::Pane),
            (13, InputMode::Move),
            (18, InputMode::Resize),
            (20, InputMode::Tab),
        ];
        for (byte, mode) in hops {
            keys.feed(byte, &mut output);
            assert_eq!(keys.mode, mode);
            assert!(output.is_empty());
        }
    }

    #[test]
    fn named_session_mode_dispatches_manager_detach_and_transitions_locally() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl o" = { actions = [{ action = "switch-mode", mode = "session" }] }
[keybinds.session]
d = { actions = ["detach"] }
w = { actions = ["switch-session", { action = "switch-mode", mode = "locked" }] }
o = { actions = [{ action = "switch-mode", mode = "normal" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        let mut keys = WindowInput {
            session_available: true,
            shortcuts,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x02\x0f" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(keys.mode, InputMode::Session);
        assert!(output.is_empty());
        keys.feed(b'w', &mut output);
        assert_eq!(output, [WindowKey::SessionManager]);
        assert_eq!(keys.mode, InputMode::Locked);

        output.clear();
        for &byte in b"\x02\x0fd" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::Detach]);
        assert_eq!(keys.mode, InputMode::Locked);

        output.clear();
        for &byte in b"\x02\x0fo\x0f\x1b" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());
        assert_eq!(keys.mode, InputMode::Session);
        assert_eq!(keys.take_mouse(), b"\x1b");
        keys.session_shortcut(27, &mut output);
        assert_eq!(keys.mode, InputMode::Locked);

        let mut local = WindowInput {
            shortcuts,
            ..WindowInput::default()
        };
        for &byte in b"\x02\x0f" {
            local.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::Byte(2), WindowKey::Byte(15)]);
        assert_eq!(local.mode, InputMode::Locked);
    }

    #[test]
    fn kitty_encoded_session_mode_opens_manager_without_child_input() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl o" = { actions = [{ action = "switch-mode", mode = "session" }] }
[keybinds.session]
w = { actions = ["switch-session", { action = "switch-mode", mode = "locked" }] }
"#,
        );
        let mut keys = WindowInput {
            session_available: true,
            kitty_keyboard_flags: 1,
            shortcuts,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[98;5u\x1b[111;5u\x1b[119;1u" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::SessionManager]);
        assert_eq!(keys.mode, InputMode::Locked);
    }

    #[test]
    fn displayed_footer_dispatches_physical_remapped_keys_and_arrows() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
clear_defaults=true
[keybinds.locked]
"Ctrl b"={actions=[{action="switch-mode",mode="normal"}]}
[keybinds.normal]
N={actions=["new-window",{action="switch-mode",mode="locked"}],display="always"}
D={actions=["new-pane-down",{action="switch-mode",mode="locked"}],display="hidden"}
[keybinds.pane]
up={actions=["focus-up"],display="always"}
"#,
        );
        let mut keys = WindowInput {
            mode: InputMode::Normal,
            shortcuts,
            footer_row: Some(24),
            footer_hitboxes: crate::chrome::footer_hitboxes_for_mode(
                80,
                FooterMode::Normal,
                false,
                shortcuts,
            ),
            ..WindowInput::default()
        };
        let column = keys.footer_hitboxes[0].0;
        let mut actions = Vec::new();
        for byte in format!("\x1b[<0;{column};24M").bytes() {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(actions, vec![WindowKey::Create]);
        assert_eq!(keys.mode, InputMode::Locked);
        for byte in format!("\x1b[<0;{column};24m").bytes() {
            keys.feed(byte, &mut actions);
        }
        actions.clear();
        keys.mode = InputMode::Normal;
        keys.feed(b'D', &mut actions);
        assert_eq!(actions, vec![WindowKey::Split(SplitAxis::Rows)]);
        actions.clear();
        keys.mode = InputMode::Pane;
        keys.footer_hitboxes =
            crate::chrome::footer_hitboxes_for_mode(80, FooterMode::Pane, false, shortcuts);
        let column = keys.footer_hitboxes[0].0;
        for byte in format!("\x1b[<0;{column};24M").bytes() {
            keys.feed(byte, &mut actions);
        }
        assert_eq!(actions, vec![WindowKey::FocusPane(Direction::Up)]);
    }

    #[test]
    fn configured_shortcuts_replace_old_keys() {
        let mut decoder = WindowInput {
            shortcuts: crate::config::Shortcuts::test_keys(*b"NRD"),
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x02N\x02R\x02D\x02c" {
            decoder.feed(byte, &mut actions);
        }
        assert_eq!(
            actions,
            [
                WindowKey::Create,
                WindowKey::Split(SplitAxis::Columns),
                WindowKey::Split(SplitAxis::Rows),
                WindowKey::Byte(2),
                WindowKey::Byte(b'c'),
            ]
        );
    }

    #[test]
    fn configured_normal_exit_does_not_reach_the_child() {
        let mut decoder = WindowInput {
            shortcuts: crate::config::Shortcuts::default().test_normal_exit(7),
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x02\x07" {
            decoder.feed(byte, &mut actions);
        }
        assert!(actions.is_empty());
        assert_eq!(decoder.mode, InputMode::Locked);
    }

    #[test]
    fn normal_window_binding_replaces_a_pane_binding() {
        let mut decoder = WindowInput {
            shortcuts: crate::config::Shortcuts::default().test_normal_action(b'x', b'&'),
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x02x\x02&" {
            decoder.feed(byte, &mut actions);
        }
        assert_eq!(
            actions,
            [WindowKey::Close, WindowKey::Byte(2), WindowKey::Byte(b'&')]
        );
    }

    #[test]
    fn kitty_encoded_prefix_and_commands_preserve_mux_shortcuts() {
        let decode = |bytes: &[u8]| {
            let mut decoder = WindowInput {
                kitty_keyboard_flags: 1,
                ..WindowInput::default()
            };
            let mut result = Vec::new();
            for &byte in bytes {
                decoder.feed(byte, &mut result);
            }
            result
        };
        assert_eq!(decode(b"\x1b[98;5u\x1b[99;1u"), vec![WindowKey::Create]);
        assert_eq!(
            decode(b"\x1b[98;5:1u\x1b[122:90;2u"),
            vec![WindowKey::ToggleZoom]
        );
        assert_eq!(
            decode(b"\x1b[1073::98;5u\x1b[1094::99;1u"),
            vec![WindowKey::Create]
        );
        // Key releases for the owned prefix and a pending mux command are local.
        assert!(decode(b"\x1b[98;5:3u").is_empty());
        let mut decoder = WindowInput {
            kitty_keyboard_flags: 1,
            ..WindowInput::default()
        };
        decoder.mode = InputMode::Normal;
        let mut actions = Vec::new();
        for &byte in b"\x1b[99;1:3u" {
            decoder.feed(byte, &mut actions);
        }
        assert!(actions.is_empty());
        assert_eq!(decoder.mode, InputMode::Normal);

        let ordinary = b"\x1b[97;3u";
        assert_eq!(
            decode(ordinary),
            ordinary
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
        let malformed = b"\x1b[99;invalidu";
        let mut decoder = WindowInput {
            kitty_keyboard_flags: 1,
            mode: InputMode::Normal,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in malformed {
            decoder.feed(byte, &mut actions);
        }
        assert_eq!(actions.first(), Some(&WindowKey::Byte(2)));
        assert_eq!(
            &actions[1..],
            &malformed
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn configured_locked_prefix_replaces_ctrl_b_for_raw_and_kitty_input() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            "[keybinds.locked]\n'Ctrl a' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }",
        );
        let mut keys = WindowInput {
            shortcuts,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x02\x01c\x01\x02\x01\x01\x01q" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            [
                WindowKey::Byte(2),
                WindowKey::Create,
                WindowKey::Byte(1),
                WindowKey::Byte(2),
                WindowKey::Byte(1),
                WindowKey::Byte(1),
                WindowKey::Byte(b'q'),
            ]
        );
        assert_eq!(keys.mode, InputMode::Locked);

        let mut kitty = WindowInput {
            shortcuts,
            kitty_keyboard_flags: 1,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[97;5u\x1b[99;1u" {
            kitty.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::Create]);
        assert_eq!(kitty.mode, InputMode::Locked);

        let mut kitty_named = WindowInput {
            shortcuts,
            session_available: true,
            kitty_keyboard_flags: 1,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[97;5u\x1b[100;1u" {
            kitty_named.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::Detach]);
    }

    #[test]
    fn cleared_defaults_do_not_dispatch_unbound_normal_shortcuts() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
clear_defaults = true
[keybinds.locked]
"Ctrl a" = { actions = [{ action = "switch-mode", mode = "normal" }] }
[keybinds.normal]
c = { actions = ["new-window", { action = "switch-mode", mode = "locked" }] }
"Ctrl w" = { actions = ["switch-session", { action = "switch-mode", mode = "locked" }] }
"?" = { actions = ["show-help", { action = "switch-mode", mode = "locked" }] }
"#,
        );
        let mut keys = WindowInput {
            shortcuts,
            session_available: true,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x01n\x01d\x01c\x01\x17\x01?" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            [
                WindowKey::Byte(1),
                WindowKey::Byte(b'n'),
                WindowKey::Byte(1),
                WindowKey::Byte(b'd'),
                WindowKey::Create,
                WindowKey::SessionManager,
                WindowKey::Help,
            ]
        );
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
            decode(b"a\x02c\x02n\x02p\x02\t\x02&\x02<\x02>\x02?\x02\x02\x02q"),
            vec![
                WindowKey::Byte(b'a'),
                WindowKey::Create,
                WindowKey::Next,
                WindowKey::Previous,
                WindowKey::Last,
                WindowKey::Close,
                WindowKey::MoveLeft,
                WindowKey::MoveRight,
                WindowKey::Help,
                WindowKey::Byte(2),
                WindowKey::Byte(2),
                WindowKey::Byte(b'q')
            ]
        );
    }

    #[test]
    fn named_session_opens_manager_for_kitty_encoded_ctrl_b_ctrl_w() {
        let mut decoder = WindowInput {
            session_available: true,
            kitty_keyboard_flags: 1,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x1b[98;5u\x1b[119;5u" {
            decoder.feed(byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::SessionManager]);
        assert_eq!(decoder.mode, InputMode::Locked);
    }

    #[test]
    fn session_manager_fallback_only_applies_to_named_sessions() {
        let mut named = WindowInput {
            session_available: true,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x02\x17" {
            named.feed(byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::SessionManager]);

        let mut local = WindowInput::default();
        actions.clear();
        for &byte in b"\x02\x17" {
            local.feed(byte, &mut actions);
        }
        assert_eq!(actions, [WindowKey::Byte(2), WindowKey::Byte(23)]);
    }

    #[test]
    fn every_documented_help_action_uses_the_normal_dispatch_table() {
        for action in crate::shortcut_help::documented_actions(false) {
            assert!(shortcut_action(action).is_some(), "missing action {action}");
        }
        let named = crate::shortcut_help::documented_actions(true);
        assert!(named.contains(&23));
        assert!(
            named
                .into_iter()
                .filter(|action| *action != 23)
                .all(|action| shortcut_action(action).is_some())
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
    fn alternate_scroll_translates_vertical_wheel_only_inside_active_pane() {
        let mut keys = WindowInput {
            pane_height: 20,
            pane_width: 38,
            pane_top: 2,
            pane_left: 41,
            alternate_scroll: true,
            bar_enabled: true,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<64;50;10M\x1b[<69;50;10M\x1b[<66;50;10M\x1b[<64;20;10M\x1b[<64;50;1M" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            b"\x1b[A\x1b[B"
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .chain([WindowKey::Previous])
                .collect::<Vec<_>>()
        );

        output.clear();
        keys.application_cursor_keys = true;
        for &byte in b"\x1b[M`R*\x1b[MaR*" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            b"\x1bOA\x1bOB"
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );

        output.clear();
        keys.mouse_tracking = MouseTracking::Button;
        for &byte in b"\x1b[<64;50;10M" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            b"\x1b[<64;9;8M"
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn footer_mouse_reports_never_reach_the_child() {
        let mut keys = WindowInput {
            pane_height: 22,
            pane_width: 80,
            pane_top: 1,
            mouse_tracking: MouseTracking::Any,
            bar_enabled: true,
            footer_row: Some(24),
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<0;2;24M\x1b[<32;3;24M\x1b[<0;3;24m\x1b[<64;4;24M" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());

        for &byte in b"\x1b[<0;2;23M" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(
            output,
            b"\x1b[<0;2;22M"
                .iter()
                .copied()
                .map(WindowKey::Byte)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn footer_clicks_enter_commands_and_dispatch_grouped_shortcuts_once() {
        let mut keys = WindowInput {
            pane_height: 22,
            pane_width: 120,
            pane_top: 1,
            bar_enabled: true,
            footer_row: Some(24),
            footer_hitboxes: crate::chrome::footer_hitboxes(120, false, false),
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<0;11;24M\x1b[<0;11;24m" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());
        assert_eq!(keys.mode, InputMode::Normal);

        let cases = [
            (11, WindowKey::Create),
            (21, WindowKey::Split(SplitAxis::Columns)),
            (35, WindowKey::Split(SplitAxis::Rows)),
            (49, WindowKey::FocusPane(Direction::Left)),
            (51, WindowKey::FocusPane(Direction::Down)),
            (53, WindowKey::FocusPane(Direction::Up)),
            (55, WindowKey::FocusPane(Direction::Right)),
            (67, WindowKey::Next),
            (69, WindowKey::Previous),
            (82, WindowKey::ToggleZoom),
            (93, WindowKey::Help),
        ];
        for (column, expected) in cases {
            keys.mode = InputMode::Normal;
            keys.footer_hitboxes = crate::chrome::footer_hitboxes(120, true, false);
            output.clear();
            for byte in format!("\x1b[<0;{column};24M\x1b[<0;{column};24m").bytes() {
                keys.feed(byte, &mut output);
            }
            assert_eq!(output, [expected]);
            assert_eq!(keys.mode, InputMode::Locked);
        }
    }

    #[test]
    fn remapped_footer_click_dispatches_its_action_directly() {
        let shortcuts = crate::config::Shortcuts::test_keys(*b"NRD");
        let mut keys = WindowInput {
            mode: InputMode::Normal,
            shortcuts,
            footer_row: Some(24),
            footer_hitboxes: crate::chrome::footer_hitboxes_with_shortcuts(
                120, true, false, shortcuts,
            ),
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<0;11;24M\x1b[<0;11;24m" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::Create]);
    }

    #[test]
    fn local_footer_blank_drag_and_wheel_stay_consumed() {
        let mut keys = WindowInput {
            pane_height: 22,
            pane_width: 80,
            pane_top: 1,
            mouse_tracking: MouseTracking::Any,
            bar_enabled: true,
            footer_row: Some(24),
            footer_hitboxes: crate::chrome::footer_hitboxes(80, false, false),
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        // Local sessions omit the named-session hint. Blank space, drag motion,
        // release and wheel remain isolated from the child.
        for &byte in b"\x1b[<0;20;24M\x1b[<32;22;23M\x1b[<0;22;23m\x1b[<64;70;24M" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());
        assert!(!keys.footer_press);
    }

    #[test]
    fn named_session_footer_requests_the_client_session_manager() {
        let mut keys = WindowInput {
            pane_height: 22,
            pane_width: 80,
            pane_top: 1,
            bar_enabled: true,
            footer_row: Some(24),
            footer_hitboxes: crate::chrome::footer_hitboxes(80, false, true),
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for &byte in b"\x1b[<0;11;24M\x1b[<0;11;24m" {
            keys.feed(byte, &mut output);
        }
        assert!(output.is_empty());
        assert_eq!(keys.mode, InputMode::Normal);
        keys.footer_hitboxes = crate::chrome::footer_hitboxes(80, true, true);
        for &byte in b"\x1b[<0;49;24M\x1b[<0;49;24m" {
            keys.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::SessionManager]);
        assert_eq!(keys.mode, InputMode::Locked);
    }

    #[test]
    fn session_mode_footer_click_dispatches_configured_manager_action() {
        let shortcuts = crate::config::Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl o" = { actions = [{ action = "switch-mode", mode = "session" }] }
[keybinds.session]
m = { actions = ["switch-session", { action = "switch-mode", mode = "locked" }], display = "always" }
"#,
        );
        let footer_hitboxes =
            crate::chrome::footer_hitboxes_for_mode(120, FooterMode::Session, true, shortcuts);
        let column = footer_hitboxes
            .iter()
            .find(|(_, _, action)| *action == b'm')
            .unwrap()
            .0;
        let mut keys = WindowInput {
            mode: InputMode::Session,
            session_available: true,
            shortcuts,
            pane_height: 22,
            pane_width: 120,
            pane_top: 1,
            bar_enabled: true,
            footer_row: Some(24),
            footer_hitboxes,
            ..WindowInput::default()
        };
        let mut output = Vec::new();
        for byte in format!("\x1b[<0;{column};24M\x1b[<0;{column};24m").bytes() {
            keys.feed(byte, &mut output);
        }
        assert_eq!(output, [WindowKey::SessionManager]);
        assert_eq!(keys.mode, InputMode::Locked);
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
    #[test]
    fn history_entries_are_local_in_each_mode_and_kitty_locked_input() {
        let source = r#"
[keybinds.locked]
"Ctrl s" = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.normal]
enter = { actions = [{ action = "switch-mode", mode = "history" }] }
s = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.pane]
s = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.resize]
s = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.move]
s = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.tab]
s = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.session]
s = { actions = [{ action = "switch-mode", mode = "history" }] }
"#;
        let shortcuts = crate::config::Shortcuts::test_from_config(source);
        for (mode, bytes) in [
            (InputMode::Locked, &b"\x13"[..]),
            (InputMode::Normal, &b"\r"[..]),
            (InputMode::Normal, &b"s"[..]),
            (InputMode::Pane, &b"s"[..]),
            (InputMode::Resize, &b"s"[..]),
            (InputMode::Move, &b"s"[..]),
            (InputMode::Tab, &b"s"[..]),
            (InputMode::Session, &b"s"[..]),
            (InputMode::Locked, &b"\x1b[115;5u"[..]),
        ] {
            let mut keys = WindowInput {
                mode,
                shortcuts,
                session_available: true,
                kitty_keyboard_flags: 1,
                ..WindowInput::default()
            };
            let mut actions = Vec::new();
            for &byte in bytes {
                keys.feed(byte, &mut actions);
            }
            assert_eq!(actions, [WindowKey::History], "{mode:?}");
        }
        let mut keys = WindowInput {
            shortcuts,
            kitty_keyboard_flags: 1,
            ..WindowInput::default()
        };
        let mut actions = Vec::new();
        for &byte in b"\x1b[115;5:3u" {
            keys.feed(byte, &mut actions);
        }
        assert!(actions.is_empty()); // Kitty release events cannot open a snapshot.
        for &byte in b"\x1b[200~\x13\x02s\x1b[201~" {
            keys.feed(byte, &mut actions);
        }
        assert!(!actions.contains(&WindowKey::History));
    }
}
mod control;

#[cfg(test)]
mod config_reload_input_tests {
    use super::*;
    #[test]
    fn reload_barrier_preserves_modes_partial_mouse_and_paste_markers() {
        let mut input = WindowInput::default();
        assert!(input.can_reload());
        input.mode = InputMode::Normal;
        assert!(!input.can_reload());
        input.mode = InputMode::Locked;
        input.mouse.push(27);
        assert!(!input.can_reload());
        input.mouse.clear();
        for bytes in [b"\x1b".as_slice(), b"\x1b[", b"\x1b[20", b"\x1b[200"] {
            input.tail = bytes.iter().copied().collect();
            assert!(!input.can_reload());
        }
        input.tail = b"abcdef".iter().copied().collect();
        assert!(input.can_reload());
        input.paste = true;
        assert!(!input.can_reload());
        input.paste = false;
        input.pane_press = true;
        assert!(!input.can_reload());
    }
}
