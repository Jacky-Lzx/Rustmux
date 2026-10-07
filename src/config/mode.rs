//! Default input mode for a new attachment or a runtime reset.
use super::Shortcuts;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultMode {
    #[default]
    Locked,
    Normal,
    Pane,
    Resize,
    Move,
    Tab,
    Session,
}

impl DefaultMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::Normal => "normal",
            Self::Pane => "pane",
            Self::Resize => "resize",
            Self::Move => "move",
            Self::Tab => "tab",
            Self::Session => "session",
        }
    }

    pub(super) fn parse(value: Option<&toml::Value>, shortcuts: Shortcuts) -> Result<Self, String> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let name = value.as_str().ok_or("default_mode must be a string")?;
        let mode = match name.trim().to_ascii_lowercase().as_str() {
            "locked" => Self::Locked,
            "normal" => Self::Normal,
            "pane" => Self::Pane,
            "resize" => Self::Resize,
            "move" => Self::Move,
            "tab" => Self::Tab,
            "session" => Self::Session,
            _ => {
                return Err(
                    "default_mode must be locked, normal, pane, resize, move, tab or session"
                        .into(),
                );
            }
        };
        // With implicit bindings disabled, starting in an ignored or empty
        // keymap would consume input without offering any configured actions.
        let configured = match mode {
            Self::Locked => shortcuts.locked_configured,
            Self::Normal => {
                shortcuts.normal_action_len != 0
                    || shortcuts.normal_exit_len != 0
                    || shortcuts.legacy_configured.contains(&true)
                    || [
                        shortcuts.pane_enter,
                        shortcuts.resize_enter,
                        shortcuts.move_enter,
                        shortcuts.tab_enter,
                        shortcuts.session_enter,
                    ]
                    .iter()
                    .any(Option::is_some)
            }
            Self::Pane => {
                shortcuts.pane_binding_len != 0 || shortcuts.pane_arrows.iter().any(Option::is_some)
            }
            Self::Resize => {
                shortcuts.resize_binding_len != 0
                    || shortcuts.resize_arrows.iter().any(Option::is_some)
            }
            Self::Move => {
                shortcuts.move_binding_len != 0 || shortcuts.move_arrows.iter().any(Option::is_some)
            }
            Self::Tab => {
                shortcuts.tab_binding_len != 0 || shortcuts.tab_arrows.iter().any(Option::is_some)
            }
            Self::Session => shortcuts.session_binding_len != 0,
        };
        if shortcuts.clear_defaults && !configured {
            return Err(format!(
                "default_mode '{}' requires supported bindings when clear_defaults is true",
                mode.as_str()
            ));
        }
        Ok(mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{parse_config, resolve_config};

    #[test]
    fn supported_defaults_normalize_and_resolve() {
        assert_eq!(
            resolve_config(parse_config("").unwrap()).default_mode(),
            DefaultMode::Locked
        );
        for mode in [
            DefaultMode::Locked,
            DefaultMode::Normal,
            DefaultMode::Pane,
            DefaultMode::Resize,
            DefaultMode::Move,
            DefaultMode::Tab,
            DefaultMode::Session,
        ] {
            let source = format!("default_mode = ' {} '", mode.as_str().to_ascii_uppercase());
            assert_eq!(
                resolve_config(parse_config(&source).unwrap()).default_mode(),
                mode
            );
        }
    }

    #[test]
    fn invalid_default_modes_fail_instead_of_being_ignored() {
        for value in [
            "1",
            "true",
            "[]",
            "{}",
            "''",
            "'history'",
            "'scroll'",
            "'custom'",
            "'normal-mode'",
        ] {
            assert!(
                parse_config(&format!("default_mode={value}"))
                    .unwrap_err()
                    .contains("default_mode")
            );
        }
    }

    #[test]
    fn cleared_defaults_require_an_effective_binding_even_when_hidden() {
        const ENTRY: &str = "clear_defaults=true\n[keybinds.locked]\n'Ctrl b'={actions=[{action='switch-mode',mode='normal'}]}\n";
        for (mode, key, action) in [
            ("normal", "q", "{action='switch-mode',mode='locked'}"),
            ("pane", "up", "'focus-up'"),
            ("resize", "left", "'resize-pane-left'"),
            ("move", "right", "'move-pane-right'"),
            ("tab", "n", "'new-window'"),
            ("session", "d", "'detach'"),
        ] {
            let source = format!(
                "default_mode='{mode}'\n{ENTRY}[keybinds.{mode}]\n{key}={{actions=[{action}],display='hidden'}}"
            );
            assert!(parse_config(&source).is_ok(), "{source}");
            let empty = format!("default_mode='{mode}'\n{ENTRY}[keybinds.{mode}]");
            assert!(
                parse_config(&empty)
                    .unwrap_err()
                    .contains("requires supported bindings")
            );
            let ignored = format!("{empty}\nx={{actions=['toggle-floating-terminal']}}");
            assert!(
                parse_config(&ignored)
                    .unwrap_err()
                    .contains("requires supported bindings")
            );
        }
        assert!(parse_config(&format!("default_mode='locked'\n{ENTRY}")).is_ok());
    }
}
