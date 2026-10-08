# Saved workspace rename

Select a stopped `SAVED` workspace in the Session Manager, then press Ctrl-R.
The name editor starts with its current name. Backspace edits it, Enter confirms
and Esc cancels. Names accept at most 64 ASCII letters, digits, hyphens and
underscores. Empty confirmation shows an inline error and keeps the editor open.
Full and compact layouts label the editor `Rename`; ordinary creation still
uses `New` and `Create`.

Ctrl-R also works on the selected filtered search result. Cancellation returns
to the previous search query. The editor captures the source name when it opens;
list refreshes never retarget it to a different row. Configuration reload keeps
the typed name. While an accepted rename is pending, its name is frozen, repeated
confirmation does not queue additional operations, and Esc can leave the editor.
Leaving the editor does not cancel an accepted filesystem operation.

Success closes the rename editor, clears the search and selects the new name
when it is present in the refreshed list. The footer shows `Renamed OLD to NEW`.
The snapshot disappears under its old name from `ls` and appears under its new
name. Restoring the new name preserves the saved layout and history and starts
fresh shells, as usual. Confirming an unchanged name is a validated no-op.

## Bindings and scope

```toml
[session_manager]
rename = ["Ctrl r"]
```

The `rename` action now participates in replacement, disabling (`rename = []`),
conflict validation, hot reload, `default-config`, `check-config` and server
`show-config` reports. Printable bindings remain text while searching or editing
a name. For example, `rename = ["r"]` acts in the table; Ctrl-R then becomes
inactive. A control binding can enter rename from search. Hints follow available
width; saved rows show Rename first in the table's navigation header.

This page documents the saved-only operation. Live rows now use the separate
server-owned [Live Session Rename](live-session-rename.md) operation through the
same manager binding. No CLI rename command is introduced. Save, delete and
rename share one pending operation slot; accepted work finishes before workers
join and the supervisor can fork a restored server.

## Storage and failure handling

Renaming takes the source and target workspace locks in stable name order,
without waiting for a busy lock. Both names are checked for runtime endpoints;
starting, running, stopping and stale endpoints are all refused. Creation,
restoration and saved deletion use these same locks. A stale saved row therefore
cannot rename the snapshot of a session that started while the editor was open.

The storage operation opens the private snapshot directory without following a
symlink, validates source ownership and permissions, and rejects symlinks and
nonregular sources. It does not decode the snapshot, so a corrupt private
snapshot can also be renamed. The file's contents, inode and permissions remain
the same. This track has no saved connection-time sidecar to move.

The actual move is atomic and refuses *any* existing target, including a regular
file, directory or dangling symlink. macOS uses `renameatx_np` with `RENAME_EXCL`;
Linux uses `renameat2` with `RENAME_NOREPLACE`. Unsupported platforms/filesystems
report an error instead of falling back to an overwriting or multi-step move.
Success is reported after syncing the directory. If sync fails after the move,
the footer reports failure even though the file has already changed names;
durability cannot then be confirmed. The refreshed list reflects the actual
files. Other failures retain the source and target and keep the editor available
for correction/retry.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

Read `src/config/manager.rs`, then `src/session/picker.rs` and `picker/worker.rs`.
`src/session.rs` checks both workspace identities; `src/persistence.rs` performs
the exclusive filesystem operation. `src/config/diagnostics.rs` exports the new
default, and manager diagnostics recognize it through the action catalog.

Five new unit tests cover full/compact editor labels, source/target runtime
locks, unsafe/corrupt sources, unchanged-name validation, byte/inode preservation,
existing target types and competing renames. Existing binding/CLI tests now
verify the rename action and strict diagnostics. The real PTY scenario
`tests/terminal_loop_saved_rename.py` checks cancellation, collision, empty input,
source/target sessions starting during editing, lock contention, retry, editing under config reload, repeated
confirmation, list updates and restored split panes/history under the new name.

This increment awaited the owner's review and does not record acceptance in the
shared ledger. Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 890 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 22 real PTY scenarios passed, including saved rename, saved deletion,
  live termination, restoration, manager controls and config reload.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
