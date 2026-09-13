# Windows

The CLI supports multiple terminal windows, each with one shell and a full-size
screen. `window::Windows<T>` owns their ordered collection and stable identities.
This is partial H05: there is no window bar, interactive rename prompt, split
layout or persistent session yet.

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
| Ctrl-B, then Ctrl-B | Send one literal Ctrl-B to the active child |
| `exit` in the shell | Close that window after draining its final output |

An unrecognized prefix combination forwards both bytes unchanged. A prefix can
span separate reads and waits for the following byte without a timeout. Ordinary
Escape and UTF-8 bytes are forwarded immediately. Bracketed paste markers and
payload are forwarded unchanged, including Ctrl-B combinations inside the paste.
Unbracketed pasted text is indistinguishable from typing and follows the same
shortcut rules. Key bindings are fixed for this initial integration.

There are at most 16 windows. New shells use the originally selected executable
and Rustmux's startup working directory; active-shell cwd inheritance is not yet
implemented. A failed creation or the window limit preserves existing windows
and focus, with a best-effort bell when the output queue is empty. No error dialog
or status bar is provided yet.

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
