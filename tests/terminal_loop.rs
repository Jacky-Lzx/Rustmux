fn run_scenario(name: &str) {
    let script = format!(
        "{}/tests/terminal_loop_{name}.py",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_rustmux"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("Python 3 is required for the nested-PTY integration harness");
    assert!(
        output.status.success(),
        "{name} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn graphics() {
    run_scenario("graphics");
}

#[test]
fn input() {
    run_scenario("input");
}

#[test]
fn notifications() {
    run_scenario("notifications");
}

#[test]
fn mouse_lifecycle() {
    run_scenario("mouse_lifecycle");
}

#[test]
fn windows() {
    run_scenario("windows");
}

#[test]
fn panes() {
    run_scenario("panes");
}

#[test]
fn history() {
    run_scenario("history");
}

#[test]
fn history_mode() {
    run_scenario("history_mode");
}

#[test]
fn sessions() {
    run_scenario("sessions");
}

#[test]
fn snapshots() {
    run_scenario("snapshots");
}

#[test]
fn frame_reconstruction() {
    run_scenario("frames");
}

#[test]
fn input_backpressure() {
    run_scenario("writes");
}

#[test]
fn config() {
    run_scenario("config");
}

#[test]
fn colors() {
    run_scenario("colors");
}
