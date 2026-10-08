# Rustmux

A terminal multiplexer with persistent named sessions, split panes and pane-local
terminal state. The active `main` branch was formerly named `main-human`;
`main-AI` retains the independently developed older implementation.

## Build and Run

```sh
cargo build --locked
./target/debug/rustmux
```

Use `--config PATH` (or `-c PATH`) to select a different configuration:

```sh
cargo run --locked -- --config "$HOME/.config/rustmux/config-dev.toml"
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution and review rules.
Read the [documentation](https://jacky-lzx.github.io/Rustmux/) or its
[branch and compatibility guide](docs/reference/documentation-status.md).
Named workspaces can be saved with `rustmux save-session NAME` and recreated with
`rustmux new NAME`. Optional history and autosave settings are documented in
[Session Snapshots](docs/reference/session-snapshots.md).
The historical [implementation plan and acceptance ledger](https://github.com/Jacky-Lzx/Rustmux/blob/main-AI/docs/reference/human-review-plan.md) remain on `main-AI` after the branch rename.

## Optional Compatibility Checks

After updating an external application such as Yazi, run `cargo compat` to
exercise the installed application's opt-in tests. These tests live in
`tests/compat.rs` and are not part of the normal `cargo test` run.
