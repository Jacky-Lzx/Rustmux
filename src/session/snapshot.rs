//! A bounded save-only socket, independent of the interactive client lease.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{SessionName, session_socket_path};
use crate::{
    config::PersistenceOptions,
    pane::Pane,
    pane_set::PaneSet,
    persistence::{self, PreparedSnapshot},
    window::Windows,
};

const MAX_CLIENTS: usize = 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

struct Request {
    stream: UnixStream,
    deadline: Instant,
}
struct Reply {
    stream: UnixStream,
    bytes: Vec<u8>,
    offset: usize,
    deadline: Instant,
}
struct Save {
    prepared: PreparedSnapshot,
    clients: Vec<UnixStream>,
}
struct Running {
    worker: JoinHandle<io::Result<()>>,
    clients: Vec<UnixStream>,
}

pub(crate) struct SnapshotService {
    listener: UnixListener,
    socket: PathBuf,
    identity: (u64, u64),
    directory: PathBuf,
    name: SessionName,
    options: PersistenceOptions,
    history_limit: usize,
    last_autosave: Instant,
    requests: Vec<Request>,
    replies: Vec<Reply>,
    running: Option<Running>,
    pending: Option<Save>,
    last_error: Option<String>,
}

impl SnapshotService {
    pub(crate) fn configure(&mut self, options: PersistenceOptions, history_limit: usize) {
        if self.options.autosave_interval_seconds != options.autosave_interval_seconds {
            self.last_autosave = Instant::now();
        }
        self.options = options;
        self.history_limit = history_limit;
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
    pub(crate) fn bind(
        name: &SessionName,
        options: PersistenceOptions,
        history_limit: usize,
    ) -> io::Result<Self> {
        let socket = session_socket_path(name).with_extension("save");
        match fs::symlink_metadata(&socket) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket()
                    || metadata.uid() != nix::unistd::geteuid().as_raw()
                    || metadata.mode() & 0o077 != 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "unsafe save endpoint",
                    ));
                }
                // The caller holds the exclusive server PID lease. Only its stale
                // save sidecar can exist at this point; never unlink arbitrary files.
                fs::remove_file(&socket)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let directory = persistence::state_directory()?;
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let metadata = fs::symlink_metadata(&socket)?;
        Ok(Self {
            listener,
            socket,
            identity: (metadata.dev(), metadata.ino()),
            directory,
            name: name.clone(),
            options,
            history_limit,
            last_autosave: Instant::now(),
            requests: Vec::new(),
            replies: Vec::new(),
            running: None,
            pending: None,
            last_error: None,
        })
    }

    fn client_count(&self) -> usize {
        self.requests.len()
            + self.replies.len()
            + self.running.as_ref().map_or(0, |save| save.clients.len())
            + self.pending.as_ref().map_or(0, |save| save.clients.len())
    }

    pub(crate) fn tick(&mut self, windows: &Windows<PaneSet<Pane>>, rows: u16) {
        self.poll_worker(false);
        self.send_replies();
        for _ in 0..MAX_CLIENTS {
            let Ok((stream, _)) = self.listener.accept() else {
                break;
            };
            if self.client_count() < MAX_CLIENTS && stream.set_nonblocking(true).is_ok() {
                self.requests.push(Request {
                    stream,
                    deadline: Instant::now() + REQUEST_TIMEOUT,
                });
            }
        }
        let mut ready = Vec::new();
        let requests = std::mem::take(&mut self.requests);
        for mut request in requests {
            let mut byte = [0];
            match request.stream.read(&mut byte) {
                Ok(1) if byte[0] == b'S' => ready.push(request.stream),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && Instant::now() < request.deadline =>
                {
                    self.requests.push(request)
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    self.requests.push(request)
                }
                _ => {}
            }
        }
        if !ready.is_empty() {
            self.queue(windows, rows, ready);
        } else if self.options.autosave_interval_seconds > 0
            && self.last_autosave.elapsed().as_secs() >= self.options.autosave_interval_seconds
        {
            self.last_autosave = Instant::now();
            self.queue(windows, rows, Vec::new());
        }
        self.start_pending();
    }

    fn queue(&mut self, windows: &Windows<PaneSet<Pane>>, rows: u16, clients: Vec<UnixStream>) {
        match PreparedSnapshot::capture(windows, rows, self.options, self.history_limit) {
            Ok(prepared) => {
                let mut clients = clients;
                if let Some(previous) = self.pending.take() {
                    clients.extend(previous.clients);
                }
                self.pending = Some(Save { prepared, clients });
            }
            Err(error) => self.complete(clients, Err(error)),
        }
    }

    fn start_pending(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(save) = self.pending.take() else {
            return;
        };
        let directory = self.directory.clone();
        let name = self.name.clone();
        match std::thread::Builder::new()
            .name("rustmux-snapshot".into())
            .spawn(move || persistence::save(&directory, &name, &save.prepared.finish()))
        {
            Ok(worker) => {
                self.running = Some(Running {
                    worker,
                    clients: save.clients,
                })
            }
            Err(error) => self.complete(save.clients, Err(error)),
        }
    }

    fn poll_worker(&mut self, wait: bool) {
        if self
            .running
            .as_ref()
            .is_some_and(|save| wait || save.worker.is_finished())
        {
            let running = self.running.take().unwrap();
            let result = running
                .worker
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("snapshot writer panicked")));
            self.complete(running.clients, result);
        }
    }

    fn complete(&mut self, clients: Vec<UnixStream>, result: io::Result<()>) {
        let bytes = match result {
            Ok(()) => {
                self.last_error = None;
                b"OK\n".to_vec()
            }
            Err(error) => {
                let short: String = error
                    .to_string()
                    .chars()
                    .take(512)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                self.last_error = Some(short.clone());
                format!("ERROR {short}\n").into_bytes()
            }
        };
        for stream in clients {
            self.replies.push(Reply {
                stream,
                bytes: bytes.clone(),
                offset: 0,
                deadline: Instant::now() + REQUEST_TIMEOUT,
            });
        }
    }

    fn send_replies(&mut self) {
        self.replies.retain_mut(|reply| {
            if Instant::now() >= reply.deadline {
                return false;
            }
            match reply.stream.write(&reply.bytes[reply.offset..]) {
                Ok(0) => false,
                Ok(count) => {
                    reply.offset += count;
                    reply.offset < reply.bytes.len()
                }
                Err(error) => matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ),
            }
        });
    }

    /// Shutdown and detach wait for all ordered writes, so a completed older
    /// write can never overwrite the final checkpoint after the server exits.
    pub(crate) fn finish(&mut self, windows: &Windows<PaneSet<Pane>>, rows: u16, checkpoint: bool) {
        if checkpoint && self.options.autosave_interval_seconds > 0 {
            self.queue(windows, rows, Vec::new());
        }
        loop {
            self.poll_worker(true);
            self.start_pending();
            if self.running.is_none() {
                break;
            }
        }
        self.send_replies();
    }
}

impl Drop for SnapshotService {
    fn drop(&mut self) {
        self.poll_worker(true);
        if let Some(pending) = self.pending.take() {
            let result = persistence::save(&self.directory, &self.name, &pending.prepared.finish());
            self.complete(pending.clients, result);
        }
        self.send_replies();
        if fs::symlink_metadata(&self.socket).is_ok_and(|m| (m.dev(), m.ino()) == self.identity) {
            let _ = fs::remove_file(&self.socket);
        }
    }
}

/// Request a save while the session is attached or detached. Success means the
/// atomic write and directory sync finished, not merely that it was scheduled.
pub fn save_session(name: &SessionName) -> io::Result<()> {
    save_session_inner(name, None)
}

pub(crate) fn save_session_with_timeout(name: &SessionName, timeout: Duration) -> io::Result<()> {
    save_session_inner(name, Some(timeout))
}

fn save_session_inner(name: &SessionName, timeout: Option<Duration>) -> io::Result<()> {
    super::live_server_pid(name)?;
    let directory = super::session_directory();
    super::ensure_private_directory(&directory)?;
    let path = session_socket_path(name).with_extension("save");
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe save endpoint",
        ));
    }
    let mut stream = UnixStream::connect(path)?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    stream.write_all(b"S")?;
    let response = if let Some(timeout) = timeout {
        read_acknowledgement(&mut stream, timeout)?
    } else {
        let mut response = String::new();
        stream.take(2048).read_to_string(&mut response)?;
        response
    };
    if response == "OK\n" {
        Ok(())
    } else {
        Err(io::Error::other(
            response
                .strip_prefix("ERROR ")
                .unwrap_or("session closed without completing its snapshot")
                .trim_end()
                .to_owned(),
        ))
    }
}

// Unix socket receive-timeout options are not available on every supported
// platform. Read nonblocking and enforce a deadline with poll instead.
fn read_acknowledgement(stream: &mut UnixStream, timeout: Duration) -> io::Result<String> {
    use nix::{
        errno::Errno,
        poll::{PollFd, PollFlags, poll},
    };
    use std::os::fd::AsFd;
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    while bytes.len() < 2048 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "session save acknowledgement timed out",
            ));
        }
        let mut buffer = [0; 256];
        let limit = buffer.len().min(2048 - bytes.len());
        match stream.read(&mut buffer[..limit]) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let mut fds = [PollFd::new(stream.as_fd(), PollFlags::POLLIN)];
                match poll(&mut fds, remaining.as_millis().clamp(1, 100) as u16) {
                    Ok(_) | Err(Errno::EINTR) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error),
        }
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod acknowledgement_tests {
    use super::*;
    #[test]
    fn stalled_and_fragmented_replies_obey_the_acknowledgement_deadline() {
        let (mut client, _server) = UnixStream::pair().unwrap();
        assert_eq!(
            read_acknowledgement(&mut client, Duration::from_millis(20))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            server.write_all(b"O").unwrap();
            std::thread::sleep(Duration::from_millis(10));
            server.write_all(b"K\n").unwrap();
        });
        assert_eq!(
            read_acknowledgement(&mut client, Duration::from_secs(2)).unwrap(),
            "OK\n"
        );
        writer.join().unwrap();
    }
}
