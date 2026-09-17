//! Secure local endpoints for persistent Rustmux sessions.

pub mod client;
pub mod frontend;
pub mod handshake;
pub mod protocol;
pub mod supervisor;

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

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

/// Return private session endpoints in stable name order.
pub fn list() -> io::Result<Vec<SessionName>> {
    list_in(&session_directory())
}

fn list_in(directory: &Path) -> io::Result<Vec<SessionName>> {
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
        sessions.push(name);
    }
    sessions.sort_unstable();
    Ok(sessions)
}

fn connect_in(directory: &Path, name: &SessionName) -> io::Result<UnixStream> {
    ensure_private_directory(directory)?;
    let path = socket_path_in(directory, name);
    let metadata = fs::symlink_metadata(&path).map_err(|error| connect_error(name, error))?;
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
    UnixStream::connect(path).map_err(|error| connect_error(name, error))
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
    fn listing_returns_only_private_session_endpoints_in_name_order() {
        let directory = TestDirectory::new();
        fs::create_dir(&directory.0).unwrap();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();

        let beta =
            SessionEndpoint::bind_in(&directory.0, &SessionName::new("beta").unwrap()).unwrap();
        let alpha =
            SessionEndpoint::bind_in(&directory.0, &SessionName::new("alpha").unwrap()).unwrap();
        let stale_path = directory.0.join("stale.sock");
        let stale = UnixListener::bind(&stale_path).unwrap();
        fs::set_permissions(&stale_path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(stale);
        let insecure_path = directory.0.join("insecure.sock");
        let insecure = UnixListener::bind(&insecure_path).unwrap();
        fs::set_permissions(&insecure_path, fs::Permissions::from_mode(0o666)).unwrap();
        drop(insecure);
        fs::write(directory.0.join("note.sock"), b"not a socket").unwrap();
        fs::write(directory.0.join("ignored"), b"not an endpoint").unwrap();

        assert_eq!(
            list_in(&directory.0).unwrap(),
            vec![
                SessionName::new("alpha").unwrap(),
                SessionName::new("beta").unwrap(),
                SessionName::new("stale").unwrap(),
            ]
        );
        drop((alpha, beta));
    }

    #[test]
    fn listing_missing_directory_is_empty_and_rejects_public_directory() {
        let directory = TestDirectory::new();
        assert!(list_in(&directory.0).unwrap().is_empty());

        fs::create_dir(&directory.0).unwrap();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            list_in(&directory.0).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
