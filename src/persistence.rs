//! Versioned workspace snapshots. Processes and terminal protocols are never serialized.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    config::{Notifications, PersistenceOptions},
    layout::{Layout, snapshot::SavedLayout},
    pane::Pane,
    pane_set::PaneSet,
    screen::{
        Screen,
        history_snapshot::{SavedRow, validate_rows},
    },
    session::SessionName,
    window::Windows,
};

pub(crate) const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    version: u32,
    rows: u16,
    columns: u16,
    active_window: usize,
    colors: bool,
    windows: Vec<SavedWindow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedWindow {
    name: String,
    layout: SavedLayout,
    panes: Vec<SavedPane>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedPane {
    id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remain_on_exit: Option<bool>,
    directory: Option<PathBuf>,
    history: Vec<SavedRow>,
}

/// Metadata and immutable screen copies taken on the event loop. History formatting
/// and disk work may run elsewhere; that worker never reads live panes or spawns processes.
pub(crate) struct PreparedSnapshot {
    snapshot: Snapshot,
    screens: Vec<(usize, usize, Screen)>,
    history_limit: usize,
}

impl PreparedSnapshot {
    pub(crate) fn capture(
        windows: &Windows<PaneSet<Pane>>,
        rows: u16,
        options: PersistenceOptions,
        history_limit: usize,
    ) -> io::Result<Self> {
        let active = windows.active().map(|w| w.id());
        let mut active_window = 0;
        let mut saved = Vec::new();
        let mut screens = Vec::new();
        for window in windows.iter() {
            // History editor windows are temporary and must never become restored shells.
            if window.content().iter().any(|(_, pane)| pane.is_temporary()) {
                continue;
            }
            let window_index = saved.len();
            if Some(window.id()) == active {
                active_window = window_index;
            }
            let mut panes = Vec::new();
            for (id, pane) in window.content().iter() {
                if options.save_scrollback {
                    screens.push((window_index, panes.len(), pane.screen().clone()));
                }
                panes.push(SavedPane {
                    id: id.get(),
                    command: pane.startup_command().map(str::to_owned),
                    remain_on_exit: pane.remain_on_exit_override(),
                    directory: pane.inherited_directory(),
                    history: Vec::new(),
                });
            }
            saved.push(SavedWindow {
                name: window.name().to_owned(),
                layout: window.content().layout().saved(),
                panes,
            });
        }
        let columns = windows
            .iter()
            .next()
            .ok_or_else(|| invalid("no windows to save"))?
            .content()
            .layout()
            .dimensions()
            .1;
        let snapshot = Snapshot {
            version: VERSION,
            rows,
            columns,
            active_window,
            colors: options.save_scrollback_colors,
            windows: saved,
        };
        snapshot.validate()?;
        Ok(Self {
            snapshot,
            screens,
            history_limit,
        })
    }

    pub(crate) fn finish(mut self) -> Snapshot {
        for (window, pane, screen) in self.screens {
            self.snapshot.windows[window].panes[pane].history =
                screen.saved_history(self.snapshot.colors, self.history_limit);
        }
        self.snapshot
    }
}

impl Snapshot {
    pub(crate) fn from_project(
        project: crate::project::Project,
        rows: u16,
        columns: u16,
    ) -> io::Result<Self> {
        let mut windows = Vec::new();
        for window in project.windows {
            let mut layout = Layout::new(crate::chrome::pane_rows(rows), columns)?;
            let first = layout.active();
            let mut panes = Vec::new();
            for pane in window.panes {
                let id = if panes.is_empty() {
                    first
                } else {
                    layout.split_active(match pane.split {
                        crate::project::Split::Right => crate::layout::SplitAxis::Columns,
                        crate::project::Split::Down => crate::layout::SplitAxis::Rows,
                    })?
                };
                panes.push(SavedPane {
                    id: id.get(),
                    command: pane.command,
                    remain_on_exit: pane.remain_on_exit,
                    directory: Some(pane.cwd),
                    history: Vec::new(),
                });
            }
            layout.select(first)?;
            windows.push(SavedWindow {
                name: window.name,
                layout: layout.saved(),
                panes,
            });
        }
        let snapshot = Self {
            version: VERSION,
            rows,
            columns,
            active_window: 0,
            colors: false,
            windows,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
    pub(crate) fn bootstrap_size(&self) -> (u16, u16) {
        (self.rows, self.columns)
    }
    fn layouts(&self, rows: u16, columns: u16) -> io::Result<Vec<Layout>> {
        if rows == 0
            || columns == 0
            || usize::from(rows) * usize::from(columns) > crate::pane::MAX_CELLS
        {
            return Err(invalid("saved terminal dimensions exceed limits"));
        }
        self.windows
            .iter()
            .map(|window| {
                Layout::from_saved(&window.layout, crate::chrome::pane_rows(rows), columns)
            })
            .collect()
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION {
            return Err(invalid("unsupported human-track snapshot version"));
        }
        if self.windows.is_empty()
            || self.windows.len() > crate::terminal::MAX_WINDOWS
            || self.active_window >= self.windows.len()
        {
            return Err(invalid("invalid saved windows or active window"));
        }
        let layouts = self.layouts(self.rows, self.columns)?;
        for (window, layout) in self.windows.iter().zip(layouts) {
            if window.name.len() > 128 || window.name.chars().any(char::is_control) {
                return Err(invalid("invalid saved window name"));
            }
            let mut ids: Vec<_> = window.panes.iter().map(|pane| pane.id).collect();
            ids.sort_unstable();
            let mut leaves: Vec<_> = layout
                .tiled_geometry()
                .panes
                .into_iter()
                .map(|(id, _)| id.get())
                .collect();
            leaves.sort_unstable();
            if ids != leaves {
                return Err(invalid("saved pane contents do not match layout leaves"));
            }
            for pane in &window.panes {
                crate::project::validate_command(pane.command.as_deref())?;
                if pane
                    .directory
                    .as_ref()
                    .is_some_and(|path| !path.is_absolute() || path.to_str().is_none())
                {
                    return Err(invalid("saved pane directory must be absolute UTF-8"));
                }
                validate_rows(&pane.history, self.colors)?;
            }
        }
        Ok(())
    }

    pub(crate) fn restore(
        &self,
        shell: &OsStr,
        rows: u16,
        columns: u16,
        notifications: Notifications,
        history_limit: usize,
        restore_history: bool,
    ) -> io::Result<Windows<PaneSet<Pane>>> {
        self.validate()?;
        // Validate every destination before any shell starts. RAII cleans up
        // already created panes if a later exec or allocation fails.
        let layouts = self.layouts(rows, columns)?;
        let mut windows = Windows::default();
        let mut ids = Vec::new();
        for (window, layout) in self.windows.iter().zip(layouts) {
            let panes = PaneSet::from_layout_with(layout, |id, rect| {
                let saved = window
                    .panes
                    .iter()
                    .find(|pane| pane.id == id.get())
                    .unwrap();
                let mut pane = Pane::spawn_with_startup(
                    shell,
                    saved.directory.as_deref(),
                    rect.rows,
                    rect.columns,
                    notifications,
                    history_limit,
                    saved.command.as_deref(),
                )?;
                pane.set_remain_on_exit(saved.remain_on_exit);
                if restore_history {
                    pane.parts_mut()
                        .2
                        .restore_history(&saved.history, self.colors)?;
                }
                Ok(pane)
            })?;
            ids.push(windows.create(window.name.clone(), panes)?);
        }
        windows.select(ids[self.active_window])?;
        Ok(windows)
    }
}

pub(crate) fn state_directory() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| invalid("neither XDG_STATE_HOME nor HOME is set"))?;
    if !base.is_absolute() {
        return Err(invalid("state directory must be absolute"));
    }
    // main uses another schema. Keep its snapshots separate from this track.
    Ok(base.join("rustmux/main-human/sessions"))
}

fn check_directory(directory: &Path, create: bool) -> io::Result<()> {
    if create {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
    }
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "snapshot directory is not private",
        ));
    }
    Ok(())
}

/// Enumerate private snapshot files without decoding potentially large histories.
/// Content validation still happens before restoring a selected workspace.
pub(crate) fn list_names(directory: &Path) -> io::Result<Vec<SessionName>> {
    match check_directory(directory, false) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        result => result?,
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name
            .to_str()
            .and_then(|name| name.strip_suffix(".toml"))
        else {
            continue;
        };
        let Ok(name) = SessionName::new(name) else {
            continue;
        };
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.is_file()
            && metadata.uid() == nix::unistd::geteuid().as_raw()
            && metadata.mode() & 0o077 == 0
        {
            names.push(name);
        }
    }
    names.sort_unstable();
    Ok(names)
}

pub(crate) fn load(directory: &Path, name: &SessionName) -> io::Result<Option<Snapshot>> {
    match check_directory(directory, false) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    }
    let path = directory.join(format!("{name}.toml"));
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "snapshot is not a private regular file",
        ));
    }
    let mut text = String::new();
    file.take(MAX_SNAPSHOT_BYTES as u64 + 1)
        .read_to_string(&mut text)?;
    if text.len() > MAX_SNAPSHOT_BYTES {
        return Err(invalid("snapshot exceeds 32 MiB"));
    }
    let snapshot: Snapshot = toml::from_str(&text).map_err(|error| invalid(error.to_string()))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

pub(crate) fn save(directory: &Path, name: &SessionName, snapshot: &Snapshot) -> io::Result<()> {
    snapshot.validate()?;
    let text = toml::to_string(snapshot).map_err(|error| invalid(error.to_string()))?;
    if text.len() > MAX_SNAPSHOT_BYTES {
        return Err(invalid("snapshot exceeds 32 MiB"));
    }
    check_directory(directory, true)?;
    let path = directory.join(format!("{name}.toml"));
    match fs::symlink_metadata(&path) {
        Ok(metadata)
            if !metadata.is_file()
                || metadata.uid() != nix::unistd::geteuid().as_raw()
                || metadata.mode() & 0o077 != 0 =>
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to replace an unsafe snapshot",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(text.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Snapshot {
        let mut layout = Layout::new(crate::chrome::pane_rows(24), 80).unwrap();
        layout
            .split_active(crate::layout::SplitAxis::Columns)
            .unwrap();
        Snapshot {
            version: VERSION,
            rows: 24,
            columns: 80,
            active_window: 0,
            colors: false,
            windows: vec![SavedWindow {
                name: "dev".into(),
                layout: layout.saved(),
                panes: vec![
                    SavedPane {
                        id: 0,
                        command: None,
                        remain_on_exit: None,
                        directory: Some(PathBuf::from("/tmp")),
                        history: vec![],
                    },
                    SavedPane {
                        id: 1,
                        command: None,
                        remain_on_exit: None,
                        directory: None,
                        history: vec![SavedRow {
                            text: "retained output".into(),
                            continued: false,
                        }],
                    },
                ],
            }],
        }
    }

    #[test]
    fn lists_only_private_named_regular_snapshots_without_decoding_history() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        save(
            directory.path(),
            &SessionName::new("work").unwrap(),
            &sample(),
        )
        .unwrap();
        // A corrupt private snapshot stays discoverable; restore reports its error.
        let corrupt = directory.path().join("broken.toml");
        fs::write(&corrupt, "version = 999").unwrap();
        fs::set_permissions(&corrupt, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(directory.path().join("public.toml"), "").unwrap();
        fs::set_permissions(
            directory.path().join("public.toml"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::os::unix::fs::symlink(&corrupt, directory.path().join("link.toml")).unwrap();
        fs::create_dir(directory.path().join("folder.toml")).unwrap();
        fs::write(directory.path().join("bad name.toml"), "").unwrap();
        fs::write(directory.path().join("ignored.tmp"), "").unwrap();
        assert_eq!(
            list_names(directory.path())
                .unwrap()
                .iter()
                .map(SessionName::as_str)
                .collect::<Vec<_>>(),
            ["broken", "work"]
        );
        assert!(
            list_names(&directory.path().join("missing"))
                .unwrap()
                .is_empty()
        );
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            list_names(directory.path()).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn atomic_private_roundtrip_and_invalid_replacement_preserves_previous_file() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let name = SessionName::new("work").unwrap();
        save(directory.path(), &name, &sample()).unwrap();
        let path = directory.path().join("work.toml");
        let before = fs::read(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let loaded = load(directory.path(), &name).unwrap().unwrap();
        assert_eq!(
            loaded.windows[0].panes[1].history[0].text,
            "retained output"
        );
        let mut bad = sample();
        bad.active_window = 100;
        assert!(save(directory.path(), &name, &bad).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        bad = sample();
        bad.version += 1;
        assert!(bad.validate().is_err());
        bad = sample();
        bad.windows[0].panes[0].id = 1;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn refuses_symlinks_insecure_files_and_nonregular_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let name = SessionName::new("work").unwrap();
        let target = directory.path().join("target");
        fs::write(&target, "untouched").unwrap();
        let path = directory.path().join("work.toml");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(load(directory.path(), &name).is_err());
        assert!(save(directory.path(), &name, &sample()).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "untouched");
        fs::remove_file(&path).unwrap();
        fs::write(&path, toml::to_string(&sample()).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(directory.path(), &name).is_err());
        fs::remove_file(&path).unwrap();
        nix::unistd::mkfifo(
            &path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(load(directory.path(), &name).is_err());
    }

    #[test]
    fn rejects_oversized_files_unknown_fields_and_unsafe_history_before_restore() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let name = SessionName::new("work").unwrap();
        let path = directory.path().join("work.toml");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.set_len(MAX_SNAPSHOT_BYTES as u64 + 1).unwrap();
        assert!(load(directory.path(), &name).is_err());
        fs::write(
            &path,
            format!("unknown_field = 1\n{}", toml::to_string(&sample()).unwrap()),
        )
        .unwrap();
        assert!(load(directory.path(), &name).is_err());
        let mut bad = sample();
        bad.windows[0].panes[1].history[0].text = "\x1b[?1049h".into();
        assert!(bad.validate().is_err());
    }
}
