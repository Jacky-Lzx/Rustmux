# Native development configuration

`examples/config-dev.toml` records the development configuration for
`main-human`. It uses the implemented mode and action names. To inspect and run
it from the repository root:

```sh
rustmux check-config --config examples/config-dev.toml --strict
rustmux new dev-check --config examples/config-dev.toml
```

The configured shell is the owner's Homebrew Fish path. Change `shell` for
another machine, or set `RUSTMUX_SHELL` to override it.

The configuration exposes the features of the original configuration through
native mode and action bindings:

| Workflow | Configuration |
| --- | --- |
| Floating shell | Normal `i` / Pane `w` toggles the same retained shell |
| Normal arrow focus | Arrow keys move pane focus and remain in Normal |
| Browse and search | Normal Enter or `s` opens History; `/`, `?`, `n`, `N` search; Ctrl-B/F/U/D move a full page |
| Edit history or command output | Default History `E` / `e`, always shown through display-only entries; selection `e` still extends by word |
| Copy selection, search result or viewport | History `y`; remains in History |
| Copy last command output and return to Locked | History `Y` |
| Show History shortcut Help | History `H`; `?` retains backward search |
| Show mode shortcut Help | Pane / Resize / Move / Tab / Session `?`; closing Help returns to its originating mode |
| Mode transitions | Native `history`, `pane`, `resize`, `move`, `tab`, `session`, `normal`, `locked`; Ctrl-V enters Move |
| Notification filtering | Enabled with exclusions for Yazi, Neovim and Lazygit |
| Session Manager | Explicit original manager action keys and existing save/rename/disconnect operations |
| Layout and theme | Explicit `default_mode = "locked"`, `compact = false`, Mocha theme |

The existing 30-second autosave interval, saved text/styles and drag/drop
permissions remain configured. History defaults continue providing selection
and query editing. The original mode/action spellings do not need aliases.

The History `E` and `e` entries intentionally omit `actions`: setting only
`display = "always"` exposes the default shortcuts without replacing their
context-sensitive behavior. An explicit `edit-last-output` binding on `e` would
also run during keyboard selection and replace its word-extension action.

The binding-display PTY scenario loads this exact example to verify mode Help,
return/dispatch behavior, editor visibility and selection word motion.

See [Floating terminal](floating-terminal.md) for geometry, process ownership,
snapshot behavior and the boundaries of this increment.
