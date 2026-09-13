# Windows

The CLI supports multiple terminal windows, each with one shell and its own
content screen. `window::Windows<T>` owns their ordered collection and stable identities.
A top window bar shows names and focus. This is partial H05: split layouts
and persistent sessions are not implemented. Window renaming uses the same bar
row as a temporary input prompt.

## Identity and focus

- Creation appends a window and makes it active.
- `WindowId` is stable within its originating collection, independent of the
  window's position and name. IDs start at zero and are never reused, even after
  all windows have been closed. They are not cross-collection or persistent IDs.
- `select` focuses an ID. `select_next` and `select_previous` wrap in creation
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

This intentionally separates the current one-pane-per-window implementation from the
broader `main` window/pane implementation, which also includes layouts, floating
terminals and persistent sessions. This model is not an H05 feature-acceptance
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

`pane::Pane` now supplies concrete contents for `Windows<Pane>`: one `PtyShell`,
one incremental `Parser` and one `Screen`. The CLI uses this same container. `Pane::spawn` validates the grid (at most 65,536 cells),
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
| Ctrl-B, then 1–9 | Select the window at that one-based position |
| Ctrl-B, then 0 | Select window 10 |
| Ctrl-B, then , | Rename the active window |
| Ctrl-B, then Ctrl-B | Send one literal Ctrl-B to the active child |
| `exit` in the shell | Close that window after draining its final output |

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
shortcut rules. Key bindings are fixed for this initial integration.

There are at most 16 windows. New shells use the originally selected executable
and Rustmux's startup working directory; active-shell cwd inheritance is not yet
implemented. A failed creation or the window limit preserves existing windows
and focus, with a best-effort bell when the output queue is empty. No creation-error dialog is provided yet.

Each iteration performs at most one bounded read/write per ready pane. Inactive
windows keep parsing output and replying to terminal queries without rendering
their grids. SIGWINCH resizes all windows. Switching invalidates the physical
renderer and redraws the selected screen with its modes after any queued frame
finishes; queued frames are never discarded midway. Selecting a window forces
one redraw even if that child's synchronized-output hold is active; later updates
still obey the hold. Window switching does not synthesize focus-in/out events.

Raw terminal input has a shared 64 KiB staging queue; each child also retains its
own 64 KiB input/reply queue. Input is decoded in order, so data before a shortcut
stays with the old child and subsequent bytes go to the newly selected one.
Queued input backpressure can delay shortcuts. On active-child exit, unprocessed
staged input and a pending prefix are discarded instead of reaching its successor.

An inactive window is removed when its output is drained and child status is
known. The active window's final frame is delivered before removal; focus then
follows the model's successor/predecessor rule. The last window's exit status is
returned. Global termination signals restore the outer terminal and clean up all
owned children. A PTY/I/O failure still ends the whole CLI; per-window error
recovery is not implemented.

The nested-PTY suite tests actual creation, previous/next selection, retained
shell variables, background output, resize of inactive PTYs, mode synchronization,
child exit and focus fallback, literal-prefix/paste forwarding, failed creation,
terminal-query replies to inactive children, the 16-window cap, and cleanup of
all recorded child PIDs on global termination. These do not claim acceptance
of the remaining H05 UI features or of all full-screen application behavior.

## Renaming a window

Ctrl-B followed by `,` opens `Rename: <current name>` on the top row. Type to
append, use Backspace to remove the last Unicode scalar, or Ctrl-U to clear.
Enter saves; Esc, Ctrl-C or Ctrl-G cancel. Empty names are allowed. Names are
limited to 128 UTF-8 bytes; excess text, malformed UTF-8 and control characters
are ignored. Editing is append-only: arrow/control sequences do not move a text
cursor. Backspace removes a combining mark separately from its base character.

Bracketed paste is enabled while the prompt is visible. Its payload is treated
as name text, including letters following Ctrl-B, and control characters such as
newlines are ignored rather than saving the name. The terminating Enter must be
outside the paste. Bare Esc is distinguished from CSI/SS3 sequences by a 30ms
minimum delay; the event loop normally observes cancellation within its 50ms
poll interval. Escape-prefixed sequences are consumed without executing them.

The prompt shows the tail of long names without splitting wide characters and
reserves a cursor cell. On extremely narrow terminals even the label is clipped.
It replaces the window-bar row in a temporary screen clone, resets the clone's
character-set/style/input modes for editing, and never overwrites the child's grid
or cursor. When the outer terminal has only one row, it temporarily covers that
row because no dedicated bar fits. Background
output and query replies continue while editing, and resize relocates the prompt
at the top row. The original pane's display and input modes are restored
when editing ends. Exiting the active child cancels the prompt before final output
and focus fallback. Saved names are immediately visible in the window bar.

Tests exercise Unicode and combining input, byte limits, invalid/control input,
paste boundaries, escape handling, narrow grids, DEC graphics/origin-mode
isolation, and real CLI save/cancel/reopen with continued child output and resize.

## Window bar and content area

The top row is reserved for window labels. The PTY and both screen grids use
`max(1, outer rows - 1)` rows, with the full terminal width. A terminal with only
one row hides the bar and retains one content row. Resizing updates every pane;
the outer 65,536-cell limit still includes the reserved row.

Labels show a one-based position and name, for example `1:shell` and `*2:editor`.
The bar uses [Catppuccin Mocha](https://catppuccin.com/palette/) with explicit RGB
colors: inactive labels use Subtext0 (`#a6adc8`) on Mantle (`#181825`); the active
window and rename prompt use Base (`#1e1e2e`) on Blue (`#89b4fa`).
The star also identifies the active window. New windows default to the name `shell`.

Each label is clipped to 24 display columns, excluding control characters and
without splitting a wide glyph. If labels do not fit, the visible starting window
advances enough to keep the active label in view. There are no click-to-select or
mouse-scroll actions on the bar yet. On extremely narrow terminals the visible
label may consist only of its highlighted prefix.

The renderer receives a composed copy of the active child screen plus the bar;
child cells and cursor are shifted down one physical row; the child model and
input modes are preserved. The copy adds allocation and
grid-copy work to CLI rendering; prior encoding-only benchmark results do not
measure that cost. Unchanged composed cells still benefit from incremental output.
Closing an inactive window also schedules a redraw so labels and positions
update, respecting any synchronized-output hold on the active child.

When the child enables mouse reporting, complete SGR and classic X10 reports are translated
from physical rows to child rows by subtracting the top bar height. Press, wheel
and motion reports outside the content area are ignored. Release reports are
clamped to the nearest content row so a drag can end. In a one-row terminal,
coordinates remain unchanged because the bar is hidden.
Candidate reports use at most 64 buffered bytes; incomplete candidates are
released after a 30ms minimum delay, subject to the event loop's polling and
backpressure. Extremely delayed/split malformed reports may therefore be forwarded
unfiltered. Paste payload is never mouse-filtered. Without mouse reporting,
ordinary Escape remains immediate.

Tests cover bar styles/labels, active-label visibility, Unicode clipping,
child-state preservation, actual PTY sizes, rename/save visibility, removal,
one-row fallback and mouse interception in a real CLI process.
