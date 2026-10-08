# Branches and Compatibility

## Active documentation

This site describes `main`, formerly named `main-human`. The older, independent
implementation is retained on `main-AI`. Renaming the branches did not merge
their implementations, import configuration aliases or convert saved sessions.
Follow the commands and settings documented here for `main`.

Running `rustmux` without arguments opens an unnamed foreground workspace.
Use `rustmux new NAME`, `rustmux attach NAME`, or
`rustmux attach NAME --create` for a persistent named session.
See [Named Session Commands](session-cli.md).

The native browser mode is `history`, with `[keybinds.history]` and
`switch-mode history`. Legacy `scroll` mode/action spellings are not aliases.
Use `check-config --strict` to detect ignored settings, and consult
[Configuration Diagnostics](config-diagnostics.md) and
[Native Development Configuration](dev-config.md).

Saved workspaces keep the existing namespace
`$XDG_STATE_HOME/rustmux/main-human/sessions`, falling back to
`~/.local/state/rustmux/main-human/sessions`. This is a storage compatibility
name, not the active branch. Files from `main-AI` use a different schema and are
not imported. Do not copy them into this directory.
See [Session Snapshots](session-snapshots.md).

## Implemented behavior and limits

The current implementation includes windows, split and floating panes, named
sessions, saved layouts/history, configuration hot reload, script control,
mouse and Kitty keyboard input, and the documented Kitty graphics paths.
Graphics display requires verified outer-terminal support and exact cell pixels.
Child clipboard access, file transfer and drag/drop require their documented
opt-in policies. A supported protocol subset does not establish compatibility
with every application or terminal.

The [Input and Rendering Loop](input-loop.md#current-compatibility) summarizes
protocol coverage. [UTF-8 and Character Width](unicode.md) describes supported
emoji sequences and the remaining shaping limits. Runtime resize can still
fail when a terminal becomes too small for a layout.

## Historical verification records

Feature chapters preserve checks, benchmark numbers, reference commits and
review notes from their original implementation revisions. Those records are
historical evidence, not a current aggregate test count, benchmark result,
installation check or feature acceptance record. Branch names in those records
use the names at the time: their reference `main` is now `main-AI`, and their
implementation `main-human` is now `main`.

The historical [plan and acceptance ledger](https://github.com/Jacky-Lzx/Rustmux/blob/main-AI/docs/reference/human-review-plan.md)
also remains on `main-AI`; its dated snapshot is not current feature coverage.
Passing tests or merging a revision does not supply the owner's acceptance.
See [Development and Contributions](development.md) for current checks and
the review policy.
