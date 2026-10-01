//! Filesystem refreshes and workspace operations stay off the picker input loop.
use super::*;
use std::sync::{
    Mutex,
    mpsc::{self, SyncSender},
};
use std::thread::{self, JoinHandle};

enum Request {
    Save(SessionName),
    Delete(SessionName),
    Rename(SessionName, SessionName, Option<i32>),
    Disconnect(SessionName, i32),
    Stop,
}
#[derive(Default)]
pub(super) struct Updates {
    pub current: Option<SessionName>,
    pub list: Option<Result<Vec<SessionInfo>, String>>,
    pub save: Option<(SessionName, Result<(), String>)>,
    pub delete: Option<(SessionName, Result<(), String>)>,
    pub rename: Option<(SessionName, SessionName, Result<(), String>)>,
    pub disconnect: Option<(SessionName, Result<(), String>)>,
}
pub(super) struct Worker {
    requests: SyncSender<Request>,
    updates: Arc<Mutex<Updates>>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn new(current: Option<SessionName>) -> io::Result<Self> {
        let (requests, rx) = mpsc::sync_channel(1);
        let updates = Arc::new(Mutex::new(Updates::default()));
        let output = updates.clone();
        let thread = thread::Builder::new()
            .name("rustmux-manager".into())
            .spawn(move || {
                use std::os::unix::fs::MetadataExt;
                let mut current = current;
                let socket = current
                    .as_ref()
                    .and_then(|name| {
                        std::fs::symlink_metadata(super::super::session_socket_path(name)).ok()
                    })
                    .map(|m| (m.dev(), m.ino()));
                loop {
                    let mut delete = None;
                    let mut rename = None;
                    let mut disconnect = None;
                    let save = match rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(Request::Save(name)) => {
                            let result = super::super::snapshot::save_session_with_timeout(
                                &name,
                                Duration::from_secs(5),
                            )
                            .map_err(|e| e.to_string());
                            Some((name, result))
                        }
                        Ok(Request::Delete(name)) => {
                            let result =
                                super::super::delete_saved(&name).map_err(|e| e.to_string());
                            delete = Some((name, result));
                            None
                        }
                        Ok(Request::Rename(old, new, server)) => {
                            let result = super::super::rename::session(&old, &new, server)
                                .map_err(|e| e.to_string());
                            rename = Some((old, new, result));
                            None
                        }
                        Ok(Request::Disconnect(name, server)) => {
                            let result = crate::control::disconnect_session(&name, server)
                                .map_err(|e| e.to_string());
                            disconnect = Some((name, result));
                            None
                        }
                        Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                    };
                    let list = super::super::list_info()
                        .map(|mut sessions| {
                            if let Some(identity) = socket {
                                current = sessions
                                    .iter()
                                    .find(|s| {
                                        std::fs::symlink_metadata(
                                            super::super::session_socket_path(&s.name),
                                        )
                                        .is_ok_and(|m| (m.dev(), m.ino()) == identity)
                                    })
                                    .map(|s| s.name.clone())
                                    .or(current.take());
                            }
                            super::super::order_info(&mut sessions, current.as_ref());
                            sessions
                        })
                        .map_err(|e| e.to_string());
                    let mut slot = output.lock().expect("manager mailbox");
                    slot.list = Some(list);
                    slot.current = current.clone();
                    if rename.is_some() {
                        slot.rename = rename;
                    }
                    if disconnect.is_some() {
                        slot.disconnect = disconnect;
                    }
                    if delete.is_some() {
                        slot.delete = delete;
                    }
                    if save.is_some() {
                        slot.save = save;
                    }
                }
            })?;
        Ok(Self {
            requests,
            updates,
            thread: Some(thread),
        })
    }
    pub fn save(&self, name: SessionName) -> bool {
        self.requests.try_send(Request::Save(name)).is_ok()
    }
    pub fn delete(&self, name: SessionName) -> bool {
        self.requests.try_send(Request::Delete(name)).is_ok()
    }
    pub fn rename(&self, old: SessionName, new: SessionName, server: Option<i32>) -> bool {
        self.requests
            .try_send(Request::Rename(old, new, server))
            .is_ok()
    }
    pub fn disconnect(&self, name: SessionName, server: i32) -> bool {
        self.requests
            .try_send(Request::Disconnect(name, server))
            .is_ok()
    }
    pub fn poll(&self) -> Updates {
        self.updates
            .try_lock()
            .ok()
            .map(|mut slot| std::mem::take(&mut *slot))
            .unwrap_or_default()
    }
    pub fn shutdown(mut self) {
        self.finish();
    }
    fn finish(&mut self) {
        if let Some(thread) = self.thread.take() {
            // Complete accepted operations and end all workers before a picker choice
            // can restore/create a session through the supervisor's fork path.
            let _ = self.requests.send(Request::Stop);
            let _ = thread.join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.finish();
    }
}
