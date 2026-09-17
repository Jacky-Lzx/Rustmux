//! Process orchestration for named persistent sessions.

use std::ffi::OsStr;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsFd, AsRawFd};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, poll};
use nix::unistd::{ForkResult, fork, setsid};

use super::{SessionEndpoint, SessionName, acquire_client, client, connect, handshake};

const ACCEPT_POLL_MILLIS: u16 = 1000;

/// Start a detached server for `name` and attach this terminal to it.
///
/// This must run during single-threaded process startup because the child is
/// created with `fork` and continues in Rust before starting any other threads.
pub fn create(name: &SessionName, shell: &OsStr) -> io::Result<u8> {
    let endpoint = SessionEndpoint::bind(name)?;
    // SAFETY: the CLI calls this during single-threaded startup, so the child
    // cannot inherit locks held by another thread.
    match unsafe { fork() }? {
        ForkResult::Parent { .. } => {
            drop(endpoint.relinquish());
            attach(name)
        }
        ForkResult::Child => {
            let status = run_server(endpoint, shell).unwrap_or(1);
            std::process::exit(i32::from(status));
        }
    }
}

/// Attach this terminal to an existing named session.
pub fn attach(name: &SessionName) -> io::Result<u8> {
    let _lease = acquire_client(name)?;
    client::run(connect(name)?)
}

fn run_server(endpoint: SessionEndpoint, shell: &OsStr) -> io::Result<u8> {
    detach_process(endpoint.listener().as_raw_fd())?;
    let peer = accept_peer(&endpoint)?;
    crate::terminal::serve_session(shell, &endpoint, peer)
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
