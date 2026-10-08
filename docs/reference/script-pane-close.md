# Script pane close

```sh
rustmux list-panes -s work --toml
rustmux close-pane -s work -p 2
rustmux close-pane -s work
```

`close-pane` removes a visible runtime pane, including one in an inactive window
or one retained after process exit. It accepts `-s SESSION` (default `default`)
and optional `-p ID` (default the active pane). Enumerate IDs before using them;
they remain stable within the running server but are not persistent across
restoration. Hidden interactive undo panes are not script targets.

The command returns zero and prints nothing on success. It is an explicit
closure without a confirmation prompt and does not retain the removed pane for
interactive close undo. Ownership cleanup closes its PTY, stops the directly
owned child if still running and reaps it, following existing `PtyShell` drop
behavior. An exited/reaped child is not signaled again. Other owned pane
processes, terminal state and IDs are preserved.

Removal releases the pane's screen, history, queued input and raw output buffers.
Capture or finish logging before explicit closure if that output is needed.
Further control requests targeting its ID return an unknown-pane error.
`remain_on_exit` retains natural process exits; it does not prevent explicit closure.

If a window has several panes, closing an inactive pane preserves that window's
selected pane. Closing its selected pane chooses the next pane in layout
traversal, or the preceding pane at the end. Its sibling subtree expands into
the vacated area and zoom is cleared, matching the layout model's close rules.
The active window remains selected. Closing a window's sole pane removes that
window: an inactive removal preserves the active window's identity, while an
active removal selects its successor or its predecessor at the end. Display
window numbers can change, so enumerate before using stored numbers.

The final pane of the whole session is protected. An attempt to close it returns
nonzero with `cannot close the final session pane; use kill SESSION`, preserving
its process, layout, focus and overlays. Use `rustmux kill SESSION` to end the
server. Unknown/stale IDs and malformed wire requests also fail without removing
anything. This final-pane rule keeps a running server with a valid active layout
and a usable response channel; it applies to both live and retained final panes.

Both attached and detached servers accept closure without acquiring the
interactive-client lease. Accepted operations use the existing control refresh
path to synchronize surviving PTYs/screens, including inactive windows. Attached
clients dismiss History/help/prompts, reset shortcuts to Locked mode and redraw.
An unchanged active pane receives no focus event; a newly active pane receives
the normal focus-in report if enabled. A reply is not a barrier for surviving
applications' SIGWINCH handling or rendering. Synchronization I/O failures
propagate through runtime cleanup.

Subsequent manual or configured automatic snapshots contain the surviving
windows/panes and remembered focus. Closure alone does not force a disk write
or delete an existing saved workspace. Saving before a restart excludes removed
startup jobs from replay; restoring an older snapshot can still recreate them.
Script closure does not change a previously stored interactive undo record.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

This increment is based on reviewed CI compatibility fix `3c1c6d3`, now merged
into `main-human`, and includes the reviewed startup-arguments implementation.
`main` at `57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks
this scripting command, though interactive close actions exist in both tracks.
The implementation extends the human control channel and reuses the layout,
window ownership and child-cleanup models without changing handshake/save formats.
An older server rejects the new action until restarted.

Read CLI/request and refresh classification in `src/control.rs`, followed by
target validation, final-pane guard and ownership removal in
`src/terminal/control.rs`. `PaneSet::close` preserves surviving owned contents;
`Windows::close` manages window fallback and last-window state. Existing event
loops route focus transitions and synchronize surviving sizes. The guard is
checked before removing ownership, so it never empties the runtime collection.

The CLI test checks omitted and explicit targets. Existing pane/window model
tests cover ownership transfer and focus/removal rules.
`tests/terminal_loop_pane_close.py` uses owned named servers, real raw
applications and actual PTYs. It checks child exit/reaping, stable surviving
IDs/PIDs, nested inactive closure and size expansion, exactly one fallback focus
report, physical input routing, zoom closure and cursor mode, stale/malformed
requests, failed-request History isolation, successful refresh, absence of close
undo, active traversal successor/predecessor, final-pane protection while attached
and detached, retained pane cleanup, window renumbering, saved survivor geometry
and restoration without replaying removed startup jobs. Client exits check
restored outer terminal attributes.

This branch awaited owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01, using Rust 1.99.0 (the
toolchain from the reported CI run):

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`: 925 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 31 real PTY scenarios passed, including pane closure, startup, resizing,
  script focus, snapshot restoration, session management and terminal-device faults.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`
  passed.
- `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.
