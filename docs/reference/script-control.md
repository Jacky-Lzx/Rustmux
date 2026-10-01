# Script Control

Control a named running session while attached or detached, without taking its
interactive client lease. Each command accepts `-s SESSION` (default `default`).
Pane commands accept `-p ID`; omission selects the current active pane.

```sh
rustmux list-panes -s work --toml
rustmux new-window -s work --name logs
rustmux split-pane -s work -p 0 --down
rustmux send-keys -s work -p 0 --literal --enter 'printf "hello\n"'
rustmux send-keys -s work -p 0 'Ctrl c'
rustmux capture-pane -s work -p 0 --history
rustmux respawn-pane -s work -p 0
rustmux join-pane -s work -p 0 --to-pane 1 --down
rustmux break-pane -s work -p 0 --name editor
rustmux save-session work
```

Enumerate IDs before using them. They identify owned panes within the running
server and remain unchanged across joins, breaks and interactive pane moves.
IDs are not saved; enumerate again after restoring a workspace. Hidden undo panes
are not command targets.

`list-panes` prints tab-separated ID, one-based window number, title and working
directory. Control characters in text fields are replaced with spaces.
`--toml` prints a `panes` array with `id`, `pid`, `window`, `window_name`, `active`,
`selected`, `title` and an optional `directory`. `selected` means focused within
that window; `active` also requires that window to be active. Exit state and
restart semantics are described in [Retained Panes and Respawn](pane-lifecycle.md).

New windows and splits gain focus and print their new pane ID. A split inherits
the specified pane's directory. Joins require two different windows and preserve
the moved process, screen and ID; an emptied source window is removed. Breaks
also preserve them. Breaking a window's sole pane is a no-op returning its ID.
Invalid IDs, invalid names, exhausted limits and impossible splits return a
nonzero exit status. Captures and input requests do not change focus.
Temporary history/output editor panes cannot be split or moved by script.

`send-keys --literal` joins arguments with spaces. Named keys accept a single
Unicode character, `Ctrl c`-style control keys, Enter, Tab, Esc, Space, Backspace,
Delete, arrows, Home, End, PageUp and PageDown. Arrows use conventional CSI
sequences. `--enter` appends CR. The entire request is limited to 4096 input
bytes. A full pane input queue rejects the request before inserting any bytes.
Success means queued input, not completion of the shell command.

`capture-pane` returns plain visible text. `--history` includes retained primary
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

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 854 passed,
  zero failed, six existing tests ignored by default.
- Focused real PTY control and project scenarios passed, including server-side
  input rejection and fragmented requests.
- Rust formatting, Clippy with warnings denied and the mdBook build passed.

This review branch is implementation evidence; its final changes await the
owner's review. Linux CI and installed-client validation were not performed.
