//! Process orchestration for named persistent sessions.

use std::ffi::OsStr;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::time::Duration;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, poll};
use nix::sys::signal::{Signal, kill as send_signal};
use nix::unistd::{ForkResult, fork, setsid};

use super::{
    SessionEndpoint, SessionName, acquire_client, acquire_server, client, connect, handshake,
    live_server_pid, protocol::ClientMessage, session_socket_path,
};

const ACCEPT_POLL_MILLIS: u16 = 1000;
const DETACHED_ROWS: u16 = 24;
const DETACHED_COLUMNS: u16 = 80;
const DETACHED_START_TIMEOUT: Duration = Duration::from_secs(2);

/// Start a detached server for `name` and attach this terminal to it.
///
/// This must run during single-threaded process startup because the child is
/// created with `fork` and continues in Rust before starting any other threads.
pub fn create(name: &SessionName, shell: &OsStr, detached: bool) -> io::Result<u8> {
    let endpoint = SessionEndpoint::bind(name)?;
    // SAFETY: the CLI calls this during single-threaded startup, so the child
    // cannot inherit locks held by another thread.
    match unsafe { fork() }? {
        ForkResult::Parent { .. } => {
            drop(endpoint.relinquish());
            if detached {
                start_detached(name)
            } else {
                attach(name)
            }
        }
        ForkResult::Child => {
            let status = run_server(endpoint, name, shell).unwrap_or(1);
            std::process::exit(i32::from(status));
        }
    }
}

fn start_detached(name: &SessionName) -> io::Result<u8> {
    let mut peer = handshake::client(connect(name)?, DETACHED_ROWS, DETACHED_COLUMNS)?;
    peer.stream().set_nonblocking(false)?;
    peer.stream()
        .set_read_timeout(Some(DETACHED_START_TIMEOUT))?;
    peer.stream()
        .set_write_timeout(Some(DETACHED_START_TIMEOUT))?;
    let detach = ClientMessage::Detach
        .encode()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    peer.stream_mut().write_all(&detach)?;

    let mut discard = [0; 1024];
    loop {
        match peer.stream_mut().read(&mut discard) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("session '{name}' did not finish detached startup"),
                ));
            }
            Err(error) => return Err(error),
        }
    }
    live_server_pid(name).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("session '{name}' failed to start: {error}"),
        )
    })?;
    Ok(0)
}

/// Attach this terminal to an existing named session.
pub fn attach(name: &SessionName) -> io::Result<u8> {
    let mut name = name.clone();
    loop {
        match attach_once(&name)? {
            client::ClientExit::Process(status) => return Ok(status),
            client::ClientExit::Detached => return Ok(0),
            client::ClientExit::SessionManager => match manage_sessions(Some(&name), false)? {
                Some(next) => name = next,
                None => return Ok(0),
            },
        }
    }
}

fn attach_once(name: &SessionName) -> io::Result<client::ClientExit> {
    let _lease = acquire_client(name)?;
    client::run(connect(name)?, name)
}

/// Attach directly when one session exists, or ask the user to choose among several.
pub fn choose_and_attach() -> io::Result<u8> {
    match manage_sessions(None, true)? {
        Some(name) => attach(&name),
        None => Ok(0),
    }
}

fn manage_sessions(
    return_to: Option<&SessionName>,
    attach_single_directly: bool,
) -> io::Result<Option<SessionName>> {
    let mut return_to = return_to.cloned();
    let mut changed = false;
    loop {
        let mut sessions = super::list_info()?;
        order_sessions(&mut sessions, return_to.as_ref());
        match sessions.as_slice() {
            [] if changed || return_to.is_some() => return Ok(None),
            [] => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no sessions are running",
                ));
            }
            [session] if attach_single_directly && !changed => {
                return Ok(Some(session.name.clone()));
            }
            _ => {}
        }
        match super::picker::choose(&sessions, return_to.as_ref())? {
            super::picker::Choice::Attach(name) => return Ok(Some(name)),
            super::picker::Choice::Create(name) => {
                let shell = crate::config::shell()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                create(&name, &shell, true)?;
                return Ok(Some(name));
            }
            super::picker::Choice::Kill(name) => {
                kill(&name)?;
                if return_to.as_ref() == Some(&name) {
                    return_to = None;
                }
                changed = true;
            }
            super::picker::Choice::Cancel => return Ok(return_to),
        }
    }
}

fn order_sessions(sessions: &mut [super::SessionInfo], current: Option<&SessionName>) {
    super::order_info(sessions, current);
}

/// Ask a live named-session server to terminate and wait for endpoint cleanup.
pub fn kill(name: &SessionName) -> io::Result<()> {
    let process_id = live_server_pid(name)?;
    send_signal(process_id, Signal::SIGTERM)?;

    let path = session_socket_path(name);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while path.exists() {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("session '{name}' did not terminate"),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

fn run_server(endpoint: SessionEndpoint, name: &SessionName, shell: &OsStr) -> io::Result<u8> {
    detach_process(endpoint.listener().as_raw_fd())?;
    let _server = acquire_server(name)?;
    let peer = accept_peer(&endpoint)?;
    crate::terminal::serve_session(shell, name, &endpoint, peer)
}

fn detach_process(listener: i32) -> io::Result<()> {
    setsid()?;
    let null = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    for descriptor in [
        nix::libc::STDIN_FILENO,
        nix::libc::STDOUT_FILENO,
        nix::libc::STDERR_FILENO,
    ] {
        // SAFETY: both descriptors are live integers and dup2 atomically
        // replaces only the selected standard descriptor.
        if unsafe { nix::libc::dup2(null.as_raw_fd(), descriptor) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    drop(null);
    close_inherited_descriptors(listener)?;
    Ok(())
}

fn close_inherited_descriptors(listener: i32) -> io::Result<()> {
    let directory = match fs::read_dir("/dev/fd") {
        Ok(directory) => directory,
        Err(_) => fs::read_dir("/proc/self/fd")?,
    };
    let descriptors: Vec<_> = directory
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&descriptor| descriptor > nix::libc::STDERR_FILENO && descriptor != listener)
        .collect();
    for descriptor in descriptors {
        // Closing an entry raced with directory iteration is harmless.
        // SAFETY: each value came from the child process's descriptor directory.
        unsafe { nix::libc::close(descriptor) };
    }
    Ok(())
}

fn accept_peer(endpoint: &SessionEndpoint) -> io::Result<handshake::ServerPeer> {
    loop {
        match endpoint.listener().accept() {
            Ok((stream, _)) => match handshake::server(stream) {
                Ok(peer) => return Ok(peer),
                Err(_) => continue,
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let mut poll_fd = [PollFd::new(endpoint.listener().as_fd(), PollFlags::POLLIN)];
                match poll(&mut poll_fd, ACCEPT_POLL_MILLIS) {
                    Ok(_) | Err(Errno::EINTR) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionInfo;

    #[test]
    fn manager_orders_groups_by_recent_connection_then_name() {
        let mut sessions = [
            info("old-detached", false, Some(10)),
            info("new-attached", true, Some(40)),
            info("current", false, Some(5)),
            info("old-attached", true, Some(20)),
            info("new-detached", false, Some(30)),
        ];
        let current = SessionName::new("current").unwrap();
        order_sessions(&mut sessions, Some(&current));
        assert_eq!(
            sessions
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            [
                "current",
                "new-attached",
                "old-attached",
                "new-detached",
                "old-detached",
            ]
        );
    }

    fn info(name: &str, attached: bool, last_connected_at: Option<u64>) -> SessionInfo {
        SessionInfo {
            name: SessionName::new(name).unwrap(),
            attached,
            server_pid: None,
            last_connected_at,
        }
    }
}
