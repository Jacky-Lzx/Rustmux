# Session Client Bridge

`session::client::run` connects one negotiated session socket to the user's
controlling terminal. It verifies that standard input and output refer to the
same terminal, reads the initial window size, completes the protocol handshake,
and only then enters raw mode and the alternate screen. A failed connection or
handshake therefore cannot leave the terminal modified.

While attached, the client polls the terminal and Unix socket together. Terminal
input is encoded as bounded `Input` frames, SIGWINCH is coalesced into the newest
`Resize`, and `Output` frames are written directly to the terminal. Partial
writes retain their frame or output bytes, while backpressure stops the opposite
side from adding an unbounded queue. The bridge accepts the server's final
`Exit` status, or its `OpenSessionManager` control, only after all preceding
display bytes have reached the terminal.

Signals, protocol errors, socket disconnects and normal process exit all pass
through the same terminal guard. It restores termios plus cursor, keypad, mouse,
focus, paste, style and alternate-screen modes before returning. A server socket
that closes without `Exit` is reported as an incomplete session rather than a
successful command.

The bridge forwards Rustmux prefix keys to the server-side window parser except
for Ctrl-B `d` and Ctrl-B Ctrl-W. Both queue `Detach` after any earlier bytes and
return once the frame is written. The first exits to the outer terminal; the
second restores the terminal before the supervisor opens the Session Manager.
Bracketed paste contents never trigger either shortcut.

A named-session server can request the same manager transition when its footer
hint is clicked. The client stops accepting further terminal or socket input,
drains prior display output, restores the terminal, and then returns control to
the supervisor. Local unnamed processes do not advertise that footer action.

PTY unit tests exercise the real raw-mode boundary: initial dimensions and
resize propagation, keyboard input, rendered output, terminal controls and exact
termios restoration.
