# Script pane move

```sh
rustmux list-panes -s work --toml
rustmux move-pane -s work -p 0 --direction right
rustmux move-pane -s work --direction up
```

`move-pane` exchanges a pane's layout position with its nearest neighbor in the
requested direction within the same window. It accepts `-s SESSION` (default
`default`), optional `-p ID` (source, default the active pane), and required
`--direction left|right|up|down`. Enumerate runtime IDs before use; they remain
stable during the server lifetime but change when a workspace is restored.
Hidden panes stored for interactive undo are not targets.

Neighbor selection uses the same tiled rectangles and scoring as interactive
pane movement. Candidates must be wholly on the requested side with positive
perpendicular overlap. Diagonal and corner-only neighbors do not qualify.
Prefer the smallest edge gap, then the nearest perpendicular starting edge,
largest overlap, nearest perpendicular center and finally layout traversal
order. A full-height left pane therefore moves right into the upper right slot,
even when the lower slot has an extra row. There is no edge wrapping. Because
slot sizes can differ, moving in one direction and back does not always undo
the move; use `swap-pane` when an explicit pair is needed.

The command preserves the global active window and each window's remembered
selected pane. It never selects the source or neighbor. An inactive source can
exchange with the active pane, which retains focus in its new position. The
same child receives subsequent input using its existing application modes, and
the move itself emits no focus-in/focus-out reports. Inactive windows can also
be rearranged without being activated.

Split slots, separator positions and proportions stay unchanged; only leaf pane
identities exchange positions. Processes, runtime IDs, screen contents, history,
parser state, startup commands and input/output queues remain owned by the same
panes. Normal control refresh reflows screens and synchronizes PTY dimensions
with their new slots in all windows. Unequal slots can change the two targets'
dimensions. Unaffected panes keep their sizes. Retained exited panes can move
without restarting their jobs or losing recorded output and exit status.

Success returns zero and prints nothing. Successful requests dismiss
History/help/prompts, reset shortcuts to Locked mode and redraw attached clients.
Replies acknowledge accepted layout changes, rather than completion of
application SIGWINCH handling or displayed frames. Physical input and independent
control requests have no additional ordering guarantee. Size synchronization
I/O errors follow the existing runtime cleanup path.

Unknown/stale IDs, malformed requests, zoomed target layouts, temporary
history/output editor panes (as either source or neighbor), and directions with
no neighbor fail before mutation. Single-pane windows have no neighbors.
Failures preserve layout, focus and overlays. Turn off zoom in the target
window before moving. Cross-window movement remains available through
`join-pane`/`break-pane`.

Attached and detached servers accept this command without taking the interactive
client lease. The next manual or configured automatic snapshot records the new
positions, startup commands and unchanged selected pane; a move does not force
a disk write. Restoration recreates the saved layout with fresh startup
processes. It does not resume previous children. Handshake and snapshot formats
remain compatible, and interactive close-undo storage is unaffected.

## Review and verification

The base is reviewed `swap-pane` commit `623821a`, merged into `main-human`.
Both tracks already support interactive pane movement; `main` at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks this script command.
The implementation extends existing directional movement to an explicit source,
validates both targets' eligibility before exchanging leaf identities, and uses
normal control refresh. Interactive edge/zoom moves keep their existing no-op
behavior; script requests report errors for those cases. Older servers reject
the new action until restarted.

Reading order: CLI/request mapping and refresh classification in `src/control.rs`,
runtime target and eligibility validation in `src/terminal/control.rs`, then
`move_target`/`move_pane` in `src/layout.rs` and the delegate in `src/pane_set.rs`.
Existing event loops synchronize sizes and redraw without changing focus.

CLI coverage checks omitted/explicit source, ID zero, all four directions and
invalid/missing arguments. The layout test verifies geometric neighbor selection,
exact slots/separators, unchanged selection, inactive-to-active exchanges,
vertical inverses, closed IDs, edges, single panes and zoom failure isolation.
Existing interactive movement coverage exercises the shared implementation.
`tests/terminal_loop_pane_move.py` uses raw applications in real PTYs to check
active/inactive movement, actual dimensions, stable PIDs, focus bytes, application
cursor mode and physical input, screen ownership, successful overlay/prefix
refresh, failed edge/stale/malformed/zoom/editor requests preserving History,
detached movement, retained exited output without respawn and saved selection/
positions restored with fresh startup jobs. Editor coverage also checks a
temporary pane as the neighbor of a normal pane created by interactive split.
Client exits verify restored outer terminal attributes.

This branch awaits owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01, using Rust 1.99.0:

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  938 passed, zero failed, six existing ignored tests, across 47 test targets.
- All 36 real PTY scenarios passed, including directional pane moves, interactive
  movement, swap, zoom, temporary editors, snapshots and session management.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`
  passed.
- `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.
