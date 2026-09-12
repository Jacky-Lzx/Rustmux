# Build and Run

Use a Rust toolchain supporting edition 2024. From the `main-human` checkout:

```sh
cargo build --locked
./target/debug/rustmux
```

The current program prints a diagnostic to stderr explaining that terminal
functionality is not implemented, and exits with status 1. This is expected for
the current implementation. It does not start a shell or change terminal modes.

Run the binary from its checkout while developing. Installation as a usable
terminal multiplexer is not available on this track yet.
