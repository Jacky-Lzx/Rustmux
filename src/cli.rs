//! Command-line definitions for local and persistent sessions.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::session::SessionName;

#[derive(Debug, Parser)]
#[command(name = "rustmux", version, about = "A small terminal multiplexer")]
pub struct Cli {
    /// Load configuration from PATH instead of the default config file.
    #[arg(short = 'c', long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    #[command(flatten)]
    Control(crate::control::Command),
    /// Print a TOML template of built-in settings without loading user configuration.
    #[command(name = "default-config", visible_alias = "dump-config")]
    DefaultConfig,
    /// Validate the selected config and report settings used by newly started sessions.
    CheckConfig {
        /// Print a machine-readable inspection report.
        #[arg(long)]
        toml: bool,
        /// Treat ignored fields and bindings as errors.
        #[arg(long)]
        strict: bool,
    },
    /// Save a running session's layout and optional history to disk.
    #[command(name = "save-session", visible_alias = "save")]
    Save { name: SessionName },
    /// Create a named session and attach to it.
    New {
        /// Session name: ASCII letters, numbers, '-' or '_'.
        name: SessionName,
        /// Leave the new session running without attaching this terminal.
        #[arg(short, long)]
        detached: bool,
        /// Start from a project TOML file instead of the saved workspace.
        #[arg(long)]
        layout: Option<PathBuf>,
    },
    /// Attach to a named session, or choose running and saved sessions.
    Attach {
        /// Session name; omit to choose running or saved sessions.
        name: Option<SessionName>,
        /// Create or restore the named session when it is not running.
        #[arg(long, requires = "name")]
        create: bool,
    },
    /// List running sessions and saved workspaces.
    #[command(visible_alias = "ls")]
    List {
        /// Show connection state, server PID and last connection time.
        #[arg(short, long)]
        long: bool,
    },
    /// Terminate a named session.
    Kill {
        /// Existing session name.
        name: SessionName,
    },
    /// Terminate every running named session.
    #[command(visible_alias = "ka")]
    KillAll {
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        yes: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn parses_local_new_attach_list_and_kill() {
        assert!(Cli::try_parse_from(["rustmux"]).unwrap().command.is_none());
        assert_eq!(
            Cli::try_parse_from(["rustmux", "new", "work"])
                .unwrap()
                .command,
            Some(Command::New {
                name: SessionName::new("work").unwrap(),
                detached: false,
                layout: None,
            })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "new", "work", "--detached"])
                .unwrap()
                .command,
            Some(Command::New {
                name: SessionName::new("work").unwrap(),
                detached: true,
                layout: None,
            })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "attach", "work-2"])
                .unwrap()
                .command,
            Some(Command::Attach {
                name: Some(SessionName::new("work-2").unwrap()),
                create: false,
            })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "attach"]).unwrap().command,
            Some(Command::Attach {
                name: None,
                create: false,
            })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "list"]).unwrap().command,
            Some(Command::List { long: false })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "ls", "--long"])
                .unwrap()
                .command,
            Some(Command::List { long: true })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "kill", "work_2"])
                .unwrap()
                .command,
            Some(Command::Kill {
                name: SessionName::new("work_2").unwrap()
            })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "ka", "--yes"])
                .unwrap()
                .command,
            Some(Command::KillAll { yes: true })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "kill-all"])
                .unwrap()
                .command,
            Some(Command::KillAll { yes: false })
        );
    }

    #[test]
    fn clap_rejects_unknown_missing_extra_and_invalid_names() {
        for arguments in [
            &["rustmux", "unknown"][..],
            &["rustmux", "new"][..],
            &["rustmux", "attach", "one", "two"][..],
            &["rustmux", "list", "extra"][..],
            &["rustmux", "kill"][..],
            &["rustmux", "kill", "one", "two"][..],
            &["rustmux", "kill-all", "extra"][..],
            &["rustmux", "new", "../escape"][..],
        ] {
            assert!(Cli::try_parse_from(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn config_path_is_optional_and_available_before_or_after_subcommands() {
        assert!(Cli::try_parse_from(["rustmux"]).unwrap().config.is_none());
        for arguments in [
            vec!["rustmux", "--config", "configs/dev config.toml"],
            vec!["rustmux", "-c", "configs/dev config.toml", "new", "work"],
            vec![
                "rustmux",
                "new",
                "work",
                "--config",
                "configs/dev config.toml",
            ],
            vec!["rustmux", "attach", "work", "-c", "configs/dev config.toml"],
        ] {
            assert_eq!(
                Cli::try_parse_from(arguments).unwrap().config,
                Some(PathBuf::from("configs/dev config.toml"))
            );
        }
        assert!(Cli::try_parse_from(["rustmux", "--config"]).is_err());
    }

    #[test]
    fn clap_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn attach_create_requires_a_name_and_preserves_the_config_short_option() {
        for arguments in [
            vec!["rustmux", "attach", "work", "--create", "-c", "dev.toml"],
            vec!["rustmux", "-c", "dev.toml", "attach", "--create", "work"],
        ] {
            let cli = Cli::try_parse_from(arguments).unwrap();
            assert_eq!(cli.config, Some(PathBuf::from("dev.toml")));
            assert_eq!(
                cli.command,
                Some(Command::Attach {
                    name: Some(SessionName::new("work").unwrap()),
                    create: true,
                })
            );
        }
        assert!(Cli::try_parse_from(["rustmux", "attach", "--create"]).is_err());
        assert!(Cli::try_parse_from(["rustmux", "attach", "--create", "../bad"]).is_err());
        assert!(Cli::try_parse_from(["rustmux", "attach", "work", "-c"]).is_err());
    }

    #[test]
    fn save_command_requires_a_valid_name_and_does_not_attach() {
        for action in ["save", "save-session"] {
            assert_eq!(
                Cli::try_parse_from(["rustmux", action, "work"])
                    .unwrap()
                    .command,
                Some(Command::Save {
                    name: SessionName::new("work").unwrap()
                })
            );
            assert!(Cli::try_parse_from(["rustmux", action]).is_err());
            assert!(Cli::try_parse_from(["rustmux", action, "../bad"]).is_err());
        }
    }
}
