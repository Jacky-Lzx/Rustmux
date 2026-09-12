#[test]
fn interactive_terminal_and_restoration() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/terminal_loop.py"
        ))
        .arg(env!("CARGO_BIN_EXE_rustmux"))
        .output()
        .expect("Python 3 is required for the nested-PTY integration harness");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
