//! Presentation metadata is independent of the physical binding's dispatch.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BindingDisplay {
    Always,
    Help,
    Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BindingMode {
    Locked,
    Normal,
    Pane,
    Resize,
    Move,
    Tab,
    Session,
    History,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BindingHint {
    pub key: HistoryKey,
    pub label: &'static str,
}

pub(super) fn mode(name: &str) -> Option<BindingMode> {
    Some(match name {
        "locked" => BindingMode::Locked,
        "normal" => BindingMode::Normal,
        "pane" => BindingMode::Pane,
        "resize" => BindingMode::Resize,
        "move" => BindingMode::Move,
        "tab" => BindingMode::Tab,
        "session" => BindingMode::Session,
        "history" => BindingMode::History,
        _ => return None,
    })
}

pub(super) fn parse_display_key(key: &str) -> Option<HistoryKey> {
    match key {
        "pageup" => Some(HistoryKey::PageUp),
        "pagedown" => Some(HistoryKey::PageDown),
        "home" => Some(HistoryKey::Home),
        "end" => Some(HistoryKey::End),
        _ => parse_arrow_key(key)
            .map(|direction| match direction {
                Direction::Left => HistoryKey::Left,
                Direction::Down => HistoryKey::Down,
                Direction::Up => HistoryKey::Up,
                Direction::Right => HistoryKey::Right,
            })
            .or_else(|| parse_mode_key(key).map(HistoryKey::Byte)),
    }
}

pub(super) fn parse_displays(table: &toml::Table, shortcuts: &mut Shortcuts) -> Result<(), String> {
    let mut count = 0;
    for (name, bindings) in table {
        let Some(mode) = mode(name) else { continue };
        let bindings = bindings
            .as_table()
            .ok_or_else(|| format!("keybinds.{name} must be a table"))?;
        for (key, binding) in bindings {
            let Some(value) = binding.get("display") else {
                continue;
            };
            let display = match value.as_str() {
                Some("always") => BindingDisplay::Always,
                Some("help" | "help-menu") => BindingDisplay::Help,
                Some("hidden" | "never") => BindingDisplay::Hidden,
                _ => {
                    return Err(format!(
                        "keybinds.{name}.{key}.display must be always, help or hidden"
                    ));
                }
            };
            // Ignore metadata attached to unsupported actions, just as dispatch does.
            // Probe without display to avoid confusing a legacy alias (Enter/Ctrl-M)
            // with a supported binding using the same physical byte.
            if binding.get("actions").is_some() {
                let mut action_binding = binding.clone();
                action_binding
                    .as_table_mut()
                    .expect("display binding table")
                    .remove("display");
                let baseline = Shortcuts {
                    clear_defaults: true,
                    locked_enter: 0,
                    ..Shortcuts::default()
                };
                let one = toml::Value::Table(toml::Table::from_iter([(
                    name.clone(),
                    toml::Value::Table(toml::Table::from_iter([(key.clone(), action_binding)])),
                )]));
                if !parse_keybinds(Some(&one), baseline).is_ok_and(|result| result != baseline) {
                    continue;
                }
            }
            let parsed = parse_display_key(key)
                .ok_or_else(|| format!("unsupported keybinds.{name}.{key} display key"))?;
            if let Some((_, _, previous)) = shortcuts.displays[..count]
                .iter()
                .flatten()
                .find(|(m, k, _)| *m == mode && *k == parsed)
            {
                if *previous == display {
                    continue;
                }
                return Err(format!(
                    "duplicate keybinds.{name}.{key} display key with conflicting visibility"
                ));
            }
            if count == shortcuts.displays.len() {
                return Err("too many binding display overrides".into());
            }
            shortcuts.displays[count] = Some((mode, parsed, display));
            count += 1;
        }
    }
    Ok(())
}

impl Shortcuts {
    pub(crate) fn display(&self, mode: BindingMode, key: HistoryKey) -> BindingDisplay {
        self.displays
            .iter()
            .flatten()
            .find_map(|(m, k, display)| (*m == mode && *k == key).then_some(*display))
            .unwrap_or(BindingDisplay::Always)
    }

    pub(crate) fn has_display(&self, mode: BindingMode) -> bool {
        self.displays.iter().flatten().any(|(m, _, _)| *m == mode)
    }

    pub(crate) fn uses_binding_hints(&self, mode: BindingMode) -> bool {
        self.has_display(mode)
            || (mode == BindingMode::Normal && self.normal_arrows.iter().any(Option::is_some))
    }

    /// Enumerate actual supported physical bindings, including aliases and arrows.
    /// Explicit display entries precede implicit defaults in a bounded footer.
    pub(crate) fn hints(&self, mode: BindingMode, session: bool, help: bool) -> Vec<BindingHint> {
        let mut keys: Vec<_> = self
            .displays
            .iter()
            .flatten()
            .filter_map(|(m, key, _)| (*m == mode).then_some(*key))
            .collect();
        if !help && mode == BindingMode::Normal {
            keys.extend(
                b"c%\"hjklnpZ?"
                    .iter()
                    .map(|action| HistoryKey::Byte(self.key_for(*action))),
            );
            keys.extend(self.tab_enter.map(HistoryKey::Byte));
            keys.extend(self.session_enter.map(HistoryKey::Byte));
            if session {
                keys.push(HistoryKey::Byte(self.key_for(23)));
            }
        } else if !help && mode == BindingMode::History {
            keys.extend(b"/?nNqEe".iter().copied().map(HistoryKey::Byte));
        } else {
            keys.extend((1..=127).map(HistoryKey::Byte));
        }
        if help
            || !matches!(mode, BindingMode::Normal | BindingMode::History)
            || (mode == BindingMode::Normal && self.normal_arrows.iter().any(Option::is_some))
        {
            keys.extend([
                HistoryKey::Up,
                HistoryKey::Down,
                HistoryKey::Left,
                HistoryKey::Right,
                HistoryKey::PageUp,
                HistoryKey::PageDown,
                HistoryKey::Home,
                HistoryKey::End,
            ]);
        }
        let mut seen = Vec::new();
        keys.into_iter()
            .filter_map(|key| {
                if seen.contains(&key) {
                    return None;
                }
                seen.push(key);
                let display = self.display(mode, key);
                if display == BindingDisplay::Hidden || (!help && display != BindingDisplay::Always)
                {
                    return None;
                }
                self.binding_label(mode, key, session)
                    .map(|label| BindingHint { key, label })
            })
            .collect()
    }

    pub(crate) fn binding_label(
        &self,
        mode: BindingMode,
        key: HistoryKey,
        session: bool,
    ) -> Option<&'static str> {
        let byte = if let HistoryKey::Byte(byte) = key {
            Some(byte)
        } else {
            None
        };
        let direction = match key {
            HistoryKey::Left => Some(Direction::Left),
            HistoryKey::Down => Some(Direction::Down),
            HistoryKey::Up => Some(Direction::Up),
            HistoryKey::Right => Some(Direction::Right),
            _ => None,
        };
        Some(match mode {
            BindingMode::Locked => {
                let byte = byte?;
                if self.enters_history_locked(byte) {
                    "History"
                } else if byte == self.locked_entry_key()
                    && (!self.clear_defaults || self.locked_configured)
                {
                    "Commands"
                } else {
                    return None;
                }
            }
            BindingMode::Normal => {
                if let Some(direction) = direction {
                    return Some(match self.normal_arrow_binding(direction)?.direction {
                        Direction::Left => "Focus left",
                        Direction::Down => "Focus down",
                        Direction::Up => "Focus up",
                        Direction::Right => "Focus right",
                    });
                }
                let byte = byte?;
                if self.enters_pane(byte) {
                    "Pane"
                } else if self.enters_resize(byte) {
                    "Resize"
                } else if self.enters_move(byte) {
                    "Move"
                } else if self.enters_tab(byte) {
                    "Tab"
                } else if self.enters_session(byte) {
                    if !session {
                        return None;
                    }
                    "Session"
                } else if self.exits_normal(byte) {
                    "Lock"
                } else if byte == self.locked_entry_key() {
                    "Literal prefix"
                } else {
                    match self.resolve(byte)? {
                        b'c' => "New window",
                        b'&' => "Close window",
                        b'n' => "Next window",
                        b'p' => "Previous window",
                        b'\t' => "Last window",
                        b'1'..=b'9' | b'0' => "Select window",
                        b',' => "Rename window",
                        b'<' => "Move window left",
                        b'>' => "Move window right",
                        b'%' => "Split right",
                        b'"' => "Split down",
                        b'h' => "Focus left",
                        b'j' => "Focus down",
                        b'k' => "Focus up",
                        b'l' => "Focus right",
                        8 => "Resize left",
                        10 => "Resize down",
                        11 => "Resize up",
                        12 => "Resize right",
                        b'o' => "Next pane",
                        b'R' => "Respawn pane",
                        b'x' => "Close pane",
                        b'Z' => "Zoom",
                        b'z' => "Restore pane",
                        b'{' => "Swap previous pane",
                        b'}' => "Swap next pane",
                        b'!' => "Pane to window",
                        b'm' => "Move pane",
                        b'[' => "History",
                        b'E' => "Edit history",
                        b'e' => "Edit output",
                        b'?' => "Help",
                        23 if session => "Sessions",
                        b'd' if session => "Detach",
                        b'q' => "Quit",
                        _ => return None,
                    }
                }
            }
            BindingMode::Pane => {
                let action = if let Some(byte) = byte {
                    self.pane_binding(byte)?.action
                } else {
                    self.pane_arrow_binding(direction?)?.action
                };
                match action {
                    PaneAction::Help => "Help",
                    PaneAction::Break => "Break",
                    PaneAction::MovePreviousWindow => "Move to previous window",
                    PaneAction::MoveNextWindow => "Move to next window",
                    PaneAction::SplitRight => "Split right",
                    PaneAction::SplitDown => "Split down",
                    PaneAction::FocusLeft => "Focus left",
                    PaneAction::FocusDown => "Focus down",
                    PaneAction::FocusUp => "Focus up",
                    PaneAction::FocusRight => "Focus right",
                    PaneAction::Next => "Next pane",
                    PaneAction::Zoom => "Zoom",
                    PaneAction::Close => "Close pane",
                    PaneAction::Respawn => "Respawn pane",
                    PaneAction::Normal => "Normal",
                    PaneAction::Resize => "Resize",
                    PaneAction::Move => "Move",
                    PaneAction::Tab => "Tab",
                    PaneAction::Session if session => "Session",
                    PaneAction::Session => return None,
                    PaneAction::Locked => "Lock",
                    PaneAction::History => "History",
                }
            }
            BindingMode::Resize => {
                let action = if let Some(byte) = byte {
                    self.resize_binding(byte)?.action
                } else {
                    self.resize_arrow_action(direction?)?
                };
                match action {
                    ResizeAction::Help => "Help",
                    ResizeAction::Resize(Direction::Left) => "Resize left",
                    ResizeAction::Resize(Direction::Down) => "Resize down",
                    ResizeAction::Resize(Direction::Up) => "Resize up",
                    ResizeAction::Resize(Direction::Right) => "Resize right",
                    ResizeAction::Normal => "Normal",
                    ResizeAction::Pane => "Pane",
                    ResizeAction::Move => "Move",
                    ResizeAction::Tab => "Tab",
                    ResizeAction::Session if session => "Session",
                    ResizeAction::Session => return None,
                    ResizeAction::Locked => "Lock",
                    ResizeAction::History => "History",
                }
            }
            BindingMode::Move => {
                let action = if let Some(byte) = byte {
                    self.move_binding(byte)?.action
                } else {
                    self.move_arrow_action(direction?)?
                };
                match action {
                    MoveAction::Help => "Help",
                    MoveAction::Move(Direction::Left) => "Move left",
                    MoveAction::Move(Direction::Down) => "Move down",
                    MoveAction::Move(Direction::Up) => "Move up",
                    MoveAction::Move(Direction::Right) => "Move right",
                    MoveAction::Normal => "Normal",
                    MoveAction::Pane => "Pane",
                    MoveAction::Resize => "Resize",
                    MoveAction::Tab => "Tab",
                    MoveAction::Session if session => "Session",
                    MoveAction::Session => return None,
                    MoveAction::Locked => "Lock",
                    MoveAction::History => "History",
                }
            }
            BindingMode::Tab => {
                let binding = if let Some(byte) = byte {
                    self.tab_binding(byte)?
                } else {
                    self.tab_arrow_binding(direction?)?
                };
                match binding.action {
                    TabAction::Next => "Next window",
                    TabAction::Previous => "Previous window",
                    TabAction::MoveLeft => "Move window left",
                    TabAction::MoveRight => "Move window right",
                    TabAction::New => "New window",
                    TabAction::Rename => "Rename window",
                    TabAction::Close => "Close window",
                    TabAction::Select(_) => "Select window",
                    TabAction::Help => "Help",
                    TabAction::Normal => "Normal",
                    TabAction::Pane => "Pane",
                    TabAction::Resize => "Resize",
                    TabAction::Move => "Move",
                    TabAction::Session if session => "Session",
                    TabAction::Session => return None,
                    TabAction::Locked => "Lock",
                    TabAction::History => "History",
                }
            }
            BindingMode::Session => {
                if !session {
                    return None;
                }
                match self.session_binding(byte?)?.action {
                    SessionAction::Detach => "Detach",
                    SessionAction::Manager => "Sessions",
                    SessionAction::Help => "Help",
                    SessionAction::Normal => "Normal",
                    SessionAction::Pane => "Pane",
                    SessionAction::Resize => "Resize",
                    SessionAction::Move => "Move",
                    SessionAction::Tab => "Tab",
                    SessionAction::Locked => "Lock",
                    SessionAction::History => "History",
                }
            }
            BindingMode::History => {
                if let Some(binding) = self.history_binding(key) {
                    if binding.contains(HistoryAction::SwitchMode(HistoryMode::Session)) && !session
                    {
                        return None;
                    }
                    let action = binding
                        .actions
                        .into_iter()
                        .flatten()
                        .find(|action| {
                            matches!(
                                action,
                                HistoryAction::EditHistory
                                    | HistoryAction::EditLastOutput
                                    | HistoryAction::CopyLastOutput
                                    | HistoryAction::Key(HistoryKey::Byte(b'y'))
                            )
                        })
                        .or_else(|| {
                            binding
                                .actions
                                .into_iter()
                                .flatten()
                                .find(|action| !matches!(action, HistoryAction::SwitchMode(_)))
                        })
                        .or(binding.actions[0])?;
                    match action {
                        HistoryAction::Help => "Help",
                        HistoryAction::EditHistory => "Edit history",
                        HistoryAction::EditLastOutput => "Edit output",
                        HistoryAction::CopyLastOutput => "Copy output",
                        HistoryAction::SwitchMode(HistoryMode::Locked) => "Exit",
                        HistoryAction::SwitchMode(HistoryMode::Normal) => "Normal",
                        HistoryAction::SwitchMode(HistoryMode::Pane) => "Pane",
                        HistoryAction::SwitchMode(HistoryMode::Resize) => "Resize",
                        HistoryAction::SwitchMode(HistoryMode::Move) => "Move",
                        HistoryAction::SwitchMode(HistoryMode::Tab) => "Tab",
                        HistoryAction::SwitchMode(HistoryMode::Session) => "Session",
                        HistoryAction::Key(HistoryKey::Byte(b'e')) => "Next word",
                        HistoryAction::Key(key) => history_label(key)?,
                    }
                } else if !self.clear_defaults {
                    history_label(key)?
                } else {
                    return None;
                }
            }
        })
    }
}

fn history_label(key: HistoryKey) -> Option<&'static str> {
    Some(match key {
        HistoryKey::Up | HistoryKey::Byte(b'k') => "Scroll up",
        HistoryKey::Down | HistoryKey::Byte(b'j') => "Scroll down",
        HistoryKey::PageUp => "Page up",
        HistoryKey::PageDown | HistoryKey::Byte(b' ') => "Page down",
        HistoryKey::Byte(21) => "Half page up",
        HistoryKey::Byte(4) => "Half page down",
        HistoryKey::Home | HistoryKey::Byte(b'g') => "Top",
        HistoryKey::End | HistoryKey::Byte(b'G') => "Bottom",
        HistoryKey::Byte(b'/') => "Search forward",
        HistoryKey::Byte(b'?') => "Search backward",
        HistoryKey::Byte(b'n') => "Next match",
        HistoryKey::Byte(b'N') => "Previous match",
        HistoryKey::Byte(b'y' | 13) => "Copy",
        HistoryKey::Byte(b'v') => "Selection",
        HistoryKey::Byte(b'q' | 27 | 3) => "Exit",
        HistoryKey::Byte(b'E') => "Edit history",
        HistoryKey::Byte(b'e') => "Edit output / word",
        HistoryKey::Left | HistoryKey::Byte(b'h') => "Selection left",
        HistoryKey::Right | HistoryKey::Byte(b'l') => "Selection right",
        HistoryKey::Byte(b'b') => "Previous word",
        HistoryKey::Byte(b'0') => "Line start",
        HistoryKey::Byte(b'$') => "Line end",
        HistoryKey::Byte(b'o') => "Swap selection",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_filters_presentation_without_changing_dispatch() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
clear_defaults=true
[keybinds.locked]
"Ctrl b"={actions=[{action="switch-mode",mode="normal"}]}
[keybinds.normal]
N={actions=["new-window",{action="switch-mode",mode="locked"}],display="help"}
D={actions=["new-pane-down",{action="switch-mode",mode="locked"}],display="hidden"}
H={actions=[{action="switch-mode",mode="history"}],display="always"}
[keybinds.pane]
h={actions=["focus-left"],display="help"}
left={actions=["focus-left"],display="hidden"}
[keybinds.resize]
r={actions=["resize-pane-left"],display="always"}
[keybinds.move]
m={actions=["move-pane-left"],display="hidden"}
[keybinds.tab]
"1"={actions=[{action="go-to-window",index=1}],display="hidden"}
[keybinds.session]
w={actions=["switch-session",{action="switch-mode",mode="locked"}],display="help"}
[keybinds.history]
y={actions=["copy-history"],display="help"}
"?"={actions=["history-search-backward"],display="hidden"}
"Ctrl b"={actions=["scroll-bottom"],display="always"}
"#,
        );
        assert_eq!(shortcuts.resolve(b'N'), Some(b'c'));
        assert_eq!(shortcuts.resolve(b'D'), Some(b'"'));
        let footer = shortcuts.hints(BindingMode::Normal, false, false);
        assert!(footer.iter().any(|hint| hint.key == HistoryKey::Byte(b'H')));
        for key in *b"ND" {
            assert!(!footer.iter().any(|hint| hint.key == HistoryKey::Byte(key)));
        }
        let help = shortcuts.hints(BindingMode::Normal, false, true);
        assert!(help.iter().any(|hint| hint.key == HistoryKey::Byte(b'N')));
        assert!(!help.iter().any(|hint| hint.key == HistoryKey::Byte(b'D')));
        assert!(shortcuts.pane_binding(b'h').is_some());
        assert!(shortcuts.pane_arrow_binding(Direction::Left).is_some());
        assert_eq!(shortcuts.pane_key(PaneAction::FocusLeft), None);
        assert!(shortcuts.hints(BindingMode::Pane, false, false).is_empty());
        assert_eq!(shortcuts.hints(BindingMode::Pane, false, true).len(), 1);
        assert_eq!(shortcuts.hints(BindingMode::Resize, false, false).len(), 1);
        assert!(shortcuts.hints(BindingMode::Move, false, true).is_empty());
        assert!(shortcuts.hints(BindingMode::Tab, false, true).is_empty());
        assert!(
            shortcuts
                .hints(BindingMode::Session, false, true)
                .is_empty()
        );
        assert_eq!(shortcuts.hints(BindingMode::Session, true, true).len(), 1);
        let footer = shortcuts.hints(BindingMode::History, false, false);
        assert_eq!(
            footer,
            vec![BindingHint {
                key: HistoryKey::Byte(2),
                label: "Bottom"
            }]
        );
        assert_eq!(shortcuts.hints(BindingMode::History, false, true).len(), 2);
        assert!(shortcuts.history_binding(HistoryKey::Byte(b'?')).is_some());
    }

    #[test]
    fn history_chains_describe_the_operation_before_bottom_or_mode() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.history]
E={actions=["scroll-bottom",{action="switch-mode",mode="locked"},"edit-history"],display="always"}
y={actions=["scroll-bottom","copy-last-output",{action="switch-mode",mode="normal"}],display="help"}
e={actions=["history-selection-next-word"],display="always"}
"#,
        );
        assert_eq!(
            shortcuts.binding_label(BindingMode::History, HistoryKey::Byte(b'E'), false),
            Some("Edit history")
        );
        assert_eq!(
            shortcuts.binding_label(BindingMode::History, HistoryKey::Byte(b'y'), false),
            Some("Copy output")
        );
        assert_eq!(
            shortcuts.binding_label(BindingMode::History, HistoryKey::Byte(b'e'), false),
            Some("Next word")
        );
    }

    #[test]
    fn ignored_actions_do_not_override_a_supported_physical_alias_display() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.normal]
"Ctrl m"={actions=[{action="switch-mode",mode="move"}],display="help"}
enter={actions=[{action="switch-mode",mode="scroll"}],display="always"}
"Ctrl -"={actions=["future"],display="hidden"}
"#,
        );
        assert!(shortcuts.enters_move(13));
        assert_eq!(
            shortcuts.display(BindingMode::Normal, HistoryKey::Byte(13)),
            BindingDisplay::Help
        );
        assert!(
            shortcuts
                .hints(BindingMode::Normal, false, true)
                .iter()
                .any(|hint| hint.key == HistoryKey::Byte(13) && hint.label == "Move")
        );
        assert!(
            !shortcuts
                .hints(BindingMode::Normal, false, false)
                .iter()
                .any(|hint| hint.key == HistoryKey::Byte(13))
        );
    }

    #[test]
    fn same_visibility_for_physical_aliases_is_coalesced() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.pane]
"Ctrl m"={actions=[{action="switch-mode",mode="move"}],display="help"}
enter={actions=[{action="switch-mode",mode="locked"}],display="help"}
"#,
        );
        assert_eq!(
            shortcuts.hints(BindingMode::Pane, false, true),
            vec![BindingHint {
                key: HistoryKey::Byte(13),
                label: "Move"
            }]
        );
        assert!(shortcuts.hints(BindingMode::Pane, false, false).is_empty());
    }

    #[test]
    fn display_only_overrides_defaults_and_rejects_inactive_keys() {
        let shortcuts = Shortcuts::test_from_config(
            r#"
[keybinds.locked]
"Ctrl b"={display="hidden"}
[keybinds.normal]
c={display="help-menu"}
[keybinds.history]
e={display="never"}
"#,
        );
        assert_eq!(shortcuts.locked_entry_key(), 2);
        assert!(shortcuts.hints(BindingMode::Locked, false, true).is_empty());
        assert_eq!(shortcuts.resolve(b'c'), Some(b'c'));
        assert_eq!(
            shortcuts.display(BindingMode::Normal, HistoryKey::Byte(b'c')),
            BindingDisplay::Help
        );
        assert_eq!(
            shortcuts.display(BindingMode::History, HistoryKey::Byte(b'e')),
            BindingDisplay::Hidden
        );
        assert!(
            parse_config("clear_defaults=true\n[keybinds.normal]\nc={display='hidden'}").is_err()
        );
        assert!(parse_config("[keybinds.normal]\nF={display='hidden'}").is_err());
    }

    #[test]
    fn invalid_display_values_and_alias_collisions_are_errors() {
        for mode in [
            "locked", "normal", "pane", "resize", "move", "tab", "session", "history",
        ] {
            for value in ["'sometimes'", "true", "1", "[]", "{}"] {
                let error =
                    parse_config(&format!("[keybinds.{mode}]\nc={{display={value}}}")).unwrap_err();
                assert!(
                    error.contains("display must be always, help or hidden"),
                    "{error}"
                );
            }
        }
        assert!(
            parse_config("[keybinds.history]\nenter={display='help'}\n'Ctrl m'={display='hidden'}")
                .unwrap_err()
                .contains("duplicate")
        );
    }
}
