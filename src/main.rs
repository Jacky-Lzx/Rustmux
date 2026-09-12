use std::process::ExitCode;

fn main() -> ExitCode {
    let shell = std::env::var_os("RUSTMUX_SHELL")
        .or_else(|| std::env::var_os("SHELL"))
        .unwrap_or_else(|| "/bin/sh".into());
    match rustmux::terminal::run(&shell) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("rustmux: {error}");
            ExitCode::FAILURE
        }
    }
}
