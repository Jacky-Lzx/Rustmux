//! Configuration commands work with pipes, without a terminal or session server.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn command(root: &Path, arguments: &[&str]) -> Output {
    base_command(root).args(arguments).output().unwrap()
}
fn base_command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rustmux"));
    command
        .env("XDG_CONFIG_HOME", root)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("HOME", root)
        .env("RUSTMUX", "1") // These read-only commands are allowed inside a pane.
        .env_remove("RUSTMUX_SHELL")
        .env_remove("SHELL");
    command
}
fn report(output: &Output) -> toml::Table {
    toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap()
}
fn settings(report: &toml::Table) -> &toml::Table {
    report["settings"].as_table().unwrap()
}

#[test]
fn exported_defaults_round_trip_and_ignore_active_broken_config() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir(root.join("rustmux")).unwrap();
    fs::write(root.join("rustmux/config.toml"), "shell=[invalid").unwrap();
    let first = command(root, &["config", "default"]);
    let alias = command(root, &["--config", "missing.toml", "config", "dump"]);
    assert!(first.status.success());
    assert_eq!(alias.stdout, first.stdout);
    assert!(alias.status.success());
    let template = root.join("defaults.toml");
    fs::write(&template, &first.stdout).unwrap();
    let checked = command(
        root,
        &[
            "config",
            "check",
            "--config",
            template.to_str().unwrap(),
            "--toml",
            "--strict",
        ],
    );
    assert!(checked.status.success(), "{checked:?}");
    let checked = report(&checked);
    assert!(checked["warnings"].as_array().unwrap().is_empty());
    assert_eq!(
        settings(&checked)["scrollback_lines"].as_integer(),
        Some(1000)
    );
    assert_eq!(
        settings(&checked)["autosave_interval_seconds"].as_integer(),
        Some(0)
    );
    assert_eq!(settings(&checked)["save_scrollback"].as_bool(), Some(false));
    assert_eq!(settings(&checked)["remain_on_exit"].as_bool(), Some(false));
    assert_eq!(settings(&checked)["default_mode"].as_str(), Some("locked"));
    assert_eq!(settings(&checked)["tab_name"].as_str(), Some("application"));
    assert_eq!(
        settings(&checked)["mouse_hover_cursor"].as_bool(),
        Some(false)
    );
    assert_eq!(
        settings(&checked)["idle_frame_coalescing"].as_bool(),
        Some(false)
    );
    assert!(!root.join("state").exists());
    assert_eq!(
        fs::read_to_string(root.join("rustmux/config.toml")).unwrap(),
        "shell=[invalid"
    );
}

#[test]
fn strict_normal_focus_arrows_accept_complete_chains_only() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let config = root.join("normal.toml");
    fs::write(
        &config,
        r#"
clear_defaults=true
default_mode="normal"
[keybinds.locked]
"Ctrl b"={actions=[{action="switch-mode",mode="normal"}]}
[keybinds.normal]
left={actions=["focus-left"],display="always"}
down={actions=["focus-down"],display="help"}
up={actions=["focus-up"],display="hidden"}
right={actions=["focus-right",{action="switch-mode",mode="locked"}]}
"#,
    )
    .unwrap();
    let output = command(
        root,
        &[
            "config",
            "check",
            "--config",
            config.to_str().unwrap(),
            "--toml",
            "--strict",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(report(&output)["warnings"].as_array().unwrap().is_empty());
    for chain in [
        "'focus-left','close-pane'",
        "'focus-left',{action='switch-mode',mode='pane'}",
        "{action='switch-mode',mode='locked'},'focus-left'",
    ] {
        fs::write(
            &config,
            format!("[keybinds.normal]\nleft={{actions=[{chain}],display='always'}}"),
        )
        .unwrap();
        let output = command(
            root,
            &[
                "config",
                "check",
                "--config",
                config.to_str().unwrap(),
                "--toml",
                "--strict",
            ],
        );
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(report(&output)["warnings"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn compact_is_boolean_and_reported_without_ignored_option_warnings() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let config = root.join("compact.toml");
    for (text, value) in [
        ("", false),
        ("compact=false", false),
        ("compact=true", true),
    ] {
        fs::write(&config, text).unwrap();
        let output = command(
            root,
            &[
                "config",
                "check",
                "--config",
                config.to_str().unwrap(),
                "--toml",
                "--strict",
            ],
        );
        assert!(output.status.success(), "{output:?}");
        let checked = report(&output);
        assert_eq!(settings(&checked)["compact"].as_bool(), Some(value));
        assert!(checked["warnings"].as_array().unwrap().is_empty());
    }
    for value in ["'true'", "1", "[]"] {
        fs::write(&config, format!("compact={value}")).unwrap();
        let output = command(
            root,
            &["config", "check", "--config", config.to_str().unwrap()],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("compact"));
    }
}

#[test]
fn idle_frame_coalescing_is_opt_in_boolean_and_reported() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let config = root.join("coalescing.toml");
    for (text, value) in [
        ("", false),
        ("idle_frame_coalescing=false", false),
        ("idle_frame_coalescing=true", true),
    ] {
        fs::write(&config, text).unwrap();
        let output = command(
            root,
            &[
                "config",
                "check",
                "--config",
                config.to_str().unwrap(),
                "--toml",
                "--strict",
            ],
        );
        assert!(output.status.success(), "{output:?}");
        let checked = report(&output);
        assert_eq!(
            settings(&checked)["idle_frame_coalescing"].as_bool(),
            Some(value)
        );
        assert!(checked["warnings"].as_array().unwrap().is_empty());
        let plain = command(
            root,
            &["config", "check", "--config", config.to_str().unwrap()],
        );
        assert!(plain.status.success(), "{plain:?}");
        assert!(
            String::from_utf8_lossy(&plain.stdout)
                .contains(&format!("idle_frame_coalescing: {value}"))
        );
    }
    for value in ["'true'", "1", "[]"] {
        fs::write(&config, format!("idle_frame_coalescing={value}")).unwrap();
        let output = command(
            root,
            &["config", "check", "--config", config.to_str().unwrap()],
        );
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("idle_frame_coalescing must be a boolean")
        );
    }
}

#[test]
fn default_mode_is_reported_and_invalid_modes_fail_strict_checks() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let config = root.join("mode.toml");
    for mode in [
        "locked", "normal", "pane", "resize", "move", "tab", "session",
    ] {
        fs::write(
            &config,
            format!("default_mode=' {} '", mode.to_ascii_uppercase()),
        )
        .unwrap();
        let output = command(
            root,
            &[
                "config",
                "check",
                "--config",
                config.to_str().unwrap(),
                "--toml",
                "--strict",
            ],
        );
        assert!(output.status.success(), "{output:?}");
        let checked = report(&output);
        assert_eq!(settings(&checked)["default_mode"].as_str(), Some(mode));
        assert!(checked["warnings"].as_array().unwrap().is_empty());
    }
    for value in ["false", "'history'", "'custom'"] {
        fs::write(&config, format!("default_mode={value}")).unwrap();
        let output = command(
            root,
            &[
                "config",
                "check",
                "--config",
                config.to_str().unwrap(),
                "--toml",
            ],
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("default_mode"));
    }
    assert!(!root.join("state").exists());
}

#[test]
fn discovered_missing_file_uses_defaults_but_explicit_missing_fails() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let defaults = command(root, &["config", "check", "--toml"]);
    assert!(defaults.status.success());
    let defaults = report(&defaults);
    assert_eq!(defaults["file_loaded"].as_bool(), Some(false));
    assert_eq!(defaults["explicit"].as_bool(), Some(false));
    assert_eq!(defaults["shell_source"].as_str(), Some("fallback"));
    assert_eq!(settings(&defaults)["shell"].as_str(), Some("/bin/sh"));
    let missing = root.join("missing.toml");
    let failed = command(
        root,
        &[
            "--config",
            missing.to_str().unwrap(),
            "config",
            "check",
            "--toml",
        ],
    );
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(
        String::from_utf8(failed.stderr)
            .unwrap()
            .contains("could not read")
    );
    assert!(!root.join("rustmux").exists());
}

#[test]
fn reports_effective_settings_and_environment_shell_precedence_without_execution() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let config = root.join("selected.toml");
    let shell = root.join("shell-marker.sh");
    let marker = root.join("executed");
    fs::write(&shell, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        &config,
        format!(
            r#"shell={:?}
scrollback_lines=0
remain_on_exit=true
mouse_hover_cursor=true
autosave_interval_seconds=12
save_scrollback=true
save_scrollback_colors=true
[notifications]
long_command_bell=false
command_duration_seconds=9
"#,
            shell.to_str().unwrap()
        ),
    )
    .unwrap();
    let arguments = [
        "config",
        "check",
        "--config",
        config.to_str().unwrap(),
        "--toml",
    ];
    let configured = command(root, &arguments);
    assert!(configured.status.success());
    let configured = report(&configured);
    assert_eq!(configured["shell_source"].as_str(), Some("config"));
    assert_eq!(
        settings(&configured)["mouse_hover_cursor"].as_bool(),
        Some(true)
    );
    assert_eq!(
        settings(&configured)["scrollback_lines"].as_integer(),
        Some(0)
    );
    assert_eq!(
        settings(&configured)["save_scrollback_colors"].as_bool(),
        Some(true)
    );
    assert_eq!(
        settings(&configured)["command_duration_seconds"].as_integer(),
        Some(9)
    );
    let override_output = base_command(root)
        .env("RUSTMUX_SHELL", "/override-shell")
        .env("SHELL", "/login-shell")
        .args(arguments)
        .output()
        .unwrap();
    assert!(override_output.status.success());
    let overridden = report(&override_output);
    assert_eq!(overridden["shell_source"].as_str(), Some("RUSTMUX_SHELL"));
    assert_eq!(
        settings(&overridden)["shell"].as_str(),
        Some("/override-shell")
    );
    fs::write(&config, "").unwrap();
    let login_output = base_command(root)
        .env("RUSTMUX_SHELL", "")
        .env("SHELL", "/login-shell")
        .args(arguments)
        .output()
        .unwrap();
    let login = report(&login_output);
    assert_eq!(login["shell_source"].as_str(), Some("SHELL"));
    assert_eq!(settings(&login)["shell"].as_str(), Some("/login-shell"));
    assert!(!marker.exists());
    assert!(!root.join("state").exists());
}

#[test]
fn ignored_options_warn_and_strict_failure_keeps_a_parseable_report() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let path = root.join("mixed.toml");
    fs::write(
        &path,
        r#"scrolback_lines=0
future_option="light"
[keybinds.normal]
X={actions=["toggle-floating-terminal"]}
"#,
    )
    .unwrap();
    let permissive = command(
        root,
        &["--config", path.to_str().unwrap(), "config", "check"],
    );
    assert!(permissive.status.success());
    assert!(String::from_utf8_lossy(&permissive.stderr).contains("ignored binding"));
    let strict = command(
        root,
        &[
            "config",
            "check",
            "--config",
            path.to_str().unwrap(),
            "--strict",
            "--toml",
        ],
    );
    assert_eq!(strict.status.code(), Some(1));
    assert!(strict.stderr.is_empty());
    let strict = report(&strict);
    assert_eq!(strict["warnings"].as_array().unwrap().len(), 3);
    assert_eq!(
        settings(&strict)["scrollback_lines"].as_integer(),
        Some(1000)
    );
}

#[test]
fn rejects_invalid_supported_settings_and_conflicting_bindings() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let path = root.join("invalid.toml");
    for source in [
        "scrollback_lines=-1",
        "remain_on_exit='yes'",
        "mouse_hover_cursor='true'",
        "save_scrollback=1",
        "clear_defaults=true",
        "[notifications]\ncommand_duration_seconds=0",
        "[shortcuts]\nnew_window='x'",
        "[keybinds.history]\nx={actions=['unsupported']}",
        "[broken",
    ] {
        fs::write(&path, source).unwrap();
        let result = command(
            root,
            &["config", "check", "--config", path.to_str().unwrap()],
        );
        assert_eq!(result.status.code(), Some(1), "{source}: {result:?}");
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains(&format!("invalid {}", path.display()))
        );
    }
}

#[test]
fn discovered_file_and_explicit_override_select_the_same_startup_paths() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir(root.join("rustmux")).unwrap();
    let discovered = root.join("rustmux/config.toml");
    fs::write(&discovered, "remain_on_exit=true").unwrap();
    let default = command(root, &["config", "check", "--toml"]);
    let default = report(&default);
    assert_eq!(default["path"].as_str(), discovered.to_str());
    assert_eq!(default["explicit"].as_bool(), Some(false));
    let selected = root.join("selected.toml");
    fs::write(&selected, "remain_on_exit=false").unwrap();
    let result = command(
        root,
        &[
            "config",
            "check",
            "--config",
            selected.to_str().unwrap(),
            "--toml",
        ],
    );
    let explicit = report(&result);
    assert_eq!(explicit["explicit"].as_bool(), Some(true));
    assert_eq!(explicit["path"].as_str(), selected.to_str());
    assert_eq!(settings(&explicit)["remain_on_exit"].as_bool(), Some(false));
    assert_eq!(settings(&default)["remain_on_exit"].as_bool(), Some(true));
}

#[test]
fn manager_keys_are_reported_independently_of_clear_defaults_and_ignored_actions_warn() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("manager.toml");
    fs::write(
        &path,
        r#"
clear_defaults=true
[keybinds.locked]
"Ctrl b"={actions=[{action="switch-mode",mode="normal"}]}
[session_manager]
down=["n"]
save=[]
rename=["Ctrl r"]
disconnect=["Ctrl d"]
unimplemented=[]
"#,
    )
    .unwrap();
    let output = command(
        temporary.path(),
        &[
            "config",
            "check",
            "--config",
            path.to_str().unwrap(),
            "--toml",
            "--strict",
        ],
    );
    assert!(!output.status.success());
    let report = report(&output);
    assert_eq!(report["warnings"].as_array().unwrap().len(), 1);
    assert!(
        report["warnings"][0]
            .as_str()
            .unwrap()
            .contains("session_manager")
    );
    assert_eq!(report["session_manager"]["down"][0].as_str(), Some("n"));
    assert_eq!(
        report["session_manager"]["disconnect"][0].as_str(),
        Some("Ctrl d")
    );
    assert_eq!(
        report["session_manager"]["rename"][0].as_str(),
        Some("Ctrl r")
    );
    assert!(
        report["session_manager"]["save"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(report["session_manager"]["up"].as_array().unwrap().len(), 2);
    fs::write(
        &path,
        "[session_manager]\nrename=['Ctrl r']\ndisconnect=['Ctrl d']",
    )
    .unwrap();
    let implemented = command(
        temporary.path(),
        &[
            "config",
            "check",
            "--config",
            path.to_str().unwrap(),
            "--toml",
            "--strict",
        ],
    );
    assert!(implemented.status.success());
    fs::write(&path, "[session_manager]\nup=['j']").unwrap();
    let failed = command(
        temporary.path(),
        &[
            "config",
            "check",
            "--config",
            path.to_str().unwrap(),
            "--toml",
        ],
    );
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(
        String::from_utf8(failed.stderr)
            .unwrap()
            .contains("conflicts")
    );
    assert!(!temporary.path().join("state").exists());
}

#[test]
fn theme_diagnostics_validate_and_report_the_resolved_palette() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let path = root.join("selected.toml");
    fs::write(
        &path,
        "[theme]\npreset='light'\n[theme.colors]\naccent='#01AbEF'\n",
    )
    .unwrap();
    let output = command(
        root,
        &[
            "config",
            "check",
            "--config",
            path.to_str().unwrap(),
            "--strict",
            "--toml",
        ],
    );
    assert!(output.status.success(), "{:?}", output);
    let inspected = report(&output);
    let colors = &settings(&inspected)["theme"];
    assert_eq!(colors["accent"].as_str(), Some("#01abef"));
    assert_eq!(colors["background"].as_str(), Some("#f5f6fa"));
    assert_eq!(colors.as_table().unwrap().len(), 16);
    for source in [
        "theme='light'",
        "[theme]\npreset='Light'",
        "[theme]\ncolors=[]",
        "[theme]\nunknown='x'",
        "[theme.colors]\naccent=123",
        "[theme.colors]\naccent='#1234'",
        "[theme.colors]\nacent='#112233'",
    ] {
        fs::write(&path, source).unwrap();
        let output = command(
            root,
            &["config", "check", "--config", path.to_str().unwrap()],
        );
        assert!(!output.status.success(), "accepted: {source}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("theme"),
            "{:?}",
            output
        );
    }
}

#[test]
fn strict_display_validation_accepts_supported_overrides_and_reports_ignored_actions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let config = root.join("display.toml");
    let arguments = [
        "config",
        "check",
        "--config",
        config.to_str().unwrap(),
        "--strict",
        "--toml",
    ];
    fs::write(&config, "[keybinds.normal]\nc={display='help'}\n[keybinds.history]\ny={display='hidden'}\nH={actions=['show-help'],display='always'}").unwrap();
    let output = command(root, &arguments);
    assert!(output.status.success(), "{output:?}");
    assert!(report(&output)["warnings"].as_array().unwrap().is_empty());
    for mode in [
        "locked", "normal", "pane", "resize", "move", "tab", "session", "history",
    ] {
        fs::write(
            &config,
            format!("[keybinds.{mode}]\nc={{display='sometimes'}}"),
        )
        .unwrap();
        let output = command(root, &arguments);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("display must be always, help or hidden")
        );
    }
    fs::write(
        &config,
        "[keybinds.normal]\nc={actions=['future-action'],display='hidden'}",
    )
    .unwrap();
    let output = command(root, &arguments);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        report(&output)["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("ignored binding"))
    );
}
