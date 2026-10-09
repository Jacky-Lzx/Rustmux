# Session Manager controls and saving

Open the Session Manager with Ctrl-B then Ctrl-W inside a named session, or
`rustmux attach` when multiple running/saved workspaces exist. Its shortcuts now
use the attaching client's selected configuration, including `--config PATH`.
The manager checks that startup-selected file every 500 ms while it is open.

On opening, the manager selects the first session in display order that has no
attached client, skipping the current session. Saved workspaces are eligible
too. If no other unattached session exists, it selects the first row (the current
session when opened from one). Navigation and list refreshes retain your selection.

```toml
[session_manager]
up = ["k", "up"]
down = ["j", "down"]
search = ["/"]
complete = ["tab"]
open = ["enter"]
create = ["a"]
save = ["Ctrl a"]
rename = ["Ctrl r"]
disconnect = ["Ctrl x"]
delete = ["d"]
cancel = ["esc", "q"]
backspace = ["backspace"]
```

Each array replaces that action's defaults. Omitted actions retain defaults;
`[]` disables the action. This table is independent of `clear_defaults` and the
pane-mode keybindings. Keys include printable ASCII characters, `up`, `down`,
`enter`, `tab`, `esc`, `backspace`, `space`, and `Ctrl a` through `Ctrl z`.
Special key names are case insensitive; printable characters retain their case.
Terminal-equivalent aliases share one key: Ctrl-M/Ctrl-J/Enter and
Ctrl-H/Backspace. Duplicate keys, conflicts with other effective actions,
unsupported key names, non-string entries and arrays longer than 16 keys are
errors. Ctrl-C remains an unconditional interrupt/close key and cannot be bound.

Navigation and action hints follow the effective bindings and available width.
During search or name editing, printable action keys remain text; use arrows or
control keys for navigation, completion, confirmation, cancellation and saving.
Ctrl-R opens the selected live or saved workspace name editor from the table or filtered results;
see [Saved Workspace Rename](saved-session-rename.md) and
[Live Session Rename](live-session-rename.md).
Name entry supports confirmation, cancellation, backspace and saving.
Hints in editors only show usable keys.
The manager preserves the current search/name text during a valid reload.
Partial escape reports finish before the new bindings apply. Invalid updates
retain the entire previous keymap and show an error. Replacing a keymap cancels
an armed deletion/termination confirmation.

Unlike the server's entry-key policy, the manager's bindings can update while
editing even if the file also changes a server restart-only setting. The manager
is a separate client view; its configuration source need not match the attached
server's source. Opening it requires a valid initial configuration. Fixing an
invalid file while the view remains open clears the error. Explicit-file
deletion keeps the previous bindings; default-file deletion restores defaults.

`config check --toml` and `config show` include a `[session_manager]` table of
effective bindings. The former reads the selected file, while the latter reports
the named server's applied configuration. `config default` exports the manager
defaults too. `rename` supports live and saved workspaces. Unknown manager
actions remain ignored and are reported by `config check`; `--strict` rejects
those warnings.

## Manual saving

Ctrl-A saves the current session when the manager was opened from one. Moving
the selection to another session does not change that target. In the standalone
`attach` picker, Save targets the selected live session. A saved-only workspace
cannot be saved again without restoring it; if the current session disappears,
Save reports an error rather than silently saving another session.

The manager uses the existing private save endpoint. The live server chooses
the saving options from its own configuration and captures its own workspace.
Saving stays available with autosave disabled. One save runs at a time; repeated
Save presses while it is pending do not queue additional writes. Keyboard input,
search and configuration updates remain available during the save.

The footer displays `Saving NAME`, then `Saved NAME` only after the server replies
that its atomic write and directory sync have completed. Errors appear as
`Save failed`. The manager waits up to five seconds for an acknowledgement;
a timeout cannot confirm the result and does not cancel an accepted server write.
The CLI's existing save wait behavior is preserved. Save results temporarily
take priority over configuration/list errors; another key restores the ordinary
hints or outstanding error. Closing the manager waits for an accepted save
request to finish or time out.

## List refresh and termination

While open, the manager refreshes the running/saved session list in the
background every 500 ms and immediately after a save, deletion or rename completes. Search and name
editing remain open. Selection follows the same session name when it still
exists; otherwise it moves to the nearest valid row. A changed list cancels an
armed deletion/termination confirmation. Refresh failures retain the displayed list and
show an error. A selected workspace is checked again before attachment or
restoration, so a workspace that stopped while the picker was open can restore
from its saved snapshot.

Press the `delete` key twice consecutively to terminate a live session, keeping
its snapshot, or to delete the snapshot of a saved-only workspace. Default `dd`
requires confirmation in both cases. The footer distinguishes `Kill` from
`Delete`. See [Saved Workspace Deletion](saved-session-delete.md) for behavior
and safety checks. Ctrl-R renames live or saved workspaces. Ctrl-X gracefully
disconnects the displayed client of another attached session; see
[Session Manager Disconnect](session-manager-disconnect.md).

Configuration reading and list/save operations use bounded mailboxes outside
the input loop. Before returning a choice, the manager shuts down and joins its
workers so restoration/creation can safely enter the supervisor's fork path.
The pane processes continue running while the manager is open.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

Read `src/config/manager.rs`, then `src/session/picker.rs` and its
`worker.rs` module. `src/config/reload.rs` provides the shared bounded reader
and explicit picker shutdown. Source propagation and revalidation are in
`src/session/supervisor.rs`; the bounded acknowledgement wait uses
`src/session/snapshot.rs`.

Binding tests cover replacement, disabling, alias conflicts, validation,
editing semantics and exported-binding round trips. Picker tests cover current
versus selected save targets and saved/missing targets. The real PTY scenario
`tests/terminal_loop_manager.py` pauses its own server to verify responsive
editing and absence of early success, then checks acknowledgement, write
failure isolation, config reload/recovery, editing text, list refresh and
preserved shell state. Existing creation, termination and restoration PTY
scenarios provide cumulative coverage of worker shutdown before forking.

This branch awaited the owner's final review and does not record feature
acceptance in the shared ledger. Linux CI and installed-client validation have
not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 881 passed,
  zero failed, six existing tests ignored by default, across 47 test targets.
- All 20 real PTY scenarios passed. Four new library tests cover binding
  behavior, save-target selection and acknowledgement deadlines; the new CLI
  test checks manager diagnostics and default inheritance.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  Rust formatting, `git diff --check` and the mdBook build passed.
