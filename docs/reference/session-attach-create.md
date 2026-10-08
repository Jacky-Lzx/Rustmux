# Attach or create a named session

```sh
rustmux attach work --create
rustmux --config ./dev.toml attach work --create
```

`attach NAME --create` offers a single entry point for a named workspace:

- A running server is attached through the ordinary client bridge. Its server
  and pane PIDs, shell variables, layout, history and configuration stay intact.
- A saved workspace with no running server is restored into a new server and
  fresh shells. Layout, active window/pane and working directories are restored;
  saved history is loaded when enabled in the startup configuration. Commands
  explicitly recorded by a project layout retain their existing replay behavior.
- A name without a running server or snapshot starts a fresh session using the
  selected startup configuration.

The name is required. `--create` is a long option only; the existing `-c PATH`
configuration option retains its meaning. Plain `attach NAME` keeps requiring a
running server, and unnamed `attach` keeps its Session Manager behavior.
`new NAME` continues to reject a running name. History persistence and autosave
remain opt-in; see [Session Snapshots](session-snapshots.md).

When a server is already running, this command does not load or apply the
attaching client's startup config or disk snapshot. `--config PATH` is retained
for later creation/restoration through the manager. The running server continues
watching its own configuration source.

Fresh and restored servers perform their first handshake with the real
attaching terminal. Restoring a snapshot saved at a different width keeps old
output in history above the fresh screen. The command verifies a usable terminal
and nonzero dimensions before binding a new server. It restores terminal modes
on detach and on errors, using the existing client path. Nested interactive
Rustmux invocations remain rejected before creation.

## Failure and concurrency behavior

Creation is allowed only after the existing private socket/PID checks report
that no server is running. Unsafe sockets or PID files, corrupt locked PID
records, busy client leases, config errors and invalid snapshots are errors.
An attached server is never replaced or disconnected to make room for this
command. Only one displayed client can attach at a time.

Creation uses the same workspace lock and private endpoint binding as `new`.
An existing stale private socket can be reclaimed by that path, while a file or
insecure endpoint is preserved and rejected. Concurrent creation, rename,
deletion or disconnect can hold the workspace lock or claim the endpoint first;
this attempt then fails without overwriting another session or snapshot. There
is no automatic retry or takeover. Retry after the competing operation finishes.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

This increment follows the reviewed Session Manager disconnect feature. Its
reference is `main` commit `57d598657ad7acf00d6a0ddf734fba8f48d50e4c`,
`src/cli.rs` (`AttachArgs`) and `src/session.rs` (`attach_or_create`). Intentional
differences preserve human-track conventions: the name is required with
`--create`, `-c` stays the global config option, live servers retain their startup
configuration, and creation is restricted to verified missing-server errors.
The existing single-client lease and fail-on-contention behavior remain in use.

Read `src/cli.rs`, the Attach dispatch in `src/main.rs`, then
`src/session/supervisor.rs::attach_or_create`. The implementation reuses existing
creation, snapshot validation and client attachment instead of adding a new
server or protocol path. CLI unit coverage checks the name requirement and
preserved `-c` meaning; the nested-session guard includes the new option.

`tests/terminal_loop_attach_create.py` exercises real PTYs and owned named
servers: first creation, explicit config selection, duplicate-client rejection,
unchanged live process identities and shell state despite invalid startup config
and snapshot, saved restoration at a changed size, preserved layout/focus/cwd and
history, fresh shell state, strict plain attachment, restored termios, missing
terminal, nested invocation, invalid config/snapshot, workspace lock contention,
unsafe pathname preservation and stale-socket recovery.

This branch awaited owner review. It does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 903 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 25 real PTY scenarios passed, including attach/create, restoration,
  autosave, session management, rename and disconnect.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
