# Script focus control

```sh
rustmux pane list -s work --toml
rustmux pane select -s work -p 0
rustmux window select -s work -w 2
```

`pane select` without `--direction` requires `-p ID` (or `--pane ID`). It finds
the pane's runtime ID across all windows, selects it and activates its owning window. `window select`
requires `-w NUMBER` (or `--window NUMBER`) and selects that window's remembered
pane. Numbers are one-based display positions, matching the window bar and the
`window` field of `pane list`. They can change after removal or reordering.
Enumerate current targets before choosing; use a pane ID when identity must
survive window moves. The default session target is `default`; `-s SESSION`
selects another named running server.

`pane select --direction left|right|up|down` selects a geometric neighbor of the
active pane. With `-p ID`, that ID supplies the origin; the origin need not be
selected or in the active window. No neighbor returns an error without changing
focus. See [Script Directional Focus](script-directional-focus.md) for geometry,
zoom and compatibility details.

Successful commands print nothing and exit with status zero. Missing required
arguments, zero window numbers, unknown windows/panes and malformed requests
return a nonzero status. An invalid target preserves focus and interactive
overlays. Closed panes held in the undo slot are unavailable, while visible
retained exited panes remain selectable without respawning. Temporary editor
panes can be selected using the same IDs as ordinary panes.

The commands keep all owned processes, runtime IDs, shell variables, screen
contents, parser state and queues. They work with an attached or detached server
without taking or disconnecting its displayed-client lease. Detached selection
sets the focus used on the next attachment and by the next snapshot save.
Selecting the current window preserves last-window history; selecting the
current pane emits no duplicate application focus event.

For an attached client, successful selection dismisses History, help and prompts
and returns shortcut handling to Locked mode, including selection of the current
target. It forces a redraw through the existing frontend refresh path. Input
subsequently handled by the server goes to the selected pane; bytes already
queued for a child stay with that child. Interactive input and control requests
arrive independently, so the command is not a barrier for physical keystrokes
or completion of a displayed frame.

Application input modes and supported focus-in/focus-out reports follow the
selected screen. Zoom stays enabled: selecting a different pane transfers the
enlarged view to it and restores the old target's tiled PTY/screen dimensions.
Size changes use the existing screen reflow and bounded history behavior.
The runtime synchronizes sizes after accepted controls, both attached and
detached. A resize I/O failure uses ordinary runtime cleanup instead of continuing
to display inconsistent geometry. Successful replies acknowledge accepted focus;
use `pane list` for observed focus and wait for actual terminal frames when
coordinating independent interactive operations.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

This increment follows reviewed `attach --create` commit `d322483`. `main` at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks these commands.
This adds a scripting capability beyond current `main` without importing its
implementation. It retains the human-track control socket, bounded request and
response handling, runtime pane identities and existing window selection model.
The displayed-client protocol remains unchanged.

Read the CLI/request variants and frontend-refresh classification in
`src/control.rs`, then selection in `src/terminal/control.rs` and size/focus
handling in both event loops in `src/terminal.rs`. CLI unit coverage checks
explicit arguments, pane ID zero and one-based window numbers.

`tests/terminal_loop_focus_control.py` uses real attached terminals, named servers,
independent control clients and raw application probes. It checks cross-window
pane selection, remembered pane/last-window focus, reordered window numbering,
keyboard routing, unchanged PIDs and shell variables, zoom size transfer and
restoration, invalid-target/zero-wire-request isolation, overlay dismissal and
prefix reset, detached selection/snapshot focus, retained exited panes,
application cursor modes, actual focus/blur bytes, absence of duplicate focus
events and terminal restoration.

This branch awaited owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 905 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 26 real PTY scenarios passed, including script focus, attached/detached
  controls, zoom, snapshots, session management and lifecycle handling.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
- An earlier full run failed in the existing Session Manager scenario during
  its first client attachment with `ERANGE` (Result too large), before any
  terminal frame. The isolated manager scenario and final four-thread full run
  passed. The cause remains undetermined; a similar failure was already recorded
  in the earlier Session Manager disconnect review.
