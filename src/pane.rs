//! Per-pane process and terminal state. Polling and rendering belong to the caller.

use crate::{parser::Parser, pty::PtyShell, screen::Screen};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::{ffi::OsStr, io};

pub(crate) const MAX_CELLS: usize = 64 * 1024;

/// Owns one nonblocking PTY, incremental parser and screen. Moving a pane between
/// windows does not restart its process or reset its parsing state. Dropping it
/// uses PtyShell's close/kill/reap behavior for the direct child.
#[derive(Debug)]
pub struct Pane {
    shell: PtyShell,
    parser: Parser,
    screen: Screen,
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
        self.parser
            .advance_with_replies(&mut self.screen, bytes, reply);
    }

    /// Flush an incomplete UTF-8 sequence when the caller observes PTY EOF.
    pub fn finish_output(&mut self) {
        self.parser.finish(&mut self.screen);
    }

    // The existing event loop borrows disjoint state while it is migrated to
    // per-window polling. Output queues/timers stay there in this step.
    pub(crate) fn parts_mut(&mut self) -> (&mut PtyShell, &mut Parser, &mut Screen) {
        (&mut self.shell, &mut self.parser, &mut self.screen)
    }
}
