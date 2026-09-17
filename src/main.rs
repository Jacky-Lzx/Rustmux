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
    match command {
        None => {
            let shell = rustmux::config::shell()?;
            rustmux::terminal::run(&shell).map_err(|error| error.to_string())
        }
        Some(rustmux::cli::Command::New { name }) => {
            let shell = rustmux::config::shell()?;
            rustmux::session::supervisor::create(&name, &shell).map_err(|error| error.to_string())
        }
        Some(rustmux::cli::Command::Attach { name }) => {
            rustmux::session::supervisor::attach(&name).map_err(|error| error.to_string())
        }
        Some(rustmux::cli::Command::List) => {
            for name in rustmux::session::list().map_err(|error| error.to_string())? {
                println!("{name}");
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
}
