# Rustmux: main-human

This documentation describes the implementation on `main-human`. The branch
contains a PTY and shell lifecycle library, integration tests and contribution
rules. The command-line program is still a placeholder: interactive input,
rendering and terminal multiplexing are not yet connected.

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
