//! Server-owned live rename. Keep listening descriptors, PID and client leases.
use super::*;
use std::ffi::CString;
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug)]
pub(crate) struct Identity {
    directory: PathBuf,
    name: Arc<RwLock<SessionName>>,
    socket: (u64, u64),
}
impl Identity {
    pub fn new(directory: PathBuf, name: SessionName, socket: (u64, u64)) -> Self {
        Self {
            directory,
            name: Arc::new(RwLock::new(name)),
            socket,
        }
    }
    pub fn name(&self) -> SessionName {
        self.name.read().expect("session identity").clone()
    }
    pub fn path(&self) -> PathBuf {
        socket_path_in(&self.directory, &self.name())
    }

    pub fn rename(
        &self,
        source: &str,
        target: &str,
        snapshots: &mut snapshot::SnapshotService,
    ) -> io::Result<String> {
        let old = self.name();
        if old.as_str() != source {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "session name changed; refresh and retry",
            ));
        }
        let new =
            SessionName::new(target).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let (first, second) = if old <= new {
            (&old, &new)
        } else {
            (&new, &old)
        };
        let _first = acquire_workspace_in(&self.directory, first)?;
        let _second = if first != second {
            Some(acquire_workspace_in(&self.directory, second)?)
        } else {
            None
        };
        let metadata = fs::symlink_metadata(self.path())?;
        if (metadata.dev(), metadata.ino()) != self.socket {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "session endpoint identity changed",
            ));
        }
        if old == new {
            return Ok(String::new());
        }
        snapshots.rename_ready()?;
        move_paths(&self.directory, snapshots.directory(), &old, &new)?;
        snapshots.renamed(&new);
        *self.name.write().expect("session identity") = new;
        Ok(String::new())
    }
}

fn move_paths(
    runtime: &Path,
    state: &Path,
    old: &SessionName,
    new: &SessionName,
) -> io::Result<()> {
    let runtime = private_directory(runtime)?;
    let mut moves = Vec::new();
    // Publish the listening socket last. Existing client and server locks
    // move as the same inodes, so their open descriptors keep ownership.
    for extension in ["pid", "lock", "last", "save", "control"] {
        plan(
            &mut moves,
            &runtime,
            old,
            new,
            extension,
            extension == "last",
            matches!(extension, "save" | "control"),
        )?;
    }
    match private_directory(state) {
        Ok(state) => plan(&mut moves, &state, old, new, "toml", true, false)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    plan(&mut moves, &runtime, old, new, "sock", false, true)?;
    perform(&moves)
}

fn private_directory(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.uid() != effective_user_id() || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "rename directory is not private",
        ));
    }
    Ok(file)
}
struct Move {
    directory: File,
    old: CString,
    new: CString,
}
fn plan(
    moves: &mut Vec<Move>,
    directory: &File,
    old: &SessionName,
    new: &SessionName,
    extension: &str,
    optional: bool,
    socket: bool,
) -> io::Result<()> {
    use nix::{fcntl::AtFlags, sys::stat::fstatat};
    let old = CString::new(format!("{old}.{extension}")).expect("validated name");
    let new = CString::new(format!("{new}.{extension}")).expect("validated name");
    match fstatat(directory, new.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "rename destination {} already exists",
                    new.to_string_lossy()
                ),
            ));
        }
        Err(Errno::ENOENT) => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = match fstatat(directory, old.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(Errno::ENOENT) if optional => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let kind = if socket {
        nix::libc::S_IFSOCK
    } else {
        nix::libc::S_IFREG
    };
    if metadata.st_mode & nix::libc::S_IFMT != kind
        || metadata.st_uid != effective_user_id()
        || metadata.st_mode & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to move an unsafe session artifact",
        ));
    }
    moves.push(Move {
        directory: directory.try_clone()?,
        old,
        new,
    });
    Ok(())
}
fn perform(moves: &[Move]) -> io::Result<()> {
    let mut completed = 0;
    let result = (|| {
        for item in moves {
            crate::persistence::rename_noreplace(&item.directory, &item.old, &item.new)?;
            completed += 1;
        }
        for item in moves {
            item.directory.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for item in moves[..completed].iter().rev() {
            crate::persistence::rename_noreplace(&item.directory, &item.new, &item.old)
                .and_then(|()| item.directory.sync_all())
                .map_err(|rollback| {
                    io::Error::other(format!(
                        "rename failed: {error}; rollback failed: {rollback}"
                    ))
                })?;
        }
        return Err(error);
    }
    Ok(())
}

pub(crate) fn session(old: &SessionName, new: &SessionName, server: Option<i32>) -> io::Result<()> {
    if let Some(server) = server {
        if super::live_server_pid(old)?.as_raw() != server {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "session server changed; refresh and retry",
            ));
        }
        crate::control::rename_session(old, new, server)
    } else {
        super::rename_saved(old, new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transaction_preserves_live_leases_snapshot_bytes_and_listener_identity() {
        let runtime = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let old = SessionName::new("before").unwrap();
        let new = SessionName::new("after").unwrap();
        let endpoint = SessionEndpoint::bind_in(runtime.path(), &old).unwrap();
        let server = acquire_server_in(runtime.path(), &old, std::process::id()).unwrap();
        let client = acquire_client_in(runtime.path(), &old).unwrap();
        let mut listeners = Vec::new();
        for extension in ["save", "control"] {
            let path = endpoint.path().with_extension(extension);
            listeners.push(UnixListener::bind(&path).unwrap());
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let old_snapshot = state.path().join("before.toml");
        fs::write(&old_snapshot, b"retained snapshot bytes").unwrap();
        fs::set_permissions(&old_snapshot, fs::Permissions::from_mode(0o600)).unwrap();
        let inode = fs::metadata(&old_snapshot).unwrap().ino();
        move_paths(runtime.path(), state.path(), &old, &new).unwrap();
        let identity = endpoint.rename_identity();
        *identity.name.write().unwrap() = new.clone();
        assert!(!old_snapshot.exists());
        let new_snapshot = state.path().join("after.toml");
        assert_eq!(fs::read(&new_snapshot).unwrap(), b"retained snapshot bytes");
        assert_eq!(fs::metadata(new_snapshot).unwrap().ino(), inode);
        assert_eq!(
            live_server_pid_in(runtime.path(), &new).unwrap().as_raw(),
            std::process::id() as i32
        );
        assert!(live_server_pid_in(runtime.path(), &old).is_err());
        assert_eq!(
            acquire_client_in(runtime.path(), &new).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(UnixStream::connect(endpoint.path()).is_ok());
        drop(client);
        drop(acquire_client_in(runtime.path(), &new).unwrap());
        let reused = SessionEndpoint::bind_in(runtime.path(), &old).unwrap();
        drop(server);
        drop(endpoint);
        assert!(
            reused.path().exists(),
            "renamed endpoint cleanup removed reused old name"
        );
        assert!(!runtime.path().join("after.sock").exists());
        assert!(!runtime.path().join("after.pid").exists());
    }

    #[test]
    fn failed_exclusive_move_rolls_back_prior_artifacts_without_overwriting_racer() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let old = SessionName::new("old").unwrap();
        let new = SessionName::new("new").unwrap();
        let dir = private_directory(directory.path()).unwrap();
        let mut moves = Vec::new();
        for extension in ["pid", "lock"] {
            let path = directory.path().join(format!("old.{extension}"));
            fs::write(&path, extension).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            plan(&mut moves, &dir, &old, &new, extension, false, false).unwrap();
        }
        fs::write(directory.path().join("new.lock"), "racer").unwrap();
        assert_eq!(
            perform(&moves).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("old.pid")).unwrap(),
            "pid"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("old.lock")).unwrap(),
            "lock"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("new.lock")).unwrap(),
            "racer"
        );
        assert!(!directory.path().join("new.pid").exists());
    }
}
