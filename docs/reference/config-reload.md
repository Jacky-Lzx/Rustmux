# Configuration hot reload

Foreground sessions and named-session servers check their selected configuration
file every 500 ms. Edit the file selected at startup to update the running
session. Relative paths are anchored to the startup working directory; attaching
with another `--config` path does not replace the server's source.

```sh
rustmux new --detached work --config ./work.toml
# Edit ./work.toml, then inspect the server's applied settings:
rustmux config show -s work
```

| Setting | Effect of a successful reload |
| --- | --- |
| Supported shortcuts and mode bindings | Update together at the next safe input boundary; footer and Help use the new bindings |
| `compact` | Resize existing panes and move mode/prompt status to the top row; reject the whole update if any window cannot fit the smaller canvas |
| `default_mode` | Update the mode used for subsequent runtime resets and attachments; preserve the current mode. Explicit switch-mode bindings keep their target |
| Theme | Update interface colors together; existing child cells and terminal defaults retain their colors |
| `mouse_hover_cursor` | Enable or disable window-control pointer feedback; disabling restores the application's latest pointer shape |
| `clipboard_read` | Enable or disable [rich clipboard reads](rich-clipboard.md); disabling cancels the active read lease and drops undelivered data |
| `clipboard_write` | Enable or disable [child OSC 52 writes](pane-clipboard.md) and [rich writes](rich-clipboard.md); disabling invalidates capture and cancels the write lease |
| `file_transfer` | Enable or disable the [OSC 5113 relay](file-transfer.md); disabling cancels active transfers and discards undelivered host replies |
| `drag_source` | Enable or disable [OSC 72 drag sources](drag-source.md); disabling cancels the gesture, drops staged data and unregisters the source |
| `drop_target` | Enable or disable [OSC 72 receiving](drop-target.md); disabling cancels the drop, discards staged data and unregisters all targets |
| Notifications | Update existing panes, including the hidden pane retained for undo; running commands use the latest policy at completion |
| `remain_on_exit` | Update the session policy; disabling it also removes already drained, exited panes that use this default. Explicit project/pane overrides still apply |
| `tab_name` | Switch automatically named windows between foreground program names (`application`, the default) and OSC terminal titles (`title`); explicit names remain fixed |
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
policy. See [Interface Themes](interface-themes.md) for presets, color overrides
and client/server ownership.

## Applying updates and handling errors

A valid update applies atomically. With an attached terminal it waits for LOCKED
mode, an empty input queue, and the end of paste, partial input reports, mouse
presses or drags, History, Help and editing prompts. Actions already in progress
finish with their original bindings. A later invalid file cancels an unapplied
candidate. Detached sessions apply valid updates in their server loop, so new
panes created through script control also use the updated settings.

Invalid TOML, unsupported values for implemented settings, unreadable files and
restart-only changes retain the last successfully applied configuration. Errors
appear in the attached footer and in `config show`. Fixing the file clears the
error. Compatible ignored options retain the ordinary startup parser's behavior;
use `config check --strict` to find these options before saving.

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

`config show -s NAME` returns TOML and is available through pipes, inside a pane,
or while the server is detached. Unlike `config check`, it reports what the
server has actually applied rather than reading the current file for a future
startup.

| Field | Meaning |
| --- | --- |
| `path` | Absolute startup-selected source path |
| `generation` | Number of changed configurations successfully applied since this server started |
| `pending` | Whether a valid newer configuration is waiting for a safe boundary |
| `error` | Latest reload error, omitted when there is none |
| `new_window_key` | Current preferred NORMAL-mode new-window key, omitted if that action is inactive |
| `[settings]` | Applied settings, using the same fields as configuration diagnostics |
| `[session_manager]` | Applied manager bindings on this server; the client-side manager uses its own selected source |

`[settings].notifications_enabled` and
`[settings].notification_excluded_applications` expose the applied notification
switch and normalized application list. `desktop_notifications` reports the
applied `desktop` option. In-flight application observations survive
a reload; see [Notification Filters](notification-filters.md).

Pane-mode bindings are validated and applied but are not flattened into this report;
Help displays their active definitions. The report describes the session's
defaults, not per-pane overrides, existing child environments or existing
history capacities. The low-level library entry points that receive raw settings
without a loaded configuration do not enable a watcher and cannot report reload
status.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

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

This implementation awaited the owner's final review and does not record feature
acceptance in the shared ledger.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 875 passed,
  zero failed, six existing tests ignored by default, across 47 test targets.
- All 19 real PTY scenarios passed, including the new hot-reload scenario.
  The five new library tests passed as part of the same run.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  Rust formatting, `git diff --check` and the mdBook build passed.
