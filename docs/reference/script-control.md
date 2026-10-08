# Script Control

Control a named running session while attached or detached, without taking its
interactive client lease. Pane operations belong to `rustmux pane` and window
operations belong to `rustmux window`. Use `rustmux pane --help` and
`rustmux window --help` to list their operations. The previous flat pane/window
commands have been removed.
Inspecting the running server's settings uses `rustmux config show -s SESSION`;
see [Configuration diagnostics](config-diagnostics.md) for configuration commands.

Each operation accepts `-s SESSION` (default `default`) after its name, for
example `rustmux pane list -s work`. The global `-c PATH` / `--config PATH` option
can appear before the group, between the group and operation, or after the operation.
Operations targeting an individual pane accept `-p ID`; most default to the
current active pane.
`pane select` requires an ID or a direction.

```sh
rustmux config show -s work
rustmux pane list -s work --toml
rustmux pane select -s work -p 0
rustmux pane select -s work --direction right
rustmux pane select -s work -p 0 --direction down
rustmux window select -s work -w 2
rustmux window rename -s work -w 2 'build logs'
rustmux window move -s work -w 2 --direction left
rustmux window new -s work --name logs
rustmux window new -s work --name checks --cwd /absolute/project --command 'cargo test'
rustmux pane split -s work -p 0 --down
rustmux pane resize -s work -p 0 --direction right --cells 3
rustmux pane zoom -s work -p 0 --on
rustmux pane zoom -s work --off
rustmux pane swap -s work -p 0 --to-pane 2
rustmux pane move -s work -p 0 --direction right
rustmux pane send-keys -s work -p 0 --literal --enter 'printf "hello\n"'
rustmux pane send-keys -s work -p 0 'Ctrl c'
rustmux pane capture -s work -p 0 --history
rustmux pane respawn -s work -p 0
rustmux pane close -s work -p 2
rustmux window close -s work -w 2
rustmux pane join -s work -p 0 --to-pane 1 --down
rustmux pane break -s work -p 0 --name editor
rustmux save-session work
```

Enumerate IDs before using them. They identify owned panes within the running
server and remain unchanged across joins, breaks and interactive pane moves.
IDs are not saved; enumerate again after restoring a workspace. Hidden undo panes
are not command targets.

`pane select -p ID` focuses the runtime pane and its owning window.
`pane select [--pane ID] --direction left|right|up|down` instead selects that
origin's nearest geometric neighbor in the same window; omitting the ID uses
the active pane. It preserves pane positions and zoom, and returns an error
without changing focus if there is no neighbor. See
[Script Directional Focus](script-directional-focus.md) for selection rules,
focus events and snapshots.
`window select -w NUMBER` selects the one-based position reported by `pane list`
and the window bar, preserving the selected pane within that window. It requires
an explicit number. Both commands print nothing on success. Window numbers follow display
order and can change after window removal or reordering; runtime pane IDs remain
stable. See [Script Focus Control](script-focus.md) for zoom, input and overlay
behavior.

`window rename [-w NUMBER] NAME` changes a window label without selecting it.
Omitting the number targets the active window. Names accept 1–128 UTF-8 bytes
without control characters; duplicate names are allowed. Numbers follow display
order. Successful requests redraw the bar and reset overlays and shortcuts like
the other mutating script controls. See [Script Window Rename](script-window-rename.md)
for validation, focus and snapshot behavior.

`window close [-w NUMBER]` removes a whole window and cleans up every directly
owned pane child in it. Omission targets the active window. Numbers follow the
current display order. The session's final window is preserved with an error,
even if it has several panes; use `kill SESSION` to end the whole session. See
[Script Window Close](script-window-close.md) for focus, cleanup and persistence.

`window move [-w NUMBER] --direction left|right` moves a window one position
in display order. Omission targets the active window. Movement wraps from the
left edge to the end, or from the right edge to the start, preserving the order
of other windows. Active identity and last-window history are preserved even
when moving an inactive target. Re-enumerate numbers after moving. See
[Script Window Move](script-window-move.md) for layouts, refresh and snapshots.

`pane close [-p ID]` removes a visible pane and cleans up its directly owned
child. Omission targets the active pane. Closing a window's only pane removes
that window; the session's final pane is preserved with an error, so use
`kill SESSION` to end the whole session. It does not enter interactive close-undo
storage. See [Script Pane Close](script-pane-close.md) for focus, retained panes
and persistence.

`pane resize [-p ID] --direction left|right|up|down [--cells N]` moves the
nearest separator of the matching orientation, including in an inactive window,
without selecting the target. Directions describe separator movement; `right`
grows the left subtree and shrinks the right subtree. The target defaults to the
active pane and the movement defaults to one cell. Requests clamp to valid pane
content sizes; zoom, a missing separator or an already reached boundary returns
a nonzero status. See [Script Pane Resize](script-pane-resize.md) for limits,
PTY synchronization and persistence.

`pane zoom [-p ID] [--on | --off]` selects a pane and its window, then sets or
toggles that window's full-pane view. Omission targets the active pane; omitting
both flags toggles. Explicit states can be repeated without reversing the view.
A single-pane window already fills the area and stays unzoomed. See
[Script Pane Zoom](script-pane-zoom.md) for focus, actual PTY sizes and snapshots.

`pane swap [-p ID] --to-pane ID` exchanges two positions within one unzoomed
window. Omission uses the active pane as the source; both targets must be in the
same window. Focus, processes and IDs are preserved even for inactive targets;
the applications resize to their new slots. Swapping a pane with itself succeeds
without changing layout. See [Script Pane Swap](script-pane-swap.md) for validation,
retained output and saved geometry.

`pane move [-p ID] --direction left|right|up|down` exchanges the source with its
nearest geometric neighbor in the same window. Omission uses the active pane.
It preserves focus and process identity, including in inactive windows; each
application resizes to its new slot. Movement does not wrap. Missing neighbors,
zoomed layouts and temporary editor targets return a nonzero status without
changing the layout. See [Script Pane Move](script-pane-move.md) for neighbor
selection, validation and snapshots.

`config show` returns the running server's applied scalar settings and reload
status as TOML without changing focus or dismissing overlays. See
[Configuration Hot Reload](config-reload.md) for source selection, pending
updates and error handling.

`pane list` prints tab-separated ID, one-based window number, title and working
directory. Control characters in text fields are replaced with spaces.
`--toml` prints a `panes` array with `id`, `pid`, `window`, `window_name`, `active`,
`selected`, `title` and an optional `directory`. `selected` means focused within
that window; `active` also requires that window to be active. Exit state and
restart semantics are described in [Retained Panes and Respawn](pane-lifecycle.md).

New windows and splits gain focus and print their new pane ID. A split inherits
the specified pane's directory. Both commands accept `--cwd /absolute/directory`
to override inheritance and `--command 'shell command'` to launch a recorded job.
Omitting the command starts an interactive shell. Startup commands are retained
for respawn and workspace restoration; see [Script Pane Startup](script-pane-startup.md).
Joins require two different windows and preserve
the moved process, screen and ID; an emptied source window is removed. Breaks
also preserve them. Breaking a window's sole pane is a no-op returning its ID.
Invalid IDs, invalid names, exhausted limits and impossible splits return a
nonzero exit status. Captures and input requests do not change focus.
Temporary history/output editor panes cannot be split or moved by script.

`pane send-keys --literal` joins arguments with spaces. Named keys accept a single
Unicode character, `Ctrl c`-style control keys, Enter, Tab, Esc, Space, Backspace,
Delete, arrows, Home, End, PageUp and PageDown. Arrows use conventional CSI
sequences. `--enter` appends CR. The entire request is limited to 4096 input
bytes. A full pane input queue rejects the request before inserting any bytes.
Success means queued input, not completion of the shell command.

`pane capture` returns plain visible text. `--history` includes retained primary
history when the primary screen is active; alternate-screen captures return that
application's visible screen. Soft wraps are joined, explicit trailing spaces and
Unicode are preserved, and styling is omitted. Captures describe the currently
parsed model, not a synchronization barrier for preceding commands. Poll for an
expected marker when coordinating asynchronous output.

## Transport and review

A private `.control` socket carries one length-prefixed TOML request/response per
connection. The interactive handshake and `.save` protocol stay compatible.
At most four clients are retained, each with a two-second request/reply deadline.
Request bodies are bounded to 64 KiB, total queued replies to 16 MiB, and each
client receives one bounded read per event-loop tick. Captures reserve space for
worst-case TOML escaping and reject output beyond the bounded response size.
Malformed requests belong to that controller; they do not stop the server.
The client uses a five-second response timeout. Private endpoint validation and
inode-aware cleanup follow the named-session rules.

Reading order: `src/control.rs`, `src/terminal/control.rs`, event-loop integration
in `src/terminal.rs`, pane identity in `src/pane.rs`, then CLI dispatch.
`cargo test --test terminal_loop control` verifies attached/detached operations,
malformed and idle controllers, bounded input, failed ID isolation and stable
process/ID behavior after joins and breaks.

[Project layouts](project-layouts.md) cover startup files and recorded commands.
Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 860 passed,
  zero failed, six existing tests ignored by default.
- Focused real PTY control and project scenarios passed, including server-side
  input rejection and fragmented requests.
- Rust formatting, Clippy with warnings denied and the mdBook build passed.

This review branch is implementation evidence; its final changes await the
owner's review. Linux CI and installed-client validation were not performed.

Raw PTY bytes are available through `pane read-output`, `pane subscribe` and
`pane log`. Continuous clients require retained panes and report buffer loss;
see [pane output](pane-output.md) for cursors, bounds and lifecycle behavior.
