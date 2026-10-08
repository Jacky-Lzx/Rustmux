# Rustmux

This documentation describes the implementation on `main`, formerly `main-human`. It
provides multiple shell windows and split panes with prefix-key switching, parsed
screen rendering, named persistent sessions, dynamic resize propagation and
outer-terminal restoration. The terminal protocol subset is documented in the
[input and rendering loop](reference/input-loop.md).

Named workspaces support [disk snapshots and optional restored history](reference/session-snapshots.md),
including manual saves and opt-in autosave across detach and orderly shutdown.

Developers can review the [PTY lifecycle module](reference/pty-lifecycle.md), which
starts an interactive shell on a controlling terminal and owns its cleanup.

`main-AI` retains the independent older implementation. Its commands and
configuration are not automatically compatible with this branch. See
[Branches and Compatibility](reference/documentation-status.md).

Start with [Build and Run](getting-started/quick-start.md), then read
[Development and Contributions](reference/development.md).

The historical [implementation plan and acceptance ledger](https://github.com/Jacky-Lzx/Rustmux/blob/main-AI/docs/reference/human-review-plan.md)
remain on `main-AI`. The ledger records review evidence separately from
this branch's usage documentation.
