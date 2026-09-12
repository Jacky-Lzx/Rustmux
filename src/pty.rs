//! Own a shell and its PTY. No input loop or screen rendering is provided here.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};

use nix::pty::{Winsize, openpty};
use nix::unistd::setsid;

/// Owns the master descriptor and the direct shell child.
///
/// Reads and writes are blocking. Drain output before waiting: terminal drain during shell exit can
/// wait even when the PTY buffer is not full. Drop closes the master, kills a still-running shell,
/// and reaps it; use `terminate` to observe cleanup errors. This does not promise to terminate
/// detached descendants that have escaped the controlling terminal.
#[derive(Debug)]
pub struct PtyShell {
    master: Option<File>,
    child: Child,
}

impl PtyShell {
    /// Start an interactive shell with a new session and controlling terminal.
    ///
    /// `shell` is passed directly to Command (no shell interpolation); names without a slash use
    /// PATH. Invalid explicit paths fail without fallback. Call during single-threaded startup:
    /// openpty does not atomically set CLOEXEC, so concurrent unrelated process spawning could
    /// inherit its original descriptors before they are replaced with close-on-exec copies.
    pub fn spawn(shell: impl AsRef<OsStr>, rows: u16, columns: u16) -> io::Result<Self> {
        if rows == 0 || columns == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PTY dimensions must be nonzero",
            ));
        }
        let size = Winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pair = openpty(Some(&size), None)?;
        // Move both descriptors above stderr even if the caller closed stdio.
        // CLOEXEC prevents master/slave copies surviving a successful exec.
        let master = private_fd(pair.master)?;
        let slave = private_fd(pair.slave)?;
        let mut command = Command::new(shell);
        command
            .arg("-i")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: after fork, only system calls and allocation-free OS error construction run.
        // Command has already installed the slave on fd 0. No locks, allocation, environment access
        // or Rust destructors run here.
        unsafe {
            command.pre_exec(|| {
                setsid()?;
                if nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        // Command reports exec/pre_exec errors synchronously and reaps failed children. On failure
        // all parent descriptors are released by RAII.
        let child = command.spawn()?;
        // Drop Command now so the parent retains no slave descriptors.
        drop(command);
        Ok(Self {
            master: Some(master.into()),
            child,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Borrow the master for polling. None after explicit termination.
    pub fn master_fd(&self) -> Option<BorrowedFd<'_>> {
        self.master.as_ref().map(AsFd::as_fd)
    }

    /// Update character dimensions; the kernel notifies the PTY foreground process group.
    /// Zero dimensions are rejected. Returns NotConnected after termination.
    pub fn resize(&mut self, rows: u16, columns: u16) -> io::Result<()> {
        if rows == 0 || columns == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PTY dimensions must be nonzero",
            ));
        }
        let size = Winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let master = self.master()?;
        // SAFETY: master is live and size points to initialized Winsize storage.
        if unsafe { nix::libc::ioctl(master.as_raw_fd(), nix::libc::TIOCSWINSZ, &size) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Reap without blocking; repeated calls return the cached exit status.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Wait for the direct child; drain PTY output first to avoid exit-time stalls.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    /// Close the terminal, forcibly stop the direct shell if needed, and reap it.
    pub fn terminate(&mut self) -> io::Result<ExitStatus> {
        self.master.take();
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        if let Err(error) = self.child.kill() {
            // The child can exit between try_wait and kill.
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            return Err(error);
        }
        self.child.wait()
    }

    fn master(&mut self) -> io::Result<&mut File> {
        self.master
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "PTY is closed"))
    }
}

fn private_fd(fd: OwnedFd) -> io::Result<OwnedFd> {
    // On our Unix targets std duplicates with CLOEXEC and skips descriptors 0–2.
    // The closed-stdio integration test guards this implementation dependency.
    fd.try_clone()
}

impl Read for PtyShell {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.master()?.read(buf) {
            // Linux reports PTY hangup as EIO; macOS returns zero bytes.
            Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => Ok(0),
            result => result,
        }
    }
}

impl Write for PtyShell {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.master()?.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.master()?.flush()
    }
}

impl Drop for PtyShell {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}
