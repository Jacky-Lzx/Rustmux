# Rustmux: main-human

This documentation describes the implementation on `main-human`. The branch
provides a single interactive shell with keyboard input, direct terminal-output
forwarding, dynamic resize propagation and outer-terminal restoration. It does not yet provide split panes,
session persistence, a screen parser.

Developers can review the [PTY lifecycle module](reference/pty-lifecycle.md), which
starts an interactive shell on a controlling terminal and owns its cleanup.

The `main` track has a broader feature set. Use the branch selector on the
published site to read its documentation; a feature described there is not
necessarily available on this track.

Start with [Build and Run](getting-started/quick-start.md), then read
[Development and Contributions](reference/development.md).

The [shared implementation plan and acceptance ledger](https://github.com/Jacky-Lzx/Rustmux/blob/main/docs/reference/human-review-plan.md)
are maintained on `main`. The ledger records review evidence separately from
this branch's usage documentation.
