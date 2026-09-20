//! Load the small configuration surface supported by the human-reviewed track.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_COMMAND_DURATION_SECONDS: u64 = 5;
pub const DEFAULT_SCROLLBACK_LINES: usize = crate::screen::SCROLLBACK_MAX_LINES;

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
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ParsedConfig {
    shell: Option<String>,
    notifications: Notifications,
    scrollback_lines: Option<usize>,
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
    })
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
