# Script directional focus

```sh
rustmux select-pane -s work --direction right
rustmux select-pane -s work -p 0 --direction down
rustmux select-pane -s work -p 0
```

`select-pane --direction left|right|up|down` focuses the nearest geometric neighbor
of an origin pane in the same window. The origin defaults to the active pane;
optional `-p ID`/`--pane ID` supplies an explicit origin, which may be unselected
or in an inactive window. Success selects the neighbor and activates its window.
The origin is never briefly selected. Without `--direction`, the existing
`select-pane -p ID` behavior still selects that exact ID. Supplying neither an
ID nor a direction is an error. The default session is `default`, and `-s SESSION`
chooses another running server.

Selection follows the existing interactive geometry rules. Candidates must lie
wholly on the requested side with positive perpendicular overlap. Diagonal and
corner-only contact do not qualify. Prefer smallest edge gap, nearest
perpendicular starting edge, largest overlap, nearest perpendicular center and
finally traversal order. A full-height left pane therefore chooses the upper
right pane even if the lower pane is larger. No edge wrapping occurs, and moving
back is not always an inverse in uneven layouts. This changes focus; pane
positions, separator positions and split ratios remain unchanged. To exchange
positions, use `move-pane` or `swap-pane`.

Zoom remains enabled. Neighbor lookup uses the underlying tiled rectangles even
for a hidden explicit origin. Selecting a new target transfers the enlarged
view to it; normal control refresh resizes both its PTY/screen and the former
selected pane back to the appropriate tiled size. Inactive target windows are
also supported. Processes, runtime IDs, startup commands, history, screen and
parser state, and queued input/output remain owned by their original panes.
Retained exited panes can be selected without restarting their jobs; temporary
editor panes are eligible just as with exact-ID selection. Hidden close-undo
panes are not targets. Runtime IDs must be enumerated again after restoration.

Success returns zero and prints nothing, dismisses History/help/prompts, resets
shortcuts to Locked mode and redraws an attached client. The same normal focus
transition routes focus-out to the previously active application and focus-in
to the newly active application if they have requested reports. An unselected
origin does not receive an intermediate focus report. Input and application
cursor modes follow the selected screen. If an explicit origin's neighbor is
already active, selection succeeds without duplicate application focus events
and still resets overlays. A changed selection updates last-window history by
the existing window-selection rules.

Unknown/stale origins, malformed requests and missing neighbors (including
single-pane windows) return a nonzero status before changing remembered/global
focus, geometry or overlays. Failed zoomed selections retain zoom and its active
target. Selection replies acknowledge accepted focus, not completion of a
displayed frame or application resize handling. Independent physical input and
control requests have no added ordering guarantee; bytes already queued for a
child remain with it. Size synchronization failures use normal runtime cleanup.

Attached and detached servers accept requests without taking the interactive
client lease. Detached selection applies to the next attachment and manual or
configured automatic snapshot, which records the selected window/pane and zoom.
The command does not force a save. Workspace restoration launches fresh startup
processes in the saved geometry rather than resuming previous children.

## Review and verification

The base is reviewed `move-pane` commit `a0e76bf`, merged into `main-human`.
Both tracks already support interactive directional focus; `main` at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c` lacks this script option.
This extends the existing CLI without changing the exact-ID request shape:
`--direction` uses the additive `select-pane-direction` wire action. Older
servers continue accepting exact-ID requests and reject the new action until
restarted. The displayed-client handshake and snapshot formats are unchanged.

Reading order: argument validation, CLI/request mapping and refresh classification
in `src/control.rs`, source lookup and window activation in
`src/terminal/control.rs`, then `select_direction_from` and shared neighbor
lookup in `src/layout.rs`/`src/pane_set.rs`. Interactive focus and movement use
the same geometric helper; their edge and zoom policies remain unchanged.

CLI tests cover all four directions, omitted/explicit origin, ID zero, invalid
arguments and the original required exact-ID/window forms. Layout tests cover
inactive origins, already selected neighbors, stale/edge failure isolation,
unchanged tiled geometry and zoom transfer, alongside existing alignment and
no-wrapping fixtures. `tests/terminal_loop_directional_focus.py` uses raw probes
and actual PTYs to check inactive-window origins, exact focus/blur bytes without
intermediate origin focus, application cursor modes and physical input, all
four directions, same-target success, stale/malformed/edge failures preserving
History, zoomed lookup and actual dimensions, overlay/prefix refresh, unchanged
PIDs, detached focus, retained exited panes without respawn, and saved selection/
zoom restored with fresh startup jobs. Client exits check outer termios restoration.

This branch awaits owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01, using Rust 1.99.0:

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  941 passed, zero failed, six existing ignored tests, across 47 test targets.
- All 37 real PTY scenarios passed, including directional focus, exact-ID focus,
  interactive focus/movement, zoom, swaps, snapshots and session management.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`
  passed.
- `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.
