# Windows

The CLI supports multiple terminal windows, each with one or more shell panes. `window::Windows<T>` owns their ordered collection and stable identities.
A top window bar shows names and focus. Basic [interactive splitting](interactive-splits.md)
and [named persistent sessions](session-cli.md) are available. Window renaming
edits the active label directly in the bar.

## Identity and focus

- Creation appends a window and makes it active.
- `WindowId` is stable within its originating collection, independent of the
  window's position and name. IDs start at zero and are never reused, even after
  all windows have been closed. They are not cross-collection or persistent IDs.
- `select` focuses an ID. `select_next` and `select_previous` wrap in current display
  order. An empty collection returns `None`; a single window remains selected.
- Renaming preserves identity, order, contents and focus. Names are opaque
  metadata: empty and duplicate names are allowed. A future UI must handle
  control-character escaping and interactive validation before rendering names.
- Closing an inactive window preserves the active window's identity. Closing
  the active window selects its successor, or its predecessor if it was last.
  Closing the only window leaves the collection empty; creating again works.
- Selecting, renaming or closing an unknown ID returns `NotFound` without changing
  the collection. ID exhaustion fails creation without wrapping or changing focus;
  the supplied content is dropped on this error, as documented by `create`.

## Ownership boundary

`T` has no Clone requirement. It can later contain a PTY, parser and screen.
Switching and renaming operate on metadata and never recreate or clone that
content. `get_mut(id)` lets the event loop process background output without
changing focus. `active_mut()` accesses the currently selected content.

`close` returns the removed `Window<T>` to the caller. It does not kill a process
or wait for it; the caller owns that cleanup decision. Dropping the returned
window drops its content normally, and dropping the collection drops remaining
contents. `into_content` transfers the removed content without cloning it.
No terminal output or other I/O occurs inside this model.

This keeps window identity separate from its owned pane layout. The broader
`main` implementation also includes floating terminals and persistent sessions. This model is not an H05 feature-acceptance
claim. The event loop reads inactive windows, routes keyboard input to the active
window, and synchronizes display and modes when focus changes.

## Verification

`cargo test --test windows` covers cyclic selection, stale IDs, Unicode names,
all three-window focus/removal combinations, empty collection reuse and ownership
transfer. A parser/screen fixture verifies that an incomplete UTF-8 sequence,
private input modes and background output stay with their originating windows.
The unit test in `src/window.rs` exercises the last available ID and exhaustion.
These are model tests, not interactive multi-window or process-preservation tests.

## Per-window terminal contents

`pane::Pane` supplies one `PtyShell`, incremental `Parser` and `Screen`. The CLI
now owns `Windows<PaneSet<Pane>>`, with one owned shell per layout leaf. `Pane::spawn` validates the grid (at most 65,536 cells),
allocates it before spawning, and sets the PTY master nonblocking. Failure after
spawning drops the owned shell, closing the master and reclaiming the direct child.
Follow the existing single-threaded spawning requirement of `PtyShell`.

The caller polls and reads `shell_mut()`, passes received bytes to
`process_output`, and sends generated replies back to that pane's shell. Reserve
reply capacity using `parser::MAX_REPLY_BYTES` before reading. `finish_output`
flushes partial parser input on EOF. Raw shell access is deliberately low-level:
readiness, queue limits, matching PTY/model resize and process status remain the
caller's responsibility. The CLI retains its existing resize, EOF, backpressure,
signal and outer-terminal restoration behavior.

The outer renderer stays outside Pane because it describes the physical output
stream, not an individual child's screen. Switching displayed contents must
therefore account for the previously displayed grid and modes. Each Pane now also owns its child-bound queue, dirty flag, synchronized-output
start time, EOF timestamp and cached exit status. These remain attached to the
same child across focus changes. The physical-terminal output queue, renderer
cache and 6ms frame cadence remain shared in the event loop.

`cargo test --test panes` starts two real shells in `Windows<Pane>`, verifies
nonblocking masters, stable PIDs across selection, background screen updates,
separate parser/mode state and reclamation of one child while the other continues
executing commands. Startup errors and invalid dimensions are also covered.
These ownership tests complement the interactive CLI tests described below.

## Independent I/O state

The crate-private `PaneIo` keeps keyboard bytes and generated terminal replies
in one FIFO for that child, bounded to 64 KiB by the event loop. Read capacity
reserves `MAX_REPLY_BYTES` for every consumed child-output byte; a nearly full
queue can pause child reads while still accepting a smaller amount of input.
Once the child has been reaped, final output may be drained without reserving
reply capacity. EOF stops further input and child reads.

The existing CLI loop now reads and updates this per-pane state rather than
keeping it in local variables. Direct `process_output` marks the pane dirty;
`finish_output` records EOF and the first observation time as well as flushing
partial parser input. The raw shell API remains low-level: lifecycle operations
performed outside the event loop do not automatically update cached status.

Unit tests cover input/reply capacity boundaries, final draining after exit,
and window switching/removal with different queues, synchronization times and
exit states. Existing real-PTY tests cover the actual forwarding and restoration
paths. These states now drive multi-window polling.

## Interactive controls

| Input | Action |
| --- | --- |
| Ctrl-B, then c | Create a shell window and select it |
| Ctrl-B, then n | Select the next window, wrapping |
| Ctrl-B, then p | Select the previous window, wrapping |
| Ctrl-B, then Tab | Return to the last active window |
| Ctrl-B, then & | Confirm closing the active window |
| Ctrl-B, then < / > | Move the active window left / right one position |
| Ctrl-B, then 1–9 | Select the window at that one-based position |
| Ctrl-B, then 0 | Select window 10 |
| Ctrl-B, then , | Rename the active window |
| Ctrl-B, then Ctrl-B | Send one literal Ctrl-B to the active child |
| Ctrl-B, then % / " | Split the active pane left/right or top/bottom |
| Ctrl-B, then [ | Browse current-pane history |
| Ctrl-B, then E | Open current-pane history and visible text in `$VISUAL` or `$EDITOR` |
| Ctrl-B, then e | Open the last completed command's output in `$VISUAL` or `$EDITOR` |
| Ctrl-B, then Z | Toggle active-pane zoom |
| Ctrl-B, then z | Restore the most recently closed pane |
| Ctrl-B, then o | Select the next pane, wrapping |
| Ctrl-B, then h / j / k / l | Select the pane left / down / up / right |
| `exit` in the shell | Close that pane after draining its final output |

Numeric shortcuts follow the current window-bar positions, not stable IDs. Closing
an earlier window shifts later numbers down. Missing positions are ignored and
the shortcut is consumed; selecting the active position keeps focus unchanged.
Only one digit is consumed: Ctrl-B, then `1`, then `0` selects window 1 and sends
`0` to its child. Use next/previous for windows 11–16. Renaming does not change
numbers, and digits inside bracketed paste remain child input.

An unrecognized prefix combination forwards both bytes unchanged. A prefix can
span separate reads and waits for the following byte without a timeout. Ordinary UTF-8 bytes are forwarded immediately. When mouse reporting is enabled,
Escape may be held briefly to recognize a mouse report; see the window-bar rules
below. Bracketed paste markers and
payload are forwarded unchanged, including Ctrl-B combinations inside the paste.
Unbracketed pasted text is indistinguishable from typing and follows the same
shortcut rules. The `new_window`, `split_right`, and `split_down` keys can be
changed under `[shortcuts]` in `config.toml`; see the quick start. Other bindings,
including the Ctrl-B prefix, remain fixed.
Main-style `[keybinds.normal]` can also override the five supported window
operations listed in the quick start. Binding `x` to `close-window` replaces
the prior `x` close-pane behavior for that key.

The left side of the bottom bar shows `LOCKED` in red during ordinary child
input. Pressing Ctrl-B changes it to green `NORMAL` while Rustmux waits for the
second shortcut byte; completing most shortcuts returns it to `LOCKED`.
With a configured NORMAL-to-PANE binding, the badge changes to lavender `PANE`.
Its footer shows the configured break, split, focus, zoom, and close keys that
fit, plus adjacent-window move keys when there is room; clicking one invokes
the same binding. Focus remains in PANE mode, while
structural actions followed by `switch-mode locked` return to LOCKED. PANE
also accepts configured `left/down/up/right` aliases from ordinary CSI or
application-cursor (SS3) sequences; modified arrows do not match. A
configured Esc exit is consumed locally, including when received as a lone
Escape byte. Moving a pane to the previous or next window wraps in bar order,
splits the destination's first pane to the right, and preserves the running
process. Failed destination splits leave both layouts and focus unchanged and
ring the bell. Attached
session clients forward Ctrl-B immediately, so the server displays the same mode
transition as a foreground-only run. The top bar uses all of its available width
for the session name and window labels.

Ctrl-B, then `E` writes a plain-text snapshot of the active pane's retained
primary-screen history and meaningful visible rows to a private temporary file.
Soft-wrapped rows are joined and hard row boundaries remain newlines. Rustmux
opens the file in a new `history` window using `$VISUAL`, then `$EDITOR`, then
`vi`; closing the editor returns to the original window and removes the file.
The source pane keeps running. Alternate-screen panes, the 16-window limit and
editor startup failures leave existing windows unchanged and ring the bell.

Ctrl-B, then `e` opens the most recent command-output region in a temporary
`output` window. With shell integration, capture starts at `OSC 133;C` and completes at
`OSC 133;D` or the following `OSC 133;A`. ANSI control sequences are omitted
from the plain-text snapshot. Without OSC 133, an Enter outside bracketed paste
starts best-effort capture: the first line is treated as command echo and the last
line as the following prompt. A later OSC 133 marker replaces this heuristic with
exact boundaries. Empty output, output over 4 MiB, the window limit and editor
startup failures ring the bell.

There are at most 16 windows. New windows and splits use the originally selected
executable and inherit the active pane's latest valid OSC 7 working directory.
When OSC 7 is absent, macOS and Linux inspect the foreground process, then the
shell process. Yazi's foreground directory overrides a stale OSC 7 value because
navigation changes Yazi rather than its parent shell. Missing and invalid paths
fall back to Rustmux's startup working directory. A failed creation or the window
limit preserves existing windows and focus, with a
best-effort bell when the output queue is empty. No creation-error dialog is provided yet.

Each iteration performs at most one bounded read/write per ready pane. Inactive
windows keep parsing output and replying to terminal queries without rendering
their grids. SIGWINCH resizes all windows. Switching invalidates the physical
renderer and redraws the selected screen with its modes after any queued frame
finishes; queued frames are never discarded midway. Selecting a window forces
one redraw even if that child's synchronized-output hold is active; later updates
still obey the hold. Window and pane focus changes synthesize focus-out/focus-in
for children that enabled focus reporting.

Raw terminal input has a shared 64 KiB staging queue; each child also retains its
own 64 KiB input/reply queue. Input is decoded in order, so data before a shortcut
stays with the old child and subsequent bytes go to the newly selected one.
Queued input backpressure can delay shortcuts. On active-child exit, unprocessed
staged input and a pending prefix are discarded instead of reaching its successor.

A pane is removed when its output is drained and child status is known. In the
active window, its final frame is delivered before removal. The surviving layout
expands and focus follows its successor/predecessor rule. Removing the final pane
closes that window; removing the final window returns its exit status. Global termination signals restore the outer terminal and clean up all
owned children. A PTY/I/O failure still ends the whole CLI; per-window error
recovery is not implemented.

The nested-PTY suite tests actual creation, previous/next selection, retained
shell variables, background output, resize of inactive PTYs, mode synchronization,
child exit and focus fallback, literal-prefix/paste forwarding, failed creation,
terminal-query replies to inactive children, the 16-window cap, and cleanup of
all recorded child PIDs on global termination. These do not claim acceptance
of the remaining H05 UI features or of all full-screen application behavior.

## Renaming a window

Ctrl-B followed by `,` replaces the active window name with an empty edit value
and places a steady bar cursor at the end of that label. Type to append and use
Backspace to remove the last Unicode scalar or Ctrl-U to clear. Enter saves;
Esc, Ctrl-C or Ctrl-G cancel and restore the original name. Empty names are
allowed. Names are limited to 128 UTF-8 bytes; excess text, malformed UTF-8 and
control characters are ignored. Editing is append-only: arrow/control sequences
do not move a text cursor. Backspace removes a combining mark separately from
its base character.
The bottom bar shows `RENAME`. When it is wide enough, Pink/Lavender
`<Enter> Save` and `<Esc> Cancel` hints appear at the right; narrow rows keep only
the mode label while the top bar reserves its space for the editable name.

Bracketed paste is enabled while the prompt is visible. Its payload is treated
as name text, including letters following Ctrl-B, and control characters such as
newlines are ignored rather than saving the name. The terminating Enter must be
outside the paste. Bare Esc is distinguished from CSI/SS3 sequences by a 30ms
minimum delay; the event loop normally observes cancellation within its 50ms
poll interval. Escape-prefixed sequences are consumed without executing them.

The active label clips long names without splitting wide characters and reserves
a cursor cell. On extremely narrow terminals even the label is clipped. The edit
exists only in a temporary screen clone until it is saved, so it never overwrites
the child's grid, cursor, or stored window name. With fewer than three outer rows
there is no footer, but the name remains editable in the top bar when that bar
fits. Background output and query replies continue while editing, and resize
relocates the cursor with the active label. The original pane's display and input
modes are restored when editing ends. Exiting the active child cancels the prompt
before final output and focus fallback. Saved names are immediately visible in
the window bar.

Tests exercise Unicode and combining input, byte limits, invalid/control input,
paste boundaries, escape handling, narrow grids, DEC graphics/origin-mode
isolation, and real CLI save/cancel/reopen with continued child output and resize.

## Window bar and content area

The top row is reserved for window labels. When the outer terminal has at least
three rows, the bottom row shows contextual shortcut hints and the PTY uses the
rows between them. A two-row terminal keeps the top bar and one content row; a
one-row terminal hides both bars and retains one content row. Resizing updates
every pane; the outer 65,536-cell limit still includes the reserved rows.

Labels show a one-based position and name, for example `1 shell` and `2 editor`.
The active window uses a green Catppuccin Mocha badge; inactive windows use
foreground-colored badges. If a pane outside the current focus emits BEL, `[!]`
is appended to that window's name and that pane's border turns Peach (`#fab387`).
Its pane title also gains `[!]`. Focusing the pane clears its reminder; selecting
a window does not clear reminders from its other panes. BEL used to terminate an
OSC string does not create an activity marker.

Each pane also tracks command lifetime from shell-integration `OSC 133;C`
(command start) through `OSC 133;D` or the following `OSC 133;A` (completion).
Completing a command after the configured threshold rings the outer terminal once
and enters the same pane-specific bell state. Short commands and the heuristic
Enter-based output capture do not ring. A detached session retains the visual
bell state but has no terminal on which to make the completion bell audible. The
`[notifications]` configuration table can disable this behavior with
`long_command_bell = false` or set a positive whole-second threshold with
`command_duration_seconds`; the defaults are enabled and five seconds.
The bar uses [Catppuccin Mocha](https://catppuccin.com/palette/) with explicit RGB
colors. It draws dark badge text (`#11111b`) on Text (`#cdd6f4`) for inactive
windows and Green (`#a6e3a1`) for the active window, with Powerline separators
transitioning to the Base (`#1e1e2e`) bar background. Rename keeps the active
badge style and uses the footer for its mode and actions; history prompts use the
active badge colors.
The badge color identifies the active window. New windows default to the name `shell`.

Each label is clipped to the available display columns, excluding control
characters and without splitting a wide glyph. If labels do not fit, the visible
starting window advances enough to keep the active label in view. Scrolling up
anywhere on the bar selects the previous window; scrolling down selects the next,
wrapping at either end. Left-clicking either Powerline arrow or the label between
them selects that visible window. Both actions return input to `LOCKED` mode.
Session text and empty space are not clickable. A press that begins
on the bar consumes its release;
a drag that begins in a mouse-aware child can still release on the bar. On
extremely narrow terminals the visible label may consist only of its highlighted
prefix.

The bottom bar starts with the current mode in a plain rectangular red, green, or lavender
badge, followed by `Ctrl-B Commands` in locked mode or the available window/pane
keys in normal mode. The mode badge has no Powerline arrows. Each shortcut uses
Pink key text on the Base background, followed by a Powerline transition into
a sentence-case Lavender action label and back to Base. Complete hints are added
from left to right; a hint that
does not fit is omitted instead of being split. NORMAL mode reserves space for
`? Help`, so the complete shortcut reference remains reachable when middle hints
do not fit. Left-clicking a visible server-side hint executes the same action as
its key and consumes the matching release. The mode badge itself is not
clickable. In grouped hints such as `n/p` or `h/j/k/l`, clicking a key chooses
that key; clicking its label or padding chooses the first displayed key.
Blank space, hidden hints, drags and wheel reports do nothing and never reach a
child. Named sessions show `Ctrl-W Sessions` only after Ctrl-B enters NORMAL
mode; LOCKED mode still shows only `Ctrl-B Commands`. Clicking the Ctrl-W hint
asks the attached client to restore the outer terminal before opening the
Session Manager; cancelling the manager reconnects the session that opened it.
Local, unnamed Rustmux processes omit this hint because they have no session
manager to open.

Ctrl-B followed by `?`, or the clickable Help hint, opens a centered actionable
shortcut panel. Pressing or clicking a listed command closes the panel and runs
the same action as its normal shortcut. Named sessions include their Session
Manager entry. Esc, `q` or `?` closes without an action; unknown keys and pasted
input remain modal. Short terminals page with Left/Right, Page Up/Page Down or
the mouse wheel. See [Shortcut Help](shortcut-help.md).

When the bar is visible, Rustmux requests basic outer-terminal button reports
even if the child has mouse tracking disabled. In that case content-area events
are consumed instead of reaching the shell. If the child enables mouse tracking,
content events retain its encoding and are translated to pane-local coordinates.

The renderer receives a composed copy of the active child screen plus the bars;
child cells and cursor are shifted down one physical row; the child model and
input modes are preserved. The copy adds allocation and
grid-copy work to CLI rendering; prior encoding-only benchmark results do not
measure that cost. Unchanged composed cells still benefit from incremental output.
Closing an inactive window also schedules a redraw so labels and positions
update, respecting any synchronized-output hold on the active child.

When the child enables mouse reporting, complete SGR and classic X10 reports are translated
from physical coordinates to active-pane coordinates, accounting for its column,
row and the top bar. A left press in another pane selects it and consumes the
matching release instead of reaching either child. Other press, wheel and motion
reports outside the active pane are ignored. Releases from a child drag are
clamped to its nearest cell so the drag can end outside that pane.
Dragging a pane separator is handled locally and resizes that split; these motion
events are never forwarded to a child.
Candidate reports use at most 64 buffered bytes; incomplete candidates are
released after a 30ms minimum delay, subject to the event loop's polling and
backpressure. Extremely delayed/split malformed reports may therefore be forwarded
unfiltered. Paste payload is never mouse-filtered. Without mouse reporting,
ordinary Escape remains immediate.

Tests cover bar styles/labels, active-label visibility, Unicode clipping,
child-state preservation, actual PTY sizes, rename/save visibility, removal,
one-row fallback and mouse interception in a real CLI process.

## Returning to the last window

Ctrl-B followed by Tab returns to the last explicitly active window.
Repeated use toggles between two windows. Creation, numeric selection and next/previous
selection update this record when focus changes. Selecting the already active
window, an invalid number or renaming leaves the record unchanged.

The record uses a stable ID, so removing other windows and changing bar numbers
cannot redirect it. Closing the recorded target clears it. Automatic focus
fallback after an active window exits keeps the existing record only if it refers
to another surviving window; it never records the closed window or the newly
active fallback itself. Without a record the shortcut is consumed with no effect.
This stores one previous window, not an unlimited navigation history.

Model tests cover identity, toggling, cyclic selection, no-op selection, renaming
and removal. The nested PTY test switches between windows 1 and 10 and checks that
commands reach the original shells and the bar follows focus. Bracketed paste
containing Ctrl-B followed by Tab remains child input.

## Closing a window explicitly

Ctrl-B followed by `&` opens `Close window? Type yes:` in the top bar using the
same bounded text editor as renaming. Type exactly lowercase `yes` and press
Enter to close. Empty input, any other answer, Esc, Ctrl-C or Ctrl-G cancel.
Pasted newlines do not confirm; Enter must arrive outside bracketed paste.
Wide rows show `<Enter> Close` and `<Esc> Cancel` hints using the same prompt
colors; the hints disappear before reducing the answer or cursor space.
The confirmation is for the active window; switching shortcuts are not executed
while the editor is open. Output, terminal replies and resize continue normally.

Confirmation waits for any already encoded physical frame to finish. Rustmux
then closes the PTY, forcibly stops the direct shell if needed and reaps it,
using the existing `PtyShell::terminate` behavior. This can lose unsaved work;
it is not a graceful application exit and does not guarantee termination of all
detached descendants. Unread child output and pending input are discarded.
The successor is selected, or the predecessor if the closed window was last in
order; other windows keep running. Closing the only window restores the outer
terminal before child cleanup and exits Rustmux with status 0. Normal shell
exit still drains output and returns its own status. A child that exits while
the confirmation remains open follows the normal exit path.

Tests cover cancellation, background output, resize, pasted-newline protection,
direct-child reclamation, retained survivor shell state, discarded trailing
input, last-window restoration and natural exit during confirmation.

## Reordering windows

Ctrl-B followed by `<` or `>` swaps the active window with its left or right
neighbor. Moving left from the first position wraps to the end; moving right
from the last position wraps to the start. Other windows retain their relative
order. Empty and single-window collections are unchanged. The active shell,
stable ID, name, contents and last-window record are preserved. Numeric shortcuts
and next/previous selection follow the resulting display order, as does the
successor/predecessor rule when a window closes. New windows still append at the
right edge. These changes last only for the current process; there is no saved
window order across launches.

Reordering schedules a bar redraw through the normal frame cadence. It does not
invalidate the child display or interrupt its synchronized-output transaction;
bar updates can therefore wait for that transaction to finish or time out.
Inside an editor the keys are text, and bracketed-paste payload is passed through.

Model tests cover both edges, empty/single-window collections, content ownership,
last-window history, cyclic focus and removal after reordering. The nested PTY
suite checks displayed names/positions and actual shell state while moving,
selecting by number, returning to the last window and closing a moved window.
