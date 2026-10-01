# Script pane zoom

```sh
rustmux list-panes -s work --toml
rustmux zoom-pane -s work -p 2 --on
rustmux zoom-pane -s work -p 2 --off
rustmux zoom-pane -s work
```

`zoom-pane` selects a visible runtime pane and activates its window, then sets
or toggles that window's zoom. It accepts `-s SESSION` (default `default`) and
optional `-p ID` (default the active pane). Enumerate IDs before using them;
they remain stable within a running server but are not persistent identifiers
across restoration. Hidden interactive undo panes are not targets.

`--on` enables the selected pane's full-window view. `--off` restores the
underlying split layout. Omission of both flags toggles the window's current
zoom state; it does not test whether the supplied pane was already selected.
The flags conflict. Repeating an explicit state does not reverse the view.
Every successful operation selects the supplied target, including `--off`;
specifying a pane in an inactive window activates that window. Other windows'
zoom, layouts and remembered pane selection are preserved.

Zoom belongs to the window, so `--on` on a hidden sibling transfers the full
view to it. The previous target returns to its underlying tiled dimensions.
A window with only one pane already fills its available area: all three forms
succeed and leave its layout unzoomed. Visible retained exited panes are also
valid targets; zoom does not respawn them or change their exit state.

Successful commands return zero and print nothing. Unknown/stale pane IDs,
invalid CLI arguments and malformed wire requests return a nonzero status
without changing focus, zoom or overlays. Validation happens before selecting
the target. The command preserves owned child processes, runtime IDs, pane
contents, terminal modes and input/output queues.

The existing control refresh synchronizes PTYs and screens across all windows,
both attached and detached. The selected pane gains the full content rectangle
when zoomed; hidden siblings retain their tiled sizes. Unzoom restores the split
sizes and uses the existing screen reflow/history rules. Size synchronization
I/O failures follow runtime cleanup. A successful reply acknowledges accepted
state, not completion of application SIGWINCH handling or a displayed frame.
Poll actual application output or terminal frames when coordinating them.

Attached clients dismiss History/help/prompts, return shortcuts to Locked mode
and redraw, including repeated explicit states and single-pane no-ops. Focus
events follow the normal transition: enabled old/new panes receive one
focus-out/focus-in respectively. Keeping the same active pane emits no duplicate
focus event. Input handled after the operation goes to the selected pane and
application input modes follow it. Already queued input remains with its child;
independent physical input and control requests are not ordered by the command.
Window selection uses the existing last-window model.

Detached operations set the view and selection used by the next attachment.
Subsequent manual or configured automatic snapshots record each surviving
window's zoom and selected pane, plus the active window. The command itself
does not force a snapshot write. Restore restarts recorded startup commands;
it does not resume the old processes. Handshake and snapshot formats are unchanged.

## Review and verification

The base is reviewed `close-window` commit `94665e4`, merged into `main-human`.
Both tracks already support interactive pane zoom. `main` at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks this script command.
This implementation reuses pane target lookup, window selection, layout zoom
and the existing control refresh. Older servers reject the new action until
restarted.

Read CLI/request mapping and refresh classification in `src/control.rs`, then
target validation and zoom application in `src/terminal/control.rs`. The
`PaneSet`/`Layout` models provide selection and zoom; existing event loops
synchronize sizes, route focus events and redraw.

CLI coverage checks default toggle, pane ID zero, explicit states and conflicting
flags. `tests/terminal_loop_pane_zoom.py` uses real named servers, actual raw
application PTYs and physical terminal input. It checks nested tiled/full sizes,
hidden sibling transfer, cross-window selection, repeated explicit state without
inversion or duplicate focus events, single-pane no-ops, cursor modes and input
routing, invalid/stale/malformed request isolation, History and prefix refresh,
default toggling, retained exited targets without respawn, detached operations,
stable live process identities and server PID, and saved selection/zoom restoration
with fresh startup processes. Client exits verify restored outer terminal attributes.

This branch awaits owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01, using Rust 1.99.0:

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  929 passed, zero failed, six existing ignored tests, across 47 test targets.
- All 33 real PTY scenarios passed, including scripted zoom, window/pane
  closure, snapshot restoration, session management and terminal-device faults.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`
  passed.
- `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.
