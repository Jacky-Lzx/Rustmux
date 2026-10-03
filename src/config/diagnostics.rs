//! Inspect the same source and parser used at startup; never start a session.
use super::*;
use serde::Serialize;

/// Selected source and effective scalar settings for a newly started session.
/// Keybindings are validated but are not exported as a flattened configuration.
#[derive(Debug, Serialize)]
pub struct Inspection {
    pub path: String,
    pub explicit: bool,
    pub file_loaded: bool,
    pub shell_source: &'static str,
    pub warnings: Vec<String>,
    pub settings: Settings,
    pub session_manager: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct Settings {
    pub theme: std::collections::BTreeMap<String, String>,
    pub shell: String,
    pub scrollback_lines: usize,
    pub remain_on_exit: bool,
    pub mouse_hover_cursor: bool,
    pub clear_defaults: bool,
    pub autosave_interval_seconds: u64,
    pub save_scrollback: bool,
    pub save_scrollback_colors: bool,
    pub long_command_bell: bool,
    pub notifications_enabled: bool,
    pub desktop_notifications: bool,
    pub notification_excluded_applications: Vec<String>,
    pub command_duration_seconds: u64,
}

/// Validate one read of the selected source and expose the resulting settings.
/// Missing discovered files use defaults; explicitly selected files must exist.
pub fn inspect(path: Option<&Path>) -> Result<Inspection, String> {
    let selected = path.map(Path::to_owned).unwrap_or_else(config_path);
    let source = read_config(&selected, path.is_none())?;
    let parsed = match &source {
        Some(source) => parse_config(source)
            .map_err(|error| format!("invalid {}: {error}", selected.display()))?,
        None => ParsedConfig::default(),
    };
    let shell_source = if env::var_os("RUSTMUX_SHELL").is_some_and(|value| !value.is_empty()) {
        "RUSTMUX_SHELL"
    } else if parsed.shell.is_some() {
        "config"
    } else if env::var_os("SHELL").is_some_and(|value| !value.is_empty()) {
        "SHELL"
    } else {
        "fallback"
    };
    let warnings = source.as_deref().map(ignored_options).unwrap_or_default();
    let config = resolve_config(parsed);
    Ok(Inspection {
        path: selected.to_string_lossy().into_owned(),
        explicit: path.is_some(),
        file_loaded: source.is_some(),
        shell_source,
        warnings,
        settings: Settings::from(&config),
        session_manager: config.manager.report(),
    })
}

impl From<&Config> for Settings {
    fn from(config: &Config) -> Self {
        Self {
            theme: config.theme.report(),
            shell: config.shell.to_string_lossy().into_owned(),
            scrollback_lines: config.scrollback_lines,
            remain_on_exit: config.remain_on_exit,
            mouse_hover_cursor: config.mouse_hover_cursor,
            clear_defaults: config.shortcuts.clear_defaults,
            autosave_interval_seconds: config.persistence.autosave_interval_seconds,
            save_scrollback: config.persistence.save_scrollback,
            save_scrollback_colors: config.persistence.save_scrollback_colors,
            long_command_bell: config.notifications.long_command_bell,
            notifications_enabled: config.notifications.enabled,
            desktop_notifications: config.notifications.desktop,
            notification_excluded_applications: config.notifications.exclude_applications.to_vec(),
            command_duration_seconds: config.notifications.command_duration.as_secs(),
        }
    }
}

fn ignored_options(source: &str) -> Vec<String> {
    // The caller already validated this exact source with parse_config.
    let table = source.parse::<toml::Table>().expect("validated config");
    let mut warnings = Vec::new();
    for key in table.keys() {
        if !matches!(
            key.as_str(),
            "shell"
                | "scrollback_lines"
                | "remain_on_exit"
                | "mouse_hover_cursor"
                | "clear_defaults"
                | "autosave_interval_seconds"
                | "save_scrollback"
                | "save_scrollback_colors"
                | "notifications"
                | "shortcuts"
                | "keybinds"
                | "session_manager"
                | "theme"
        ) {
            warnings.push(format!("ignored top-level option {key:?}"));
        }
    }
    if let Some(manager) = table.get("session_manager").and_then(toml::Value::as_table) {
        for name in manager.keys() {
            if !manager::Action::ALL
                .iter()
                .any(|(_, action)| *action == name)
            {
                warnings.push(format!("ignored session_manager action {name:?}"));
            }
        }
    }
    if let Some(notifications) = table.get("notifications").and_then(toml::Value::as_table) {
        for key in notifications.keys() {
            if !matches!(
                key.as_str(),
                "enabled"
                    | "desktop"
                    | "long_command_bell"
                    | "command_duration_seconds"
                    | "exclude_applications"
            ) {
                warnings.push(format!("ignored notifications option {key:?}"));
            }
        }
    }
    if let Some(modes) = table.get("keybinds").and_then(toml::Value::as_table) {
        for (mode, bindings) in modes {
            if !matches!(
                mode.as_str(),
                "locked" | "normal" | "pane" | "resize" | "move" | "tab" | "session" | "history"
            ) {
                warnings.push(format!("ignored keybinds mode {mode:?}"));
                continue;
            }
            for (key, binding) in bindings.as_table().expect("validated supported mode") {
                // Use the real binding parser to recognize complete supported chains.
                // Disable defaults and use a nonbinding entry sentinel, avoiding conflicts
                // with independent custom bindings while probing a single binding.
                let baseline = Shortcuts {
                    clear_defaults: true,
                    locked_enter: 0,
                    ..Shortcuts::default()
                };
                let one = toml::Value::Table(toml::Table::from_iter([(
                    mode.clone(),
                    toml::Value::Table(toml::Table::from_iter([(key.clone(), binding.clone())])),
                )]));
                if parse_keybinds(Some(&one), baseline).is_ok_and(|result| result == baseline) {
                    warnings.push(format!("ignored binding keybinds.{mode}.{key:?}"));
                }
                if let Some(fields) = binding.as_table() {
                    for field in fields.keys() {
                        if !matches!(field.as_str(), "actions" | "display") {
                            warnings.push(format!(
                                "ignored binding option keybinds.{mode}.{key:?}.{field:?}"
                            ));
                        }
                    }
                }
            }
        }
    }
    warnings.sort();
    warnings
}

/// Reusable scalar-default template. Binding defaults remain implicit so exporting
/// and loading this file preserves the parser's built-in keybinding behavior.
/// Shell selection stays dynamic through RUSTMUX_SHELL, SHELL and /bin/sh.
pub fn default_config() -> String {
    format!(
        r#"# Rustmux main-human built-in settings.
# Shell precedence: RUSTMUX_SHELL, uncommented shell setting, SHELL, /bin/sh.
# shell = "/bin/sh"
scrollback_lines = {DEFAULT_SCROLLBACK_LINES}
remain_on_exit = false
mouse_hover_cursor = false
clear_defaults = false
autosave_interval_seconds = 0
save_scrollback = false
save_scrollback_colors = false

[theme]
preset = "mocha"
# Optional [theme.colors] overrides use exact #RRGGBB values.

[notifications]
enabled = true
desktop = false
long_command_bell = true
command_duration_seconds = {DEFAULT_COMMAND_DURATION_SECONDS}
exclude_applications = ["yazi", "nvim", "lazygit"]

[session_manager]
up = ["k", "up"]
down = ["j", "down"]
search = ["/"]
complete = ["tab"]
open = ["enter"]
create = ["a"]
save = ["Ctrl a"]
# Rename a live or saved workspace.
rename = ["Ctrl r"]
# Detach the selected other session's displayed client; keep its panes running.
disconnect = ["Ctrl x"]
# Press twice to kill a live session or delete a saved-only snapshot.
delete = ["d"]
cancel = ["esc", "q"]
backspace = ["backspace"]

# Keybinding defaults are implicit. Add [shortcuts] or [keybinds.MODE] overrides.
# Default prefix: Ctrl-B; new window: c; split right: %; split down: \".
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hover_option_is_typed_reported_and_recognized_in_strict_diagnostics() {
        assert!(!resolve_config(parse_config("").unwrap()).mouse_hover_cursor());
        let source = "mouse_hover_cursor=true";
        let config = resolve_config(parse_config(source).unwrap());
        assert!(config.mouse_hover_cursor());
        assert!(Settings::from(&config).mouse_hover_cursor);
        assert!(ignored_options(source).is_empty());
        for source in ["mouse_hover_cursor=1", "mouse_hover_cursor='true'"] {
            assert!(
                parse_config(source)
                    .unwrap_err()
                    .contains("must be a boolean")
            );
        }
    }
    #[test]
    fn exported_template_preserves_all_builtin_defaults() {
        assert_eq!(
            resolve_config(parse_config(&default_config()).unwrap()),
            resolve_config(ParsedConfig::default())
        );
        assert!(ignored_options(&default_config()).is_empty());
    }
    #[test]
    fn reports_ignored_fields_modes_and_incomplete_action_chains() {
        let source = r#"
scrolback_lines=0
future_option="light"
[notifications]
long_command_bell=false
typo=true
[keybinds.search]
x={actions=["search"]}
[keybinds.normal]
y={actions=["new-window"]}
N={actions=["new-window",{action="switch-mode",mode="locked"}],display="always",typo=true}
[keybinds.pane]
x={actions=["unsupported"]}
"#;
        parse_config(source).unwrap();
        let warnings = ignored_options(source);
        assert_eq!(warnings.len(), 7, "{warnings:?}");
        assert!(warnings.iter().any(|value| value.contains("normal.\"y\"")));
        assert!(warnings.iter().any(|value| value.contains("pane.\"x\"")));
        assert!(
            !warnings
                .iter()
                .any(|value| value == "ignored binding keybinds.normal.\"N\"")
        );
    }
    #[test]
    fn supported_bindings_in_every_mode_have_no_false_warnings() {
        let source = r#"
clear_defaults=true
[keybinds.locked]
"Ctrl a"={actions=[{action="switch-mode",mode="normal"}]}
[keybinds.normal]
"Ctrl b"={actions=[{action="switch-mode",mode="locked"}]}
N={actions=["new-window",{action="switch-mode",mode="locked"}]}
[keybinds.pane]
R={actions=["respawn-pane",{action="switch-mode",mode="locked"}]}
[keybinds.resize]
h={actions=["resize-pane-left"]}
[keybinds.move]
h={actions=["move-pane-left"]}
[keybinds.tab]
n={actions=["next-window"]}
[keybinds.session]
s={actions=["switch-session",{action="switch-mode",mode="locked"}]}
[keybinds.history]
q={actions=[{action="switch-mode",mode="locked"}]}
"#;
        parse_config(source).unwrap();
        assert!(
            ignored_options(source).is_empty(),
            "{:?}",
            ignored_options(source)
        );
    }
}
