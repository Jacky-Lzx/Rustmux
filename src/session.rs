//! Secure local endpoints for persistent Rustmux sessions.

pub mod client;
pub mod frontend;
pub mod handshake;
mod picker;
pub mod protocol;
pub mod supervisor;

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use nix::unistd::Pid;

const MAX_PID_BYTES: u64 = 32;

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
    path: PathBuf,
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

    pub fn path(&self) -> &Path {
        &self.path
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
            path,
            identity,
            unlink_on_drop: true,
        })
    }

    /// Close this process's listener copy without unlinking a forked server's path.
    pub(crate) fn relinquish(mut self) -> PathBuf {
        self.unlink_on_drop = false;
        self.path.clone()
    }
}

impl Drop for SessionEndpoint {
    fn drop(&mut self) {
        if !self.unlink_on_drop {
            return;
        }
        let owns_path = fs::symlink_metadata(&self.path)
            .map(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
            .unwrap_or(false);
        if owns_path {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(self.path.with_extension("lock"));
            let _ = fs::remove_file(self.path.with_extension("pid"));
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

fn acquire_client_in(directory: &Path, name: &SessionName) -> io::Result<ClientLease> {
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

/// Return private session endpoints in stable name order.
pub fn list() -> io::Result<Vec<SessionName>> {
    Ok(list_info()?
        .into_iter()
        .map(|session| session.name)
        .collect())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionInfo {
    pub(crate) name: SessionName,
    pub(crate) attached: bool,
    pub(crate) server_pid: Option<i32>,
}

pub(crate) fn list_info() -> io::Result<Vec<SessionInfo>> {
    list_info_in(&session_directory())
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

fn ensure_private_directory(directory: &Path) -> io::Result<()> {
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

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

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
}
