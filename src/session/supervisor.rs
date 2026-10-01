//! Process orchestration for named persistent sessions.

use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;
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
pub fn create(
    name: &SessionName,
    config: &crate::config::Config,
    detached: bool,
    config_path: Option<&Path>,
) -> io::Result<u8> {
    create_with_bootstrap(name, config, detached, config_path, None, None)
}

/// Validate an explicit project layout before binding or forking its named server.
pub fn create_from_layout(
    name: &SessionName,
    config: &crate::config::Config,
    detached: bool,
    config_path: Option<&Path>,
    layout: &Path,
) -> io::Result<u8> {
    create_with_bootstrap(name, config, detached, config_path, None, Some(layout))
}

fn create_with_bootstrap(
    name: &SessionName,
    config: &crate::config::Config,
    detached: bool,
    config_path: Option<&Path>,
    size: Option<(u16, u16)>,
    layout: Option<&Path>,
) -> io::Result<u8> {
    let size = if layout.is_some() {
        if detached {
            Some((40, 120))
        } else {
            let file = crate::terminal_device::TerminalDevice::open_controlling()?;
            let size = crate::terminal_device::window_size(&file)?;
            Some((size.ws_row, size.ws_col))
        }
    } else {
        size
    };
    let workspace = super::acquire_workspace(name)?;
    let snapshot = if let Some(layout) = layout {
        let (rows, columns) = size.unwrap();
        Some(crate::project::load(layout, rows, columns)?)
    } else {
        crate::persistence::load(&crate::persistence::state_directory()?, name)?
    };
    let bootstrap_size = size
        .or_else(|| snapshot.as_ref().map(|snapshot| snapshot.bootstrap_size()))
        .unwrap_or((DETACHED_ROWS, DETACHED_COLUMNS));
    let endpoint = SessionEndpoint::bind(name)?;
    drop(workspace);
    // SAFETY: the CLI calls this during single-threaded startup, so the child
    // cannot inherit locks held by another thread.
    match unsafe { fork() }? {
        ForkResult::Parent { .. } => {
            drop(endpoint.relinquish());
            if detached {
                start_detached(name, bootstrap_size)
            } else {
                attach(name, config_path)
            }
        }
        ForkResult::Child => {
            let status = run_server(endpoint, name, config, snapshot).unwrap_or(1);
            std::process::exit(i32::from(status));
        }
    }
}

fn start_detached(name: &SessionName, size: (u16, u16)) -> io::Result<u8> {
    let mut peer = handshake::client(connect(name)?, size.0, size.1)?;
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

/// Attach to a live server, or start a fresh/restored workspace if it is absent.
///
/// Creation runs during single-threaded startup, as required by `create`.
/// Only a missing server permits creation: unsafe endpoints and attachment
/// errors must not replace an existing session. A concurrent creator can win
/// the workspace lock or socket bind, in which case this attempt fails safely.
pub fn attach_or_create(name: &SessionName, config_path: Option<&Path>) -> io::Result<u8> {
    match live_server_pid(name) {
        Ok(_) => attach(name, config_path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let config = crate::config::load_with_path(config_path)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            // Verify the interactive terminal before binding/forking, avoiding
            // an unattached server when this command runs without a terminal.
            let file = crate::terminal_device::TerminalDevice::open_controlling()?;
            let size = crate::terminal_device::window_size(&file)?;
            drop(file);
            if size.ws_row == 0 || size.ws_col == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "terminal rows and columns must be nonzero",
                ));
            }
            // The real client performs the first handshake, so fresh and saved
            // workspaces start at the attachment's current terminal dimensions.
            create_with_bootstrap(
                name,
                &config,
                false,
                config_path,
                Some((size.ws_row, size.ws_col)),
                None,
            )
        }
        Err(error) => Err(error),
    }
}

/// Attach this terminal to an existing named session.
pub fn attach(name: &SessionName, config_path: Option<&Path>) -> io::Result<u8> {
    let mut name = name.clone();
    loop {
        match attach_once(&mut name)? {
            client::ClientExit::Process(status) => return Ok(status),
            client::ClientExit::Detached => return Ok(0),
            client::ClientExit::SessionManager => {
                match manage_sessions(Some(&name), false, config_path)? {
                    Some(next) => name = next,
                    None => return Ok(0),
                }
            }
        }
    }
}

fn attach_once(name: &mut SessionName) -> io::Result<client::ClientExit> {
    use std::os::unix::fs::MetadataExt;
    let _lease = acquire_client(name)?;
    let metadata = fs::symlink_metadata(session_socket_path(name))?;
    let identity = (metadata.dev(), metadata.ino());
    let result = client::run(connect(name)?, name);
    // A local prefix shortcut can detach before an in-flight Renamed notice
    // is read. Recover the same listener identity rather than reusing an alias.
    if matches!(result, Ok(client::ClientExit::SessionManager))
        && let Some(session) = super::list_info()?.into_iter().find(|s| {
            fs::symlink_metadata(session_socket_path(&s.name))
                .is_ok_and(|m| (m.dev(), m.ino()) == identity)
        })
    {
        *name = session.name;
    }
    result
}

/// Attach directly when one session exists, or ask the user to choose among several.
pub fn choose_and_attach(config_path: Option<&Path>) -> io::Result<u8> {
    match manage_sessions(None, true, config_path)? {
        Some(name) => attach(&name, config_path),
        None => Ok(0),
    }
}

fn manage_sessions(
    return_to: Option<&SessionName>,
    attach_single_directly: bool,
    config_path: Option<&Path>,
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
                    "no running or saved sessions",
                ));
            }
            [session] if attach_single_directly && !changed => {
                restore_selected(session, config_path)?;
                return Ok(Some(session.name.clone()));
            }
            _ => {}
        }
        match super::picker::choose(&sessions, return_to.as_ref(), config_path)? {
            super::picker::Choice::Attach(name) => {
                let latest = super::list_info()?;
                let session = latest
                    .iter()
                    .find(|session| session.name == name)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, "selected session disappeared")
                    })?;
                restore_selected(session, config_path)?;
                return Ok(Some(name));
            }
            super::picker::Choice::Create(name) => {
                let config = crate::config::load_with_path(config_path)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                create(&name, &config, true, config_path)?;
                return Ok(Some(name));
            }
            super::picker::Choice::Kill(name, current) => {
                return_to = current;
                kill(&name)?;
                if return_to.as_ref() == Some(&name) {
                    return_to = None;
                }
                changed = true;
            }
            super::picker::Choice::Cancel(current) => {
                return Ok(current.filter(|name| live_server_pid(name).is_ok()));
            }
        }
    }
}

fn restore_selected(session: &super::SessionInfo, config_path: Option<&Path>) -> io::Result<()> {
    if session.saved {
        // The picker has restored the outer terminal and dropped its signal
        // registrations before we fork a fresh server for this workspace.
        let config = crate::config::load_with_path(config_path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let file = crate::terminal_device::TerminalDevice::open_controlling()?;
        let size = crate::terminal_device::window_size(&file)?;
        drop(file);
        // Restore at the actual attachment dimensions. Bootstrapping at the
        // saved width and immediately resizing would pull old history into the
        // otherwise fresh live screen through ordinary primary-grid reflow.
        create_with_bootstrap(
            &session.name,
            &config,
            true,
            config_path,
            Some((size.ws_row, size.ws_col)),
            None,
        )?;
    }
    Ok(())
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

fn run_server(
    endpoint: SessionEndpoint,
    name: &SessionName,
    config: &crate::config::Config,
    snapshot: Option<crate::persistence::Snapshot>,
) -> io::Result<u8> {
    detach_process(endpoint.listener().as_raw_fd())?;
    let _server = acquire_server(name)?;
    let shortcuts = config.shortcuts();
    let peer = accept_peer(
        &endpoint,
        shortcuts.locked_entry_key(),
        !shortcuts.clear_defaults(),
    )?;
    crate::terminal::serve_configured_session(config, name, &endpoint, peer, snapshot)
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

fn accept_peer(
    endpoint: &SessionEndpoint,
    locked_enter: u8,
    legacy_client_shortcuts: bool,
) -> io::Result<handshake::ServerPeer> {
    loop {
        match endpoint.listener().accept() {
            Ok((stream, _)) => {
                match handshake::server_with_keybinds(stream, locked_enter, legacy_client_shortcuts)
                {
                    Ok(peer) => return Ok(peer),
                    Err(_) => continue,
                }
            }
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
            saved: false,
            name: SessionName::new(name).unwrap(),
            attached,
            server_pid: None,
            last_connected_at,
        }
    }
}
