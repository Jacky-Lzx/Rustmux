use std::process::ExitCode;

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
    }
}
