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
fn terminal_device() {
    run_scenario("device");
}

#[test]
fn window_rename() {
    run_scenario("window_rename");
}

#[test]
fn pane_resize() {
    run_scenario("pane_resize");
}

#[test]
fn pane_startup() {
    run_scenario("pane_startup");
}

#[test]
fn pane_close() {
    run_scenario("pane_close");
}

#[test]
fn window_close() {
    run_scenario("window_close");
}

#[test]
fn pane_zoom() {
    run_scenario("pane_zoom");
}

#[test]
fn window_move() {
    run_scenario("window_move");
}

#[test]
fn pane_swap() {
    run_scenario("pane_swap");
}

#[test]
fn pane_move() {
    run_scenario("pane_move");
}

#[test]
fn directional_focus() {
    run_scenario("directional_focus");
}

#[test]
fn notifications() {
    run_scenario("notifications");
}

#[test]
fn desktop_notifications() {
    run_scenario("desktop_notifications");
}

#[test]
fn notification_filter() {
    run_scenario("notification_filter");
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
fn control() {
    run_scenario("control");
}

#[test]
fn project() {
    run_scenario("project");
}

#[test]
fn lifecycle() {
    run_scenario("lifecycle");
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

#[test]
fn output() {
    run_scenario("output");
}

#[test]
fn reload() {
    run_scenario("reload");
}

#[test]
fn manager() {
    run_scenario("manager");
}

#[test]
fn saved_delete() {
    run_scenario("saved_delete");
}

#[test]
fn saved_rename() {
    run_scenario("saved_rename");
}

#[test]
fn live_rename() {
    run_scenario("live_rename");
}

#[test]
fn disconnect() {
    run_scenario("disconnect");
}

#[test]
fn attach_create() {
    run_scenario("attach_create");
}

#[test]
fn focus_control() {
    run_scenario("focus_control");
}
