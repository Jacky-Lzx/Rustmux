# Operating system and machine overrides

Shared settings stay at the root of `config.toml`. Put differences under
`overrides.os.<os>` and `overrides.machine.<machine>`. Layers apply in this fixed
order, regardless of their position in the file:

1. Built-in defaults.
2. Shared settings in the selected configuration file.
3. The matching operating system override.
4. The matching machine override.

```toml
compact = false
scrollback_lines = 1000

[theme]
preset = "mocha"

[overrides.os.macos]
shell = "/opt/homebrew/bin/fish"

[overrides.os.linux]
shell = "/usr/bin/fish"

[overrides.machine."macbook"]
scrollback_lines = 10000

[overrides.machine."dev-server"]
compact = true

[overrides.machine."dev-server".notifications]
enabled = false

[overrides.machine."macbook".keybinds.normal]
x = []
```

OS names use Rust's [`std::env::consts::OS`](https://doc.rust-lang.org/std/env/consts/constant.OS.html)
identifiers, such as `macos` and `linux`. Recognizing a name in a shared file does
not imply that Rustmux can run on that platform. Names match exactly and are
case-sensitive. Quote machine names containing dots, such as
`[overrides.machine."desk.local"]`.

The machine name comes from `RUSTMUX_MACHINE` when set, otherwise from the system
hostname. Set a stable local alias when synchronizing your dotfiles:

```sh
export RUSTMUX_MACHINE=macbook
```

An empty, invalid UTF-8 or control-character-containing explicit machine name is
an error. If the system hostname is unavailable or invalid, machine overrides
are skipped and configuration diagnostics report the reason.

## Merge rules

| Setting | Override behavior |
| --- | --- |
| Scalar, such as `compact` or `shell` | Explicit values replace inherited values; omitted fields inherit. |
| `notifications`, `session_manager`, legacy `shortcuts` | Merge by field or action. Arrays replace the whole list; `[]` clears a list. |
| Keybindings | Merge by native mode and physical key. Canonical aliases refer to the same binding. Action arrays replace the complete sequence; detailed bindings merge `actions` and `display` separately. |
| `x = []` or `x = { actions = [] }` | Remove that binding, including an implicit default. Removed bindings do not dispatch or appear in Help. Reenabling one requires actions; display metadata alone cannot restore it. |
| `clear_defaults = true` | Clear all inherited keybinding and legacy shortcut declarations before applying this layer, and disable implicit defaults. A later `false` does not restore them. Session Manager bindings are independent. |
| Override `theme.colors` | Merge named colors into the inherited theme. |
| Override `theme.preset` | Reset to the preset, then apply this layer's colors; discard previous custom colors. |

The shared root keeps `main`'s existing parser and compatibility warnings. In an
override, a binding can use an action-array shorthand in addition to the native
detailed form. Shorthand replaces both actions and display metadata, restoring
visibility to `always`. Removing a remapped action does not revive its displaced
implicit default key:


```toml
[overrides.os.macos.keybinds.normal]
c = ["new-window", { action = "switch-mode", mode = "locked" }]
```

Overrides use the native modes and supported action chains of `main`, including
`history`; they do not introduce custom modes or the old `scroll` mode. The
active LOCKED-mode command entry must remain accessible. When clearing defaults,
provide a supported LOCKED-to-NORMAL binding, as required by the ordinary parser.
Within one override block, duplicate physical-key aliases are rejected.

All override blocks are checked for unknown fields, invalid types, local values
and unsupported bindings, including blocks that do not match this machine.
Nested `overrides` are not allowed. Checks involving the combined configuration,
such as Session Manager conflicts or the bindings required by `default_mode`,
run after the selected layers merge. Root options that were previously ignored
continue producing compatibility warnings.

## Inspect selection and sources

Use the existing grouped configuration commands:

```sh
rustmux config check --os linux --machine dev-server --explain
rustmux config check --config ./shared.toml --os macos --machine macbook --toml --strict
rustmux config show -s work
```

`--os` and `--machine` only simulate selection for `config check`. The explicit
machine option takes precedence over `RUSTMUX_MACHINE` and the hostname.
`--explain` prints the environment, applied layers and configuration field
sources; it cannot be combined with `--toml`. TOML diagnostics include
`[selection.environment]`, `selection.layers` and `[selection.sources]` alongside
the existing settings and warnings. Binding field names in sources use canonical
physical keys. Shell environment precedence is still reported separately by
`shell_source`: `RUSTMUX_SHELL` overrides the configured shell.

Checks validate the configuration rather than checking whether a shell
executable exists on the simulated machine. No terminal or session is started.
`config default` remains a reusable template without pinning overrides.

## Server identity and reload

The server captures its OS and machine identity at startup and reuses it during
background reloads. SSH therefore matches the remote server, and attaching from
another client does not change its selection. The client-side Session Manager
continues loading its own local configuration, as before.

Reload rebuilds the selected layers from defaults. Removing an override restores
the lower layer's value. Invalid updates retain the last applied configuration.
`config show` includes the selection actually applied by the server, which can
differ from a local `config check` or a candidate waiting to apply.

The existing [hot-reload boundaries](config-reload.md) still apply. Changes to
the entry key or the effective `clear_defaults` policy require a restart and
reject the whole update. Other valid changes wait for the existing safe input
boundary. Explicit-file deletion reports an error; deletion of a discovered
default file selects built-in defaults when compatible with the entry policy.

Read `src/config/overrides.rs` for validation, merging and provenance;
`src/config/reload.rs` for environment capture and background updates; then
`src/config/display.rs` and the input handlers for removed implicit shortcuts.
