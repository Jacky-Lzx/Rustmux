# Project Layouts

```sh
rustmux new work --layout ./project.toml
rustmux new background --detached --layout ./project.toml
```

```toml
[[windows]]
name = "development"

[[windows.panes]]
cwd = "src"
command = "cargo check; exec /bin/sh -i"

[[windows.panes]]
cwd = "."
split = "down"

[[windows]]
name = "editor"

[[windows.panes]]
cwd = "."
```

Directories resolve relative to the canonical layout file, not the launching
directory. `cwd` defaults to `.` and must exist. Each pane after the first splits
the preceding pane to the right by default, or below with `split = "down"`.
The first window and its first pane receive focus.

The whole file, directories, names, commands and final split geometry are
validated before binding the server endpoint or starting any shell. Unknown
fields, empty windows, missing directories, impossible geometry, files over
64 KiB, more than 16 windows or 128 total panes are rejected. Each window is
also subject to the existing layout limits. Attached projects validate against
the current terminal size; detached projects bootstrap at 40 rows by 120 columns.
A later attachment follows the ordinary live-resize behavior.

An explicit `--layout` overrides that name's saved snapshot. Applying it to an
already running session is rejected and does not run any startup commands.
Without `--layout`, named startup continues to restore the saved workspace or
start one fresh shell.

`command` runs through the configured shell with `-c`; otherwise the pane starts
an interactive shell with `-i`. Commands must be nonempty, at most 4096 bytes,
and contain no NUL. When a command exits its pane follows the normal exit
lifecycle. Append `; exec /bin/sh -i` or another appropriate interactive shell
when the pane should remain open. Process state is not serialized.

Explicit startup commands are kept with their owned panes when they move, and
are included in manual/automatic workspace snapshots. Restoring those snapshots
reruns the recorded commands using the currently configured shell; the original
project file is not required. Ordinary interactive commands entered later and
saved terminal output are not executed during restoration. Layout-only snapshots
from before this feature remain readable because omitted `command` means a fresh
interactive shell. The snapshot namespace remains separate from `main`.

Reading order: `src/project.rs`, `Snapshot::from_project` and optional command
handling in `src/persistence.rs`, `create_from_layout` in the supervisor, then
`Pane::spawn_with_startup` and `PtyShell::spawn_startup`. Verification includes
unit validation and `cargo test --test terminal_loop project`: invalid late
fields/geometry launch nothing, relative cwd, detached startup, running-session
rejection, saved command replay and explicit-layout precedence.
