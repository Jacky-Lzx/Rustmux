//! Command-line definitions for local and persistent sessions.

use clap::{Parser, Subcommand};

use crate::session::SessionName;

#[derive(Debug, Parser)]
#[command(name = "rustmux", version, about = "A small terminal multiplexer")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Command {
    /// Create a detached named session and attach to it.
    New {
        /// Session name: ASCII letters, numbers, '-' or '_'.
        name: SessionName,
    },
    /// Attach to an existing named session.
    Attach {
        /// Existing session name.
        name: SessionName,
    },
    /// List named session endpoints.
    List,
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
                name: SessionName::new("work").unwrap()
            })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "attach", "work-2"])
                .unwrap()
                .command,
            Some(Command::Attach {
                name: SessionName::new("work-2").unwrap()
            })
        );
        assert_eq!(
            Cli::try_parse_from(["rustmux", "list"]).unwrap().command,
            Some(Command::List)
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
    fn clap_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }
}
