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
./target/debug/rustmux list --long
./target/debug/rustmux kill-all --yes
```

Inside a named session, Ctrl-B followed by `d` detaches and restores the outer
terminal while its panes continue running. Only one client displays a session at
a time. Its name appears before the window labels in the top bar and remains the
same after reattachment. The endpoint is removed when the last pane exits. See
[Named Session Commands](../reference/session-cli.md).

Ctrl-B followed by Ctrl-W opens the Session Manager from an attached session.
The current session is selected initially; choose another session with `j`/`k`
and Enter, or press Esc or `q` to return to the current session. The current
session stays first and is marked `CURRENT`; other attached sessions precede
detached sessions. Recent connections sort first within each group.

When more than one session is running, `rustmux attach` without a name opens a
centered session window. It shows each session's attached/detached state and
server PID. Attached sessions appear before detached sessions, with each group
ordered by most recent connection and then name. Wider terminals also show the
last connection age. Move with Up/Down or `j`/`k`, attach with Enter, and cancel with Esc,
`q` or Ctrl-C. Press `/` to search names without regard to case; use Up/Down to
move through results, Tab to complete the selected name, and Esc to clear the
search. Press `a` to enter a new session name and create it with Enter. Press `d`
twice consecutively to terminate the selected session; any intervening key
cancels the first `d`. A single running session is attached directly.

`rustmux list` keeps its script-friendly name-only output. Use `rustmux list
--long` or `rustmux ls -l` for a table containing connection state, server PID
and last connection time.

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
behavior. The right side of the bar changes from red `LOCKED` to green `NORMAL`
after Ctrl-B and returns to `LOCKED` after the shortcut. Ctrl-B followed by `,`
opens the name editor:
Enter saves, Esc cancels, and Ctrl-U clears the existing name.
Ctrl-B followed by `&` asks to close the active window: type `yes` and Enter to
force close, or Esc to cancel. Unsaved work in that window can be lost. The top
bar shows the saved name and highlights the active window; left-click a visible
window label to select it, or scroll over the bar to move between windows.
The bottom bar shows shortcuts for the current input mode. Click a visible key
or label to run server-side window and pane actions; grouped hints use the exact
key clicked, while their label selects the first key. Inside a named session,
click `Ctrl-B Ctrl-W Sessions` to open the Session Manager; local unnamed
processes omit that hint. Press Ctrl-B followed by `?`, or click `? Help` after
Ctrl-B, to open the complete shortcut reference. Esc, `q` or `?` closes it;
pressing or clicking a listed command closes Help and runs that action. Unknown
keys and pasted input stay inside the panel instead of reaching the shell. Use
Left/Right, Page Up/Page Down or the mouse wheel when the commands span pages.

Use Ctrl-B followed by `%` to split left/right, or `"` to split top/bottom.
The new pane is selected. Ctrl-B followed by lowercase `h/j/k/l` selects the pane
to the left/down/up/right; `o` cycles through all panes. Directional selection
stops at the boundary. You can also left-click a pane or its border to select it.
Ctrl-B followed by `Z` enlarges the active pane to the
content area; repeat to restore the split view. Focus shortcuts work while zoomed. Ctrl-B followed by Ctrl-h/j/k/l moves the
nearest separator left/down/up/right by one cell; you can also drag a separator
with the left mouse button. Ctrl-B `{` / `}` swaps the active
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
panes share the remaining area inside box-drawing frames, with a separate border
for each pane on both sides of a split. Pane titles follow OSC 0/2 and otherwise
read `shell`.
The bar is hidden when only one row is available, and outer borders are omitted
along dimensions too small to contain them. Outer dimensions must fit within
65,536 cells. Green borders outline the focused pane.
Primary-screen rows scrolled out of the grid enter bounded
[history storage](../reference/scrollback.md). Use Ctrl-B `[` to browse a frozen snapshot; `k/j` move, `g/G` jump, and `q`
returns to live output. Its Green border changes to Peach while history
is open. In history mode, `/` opens literal search; Enter searches,
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
While browsing history, drag the left mouse button across text to copy that
selection through OSC 52 when the button is released.

Only the documented terminal-control subset is supported. Full-screen editors,
terminal queries and extended keyboard/mouse modes are not yet fully supported;
see [Input and Rendering Loop](../reference/input-loop.md#current-compatibility).
