# Input and Rendering Loop

This describes H02 input, H03 rendering integration and H04 resize handling. Read
`src/main.rs`, then `src/terminal.rs`, its queue tests, and
`tests/terminal_loop.py` (launched by `tests/terminal_loop.rs`).

## Scope and data flow

The CLI selects an executable from RUSTMUX_SHELL, SHELL or /bin/sh and starts
PtyShell with the initial outer-terminal dimensions. It requires terminal stdin
and stdout pointing to the same device. It opens that actual device separately:
this avoids modifying the parent's shared file status flags, and avoids polling
macOS's /dev/tty indirection. No shell command string is interpolated at startup.

The outer terminal enters raw mode so Ctrl-C and other input arrive as bytes.
An alternate screen preserves the previous screen contents. Input is forwarded to
the inner PTY unchanged. Output follows `PTY -> Parser -> Screen -> render ->
outer terminal`. The inner PTY's line discipline and shell handle editing and
keyboard signals. The parser supports the documented control subset and standard
eight-column tabs; unknown commands are ignored rather than passed through.

Both descriptors are nonblocking. Keyboard input has a 64 KiB queue. Output holds
at most one full ANSI frame, capped at 16 MiB; grids are limited to 65,536 cells.
These limits also apply on resize. A limit error follows normal terminal cleanup.
Child output reads pause while a frame is pending, applying backpressure without
accumulating frames. Reads are at most 8 KiB. Writes retain unsent tails and retry
Interrupted/WouldBlock on later iterations.

Changed screen state is painted at a target minimum spacing of 6 ms after the
previous frame was generated. Idle screens are not redrawn. Poll waits at most
50 ms for signal/exit checks, shortened when a frame is due. This is scheduling,
not a hard real-time guarantee. The final frame bypasses the interval on EOF.
Scrolling output updates the grid; there is no scrollback, and intermediate states
may be coalesced before painting. The output is no longer a byte-for-byte copy of
the child's stream.

## Exit and terminal restoration

The child status and PTY end-of-output are tracked independently. Normal exit
finishes UTF-8 decoding and flushes the final rendered frame. If descendants retain the slave, the loop stops once the
direct shell has exited and currently available output is drained; it does not
wait for detached descendants. A live process retaining execution after closing
its PTY gets a one-second exit grace period after observable EOF, then an error
and cleanup. macOS may keep the controlling PTY alive until that process exits,
so closed standard streams alone do not necessarily produce EOF.

Raw termios is saved before modification and restored on normal return, errors
and unwinding. Cleanup also resets common mouse/bracketed-paste modes, text style,
cursor visibility and the alternate screen. Terminal control writes have a
500 ms deadline so a blocked output device cannot prevent termios restoration.
Cleanup cannot restore a physically disconnected device, nor recover from SIGKILL
or abort. The display reset assumes a conventional outer terminal; arbitrary
pre-existing private modes and nested alternate-screen state are not captured.

signal-hook 0.3 installs flag-only handlers for HUP, TERM, INT and QUIT, without
creating threads. The event loop returns 128 + signal, restores terminal state,
and then drops the PTY owner to stop/reap its direct child. This dependency avoids
handwritten signal handlers. Suspend/resume and whole-process-tree
supervision remain out of scope. The public run function is intended for the CLI
process, with exclusive terminal ownership and single-threaded startup.

## Window size changes

SIGWINCH sets a separate atomic flag, so resize events cannot overwrite termination
signals. The event loop reads the latest outer-terminal size, resizes both model grids,
and calls PtyShell::resize;
TIOCSWINSZ updates the inner PTY and lets the kernel notify its foreground process
group. No terminal operations run in the signal handler. Coalesced events use the
latest size, including an initial recheck after handler registration to close the
startup race. Temporary zero dimensions are ignored after startup; startup still
requires nonzero dimensions. Pixel dimensions are not propagated. I/O failures use
the same terminal-restoration path as forwarding failures. An already partly sent
frame is completed before rendering the new dimensions, so resize may briefly
show stale layout before the fresh frame.

## Verification

The nested-PTY test drives the real binary: Chinese text, erase, Ctrl-C interrupting
sleep, colored cursor overwrites, alternate-screen restoration, repeated resize with a foreground SIGWINCH observer, transient zero sizes, 200 KB output, final output plus exit code 7, invalid shell startup,
non-terminal input rejection, resize-limit errors, SIGTERM, output backpressure, and a live process
closing its PTY. A supervisor retains the outer controlling session so macOS does
not revoke its terminal before attributes can be checked. All termios settings
are compared except the kernel-maintained PENDIN transient state.

Tests decode complete renderer frames into rows independently of the Rust parser.
The burst test checks the final marker after 200 KB is consumed, not retention of
all scrolled text.

Unit tests force frame/size limits, partial writes, Interrupted and WouldBlock, queue saturation,
and an operation error while a raw-terminal guard is active, checking restoration.
The previous PTY lifecycle tests remain in CI. Python 3 and PTY/process permissions
are required. Local validation is on macOS; Linux results require the CI run.

## Current compatibility

The CLI now depends on our parser's supported subset. Terminal queries, mouse modes and full emoji shaping are not implemented. Programs requiring
those features may display incorrectly or wait for an unsupported terminal reply.
Full-screen editor compatibility is not yet an acceptance claim. There is no
split layout or persistent session support.
