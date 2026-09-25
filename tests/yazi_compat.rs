/// Optional installed-Yazi compatibility smoke. It is intentionally excluded
/// from regular CI because Yazi is not a project dependency.
#[test]
#[ignore = "requires an installed Yazi; run with cargo test --test yazi_compat -- --ignored"]
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
