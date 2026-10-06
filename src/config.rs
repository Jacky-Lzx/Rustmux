//! Load the small configuration surface supported by the human-reviewed track.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::layout::Direction;

mod diagnostics;
pub(crate) mod manager;
pub(crate) mod reload;
pub use diagnostics::{Inspection, Settings, default_config, inspect};

const DEFAULT_COMMAND_DURATION_SECONDS: u64 = 5;
pub const DEFAULT_SCROLLBACK_LINES: usize = crate::screen::SCROLLBACK_MAX_LINES;
const SHORTCUT_NAMES: [&str; 3] = ["new_window", "split_right", "split_down"];
const DEFAULT_SHORTCUT_KEYS: [u8; 3] = *b"c%\"";
const FIXED_SHORTCUT_KEYS: &[u8] = b"np\t&x<> {}!moZz[Ee?hjkl,1234567890dq";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The mode resumed after closing a History snapshot.
pub enum HistoryMode {
    Locked,
    Normal,
    Pane,
    Resize,
    Move,
    Tab,
    Session,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryKey {
    Byte(u8),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

impl HistoryKey {
    pub(crate) fn sequence(self) -> &'static [u8] {
        match self {
            Self::Byte(_) => &[],
            Self::Up => b"\x1b[A",
            Self::Down => b"\x1b[B",
            Self::Left => b"\x1b[D",
            Self::Right => b"\x1b[C",
            Self::PageUp => b"\x1b[5~",
            Self::PageDown => b"\x1b[6~",
            Self::Home => b"\x1b[H",
            Self::End => b"\x1b[F",
        }
    }

    pub(crate) fn from_sequence(sequence: &[u8]) -> Option<Self> {
        Some(match sequence {
            b"\x1b[A" | b"\x1bOA" => Self::Up,
            b"\x1b[B" | b"\x1bOB" => Self::Down,
            b"\x1b[D" | b"\x1bOD" => Self::Left,
            b"\x1b[C" | b"\x1bOC" => Self::Right,
            b"\x1b[5~" => Self::PageUp,
            b"\x1b[6~" => Self::PageDown,
            b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" | b"\x1b[7~" => Self::Home,
            b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => Self::End,
            _ => return None,
        })
    }

    pub(crate) fn label(self) -> String {
        match self {
            Self::Byte(27) => "Esc".into(),
            Self::Byte(13) => "Enter".into(),
            Self::Byte(9) => "Tab".into(),
            Self::Byte(byte @ 1..=26) => format!("Ctrl-{}", char::from(b'A' + byte - 1)),
            Self::Byte(byte) => char::from(byte).to_string(),
            Self::Up => "↑".into(),
            Self::Down => "↓".into(),
            Self::Left => "←".into(),
            Self::Right => "→".into(),
            Self::PageUp => "PgUp".into(),
            Self::PageDown => "PgDn".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryAction {
    Key(HistoryKey),
    SwitchMode(HistoryMode),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaneAction {
    Break,
    MovePreviousWindow,
    MoveNextWindow,
    SplitRight,
    SplitDown,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    Next,
    Zoom,
    Close,
    Respawn,
    Normal,
    Resize,
    Move,
    Tab,
    Session,
    Locked,
    History,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneBinding {
    key: u8,
    pub action: PaneAction,
    pub stay: bool,
    preferred: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneArrowBinding {
    pub action: PaneAction,
    pub stay: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizeAction {
    Resize(Direction),
    Normal,
    Pane,
    Move,
    Tab,
    Session,
    Locked,
    History,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResizeBinding {
    key: u8,
    pub action: ResizeAction,
    preferred: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoveAction {
    Move(Direction),
    Normal,
    Pane,
    Resize,
    Tab,
    Session,
    Locked,
    History,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MoveBinding {
    key: u8,
    pub action: MoveAction,
    preferred: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabAction {
    Next,
    Previous,
    MoveLeft,
    MoveRight,
    New,
    Rename,
    Close,
    Select(usize),
    Help,
    Normal,
    Pane,
    Resize,
    Move,
    Session,
    Locked,
    History,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabBinding {
    key: u8,
    pub action: TabAction,
    pub stay: bool,
    preferred: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionAction {
    Detach,
    Manager,
    Help,
    Normal,
    Pane,
    Resize,
    Move,
    Tab,
    Locked,
    History,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionBinding {
    key: u8,
    pub action: SessionAction,
    preferred: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shortcuts {
    clear_defaults: bool,
    locked_enter: u8,
    locked_configured: bool,
    keys: [u8; 3],
    legacy_configured: [bool; 3],
    locked_history: [Option<u8>; 8],
    history_bindings: [Option<(HistoryKey, HistoryAction)>; 48],
    history_binding_len: usize,
    normal_actions: [Option<(u8, u8)>; 48],
    normal_action_len: usize,
    pane_enter: Option<u8>,
    resize_enter: Option<u8>,
    move_enter: Option<u8>,
    tab_enter: Option<u8>,
    session_enter: Option<u8>,
    pane_bindings: [Option<PaneBinding>; 32],
    pane_binding_len: usize,
    pane_arrows: [Option<PaneArrowBinding>; 4],
    resize_bindings: [Option<ResizeBinding>; 16],
    resize_binding_len: usize,
    resize_arrows: [Option<ResizeAction>; 4],
    move_bindings: [Option<MoveBinding>; 16],
    move_binding_len: usize,
    move_arrows: [Option<MoveAction>; 4],
    tab_bindings: [Option<TabBinding>; 32],
    tab_binding_len: usize,
    tab_arrows: [Option<TabBinding>; 4],
    session_bindings: [Option<SessionBinding>; 16],
    session_binding_len: usize,
    normal_exit: [u8; 8],
    normal_exit_len: usize,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            clear_defaults: false,
            locked_enter: 2,
            locked_configured: false,
            keys: DEFAULT_SHORTCUT_KEYS,
            legacy_configured: [false; 3],
            locked_history: [None; 8],
            history_bindings: [None; 48],
            history_binding_len: 0,
            normal_actions: [None; 48],
            normal_action_len: 0,
            pane_enter: None,
            resize_enter: None,
            move_enter: None,
            tab_enter: None,
            session_enter: None,
            pane_bindings: [None; 32],
            pane_binding_len: 0,
            pane_arrows: [None; 4],
            resize_bindings: [None; 16],
            resize_binding_len: 0,
            resize_arrows: [None; 4],
            move_bindings: [None; 16],
            move_binding_len: 0,
            move_arrows: [None; 4],
            tab_bindings: [None; 32],
            tab_binding_len: 0,
            tab_arrows: [None; 4],
            session_bindings: [None; 16],
            session_binding_len: 0,
            normal_exit: [0; 8],
            normal_exit_len: 0,
        }
    }
}

impl Shortcuts {
    pub fn clear_defaults(self) -> bool {
        self.clear_defaults
    }

    pub fn locked_entry_key(self) -> u8 {
        self.locked_enter
    }

    #[cfg(test)]
    pub(crate) fn test_from_config(source: &str) -> Self {
        parse_config(source).unwrap().shortcuts
    }
    #[cfg(test)]
    pub(crate) fn test_keys(keys: [u8; 3]) -> Self {
        Self {
            keys,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn test_normal_exit(mut self, key: u8) -> Self {
        self.normal_exit[0] = key;
        self.normal_exit_len = 1;
        self
    }

    #[cfg(test)]
    pub(crate) fn test_normal_action(mut self, key: u8, action: u8) -> Self {
        self.normal_actions[0] = Some((key, action));
        self.normal_action_len = 1;
        self
    }

    pub fn key_for(self, action: u8) -> u8 {
        if let Some((key, _)) = self.normal_actions[..self.normal_action_len]
            .iter()
            .flatten()
            .find(|(key, mapped)| *key == action && *mapped == action)
        {
            return *key;
        }
        if let Some((key, _)) = self.normal_actions[..self.normal_action_len]
            .iter()
            .flatten()
            .find(|(_, mapped)| *mapped == action)
        {
            return *key;
        }
        DEFAULT_SHORTCUT_KEYS
            .iter()
            .position(|&key| key == action)
            .map_or(action, |index| self.keys[index])
    }

    pub fn resolve(self, key: u8) -> Option<u8> {
        if let Some((_, action)) = self.normal_actions[..self.normal_action_len]
            .iter()
            .flatten()
            .find(|(configured, _)| *configured == key)
        {
            return Some(*action);
        }
        if self.normal_actions[..self.normal_action_len]
            .iter()
            .flatten()
            .any(|(_, action)| *action == key)
            && !(key == b'[' && !self.clear_defaults)
        {
            return None;
        }
        if self.clear_defaults {
            return self
                .keys
                .iter()
                .enumerate()
                .find(|(index, configured)| self.legacy_configured[*index] && **configured == key)
                .map(|(index, _)| DEFAULT_SHORTCUT_KEYS[index]);
        }
        if let Some(index) = self.keys.iter().position(|&configured| configured == key) {
            Some(DEFAULT_SHORTCUT_KEYS[index])
        } else if DEFAULT_SHORTCUT_KEYS.contains(&key) {
            None
        } else {
            Some(key)
        }
    }

    pub fn action_is_active(self, action: u8) -> bool {
        self.resolve(self.key_for(action)) == Some(action)
    }

    pub fn exits_normal(self, key: u8) -> bool {
        self.normal_exit[..self.normal_exit_len].contains(&key)
    }

    pub fn enters_history_locked(self, key: u8) -> bool {
        self.locked_history.contains(&Some(key))
    }

    pub fn history_binding(self, key: HistoryKey) -> Option<HistoryAction> {
        self.history_bindings[..self.history_binding_len]
            .iter()
            .flatten()
            .find_map(|(configured, action)| (*configured == key).then_some(*action))
    }

    pub(crate) fn history_bindings(self) -> impl Iterator<Item = (HistoryKey, HistoryAction)> {
        self.history_bindings
            .into_iter()
            .take(self.history_binding_len)
            .flatten()
    }

    pub fn enters_pane(self, key: u8) -> bool {
        self.pane_enter == Some(key)
    }

    pub fn enters_resize(self, key: u8) -> bool {
        self.resize_enter == Some(key)
    }

    pub fn enters_move(self, key: u8) -> bool {
        self.move_enter == Some(key)
    }

    pub fn enters_tab(self, key: u8) -> bool {
        self.tab_enter == Some(key)
    }

    pub fn tab_entry_key(self) -> Option<u8> {
        self.tab_enter
    }

    pub fn enters_session(self, key: u8) -> bool {
        self.session_enter == Some(key)
    }

    pub fn session_entry_key(self) -> Option<u8> {
        self.session_enter
    }

    pub fn pane_binding(self, key: u8) -> Option<PaneBinding> {
        self.pane_bindings[..self.pane_binding_len]
            .iter()
            .flatten()
            .find(|binding| binding.key == key)
            .copied()
    }

    pub fn pane_arrow_binding(self, direction: Direction) -> Option<PaneArrowBinding> {
        self.pane_arrows[arrow_index(direction)]
    }

    pub fn pane_key(self, action: PaneAction) -> Option<u8> {
        self.pane_bindings[..self.pane_binding_len]
            .iter()
            .flatten()
            .filter(|binding| binding.action == action)
            .max_by_key(|binding| binding.preferred)
            .map(|binding| binding.key)
    }

    pub fn resize_binding(self, key: u8) -> Option<ResizeBinding> {
        self.resize_bindings[..self.resize_binding_len]
            .iter()
            .flatten()
            .find(|binding| binding.key == key)
            .copied()
    }

    pub fn resize_arrow_action(self, direction: Direction) -> Option<ResizeAction> {
        self.resize_arrows[arrow_index(direction)]
    }

    pub fn resize_key(self, action: ResizeAction) -> Option<u8> {
        self.resize_bindings[..self.resize_binding_len]
            .iter()
            .flatten()
            .filter(|binding| binding.action == action)
            .max_by_key(|binding| binding.preferred)
            .map(|binding| binding.key)
    }

    pub fn move_binding(self, key: u8) -> Option<MoveBinding> {
        self.move_bindings[..self.move_binding_len]
            .iter()
            .flatten()
            .find(|binding| binding.key == key)
            .copied()
    }

    pub fn move_arrow_action(self, direction: Direction) -> Option<MoveAction> {
        self.move_arrows[arrow_index(direction)]
    }

    pub fn move_key(self, action: MoveAction) -> Option<u8> {
        self.move_bindings[..self.move_binding_len]
            .iter()
            .flatten()
            .filter(|binding| binding.action == action)
            .max_by_key(|binding| binding.preferred)
            .map(|binding| binding.key)
    }

    pub fn tab_binding(self, key: u8) -> Option<TabBinding> {
        self.tab_bindings[..self.tab_binding_len]
            .iter()
            .flatten()
            .find(|binding| binding.key == key)
            .copied()
    }

    pub fn tab_arrow_binding(self, direction: Direction) -> Option<TabBinding> {
        self.tab_arrows[arrow_index(direction)]
    }

    pub fn tab_key(self, action: TabAction) -> Option<u8> {
        self.tab_bindings[..self.tab_binding_len]
            .iter()
            .flatten()
            .filter(|binding| binding.action == action)
            .max_by_key(|binding| binding.preferred)
            .map(|binding| binding.key)
    }

    pub fn session_binding(self, key: u8) -> Option<SessionBinding> {
        self.session_bindings[..self.session_binding_len]
            .iter()
            .flatten()
            .find(|binding| binding.key == key)
            .copied()
    }

    pub fn session_key(self, action: SessionAction) -> Option<u8> {
        self.session_bindings[..self.session_binding_len]
            .iter()
            .flatten()
            .filter(|binding| binding.action == action)
            .max_by_key(|binding| binding.preferred)
            .map(|binding| binding.key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notifications {
    pub enabled: bool,
    pub long_command_bell: bool,
    pub desktop: bool,
    pub command_duration: Duration,
    pub exclude_applications: std::sync::Arc<[String]>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            long_command_bell: true,
            desktop: false,
            command_duration: Duration::from_secs(DEFAULT_COMMAND_DURATION_SECONDS),
            exclude_applications: ["yazi", "nvim", "lazygit"].map(str::to_owned).into(),
        }
    }
}

impl Notifications {
    pub fn command_bell_after(&self) -> Option<Duration> {
        (self.enabled && self.long_command_bell).then_some(self.command_duration)
    }

    pub(crate) fn command_reminder_after(&self) -> Option<Duration> {
        (self.enabled && (self.long_command_bell || self.desktop)).then_some(self.command_duration)
    }

    pub fn excludes_application(&self, application: &str) -> bool {
        let name = Path::new(application)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(application);
        self.exclude_applications
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(name))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    source: Option<reload::Source>,
    manager: manager::Bindings,
    theme: crate::theme::Theme,
    shell: OsString,
    notifications: Notifications,
    scrollback_lines: usize,
    shortcuts: Shortcuts,
    persistence: PersistenceOptions,
    remain_on_exit: bool,
    mouse_hover_cursor: bool,
    clipboard_write: bool,
}

/// Disk saving is opt-in; explicit manual saves remain available with defaults.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PersistenceOptions {
    pub autosave_interval_seconds: u64,
    pub save_scrollback: bool,
    pub save_scrollback_colors: bool,
}

impl Config {
    pub fn clipboard_write(&self) -> bool {
        self.clipboard_write
    }
    pub fn mouse_hover_cursor(&self) -> bool {
        self.mouse_hover_cursor
    }
    pub(crate) fn theme(&self) -> crate::theme::Theme {
        self.theme
    }
    pub(crate) fn manager(&self) -> &manager::Bindings {
        &self.manager
    }
    pub fn remain_on_exit(&self) -> bool {
        self.remain_on_exit
    }
    pub fn persistence(&self) -> PersistenceOptions {
        self.persistence
    }
    pub fn shell(&self) -> &OsString {
        &self.shell
    }

    pub fn notifications(&self) -> Notifications {
        self.notifications.clone()
    }

    pub fn scrollback_lines(&self) -> usize {
        self.scrollback_lines
    }

    pub fn shortcuts(&self) -> Shortcuts {
        self.shortcuts
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ParsedConfig {
    manager: manager::Bindings,
    theme: crate::theme::Theme,
    shell: Option<String>,
    notifications: Notifications,
    scrollback_lines: Option<usize>,
    shortcuts: Shortcuts,
    persistence: PersistenceOptions,
    remain_on_exit: bool,
    mouse_hover_cursor: bool,
    clipboard_write: bool,
}

/// Load and validate the complete configuration used by a new session.
pub fn load() -> Result<Config, String> {
    load_with_path(None)
}

/// Load an explicit configuration file, or use the default discovery path.
/// An explicitly selected file must exist; a missing default file is optional.
pub fn load_with_path(path: Option<&Path>) -> Result<Config, String> {
    let configured = match path {
        Some(path) => load_config(path, false)?,
        None => load_config(&config_path(), true)?,
    };
    let selected = path.map(Path::to_owned).unwrap_or_else(config_path);
    let selected = if selected.is_absolute() {
        selected
    } else {
        env::current_dir()
            .map_err(|error| error.to_string())?
            .join(selected)
    };
    let mut config = resolve_config(configured);
    config.source = Some(reload::Source {
        path: selected,
        allow_missing: path.is_none(),
    });
    Ok(config)
}

fn resolve_config(configured: ParsedConfig) -> Config {
    Config {
        source: None,
        manager: configured.manager,
        theme: configured.theme,
        shell: select_shell(
            env::var_os("RUSTMUX_SHELL"),
            configured.shell.map(OsString::from),
            env::var_os("SHELL"),
        ),
        notifications: configured.notifications,
        scrollback_lines: configured
            .scrollback_lines
            .unwrap_or(DEFAULT_SCROLLBACK_LINES),
        shortcuts: configured.shortcuts,
        persistence: configured.persistence,
        remain_on_exit: configured.remain_on_exit,
        mouse_hover_cursor: configured.mouse_hover_cursor,
        clipboard_write: configured.clipboard_write,
    }
}

/// Select the shell used for initial, split and newly created panes.
///
/// `RUSTMUX_SHELL` remains an explicit per-process override. Without it, the
/// `shell` value in `config.toml` takes precedence over the login shell.
pub fn shell() -> Result<OsString, String> {
    load().map(|config| config.shell)
}

fn select_shell(
    override_shell: Option<OsString>,
    configured: Option<OsString>,
    login_shell: Option<OsString>,
) -> OsString {
    override_shell
        .filter(|shell| !shell.is_empty())
        .or(configured)
        .or_else(|| login_shell.filter(|shell| !shell.is_empty()))
        .unwrap_or_else(|| OsString::from("/bin/sh"))
}

fn load_config(path: &Path, allow_missing: bool) -> Result<ParsedConfig, String> {
    let source = read_config(path, allow_missing)?;
    match source {
        Some(source) => {
            parse_config(&source).map_err(|error| format!("invalid {}: {error}", path.display()))
        }
        None => Ok(ParsedConfig::default()),
    }
}

fn read_config(path: &Path, allow_missing: bool) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(source) => Ok(Some(source)),
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn parse_config(source: &str) -> Result<ParsedConfig, String> {
    let document = source
        .parse::<toml::Table>()
        .map_err(|error| error.to_string())?;
    let shell = match document.get("shell") {
        None => None,
        Some(value) => {
            let shell = value
                .as_str()
                .ok_or_else(|| "shell must be a string".to_owned())?;
            if shell.trim().is_empty() || shell.contains('\0') {
                return Err("shell must be a nonempty executable name or path".to_owned());
            }
            Some(shell.to_owned())
        }
    };
    let notifications = parse_notifications(document.get("notifications"))?;
    let boolean = |key: &str| -> Result<bool, String> {
        document
            .get(key)
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| format!("{key} must be a boolean"))
            })
            .transpose()
            .map(|value| value.unwrap_or(false))
    };
    let persistence = PersistenceOptions {
        autosave_interval_seconds: document
            .get("autosave_interval_seconds")
            .map(|value| {
                value
                    .as_integer()
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or_else(|| {
                        "autosave_interval_seconds must be a nonnegative integer".to_owned()
                    })
            })
            .transpose()?
            .unwrap_or(0),
        save_scrollback: boolean("save_scrollback")?,
        save_scrollback_colors: boolean("save_scrollback_colors")?,
    };
    let clear_defaults = document
        .get("clear_defaults")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| "clear_defaults must be a boolean".to_owned())
        })
        .transpose()?
        .unwrap_or(false);
    let shortcuts = parse_keybinds(
        document.get("keybinds"),
        parse_shortcuts(document.get("shortcuts"), clear_defaults)?,
    )?;
    if clear_defaults && !shortcuts.locked_configured {
        return Err(
            "clear_defaults requires a supported keybinds.locked switch-mode normal binding"
                .to_owned(),
        );
    }
    let scrollback_lines = document
        .get("scrollback_lines")
        .map(|value| {
            value
                .as_integer()
                .and_then(|lines| usize::try_from(lines).ok())
                .ok_or_else(|| "scrollback_lines must be a nonnegative integer".to_owned())
        })
        .transpose()?;
    Ok(ParsedConfig {
        manager: manager::Bindings::parse(document.get("session_manager"))?,
        theme: document
            .get("theme")
            .map(|value| {
                value
                    .clone()
                    .try_into::<crate::theme::ThemeConfig>()
                    .map_err(|error| format!("theme: {error}"))?
                    .resolve()
            })
            .transpose()?
            .unwrap_or_default(),
        shell,
        notifications,
        scrollback_lines,
        shortcuts,
        persistence,
        remain_on_exit: boolean("remain_on_exit")?,
        mouse_hover_cursor: boolean("mouse_hover_cursor")?,
        clipboard_write: boolean("clipboard_write")?,
    })
}

fn parse_shortcuts(value: Option<&toml::Value>, clear_defaults: bool) -> Result<Shortcuts, String> {
    let mut shortcuts = Shortcuts {
        clear_defaults,
        ..Shortcuts::default()
    };
    let Some(value) = value else {
        return Ok(shortcuts);
    };
    let table = value.as_table().ok_or("shortcuts must be a table")?;
    for (name, value) in table {
        let index = SHORTCUT_NAMES
            .iter()
            .position(|known| known == name)
            .ok_or_else(|| format!("unknown shortcuts action: {name}"))?;
        let key = value
            .as_str()
            .filter(|text| text.len() == 1 && text.as_bytes()[0].is_ascii_graphic())
            .map(|text| text.as_bytes()[0])
            .ok_or_else(|| format!("shortcuts.{name} must be one printable ASCII key"))?;
        shortcuts.keys[index] = key;
        shortcuts.legacy_configured[index] = true;
    }
    validate_shortcuts(shortcuts)?;
    Ok(shortcuts)
}

fn validate_shortcuts(shortcuts: Shortcuts) -> Result<(), String> {
    for (index, key) in shortcuts.keys.iter().enumerate() {
        if shortcuts.clear_defaults && !shortcuts.legacy_configured[index] {
            continue;
        }
        if shortcuts.keys[..index]
            .iter()
            .enumerate()
            .any(|(prior, previous)| {
                *previous == *key
                    && (!shortcuts.clear_defaults || shortcuts.legacy_configured[prior])
            })
            || (!shortcuts.clear_defaults && FIXED_SHORTCUT_KEYS.contains(key))
        {
            return Err(format!(
                "shortcuts.{} conflicts with another shortcut",
                SHORTCUT_NAMES[index]
            ));
        }
    }
    Ok(())
}

fn parse_keybinds(
    value: Option<&toml::Value>,
    mut shortcuts: Shortcuts,
) -> Result<Shortcuts, String> {
    let Some(value) = value else {
        return Ok(shortcuts);
    };
    let keybinds = value.as_table().ok_or("keybinds must be a table")?;
    if let Some(locked) = keybinds.get("locked") {
        let locked = locked.as_table().ok_or("keybinds.locked must be a table")?;
        let mut configured = false;
        for (key, binding) in locked {
            let enters_normal = binding
                .as_table()
                .and_then(|table| table.get("actions"))
                .and_then(toml::Value::as_array)
                .is_some_and(|actions| {
                    actions.len() == 1
                        && actions.iter().any(|action| {
                            action.as_table().is_some_and(|table| {
                                table.get("action").and_then(toml::Value::as_str)
                                    == Some("switch-mode")
                                    && table.get("mode").and_then(toml::Value::as_str)
                                        == Some("normal")
                            })
                        })
                });
            if history_switch(binding) {
                let byte = parse_mode_key(key)
                    .filter(|byte| (1..=26).contains(byte))
                    .ok_or_else(|| {
                        format!("keybinds.locked.{key} must be Ctrl A through Ctrl Z")
                    })?;
                let slot = shortcuts
                    .locked_history
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or("too many keybinds.locked history-mode entry keys")?;
                *slot = Some(byte);
            } else if enters_normal {
                let byte = parse_mode_key(key)
                    .filter(|byte| (1..=26).contains(byte))
                    .ok_or_else(|| {
                        format!("keybinds.locked.{key} must be Ctrl A through Ctrl Z")
                    })?;
                if configured {
                    return Err(
                        "multiple keybinds.locked normal-mode entry keys are not supported yet"
                            .to_owned(),
                    );
                }
                shortcuts.locked_enter = byte;
                shortcuts.locked_configured = true;
                configured = true;
            }
        }
    }
    if !shortcuts.clear_defaults
        && shortcuts.locked_enter != 2
        && matches!(shortcuts.locked_enter, 8 | 9 | 10 | 11 | 12 | 23)
    {
        return Err("keybinds.locked entry key conflicts with a NORMAL-mode shortcut".to_owned());
    }
    let normal = keybinds
        .get("normal")
        .map(|value| value.as_table().ok_or("keybinds.normal must be a table"))
        .transpose()?;
    let mut configured_actions = [false; 3];
    if let Some(normal) = normal {
        for (key, binding) in normal {
            let Some(actions) = binding
                .as_table()
                .and_then(|table| table.get("actions"))
                .and_then(toml::Value::as_array)
            else {
                continue;
            };
            let names: Vec<_> = actions
                .iter()
                .filter_map(|value| {
                    value.as_str().or_else(|| {
                        value
                            .as_table()
                            .and_then(|table| table.get("action"))
                            .and_then(toml::Value::as_str)
                    })
                })
                .collect();
            if history_switch(binding) {
                let byte = parse_mode_key(key).ok_or_else(|| {
                    format!("keybinds.normal.{key} must be a supported history-mode key")
                })?;
                if shortcuts.normal_action_len == shortcuts.normal_actions.len() {
                    return Err("too many supported keybinds.normal actions".to_owned());
                }
                shortcuts.normal_actions[shortcuts.normal_action_len] = Some((byte, b'['));
                shortcuts.normal_action_len += 1;
            } else if let Some(index) = names
                .iter()
                .position(|name| matches!(*name, "new-window" | "new-pane-right" | "new-pane-down"))
            {
                let action = names[index];
                let slot = match action {
                    "new-window" => 0,
                    "new-pane-right" => 1,
                    _ => 2,
                };
                let exits_locked = actions.iter().any(|value| {
                    value.as_table().is_some_and(|table| {
                        table.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
                            && table.get("mode").and_then(toml::Value::as_str) == Some("locked")
                    })
                });
                let complete = exits_locked
                    && actions
                        .iter()
                        .filter(|value| value.as_str() == Some(action))
                        .count()
                        == 1
                    && actions.iter().all(|value| {
                        value.as_str() == Some(action)
                            || value.as_table().is_some_and(|table| {
                                table.get("action").and_then(toml::Value::as_str)
                                    == Some("switch-mode")
                                    && table.get("mode").and_then(toml::Value::as_str)
                                        == Some("locked")
                            })
                    });
                if !complete {
                    continue;
                }
                if configured_actions[slot] {
                    return Err(format!(
                        "multiple keybinds.normal keys for {action} are not supported yet"
                    ));
                }
                configured_actions[slot] = true;
                let byte = parse_printable_key(key).ok_or_else(|| {
                    format!("keybinds.normal.{key} must be one printable ASCII key for {action}")
                })?;
                shortcuts.keys[slot] = byte;
                shortcuts.legacy_configured[slot] = true;
            } else if let Some((action, canonical)) = names.iter().find_map(|name| {
                let canonical = match *name {
                    "respawn-pane" => b'R',
                    "close-window" => b'&',
                    "rename-window" => b',',
                    "next-window" => b'n',
                    "previous-window" => b'p',
                    "move-window-left" => b'<',
                    "move-window-right" => b'>',
                    "show-help" => b'?',
                    "switch-session" => 23,
                    "focus-left" => b'h',
                    "focus-down" => b'j',
                    "focus-up" => b'k',
                    "focus-right" => b'l',
                    _ => return None,
                };
                Some((*name, canonical))
            }) {
                let ends_locked = actions.len() == 2
                    && actions[1].as_table().is_some_and(|table| {
                        table.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
                            && table.get("mode").and_then(toml::Value::as_str) == Some("locked")
                    });
                let complete = actions[0].as_str() == Some(action)
                    && (ends_locked || (action == "rename-window" && actions.len() == 1));
                if !complete {
                    continue;
                }
                let byte = if action == "switch-session" {
                    parse_mode_key(key)
                } else {
                    parse_normal_action_key(key)
                }
                .ok_or_else(|| {
                    format!("keybinds.normal.{key} must be a supported key for {action}")
                })?;
                if shortcuts.normal_action_len == shortcuts.normal_actions.len() {
                    return Err("too many supported keybinds.normal actions".to_owned());
                }
                shortcuts.normal_actions[shortcuts.normal_action_len] = Some((byte, canonical));
                shortcuts.normal_action_len += 1;
            } else if actions.len() == 2
                && actions[0]
                    .as_table()
                    .and_then(|table| table.get("action"))
                    .and_then(toml::Value::as_str)
                    == Some("go-to-window")
                && actions[1].as_table().is_some_and(|table| {
                    table.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
                        && table.get("mode").and_then(toml::Value::as_str) == Some("locked")
                })
                && let Some(index) = actions[0]
                    .as_table()
                    .and_then(|table| table.get("index"))
                    .and_then(toml::Value::as_integer)
                    .filter(|index| (1..=10).contains(index))
            {
                let byte = parse_normal_action_key(key).ok_or_else(|| {
                format!("keybinds.normal.{key} must be one printable ASCII key or tab for go-to-window")
            })?;
                let canonical = if index == 10 {
                    b'0'
                } else {
                    b'0' + index as u8
                };
                if shortcuts.normal_action_len == shortcuts.normal_actions.len() {
                    return Err("too many supported keybinds.normal actions".to_owned());
                }
                shortcuts.normal_actions[shortcuts.normal_action_len] = Some((byte, canonical));
                shortcuts.normal_action_len += 1;
            } else if actions.len() == 1
                && names.len() == 1
                && names[0] == "switch-mode"
                && actions[0]
                    .as_table()
                    .and_then(|table| table.get("mode"))
                    .and_then(toml::Value::as_str)
                    == Some("pane")
                && let Some(byte) = parse_mode_key(key)
            {
                if shortcuts.pane_enter.replace(byte).is_some() {
                    return Err(
                        "multiple keybinds.normal pane-mode entry keys are not supported yet"
                            .to_owned(),
                    );
                }
            } else if actions.len() == 1
                && names.len() == 1
                && names[0] == "switch-mode"
                && actions[0]
                    .as_table()
                    .and_then(|table| table.get("mode"))
                    .and_then(toml::Value::as_str)
                    == Some("resize")
                && let Some(byte) = parse_mode_key(key)
            {
                if shortcuts.resize_enter.replace(byte).is_some() {
                    return Err(
                        "multiple keybinds.normal resize-mode entry keys are not supported yet"
                            .to_owned(),
                    );
                }
            } else if actions.len() == 1
                && names.len() == 1
                && names[0] == "switch-mode"
                && actions[0]
                    .as_table()
                    .and_then(|table| table.get("mode"))
                    .and_then(toml::Value::as_str)
                    == Some("move")
                && let Some(byte) = parse_mode_key(key)
            {
                if shortcuts.move_enter.replace(byte).is_some() {
                    return Err(
                        "multiple keybinds.normal move-mode entry keys are not supported yet"
                            .to_owned(),
                    );
                }
            } else if actions.len() == 1
                && names.len() == 1
                && names[0] == "switch-mode"
                && actions[0]
                    .as_table()
                    .and_then(|table| table.get("mode"))
                    .and_then(toml::Value::as_str)
                    == Some("tab")
                && let Some(byte) = parse_mode_key(key)
            {
                if shortcuts.tab_enter.replace(byte).is_some() {
                    return Err(
                        "multiple keybinds.normal tab-mode entry keys are not supported yet"
                            .to_owned(),
                    );
                }
            } else if actions.len() == 1
                && names.len() == 1
                && names[0] == "switch-mode"
                && actions[0]
                    .as_table()
                    .and_then(|table| table.get("mode"))
                    .and_then(toml::Value::as_str)
                    == Some("session")
                && let Some(byte) = parse_mode_key(key)
            {
                if shortcuts.session_enter.replace(byte).is_some() {
                    return Err(
                        "multiple keybinds.normal session-mode entry keys are not supported yet"
                            .to_owned(),
                    );
                }
            } else if actions.len() == 1
                && names.len() == 1
                && names[0] == "switch-mode"
                && actions[0]
                    .as_table()
                    .and_then(|table| table.get("mode"))
                    .and_then(toml::Value::as_str)
                    == Some("locked")
                && let Some(byte) = parse_mode_key(key)
            {
                if shortcuts.exits_normal(byte) {
                    return Err(format!("duplicate keybinds.normal locked-mode exit: {key}"));
                }
                if shortcuts.normal_exit_len == shortcuts.normal_exit.len() {
                    return Err("too many keybinds.normal locked-mode exits".to_owned());
                }
                shortcuts.normal_exit[shortcuts.normal_exit_len] = byte;
                shortcuts.normal_exit_len += 1;
            }
        }
    }
    validate_shortcuts(shortcuts)?;
    parse_pane_bindings(keybinds.get("pane"), &mut shortcuts)?;
    parse_resize_bindings(keybinds.get("resize"), &mut shortcuts)?;
    parse_move_bindings(keybinds.get("move"), &mut shortcuts)?;
    parse_tab_bindings(keybinds.get("tab"), &mut shortcuts)?;
    parse_session_bindings(keybinds.get("session"), &mut shortcuts)?;
    parse_history_bindings(keybinds.get("history"), &mut shortcuts)?;
    let locked_enter = shortcuts.locked_entry_key();
    if shortcuts.enters_history_locked(locked_enter) {
        return Err(
            "keybinds.locked history-mode entry conflicts with the normal-mode entry".into(),
        );
    }
    if shortcuts.exits_normal(locked_enter)
        || [
            shortcuts.pane_enter,
            shortcuts.resize_enter,
            shortcuts.move_enter,
            shortcuts.tab_enter,
            shortcuts.session_enter,
        ]
        .contains(&Some(locked_enter))
    {
        return Err("keybinds.locked entry key conflicts with a NORMAL-mode shortcut".to_owned());
    }
    for (key, action) in shortcuts.normal_actions[..shortcuts.normal_action_len]
        .iter()
        .flatten()
    {
        if shortcuts
            .keys
            .iter()
            .enumerate()
            .any(|(index, configured)| {
                configured == key
                    && (!shortcuts.clear_defaults || shortcuts.legacy_configured[index])
            })
            || (*action == b'['
                && [
                    shortcuts.pane_enter,
                    shortcuts.resize_enter,
                    shortcuts.move_enter,
                    shortcuts.tab_enter,
                    shortcuts.session_enter,
                ]
                .contains(&Some(*key)))
            || shortcuts.exits_normal(*key)
            || *key == locked_enter
        {
            return Err(
                "keybinds.normal action key conflicts with another configured shortcut".to_owned(),
            );
        }
    }
    for key in &shortcuts.normal_exit[..shortcuts.normal_exit_len] {
        if shortcuts
            .keys
            .iter()
            .enumerate()
            .any(|(index, configured)| {
                configured == key
                    && (!shortcuts.clear_defaults || shortcuts.legacy_configured[index])
            })
            || (!shortcuts.clear_defaults && FIXED_SHORTCUT_KEYS.contains(key))
            || shortcuts.normal_actions[..shortcuts.normal_action_len]
                .iter()
                .flatten()
                .any(|(configured, _)| configured == key)
            || *key == shortcuts.locked_enter
            || (!shortcuts.clear_defaults && *key == 23)
        {
            return Err("keybinds.normal locked-mode exit conflicts with a command".to_owned());
        }
    }
    Ok(shortcuts)
}

fn history_switch(binding: &toml::Value) -> bool {
    binding
        .get("actions")
        .and_then(toml::Value::as_array)
        .is_some_and(|actions| {
            matches!(actions.as_slice(), [action]
            if action.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
            && action.get("mode").and_then(toml::Value::as_str) == Some("history"))
        })
}

fn parse_history_bindings(
    value: Option<&toml::Value>,
    shortcuts: &mut Shortcuts,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let table = value.as_table().ok_or("keybinds.history must be a table")?;
    for (name, binding) in table {
        let key = match name.as_str() {
            "up" => HistoryKey::Up,
            "down" => HistoryKey::Down,
            "left" => HistoryKey::Left,
            "right" => HistoryKey::Right,
            "pageup" => HistoryKey::PageUp,
            "pagedown" => HistoryKey::PageDown,
            "home" => HistoryKey::Home,
            "end" => HistoryKey::End,
            _ => HistoryKey::Byte(
                parse_mode_key(name)
                    .ok_or_else(|| format!("unsupported keybinds.history key: {name}"))?,
            ),
        };
        let actions = binding
            .get("actions")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| format!("keybinds.history.{name} requires actions"))?;
        let [single] = actions.as_slice() else {
            return Err(format!(
                "keybinds.history.{name} requires exactly one action"
            ));
        };
        let action = if single.get("action").and_then(toml::Value::as_str) == Some("switch-mode") {
            let mode = match single.get("mode").and_then(toml::Value::as_str) {
                Some("locked") => HistoryMode::Locked,
                Some("normal") => HistoryMode::Normal,
                Some("pane") => HistoryMode::Pane,
                Some("resize") => HistoryMode::Resize,
                Some("move") => HistoryMode::Move,
                Some("tab") => HistoryMode::Tab,
                Some("session") => HistoryMode::Session,
                _ => {
                    return Err(format!(
                        "unsupported keybinds.history.{name} switch-mode target"
                    ));
                }
            };
            HistoryAction::SwitchMode(mode)
        } else {
            let key = match single.as_str() {
                Some("scroll-up") => HistoryKey::Byte(b'k'),
                Some("scroll-down") => HistoryKey::Byte(b'j'),
                Some("scroll-page-up") => HistoryKey::PageUp,
                Some("scroll-page-down") => HistoryKey::PageDown,
                Some("scroll-half-page-up") => HistoryKey::Byte(21),
                Some("scroll-half-page-down") => HistoryKey::Byte(4),
                Some("scroll-top") => HistoryKey::Byte(b'g'),
                Some("scroll-bottom") => HistoryKey::Byte(b'G'),
                Some("history-search-forward") => HistoryKey::Byte(b'/'),
                Some("history-search-backward") => HistoryKey::Byte(b'?'),
                Some("history-next-match") => HistoryKey::Byte(b'n'),
                Some("history-previous-match") => HistoryKey::Byte(b'N'),
                Some("copy-history") => HistoryKey::Byte(b'y'),
                Some("toggle-history-selection") => HistoryKey::Byte(b'v'),
                Some("history-selection-left") => HistoryKey::Byte(b'h'),
                Some("history-selection-right") => HistoryKey::Byte(b'l'),
                Some("history-selection-previous-word") => HistoryKey::Byte(b'b'),
                Some("history-selection-next-word") => HistoryKey::Byte(b'e'),
                Some("history-selection-line-start") => HistoryKey::Byte(b'0'),
                Some("history-selection-line-end") => HistoryKey::Byte(b'$'),
                Some("history-selection-swap") => HistoryKey::Byte(b'o'),
                _ => return Err(format!("unsupported keybinds.history.{name} action")),
            };
            HistoryAction::Key(key)
        };
        if shortcuts.history_binding(key).is_some() {
            return Err(format!("duplicate keybinds.history key: {name}"));
        }
        if shortcuts.history_binding_len == shortcuts.history_bindings.len() {
            return Err("too many keybinds.history bindings".into());
        }
        shortcuts.history_bindings[shortcuts.history_binding_len] = Some((key, action));
        shortcuts.history_binding_len += 1;
    }
    Ok(())
}

fn parse_pane_bindings(
    value: Option<&toml::Value>,
    shortcuts: &mut Shortcuts,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let pane = value.as_table().ok_or("keybinds.pane must be a table")?;
    for (key, binding) in pane {
        let Some(table) = binding.as_table() else {
            continue;
        };
        let Some(actions) = table.get("actions").and_then(toml::Value::as_array) else {
            continue;
        };
        let parsed = match actions.as_slice() {
            [single] => match single.as_str() {
                Some("focus-left") => Some((PaneAction::FocusLeft, true)),
                Some("focus-down") => Some((PaneAction::FocusDown, true)),
                Some("focus-up") => Some((PaneAction::FocusUp, true)),
                Some("focus-right") => Some((PaneAction::FocusRight, true)),
                Some("focus-next-pane") => Some((PaneAction::Next, true)),
                _ => single.as_table().and_then(|value| {
                    (value.get("action").and_then(toml::Value::as_str) == Some("switch-mode"))
                        .then(|| match value.get("mode").and_then(toml::Value::as_str) {
                            Some("normal") => Some((PaneAction::Normal, false)),
                            Some("history") => Some((PaneAction::History, false)),
                            Some("resize") => Some((PaneAction::Resize, false)),
                            Some("move") => Some((PaneAction::Move, false)),
                            Some("tab") => Some((PaneAction::Tab, false)),
                            Some("session") => Some((PaneAction::Session, false)),
                            Some("locked") => Some((PaneAction::Locked, false)),
                            _ => None,
                        })
                        .flatten()
                }),
            },
            [first, second]
                if second.as_table().is_some_and(|table| {
                    table.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
                        && table.get("mode").and_then(toml::Value::as_str) == Some("locked")
                }) =>
            {
                match first.as_str() {
                    Some("break-pane") => Some((PaneAction::Break, false)),
                    Some("move-pane-previous-window") => {
                        Some((PaneAction::MovePreviousWindow, false))
                    }
                    Some("move-pane-next-window") => Some((PaneAction::MoveNextWindow, false)),
                    Some("new-pane-right") => Some((PaneAction::SplitRight, false)),
                    Some("new-pane-down") => Some((PaneAction::SplitDown, false)),
                    Some("toggle-pane-zoom") => Some((PaneAction::Zoom, false)),
                    Some("close-pane") => Some((PaneAction::Close, false)),
                    Some("respawn-pane") => Some((PaneAction::Respawn, false)),
                    _ => None,
                }
            }
            _ => None,
        };
        let Some((action, stay)) = parsed else {
            continue;
        };
        let preferred = table.get("display").and_then(toml::Value::as_str) == Some("always");
        if let Some(direction) = parse_arrow_key(key) {
            shortcuts.pane_arrows[arrow_index(direction)] = Some(PaneArrowBinding { action, stay });
            continue;
        }
        let Some(key) = parse_mode_key(key) else {
            continue;
        };
        if shortcuts.pane_binding_len == shortcuts.pane_bindings.len() {
            return Err("too many supported keybinds.pane bindings".to_owned());
        }
        shortcuts.pane_bindings[shortcuts.pane_binding_len] = Some(PaneBinding {
            key,
            action,
            stay,
            preferred,
        });
        shortcuts.pane_binding_len += 1;
    }
    Ok(())
}

fn parse_resize_bindings(
    value: Option<&toml::Value>,
    shortcuts: &mut Shortcuts,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let resize = value.as_table().ok_or("keybinds.resize must be a table")?;
    for (key, binding) in resize {
        let Some(table) = binding.as_table() else {
            continue;
        };
        let Some(actions) = table.get("actions").and_then(toml::Value::as_array) else {
            continue;
        };
        let [single] = actions.as_slice() else {
            continue;
        };
        let action = match single.as_str() {
            Some("resize-pane-left") => Some(ResizeAction::Resize(Direction::Left)),
            Some("resize-pane-down") => Some(ResizeAction::Resize(Direction::Down)),
            Some("resize-pane-up") => Some(ResizeAction::Resize(Direction::Up)),
            Some("resize-pane-right") => Some(ResizeAction::Resize(Direction::Right)),
            _ => single.as_table().and_then(|value| {
                (value.get("action").and_then(toml::Value::as_str) == Some("switch-mode"))
                    .then(|| match value.get("mode").and_then(toml::Value::as_str) {
                        Some("normal") => Some(ResizeAction::Normal),
                        Some("history") => Some(ResizeAction::History),
                        Some("pane") => Some(ResizeAction::Pane),
                        Some("move") => Some(ResizeAction::Move),
                        Some("tab") => Some(ResizeAction::Tab),
                        Some("session") => Some(ResizeAction::Session),
                        Some("locked") => Some(ResizeAction::Locked),
                        _ => None,
                    })
                    .flatten()
            }),
        };
        let Some(action) = action else {
            continue;
        };
        if let Some(direction) = parse_arrow_key(key) {
            shortcuts.resize_arrows[arrow_index(direction)] = Some(action);
            continue;
        }
        let Some(key) = parse_mode_key(key) else {
            continue;
        };
        if shortcuts.resize_binding_len == shortcuts.resize_bindings.len() {
            return Err("too many supported keybinds.resize bindings".to_owned());
        }
        shortcuts.resize_bindings[shortcuts.resize_binding_len] = Some(ResizeBinding {
            key,
            action,
            preferred: table.get("display").and_then(toml::Value::as_str) == Some("always"),
        });
        shortcuts.resize_binding_len += 1;
    }
    Ok(())
}

fn parse_move_bindings(
    value: Option<&toml::Value>,
    shortcuts: &mut Shortcuts,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let move_mode = value.as_table().ok_or("keybinds.move must be a table")?;
    for (key, binding) in move_mode {
        let Some(table) = binding.as_table() else {
            continue;
        };
        let Some(actions) = table.get("actions").and_then(toml::Value::as_array) else {
            continue;
        };
        let [single] = actions.as_slice() else {
            continue;
        };
        let action = match single.as_str() {
            Some("move-pane-left") => Some(MoveAction::Move(Direction::Left)),
            Some("move-pane-down") => Some(MoveAction::Move(Direction::Down)),
            Some("move-pane-up") => Some(MoveAction::Move(Direction::Up)),
            Some("move-pane-right") => Some(MoveAction::Move(Direction::Right)),
            _ => single.as_table().and_then(|value| {
                (value.get("action").and_then(toml::Value::as_str) == Some("switch-mode"))
                    .then(|| match value.get("mode").and_then(toml::Value::as_str) {
                        Some("normal") => Some(MoveAction::Normal),
                        Some("history") => Some(MoveAction::History),
                        Some("pane") => Some(MoveAction::Pane),
                        Some("resize") => Some(MoveAction::Resize),
                        Some("tab") => Some(MoveAction::Tab),
                        Some("session") => Some(MoveAction::Session),
                        Some("locked") => Some(MoveAction::Locked),
                        _ => None,
                    })
                    .flatten()
            }),
        };
        let Some(action) = action else {
            continue;
        };
        if let Some(direction) = parse_arrow_key(key) {
            shortcuts.move_arrows[arrow_index(direction)] = Some(action);
            continue;
        }
        let Some(key) = parse_mode_key(key) else {
            continue;
        };
        if shortcuts.move_binding_len == shortcuts.move_bindings.len() {
            return Err("too many supported keybinds.move bindings".to_owned());
        }
        shortcuts.move_bindings[shortcuts.move_binding_len] = Some(MoveBinding {
            key,
            action,
            preferred: table.get("display").and_then(toml::Value::as_str) == Some("always"),
        });
        shortcuts.move_binding_len += 1;
    }
    Ok(())
}

fn parse_tab_bindings(
    value: Option<&toml::Value>,
    shortcuts: &mut Shortcuts,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let tab = value.as_table().ok_or("keybinds.tab must be a table")?;
    for (key, binding) in tab {
        let Some(table) = binding.as_table() else {
            continue;
        };
        let Some(actions) = table.get("actions").and_then(toml::Value::as_array) else {
            continue;
        };
        let (first, stay) = match actions.as_slice() {
            [first] => (first, true),
            [first, second]
                if second.as_table().is_some_and(|table| {
                    table.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
                        && table.get("mode").and_then(toml::Value::as_str) == Some("locked")
                }) =>
            {
                (first, false)
            }
            _ => continue,
        };
        let action = match first.as_str() {
            Some("next-window") => Some(TabAction::Next),
            Some("previous-window") => Some(TabAction::Previous),
            Some("move-window-left") => Some(TabAction::MoveLeft),
            Some("move-window-right") => Some(TabAction::MoveRight),
            Some("new-window") => Some(TabAction::New),
            Some("rename-window") => Some(TabAction::Rename),
            Some("close-window") => Some(TabAction::Close),
            Some("show-help") => Some(TabAction::Help),
            _ => first.as_table().and_then(|value| {
                match value.get("action").and_then(toml::Value::as_str) {
                    Some("go-to-window") => value
                        .get("index")
                        .and_then(toml::Value::as_integer)
                        .and_then(|index| usize::try_from(index).ok())
                        .filter(|index| (1..=16).contains(index))
                        .map(TabAction::Select),
                    Some("switch-mode") if stay => {
                        match value.get("mode").and_then(toml::Value::as_str) {
                            Some("normal") => Some(TabAction::Normal),
                            Some("history") => Some(TabAction::History),
                            Some("pane") => Some(TabAction::Pane),
                            Some("resize") => Some(TabAction::Resize),
                            Some("move") => Some(TabAction::Move),
                            Some("session") => Some(TabAction::Session),
                            Some("locked") => Some(TabAction::Locked),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }),
        };
        let Some(action) = action else {
            continue;
        };
        let binding = TabBinding {
            key: 0,
            action,
            stay,
            preferred: table.get("display").and_then(toml::Value::as_str) == Some("always"),
        };
        if let Some(direction) = parse_arrow_key(key) {
            shortcuts.tab_arrows[arrow_index(direction)] = Some(binding);
            continue;
        }
        let Some(key) = parse_mode_key(key) else {
            continue;
        };
        if shortcuts.tab_binding_len == shortcuts.tab_bindings.len() {
            return Err("too many supported keybinds.tab bindings".to_owned());
        }
        shortcuts.tab_bindings[shortcuts.tab_binding_len] = Some(TabBinding { key, ..binding });
        shortcuts.tab_binding_len += 1;
    }
    Ok(())
}

fn parse_session_bindings(
    value: Option<&toml::Value>,
    shortcuts: &mut Shortcuts,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let session = value.as_table().ok_or("keybinds.session must be a table")?;
    for (key, binding) in session {
        let Some(table) = binding.as_table() else {
            continue;
        };
        let Some(actions) = table.get("actions").and_then(toml::Value::as_array) else {
            continue;
        };
        let action = match actions.as_slice() {
            [single] => match single.as_str() {
                Some("detach") => Some(SessionAction::Detach),
                Some("show-help") => Some(SessionAction::Help),
                _ => single.as_table().and_then(|value| {
                    (value.get("action").and_then(toml::Value::as_str) == Some("switch-mode"))
                        .then(|| match value.get("mode").and_then(toml::Value::as_str) {
                            Some("normal") => Some(SessionAction::Normal),
                            Some("history") => Some(SessionAction::History),
                            Some("pane") => Some(SessionAction::Pane),
                            Some("resize") => Some(SessionAction::Resize),
                            Some("move") => Some(SessionAction::Move),
                            Some("tab") => Some(SessionAction::Tab),
                            Some("locked") => Some(SessionAction::Locked),
                            _ => None,
                        })
                        .flatten()
                }),
            },
            [first, second]
                if first.as_str() == Some("switch-session")
                    && second.as_table().is_some_and(|value| {
                        value.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
                            && value.get("mode").and_then(toml::Value::as_str) == Some("locked")
                    }) =>
            {
                Some(SessionAction::Manager)
            }
            _ => None,
        };
        let (Some(action), Some(key)) = (action, parse_mode_key(key)) else {
            continue;
        };
        if shortcuts.session_binding_len == shortcuts.session_bindings.len() {
            return Err("too many supported keybinds.session bindings".to_owned());
        }
        shortcuts.session_bindings[shortcuts.session_binding_len] = Some(SessionBinding {
            key,
            action,
            preferred: table.get("display").and_then(toml::Value::as_str) == Some("always"),
        });
        shortcuts.session_binding_len += 1;
    }
    Ok(())
}

fn parse_arrow_key(key: &str) -> Option<Direction> {
    Some(match key {
        "left" => Direction::Left,
        "down" => Direction::Down,
        "up" => Direction::Up,
        "right" => Direction::Right,
        _ => return None,
    })
}

fn arrow_index(direction: Direction) -> usize {
    match direction {
        Direction::Left => 0,
        Direction::Down => 1,
        Direction::Up => 2,
        Direction::Right => 3,
    }
}

fn parse_printable_key(key: &str) -> Option<u8> {
    (key.len() == 1 && key.as_bytes()[0].is_ascii_graphic()).then(|| key.as_bytes()[0])
}

fn parse_normal_action_key(key: &str) -> Option<u8> {
    (key == "tab")
        .then_some(b'\t')
        .or_else(|| parse_printable_key(key))
}

fn parse_mode_key(key: &str) -> Option<u8> {
    if key.eq_ignore_ascii_case("esc") {
        return Some(27);
    }
    if key.eq_ignore_ascii_case("enter") {
        return Some(13);
    }
    if key.eq_ignore_ascii_case("tab") {
        return Some(9);
    }
    if let Some(letter) = key.strip_prefix("Ctrl ") {
        let byte = parse_printable_key(letter)?.to_ascii_lowercase();
        return (byte.is_ascii_lowercase()).then_some(byte - b'a' + 1);
    }
    parse_printable_key(key)
}

fn parse_notifications(value: Option<&toml::Value>) -> Result<Notifications, String> {
    let Some(value) = value else {
        return Ok(Notifications::default());
    };
    let table = value
        .as_table()
        .ok_or_else(|| "notifications must be a table".to_owned())?;
    let mut notifications = Notifications::default();
    if let Some(value) = table.get("enabled") {
        notifications.enabled = value
            .as_bool()
            .ok_or("notifications.enabled must be a boolean")?;
    }
    if let Some(value) = table.get("desktop") {
        notifications.desktop = value
            .as_bool()
            .ok_or("notifications.desktop must be a boolean")?;
    }
    if let Some(value) = table.get("exclude_applications") {
        let values = value
            .as_array()
            .ok_or("notifications.exclude_applications must be an array of strings")?;
        if values.len() > 256 {
            return Err("notifications.exclude_applications accepts at most 256 entries".into());
        }
        let mut names = Vec::<String>::new();
        for value in values {
            let text = value
                .as_str()
                .ok_or("notifications.exclude_applications must be an array of strings")?
                .trim();
            let name = Path::new(text)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| {
                    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
                })
                .ok_or(
                    "notification application names must contain 1-256 bytes without controls",
                )?;
            if !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
                names.push(name.to_owned());
            }
        }
        notifications.exclude_applications = names.into();
    }
    if let Some(value) = table.get("long_command_bell") {
        notifications.long_command_bell = value
            .as_bool()
            .ok_or_else(|| "notifications.long_command_bell must be a boolean".to_owned())?;
    }
    if let Some(value) = table.get("command_duration_seconds") {
        let seconds = value.as_integer().ok_or_else(|| {
            "notifications.command_duration_seconds must be a positive integer".to_owned()
        })?;
        let seconds = u64::try_from(seconds)
            .ok()
            .filter(|&seconds| seconds > 0)
            .ok_or_else(|| {
                "notifications.command_duration_seconds must be a positive integer".to_owned()
            })?;
        notifications.command_duration = Duration::from_secs(seconds);
    }
    Ok(notifications)
}

/// Return the user configuration path without creating it.
pub fn config_path() -> PathBuf {
    config_path_from(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

fn config_path_from(xdg: Option<OsString>, home: Option<OsString>) -> PathBuf {
    if let Some(directory) = xdg.filter(|directory| !directory.is_empty()) {
        PathBuf::from(directory).join("rustmux/config.toml")
    } else if let Some(home) = home.filter(|home| !home.is_empty()) {
        PathBuf::from(home).join(".config/rustmux/config.toml")
    } else {
        PathBuf::from(".config/rustmux/config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_config_loads_selected_file_and_reports_path_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dev config.toml");
        fs::write(
            &path,
            r#"scrollback_lines = 321
clear_defaults = true
[keybinds.locked]
"Ctrl a" = { actions = [{ action = "switch-mode", mode = "normal" }] }
"#,
        )
        .unwrap();
        let config = load_with_path(Some(&path)).unwrap();
        assert_eq!(config.scrollback_lines(), 321);
        assert!(config.shortcuts().clear_defaults());

        fs::write(&path, "shell = 123").unwrap();
        let error = load_with_path(Some(&path)).err().unwrap();
        assert!(error.contains(&format!("invalid {}:", path.display())));
        assert!(error.contains("shell must be a string"));

        fs::remove_file(&path).unwrap();
        let error = load_with_path(Some(&path)).err().unwrap();
        assert!(error.contains(&format!("could not read {}:", path.display())));
        assert!(load_config(&path, true).is_ok());
    }

    #[test]
    fn shell_precedence_keeps_environment_override_and_login_default() {
        assert_eq!(
            select_shell(
                Some("override".into()),
                Some("configured".into()),
                Some("login".into()),
            ),
            OsString::from("override")
        );
        assert_eq!(
            select_shell(None, Some("configured".into()), Some("login".into())),
            OsString::from("configured")
        );
        assert_eq!(
            select_shell(None, None, Some("login".into())),
            OsString::from("login")
        );
        assert_eq!(select_shell(None, None, None), OsString::from("/bin/sh"));
    }

    #[test]
    fn parser_reads_shell_scrollback_and_ignores_future_configuration() {
        assert_eq!(
            parse_config(
                r#"
shell = "/opt/homebrew/bin/fish"
scrollback_lines = 5000

[theme]
preset = "mocha"
"#,
            )
            .unwrap(),
            ParsedConfig {
                manager: manager::Bindings::default(),
                theme: crate::theme::Theme::default(),
                shell: Some("/opt/homebrew/bin/fish".to_owned()),
                notifications: Notifications::default(),
                scrollback_lines: Some(5000),
                shortcuts: Shortcuts::default(),
                persistence: PersistenceOptions::default(),
                remain_on_exit: false,
                mouse_hover_cursor: false,
                clipboard_write: false,
            }
        );
        assert_eq!(
            parse_config("scrollback_lines = 5000").unwrap(),
            ParsedConfig {
                scrollback_lines: Some(5000),
                ..ParsedConfig::default()
            }
        );
        assert_eq!(
            parse_config("scrollback_lines = 0")
                .unwrap()
                .scrollback_lines,
            Some(0)
        );
        assert!(parse_config("shell = 7").unwrap_err().contains("string"));
        assert!(
            parse_config("shell = \"  \"")
                .unwrap_err()
                .contains("nonempty")
        );
    }

    #[test]
    fn pane_retention_is_opt_in_and_respawn_bindings_are_supported() {
        assert!(!parse_config("").unwrap().remain_on_exit);
        assert!(
            parse_config("remain_on_exit = true")
                .unwrap()
                .remain_on_exit
        );
        assert!(parse_config("remain_on_exit = 'true'").is_err());
        let shortcuts = parse_config("clear_defaults=true\n[keybinds.locked]\n\"Ctrl b\"={actions=[{action='switch-mode', mode='normal'}]}\n[keybinds.normal]\nr={actions=['respawn-pane', {action='switch-mode', mode='locked'}]}\n[keybinds.pane]\nR={actions=['respawn-pane', {action='switch-mode', mode='locked'}]}").unwrap().shortcuts;
        assert_eq!(shortcuts.resolve(b'r'), Some(b'R'));
        assert_eq!(
            shortcuts.pane_binding(b'R').unwrap().action,
            PaneAction::Respawn
        );
    }

    #[test]
    fn persistence_defaults_are_opt_in_and_values_are_validated() {
        assert_eq!(
            parse_config("").unwrap().persistence,
            PersistenceOptions::default()
        );
        assert_eq!(parse_config("autosave_interval_seconds = 30\nsave_scrollback = true\nsave_scrollback_colors = true").unwrap().persistence,
            PersistenceOptions { autosave_interval_seconds: 30, save_scrollback: true, save_scrollback_colors: true });
        for source in [
            "autosave_interval_seconds = -1",
            "autosave_interval_seconds = 0.5",
            "save_scrollback = 1",
            "save_scrollback_colors = 'true'",
        ] {
            assert!(parse_config(source).is_err());
        }
    }

    #[test]
    fn parser_rejects_invalid_scrollback_limits() {
        for source in [
            "scrollback_lines = -1",
            "scrollback_lines = 1.5",
            "scrollback_lines = true",
            "scrollback_lines = \"1000\"",
        ] {
            assert!(parse_config(source).is_err(), "accepted {source:?}");
        }
    }

    #[test]
    fn parser_reads_shortcut_keys_and_rejects_collisions() {
        let shortcuts =
            parse_config("[shortcuts]\nnew_window = 'N'\nsplit_right = 'R'\nsplit_down = 'D'")
                .unwrap()
                .shortcuts;
        assert_eq!(shortcuts.resolve(b'N'), Some(b'c'));
        assert_eq!(shortcuts.resolve(b'c'), None);
        assert_eq!(shortcuts.key_for(b'%'), b'R');
        for source in [
            "shortcuts = true",
            "[shortcuts]\nnew_window = 'ab'",
            "[shortcuts]\nnew_window = 'é'",
            "[shortcuts]\nnew_window = 'n'",
            "[shortcuts]\nnew_window = 'd'",
            "[shortcuts]\nnew_window = '%'",
            "[shortcuts]\nnew_window = 'N'\nsplit_right = 'N'",
            "[shortcuts]\nnew_widow = 'N'",
        ] {
            assert!(parse_config(source).is_err(), "accepted {source:?}");
        }
    }

    #[test]
    fn mode_keybinds_accept_main_style_actions_and_normal_exit() {
        let parsed = parse_config(
            r#"
[keybinds.locked]
"Ctrl b" = { actions = [{ action = "switch-mode", mode = "normal" }], display = "always" }
[keybinds.normal]
N = { actions = ["new-window", { action = "switch-mode", mode = "locked" }], display = "always" }
esc = { actions = [{ action = "switch-mode", mode = "locked" }], display = "help" }
"Ctrl g" = { actions = [{ action = "switch-mode", mode = "locked" }], display = "help" }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }], display = "always" }
"#,
        )
        .unwrap();
        assert_eq!(parsed.shortcuts.key_for(b'c'), b'N');
        assert_eq!(parsed.shortcuts.locked_entry_key(), 2);
        assert!(parsed.shortcuts.exits_normal(27));
        assert!(parsed.shortcuts.exits_normal(7));
        assert!(!parsed.shortcuts.exits_normal(b'p'));
        let remapped = parse_config(
            "[keybinds.locked]\n'Ctrl a' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }",
        )
        .unwrap();
        assert_eq!(remapped.shortcuts.locked_entry_key(), 1);
        for source in [
            "[keybinds.locked]\na = { actions = [{ action = 'switch-mode', mode = 'normal' }] }",
            "[keybinds.locked]\n'Ctrl a' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }\n'Ctrl b' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }",
            "[keybinds.locked]\n'Ctrl w' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }",
            "[keybinds.locked]\n'Ctrl a' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }\n[keybinds.normal]\n'Ctrl a' = { actions = [{ action = 'switch-mode', mode = 'pane' }] }",
        ] {
            assert!(parse_config(source).is_err(), "accepted {source:?}");
        }
        let unsupported =
            parse_config("[keybinds.normal]\nN = { actions = ['new-window', 'future-action'] }")
                .unwrap();
        assert_eq!(unsupported.shortcuts.key_for(b'c'), b'c');
    }

    #[test]
    fn clear_defaults_keeps_only_explicit_supported_bindings() {
        let shortcuts = parse_config(
            r#"
clear_defaults = true
[keybinds.locked]
"Ctrl a" = { actions = [{ action = "switch-mode", mode = "normal" }] }
[keybinds.normal]
c = { actions = ["new-window", { action = "switch-mode", mode = "locked" }] }
x = { actions = ["close-window", { action = "switch-mode", mode = "locked" }] }
"Ctrl w" = { actions = ["switch-session", { action = "switch-mode", mode = "locked" }] }
"?" = { actions = ["show-help", { action = "switch-mode", mode = "locked" }] }
1 = { actions = [{ action = "go-to-window", index = 1 }, { action = "switch-mode", mode = "locked" }] }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.pane]
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        )
        .unwrap()
        .shortcuts;
        assert!(shortcuts.clear_defaults());
        assert_eq!(shortcuts.locked_entry_key(), 1);
        for (key, action) in [
            (b'c', b'c'),
            (b'x', b'&'),
            (23, 23),
            (b'?', b'?'),
            (b'1', b'1'),
        ] {
            assert_eq!(shortcuts.resolve(key), Some(action));
        }
        for key in [b'n', b'%', b'Z', b'0', 8, b'd'] {
            assert_eq!(shortcuts.resolve(key), None, "unexpected key {key}");
        }
        assert!(shortcuts.enters_pane(16));
        assert_eq!(
            shortcuts.pane_binding(27).unwrap().action,
            PaneAction::Locked
        );
        assert_eq!(
            parse_config("").unwrap().shortcuts.resolve(b'n'),
            Some(b'n')
        );
        assert_eq!(
            parse_config("clear_defaults = false")
                .unwrap()
                .shortcuts
                .resolve(b'n'),
            Some(b'n')
        );
    }

    #[test]
    fn clear_defaults_requires_an_explicit_locked_entry_and_boolean_value() {
        for source in [
            "clear_defaults = true",
            "clear_defaults = true\n[keybinds.normal]\nc = { actions = ['new-window', { action = 'switch-mode', mode = 'locked' }] }",
            "clear_defaults = 1",
        ] {
            assert!(parse_config(source).is_err(), "accepted {source:?}");
        }
        let shortcuts = parse_config(
            "clear_defaults = true\n[keybinds.locked]\n'Ctrl a' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }\n[keybinds.pane]\nesc = { actions = [{ action = 'switch-mode', mode = 'locked' }] }",
        )
        .unwrap()
        .shortcuts;
        assert_eq!(
            shortcuts.pane_binding(27).unwrap().action,
            PaneAction::Locked
        );
        let shortcuts = parse_config(
            "clear_defaults = true\n[keybinds.locked]\n'Ctrl h' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }",
        )
        .unwrap()
        .shortcuts;
        assert_eq!(shortcuts.locked_entry_key(), 8);
        assert_eq!(shortcuts.resolve(8), None);
        assert!(parse_config(
            "clear_defaults = true\n[keybinds.locked]\n'Ctrl h' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }\n[keybinds.normal]\n'Ctrl h' = { actions = ['show-help', { action = 'switch-mode', mode = 'locked' }] }",
        )
        .is_err());
    }

    #[test]
    fn pane_mode_reads_supported_actions_and_ignores_unsupported_sequences() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
[keybinds.pane]
b = { actions = ["break-pane", { action = "switch-mode", mode = "locked" }] }
n = { actions = ["new-pane-right", { action = "switch-mode", mode = "locked" }] }
r = { actions = ["new-pane-right", { action = "switch-mode", mode = "locked" }], display = "always" }
d = { actions = ["new-pane-down", { action = "switch-mode", mode = "locked" }] }
h = { actions = ["focus-left"] }
left = { actions = ["focus-left"] }
down = { actions = ["focus-down"] }
up = { actions = ["focus-up"] }
right = { actions = ["focus-right"] }
tab = { actions = ["focus-next-pane"] }
f = { actions = ["toggle-pane-zoom", { action = "switch-mode", mode = "locked" }] }
x = { actions = ["close-pane", { action = "switch-mode", mode = "locked" }] }
p = { actions = [{ action = "switch-mode", mode = "normal" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"[" = { actions = ["move-pane-previous-window", { action = "switch-mode", mode = "locked" }] }
"]" = { actions = ["move-pane-next-window", { action = "switch-mode", mode = "locked" }] }
"#,
        );
        assert!(shortcuts.enters_pane(16));
        assert_eq!(shortcuts.pane_key(PaneAction::SplitRight), Some(b'r'));
        assert_eq!(
            shortcuts.pane_binding(b'h').unwrap().action,
            PaneAction::FocusLeft
        );
        assert!(shortcuts.pane_binding(b'h').unwrap().stay);
        for (direction, action) in [
            (Direction::Left, PaneAction::FocusLeft),
            (Direction::Down, PaneAction::FocusDown),
            (Direction::Up, PaneAction::FocusUp),
            (Direction::Right, PaneAction::FocusRight),
        ] {
            let binding = shortcuts.pane_arrow_binding(direction).unwrap();
            assert_eq!(binding.action, action);
            assert!(binding.stay);
        }
        assert_eq!(shortcuts.pane_binding(9).unwrap().action, PaneAction::Next);
        assert_eq!(
            shortcuts.pane_binding(27).unwrap().action,
            PaneAction::Locked
        );
        assert_eq!(
            shortcuts.pane_binding(b'[').unwrap().action,
            PaneAction::MovePreviousWindow
        );
        assert_eq!(
            shortcuts.pane_binding(b']').unwrap().action,
            PaneAction::MoveNextWindow
        );
    }

    #[test]
    fn resize_mode_reads_main_style_directional_and_transition_bindings() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.normal]
r = { actions = [{ action = "switch-mode", mode = "resize" }] }
[keybinds.resize]
left = { actions = ["resize-pane-left"] }
down = { actions = ["resize-pane-down"] }
up = { actions = ["resize-pane-up"] }
right = { actions = ["resize-pane-right"] }
h = { actions = ["resize-pane-left"], display = "always" }
j = { actions = ["resize-pane-down"] }
k = { actions = ["resize-pane-up"] }
l = { actions = ["resize-pane-right"] }
r = { actions = [{ action = "switch-mode", mode = "normal" }] }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        assert!(shortcuts.enters_resize(b'r'));
        assert_eq!(
            shortcuts.resize_key(ResizeAction::Resize(Direction::Left)),
            Some(b'h')
        );
        for direction in [
            Direction::Left,
            Direction::Down,
            Direction::Up,
            Direction::Right,
        ] {
            assert_eq!(
                shortcuts.resize_arrow_action(direction),
                Some(ResizeAction::Resize(direction))
            );
        }
        assert_eq!(
            shortcuts.resize_binding(b'r').unwrap().action,
            ResizeAction::Normal
        );
        assert_eq!(
            shortcuts.resize_binding(16).unwrap().action,
            ResizeAction::Pane
        );
        assert_eq!(
            shortcuts.resize_binding(27).unwrap().action,
            ResizeAction::Locked
        );
    }

    #[test]
    fn move_mode_reads_main_style_directional_and_transition_bindings() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
[keybinds.move]
left = { actions = ["move-pane-left"] }
down = { actions = ["move-pane-down"] }
up = { actions = ["move-pane-up"] }
right = { actions = ["move-pane-right"] }
h = { actions = ["move-pane-left"], display = "always" }
j = { actions = ["move-pane-down"] }
k = { actions = ["move-pane-up"] }
l = { actions = ["move-pane-right"] }
m = { actions = [{ action = "switch-mode", mode = "normal" }] }
r = { actions = [{ action = "switch-mode", mode = "resize" }] }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        assert!(shortcuts.enters_move(13));
        assert_eq!(
            shortcuts.move_key(MoveAction::Move(Direction::Left)),
            Some(b'h')
        );
        for direction in [
            Direction::Left,
            Direction::Down,
            Direction::Up,
            Direction::Right,
        ] {
            assert_eq!(
                shortcuts.move_arrow_action(direction),
                Some(MoveAction::Move(direction))
            );
        }
        assert_eq!(
            shortcuts.move_binding(b'm').unwrap().action,
            MoveAction::Normal
        );
        assert_eq!(
            shortcuts.move_binding(b'r').unwrap().action,
            MoveAction::Resize
        );
        assert_eq!(shortcuts.move_binding(16).unwrap().action, MoveAction::Pane);
        assert_eq!(
            shortcuts.move_binding(27).unwrap().action,
            MoveAction::Locked
        );
    }

    #[test]
    fn tab_mode_reads_main_style_window_bindings_and_transitions() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
[keybinds.tab]
left = { actions = ["previous-window"] }
right = { actions = ["next-window"] }
h = { actions = ["previous-window"], display = "always" }
l = { actions = ["next-window"], display = "always" }
tab = { actions = ["previous-window"] }
"<" = { actions = ["move-window-left"] }
">" = { actions = ["move-window-right"] }
n = { actions = ["new-window", { action = "switch-mode", mode = "locked" }] }
r = { actions = ["rename-window"] }
x = { actions = ["close-window", { action = "switch-mode", mode = "locked" }] }
3 = { actions = [{ action = "go-to-window", index = 3 }], display = "hidden" }
p = { actions = [{ action = "switch-mode", mode = "normal" }] }
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        );
        assert!(shortcuts.enters_tab(20));
        assert_eq!(shortcuts.tab_key(TabAction::Previous), Some(b'h'));
        assert_eq!(shortcuts.tab_key(TabAction::Next), Some(b'l'));
        assert_eq!(
            shortcuts.tab_arrow_binding(Direction::Left).unwrap().action,
            TabAction::Previous
        );
        assert_eq!(
            shortcuts
                .tab_arrow_binding(Direction::Right)
                .unwrap()
                .action,
            TabAction::Next
        );
        assert_eq!(
            shortcuts.tab_binding(9).unwrap().action,
            TabAction::Previous
        );
        assert_eq!(
            shortcuts.tab_binding(b'3').unwrap().action,
            TabAction::Select(3)
        );
        assert!(!shortcuts.tab_binding(b'n').unwrap().stay);
        assert!(!shortcuts.tab_binding(b'x').unwrap().stay);
        assert_eq!(
            shortcuts.tab_binding(b'x').unwrap().action,
            TabAction::Close
        );
        assert!(shortcuts.tab_binding(b'r').unwrap().stay);
        assert_eq!(
            shortcuts.tab_binding(b'<').unwrap().action,
            TabAction::MoveLeft
        );
        assert_eq!(
            shortcuts.tab_binding(b'>').unwrap().action,
            TabAction::MoveRight
        );
        assert_eq!(
            shortcuts.tab_binding(b'p').unwrap().action,
            TabAction::Normal
        );
        assert_eq!(shortcuts.tab_binding(13).unwrap().action, TabAction::Move);
        assert_eq!(shortcuts.tab_binding(27).unwrap().action, TabAction::Locked);
    }

    #[test]
    fn session_mode_reads_main_style_bindings_and_cross_mode_entries() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl o" = { actions = [{ action = "switch-mode", mode = "session" }] }
[keybinds.pane]
"Ctrl o" = { actions = [{ action = "switch-mode", mode = "session" }] }
[keybinds.tab]
"Ctrl o" = { actions = [{ action = "switch-mode", mode = "session" }] }
[keybinds.resize]
"Ctrl o" = { actions = [{ action = "switch-mode", mode = "session" }] }
[keybinds.move]
"Ctrl o" = { actions = [{ action = "switch-mode", mode = "session" }] }
[keybinds.session]
d = { actions = ["detach"], display = "always" }
w = { actions = ["switch-session", { action = "switch-mode", mode = "locked" }], display = "always" }
o = { actions = [{ action = "switch-mode", mode = "normal" }], display = "always" }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
esc = { actions = [{ action = "switch-mode", mode = "locked" }] }
"?" = { actions = ["show-help"] }
x = { actions = ["unsupported-action"] }
"#,
        );
        assert!(shortcuts.enters_session(15));
        assert_eq!(shortcuts.session_entry_key(), Some(15));
        assert_eq!(
            shortcuts.pane_binding(15).unwrap().action,
            PaneAction::Session
        );
        assert_eq!(
            shortcuts.tab_binding(15).unwrap().action,
            TabAction::Session
        );
        assert_eq!(
            shortcuts.resize_binding(15).unwrap().action,
            ResizeAction::Session
        );
        assert_eq!(
            shortcuts.move_binding(15).unwrap().action,
            MoveAction::Session
        );
        for (key, action) in [
            (b'd', SessionAction::Detach),
            (b'w', SessionAction::Manager),
            (b'o', SessionAction::Normal),
            (16, SessionAction::Pane),
            (27, SessionAction::Locked),
            (b'?', SessionAction::Help),
        ] {
            assert_eq!(shortcuts.session_binding(key).unwrap().action, action);
        }
        assert_eq!(shortcuts.session_key(SessionAction::Manager), Some(b'w'));
        assert!(shortcuts.session_binding(b'x').is_none());
    }

    #[test]
    fn supported_modes_accept_direct_cross_mode_transitions() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.normal]
[keybinds.pane]
"Ctrl r" = { actions = [{ action = "switch-mode", mode = "resize" }] }
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
[keybinds.resize]
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
[keybinds.move]
"Ctrl t" = { actions = [{ action = "switch-mode", mode = "tab" }] }
[keybinds.tab]
"Ctrl r" = { actions = [{ action = "switch-mode", mode = "resize" }] }
"Ctrl p" = { actions = [{ action = "switch-mode", mode = "pane" }] }
"Ctrl m" = { actions = [{ action = "switch-mode", mode = "move" }] }
"#,
        );
        assert_eq!(
            shortcuts.pane_binding(18).unwrap().action,
            PaneAction::Resize
        );
        assert_eq!(shortcuts.pane_binding(13).unwrap().action, PaneAction::Move);
        assert_eq!(shortcuts.pane_binding(20).unwrap().action, PaneAction::Tab);
        assert_eq!(
            shortcuts.resize_binding(13).unwrap().action,
            ResizeAction::Move
        );
        assert_eq!(
            shortcuts.resize_binding(20).unwrap().action,
            ResizeAction::Tab
        );
        assert_eq!(shortcuts.move_binding(20).unwrap().action, MoveAction::Tab);
        assert_eq!(shortcuts.tab_binding(18).unwrap().action, TabAction::Resize);
        assert_eq!(shortcuts.tab_binding(16).unwrap().action, TabAction::Pane);
        assert_eq!(shortcuts.tab_binding(13).unwrap().action, TabAction::Move);
    }

    #[test]
    fn normal_window_action_overrides_displaced_default_key() {
        let shortcuts = parse_config(
            r#"
[keybinds.normal]
x = { actions = ["close-window", { action = "switch-mode", mode = "locked" }] }
"#,
        )
        .unwrap()
        .shortcuts;
        assert_eq!(shortcuts.resolve(b'x'), Some(b'&'));
        assert_eq!(shortcuts.resolve(b'&'), None);
        assert_eq!(shortcuts.key_for(b'&'), b'x');
        assert!(!shortcuts.action_is_active(b'x'));
        let aliases = parse_config(
            r#"
[keybinds.normal]
tab = { actions = ["previous-window", { action = "switch-mode", mode = "locked" }] }
"," = { actions = ["rename-window"] }
"#,
        )
        .unwrap()
        .shortcuts;
        assert_eq!(aliases.resolve(b'\t'), Some(b'p'));
        assert!(!aliases.action_is_active(b'\t'));
        assert_eq!(aliases.resolve(b','), Some(b','));
    }

    #[test]
    fn parser_reads_notification_defaults_overrides_and_disable_switch() {
        assert_eq!(
            parse_config("").unwrap().notifications,
            Notifications::default()
        );
        assert_eq!(
            parse_config(
                r#"
[notifications]
long_command_bell = true
command_duration_seconds = 12
future_option = "ignored"
"#,
            )
            .unwrap()
            .notifications,
            Notifications {
                long_command_bell: true,
                command_duration: Duration::from_secs(12),
                ..Notifications::default()
            }
        );
        let disabled = parse_config(
            r#"
[notifications]
long_command_bell = false
"#,
        )
        .unwrap()
        .notifications;
        assert_eq!(disabled.command_bell_after(), None);
    }

    #[test]
    fn desktop_delivery_is_opt_in_and_independent_of_bell() {
        assert!(!Notifications::default().desktop);
        let configured = parse_config("[notifications]\ndesktop=true\nlong_command_bell=false")
            .unwrap()
            .notifications;
        assert_eq!(configured.command_bell_after(), None);
        assert!(configured.command_reminder_after().is_some());
        let disabled = parse_config("[notifications]\ndesktop=true\nenabled=false")
            .unwrap()
            .notifications;
        assert_eq!(disabled.command_reminder_after(), None);
        assert!(parse_config("[notifications]\ndesktop=1").is_err());
    }

    #[test]
    fn notification_exclusions_normalize_names_and_can_clear_defaults() {
        let defaults = Notifications::default();
        assert!(defaults.excludes_application("/usr/bin/NVIM"));
        assert!(!defaults.excludes_application("nvim-wrapper"));
        let configured = parse_config("[notifications]\nexclude_applications=[' /usr/bin/Python ', 'PYTHON', 'sleep']\nenabled=false\n").unwrap().notifications;
        assert_eq!(
            configured.exclude_applications.as_ref(),
            ["Python", "sleep"]
        );
        assert!(configured.excludes_application("/opt/bin/python"));
        assert_eq!(configured.command_bell_after(), None);
        let cleared = parse_config("[notifications]\nexclude_applications=[]\n")
            .unwrap()
            .notifications;
        assert!(cleared.exclude_applications.is_empty());
        assert!(cleared.command_bell_after().is_some());
    }

    #[test]
    fn notification_exclusions_reject_invalid_or_unbounded_input() {
        for source in [
            "[notifications]\nenabled=1",
            "[notifications]\nexclude_applications='nvim'",
            "[notifications]\nexclude_applications=[1]",
            "[notifications]\nexclude_applications=['']",
            "[notifications]\nexclude_applications=['  ']",
            "[notifications]\nexclude_applications=['/']",
            "[notifications]\nexclude_applications=[\"bad\\nname\"]",
        ] {
            assert!(parse_config(source).is_err(), "accepted {source:?}");
        }
        let names = vec!["'nvim'"; 257].join(",");
        assert!(parse_config(&format!("[notifications]\nexclude_applications=[{names}]")).is_err());
        assert!(
            parse_config(&format!(
                "[notifications]\nexclude_applications=['{}']",
                "a".repeat(257)
            ))
            .is_err()
        );
    }

    #[test]
    fn parser_rejects_invalid_notification_values() {
        for source in [
            "notifications = true",
            "[notifications]\nlong_command_bell = 1",
            "[notifications]\ncommand_duration_seconds = 0",
            "[notifications]\ncommand_duration_seconds = -1",
            "[notifications]\ncommand_duration_seconds = 1.5",
        ] {
            assert!(parse_config(source).is_err(), "accepted {source:?}");
        }
    }

    #[test]
    fn config_path_prefers_xdg_then_home() {
        assert_eq!(
            config_path_from(Some("/xdg".into()), Some("/home/user".into())),
            PathBuf::from("/xdg/rustmux/config.toml")
        );
        assert_eq!(
            config_path_from(None, Some("/home/user".into())),
            PathBuf::from("/home/user/.config/rustmux/config.toml")
        );
    }

    #[test]
    fn history_mode_accepts_multiple_entries_and_navigation_bindings() {
        let shortcuts = parse_config(
            r#"
[keybinds.locked]
"Ctrl s" = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.normal]
enter = { actions = [{ action = "switch-mode", mode = "history" }] }
s = { actions = [{ action = "switch-mode", mode = "history" }] }
[keybinds.history]
u = { actions = ["scroll-page-up"] }
down = { actions = ["scroll-down"] }
"/" = { actions = ["history-search-forward"] }
p = { actions = [{ action = "switch-mode", mode = "pane" }] }
"Ctrl g" = { actions = [{ action = "switch-mode", mode = "locked" }] }
"#,
        )
        .unwrap()
        .shortcuts;
        assert!(shortcuts.enters_history_locked(19));
        for key in [b'[', b's', 13] {
            assert_eq!(shortcuts.resolve(key), Some(b'['));
        }
        assert_eq!(
            shortcuts.history_binding(HistoryKey::Byte(b'u')),
            Some(HistoryAction::Key(HistoryKey::PageUp))
        );
        assert_eq!(
            shortcuts.history_binding(HistoryKey::Down),
            Some(HistoryAction::Key(HistoryKey::Byte(b'j')))
        );
        assert_eq!(
            shortcuts.history_binding(HistoryKey::Byte(b'p')),
            Some(HistoryAction::SwitchMode(HistoryMode::Pane))
        );
    }

    #[test]
    fn history_mode_rejects_invalid_keys_actions_and_conflicting_entries() {
        for source in [
            "[keybinds.history]\nx = { actions = ['future-action'] }",
            "[keybinds.history]\nx = { actions = ['scroll-up', 'scroll-down'] }",
            "[keybinds.history]\n'Alt x' = { actions = ['scroll-up'] }",
            "[keybinds.history]\nx = { actions = [{ action = 'switch-mode', mode = 'future' }] }",
            "[keybinds.history]\nenter = { actions = ['scroll-up'] }\n'Ctrl m' = { actions = ['scroll-down'] }",
            "[keybinds.locked]\n'Ctrl b' = { actions = [{ action = 'switch-mode', mode = 'history' }] }",
        ] {
            assert!(parse_config(source).is_err(), "accepted {source:?}");
        }
        let collision = "[keybinds.normal]\ns = { actions = [{ action = 'switch-mode', mode = 'history' }] }\nenter = { actions = [{ action = 'switch-mode', mode = 'history' }] }\n'Ctrl m' = { actions = [{ action = 'switch-mode', mode = 'move' }] }";
        assert!(parse_config(collision).is_err());
    }
}
