//! Own a shell and its PTY. No input loop or screen rendering is provided here.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use nix::pty::{Winsize, openpty};
use nix::unistd::{Pid, setsid, tcgetpgrp};

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
        Self::spawn_in(shell, None, rows, columns)
    }

    pub(crate) fn spawn_in(
        shell: impl AsRef<OsStr>,
        directory: Option<&Path>,
        rows: u16,
        columns: u16,
    ) -> io::Result<Self> {
        let mut command = Command::new(shell);
        command.arg("-i");
        if let Some(directory) = directory {
            command.current_dir(directory);
        }
        Self::spawn_command(command, rows, columns)
    }

    /// Start the configured visual editor on one snapshot file. A POSIX shell expands the
    /// conventional editor variables so values such as `nvim -R` retain their arguments.
    pub(crate) fn spawn_editor(path: &OsStr, rows: u16, columns: u16) -> io::Result<Self> {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exec ${VISUAL:-${EDITOR:-vi}} \"$1\"")
            .arg("rustmux-history")
            .arg(path);
        Self::spawn_command(command, rows, columns)
    }

    fn spawn_command(mut command: Command, rows: u16, columns: u16) -> io::Result<Self> {
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
        command
            .env(crate::RUSTMUX_ENV, "1")
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

    /// Choose a directory for a shell created from this PTY.
    ///
    /// A valid OSC 7 path normally wins. Yazi changes its own directory without
    /// changing the parent shell, so its foreground process takes precedence.
    /// When OSC 7 is absent, inspect the foreground process and then the shell.
    pub(crate) fn inherited_directory(&self, tracked: Option<&Path>) -> Option<PathBuf> {
        let tracked = tracked.filter(|path| path.is_dir()).map(Path::to_owned);
        let foreground = self
            .master_fd()
            .and_then(|master| tcgetpgrp(master).ok())
            .filter(|pid| pid.as_raw() > 0);
        let foreground_name = foreground.and_then(process_name);
        let inspect_process = tracked.is_none()
            || foreground_name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("yazi"));
        let process_directory = inspect_process
            .then(|| foreground.and_then(process_current_directory))
            .flatten()
            .filter(|path| path.is_dir())
            .or_else(|| {
                inspect_process
                    .then(|| process_current_directory(Pid::from_raw(self.child.id() as i32)))
                    .flatten()
                    .filter(|path| path.is_dir())
            });
        preferred_directory(tracked, foreground_name.as_deref(), process_directory)
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

    /// Stop a foreground job while retaining the shell and its PTY.
    /// A separate job group is killed; shell builtins receive SIGINT instead.
    /// Returns true when a separate job was stopped and its display modes need cleanup.
    pub fn stop_foreground(&mut self) -> io::Result<bool> {
        use nix::{
            sys::signal::{Signal, kill, killpg},
            unistd::{Pid, tcgetpgrp},
        };
        if self.child.try_wait()?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "shell already exited",
            ));
        }
        let foreground = tcgetpgrp(self.master()?)?;
        if foreground.as_raw() <= 0 {
            return Err(io::Error::other("invalid foreground process group"));
        }
        let shell = Pid::from_raw(self.child.id() as i32);
        let result = if foreground == shell {
            kill(shell, Signal::SIGINT)
        } else {
            killpg(foreground, Signal::SIGKILL)
        };
        match result {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(foreground != shell),
            Err(error) => Err(error.into()),
        }
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

fn preferred_directory(
    tracked: Option<PathBuf>,
    foreground_name: Option<&str>,
    process_directory: Option<PathBuf>,
) -> Option<PathBuf> {
    if foreground_name.is_some_and(|name| name.eq_ignore_ascii_case("yazi")) {
        process_directory.or(tracked)
    } else {
        tracked.or(process_directory)
    }
}

#[cfg(target_os = "macos")]
fn process_current_directory(pid: Pid) -> Option<PathBuf> {
    // SAFETY: proc_vnodepathinfo is a plain C data structure that may be zero-initialized.
    let mut info = unsafe { std::mem::zeroed::<nix::libc::proc_vnodepathinfo>() };
    let size = std::mem::size_of_val(&info);
    // SAFETY: proc_pidinfo writes at most `size` bytes to this valid buffer and does not retain it.
    let length = unsafe {
        nix::libc::proc_pidinfo(
            pid.as_raw(),
            nix::libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut nix::libc::proc_vnodepathinfo).cast(),
            size.try_into().ok()?,
        )
    };
    if usize::try_from(length).ok()? < size {
        return None;
    }
    let path = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|byte| *byte as u8)
        .take_while(|byte| *byte != 0)
        .collect::<Vec<_>>();
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(path)))
}

#[cfg(target_os = "linux")]
fn process_current_directory(pid: Pid) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{}/cwd", pid.as_raw())).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_current_directory(_: Pid) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "macos")]
fn process_name(pid: Pid) -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: proc_name writes at most the supplied buffer length and does not retain its pointer.
    let length = unsafe {
        nix::libc::proc_name(
            pid.as_raw(),
            buffer.as_mut_ptr().cast(),
            buffer.len().try_into().ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    let length = usize::try_from(length).ok()?.min(buffer.len());
    let end = buffer[..length]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length);
    (!buffer[..end].is_empty()).then(|| String::from_utf8_lossy(&buffer[..end]).into_owned())
}

#[cfg(target_os = "linux")]
fn process_name(pid: Pid) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{}/comm", pid.as_raw())).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_name(_: Pid) -> Option<String> {
    None
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

#[cfg(test)]
mod directory_tests {
    use super::*;

    #[test]
    fn tracked_directory_wins_except_for_yazi() {
        let tracked = PathBuf::from("/tracked");
        let process = PathBuf::from("/process");
        assert_eq!(
            preferred_directory(Some(tracked.clone()), Some("fish"), Some(process.clone())),
            Some(tracked.clone())
        );
        assert_eq!(
            preferred_directory(Some(tracked), Some("YAZI"), Some(process.clone())),
            Some(process)
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_directory_resolves_this_process() {
        assert_eq!(
            process_current_directory(Pid::this()).and_then(|path| path.canonicalize().ok()),
            std::env::current_dir()
                .and_then(|path| path.canonicalize())
                .ok()
        );
        assert!(process_name(Pid::this()).is_some());
    }
}
