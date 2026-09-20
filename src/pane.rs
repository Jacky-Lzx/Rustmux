//! Per-pane process and terminal state. Polling and rendering belong to the caller.

use crate::{
    parser::Parser,
    pty::PtyShell,
    screen::Screen,
    semantic::{PromptEvent, SemanticOutput},
};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{
    collections::VecDeque,
    ffi::OsStr,
    io,
    process::ExitStatus,
    time::{Duration, Instant},
};
use tempfile::{Builder, NamedTempFile};

pub(crate) const INPUT_LIMIT: usize = 64 * 1024;

pub(crate) const MAX_CELLS: usize = 64 * 1024;
pub(crate) const MAX_REPLY_DRAIN_BYTES: usize = 8192;
/// Owns one nonblocking PTY, incremental parser and screen. Moving a pane between
/// windows does not restart its process or reset its parsing state. Dropping it
/// uses PtyShell's close/kill/reap behavior for the direct child.
#[derive(Debug)]
pub struct Pane {
    shell: PtyShell,
    parser: Parser,
    screen: Screen,
    io: PaneIo,
    command_bell_after: Option<Duration>,
    _temporary_file: Option<TemporaryFile>,
}

#[derive(Debug)]
struct TemporaryFile(NamedTempFile);

impl TemporaryFile {
    fn snapshot(text: &str) -> io::Result<Self> {
        let mut file = Builder::new()
            .prefix("rustmux-snapshot-")
            .suffix(".txt")
            .tempfile()?;
        file.write_all(text.as_bytes())?;
        Ok(Self(file))
    }
}

/// Prepared screen storage with exclusive access to its originating pane.
/// Dropping without commit leaves the pane and PTY untouched. Holding this borrow
/// prevents output from making the prepared screen stale before it is committed.
#[must_use = "commit the prepared resize or drop it to cancel"]
pub struct PreparedPaneResize<'a> {
    pane: &'a mut Pane,
    screen: Option<Screen>,
    prompt_start: Option<(usize, usize)>,
    rows: u16,
    columns: u16,
}

impl PreparedPaneResize<'_> {
    /// Update the live PTY first, then install the already prepared screen.
    /// A PTY error leaves this pane's screen and I/O metadata unchanged.
    /// Already observed EOF or exit needs only the model update.
    pub fn commit(self) -> io::Result<()> {
        let Self {
            pane,
            screen,
            prompt_start,
            rows,
            columns,
        } = self;
        if pane.io.status.is_none() && !pane.io.eof {
            pane.shell.resize(rows, columns)?;
        }
        if let Some(screen) = screen {
            pane.screen = screen;
        }
        pane.io.prompt_start = prompt_start;
        pane.screen.set_synchronized_output(false);
        pane.io.synchronized_since = None;
        pane.io.dirty = true;
        Ok(())
    }
}

/// State that must follow the child when focus changes. The physical terminal's
/// output queue, renderer cache and frame cadence remain shared by the event loop.
#[derive(Debug)]
pub(crate) struct PaneIo {
    pub to_shell: VecDeque<u8>,
    pub dirty: bool,
    pub synchronized_since: Option<Instant>,
    pub eof: bool,
    pub eof_at: Option<Instant>,
    pub status: Option<ExitStatus>,
    pub semantic: SemanticOutput,
    pub prompt_start: Option<(usize, usize)>,
    pub bell_pending: bool,
    command_bell_pending: bool,
}

impl Default for PaneIo {
    fn default() -> Self {
        Self {
            to_shell: VecDeque::new(),
            dirty: true,
            synchronized_since: None,
            eof: false,
            eof_at: None,
            status: None,
            semantic: SemanticOutput::default(),
            prompt_start: None,
            bell_pending: false,
            command_bell_pending: false,
        }
    }
}

impl PaneIo {
    pub fn accepts_input(&self) -> bool {
        !self.eof && self.status.is_none() && self.to_shell.len() < INPUT_LIMIT
    }

    pub fn reply_read_limit(&self) -> usize {
        if self.eof {
            0
        } else if self.status.is_some() {
            // No live child to receive replies; drain its final output.
            MAX_REPLY_DRAIN_BYTES
        } else {
            (INPUT_LIMIT - self.to_shell.len()) / crate::parser::MAX_REPLY_BYTES
        }
    }
}

impl Pane {
    /// Construct the screen before starting a process, then make the master
    /// nonblocking. Startup failures leave no live child behind. Follow
    /// PtyShell::spawn's single-threaded process-spawning requirement.
    pub fn spawn(shell: impl AsRef<OsStr>, rows: u16, columns: u16) -> io::Result<Self> {
        Self::spawn_in(
            shell,
            None,
            rows,
            columns,
            crate::config::Notifications::default(),
        )
    }

    pub(crate) fn spawn_in(
        shell: impl AsRef<OsStr>,
        directory: Option<&Path>,
        rows: u16,
        columns: u16,
        notifications: crate::config::Notifications,
    ) -> io::Result<Self> {
        if usize::from(rows) * usize::from(columns) > MAX_CELLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pane exceeds cell limit",
            ));
        }
        let screen = Screen::new(usize::from(rows), usize::from(columns))?;
        let directory = directory.filter(|path| path.is_dir());
        let shell = PtyShell::spawn_in(shell, directory, rows, columns)?;
        let master = shell.master_fd().expect("new PTY is open");
        let flags = OFlag::from_bits_truncate(fcntl(master, FcntlArg::F_GETFL)?);
        fcntl(master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        let mut io = PaneIo::default();
        if let Some(directory) = directory {
            io.semantic.set_current_directory(directory.to_owned());
        }
        Ok(Self {
            shell,
            parser: Parser::new(),
            screen,
            io,
            command_bell_after: notifications.command_bell_after(),
            _temporary_file: None,
        })
    }

    /// Start an editor in its own pane with a private, automatically removed snapshot file.
    pub(crate) fn spawn_editor(text: &str, rows: u16, columns: u16) -> io::Result<Self> {
        if usize::from(rows) * usize::from(columns) > MAX_CELLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pane exceeds cell limit",
            ));
        }
        let screen = Screen::new(usize::from(rows), usize::from(columns))?;
        let temporary_file = TemporaryFile::snapshot(text)?;
        let shell = PtyShell::spawn_editor(temporary_file.0.path().as_os_str(), rows, columns)?;
        let master = shell.master_fd().expect("new PTY is open");
        let flags = OFlag::from_bits_truncate(fcntl(master, FcntlArg::F_GETFL)?);
        fcntl(master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(Self {
            shell,
            parser: Parser::new(),
            screen,
            io: PaneIo::default(),
            command_bell_after: None,
            _temporary_file: Some(temporary_file),
        })
    }

    /// Keep the shell but discard user input intended for the stopped foreground job.
    pub(crate) fn stop_for_hide(&mut self) -> io::Result<()> {
        let stopped = self.shell.stop_foreground()?;
        self.io.semantic.cancel_current();
        if stopped {
            // A killed full-screen job cannot restore these modes itself.
            self.parser = Parser::new();
            self.screen.leave_alternate();
            self.screen.soft_reset();
            self.screen
                .set_mouse_tracking(crate::screen::MouseTracking::Off);
            self.screen.set_sgr_mouse(false);
            self.screen.set_focus_reporting(false);
            self.screen.set_bracketed_paste(false);
        }
        self.io.to_shell.clear();
        Ok(())
    }

    pub fn shell(&self) -> &PtyShell {
        &self.shell
    }

    /// Raw PTY access for readiness-driven reads/writes and lifecycle handling.
    /// Reads must be passed to process_output; readiness and queue limits are
    /// caller responsibilities. Direct resize must also update the screen.
    pub fn shell_mut(&mut self) -> &mut PtyShell {
        &mut self.shell
    }

    /// Validate dimensions and prepare resized grids before issuing any PTY ioctl.
    /// Same-size requests avoid copying screen cells. A commit still releases any
    /// synchronized-output hold and schedules redraw, as a terminal resize does.
    pub fn prepare_resize(
        &mut self,
        rows: u16,
        columns: u16,
    ) -> io::Result<PreparedPaneResize<'_>> {
        if rows == 0 || columns == 0 || usize::from(rows) * usize::from(columns) > MAX_CELLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid pane dimensions",
            ));
        }
        let mut prompt_start = self.io.prompt_start;
        let screen = if self.screen.dimensions() == (usize::from(rows), usize::from(columns)) {
            None
        } else {
            let mut screen = self.screen.clone();
            if columns != self.screen.dimensions().1 as u16 {
                if let Some(start) = prompt_start {
                    prompt_start = Some(screen.resize_preserving_tail(
                        usize::from(rows),
                        usize::from(columns),
                        start,
                    )?);
                } else {
                    screen.resize(usize::from(rows), usize::from(columns))?;
                }
            } else {
                let cursor_offset = prompt_start
                    .map(|start| (self.screen.cursor().0.saturating_sub(start.0), start.1));
                screen.resize(usize::from(rows), usize::from(columns))?;
                if let Some((offset, column)) = cursor_offset {
                    prompt_start = Some((
                        screen.cursor().0.saturating_sub(offset),
                        column.min(usize::from(columns) - 1),
                    ));
                }
            }
            Some(screen)
        };
        Ok(PreparedPaneResize {
            pane: self,
            screen,
            prompt_start,
            rows,
            columns,
        })
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub(crate) fn last_command_output(&self) -> Option<String> {
        self.io.semantic.last_output()
    }

    pub(crate) fn terminal_title(&self) -> &str {
        self.io
            .semantic
            .title()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or("shell")
    }

    pub(crate) fn command_submitted(&mut self) {
        self.io.semantic.command_submitted();
        self.io.prompt_start = None;
    }

    pub(crate) fn inherited_directory(&self) -> Option<PathBuf> {
        self.shell
            .inherited_directory(self.io.semantic.current_directory())
    }

    /// Consume child output and route terminal replies back to this same child.
    /// The caller must reserve reply capacity before reading (MAX_REPLY_BYTES).
    pub fn process_output(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        self.io.dirty = true;
        let mut events = Vec::new();
        self.io
            .semantic
            .advance_with_prompt_events(bytes, &mut |offset, event| events.push((offset, event)));
        let mut start = 0;
        for (end, event) in events {
            self.process_output_segment(&bytes[start..end], reply);
            self.io.prompt_start = match event {
                PromptEvent::Start => Some(self.screen.cursor()),
                PromptEvent::End => None,
            };
            start = end;
        }
        self.process_output_segment(&bytes[start..], reply);
        let completed_commands = self.io.semantic.take_completed_commands();
        if self.command_bell_after.is_some_and(|threshold| {
            completed_commands
                .into_iter()
                .any(|duration| duration >= threshold)
        }) {
            self.io.bell_pending = true;
            self.io.command_bell_pending = true;
        }
    }

    pub(crate) fn take_command_bell(&mut self) -> bool {
        std::mem::take(&mut self.io.command_bell_pending)
    }

    fn process_output_segment(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        let before = self.screen.primary_scroll_count();
        self.parser
            .advance_with_replies(&mut self.screen, bytes, reply);
        self.io.bell_pending |= self.parser.take_bell();
        let scrolled = self.screen.primary_scroll_count().saturating_sub(before);
        if let Some((row, column)) = self.io.prompt_start {
            self.io.prompt_start = Some((row.saturating_sub(scrolled as usize), column));
        }
    }

    /// Flush an incomplete UTF-8 sequence when the caller observes PTY EOF.
    pub fn finish_output(&mut self) {
        self.parser.finish(&mut self.screen);
        self.io.dirty = true;
        self.io.eof = true;
        self.io.eof_at.get_or_insert_with(Instant::now);
    }

    pub(crate) fn io(&self) -> &PaneIo {
        &self.io
    }

    // Borrow disjoint pane state for readiness-driven event-loop operations.
    pub(crate) fn parts_mut(&mut self) -> (&mut PtyShell, &mut Parser, &mut Screen, &mut PaneIo) {
        (
            &mut self.shell,
            &mut self.parser,
            &mut self.screen,
            &mut self.io,
        )
    }
}

#[cfg(test)]
mod io_tests {
    use super::*;
    use crate::{parser::MAX_REPLY_BYTES, window::Windows};
    use std::{os::unix::process::ExitStatusExt, time::Duration};

    #[test]
    fn input_and_reply_capacity_stop_at_lifecycle_boundaries() {
        let mut state = PaneIo::default();
        assert!(state.accepts_input());
        assert_eq!(state.reply_read_limit(), INPUT_LIMIT / MAX_REPLY_BYTES);
        state.to_shell.resize(INPUT_LIMIT - MAX_REPLY_BYTES, b'x');
        assert_eq!(state.reply_read_limit(), 1);
        state.to_shell.push_back(b'y');
        assert_eq!(state.reply_read_limit(), 0);
        assert!(state.accepts_input());
        state.to_shell.resize(INPUT_LIMIT, b'z');
        assert!(!state.accepts_input());
        assert_eq!(state.reply_read_limit(), 0);
        // Reaping a child must allow draining final output even with a full queue.
        state.status = Some(ExitStatus::from_raw(0));
        assert!(!state.accepts_input());
        assert_eq!(state.reply_read_limit(), MAX_REPLY_DRAIN_BYTES);
        state.eof = true;
        assert_eq!(state.reply_read_limit(), 0);
        state.status = None;
        state.to_shell.clear();
        assert!(!state.accepts_input());
        assert_eq!(state.reply_read_limit(), 0);
    }

    #[test]
    fn focus_and_removal_do_not_mix_queues_deadlines_or_exit_status() {
        let mut windows = Windows::default();
        let a = windows.create("a".into(), PaneIo::default()).unwrap();
        let b = windows.create("b".into(), PaneIo::default()).unwrap();
        let started = Instant::now() - Duration::from_millis(100);
        let first = windows.get_mut(a).unwrap().content_mut();
        first.to_shell.extend(b"keyboard\x1b[3;4R");
        first.synchronized_since = Some(started);
        first.eof_at = Some(started);
        first.eof = true;
        first.status = Some(ExitStatus::from_raw(7 << 8));
        first.dirty = false;
        windows.select(a).unwrap();
        windows.select_next();
        let second = windows.active_mut().unwrap().content_mut();
        assert!(second.to_shell.is_empty());
        assert!(second.accepts_input());
        assert!(second.dirty);
        assert!(second.synchronized_since.is_none());
        assert!(second.eof_at.is_none());
        second.to_shell.extend(b"other");
        let removed = windows.close(a).unwrap().into_content();
        assert_eq!(
            removed.to_shell.into_iter().collect::<Vec<_>>(),
            b"keyboard\x1b[3;4R"
        );
        assert_eq!(removed.synchronized_since, Some(started));
        assert_eq!(removed.eof_at, Some(started));
        assert_eq!(removed.status.unwrap().code(), Some(7));
        assert_eq!(windows.active().unwrap().id(), b);
        assert_eq!(
            windows
                .active()
                .unwrap()
                .content()
                .to_shell
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            b"other"
        );
    }
}
