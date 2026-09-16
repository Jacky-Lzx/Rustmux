# Frontend I/O Boundary

The terminal event loop now receives its outer connection through the private
`Frontend` boundary. A frontend supplies one pollable descriptor, bounded input
and output operations, and pending terminal-size changes. PTY ownership, input
decoding, screen composition and rendering remain inside the existing event
loop.

`LocalFrontend` is the only implementation in this change. It opens the real
terminal device, enters raw and alternate-screen modes, translates SIGWINCH into
size updates, and restores terminal state on every exit path. Its input and
output still use the existing 64 KiB input queue, 16 MiB rendered-frame limit,
nonblocking writes and backpressure behavior.

This refactor does not change CLI behavior or create a persistent session. Its
purpose is to give the next H13 step one explicit place to implement a Unix
socket frontend using the bounded session protocol. That frontend can provide
input, resize and output without duplicating the multiplexer event loop.

The existing real-PTY integration suite continues to cover shell interaction,
resizes, terminal restoration, multiple windows and panes, history modes and
cleanup through `LocalFrontend`. Unit tests also retain injected output failures
and exact termios restoration checks.
