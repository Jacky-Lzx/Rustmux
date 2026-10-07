# Compact layout

Set the top-level boolean in the session's configuration:

```toml
compact = true
```

The default is `false`. Compact layout keeps the window bar and places the current
input mode at its right edge. It removes the bottom shortcut bar and gives that
row to the panes. Shortcuts and Shortcut Help continue to use the configured
bindings. Labels clip at terminal cell boundaries when space is limited; the mode
badge has no tab click action.

With a 24 × 80 terminal and one framed pane, the child receives 21 × 78 cells in
compact layout, compared with 20 × 78 cells in the standard layout. Child cursor,
colors, terminal modes, and mouse coordinates follow the actual content geometry.
A one-row terminal still dedicates its only row to content.

History status and its search editor use the top row. Window rename and other
local prompts also use the top row, with a visible input cursor and action hints
when they fit. Save and configuration errors replace the top row until resolved;
covered tab labels have no mouse action. These displays do not overwrite the last
pane row.

## Configuration and reload

`default-config` exports `compact = false`. `check-config --toml` and a running
server's `show-config` report `[settings].compact`; values other than TOML
booleans are rejected. For example:

```sh
rustmux check-config --config ./work.toml --toml --strict
rustmux show-config -s work
```

An attached named session uses the server's layout setting. Configuration reload
uses the existing safe boundary: a local prompt, History, Help, or unfinished
input defers the update. A detached server can apply it while waiting for a client.
New windows created through the control socket use the current layout setting.

Before applying a layout change, Rustmux checks every window, including inactive
and zoomed panes. If a smaller canvas cannot retain content in each pane, the
entire configuration update is rejected: applied settings, generation, layout,
and processes are preserved. The error is available in `show-config` and appears
on the active display. Enlarge the terminal or simplify the layout, then edit the
configuration again to retry. Successful updates resize existing PTYs without
respawning shells.

## Saved sessions and project layouts

Snapshots retain the same format and physical terminal dimensions. A tree that
fits only with compact chrome can be saved and loaded. Restore checks all
windows against the new session's actual dimensions and configured layout before
starting any shells; a standard layout needs enough room for its additional
footer. History persistence retains its existing opt-in settings.

Project layouts are likewise constructed using the session's compact setting.
A client reconnecting with a different configuration file still uses the running
server's setting.

## Verification

Screen tests cover child rows, cursor and terminal mode preservation, all mode
badges, wide/narrow tab hitboxes, compact rename, and compact-only snapshot
validation. The real PTY scenario covers the gained row, its mouse coordinates,
Help and History, rename, top-bar clicks, attached and detached reload, control
creation, rejection of an undersized multi-window update, and saved restoration.
