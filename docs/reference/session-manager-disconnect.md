# Session Manager disconnect

Select another `ATTACHED` session in the Session Manager and press Ctrl-X.
The selected client's terminal is restored and its attachment exits normally.
The server, pane processes, layout, scrollback and shell variables keep running.
Its row changes to `DETACHED` and another client can attach afterward. Existing
snapshots stay intact; the operation does not implicitly save or delete them.

The manager shows `Disconnecting NAME` while waiting, then `Disconnected NAME`
only after the server has acknowledged the request and the original displayed
client's exclusive lease has been released. It refreshes the list after the
operation. A client that attaches after completion can change the row back to
`ATTACHED`; completion confirms the prior disconnect, not a permanent ban.

```toml
[session_manager]
disconnect = ["Ctrl x"]
```

The action supports replacement, disabling with `[]`, conflict validation,
hot reload, `default-config`, `check-config` and server `show-config` reports.
If another manager action uses Ctrl-X, disable or remap `disconnect` to avoid
a binding conflict. Hints follow the selected row and effective key. The table prioritizes the
Disconnect hint for an eligible row; filtered search also accepts control
bindings. Printable bindings stay text during search or name editing.
Disconnect is unavailable in the name editor.

A current row, a detached row, a saved-only row or no selection reports an
inline error without sending a request. Opening the manager from a session has
already released that attachment; this operation targets other clients. Cancel
still returns to the session that opened the manager, and Save keeps its current
session target. Ordinary local detach remains Ctrl-B then `d`. There is no new
CLI disconnect command or client takeover mode in this increment.

## Coordination and failure handling

The worker captures the selected server PID and uses the existing private,
bounded control endpoint. It takes the session workspace lock before verifying
the PID and issuing the request, so rename/restoration and a replacement client
cannot race the lease wait. The server validates its own PID again. A stale
controller cannot silently disconnect a replacement server. If the selected
server has no attachment by the time it processes the request, it reports an
error without scheduling a later client for disconnection.

The server requests the existing protocol `Detach` after queued terminal output
and any name notice drain. Detach takes priority over a simultaneous request to
open the manager. No process signals are sent by the implementation. Existing
client terminal restoration and exit paths perform the actual cleanup. The
protocol remains version 7; this feature adds only an internal control request.

The worker pins and checks the client lock's inode while waiting for its lease
release. It does not create a missing lock to manufacture success. The release
wait has a five-second deadline counted from the start of the operation and
uses the existing control transport timeouts. A stopped client or a terminal
that cannot drain output may time out. Failure then says that the request may
still complete; it neither kills the client/server nor cancels an already
accepted detach. The current list is refreshed to show observed state.

The input loop keeps navigation, search, text editing and config reload working.
Repeated disconnect keys do not queue another operation. Save, delete and rename
share the same pending-operation slot. Leaving the manager waits for accepted
work to finish or time out before joining the worker and returning to a possible
fork path. Config changes do not cancel accepted work.

## Review and verification

Read `src/config/manager.rs` and its exported defaults, then
`src/session/picker.rs` and `picker/worker.rs`. The control request, workspace lock
and server checks are in `src/control.rs` and `src/terminal.rs`.
`src/session.rs` waits for the original client lease to be released. Existing
`session/frontend.rs` and `client.rs` carry the protocol Detach and restore the
client terminal. Named servers also treat a peer connection disappearing
during the attachment as a disconnect, preserving panes instead of exiting
when a queued Detach write fails.

The existing snapshot PTY test now waits with a deadline for the server's detach
checkpoint instead of assuming it finishes before the client exits. The saved
delete fixture explicitly releases Ctrl-X before reusing it for Delete.

Three new unit tests cover binding replacement/disabling/conflicts and editor
text, eligible targets and hints, and lease release/deadline/missing-lock errors.
CLI diagnostics now recognize disconnect as implemented and still report
unknown actions. The new real PTY scenario `tests/terminal_loop_disconnect.py`
checks stale-controller rejection, repeated keys, responsive search and absence
of premature success while an owned client is stopped, reattachment, restored
termios, unchanged server/pane PIDs, shell variables and snapshots, saved/
detached/current guards, hot-reloaded bindings, returning to the current session
and timeout followed by completion after the client resumes. It also queues a
disconnect while an owned server is paused, abruptly terminates only its owned
client, and verifies that the late request and peer disappearance preserve the
server and panes. Depending on EOF ordering, the request may report no attached client
or be accepted before the connection disappears.

This branch awaits owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 901 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 24 real PTY scenarios passed, including other-client disconnect, live/
  saved rename, saved deletion, restore/autosave and manager config reload.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
- An earlier parallel run failed once during saved-session restoration with
  system `ERANGE` (Result too large). The focused snapshot scenario and final
  four-thread full run passed; the cause of that single failure is undetermined.
