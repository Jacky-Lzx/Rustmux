//! Optional application and large-PTY compatibility tests.
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

/// Installed-kitten Unicode-placeholder smoke, excluded from regular CI.
#[test]
#[ignore = "requires an installed kitten; run with cargo compat"]
fn installed_kitten_icat_displays_png_through_rustmux() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/kitten_compat.py"
        ))
        .arg(env!("CARGO_BIN_EXE_rustmux"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("Python 3 is required for the optional kitten PTY smoke test");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}

/// Large synthetic PNG through a named PTY, excluded from regular CI because
/// it captures and verifies tens of MiB of outer-terminal graphics output.
#[test]
#[ignore = "large PTY graphics smoke; run with cargo compat"]
fn large_kitty_overlay_tiles_survive_session_bridge() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/kitty_large_overlay_compat.py"
        ))
        .arg(env!("CARGO_BIN_EXE_rustmux"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("Python 3 is required for the optional large Kitty PTY smoke test");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}
