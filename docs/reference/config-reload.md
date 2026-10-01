# Configuration hot reload

Foreground sessions and named-session servers check their selected configuration
file every 500 ms. Edit the file selected at startup to update the running
session. Relative paths are anchored to the startup working directory; attaching
with another `--config` path does not replace the server's source.

```sh
rustmux new --detached work --config ./work.toml
# Edit ./work.toml, then inspect the server's applied settings:
rustmux show-config -s work
```

| Setting | Effect of a successful reload |
| --- | --- |
| Supported shortcuts and mode bindings | Update together at the next safe input boundary; footer and Help use the new bindings |
| Notifications | Update existing panes, including the hidden pane retained for undo |
| `remain_on_exit` | Update the session policy; disabling it also removes already drained, exited panes that use this default. Explicit project/pane overrides still apply |
| `shell` | Used when creating or respawning panes; existing child processes keep running |
| `scrollback_lines` | Used by future panes and as the history limit for future saves; existing panes keep their current history capacity |
| Saving options and autosave interval | Used for subsequent captures; an already captured or queued snapshot may finish using its previous options |

Shell resolution keeps its existing precedence: nonempty `RUSTMUX_SHELL`,
configured `shell`, nonempty `SHELL`, then `/bin/sh`. Updating `shell` cannot
override the server's `RUSTMUX_SHELL` environment. Changes to environment
variables in another process do not change the running server's environment.
Changing the autosave interval restarts its timer. Saving settings apply to named
sessions; foreground unnamed sessions do not gain snapshot storage.

The LOCKED entry key (normally Ctrl-B) and `clear_defaults` still require a
server restart. A file changing either is rejected as a whole, including its
other settings. Connected clients continue using the server's existing entry
policy. This increment does not add theme settings or additional binding actions
from the independent `main` track.

## Applying updates and handling errors

A valid update applies atomically. With an attached terminal it waits for LOCKED
mode, an empty input queue, and the end of paste, partial input reports, mouse
presses or drags, History, Help and editing prompts. Actions already in progress
finish with their original bindings. A later invalid file cancels an unapplied
candidate. Detached sessions apply valid updates in their server loop, so new
panes created through script control also use the updated settings.

Invalid TOML, unsupported values for implemented settings, unreadable files and
restart-only changes retain the last successfully applied configuration. Errors
appear in the attached footer and in `show-config`. Fixing the file clears the
error. Compatible ignored options retain the ordinary startup parser's behavior;
use `check-config --strict` to find these options before saving.

Deleting a discovered default file selects built-in defaults, provided that this
does not change the restart-only entry policy. Deleting an explicit `--config`
file reports a read error and retains the last good configuration. Recreating
either file is detected automatically.

The background reader accepts regular files (including symlinks to regular
files) up to 512 KiB of UTF-8. It compares file contents, including changes that
preserve size and modification time. A single worker reads and parses files and
replaces a bounded mailbox's previous update; terminal loops only sample that
mailbox without blocking. Special files are rejected. Worker shutdown does not
wait on filesystem I/O. This bound applies to reloads; startup still uses its
existing reader. A save observed midway through writing can temporarily fail
validation, then recover on the next check.

## Inspecting a running server

`show-config -s NAME` returns TOML and is available through pipes, inside a pane,
or while the server is detached. Unlike `check-config`, it reports what the
server has actually applied rather than reading the current file for a future
startup.

| Field | Meaning |
| --- | --- |
| `path` | Absolute startup-selected source path |
| `generation` | Number of changed configurations successfully applied since this server started |
| `pending` | Whether a valid newer configuration is waiting for a safe boundary |
| `error` | Latest reload error, omitted when there is none |
| `new_window_key` | Current preferred NORMAL-mode new-window key, omitted if that action is inactive |
| `[settings]` | Applied scalar settings, using the same fields as configuration diagnostics |
| `[session_manager]` | Applied manager bindings on this server; the client-side manager uses its own selected source |

Pane-mode bindings are validated and applied but are not flattened into this report;
Help displays their active definitions. The report describes the session's
defaults, not per-pane overrides, existing child environments or existing
history capacities. The low-level library entry points that receive raw settings
without a loaded configuration do not enable a watcher and cannot report reload
status.

## Review and verification

Read `src/config/reload.rs`, source selection in `src/config.rs`, then the safe
input boundary and application paths in `src/terminal.rs`. Existing notification
and snapshot services receive their new options through `src/pane.rs` and
`src/session/snapshot.rs`; the control endpoint exposes status through
`src/control.rs` and `src/terminal/control.rs`.

Four background-reader tests cover invalid updates, deletion semantics,
same-size/same-timestamp changes, cancellation of pending updates, restart-only
policy, oversized files, invalid UTF-8 and FIFOs. An input-boundary unit test
checks modes, partial input, paste and mouse presses.
`tests/terminal_loop_reload.py` uses the actual binary, PTYs and local sockets
to check attached and detached updates, retained child PIDs and shell variables,
future shell/history settings, respawn and retention, deferred mode and paste
input, notifications on an existing pane, saving settings, reconnects, source
anchoring and default-file deletion. Linux CI and installed-client validation
have not been performed.

This implementation awaits the owner's final review and does not record feature
acceptance in the shared ledger.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 875 passed,
  zero failed, six existing tests ignored by default, across 47 test targets.
- All 19 real PTY scenarios passed, including the new hot-reload scenario.
  The five new library tests passed as part of the same run.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  Rust formatting, `git diff --check` and the mdBook build passed.
