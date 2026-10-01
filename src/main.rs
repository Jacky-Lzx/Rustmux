use std::{
    io::{self, BufRead, Write},
    process::ExitCode,
};

use clap::Parser;

fn main() -> ExitCode {
    match execute(rustmux::cli::Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("rustmux: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: rustmux::cli::Cli) -> Result<u8, String> {
    let rustmux::cli::Cli { command, config } = cli;
    let config_path = config.as_deref();
    if std::env::var_os(rustmux::RUSTMUX_ENV).is_some() && starts_interactive_session(&command) {
        return Err("nested Rustmux sessions are not supported".to_owned());
    }
    match command {
        Some(rustmux::cli::Command::DefaultConfig) => {
            print!("{}", rustmux::config::default_config());
            Ok(0)
        }
        Some(rustmux::cli::Command::CheckConfig { toml, strict }) => {
            let report = rustmux::config::inspect(config_path)?;
            if toml {
                print!(
                    "{}",
                    toml::to_string(&report).map_err(|error| error.to_string())?
                );
            } else {
                println!(
                    "configuration valid: {} ({})",
                    report.path,
                    if report.file_loaded {
                        "file"
                    } else {
                        "built-in defaults"
                    }
                );
                println!("shell: {} ({})", report.settings.shell, report.shell_source);
                println!("scrollback_lines: {}", report.settings.scrollback_lines);
                println!("remain_on_exit: {}", report.settings.remain_on_exit);
                println!("clear_defaults: {}", report.settings.clear_defaults);
                println!(
                    "autosave_interval_seconds: {}",
                    report.settings.autosave_interval_seconds
                );
                println!("save_scrollback: {}", report.settings.save_scrollback);
                println!(
                    "save_scrollback_colors: {}",
                    report.settings.save_scrollback_colors
                );
                println!("long_command_bell: {}", report.settings.long_command_bell);
                println!(
                    "command_duration_seconds: {}",
                    report.settings.command_duration_seconds
                );
                for warning in &report.warnings {
                    eprintln!("rustmux: warning: {warning}");
                }
            }
            Ok(u8::from(strict && !report.warnings.is_empty()))
        }
        Some(rustmux::cli::Command::Control(command)) => {
            print!("{}", command.run().map_err(|error| error.to_string())?);
            Ok(0)
        }
        None => {
            let config = rustmux::config::load_with_path(config_path)?;
            rustmux::terminal::run_configured(&config).map_err(|error| error.to_string())
        }
        Some(rustmux::cli::Command::New {
            name,
            detached,
            layout,
        }) => {
            let config = rustmux::config::load_with_path(config_path)?;
            let result = match layout {
                Some(layout) => rustmux::session::supervisor::create_from_layout(
                    &name,
                    &config,
                    detached,
                    config_path,
                    &layout,
                ),
                None => rustmux::session::supervisor::create(&name, &config, detached, config_path),
            };
            result.map_err(|error| error.to_string())
        }
        Some(rustmux::cli::Command::Attach { name }) => match name {
            Some(name) => rustmux::session::supervisor::attach(&name, config_path)
                .map_err(|error| error.to_string()),
            None => rustmux::session::supervisor::choose_and_attach(config_path)
                .map_err(|error| error.to_string()),
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
        Some(rustmux::cli::Command::Save { name }) => {
            rustmux::session::snapshot::save_session(&name).map_err(|error| error.to_string())?;
            println!("saved {name}");
            Ok(0)
        }
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
    let sessions = rustmux::session::list_running().map_err(|error| error.to_string())?;
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
                layout: None,
            }),
            Some(Command::Attach {
                name: Some(name.clone()),
            }),
            Some(Command::New {
                name: name.clone(),
                detached: false,
                layout: Some("project.toml".into()),
            }),
        ] {
            assert!(starts_interactive_session(&command));
        }
        for command in [
            Some(Command::New {
                name: name.clone(),
                detached: true,
                layout: None,
            }),
            Some(Command::List { long: false }),
            Some(Command::Kill { name: name.clone() }),
            Some(Command::KillAll { yes: true }),
        ] {
            assert!(!starts_interactive_session(&command));
        }
    }
}
