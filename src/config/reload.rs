//! One bounded reader per configured session; terminal loops never read config files.
use super::*;
use serde::Serialize;
use std::{
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

const INTERVAL: Duration = Duration::from_millis(500);
const MAX_BYTES: u64 = 512 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Source {
    pub path: PathBuf,
    pub allow_missing: bool,
}

type Update = Result<Config, String>;

pub(crate) struct Reload {
    current: Config,
    mailbox: Arc<Mutex<Option<Update>>>,
    stop: Arc<AtomicBool>,
    candidate: Option<Config>,
    error: Option<String>,
    generation: u64,
}

impl Reload {
    pub fn new(config: &Config) -> io::Result<Option<Self>> {
        let Some(source) = config.source.clone() else {
            return Ok(None);
        };
        let mailbox = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let output = mailbox.clone();
        let cancelled = stop.clone();
        thread::Builder::new()
            .name("rustmux-config".into())
            .spawn(move || {
                let mut last: Option<Result<Option<String>, String>> = None;
                while !cancelled.load(Ordering::Relaxed) {
                    let text = read(&source);
                    if last.as_ref() != Some(&text) {
                        let update = text
                            .as_ref()
                            .map_err(Clone::clone)
                            .and_then(|text| {
                                let parsed = match text {
                                    Some(text) => parse_config(text).map_err(|error| {
                                        format!("invalid {}: {error}", source.path.display())
                                    })?,
                                    None => ParsedConfig::default(),
                                };
                                let mut config = resolve_config(parsed);
                                config.source = Some(source.clone());
                                Ok(config)
                            })
                            .map_err(|error| {
                                error
                                    .chars()
                                    .take(512)
                                    .map(|c| if c.is_control() { ' ' } else { c })
                                    .collect()
                            });
                        // Replace unsampled updates; a deferred UI never grows a queue.
                        *output.lock().expect("config mailbox") = Some(update);
                        last = Some(text);
                    }
                    thread::sleep(INTERVAL);
                }
            })?;
        Ok(Some(Self {
            current: config.clone(),
            mailbox,
            stop,
            candidate: None,
            error: None,
            generation: 0,
        }))
    }

    pub fn poll(&mut self) {
        let update = self
            .mailbox
            .try_lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(update) = update {
            let update = update.and_then(|config| {
                if config.shortcuts.locked_entry_key() != self.current.shortcuts.locked_entry_key()
                    || config.shortcuts.clear_defaults() != self.current.shortcuts.clear_defaults()
                {
                    Err(String::from(
                        "configuration reload requires restart to change the entry key or clear_defaults",
                    ))
                } else {
                    Ok(config)
                }
            });
            match update {
                Ok(config) => {
                    self.error = None;
                    self.candidate = (config != self.current).then_some(config);
                }
                Err(error) => {
                    self.error = Some(error);
                    self.candidate = None;
                }
            }
        }
    }
    pub fn pending(&self) -> bool {
        self.candidate.is_some()
    }
    pub fn take(&mut self) -> Option<Config> {
        self.candidate.take()
    }
    pub fn commit(&mut self, config: Config) {
        self.current = config;
        self.generation += 1;
    }
    pub fn current(&self) -> &Config {
        &self.current
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn status(&self) -> io::Result<String> {
        #[derive(Serialize)]
        struct Status<'a> {
            path: String,
            generation: u64,
            pending: bool,
            error: Option<&'a str>,
            new_window_key: Option<String>,
            settings: Settings,
        }
        toml::to_string(&Status {
            path: self
                .current
                .source
                .as_ref()
                .unwrap()
                .path
                .to_string_lossy()
                .into_owned(),
            generation: self.generation,
            pending: self.candidate.is_some(),
            error: self.error(),
            new_window_key: self
                .current
                .shortcuts
                .action_is_active(b'c')
                .then(|| char::from(self.current.shortcuts.key_for(b'c')).to_string()),
            settings: Settings::from(&self.current),
        })
        .map_err(io::Error::other)
    }
}
impl Drop for Reload {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn read(source: &Source) -> Result<Option<String>, String> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(&source.path)
    {
        Ok(file) => file,
        Err(error) if source.allow_missing && error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(format!("could not read {}: {error}", source.path.display())),
    };
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("configuration reload requires a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("configuration reload exceeds 512 KiB limit".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait(reload: &mut Reload, predicate: impl Fn(&Reload) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            reload.poll();
            if predicate(reload) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "reload timeout: {:?}",
                reload.error()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn initialized(reload: &mut Reload) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let ready = reload
                .mailbox
                .try_lock()
                .ok()
                .is_some_and(|slot| slot.is_some());
            if ready {
                reload.poll();
                return;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn invalid_updates_keep_last_good_and_explicit_deletion_reports_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        fs::write(&path, "scrollback_lines=7").unwrap();
        let config = load_with_path(Some(&path)).unwrap();
        let mut reload = Reload::new(&config).unwrap().unwrap();
        initialized(&mut reload);
        fs::write(&path, "scrollback_lines='invalid'").unwrap();
        wait(&mut reload, |r| r.error().is_some());
        assert_eq!(reload.current().scrollback_lines(), 7);
        assert!(reload.take().is_none());
        fs::write(&path, "scrollback_lines=9").unwrap();
        wait(&mut reload, |r| r.pending());
        let config = reload.take().unwrap();
        assert_eq!(config.scrollback_lines(), 9);
        reload.commit(config);
        assert!(reload.error().is_none());
        fs::remove_file(&path).unwrap();
        wait(&mut reload, |r| r.error().is_some());
        assert_eq!(reload.current().scrollback_lines(), 9);
    }
    #[test]
    fn detects_same_size_same_timestamp_changes_and_restores_discovered_defaults() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        fs::write(&path, "scrollback_lines=7").unwrap();
        let mut config = load_with_path(Some(&path)).unwrap();
        config.source.as_mut().unwrap().allow_missing = true;
        let mut reload = Reload::new(&config).unwrap().unwrap();
        initialized(&mut reload);
        // Replace content without using size or timestamp as a fingerprint.
        let timestamp = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, "scrollback_lines=9").unwrap();
        fs::File::open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(timestamp))
            .unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), timestamp);
        wait(&mut reload, |r| r.pending());
        let config = reload.take().unwrap();
        assert_eq!(config.scrollback_lines(), 9);
        reload.commit(config);
        fs::remove_file(&path).unwrap();
        wait(&mut reload, |r| r.pending());
        assert_eq!(
            reload.take().unwrap().scrollback_lines(),
            DEFAULT_SCROLLBACK_LINES
        );
    }
    #[test]
    fn restart_only_changes_are_atomic_and_newer_invalid_data_cancels_pending_config() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        fs::write(&path, "remain_on_exit=false").unwrap();
        let config = load_with_path(Some(&path)).unwrap();
        let mut reload = Reload::new(&config).unwrap().unwrap();
        initialized(&mut reload);
        for entry_changed in [true, false] {
            let mut changed = config.clone();
            changed.remain_on_exit = true;
            if entry_changed {
                changed.shortcuts.locked_enter = 1;
            } else {
                changed.shortcuts.clear_defaults = true;
            }
            *reload.mailbox.lock().unwrap() = Some(Ok(changed));
            reload.poll();
            assert!(reload.error().unwrap().contains("restart"));
            assert!(reload.take().is_none());
            assert!(!reload.current().remain_on_exit());
        }
        let mut changed = config.clone();
        changed.remain_on_exit = true;
        *reload.mailbox.lock().unwrap() = Some(Ok(changed));
        reload.poll();
        assert!(reload.pending());
        *reload.mailbox.lock().unwrap() = Some(Err("invalid latest file".into()));
        reload.poll();
        assert!(reload.take().is_none());
        assert!(!reload.current().remain_on_exit());
    }
    #[test]
    fn bounded_reader_rejects_large_invalid_utf8_and_special_files() {
        let root = tempfile::tempdir().unwrap();
        let source = Source {
            path: root.path().join("config.toml"),
            allow_missing: false,
        };
        fs::write(&source.path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert!(read(&source).unwrap_err().contains("512 KiB"));
        fs::write(&source.path, [255]).unwrap();
        assert!(read(&source).is_err());
        fs::remove_file(&source.path).unwrap();
        nix::unistd::mkfifo(
            &source.path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(read(&source).unwrap_err().contains("regular file"));
    }
}
