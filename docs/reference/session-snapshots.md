# Session Snapshots and Restored History

Named sessions can save their workspace to disk and recreate it after the server
stops or the machine restarts. Detach/attach continues to preserve the original
running processes; disk restoration starts **new interactive shells**.

## Save and restore

```sh
rustmux new work
# Arrange windows and panes, then run from another terminal or a Rustmux pane:
rustmux save-session work
# `rustmux save work` is an alias.

rustmux kill work
rustmux new work
```

`save-session` works while the named session is attached or detached. It does not
acquire the interactive client lease. A successful response means the atomic
file replacement and directory sync have finished. Unknown sessions, unsafe
paths and write errors return a nonzero status. Foreground Rustmux without a
session name has no disk-saving endpoint.

`new work` automatically loads the saved workspace for that name. `attach NAME`
requires a running server; it does not start a stopped session. `kill` and
`kill-all` retain snapshots. Removing a snapshot file explicitly makes the next
creation start with one fresh shell window.

`list`/`ls` includes both running sessions and saved workspaces, with each name
appearing once. `ls --long` marks stopped saved workspaces as `SAVED`, without a
server PID or last-connection time. The Session Manager includes them after live
sessions; search and navigation work for both. A selected `SAVED` entry offers
the `Restore` action. Enter starts fresh shells using the selected configuration
and current terminal dimensions before attaching. `attach` without a name
restores directly when there is just one entry.

The manager's `dd` action and `kill-all` operate only on running servers and
retain their snapshots. Stopped saved entries do not offer `dd`; remove the
snapshot file explicitly to forget them. Listing checks file names and private
regular-file metadata without decoding history. A corrupt private snapshot
remains discoverable; its validation error is reported on restore.

Snapshots include window names/order, split axes/ratios, focused panes, active
window, zoom state and working directories. Temporary history/output editor
windows and hidden undo panes are excluded. A missing directory falls back to the
server's working directory. Every restored pane starts the currently configured
shell; shell variables, processes, foreground jobs and command exit state are
not restored. Explicit [project startup commands](project-layouts.md) are rerun;
ordinary interactive commands and saved output are not executed.

The flat split-tree format is validated before any restored shell starts.
Invalid files are reported during `new`, without replacing the snapshot. All
panes must fit the terminal that creates the session; a terminal too small for
the saved layout causes startup to fail without launching its panes. Detached
restoration bootstraps at the saved dimensions instead of forcing a large layout
into the usual 24 by 80 startup size.

## Optional history and autosave

Add these top-level settings to the selected configuration:

```toml
save_scrollback = true
save_scrollback_colors = true
scrollback_lines = 5000
autosave_interval_seconds = 30
```

All three saving options default to disabled (`false`, `false`, `0`). Explicit
manual saving remains available with those defaults and saves only the layout.
Configuration is loaded when the server starts; these settings do not hot reload.

`save_scrollback` includes up to `scrollback_lines` physical rows per pane, drawn
from retained primary history and meaningful visible primary rows. The existing
history cell budget also applies. It saves the primary screen even when an
application is using the alternate screen. `save_scrollback_colors` additionally
retains symbolic default/indexed colors, RGB colors and text attributes using
SGR. Custom palette overrides, image data, links and terminal modes are not saved.

Restored output becomes history **above a fresh live screen**. It is available
through Ctrl-B `[` for browsing, searching and copying. Soft-wrap links, Unicode,
combining marks and explicitly written spaces are retained; history reflows to
the new pane width and remains subject to its configured history limits.
Disabling `save_scrollback` omits history from new saves and skips history in old
snapshots when restoring. Enabling colors later does not change older plain
snapshots; each snapshot records its own format.

Restoration accepts printable UTF-8 and bounded SGR sequences only. Cursor
commands, clipboard requests, terminal queries and other controls in a modified
snapshot are rejected. Saved text is never sent to a shell as input.

A positive autosave interval schedules saves while attached or detached, and
checkpoints on detach and orderly shutdown (including `kill`). Autosave cannot
capture output produced after the last completed save if the server is forcibly
killed or power is lost. Formatting and disk writes use one background worker;
pending requests are coalesced into the newest captured workspace. Detach and
shutdown wait for ordered writes. Screen grid copies and directory capture still
run on the event loop; this first implementation has no unchanged-pane cache.

Write failures preserve the running panes and are reported to manual callers.
An attached session shows `Save failed:` in its footer until a subsequent save
succeeds. Detached failures remain visible on the next attachment.

## Storage and bounds

Snapshots live in `$XDG_STATE_HOME/rustmux/main-human/sessions`, or
`~/.local/state/rustmux/main-human/sessions`. This track uses a separate namespace
because its schema differs from `main`; neither track imports the other's files.
The directory must be owned by the user with no group/other permissions; newly
created directories use mode 0700 and files use 0600. Files are replaced through
a synced temporary file in the same directory. Symlinks, nonregular files,
insecure files, unknown fields, invalid topology and files over 32 MiB are rejected.

The save-only runtime socket has a `.save` suffix and follows the private runtime
endpoint rules. At most four control clients are retained. Idle request clients
expire after two seconds. The interactive protocol and its version are unchanged.

## Review and verification

Reading order: `src/layout/snapshot.rs`, `src/screen/history_snapshot.rs`,
`src/persistence.rs`, `src/session/snapshot.rs`, then the integration in
`src/config.rs`, `src/session/supervisor.rs`, `src/terminal.rs` and the CLI.
For saved-session discovery and selection, also review `src/session.rs`,
`src/session/picker.rs`, and the running-only `kill-all` path in `src/main.rs`.

Unit coverage checks split ratios/focus/zoom, malformed graphs, styled Unicode
reflow, alternate-screen exclusion, unsafe controls, atomic storage and file
permissions. `cargo test --test terminal_loop snapshots` exercises real nested
PTY save/restart/restore at different widths, fresh shell state, directories,
concurrent and detached saves, saved-history opt-out, invalid files, write failure
isolation and autosave. It also checks saved CLI listings, manager search and
restoration at the current terminal size, without duplicate names or saved text
appearing in the fresh live screen.
The snapshot PTY scenario requires Python 3.11 or later for its standard-library
TOML reader. The work on the review branch is implementation evidence, not owner acceptance.

Local verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 851 passed,
  zero failed, six ignored by their existing default test settings.
- The strengthened PTY scenario was rerun successfully after adding saved CLI
  listings, manager restoration, saved-size and malformed save-request checks.
- `cargo fmt --all -- --check` and
  `cargo clippy --all-targets --all-features --locked --offline -- -D warnings` passed.
- `mdbook build` passed. Linux CI and owner acceptance remain unverified.
