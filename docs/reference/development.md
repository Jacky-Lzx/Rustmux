# Development and Contributions

## Local checks

```sh
cargo fmt --all --check
cargo build --locked
cargo test --all-targets --locked -- --test-threads=4
cargo clippy --all-targets --all-features --locked -- -D warnings
```

The Rust CI workflow runs these checks on macOS and Linux, with at most four
tests running concurrently per test binary. PTY integration tests start real
shells and verify controlling-terminal setup, initial size, exit and
resource cleanup. The input-loop harness requires Python 3 and creates an outer
PTY to run the actual binary, checking keyboard forwarding and terminal recovery. They require permission to create PTYs and child processes.
See [PTY lifecycle](pty-lifecycle.md) for the implementation and review boundaries.

The optional [SSH rendering experiment](rendering-performance.md#ssh-input-to-frame-experiment)
is run manually with Python and OpenSSH. It is not invoked by `cargo test` or CI.

## Contributions

`main` is the former `main-human`, and `main-AI` is the former `main`.
The branch rename preserves contribution scope and review requirements.
Existing issue labels may still use the former branch names.

The owner may personally commit directly. AI and other contributors require a
PR and the owner's review. See the branch's
[CONTRIBUTING.md](https://github.com/Jacky-Lzx/Rustmux/blob/main/CONTRIBUTING.md)
for the complete policy.

## Documentation

Edit Markdown in `docs/` and keep the navigation in `docs/SUMMARY.md` current.
With mdBook 0.5.4 installed, run this from the `main` checkout:

```sh
mdbook build
python3 -m http.server 8000 --directory dist
```

Open the preview at `http://localhost:8000/`, or use `mdbook serve` for live source
editing. GitHub Pages uses the same build command with its repository base path
and publishes only `main`'s documentation. The former `main-human` is now `main`;
`main-AI` retains the older implementation. The build uses this branch's content,
edit links and search index. Its homepage and visual theme come from `main-AI`,
with presentation assets in `docs/theme/`, loaded by `book.toml`. They do not
require another checkout or the former dual-track build scripts. Local builds do
not publish anything.

See [Branches and Compatibility](documentation-status.md) for storage paths,
configuration differences and how to interpret historical verification records.
