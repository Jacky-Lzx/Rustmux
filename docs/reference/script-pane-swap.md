# Script pane swap

```sh
rustmux list-panes -s work --toml
rustmux swap-pane -s work -p 0 --to-pane 2
rustmux swap-pane -s work --to-pane 0
```

`swap-pane` exchanges the layout positions of two visible runtime panes in the
same window. It accepts `-s SESSION` (default `default`), optional `-p ID`
(source, default the active pane), and required `--to-pane ID` (destination).
Enumerate runtime IDs before using them. They remain stable within the server
and after a swap, but are not persistent identifiers across restoration.
Hidden panes stored for interactive undo are not targets.

The command preserves global active window identity and each window's remembered
selected pane. It does not select either target. An inactive window's panes can
be swapped without activating it. If an active pane participates, focus follows
that same identity into its new slot. Input subsequently handled by the server
continues to reach the same active child and use its application modes. A swap
does not emit focus-in/focus-out reports because focus identity is unchanged.

The split tree's slots, separator positions and proportions are unchanged; only
their leaf pane identities are exchanged. Each pane retains its process, runtime
ID, screen, history, parser state, startup command and input/output queues. The
existing control refresh resizes PTYs and reflows screens into the new slots,
including in inactive windows. Unequal slots can change both applications'
dimensions. Unaffected panes retain their sizes. Exited retained panes can be
swapped without respawning them or losing their recorded exit state/output.

Success returns zero and prints nothing. Swapping a pane with itself in an
unzoomed layout succeeds as a no-op, including single-pane windows. Successful
requests dismiss History/help/prompts, reset shortcuts to Locked mode and redraw
an attached client, including same-target no-ops. Replies acknowledge accepted
layout changes, not completion of application SIGWINCH handling or displayed
frames. Independent physical input and control requests are not ordered by a swap.
Size synchronization I/O failures follow the normal runtime cleanup path.

Unknown/stale IDs, malformed requests, panes in different windows, temporary
history/output editor panes and zoomed target layouts are rejected before layout
mutation, leaving focus, geometry and overlays unchanged. Zoom rejection also
applies to same-target requests. Use `zoom-pane --off` on the target window before
swapping. Cross-window movement remains available through `join-pane`/`break-pane`;
this command only exchanges slots within one window.

Attached and detached servers accept swaps without acquiring the interactive
client lease. The next manual or configured automatic snapshot records exchanged
pane positions, startup commands and the unchanged remembered selection. A swap
does not force a disk write. Restoration recreates that geometry with fresh
startup processes; it does not resume the old children. The handshake and
snapshot formats are unchanged. Interactive close-undo storage is unaffected.

## Review and verification

The base is reviewed `move-window` commit `16485a2`, merged into `main-human`.
Both tracks already support interactive pane movement; `main` at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks this script command.
The implementation reuses runtime target lookup, the existing leaf-exchange
operation and normal control refresh. It validates both targets before changing
the layout and never transfers owned contents between pane IDs. Older servers
reject the new action until restarted.

Read CLI/request mapping and refresh classification in `src/control.rs`, target
and eligibility validation in `src/terminal/control.rs`, then `swap_panes` in
`src/pane_set.rs`/`src/layout.rs`. Existing event loops synchronize sizes and redraw.

CLI coverage checks omitted/explicit source, runtime ID zero and required valid
destination. The layout test checks exact slots/separators, preserved active
identity, inverse swaps, same-target no-ops, closed-ID isolation and zoom rejection.
Existing pane ownership tests cover content membership and lifetime.
`tests/terminal_loop_pane_swap.py` uses real PTYs and raw applications. It checks
unequal active/inactive swaps, unchanged live PIDs and focus bytes, screen contents
remaining with their runtime IDs, physical input and cursor mode, same-target
refresh, stale/malformed/cross-window/zoom failure isolation, History and prefix
refresh, detached swaps, retained output/exit status without respawn, and saved
selection/geometry restoration with fresh startup processes. Client exits verify
restored outer terminal attributes.

This branch awaits owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01, using Rust 1.99.0:

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  935 passed, zero failed, six existing ignored tests, across 47 test targets.
- All 35 real PTY scenarios passed, including targeted pane swaps, interactive
  movement, zoom, window/pane closure, snapshots and session management.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`
  passed.
- `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.
