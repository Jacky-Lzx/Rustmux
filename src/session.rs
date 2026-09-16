//! Secure local endpoints for persistent Rustmux sessions.

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

/// A nonblocking listener and the socket pathname it owns.
#[derive(Debug)]
pub struct SessionEndpoint {
    listener: UnixListener,
    path: PathBuf,
    identity: (u64, u64),
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
        let path = directory.join(format!("{name}.sock"));
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
        })
    }
}

impl Drop for SessionEndpoint {
    fn drop(&mut self) {
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
}
