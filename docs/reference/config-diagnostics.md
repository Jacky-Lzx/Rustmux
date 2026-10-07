# Configuration diagnostics and default export

`[settings].clipboard_write` reports the Boolean policy for
[child OSC 52 writes](pane-clipboard.md) and [rich writes](rich-clipboard.md),
defaulting to `false`. `[settings].clipboard_read` independently reports
[rich clipboard reads](rich-clipboard.md), also disabled by default.
`[settings].drag_source` reports the independent, default-off [OSC 72 source relay](drag-source.md).
`[settings].drop_target` reports the independent, default-off [OSC 72 receiving relay](drop-target.md).
`[settings].file_transfer` reports the independent [OSC 5113 relay](file-transfer.md),
also disabled by default.

Inspect configuration before starting a terminal or named-session server:

```sh
rustmux check-config
rustmux --config ./project-config.toml check-config --strict
rustmux check-config --config ./project-config.toml --toml
rustmux default-config > ./rustmux-defaults.toml
rustmux check-config --config ./rustmux-defaults.toml --strict
```

`default-config` (alias `dump-config`) prints a reusable TOML template of the
scalar defaults and Session Manager keys implemented on `main-human`. It does not load user configuration,
even if `--config` names a missing or invalid file. It does not write files itself.
Pane-mode keybinding defaults remain implicit: the template deliberately avoids pinning a
separate copy of the binding tables. See [windows](windows.md) and
[history](history-view.md) for bindings and supported overrides.

The shell setting is commented out in the template. Shell selection remains
`RUSTMUX_SHELL`, configured `shell`, `SHELL`, then `/bin/sh`, skipping empty
environment values. Default saving settings remain disabled, while manual
`save-session` stays available. Loading the exported template preserves all
built-in effective settings, including the binding defaults.

`check-config` uses the same file reader, supported-option validation, binding
parser and shell resolution as session startup. By default it selects
`$XDG_CONFIG_HOME/rustmux/config.toml` or `~/.config/rustmux/config.toml`. A missing
discovered file is valid and selects built-in defaults. An explicit `--config`
file must exist. The report identifies the selected path and whether a file was
loaded, so defaults cannot be mistaken for a loaded configuration.

The command works with ordinary pipes and inside a Rustmux pane. It does not
start a shell, create a session endpoint or state directory, attach a terminal,
write configuration, or update a running session. The reported settings apply
to a newly started local session or server using this process's environment.
Existing named servers watch their startup-selected file; attaching with a
different configuration does not replace that source. Use
[`show-config`](config-reload.md#inspecting-a-running-server) to inspect a running
server's applied settings and reload errors.

Ordinary output lists the selected path, effective scalar settings and shell
source. Warnings go to stderr. `--toml` returns a machine-readable inspection
report on stdout, with warnings included in the `warnings` array. A successful
inspection is not an exported configuration: pane-mode bindings are validated but are not
flattened into the report, notification settings are presented as scalar fields,
and paths and shell names use their UTF-8 display representation.

| TOML field | Meaning |
| --- | --- |
| `path` | Selected configuration path |
| `explicit` | Whether `--config` selected that path |
| `file_loaded` | Whether configuration came from a file rather than defaults |
| `shell_source` | `RUSTMUX_SHELL`, `config`, `SHELL`, or `fallback` |
| `warnings` | Diagnostics for ignored options or bindings |
| `[settings]` | Effective shell, scrollback limit, retention, hover feedback, theme, clear-defaults flag, saving settings, and notification settings |
| `[session_manager]` | Effective manager action keys, including disabled actions as empty arrays |

Supported configuration errors, unreadable selected files, invalid TOML and
binding conflicts return exit status 1, with an error on stderr and no report.
The parser remains compatible with existing files that contain options from the
independent `main` track. Diagnostics identify ignored top-level options,
notification fields, binding modes, binding metadata other than `actions` and
`display`, and entire bindings whose action chains are ignored by the real
parser. This includes incomplete chains that would otherwise silently fall back
to a default shortcut.

Without `--strict`, ignored options produce warnings and the command succeeds.
With `--strict`, any warning gives exit status 1; the report is still available,
including with `--toml`. Supported files therefore can be checked in scripts
without mistaking ignored options for applied settings. This is a diagnostic
layer: ordinary startup continues using its existing compatible parsing rules.

The inspection validates the implemented configuration subset, not every nested
field in `main`'s schema. In particular, it does not check ignored parameters
inside otherwise supported action tables. It does not check that the selected
shell executable exists or can start, and it does not certify editor or terminal
compatibility. Shell startup still performs its own executable validation.
See [configuration hot reload](config-reload.md) for live updates and restart-only
settings. See [Interface Themes](interface-themes.md) and
[Mouse Hover Feedback](mouse-hover.md) for the supported display settings.

## Review and verification

Read `src/config/diagnostics.rs`, then the shared reader and resolver in
`src/config.rs`, CLI definitions in `src/cli.rs`, and command dispatch in
`src/main.rs`. `tests/config_cli.rs` runs the actual binary with pipes and
isolated configuration/state directories. It covers exported-default round
trips, missing discovered versus explicit files, ignored-option strict failures,
invalid settings and binding conflicts, source precedence, shell environment
overrides, nested invocation, and preservation of files and state directories.

The library tests check template equivalence and binding diagnostics across all
supported modes, including custom entry keys and default-cleared configurations.
This implementation awaits the owner's final review; it does not record feature
acceptance in the shared ledger. Linux CI and installed-client checks were not
performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 869 passed,
  zero failed, six existing tests ignored by default, across 47 test targets.
- All six configuration CLI integration tests and all 18 real PTY scenarios
  passed. The three new diagnostics library tests also passed.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  Rust formatting, `git diff --check` and the mdBook build passed.
