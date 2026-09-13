# Interactive Splits

Each window starts with one shell. Splitting creates an independent shell using
Rustmux's startup executable and working directory, then selects the new pane.
Existing processes, parser state and queued input stay attached to their pane IDs.

| After Ctrl-B | Action |
| --- | --- |
| `%` | Split left/right; select the right pane |
| `"` | Split top/bottom; select the bottom pane |
| `h` / `j` / `k` / `l` | Focus left / down / up / right |
| `o` | Cycle through layout traversal order, wrapping |
| Tab | Return to the last active window |

Directional keys are lowercase, do not move diagonally and stop at boundaries.
The former last-window binding `l` now means right-pane focus. Plain Tab still
reaches the shell; these commands only apply after Ctrl-B. Bracketed paste payload
is forwarded unchanged, including shortcut bytes.

Splits share space approximately equally and reserve a one-cell separator.
The [layout model](split-layout.md) describes odd sizes and nested minimum sizes.
The CLI synchronizes each child PTY and screen with its rectangle after splitting,
outer resize and pane removal. New windows use the full content area even when
the selected pane in the old window is smaller.

All panes continue parsing output and answering terminal queries, including panes
in background windows. The active pane supplies the cursor and input modes; all
visible pane screens are composed into a frame. Normal updates wait while any
pane in that window holds synchronized output, with the existing one-second hold
limit. Explicit focus/layout changes force a frame. Queued frames finish before
a replacement is written. Mouse coordinates are translated into the active pane;
other-pane/separator presses are ignored, and releases clamp to its nearest cell.
Mouse clicks do not change focus.

Use shell `exit` to close a pane. Its final output is drained before removal; in
the active window its final frame is written first. Siblings expand to fill the
vacated area. Closing the focused pane chooses the next surviving layout leaf,
or the preceding one when it was last, and discards unprocessed input from the
exited pane. The last pane closes its window; the last window returns its exit
status. Ctrl-B `&` confirms closing the entire window and all its panes.

## Limits and verification

There are at most 64 panes per window and 16 windows. The physical frame, including
the bar, must fit within 65,536 cells. A split needs at least three cells along
its axis. Rejected geometry or shell creation leaves the existing set unchanged,
with a best-effort bell. A later PTY resize/I/O error ends the CLI and restores
the terminal; operating-system changes are not rolled back.

Resizing the outer terminal below any window's layout minimum currently ends the
CLI with terminal cleanup. There is no small-terminal placeholder, CLI zoom,
manual separator adjustment, or separate force-close-pane prompt yet.

`cargo test` runs the nested-PTY suite. It checks nested splits, retained shell
variables, child-observed dimensions, lowercase directional and cyclic focus,
resize, successive pane exits and final status, split creation failure, and
SGR/X10 mouse translation with out-of-pane filtering. These automated checks do
not replace owner review with interactive applications.
