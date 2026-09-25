//! Optional installed-application compatibility tests.
//!
//! Keep new opt-in compatibility checks in this test target so `cargo compat`
//! runs all of them without changing its alias. Each test must use `#[ignore]`
//! to stay out of the regular test suite.

/// Installed-Yazi preview smoke, excluded from regular CI.
#[test]
#[ignore = "requires an installed Yazi; run with cargo compat"]
fn installed_yazi_previews_png_through_rustmux() {
    let output = std::process::Command::new("python3")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/yazi_compat.py"))
        .arg(env!("CARGO_BIN_EXE_rustmux"))
        .output()
        .expect("Python 3 is required for the optional Yazi PTY smoke test");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}
