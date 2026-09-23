use std::{
    io::{self, BufRead, Write},
    process::ExitCode,
};

use clap::Parser;

fn main() -> ExitCode {
    match execute(rustmux::cli::Cli::parse().command) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("rustmux: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(command: Option<rustmux::cli::Command>) -> Result<u8, String> {
    if std::env::var_os(rustmux::RUSTMUX_ENV).is_some() && starts_interactive_session(&command) {
        return Err("nested Rustmux sessions are not supported".to_owned());
    }
    match command {
        None => {
            let config = rustmux::config::load()?;
            rustmux::terminal::run(
                config.shell(),
                config.notifications(),
                config.scrollback_lines(),
                config.shortcuts(),
            )
            .map_err(|error| error.to_string())
        }
        Some(rustmux::cli::Command::New { name, detached }) => {
            let config = rustmux::config::load()?;
            rustmux::session::supervisor::create(
                &name,
                config.shell(),
                config.notifications(),
                config.scrollback_lines(),
                config.shortcuts(),
                detached,
            )
            .map_err(|error| error.to_string())
        }
        Some(rustmux::cli::Command::Attach { name }) => match name {
            Some(name) => {
                rustmux::session::supervisor::attach(&name).map_err(|error| error.to_string())
            }
            None => {
                rustmux::session::supervisor::choose_and_attach().map_err(|error| error.to_string())
            }
        },
        Some(rustmux::cli::Command::List { long }) => {
            if long {
                print!(
                    "{}",
                    rustmux::session::format_list().map_err(|error| error.to_string())?
                );
            } else {
                for name in rustmux::session::list().map_err(|error| error.to_string())? {
                    println!("{name}");
                }
            }
            Ok(0)
        }
        Some(rustmux::cli::Command::Kill { name }) => {
            rustmux::session::supervisor::kill(&name).map_err(|error| error.to_string())?;
            Ok(0)
        }
        Some(rustmux::cli::Command::KillAll { yes }) => kill_all(yes),
    }
}

fn starts_interactive_session(command: &Option<rustmux::cli::Command>) -> bool {
    matches!(
        command,
        None | Some(rustmux::cli::Command::Attach { .. })
            | Some(rustmux::cli::Command::New {
                detached: false,
                ..
            })
    )
}

fn kill_all(skip_confirmation: bool) -> Result<u8, String> {
    let sessions = rustmux::session::list().map_err(|error| error.to_string())?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(0);
    }

    if !skip_confirmation {
        let mut input = io::stdin().lock();
        let mut error = io::stderr().lock();
        if !confirm_kill_all(&mut input, &mut error, sessions.len())
            .map_err(|error| error.to_string())?
        {
            println!("aborted");
            return Ok(0);
        }
    }

    let count = sessions.len();
    terminate_all(&sessions, rustmux::session::supervisor::kill)?;
    println!("killed {count} session(s)");
    Ok(0)
}

fn terminate_all<E: std::fmt::Display>(
    sessions: &[rustmux::session::SessionName],
    mut terminate: impl FnMut(&rustmux::session::SessionName) -> Result<(), E>,
) -> Result<(), String> {
    let failures: Vec<_> = sessions
        .iter()
        .filter_map(|name| {
            terminate(name)
                .err()
                .map(|error| format!("{name}: {error}"))
        })
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "failed to terminate {} session(s): {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

fn confirm_kill_all(
    input: &mut impl BufRead,
    output: &mut impl Write,
    count: usize,
) -> io::Result<bool> {
    write!(output, "Kill all {count} running session(s)? [y/N] ")?;
    output.flush()?;
    let mut response = String::new();
    input.read_line(&mut response)?;
    Ok(matches!(
        response.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_all_confirmation_accepts_only_explicit_yes() {
        for response in ["y\n", "YES\n", " yes \n"] {
            let mut output = Vec::new();
            assert!(confirm_kill_all(&mut response.as_bytes(), &mut output, 2).unwrap());
            assert_eq!(output, b"Kill all 2 running session(s)? [y/N] ");
        }
        for response in ["\n", "n\n", "anything\n", ""] {
            assert!(
                !confirm_kill_all(&mut response.as_bytes(), &mut Vec::new(), 2).unwrap(),
                "accepted {response:?}"
            );
        }
    }

    #[test]
    fn batch_termination_attempts_every_session_and_reports_each_failure() {
        let sessions = [
            rustmux::session::SessionName::new("one").unwrap(),
            rustmux::session::SessionName::new("two").unwrap(),
            rustmux::session::SessionName::new("three").unwrap(),
        ];
        let mut visited = Vec::new();
        let error = terminate_all(&sessions, |name| {
            visited.push(name.as_str().to_owned());
            match name.as_str() {
                "one" => Err("already gone"),
                "three" => Err("timed out"),
                _ => Ok(()),
            }
        })
        .unwrap_err();

        assert_eq!(visited, ["one", "two", "three"]);
        assert_eq!(
            error,
            "failed to terminate 2 session(s): one: already gone; three: timed out"
        );
    }

    #[test]
    fn nested_guard_applies_only_to_interactive_session_entry() {
        use rustmux::{cli::Command, session::SessionName};

        let name = SessionName::new("work").unwrap();
        for command in [
            None,
            Some(Command::New {
                name: name.clone(),
                detached: false,
            }),
            Some(Command::Attach {
                name: Some(name.clone()),
            }),
        ] {
            assert!(starts_interactive_session(&command));
        }
        for command in [
            Some(Command::New {
                name: name.clone(),
                detached: true,
            }),
            Some(Command::List { long: false }),
            Some(Command::Kill { name: name.clone() }),
            Some(Command::KillAll { yes: true }),
        ] {
            assert!(!starts_interactive_session(&command));
        }
    }
}
