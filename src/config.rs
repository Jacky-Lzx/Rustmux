//! Load the small configuration surface supported by the human-reviewed track.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::layout::Direction;

const DEFAULT_COMMAND_DURATION_SECONDS: u64 = 5;
pub const DEFAULT_SCROLLBACK_LINES: usize = crate::screen::SCROLLBACK_MAX_LINES;
const SHORTCUT_NAMES: [&str; 3] = ["new_window", "split_right", "split_down"];
const DEFAULT_SHORTCUT_KEYS: [u8; 3] = *b"c%\"";
const FIXED_SHORTCUT_KEYS: &[u8] = b"np\t&x<> {}!moZz[Ee?hjkl,1234567890dq";

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
    Normal,
    Locked,
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
    Locked,
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
    Locked,
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
    Move,
    Locked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabBinding {
    key: u8,
    pub action: TabAction,
    pub stay: bool,
    preferred: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shortcuts {
    keys: [u8; 3],
    normal_actions: [Option<(u8, u8)>; 16],
    normal_action_len: usize,
    pane_enter: Option<u8>,
    resize_enter: Option<u8>,
    move_enter: Option<u8>,
    tab_enter: Option<u8>,
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
    normal_exit: [u8; 8],
    normal_exit_len: usize,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            keys: DEFAULT_SHORTCUT_KEYS,
            normal_actions: [None; 16],
            normal_action_len: 0,
            pane_enter: None,
            resize_enter: None,
            move_enter: None,
            tab_enter: None,
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
            normal_exit: [0; 8],
            normal_exit_len: 0,
        }
    }
}

impl Shortcuts {
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
        {
            return None;
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Notifications {
    pub long_command_bell: bool,
    pub command_duration: Duration,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            long_command_bell: true,
            command_duration: Duration::from_secs(DEFAULT_COMMAND_DURATION_SECONDS),
        }
    }
}

impl Notifications {
    pub fn command_bell_after(self) -> Option<Duration> {
        self.long_command_bell.then_some(self.command_duration)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    shell: OsString,
    notifications: Notifications,
    scrollback_lines: usize,
    shortcuts: Shortcuts,
}

impl Config {
    pub fn shell(&self) -> &OsString {
        &self.shell
    }

    pub fn notifications(&self) -> Notifications {
        self.notifications
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
    shell: Option<String>,
    notifications: Notifications,
    scrollback_lines: Option<usize>,
    shortcuts: Shortcuts,
}

/// Load and validate the complete configuration used by a new session.
pub fn load() -> Result<Config, String> {
    let configured = load_config(&config_path())?;
    Ok(Config {
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
    })
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

fn load_config(path: &Path) -> Result<ParsedConfig, String> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ParsedConfig::default());
        }
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    parse_config(&source).map_err(|error| format!("invalid {}: {error}", path.display()))
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
    let shortcuts = parse_keybinds(
        document.get("keybinds"),
        parse_shortcuts(document.get("shortcuts"))?,
    )?;
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
        shell,
        notifications,
        scrollback_lines,
        shortcuts,
    })
}

fn parse_shortcuts(value: Option<&toml::Value>) -> Result<Shortcuts, String> {
    let Some(value) = value else {
        return Ok(Shortcuts::default());
    };
    let table = value.as_table().ok_or("shortcuts must be a table")?;
    let mut shortcuts = Shortcuts::default();
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
    }
    validate_shortcuts(shortcuts)?;
    Ok(shortcuts)
}

fn validate_shortcuts(shortcuts: Shortcuts) -> Result<(), String> {
    for (index, key) in shortcuts.keys.iter().enumerate() {
        if shortcuts.keys[..index].contains(key) || FIXED_SHORTCUT_KEYS.contains(key) {
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
        for (key, binding) in locked {
            let enters_normal = binding
                .as_table()
                .and_then(|table| table.get("actions"))
                .and_then(toml::Value::as_array)
                .is_some_and(|actions| {
                    actions.iter().any(|action| {
                        action.as_table().is_some_and(|table| {
                            table.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
                                && table.get("mode").and_then(toml::Value::as_str) == Some("normal")
                        })
                    })
                });
            if enters_normal && key != "Ctrl b" {
                return Err(
                    "keybinds.locked currently supports only Ctrl b to enter normal mode"
                        .to_owned(),
                );
            }
        }
    }
    let Some(normal) = keybinds.get("normal") else {
        return Ok(shortcuts);
    };
    let normal = normal.as_table().ok_or("keybinds.normal must be a table")?;
    let mut configured_actions = [false; 3];
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
        if let Some(index) = names
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
                            table.get("action").and_then(toml::Value::as_str) == Some("switch-mode")
                                && table.get("mode").and_then(toml::Value::as_str) == Some("locked")
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
        } else if let Some((action, canonical)) = names.iter().find_map(|name| {
            let canonical = match *name {
                "close-window" => b'&',
                "rename-window" => b',',
                "next-window" => b'n',
                "previous-window" => b'p',
                "move-window-left" => b'<',
                "move-window-right" => b'>',
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
            let byte = parse_normal_action_key(key).ok_or_else(|| {
                format!("keybinds.normal.{key} must be one printable ASCII key or tab for {action}")
            })?;
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
                    "multiple keybinds.normal tab-mode entry keys are not supported yet".to_owned(),
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
    validate_shortcuts(shortcuts)?;
    parse_pane_bindings(keybinds.get("pane"), &mut shortcuts)?;
    parse_resize_bindings(keybinds.get("resize"), &mut shortcuts)?;
    parse_move_bindings(keybinds.get("move"), &mut shortcuts)?;
    parse_tab_bindings(keybinds.get("tab"), &mut shortcuts)?;
    for (key, _) in shortcuts.normal_actions[..shortcuts.normal_action_len]
        .iter()
        .flatten()
    {
        if shortcuts.keys.contains(key) || shortcuts.exits_normal(*key) {
            return Err(
                "keybinds.normal action key conflicts with another configured shortcut".to_owned(),
            );
        }
    }
    for key in &shortcuts.normal_exit[..shortcuts.normal_exit_len] {
        if shortcuts.keys.contains(key)
            || FIXED_SHORTCUT_KEYS.contains(key)
            || matches!(*key, 2 | 23)
        {
            return Err("keybinds.normal locked-mode exit conflicts with a command".to_owned());
        }
    }
    Ok(shortcuts)
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
                _ => match single
                    .as_table()
                    .and_then(|value| value.get("mode"))
                    .and_then(toml::Value::as_str)
                {
                    Some("locked")
                        if single
                            .as_table()
                            .and_then(|value| value.get("action"))
                            .and_then(toml::Value::as_str)
                            == Some("switch-mode") =>
                    {
                        Some((PaneAction::Locked, false))
                    }
                    Some("normal")
                        if single
                            .as_table()
                            .and_then(|value| value.get("action"))
                            .and_then(toml::Value::as_str)
                            == Some("switch-mode") =>
                    {
                        Some((PaneAction::Normal, false))
                    }
                    _ => None,
                },
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
                        Some("pane") => Some(ResizeAction::Pane),
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
                        Some("pane") => Some(MoveAction::Pane),
                        Some("resize") => Some(MoveAction::Resize),
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
                            Some("pane") => Some(TabAction::Pane),
                            Some("move") => Some(TabAction::Move),
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
                shell: Some("/opt/homebrew/bin/fish".to_owned()),
                notifications: Notifications::default(),
                scrollback_lines: Some(5000),
                shortcuts: Shortcuts::default(),
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
        assert!(parsed.shortcuts.exits_normal(27));
        assert!(parsed.shortcuts.exits_normal(7));
        assert!(!parsed.shortcuts.exits_normal(b'p'));
        assert!(parse_config("[keybinds.locked]\n'Ctrl a' = { actions = [{ action = 'switch-mode', mode = 'normal' }] }").is_err());
        let unsupported =
            parse_config("[keybinds.normal]\nN = { actions = ['new-window', 'future-action'] }")
                .unwrap();
        assert_eq!(unsupported.shortcuts.key_for(b'c'), b'c');
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
}
