# Interactive Splits

Each window starts with one shell. Splitting creates an independent shell using
Rustmux's startup executable and the active pane's resolved working directory,
then selects the new pane. A missing directory falls back to Rustmux's startup
directory.
Existing processes, parser state and queued input stay attached to their pane IDs.

| After Ctrl-B | Action |
| --- | --- |
| `%` | Split left/right; select the right pane |
| `"` | Split top/bottom; select the bottom pane |
| `h` / `j` / `k` / `l` | Focus left / down / up / right |
| `o` | Cycle through layout traversal order, wrapping |
| `z` | Toggle active-pane zoom |
| Tab | Return to the last active window |

Directional keys are lowercase, do not move diagonally and stop at boundaries.
The former last-window binding `l` now means right-pane focus. Plain Tab still
reaches the shell; these commands only apply after Ctrl-B. Bracketed paste payload
is forwarded unchanged, including shortcut bytes.

Each pane has a complete box-drawing border. Adjacent panes keep separate borders,
so both panes retain their own edge and color. The top edge displays the latest
OSC 0/2 terminal title, falling back to `shell`. Borders belonging to the focused
pane use Catppuccin Mocha Green; unrelated borders retain the muted Subtext color.
Border cells keep the terminal's default background so terminal transparency
remains visible.
The [layout model](split-layout.md) describes odd sizes and nested minimum sizes.
The CLI synchronizes each child PTY and screen with its rectangle after splitting,
outer resize and pane removal. New windows use the full content area even when
the selected pane in the old window is smaller. Outer borders consume one row or
column on each terminal edge; split requests and resizes are rejected before a
pane would lose its final content cell. Extremely small dimensions omit the
corresponding outer-border pair.

All panes continue parsing output and answering terminal queries, including panes
in background windows. The active pane supplies the cursor and input modes; all
visible pane screens are composed into a frame. Normal updates wait while any
visible pane in that window holds synchronized output, with the existing one-second hold
limit. Explicit focus/layout changes force a frame. Queued frames finish before
a replacement is written. Mouse coordinates are translated past the window bar
and pane border into the active pane; other-pane/border presses are ignored, and
releases clamp to its nearest cell.
Mouse clicks do not change focus.

Use shell `exit` to close a pane. Its final output is drained before removal; in
the active window its final frame is written first. Siblings expand to fill the
vacated area. Closing the focused pane chooses the next surviving layout leaf,
or the preceding one when it was last, and discards unprocessed input from the
exited pane. The last pane closes its window; the last window returns its exit
status. Ctrl-B `&` confirms closing the entire window and all its panes.

## Zoom

Ctrl-B `Z` makes the active pane fill the content area beneath the window bar;
press it again to restore the original split tree. A single pane stays unzoomed.
Directional focus still uses tiled geometry, and `o` still cycles all panes.
Changing focus while zoomed enlarges the new target and returns the previous
one to its tiled dimensions. Each window retains its own zoom state.

Hidden panes keep running, parsing output and answering queries at their tiled
sizes. Their dirty screens and synchronized-output holds do not schedule or block
the visible frame. Mouse reports use the zoomed pane's full visible rectangle.
Outer resize updates both the zoom target and all hidden tiled sizes. Zoom still
requires enough terminal space for the underlying split tree.

A successful split or any pane removal exits zoom. Failed splits preserve zoom;
splitting checks the original tiled rectangle, not the enlarged view. Hidden
panes can exit without displaying a final frame; visible panes drain their final
frame before removal. Focus changes, zoom and unzoom force a complete redraw after
any already queued frame finishes. PTY resize errors follow normal CLI cleanup.

The nested-PTY suite verifies zoomed dimensions, directional and cyclic target
changes, outer resize, restoration of both pane sizes, exit while zoomed,
zoomed mouse coordinates, and terminal-query replies from hidden panes.

## Adjusting pane sizes

Press Ctrl-B, then Ctrl-h/j/k/l to move the nearest left/right or top/bottom
separator one cell left/down/up/right. Each step needs its own prefix. Plain
lowercase Ctrl-B `h/j/k/l` continues to select panes. The resize keys are control
characters, not uppercase letters: keep Ctrl held for the second key.

The nearest ancestor split on the requested axis is adjusted. Direction moves
its separator regardless of which side is active; moving right enlarges the left
subtree and shrinks the right subtree. Nested sibling panes may resize together.
Movement stops at the subtree minimum; it does not continue into an outer split.
With no matching split or while zoomed, the shortcut is a no-op.

Focus, shell processes, parser state and queued input are retained. The CLI
synchronizes all affected PTYs and screen models before redrawing. Primary text
uses the existing resize/reflow policy. Ratios are retained across outer resizing
and zoom/unzoom. Bracketed paste passes the control bytes to the child rather
than invoking these shortcuts; modal history browsing and prompts retain their
own input handling.

The nested-PTY test checks child-observed width and height changes, preserved shell
variables, reverse movement, zoom no-op and terminal restoration after termination.

## Swapping pane positions

Ctrl-B `{` swaps the active pane with the previous pane in layout traversal
order; Ctrl-B `}` swaps it with the next. Both wrap at the ends. Layout order
visits left before right and top before bottom recursively, and can differ from
creation order. Focus follows the original pane into its new position. Use `o`
when you only want to switch focus without moving panes.

Separators and saved split ratios stay in place. Each pane retains its shell,
screen/history, parser state and pending input; only its assigned rectangle
changes. Different-sized destinations invoke the normal screen/PTY resize path,
so retained text remains subject to existing reflow and history limits. No shell
is restarted. Single-pane and zoomed windows ignore swapping. History browsing
and other prompts consume their own input; bracketed paste does not invoke swaps.

The nested-PTY suite checks unequal widths, both swap directions, input following
the same shell, process identity and variables, resulting on-screen placement,
child-observed dimensions, subsequent pane exit and terminal restoration.

## Move a pane into its own window

Ctrl-B `!` moves the active pane into a new window appended to the window bar.
The new window inherits the source window's name and becomes active; Ctrl-B Tab
returns to the source window. A single-pane window is unchanged. At the 16-window
limit the operation is rejected with a best-effort bell.

The existing pane object moves intact: its shell and foreground jobs keep running,
and its parser, screen/history and pending input follow it. Pane IDs are local to
a window, so the destination assigns its own initial pane ID. The source promotes
the removed pane's sibling subtree, preserves the remaining ratios and exits zoom.
The destination is unzoomed and fills its new window. Both windows' PTY/model sizes
are synchronized through the usual resize/reflow path; moved programs receive
SIGWINCH when dimensions change. Already queued output frames finish normally.

Window identity and destination storage are prepared before source ownership is
changed. Validation/allocation errors leave both collections unchanged; later PTY
synchronization errors follow normal terminal cleanup. This action does not consume
or replace the close-undo slot and never stops the moved foreground job.
History mode and prompts consume their own keys; bracketed paste cannot invoke it.

Unit tests cover non-Clone ownership, source geometry, zoom exit, focus/last-window
selection, name inheritance, single-pane no-op and exhausted window IDs. A real
PTY test moves an interactive foreground program, checks its unchanged PID and new
width, retained screen content, source/moved shell variables and subsequent exits.
Use the destination prompt below to move into an existing window.

## Move a pane into an existing window

Ctrl-B `m` opens `Move to window #:`. Enter a window number from the top bar
and press Enter. The active pane is moved into a new split to the right of the
target window's selected pane; focus follows it. The CLI currently uses a
left/right split for this operation. Esc, Ctrl-C and Ctrl-G cancel. Bracketed
paste can fill the number, but a pasted newline does not submit it.

Numbers are bound to stable window IDs when the prompt opens. If a background
window exits while editing, its number cannot silently refer to another window.
An invalid/departed target, insufficient tiled space or the target's pane limit
rejects the move without changing ownership or layout, with a best-effort bell;
the prompt closes. Choosing the source window itself is a no-op.

The moved pane retains its shell, foreground program, parser, screen/history and
queued input. No process is started or stopped. Both source and destination use
normal screen/PTY resize synchronization, including SIGWINCH and primary reflow.
The destination's existing pane stays on the left and the moved pane gets a new
local ID on the right. Its window name and position in the bar remain unchanged.

If source panes remain, their sibling subtree is promoted and Ctrl-B Tab can
return to that source window. Moving the last source pane removes its empty
window. Successful moves exit source/destination zoom; invalid moves retain it.
The destination is validated and reserved before removing content from the source.
Later PTY synchronization errors follow normal terminal cleanup. This operation
does not use or replace the close-undo slot, and can run at the window limit
because it creates no window. History mode consumes its own keys.

Model tests cover both transfer directions, non-Clone content ownership, local ID
allocation, zoom, surviving/empty sources, same-window no-op and atomic rejection
of impossible or stale targets. The nested-PTY test covers cancellation, invalid
numbers, paste protection, uninterrupted foreground PID, changed PTY widths,
retained history and shell variables, source-window removal and terminal cleanup.

## Close and undo

Ctrl-B `x` opens `Close pane? Type yes:`. Type exactly lowercase `yes` and
press Enter to hide the pane and stop its foreground job. Empty or other answers
dismiss the prompt; Esc, Ctrl-C and Ctrl-G cancel. Bracketed paste can fill the
answer, but a pasted newline cannot confirm it. Ctrl-B `&` still permanently
closes a whole window; it does not populate the undo slot.

Ctrl-B `z` restores the last explicitly closed pane and focuses its original
shell. Zoom has moved to **Ctrl-B `Z`**. Undo with no retained pane is a no-op.
There is one slot for the whole application, not one per window. A second close
replaces it and finally closes/kills/reaps the older hidden shell. Undo consumes
the slot; natural pane exits and window closes are not undoable.

The retained pane keeps its PTY, shell PID, working directory, shell variables,
screen and bounded scrollback. A foreground process group distinct from
the shell is killed; unsaved work in that program is lost. When the shell itself
owns the foreground, it receives SIGINT instead. Builtin interruption depends on
the shell's signal handling. `exec` replaces the shell, so stopping an exec'd
program cannot preserve a shell that no longer exists; if the hidden process
exits, the undo slot is discarded. Background jobs and detached descendants are
not promised to stop. This is process retention, not process checkpoint/restart.
After killing a separate foreground job, incomplete parser input is discarded
and supported screen/input modes are reset to the primary screen, keeping its
existing primary history.

Before hiding, the already encoded physical frame finishes. Pending user input
for the pane and already staged outer input are discarded. The visible layout
removes the pane, exits zoom and resizes surviving panes normally. The hidden
PTY participates in readiness polling and is serviced in bounded nonblocking
reads/writes, including terminal-query replies, but is never
rendered or given user keystrokes. It retains its last dimensions until restore.
Output from background jobs may still change its bounded history.

If the original window's split tree and ratios have not changed, undo restores
the original position and ratio at the current outer size. Focus or zoom changes
alone do not prevent this. If the layout changed, the saved pane is inserted
beside that window's current focus using its original split axis, without undoing
newer edits. In that case it receives a new layout ID but retains the same shell.
If the original window is gone, undo creates a window with its saved name.
Insufficient space or a window/pane limit leaves the slot available for retry.
Normal resize/reflow rules and history eviction still apply.

Closing a window's sole pane hides that window if another window remains.
Closing the last visible pane exits with status zero, even if an undo slot exists;
no replacement shell is created. Application exit restores the outer terminal
before cleaning up both visible and hidden shells. Undo is available only while
the application remains running. A target that exits naturally during confirmation follows ordinary
exit handling; confirmation never transfers to another pane.

Unit and nested-PTY checks cover cancellation, paste/input isolation, zoomed close,
foreground job termination, shell PID/variables/directory and history restoration,
cache replacement/reaping, distinct undo/zoom keys, original/fallback geometry,
retry after insufficient size, natural target exit and last-visible-pane behavior.

## Limits and verification

There are at most 64 panes per window and 16 windows. The physical frame, including
the bar, must fit within 65,536 cells. A split needs at least three cells along
its axis. Rejected geometry or shell creation leaves the existing set unchanged,
with a best-effort bell. A later PTY resize/I/O error ends the CLI and restores
the terminal; operating-system changes are not rolled back.

Resizing the outer terminal below any window's layout minimum currently ends the
CLI with terminal cleanup. There is no small-terminal placeholder or mouse-drag
resizing yet.

`cargo test` runs the nested-PTY suite. It checks nested splits, retained shell
variables, child-observed dimensions, lowercase directional and cyclic focus,
resize, successive pane exits and final status, split creation failure, and
SGR/X10 mouse translation with out-of-pane filtering. These automated checks do
not replace owner review with interactive applications.
