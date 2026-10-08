# Floating terminal

A session owns at most one floating shell alongside its ordinary windows.
Normal `i` toggles it when defaults are enabled. The native development
configuration also binds Pane `w`:

```toml
[keybinds.normal]
i = { actions = ["toggle-floating-terminal", { action = "switch-mode", mode = "locked" }], display = "help" }
[keybinds.pane]
w = { actions = ["toggle-floating-terminal", { action = "switch-mode", mode = "locked" }], display = "help" }
```

The first toggle starts the configured shell in the active pane's inherited
working directory. Later toggles reuse that process, screen, history and shell
variables. Hiding it restores the same selected pane in the underlying window;
the floating PTY keeps receiving output and can receive scripted input by runtime
pane ID. Ordinary window selection hides it. Opening it in another window
reuses the same shell over that window without changing window order or numbers.

The frame is centered in the pane canvas, using three quarters of its width and
seven tenths of its height, with small-size clamps. Its border and `floating`
title identify it. The tiled text backdrop remains visible and receives live
output. Child cursor, mouse, paste and keyboard modes belong to the floating
pane while it is visible. Width-two glyphs crossing the overlay edge are clipped.
Outer resize and compact-layout reload update both hidden and visible floating
PTY dimensions. Tiny terminals use the existing border-omission rules.

Split, pane transfer, swap, separator resize, zoom, window reordering and
close-undo actions are unavailable while the floating pane owns keyboard input.
They leave the tiled layout and processes unchanged. Close confirmation removes
the floating shell directly; its normal exit also returns to the underlying
pane, and a later toggle starts a fresh shell. `remain_on_exit` applies to it
like other panes. Closing the final ordinary window still ends the session.

`pane list --toml` includes the floating pane with `floating = true` and
`window = 0`. Ordinary panes have `floating = false` and retain one-based window
numbers. Read output, send keys, select, respawn and close can target its runtime
ID, including while it is hidden. Window-number operations address ordinary
windows.

Detach/reconnect preserves the process and visible state. Session snapshots
save the floating pane separately, including visibility, directory and optional
styled history. Restore starts a new shell and reapplies retained history; it
does not recover shell variables or the previous process. The active ordinary
window and pane remain the return target. Snapshot preflight rejects a floating
record containing a split tree before starting any processes.

Floating frames have fixed proportions; interactive move/resize is not part of
this increment. Background Kitty images are hidden while the floating pane is
visible; images belonging to the floating pane use its centered geometry and
ordinary image relay. History editor actions open an ordinary temporary editor
window. Closing that window uses ordinary window fallback; toggle `i` to return
to the retained floating shell.

Validation includes generic ownership/focus tests, small-size geometry sweeps,
wide-edge composition and input-mode checks, snapshot schema/preflight checks,
and a real PTY scenario for both configured toggles, hidden output, resize,
window switching, detach/reconnect, saved history, restore and shell exit.
