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
`Exit` status only after all preceding display bytes have reached the terminal.

Signals, protocol errors, socket disconnects and normal process exit all pass
through the same terminal guard. It restores termios plus cursor, keypad, mouse,
focus, paste, style and alternate-screen modes before returning. A server socket
that closes without `Exit` is reported as an incomplete session rather than a
successful command.

The bridge deliberately does not interpret Rustmux prefix keys. Window and pane
commands remain server-side, so ordinary child input has one parser and one
meaning. Command-line session creation, attachment, background startup and an
explicit detach shortcut belong to the following orchestration layer.

The PTY unit test exercises the real raw-mode boundary: initial dimensions and
resize propagation, keyboard input, rendered output, final status and exact
termios restoration.
