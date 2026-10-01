# Script window close

```sh
rustmux list-panes -s work --toml
rustmux close-window -s work -w 2
rustmux close-window -s work
```

`close-window` removes a visible window and all its panes, including running
jobs and panes retained after process exit. It accepts `-s SESSION` (default
`default`) and an optional `-w NUMBER` (default the active window). Numbers are
one-based positions in the current window bar and `list-panes` output. Removal
and reordering change them: enumerate again before using a stored position.
Duplicate window names do not affect targeting.

The command returns zero and prints nothing on success. It closes without a
confirmation prompt and does not enter interactive close-undo storage. It removes
the entire window from the ownership model, then drops its pane collection.
Existing `PtyShell` cleanup closes each PTY, stops and reaps each directly owned
live child, and does not signal children that have already been reaped. It uses
the existing drop cleanup rather than introducing a new process-tree supervisor.
The reply acknowledges ownership removal; per-child drop cleanup retains its
existing best-effort error handling rather than reporting cleanup errors on the wire.
Hidden panes stored separately for interactive undo are not window contents;
an existing undo record is unaffected.

All removed panes' screen, history, input queues and raw output buffers are
released. Capture or finish logging before closing if that output is needed.
Their runtime pane IDs become invalid. Surviving IDs, child processes, terminal
state, layouts, zoom and remembered pane selection are preserved.

Closing an inactive window preserves active window identity. Closing the active
window selects its successor in display order, or its predecessor at the end,
using that window's remembered selected pane. The existing last-window model
discards references to removed windows. Successful operations use the normal
control refresh: surviving PTYs/screens are synchronized; an attached client
dismisses History/help/prompts, returns shortcuts to Locked mode and redraws.
If focus changes, the surviving active pane receives the usual focus-in report
when enabled. An unchanged active pane receives no focus event. The reply is
not a barrier for surviving applications' rendering or SIGWINCH handling.

The session's final window is protected, even if it contains multiple panes.
Closing it returns nonzero with
`cannot close the final session window; use kill SESSION`, preserving all panes,
focus and overlays. Use `kill SESSION` to end the server, or `close-pane` to
remove selected panes while leaving the final pane. Unknown window numbers and
malformed requests also fail before ownership removal. CLI numbers must be
between 1 and 65535; a number outside the current collection fails on the server.
This guard keeps the control response channel and active runtime layout usable.

Attached and detached servers accept the command without acquiring the
interactive-client lease. Subsequent manual or configured automatic snapshots
contain only the surviving windows and their remembered selection and layouts.
Closure does not force a disk write or delete an existing saved workspace.
Save before restarting to exclude closed jobs from replay; restoring an older
snapshot can recreate them.

## Review and verification

The base is reviewed `close-pane` commit `344ed59`, merged into `main-human`.
Both tracks have interactive window closure; `main` at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks this script command.
The implementation extends the existing control channel and reuses
`Windows::close` and owned pane cleanup. Handshake and snapshot formats are
unchanged; older servers reject the new action until restarted.

Read CLI/request mapping and refresh classification in `src/control.rs`, then
target validation and the final-window guard in `src/terminal/control.rs`.
`src/window.rs` provides removal and focus fallback; the existing event loops
route focus events, synchronize sizes and redraw.

The CLI test checks the default active target, an explicit positive number and
invalid numbers. Existing window model tests cover ownership removal, surviving
focus and last-window cleanup. `tests/terminal_loop_window_close.py` checks real
PTYs and directly owned child processes: mixed live/retained multi-pane window
cleanup, current-order targeting after a reorder, inactive closure without
survivor changes, active successor/predecessor fallback, remembered zoom and
cursor mode, exactly one focus-in report and physical input routing, malformed
request and History isolation, successful overlay refresh, absence of close undo,
last-window behavior, final multi-pane window protection while attached and
detached, saved survivor selection/zoom and restoration without replaying removed
jobs. Client exits verify restored outer terminal attributes.

This branch awaits owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01, using Rust 1.99.0:

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  927 passed, zero failed, six existing ignored tests, across 47 test targets.
- All 32 real PTY scenarios passed, including whole-window closure, pane
  closure, snapshot restoration, session management and terminal-device faults.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`
  passed.
- `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.

## CI startup-race repair

GitHub [run 36883454438](https://github.com/Jacky-Lzx/Rustmux/actions/runs/36883454438)
on `main-human` commit `9ecff78` passed Ubuntu but failed the macOS window-close
scenario at “last-window focus-out did not arrive.” The probe had written its
JSON state, but the server could still be waiting to parse its earlier DECSET
1004 output. Selecting another window at that point correctly queues no
focus-out for an application that has not yet enabled reporting.

The fixture now polls `capture-pane` for `WINDOW_CLOSE_READY`, which the probe
emits after enabling focus reporting. A capture reads the server's parsed screen,
so it establishes the required readiness boundary for both attached and detached
servers. Child-state and PID checks remain in place; focus-event assertions and
five-second timeouts are unchanged. Production code is unchanged.

A temporary controlled reproduction gated the last probe's mode output after
publishing its initial JSON. Switching away before releasing that gate reproduced
the original failure. With the corrected helper, releasing the gate and waiting
for the parsed marker allowed the same focus-out and closure assertions to pass.
The normal corrected scenario also passed five consecutive runs. These controlled
copies are local diagnostics rather than additional repository test targets.

This repair is isolated from the pending notification-filter feature and awaits
owner review. The repaired revision has not been pushed or run in GitHub CI.

Local verification of the CI repair, using Rust 1.99.0:

- The focused window-close test passed; a controlled gated startup passed after
  the fix, followed by five consecutive normal runs of the same real PTY fixture.
- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  941 passed, zero failed, six existing ignored tests, across 47 targets.
  All 37 real PTY scenarios passed under CI's four-thread setting.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.
