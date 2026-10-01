# Retained Panes and Respawn

A process normally removes its pane after its final output is drained. To keep
its output and layout for inspection, add this top-level config option before
any TOML table header:

```toml
remain_on_exit = true
```

The default is `false`. Local sessions and named servers read it at startup;
existing servers keep their original config. A project pane may override the
server default:

```toml
[[windows]]
name = "checks"
[[windows.panes]]
command = "cargo test"
remain_on_exit = true
```

`false` explicitly disables retention for that pane. An omitted project option
uses the current server default. Temporary history/output editors always close
on exit. Per-pane overrides follow panes through moves and are saved in workspace
snapshots; the server default comes from the config used to restore the workspace.
Older snapshots that omit the override remain readable.

Retained panes display `[exited N]` or `[signal N]` in their frame title. Their
screen and retained history remain available to capture, History mode, navigation,
zoom and ordinary close confirmation. Input intended for the stopped process is
discarded; Rustmux shortcuts remain available. Final output drains before the
PTY closes and the direct child is reaped. A named server stays available in
`ls` and the session manager even when every visible pane has exited. Explicitly
closing its final pane ends the server; an existing saved workspace stays listed.
For script cleanup, `close-pane -s SESSION -p ID` removes a running or retained
pane without confirmation or undo storage. It preserves the session's final
pane with an error; end the whole session using `kill SESSION` instead. See
[Script Pane Close](script-pane-close.md).
The existing EOF cleanup still finalizes incomplete terminal sequences and
releases pane graphics storage; retention preserves the text grid and history.

Snapshots store the layout, configured commands and optional output, not process
exit status. Restoring a retained interactive pane starts a new shell; restoring
a recorded project command reruns that command, which may exit and become
retained again.

## Restarting a pane

With default shortcuts, Ctrl-B then **Shift-R** restarts the active exited pane.
The help panel calls this `Respawn exited pane`. `respawn-pane` is also supported
in `[keybinds.normal]` and `[keybinds.pane]`, followed by `switch-mode` to `locked`:

```toml
[keybinds.normal]
R = { actions = ["respawn-pane", { action = "switch-mode", mode = "locked" }] }
```

The named script interface can target a background pane:

```sh
rustmux list-panes -s work --toml
rustmux respawn-pane -s work -p 0
rustmux respawn-pane -s work -p 0 --command 'cargo test' --cwd /absolute/project
```

Restart retains its pane ID, layout, selection and exit-policy override, and
creates a new process/PTY at its current content dimensions. It clears the old
screen/history, parser, graphics, terminal modes, notifications and queued input.
A missing `--command` repeats the recorded startup command, or starts an
interactive shell when there is none. An explicit command replaces the recorded
command for later respawns and snapshots. It uses the configured shell with `-c`,
with the same 4096-byte/NUL limits as project commands. To replace a project job
with an interactive shell, use `--command 'exec /bin/sh -i'` or another shell.

The directory defaults to the last tracked OSC 7 directory, otherwise the pane's
original startup directory. Once the process exits, Rustmux does not inspect its
old PID, which the OS may have reused. `--cwd` requires an absolute existing
directory. A live pane, an exit whose final output is not yet drained, a temporary
editor, an invalid command/directory or a failed exec rejects the restart.
Replacement preparation failures preserve the exited pane's output, status and
layout; no running process is implicitly killed. Interactive rejection rings the
bell, while the script command returns a nonzero status and error text.

`list-panes --toml` additionally reports `exited`, `output_complete`, and either
`exit_code` or `exit_signal` when available. `exited` describes the direct child;
`output_complete` describes its output drain. Wait for both before respawning.
The `pid` field remains the previous child's ID until a successful restart and
must not be used to signal an exited process. Runtime IDs survive respawn but
still need re-enumeration after restarting the whole server.

## Review and verification

Reading order: exit policy in `src/config.rs` and `src/project.rs`, pane exit
observation and replacement in `src/pane.rs`, attached/detached cleanup and input
handling in `src/terminal.rs`, `respawn-pane` in the control modules, and snapshot
overrides in `src/persistence.rs`.

The focused unit check covers exec-failure preservation, resource release,
identity/dimension retention and process-state reset. The real PTY lifecycle
scenario covers per-pane policy overrides, final output, normal and signal exits,
live/invalid restart rejection, detached command replay, moved exited panes,
saved-policy restoration, all-exited named servers, attached history/resize/close,
foreground respawn, partial paste cleanup and temporary editor auto-close.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 854 passed,
  zero failed, six existing tests ignored by default, across 46 targets.
- All 17 real PTY scenarios passed, including the default exit-path regressions.
- Clippy for all targets/features with warnings denied, Rust formatting and the
  mdBook build passed.

Local macOS checks are implementation evidence. The final revision awaits the
owner's review; Linux CI and the installed everyday client are not verified here.
