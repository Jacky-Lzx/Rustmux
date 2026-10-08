# Saved workspace deletion

The Session Manager now offers `Delete` for a stopped `SAVED` workspace.
Press `d` once to see `Press d again to delete saved 'NAME'`, then press `d`
again immediately to remove its disk snapshot. Any other key cancels the
confirmation. Moving selection, a changed session list, or applying new bindings
also cancels it. The confirmation belongs to both the selected name and its
live/saved state.

The manager stays open after deletion, refreshes its list immediately and shows
`Deleted NAME`. The workspace disappears from `ls` too. Other snapshots remain.
Deleting the last saved entry leaves the manager open so you can create a new
session or close it. Opening it again still uses the ordinary single-session
shortcut. A subsequent `new NAME` starts a fresh workspace with no saved history.

On live rows, `dd` still means `Kill`: it stops the server and retains its
snapshot. Stop the server first to delete its saved workspace. No command-line
snapshot deletion command is introduced in this increment.

## Keys and errors

The existing configurable `delete` action applies to both live and saved rows:

```toml
[session_manager]
delete = ["Ctrl x"]
disconnect = [] # Release the default Ctrl-X disconnect binding.
```

With this configuration, press Ctrl-X twice consecutively. `delete = []`
disables both actions. Bindings hot reload while the manager is open. Printable
keys remain text during search/name entry; leave the editor with Esc before
deleting. Full and compact footers distinguish `Kill` and `Delete`, subject to
available width.

Deletion and saving share one pending operation slot. Repeated operation keys
while busy do not enqueue more work. Navigation, editing and configuration
updates continue to work. Closing the manager joins its workers and waits for
an already accepted operation to complete. The footer reports `Delete failed`
when deletion cannot be completed, and you can retry without reopening the view.

Deletion validates the private snapshot directory and rejects symlinks,
nonregular files and files with group/other permissions or a different owner.
It does not decode the snapshot, so a corrupt private snapshot can be forgotten.
A missing snapshot is an error rather than a claim that this operation removed
it. Success follows unlink and directory sync. If directory sync fails after
unlink, the footer reports failure even though the file has been removed;
durability cannot then be confirmed.

Creation/restoration and deletion share an exclusive nonblocking workspace lock.
Creation holds it while loading the snapshot and binding its runtime socket.
Deletion holds it while checking for an endpoint and removing the snapshot.
Any runtime endpoint is refused, including one whose server is starting,
stopping or stale. A busy lock also reports failure and preserves the snapshot.
This prevents a stale saved row from deleting the snapshot of a running server.
A private `NAME.workspace` lock sidecar remains in the runtime directory after
server exit/deletion; it is not listed as a session. Retaining its inode prevents
concurrent callers from locking different files for the same name.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

The main changes are in `src/session/picker.rs`, its `worker.rs` module,
`src/session.rs`, `src/session/supervisor.rs` and `src/persistence.rs`.
Two new unit tests verify deletion scope, unsafe metadata, corrupt snapshots,
startup lock contention and runtime endpoint refusal. The new real PTY scenario
`tests/terminal_loop_saved_delete.py` verifies confirmation/cancellation, hot
reloaded keys, old-key disabling, lock failure and retry, preservation of another
workspace, disappearance from `ls`, open-manager refresh and continued use after
deletion. Existing live termination and restoration scenarios remain enabled.

This increment awaited the owner's review. It does not record acceptance in the
shared ledger. Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 884 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 21 real PTY scenarios passed, including saved deletion, live termination,
  restoration, manager controls and config reload.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
