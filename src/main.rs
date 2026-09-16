use std::process::ExitCode;

fn main() -> ExitCode {
    let shell = match rustmux::config::shell() {
        Ok(shell) => shell,
        Err(error) => {
            eprintln!("rustmux: {error}");
            return ExitCode::FAILURE;
        }
    };
    match rustmux::terminal::run(&shell) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("rustmux: {error}");
            ExitCode::FAILURE
        }
    }
}
