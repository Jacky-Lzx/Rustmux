//! Secure local endpoints for persistent Rustmux sessions.

pub mod client;
pub mod frontend;
pub mod handshake;
mod listing;
mod picker;
pub mod protocol;
pub(crate) mod rename;
pub mod snapshot;
pub mod supervisor;

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use nix::unistd::Pid;

const MAX_PID_BYTES: u64 = 32;
const MAX_TIMESTAMP_BYTES: u64 = 32;

/// Maximum encoded length of a session name.
pub const MAX_SESSION_NAME_BYTES: usize = 64;

/// A name that can be mapped to one file in the private session directory.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionName(String);

impl SessionName {
    /// Validate a user-visible session name.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidSessionName> {
        let name = name.into();
        if name.is_empty() {
            return Err(InvalidSessionName("session name cannot be empty"));
        }
        if name.len() > MAX_SESSION_NAME_BYTES {
            return Err(InvalidSessionName("session name is longer than 64 bytes"));
        }
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(InvalidSessionName(
                "session name may contain only ASCII letters, numbers, '-' and '_'",
            ));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reason a string cannot identify a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSessionName(&'static str);

impl fmt::Display for InvalidSessionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for InvalidSessionName {}

impl std::str::FromStr for SessionName {
    type Err = InvalidSessionName;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::new(name)
    }
}

/// A nonblocking listener and the socket pathname it owns.
#[derive(Debug)]
pub struct SessionEndpoint {
    listener: UnixListener,
    location: rename::Identity,
    identity: (u64, u64),
    unlink_on_drop: bool,
}

impl SessionEndpoint {
    /// Bind a session name below the current user's private runtime directory.
    pub fn bind(name: &SessionName) -> io::Result<Self> {
        Self::bind_in(&session_directory(), name)
    }

    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    pub fn path(&self) -> PathBuf {
        self.location.path()
    }

    pub(crate) fn rename_identity(&self) -> rename::Identity {
        self.location.clone()
    }

    fn bind_in(directory: &Path, name: &SessionName) -> io::Result<Self> {
        ensure_private_directory(directory)?;
        let path = socket_path_in(directory, name);
        remove_stale_socket(&path)?;

        let listener = UnixListener::bind(&path)?;
        let configured = (|| {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            drop(open_lock_file(directory, name)?);
            drop(open_pid_file(directory, name, true)?);
            match fs::remove_file(last_connected_path_in(directory, name)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let metadata = fs::symlink_metadata(&path)?;
            Ok((metadata.dev(), metadata.ino()))
        })();
        let identity = match configured {
            Ok(identity) => identity,
            Err(error) => {
                let _ = fs::remove_file(&path);
                return Err(error);
            }
        };

        Ok(Self {
            listener,
            location: rename::Identity::new(directory.to_owned(), name.clone(), identity),
            identity,
            unlink_on_drop: true,
        })
    }

    /// Close this process's listener copy without unlinking a forked server's path.
    pub(crate) fn relinquish(mut self) -> PathBuf {
        self.unlink_on_drop = false;
        self.path()
    }
}

impl Drop for SessionEndpoint {
    fn drop(&mut self) {
        if !self.unlink_on_drop {
            return;
        }
        let path = self.path();
        let owns_path = fs::symlink_metadata(&path)
            .map(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
            .unwrap_or(false);
        if owns_path {
            let _ = fs::remove_file(path.with_extension("lock"));
            let _ = fs::remove_file(path.with_extension("pid"));
            let _ = fs::remove_file(path.with_extension("last"));
            // `kill` waits for the socket to disappear before a caller can
            // recreate this name. Publish that disappearance last, so an old
            // server cannot remove the replacement's freshly created leases.
            let _ = fs::remove_file(&path);
        }
    }
}

/// Return the fixed, short runtime path used for this effective user.
pub fn session_directory() -> PathBuf {
    PathBuf::from("/tmp").join(format!("rustmux-{}", effective_user_id()))
}

/// Return the socket path for a validated session name without creating it.
pub fn session_socket_path(name: &SessionName) -> PathBuf {
    socket_path_in(&session_directory(), name)
}

/// Connect only through an endpoint owned by this user in the private directory.
pub fn connect(name: &SessionName) -> io::Result<UnixStream> {
    connect_in(&session_directory(), name)
}

/// Hold exclusive ownership of the one displayed client for a session.
#[derive(Debug)]
pub(crate) struct ClientLease {
    _lock: Flock<File>,
}

/// Hold proof that the PID record belongs to the running session server.
#[derive(Debug)]
pub(crate) struct ServerLease {
    _lock: Flock<File>,
}

pub(crate) fn acquire_server(name: &SessionName) -> io::Result<ServerLease> {
    acquire_server_in(&session_directory(), name, std::process::id())
}

fn acquire_server_in(
    directory: &Path,
    name: &SessionName,
    process_id: u32,
) -> io::Result<ServerLease> {
    ensure_private_directory(directory)?;
    let file = open_pid_file(directory, name, true)?;
    let mut lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => lock,
        Err((_, error)) if error == Errno::EWOULDBLOCK => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("session '{name}' already has a server"),
            ));
        }
        Err((_, error)) => return Err(error.into()),
    };
    lock.set_len(0)?;
    lock.seek(SeekFrom::Start(0))?;
    writeln!(lock, "{process_id}")?;
    lock.sync_data()?;
    Ok(ServerLease { _lock: lock })
}

/// Serialize snapshot deletion with loading and binding a restored workspace.
/// This sidecar stays on disk: unlinking it could split concurrent lock owners.
pub(crate) fn acquire_workspace(name: &SessionName) -> io::Result<Flock<File>> {
    acquire_workspace_in(&session_directory(), name)
}

fn acquire_workspace_in(directory: &Path, name: &SessionName) -> io::Result<Flock<File>> {
    ensure_private_directory(directory)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(directory.join(format!("{name}.workspace")))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != effective_user_id() || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe workspace lock",
        ));
    }
    Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| io::Error::from(error))
}

pub(crate) fn delete_saved(name: &SessionName) -> io::Result<()> {
    delete_saved_in(
        &session_directory(),
        &crate::persistence::state_directory()?,
        name,
    )
}

fn delete_saved_in(runtime: &Path, state: &Path, name: &SessionName) -> io::Result<()> {
    let _workspace = acquire_workspace_in(runtime, name)?;
    // Refuse any endpoint, including one whose server is starting or stopping.
    // Creation holds the same workspace lock until its socket has been bound.
    match fs::symlink_metadata(socket_path_in(runtime, name)) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "session '{name}' has a runtime endpoint; stop it before deleting its snapshot"
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    crate::persistence::delete(state, name)
}

pub(crate) fn rename_saved(old: &SessionName, new: &SessionName) -> io::Result<()> {
    rename_saved_in(
        &session_directory(),
        &crate::persistence::state_directory()?,
        old,
        new,
    )
}

fn rename_saved_in(
    runtime: &Path,
    state: &Path,
    old: &SessionName,
    new: &SessionName,
) -> io::Result<()> {
    // Stable ordering and nonblocking acquisition also cover opposite renames.
    let (first, second) = if old <= new { (old, new) } else { (new, old) };
    let _first = acquire_workspace_in(runtime, first)?;
    let _second = if first != second {
        Some(acquire_workspace_in(runtime, second)?)
    } else {
        None
    };
    for name in [old, new] {
        match fs::symlink_metadata(socket_path_in(runtime, name)) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("session '{name}' has a runtime endpoint; stop it before renaming"),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    crate::persistence::rename(state, old, new)
}

/// Return the PID only when a live server owns the PID record's lock.
pub(crate) fn live_server_pid(name: &SessionName) -> io::Result<Pid> {
    live_server_pid_in(&session_directory(), name)
}

fn live_server_pid_in(directory: &Path, name: &SessionName) -> io::Result<Pid> {
    ensure_private_directory(directory)?;
    validate_socket(name, &socket_path_in(directory, name))?;
    let file = open_pid_file(directory, name, false).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            session_not_running(name)
        } else {
            error
        }
    })?;
    let mut file = match Flock::lock(file, FlockArg::LockSharedNonblock) {
        Ok(_) => return Err(session_not_running(name)),
        Err((file, error)) if error == Errno::EWOULDBLOCK => file,
        Err((_, error)) => return Err(error.into()),
    };
    if file.metadata()?.len() > MAX_PID_BYTES {
        return Err(invalid_pid_record(name));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut contents = String::new();
    file.take(MAX_PID_BYTES + 1).read_to_string(&mut contents)?;
    let process_id: i32 = contents
        .trim()
        .parse()
        .ok()
        .filter(|process_id| *process_id > 1)
        .ok_or_else(|| invalid_pid_record(name))?;
    Ok(Pid::from_raw(process_id))
}

pub(crate) fn acquire_client(name: &SessionName) -> io::Result<ClientLease> {
    acquire_client_in(&session_directory(), name)
}

pub(crate) fn wait_for_client_release(
    name: &SessionName,
    deadline: std::time::Instant,
) -> io::Result<()> {
    wait_for_client_release_in(&session_directory(), name, deadline)
}

fn wait_for_client_release_in(
    directory: &Path,
    name: &SessionName,
    deadline: std::time::Instant,
) -> io::Result<()> {
    // The caller holds the workspace lock. Read the existing lease only; a
    // missing or unsafe lock must not manufacture an acknowledgement.
    let mut file = open_lock_file_with(directory, name, false)?;
    let metadata = file.metadata()?;
    let identity = (metadata.dev(), metadata.ino());
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "client did not finish detaching; request may still complete",
            ));
        }
        let metadata = fs::symlink_metadata(lock_path_in(directory, name))?;
        if (metadata.dev(), metadata.ino()) != identity {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "client lease identity changed",
            ));
        }
        match Flock::lock(file, FlockArg::LockSharedNonblock) {
            Ok(_lease) => return Ok(()),
            Err((lease, Errno::EWOULDBLOCK)) => {
                file = lease;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err((_, error)) => return Err(error.into()),
        }
    }
}

fn acquire_client_in(directory: &Path, name: &SessionName) -> io::Result<ClientLease> {
    let _workspace = acquire_workspace_in(directory, name)?;
    ensure_private_directory(directory)?;
    validate_socket(name, &socket_path_in(directory, name))?;
    let file = open_lock_file(directory, name)?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(ClientLease { _lock: lock }),
        Err((_, error)) if error == Errno::EWOULDBLOCK => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("session '{name}' already has an attached client"),
        )),
        Err((_, error)) => Err(error.into()),
    }
}

/// Return running and saved session names in stable name order, without duplicates.
pub fn list() -> io::Result<Vec<SessionName>> {
    Ok(list_info()?
        .into_iter()
        .map(|session| session.name)
        .collect())
}

/// Return only running sessions, for operations such as kill-all.
pub fn list_running() -> io::Result<Vec<SessionName>> {
    Ok(list_info_in(&session_directory())?
        .into_iter()
        .map(|session| session.name)
        .collect())
}

/// Format running and saved sessions as a human-readable table.
pub fn format_list() -> io::Result<String> {
    listing::format_list()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionInfo {
    pub(crate) name: SessionName,
    /// A stopped session with a disk snapshot, rather than a live endpoint.
    pub(crate) saved: bool,
    pub(crate) attached: bool,
    pub(crate) server_pid: Option<i32>,
    pub(crate) last_connected_at: Option<u64>,
}

impl SessionInfo {
    fn status(&self, current: Option<&SessionName>) -> &'static str {
        if self.saved {
            "SAVED"
        } else if current == Some(&self.name) {
            "CURRENT"
        } else if self.attached {
            "ATTACHED"
        } else {
            "DETACHED"
        }
    }

    fn last_connected_label(&self, current: Option<&SessionName>, now: u64) -> String {
        if self.saved {
            return "—".to_owned();
        }
        if current == Some(&self.name) || self.attached {
            return "Now".to_owned();
        }
        let Some(timestamp) = self.last_connected_at else {
            return "—".to_owned();
        };
        let age = now.saturating_sub(timestamp) / 1_000;
        if age < 60 {
            format!("{age}s ago")
        } else if age < 60 * 60 {
            format!("{}m ago", age / 60)
        } else if age < 24 * 60 * 60 {
            format!("{}h ago", age / (60 * 60))
        } else {
            format!("{}d ago", age / (24 * 60 * 60))
        }
    }
}

pub(crate) fn order_info(sessions: &mut [SessionInfo], current: Option<&SessionName>) {
    sessions.sort_unstable_by(|left, right| {
        session_rank(left, current)
            .cmp(&session_rank(right, current))
            .then_with(|| right.last_connected_at.cmp(&left.last_connected_at))
            .then_with(|| left.name.cmp(&right.name))
    });
}

fn session_rank(session: &SessionInfo, current: Option<&SessionName>) -> u8 {
    if session.saved {
        3
    } else if current == Some(&session.name) {
        0
    } else if session.attached {
        1
    } else {
        2
    }
}

pub(crate) fn record_connection(name: &SessionName) -> io::Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .as_millis()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "system time is too large"))?;
    record_connection_in(&session_directory(), name, timestamp)
}

fn record_connection_in(directory: &Path, name: &SessionName, timestamp: u64) -> io::Result<()> {
    ensure_private_directory(directory)?;
    validate_socket(name, &socket_path_in(directory, name))?;
    let path = last_connected_path_in(directory, name);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_user_id()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private connection record", path.display()),
        ));
    }
    file.set_len(0)?;
    writeln!(file, "{timestamp}")?;
    file.sync_data()
}

pub(crate) fn list_info() -> io::Result<Vec<SessionInfo>> {
    let live = list_info_in(&session_directory())?;
    let saved = crate::persistence::list_names(&crate::persistence::state_directory()?)?;
    Ok(merge_saved(live, saved))
}

fn merge_saved(mut live: Vec<SessionInfo>, saved: Vec<SessionName>) -> Vec<SessionInfo> {
    let running: std::collections::HashSet<_> = live.iter().map(|s| s.name.clone()).collect();
    live.extend(
        saved
            .into_iter()
            .filter(|name| !running.contains(name))
            .map(|name| SessionInfo {
                name,
                saved: true,
                attached: false,
                server_pid: None,
                last_connected_at: None,
            }),
    );
    live.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    live
}

fn list_info_in(directory: &Path) -> io::Result<Vec<SessionInfo>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_user_id()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private session directory", directory.display()),
        ));
    }

    let mut sessions = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(name) = file_name.strip_suffix(".sock") else {
            continue;
        };
        let Ok(name) = SessionName::new(name) else {
            continue;
        };
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.file_type().is_socket()
            || metadata.uid() != effective_user_id()
            || metadata.permissions().mode() & 0o077 != 0
        {
            continue;
        }
        let Some(attached) = endpoint_client_state(directory, &name, &entry.path()) else {
            continue;
        };
        let server_pid = live_server_pid_in(directory, &name).ok().map(Pid::as_raw);
        sessions.push(SessionInfo {
            saved: false,
            last_connected_at: read_last_connected_in(directory, &name),
            name,
            attached,
            server_pid,
        });
    }
    sessions.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(sessions)
}

fn connect_in(directory: &Path, name: &SessionName) -> io::Result<UnixStream> {
    ensure_private_directory(directory)?;
    let path = socket_path_in(directory, name);
    validate_socket(name, &path)?;
    UnixStream::connect(path).map_err(|error| connect_error(name, error))
}

fn validate_socket(name: &SessionName, path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| connect_error(name, error))?;
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a session socket", path.display()),
        ));
    }
    if metadata.uid() != effective_user_id() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private session socket", path.display()),
        ));
    }
    Ok(())
}

fn connect_error(name: &SessionName, error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::NotFound {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("session '{name}' does not exist"),
        )
    } else {
        error
    }
}

fn socket_path_in(directory: &Path, name: &SessionName) -> PathBuf {
    directory.join(format!("{name}.sock"))
}

fn lock_path_in(directory: &Path, name: &SessionName) -> PathBuf {
    directory.join(format!("{name}.lock"))
}

fn pid_path_in(directory: &Path, name: &SessionName) -> PathBuf {
    directory.join(format!("{name}.pid"))
}

fn last_connected_path_in(directory: &Path, name: &SessionName) -> PathBuf {
    directory.join(format!("{name}.last"))
}

fn read_last_connected_in(directory: &Path, name: &SessionName) -> Option<u64> {
    let path = last_connected_path_in(directory, name);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_user_id()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() > MAX_TIMESTAMP_BYTES
    {
        return None;
    }
    let mut contents = String::new();
    file.take(MAX_TIMESTAMP_BYTES + 1)
        .read_to_string(&mut contents)
        .ok()?;
    contents.trim().parse().ok()
}

fn open_lock_file(directory: &Path, name: &SessionName) -> io::Result<File> {
    open_lock_file_with(directory, name, true)
}

fn open_lock_file_with(directory: &Path, name: &SessionName, create: bool) -> io::Result<File> {
    let path = lock_path_in(directory, name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_user_id()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private session lock", path.display()),
        ));
    }
    Ok(file)
}

fn open_pid_file(directory: &Path, name: &SessionName, create: bool) -> io::Result<File> {
    let path = pid_path_in(directory, name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_user_id()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private session PID file", path.display()),
        ));
    }
    Ok(file)
}

fn session_not_running(name: &SessionName) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("session '{name}' is not running"),
    )
}

fn invalid_pid_record(name: &SessionName) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("session '{name}' has an invalid PID record"),
    )
}

fn endpoint_client_state(directory: &Path, name: &SessionName, socket: &Path) -> Option<bool> {
    let file = match open_lock_file_with(directory, name, false) {
        Ok(file) => file,
        // Endpoints created before client locking have no sidecar file.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return UnixStream::connect(socket).is_ok().then_some(false);
        }
        Err(_) => return None,
    };
    match Flock::lock(file, FlockArg::LockSharedNonblock) {
        // An exclusive client lock proves that the server still has an attachment.
        Err((_, error)) if error == Errno::EWOULDBLOCK => Some(true),
        // Without an attached client, probe the listening server while holding a
        // shared lock so an attachment cannot begin between these observations.
        Ok(_lock) => UnixStream::connect(socket).is_ok().then_some(false),
        Err(_) => None,
    }
}

pub(crate) fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not a directory", directory.display()),
        ));
    }
    if metadata.uid() != effective_user_id() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user", directory.display()),
        ));
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
}

fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is not a socket", path.display()),
        ));
    }

    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("session endpoint {} is already active", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn effective_user_id() -> u32 {
    nix::unistd::geteuid().as_raw()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn wait_for_stale_socket(path: &Path) {
        // Some hosts briefly accept a connect after the listener is closed.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match UnixStream::connect(path) {
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => break,
                Ok(stream) => drop(stream),
                Err(error) => panic!("unexpected stale socket probe error: {error}"),
            }
            assert!(Instant::now() < deadline, "socket remained active");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let number = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "rustmux-session-test-{}-{number}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saved_rename_locks_both_names_and_refuses_source_and_target_endpoints() {
        let runtime = TestDirectory::new();
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let old = SessionName::new("old").unwrap();
        let new = SessionName::new("new").unwrap();
        let path = state.path().join("old.toml");
        fs::write(&path, "snapshot bytes").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        for name in [&old, &new] {
            let busy = acquire_workspace_in(&runtime.0, name).unwrap();
            assert_eq!(
                rename_saved_in(&runtime.0, state.path(), &old, &new)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::WouldBlock
            );
            drop(busy);
            let endpoint = SessionEndpoint::bind_in(&runtime.0, name).unwrap();
            assert_eq!(
                rename_saved_in(&runtime.0, state.path(), &old, &new)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::AlreadyExists
            );
            drop(endpoint);
            assert_eq!(fs::read_to_string(&path).unwrap(), "snapshot bytes");
            assert!(!state.path().join("new.toml").exists());
        }
        rename_saved_in(&runtime.0, state.path(), &old, &new).unwrap();
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(state.path().join("new.toml")).unwrap(),
            "snapshot bytes"
        );
    }

    #[test]
    fn saved_deletion_serializes_with_startup_and_refuses_runtime_endpoints() {
        let runtime = TestDirectory::new();
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let name = SessionName::new("saved").unwrap();
        let path = state.path().join("saved.toml");
        // Deletion also works for corrupt snapshots that cannot be restored.
        fs::write(&path, "version = 999").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let starting = acquire_workspace_in(&runtime.0, &name).unwrap();
        assert_eq!(
            delete_saved_in(&runtime.0, state.path(), &name)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(path.exists());
        let endpoint = SessionEndpoint::bind_in(&runtime.0, &name).unwrap();
        drop(starting);
        assert_eq!(
            delete_saved_in(&runtime.0, state.path(), &name)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(path.exists());
        drop(endpoint);
        delete_saved_in(&runtime.0, state.path(), &name).unwrap();
        assert!(!path.exists());
        // A retained lock path remains the same inode after endpoint cleanup.
        let lock_path = runtime.0.join("saved.workspace");
        let identity = fs::metadata(&lock_path).unwrap().ino();
        drop(acquire_workspace_in(&runtime.0, &name).unwrap());
        assert_eq!(fs::metadata(lock_path).unwrap().ino(), identity);
    }

    #[test]
    fn names_are_bounded_and_cannot_escape_the_session_directory() {
        for name in ["default", "work_2", "build-server", "A9"] {
            assert_eq!(SessionName::new(name).unwrap().as_str(), name);
        }
        for name in ["", ".", "../other", "has space", "中文"] {
            assert!(SessionName::new(name).is_err(), "{name}");
        }
        assert!(SessionName::new("a".repeat(MAX_SESSION_NAME_BYTES)).is_ok());
        assert!(SessionName::new("a".repeat(MAX_SESSION_NAME_BYTES + 1)).is_err());
    }

    #[test]
    fn endpoint_is_private_nonblocking_and_removed_on_drop() {
        let directory = TestDirectory::new();
        let name = SessionName::new("private").unwrap();
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        assert_eq!(
            fs::symlink_metadata(&directory.0)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::symlink_metadata(endpoint.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            endpoint.listener().accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let path = endpoint.path().to_owned();
        drop(endpoint);
        assert!(!path.exists());
    }

    #[test]
    fn active_and_non_socket_paths_are_preserved_but_stale_sockets_are_replaced() {
        let directory = TestDirectory::new();
        let name = SessionName::new("conflict").unwrap();
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        assert_eq!(
            SessionEndpoint::bind_in(&directory.0, &name)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        let path = endpoint.path().to_owned();
        drop(endpoint);

        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        wait_for_stale_socket(&path);
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        drop(endpoint);

        fs::write(&path, b"keep").unwrap();
        assert_eq!(
            SessionEndpoint::bind_in(&directory.0, &name)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(path).unwrap(), b"keep");
    }

    #[test]
    fn dropping_an_old_endpoint_does_not_unlink_a_replacement() {
        let directory = TestDirectory::new();
        let name = SessionName::new("replacement").unwrap();
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        let path = endpoint.path().to_owned();
        fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        drop(endpoint);
        assert!(path.exists());
        drop(replacement);
    }

    #[test]
    fn relinquishing_closes_one_listener_without_removing_the_socket() {
        let directory = TestDirectory::new();
        let name = SessionName::new("forked").unwrap();
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        let path = endpoint.relinquish();
        assert!(path.exists());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn connecting_requires_a_private_socket() {
        let directory = TestDirectory::new();
        let name = SessionName::new("connect").unwrap();
        assert_eq!(
            connect_in(&directory.0, &name).unwrap_err().to_string(),
            "session 'connect' does not exist"
        );
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        drop(connect_in(&directory.0, &name).unwrap());

        fs::set_permissions(endpoint.path(), fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            connect_in(&directory.0, &name).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn disconnect_acknowledgement_waits_for_lease_release_and_obeys_deadline() {
        let directory = TestDirectory::new();
        let name = SessionName::new("release").unwrap();
        let _endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        let lease = acquire_client_in(&directory.0, &name).unwrap();
        use std::time::{Duration, Instant};
        assert_eq!(
            wait_for_client_release_in(
                &directory.0,
                &name,
                Instant::now() + Duration::from_millis(20)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
        drop(lease);
        wait_for_client_release_in(&directory.0, &name, Instant::now() + Duration::from_secs(1))
            .unwrap();
        // An absent lease cannot falsely acknowledge a detached client.
        fs::remove_file(lock_path_in(&directory.0, &name)).unwrap();
        assert_eq!(
            wait_for_client_release_in(
                &directory.0,
                &name,
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn client_lease_is_exclusive_recoverable_and_removed_with_endpoint() {
        let directory = TestDirectory::new();
        let name = SessionName::new("attached").unwrap();
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        let lock_path = lock_path_in(&directory.0, &name);
        assert!(lock_path.exists());
        assert_eq!(
            fs::symlink_metadata(&lock_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let first = acquire_client_in(&directory.0, &name).unwrap();
        let error = acquire_client_in(&directory.0, &name).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            error.to_string(),
            "session 'attached' already has an attached client"
        );
        drop(first);

        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            acquire_client_in(&directory.0, &name).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(acquire_client_in(&directory.0, &name).unwrap());

        drop(endpoint);
        assert!(!lock_path.exists());
    }

    #[test]
    fn connection_time_is_private_readable_and_removed_with_endpoint() {
        let directory = TestDirectory::new();
        let name = SessionName::new("recent").unwrap();
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        record_connection_in(&directory.0, &name, 1_234).unwrap();
        let path = last_connected_path_in(&directory.0, &name);
        assert_eq!(read_last_connected_in(&directory.0, &name), Some(1_234));
        assert_eq!(
            fs::symlink_metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(endpoint);
        assert!(!path.exists());
    }

    #[test]
    fn server_pid_is_reported_only_while_its_exclusive_lease_is_held() {
        let directory = TestDirectory::new();
        let name = SessionName::new("server").unwrap();
        let endpoint = SessionEndpoint::bind_in(&directory.0, &name).unwrap();
        let pid_path = pid_path_in(&directory.0, &name);

        let server = acquire_server_in(&directory.0, &name, 42).unwrap();
        assert_eq!(
            live_server_pid_in(&directory.0, &name).unwrap().as_raw(),
            42
        );
        assert_eq!(
            fs::symlink_metadata(&pid_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(server);

        let error = live_server_pid_in(&directory.0, &name).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(error.to_string(), "session 'server' is not running");
        drop(endpoint);
        assert!(!pid_path.exists());
    }

    #[test]
    fn listing_returns_only_live_private_sessions_in_name_order() {
        let directory = TestDirectory::new();
        fs::create_dir(&directory.0).unwrap();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();

        let beta =
            SessionEndpoint::bind_in(&directory.0, &SessionName::new("beta").unwrap()).unwrap();
        let alpha =
            SessionEndpoint::bind_in(&directory.0, &SessionName::new("alpha").unwrap()).unwrap();
        let attached = acquire_client_in(&directory.0, &SessionName::new("beta").unwrap()).unwrap();
        let stale_path = directory.0.join("stale.sock");
        let stale = UnixListener::bind(&stale_path).unwrap();
        fs::set_permissions(&stale_path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(stale);
        wait_for_stale_socket(&stale_path);
        drop(open_lock_file(&directory.0, &SessionName::new("stale").unwrap()).unwrap());
        let insecure_path = directory.0.join("insecure.sock");
        let insecure = UnixListener::bind(&insecure_path).unwrap();
        fs::set_permissions(&insecure_path, fs::Permissions::from_mode(0o666)).unwrap();
        drop(insecure);
        fs::write(directory.0.join("note.sock"), b"not a socket").unwrap();
        fs::write(directory.0.join("ignored"), b"not an endpoint").unwrap();

        let sessions = list_info_in(&directory.0).unwrap();
        assert_eq!(
            sessions
                .iter()
                .map(|session| (&session.name, session.attached))
                .collect::<Vec<_>>(),
            vec![
                (&SessionName::new("alpha").unwrap(), false),
                (&SessionName::new("beta").unwrap(), true),
            ]
        );
        drop(alpha.listener().accept().unwrap().0);
        assert_eq!(
            beta.listener().accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(attached);
        drop((alpha, beta));
    }

    #[test]
    fn listing_missing_directory_is_empty_and_rejects_public_directory() {
        let directory = TestDirectory::new();
        assert!(list_info_in(&directory.0).unwrap().is_empty());

        fs::create_dir(&directory.0).unwrap();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            list_info_in(&directory.0).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn saved_listing_merges_by_name_and_keeps_live_state_first() {
        let work = SessionName::new("work").unwrap();
        let offline = SessionName::new("offline").unwrap();
        let mut sessions = merge_saved(
            vec![SessionInfo {
                name: work.clone(),
                saved: false,
                attached: true,
                server_pid: Some(123),
                last_connected_at: Some(10),
            }],
            vec![offline.clone(), work.clone()],
        );
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, offline);
        assert_eq!(sessions[1].status(None), "ATTACHED");
        assert_eq!(sessions[1].server_pid, Some(123));
        assert_eq!(sessions[0].status(Some(&offline)), "SAVED");
        assert_eq!(sessions[0].last_connected_label(Some(&offline), 50), "—");
        assert_eq!(sessions[0].server_pid, None);
        order_info(&mut sessions, None);
        assert_eq!(sessions[0].name, work);
        assert_eq!(sessions[1].name, offline);
    }
}
