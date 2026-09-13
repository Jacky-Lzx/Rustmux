//! Per-pane process and terminal state. Polling and rendering belong to the caller.

use crate::{parser::Parser, pty::PtyShell, screen::Screen};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::{collections::VecDeque, ffi::OsStr, io, process::ExitStatus, time::Instant};

pub(crate) const INPUT_LIMIT: usize = 64 * 1024;

pub(crate) const MAX_CELLS: usize = 64 * 1024;

/// Owns one nonblocking PTY, incremental parser and screen. Moving a pane between
/// windows does not restart its process or reset its parsing state. Dropping it
/// uses PtyShell's close/kill/reap behavior for the direct child.
#[derive(Debug)]
pub struct Pane {
    shell: PtyShell,
    parser: Parser,
    screen: Screen,
    io: PaneIo,
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
            8192
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
        if usize::from(rows) * usize::from(columns) > MAX_CELLS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pane exceeds cell limit",
            ));
        }
        let screen = Screen::new(usize::from(rows), usize::from(columns))?;
        let shell = PtyShell::spawn(shell, rows, columns)?;
        let master = shell.master_fd().expect("new PTY is open");
        let flags = OFlag::from_bits_truncate(fcntl(master, FcntlArg::F_GETFL)?);
        fcntl(master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(Self {
            shell,
            parser: Parser::new(),
            screen,
            io: PaneIo::default(),
        })
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

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Consume child output and route terminal replies back to this same child.
    /// The caller must reserve reply capacity before reading (MAX_REPLY_BYTES).
    pub fn process_output(&mut self, bytes: &[u8], reply: &mut impl FnMut(&[u8])) {
        self.io.dirty = true;
        self.parser
            .advance_with_replies(&mut self.screen, bytes, reply);
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
        assert_eq!(state.reply_read_limit(), 8192);
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
