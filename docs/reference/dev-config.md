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
native History bindings:

| Workflow | Configuration |
| --- | --- |
| Normal arrow focus | Arrow keys move pane focus and remain in Normal |
| Browse and search | Normal Enter or `s` opens History; `/`, `?`, `n`, `N` search; Ctrl-B/F/U/D move a full page |
| Edit history or command output | Default History `E` / `e`; defaults remain enabled |
| Copy selection, search result or viewport | History `y`; remains in History |
| Copy last command output and return to Locked | History `Y` |
| Show History shortcut Help | History `H`; `?` retains backward search |
| Mode transitions | Native `history`, `pane`, `resize`, `move`, `tab`, `session`, `normal`, `locked`; Ctrl-V enters Move |
| Notification filtering | Enabled with exclusions for Yazi, Neovim and Lazygit |
| Session Manager | Explicit original manager action keys and existing save/rename/disconnect operations |
| Layout and theme | Explicit `default_mode = "locked"`, `compact = false`, Mocha theme |

The existing 30-second autosave interval, saved text/styles and drag/drop
permissions remain configured. History defaults continue providing selection
and query editing. The original mode/action spellings do not need aliases.

Floating terminal toggling remains an implementation gap; this example does
not contain an inactive binding for it. A separate runtime implementation and
review are needed before adding that action.
