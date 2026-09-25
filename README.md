# Rustmux

A terminal multiplexer, being implemented incrementally on the human review track.

## Build and Run

```sh
cargo build --locked
./target/debug/rustmux
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution and review rules.
The [implementation plan and acceptance ledger](https://github.com/Jacky-Lzx/Rustmux/blob/main/docs/reference/human-review-plan.md) are maintained on `main` (the link becomes available after publication).

## Optional Compatibility Checks

After updating an external application such as Yazi, run `cargo compat` to
exercise the installed application's opt-in tests. These tests live in
`tests/compat.rs` and are not part of the normal `cargo test` run.
