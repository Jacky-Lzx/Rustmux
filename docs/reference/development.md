# Development and Contributions

## Local checks

```sh
cargo fmt --all --check
cargo build --locked
cargo test --all-targets --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
```

The Rust CI workflow runs these checks on macOS and Linux. PTY integration tests
start real shells and verify controlling-terminal setup, initial size, exit and
resource cleanup. The input-loop harness requires Python 3 and creates an outer
PTY to run the actual binary, checking keyboard forwarding and terminal recovery. They require permission to create PTYs and child processes.
See [PTY lifecycle](pty-lifecycle.md) for the implementation and review boundaries.

## Contributions

`main-human` currently accepts bug reports and bug-fix PRs for existing code.
Feature implementation PRs and feature-related issues belong to `main` with
`track:main`. Use `track:main-human` for this track's bug reports and fixes.

The owner may personally commit directly. AI and other contributors require a
PR and the owner's review. See the branch's
[CONTRIBUTING.md](https://github.com/Jacky-Lzx/Rustmux/blob/main-human/CONTRIBUTING.md)
for the complete policy.

## Documentation

Edit Markdown in `docs/` and keep the navigation in `docs/SUMMARY.md` current.
With mdBook 0.5.4 installed, `mdbook build` or `mdbook serve` provides a standalone
preview of this branch with the default mdBook theme.

For the shared theme and branch selector, run this from the `main` checkout:

```sh
./scripts/build-docs.sh --human-source ../Rustmux-human
python3 -m http.server 8000 --directory dist
```

Open the preview at `http://localhost:8000/main-human/`. The shared build uses
`main`'s presentation assets and each branch's own content, edit links and search
index. The unified Pages workflow builds both branches before publishing one
artifact; local builds do not publish anything.
