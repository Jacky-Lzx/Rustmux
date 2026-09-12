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
Rustmux itself. Type `exit` or use the shell's EOF key to leave. The shell's exit
status is returned to the caller. Shell output is drained before normal exit, and
the outer terminal modes and previous screen are restored.

This is a single-pane forwarding implementation. Terminal output is passed through
directly; there is no screen parser, split layout, session persistence or dynamic
resize propagation yet. Start it at the desired terminal size. Extended keyboard
protocols and arbitrary application terminal modes have not been validated.
