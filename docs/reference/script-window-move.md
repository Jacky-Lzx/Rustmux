# Script window move

```sh
rustmux list-panes -s work --toml
rustmux move-window -s work -w 2 --direction left
rustmux move-window -s work --direction right
```

`move-window` moves a visible window one position in the bar without selecting
it. It accepts `-s SESSION` (default `default`), optional `-w NUMBER` (default the
active window), and required `--direction left|right`. Moving left at the first
position sends the target to the end; moving right at the last position sends it
to the start. Other windows retain their relative order. A single-window session
accepts both directions as successful no-ops.

Numbers are one-based positions in current display order, matching `list-panes`
and the window bar. Each request interprets its target using that current order;
re-enumerate after moving before using another stored number. Names are opaque
metadata and can be duplicated. Runtime pane IDs remain stable within the
running server, so they can identify which window currently owns a pane.

Active window identity, its selected pane and the last-window history are
preserved whether moving the active or an inactive window. The remembered
selected pane in every window, split geometry and zoom are unchanged. Moving
does not restart or close children, transfer panes, release retained output or
alter pane IDs, contents, application modes or existing input/output queues.
Retained exited panes move with their window without being respawned.

Success returns zero and prints nothing. Unknown window numbers, invalid CLI
arguments and malformed requests return nonzero before reordering, preserving
focus, order and overlays. CLI numbers must be between 1 and 65535; the server
also validates them against the current collection. Only left/right directions
are accepted. The model rejects unknown stable window IDs before changing order.

The command uses the normal control refresh. Attached clients redraw the bar,
dismiss History/help/prompts and return shortcuts to Locked mode, including
single-window no-ops. Surviving PTYs/screens are synchronized through the existing
path. Reordering alone does not change their sizes or application focus, so it
emits no focus-in/focus-out report. Application input modes and physical input
continue to use the same active pane. Successful replies acknowledge accepted
order rather than completion of a displayed frame. Input and control requests
arrive independently; the command is not a physical-input barrier.

Attached and detached servers accept moves without acquiring the interactive
client lease. The next manual or configured automatic snapshot records the new
display order, active window position, each remembered pane and each layout's
zoom. A move does not force a disk write. Restoration recreates the saved order
and view with fresh startup processes. Handshake and snapshot formats are unchanged.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

The base is reviewed `zoom-pane` commit `3621b35`, merged into `main-human`.
Both tracks already support interactive window movement; `main` at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks this script command.
The existing interactive movement wrappers and new targeted methods share the
same window model implementation. Reordering moves owned entries and recalculates
the active position by stable identity, without changing last-window history.
Older servers reject the new control action until restarted.

Read CLI/request mapping and refresh classification in `src/control.rs`, target
lookup and dispatch in `src/terminal/control.rs`, then targeted movement and
interactive wrappers in `src/window.rs`. Existing event loops refresh the bar
and synchronize surviving sizes.

The model test covers all active/target/direction combinations for one through
four windows, including edge wraps, active identity, unchanged last-window
history, owned content addresses and invalid-ID isolation. Existing window tests
retain interactive ordering and subsequent selection/removal coverage. CLI tests
check omitted/explicit target, required direction and invalid numbers/directions.
`tests/terminal_loop_window_move.py` uses real PTYs, raw application probes,
retained exited jobs and a physical attached client. It checks active/inactive
adjacent and edge moves, duplicate names, current numbering, stable PIDs and
unchanged PTY dimensions/focus bytes, remembered zoom/pane selection, single-window
no-op refresh, malformed request and History isolation, prefix refresh, cursor
modes and input routing, last-window toggling by identity, detached moves, saved
order/zoom/selection and restoration with fresh startup processes. Client exits
verify restored outer terminal attributes.

This branch awaited owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01, using Rust 1.99.0:

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  932 passed, zero failed, six existing ignored tests, across 47 test targets.
- All 34 real PTY scenarios passed, including targeted window moves, interactive
  movement, zoom, window/pane closure, snapshots and session management.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`
  passed.
- `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.

The initial sandboxed library run could not open shared memory or session sockets
and failed with `Operation not permitted`. The final full run with those local
permissions passed, including the affected library tests.
