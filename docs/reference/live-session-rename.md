# Live session rename

Select an `ATTACHED`, `DETACHED` or `CURRENT` row in the Session Manager and
press Ctrl-R. Edit the existing name, confirm with Enter or cancel with Esc.
The configurable `[session_manager] rename` binding and editor behavior also
apply to saved workspaces. Names use the existing 64-byte ASCII restriction.
Success selects the new name and displays `Renamed OLD to NEW`.

The server renames its identity without restarting shells or panes. The pane
IDs/PIDs, layout, scrollback, shell variables and attached client stay intact.
`ls`, the manager, the window bar, `attach`, `save`, `kill` and scripting requests
use the new name. The old name becomes available for another workspace.
The current manager row follows the same socket inode across renames; Cancel
returns to that name and Save still targets the session that opened the manager.

An existing snapshot moves with the live session, preserving its contents,
inode and permissions. Later manual saves and autosaves use the new name.
Persistence configuration and whether history/autosave are enabled stay intact.
There is no separate CLI rename command in this increment.

## Coordination and failures

The manager captures the selected server PID when opening the editor and sends
a bounded control request to that server. The server checks that PID and source
name again. A replaced server, changed alias or saved row that becomes live is
refused instead of retargeting the operation. Same-name confirmation validates
the endpoint and succeeds without moving files.

The server acquires source and destination workspace locks without waiting,
checks private directory/file ownership and permissions, and plans the moves
before starting. Any existing destination artifact, including a snapshot or a
dangling symlink, causes failure. Source/target workspace operations and client
lease acquisition use the same locks. A snapshot writer in progress causes an
immediate retry error so it cannot later recreate a snapshot under the old name.

PID and client locks move as the same inodes, retaining open descriptor leases.
Save/control listeners and the session listener retain their descriptors and
connections. The session socket moves last. Individual moves use exclusive
no-overwrite rename on macOS/Linux. Directory sync completes before the new
in-memory identity is published and success is acknowledged. Cleanup follows
that shared identity and checks socket inodes before removing endpoints, so
reusing the old name does not let the old server delete the new workspace.

This is a coordinated series of file moves, not a crash-atomic filesystem
transaction. A process/system crash during it can leave mixed names. Ordinary
move/sync failures attempt reverse-order rollback; rollback failures are reported
explicitly and can also leave mixed artifacts requiring recovery. The manager's
five-second request timeout cannot cancel or confirm an operation already
accepted by the server. A refreshed list shows the resulting filesystem state.

## Client protocol and review

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

Protocol version 7 adds a bounded `Renamed` notice. The attached client updates
its supervisor identity without exiting input/output forwarding. Notices use
nonblocking writes and preserve output framing under backpressure. A local
manager shortcut can exit before reading a notice; the supervisor recovers the
same listener inode before opening the manager. Existing version-6 named
servers must restart to work with the new client binary. Restart those servers
only after saving any workspace data that should be restored.

Read `src/session/rename.rs`, then the identity/lease changes in `session.rs`,
`terminal.rs` and `control.rs`. Snapshot writer coordination is in
`session/snapshot.rs`; protocol/client notification is in `session/protocol.rs`,
`frontend.rs` and `client.rs`. Picker/worker and supervisor code preserve the
current session across manager operations.

Six new unit tests cover retained lock/listener/snapshot identity, exclusive
move rollback with a racing destination, snapshot-writer exclusion, bounded
name validation, client identity/output continuity and fragmented notices under
backpressure. Existing protocol round trips exercise the new frame at every
byte boundary. `tests/terminal_loop_live_rename.py` checks attached/current/
detached renames, collision failure, stale controller rejection, current Save
and Cancel, unchanged pane PID and shell variables, later manual/autosaves,
single-client exclusion, old-name reuse and cleanup isolation. The saved-rename
scenario still checks a saved editor whose source becomes live while editing.

This branch awaited owner review. It does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 897 passed,
  zero failed, six existing tests ignored by default, across 47 test targets.
- All 23 real PTY scenarios passed, including live/saved rename, saved deletion,
  restoration, current manager Save/Cancel, config reload and pane lifecycle.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
