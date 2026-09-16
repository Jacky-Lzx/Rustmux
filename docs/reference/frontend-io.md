# Frontend I/O Boundary

The terminal event loop now receives its outer connection through the private
`Frontend` boundary. A frontend supplies one pollable descriptor, bounded input
and output operations, and pending terminal-size changes. PTY ownership, input
decoding, screen composition and rendering remain inside the existing event
loop.

`LocalFrontend` opens the real terminal device, enters raw and alternate-screen
modes, translates SIGWINCH into size updates, and restores terminal state on
every exit path. `session::frontend::ServerFrontend` now implements the same
boundary for an accepted Unix socket. Both use the existing 64 KiB event-loop
input queue, 16 MiB rendered-frame limit and nonblocking backpressure behavior.

The boundary reports attached, detached and disconnected states separately. If
an input message precedes `Detach` in the same socket batch, the loop consumes
those bytes before ending the attachment. Once detached, it stops requesting
socket reads and writes. A local terminal disconnect continues to surface as an
input error after terminal restoration.

This integration does not change CLI behavior or create the background session
lifecycle. It establishes that a socket client can drive the existing
multiplexer loop without duplicating pane, parser or renderer logic.

The existing real-PTY integration suite continues to cover shell interaction,
resizes, terminal restoration, multiple windows and panes, history modes and
cleanup through `LocalFrontend`. Unit tests also retain injected output failures
and exact termios restoration checks.
