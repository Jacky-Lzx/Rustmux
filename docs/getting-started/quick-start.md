# Build and Run

Use a Rust toolchain supporting edition 2024. From the `main-human` checkout:

```sh
cargo build --locked
./target/debug/rustmux
```

Run inside a terminal with nonzero dimensions; stdin and stdout must refer to
the same terminal. Rustmux opens one interactive shell, forwards keyboard input
and displays its output. It uses `RUSTMUX_SHELL`, then the top-level `shell`
value in `~/.config/rustmux/config.toml`, then `SHELL`, then `/bin/sh`.
`XDG_CONFIG_HOME` replaces `~/.config` when set. An invalid selected executable
reports an error without silently falling back.

```toml
shell = "/opt/homebrew/bin/fish"
```

`RUSTMUX_SHELL=/bin/sh ./target/debug/rustmux` remains available as a temporary
override. Changes take effect the next time Rustmux starts.

Running without arguments keeps the foreground-only behavior. To create a named
session that survives terminal detachment, or reconnect to it later, use:

```sh
./target/debug/rustmux new work
./target/debug/rustmux new --detached background
./target/debug/rustmux attach work
./target/debug/rustmux attach
./target/debug/rustmux list
./target/debug/rustmux kill-all --yes
```

Inside a named session, Ctrl-B followed by `d` detaches and restores the outer
terminal while its panes continue running. Only one client displays a session at
a time. Its name appears before the window labels in the top bar and remains the
same after reattachment. The endpoint is removed when the last pane exits. See
[Named Session Commands](../reference/session-cli.md).

Ctrl-B followed by Ctrl-W opens the Session Manager from an attached session.
The current session is selected initially; choose another session with `j`/`k`
and Enter, or press Esc or `q` to return to the current session.

When more than one session is running, `rustmux attach` without a name opens a
centered session window. It shows each session's attached/detached state and
server PID. Move with Up/Down or `j`/`k`, attach with Enter, and cancel with Esc,
`q` or Ctrl-C. Press `a` to enter a new session name and create it with Enter.
Press `d` twice consecutively to terminate the selected session; any intervening
key cancels the first `d`. A single running session is attached directly.

Starting another interactive Rustmux or attaching a session from inside a pane
is rejected before terminal modes change. Session inspection and control commands,
and `new --detached`, remain available inside panes.

Type commands normally. Ctrl-C reaches the inner terminal rather than terminating
Rustmux itself. Type `exit` or use the shell's EOF key to close the current pane. Its final pane closes the window. When the last
window closes, its exit status is returned to the caller. Shell output is drained before normal exit, and
the outer terminal modes and previous screen are restored.

Use Ctrl-B followed by `c` to create a window, `n` for the next window and `p`
for the previous one. Ctrl-B followed by `1`–`9` selects that window number;
`0` selects window 10. Ctrl-B followed by Tab returns to the last active window;
repeat it to toggle between two windows. Ctrl-B followed by `<` or `>` moves
the current window one position left or right in the bar, wrapping at the edges. Ctrl-B twice sends a literal Ctrl-B to the child. Background
shells continue running. New windows and splits inherit the active pane's valid
OSC 7 directory. Without OSC 7, macOS and Linux query the foreground process,
then the shell process; Yazi's foreground directory overrides stale OSC 7. See
[Windows](../reference/windows.md#interactive-controls) for limits and input
behavior. Ctrl-B followed by `,` opens the name editor:
Enter saves, Esc cancels, and Ctrl-U clears the existing name.
Ctrl-B followed by `&` asks to close the active window: type `yes` and Enter to
force close, or Esc to cancel. Unsaved work in that window can be lost. The top bar shows the saved name and highlights the active window.

Use Ctrl-B followed by `%` to split left/right, or `"` to split top/bottom.
The new pane is selected. Ctrl-B followed by lowercase `h/j/k/l` selects the pane
to the left/down/up/right; `o` cycles through all panes. Directional selection
stops at the boundary. Ctrl-B followed by `Z` enlarges the active pane to the
content area; repeat to restore the split view. Focus shortcuts work while zoomed. Ctrl-B followed by Ctrl-h/j/k/l moves the
nearest separator left/down/up/right by one cell. Ctrl-B `{` / `}` swaps the active
pane with the previous/next layout position, wrapping at the edges and keeping
focus on the same shell. Resizing and swapping are disabled while zoomed.
Ctrl-B `!` moves the active pane into a new window without restarting its shell
or foreground program; Ctrl-B Tab returns to the source window.
Ctrl-B `x` asks to hide the active pane and stop its foreground job: type `yes`
and Enter to confirm, or Esc to cancel. Ctrl-B `z` restores the last hidden pane
with its original shell; only one closed pane is retained. Unsaved work in the
stopped foreground program is lost. Closing the last visible pane exits the application; Ctrl-B `&` still permanently closes a window. See [Interactive Splits](../reference/interactive-splits.md).

The CLI now parses shell output and renders its own screen model. Window changes
resize both model grids and the PTY in every window. The bar reserves one row;
panes share the remaining content area, with one-cell separators. The bar
is hidden when only one row is available. Outer dimensions must fit within
65,536 cells.
Primary-screen rows scrolled out of the grid enter bounded
[history storage](../reference/scrollback.md). Use Ctrl-B `[` to browse a frozen snapshot; `k/j` move, `g/G` jump, and `q`
returns to live output. In history mode, `/` opens literal search; Enter searches,
`n/N` cycle matches, and Ctrl-C cancels query editing. Height shrink keeps the primary cursor visible and archives
rows moved off the top. Growing restores the newest retained history above the
current content. Width changes reflow primary text and retained history;
alternate-screen applications retain the clipping policy. See
[Browsing History](../reference/history-view.md).
Ctrl-B `E` opens the active pane's retained history and meaningful visible text
in `$VISUAL`, `$EDITOR` or `vi` in a temporary `history` window.
Ctrl-B `e` opens the last completed command's plain-text output in a temporary
`output` window. OSC 133 provides exact boundaries; without it, Rustmux uses
command echo and the following prompt as best-effort boundaries.

Only the documented terminal-control subset is supported. Full-screen editors,
terminal queries and extended keyboard/mouse modes are not yet fully supported;
see [Input and Rendering Loop](../reference/input-loop.md#current-compatibility).
