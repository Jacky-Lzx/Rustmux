# Build and Run

Use a Rust toolchain supporting edition 2024. From the `main-human` checkout:

```sh
cargo build --locked
./target/debug/rustmux
```

Run inside a terminal with nonzero dimensions; stdin and stdout must refer to
the same terminal. Rustmux opens one interactive shell, forwards keyboard input
and displays its output. It uses `RUSTMUX_SHELL`, then `SHELL`, then `/bin/sh`.
An invalid selected executable reports an error without silently falling back.

```sh
RUSTMUX_SHELL=/bin/sh ./target/debug/rustmux
```

Type commands normally. Ctrl-C reaches the inner terminal rather than terminating
Rustmux itself. Type `exit` or use the shell's EOF key to close the current window. When the last
window closes, its exit status is returned to the caller. Shell output is drained before normal exit, and
the outer terminal modes and previous screen are restored.

Use Ctrl-B followed by `c` to create a window, `n` for the next window and `p`
for the previous one. Ctrl-B followed by `1`–`9` selects that window number;
`0` selects window 10. Ctrl-B followed by `l` returns to the last active window;
repeat it to toggle between two windows. Ctrl-B twice sends a literal Ctrl-B to the child. Background
shells continue running. See [Windows](../reference/windows.md#interactive-controls)
for limits and input behavior. Ctrl-B followed by `,` opens the name editor:
Enter saves, Esc cancels, and Ctrl-U clears the existing name. The top bar shows the saved name and highlights the active window.

The CLI now parses shell output and renders its own screen model. Window changes
resize both model grids and the PTY in every window. The bar reserves one row;
the child receives one fewer row than the outer terminal (minimum one). The bar
is hidden when only one row is available. Outer dimensions must fit within
65,536 cells.
There is no scrollback: old text scrolled out of the grid is discarded.

Only the documented terminal-control subset is supported. Full-screen editors,
terminal queries and extended keyboard/mouse modes are not yet fully supported;
see [Input and Rendering Loop](../reference/input-loop.md#current-compatibility).
