# Script window rename

```sh
rustmux rename-window -s work 'editor'
rustmux rename-window -s work -w 2 'build logs'
rustmux list-panes -s work --toml
```

`rename-window` changes the active window's name by default. `-w NUMBER` (or
`--window NUMBER`) targets a one-based display position, matching `list-panes`
and the window bar. Positions change after removal or reordering; enumerate
current windows before using a stored number. The command takes a required
positional name, accepts `-s SESSION` and defaults to session `default`.
Use `--` before a name beginning with `-`.

Names contain 1–128 UTF-8 bytes without control characters. Spaces and Unicode
are allowed, and the limit counts bytes rather than characters. Duplicate names
are allowed: names are metadata, while numbers select a position. The bar clips
labels to the physical terminal width; `list-panes --toml` exposes the full name
for every pane in the window. The plain listing continues to show each pane's
terminal title rather than its window name.

Success prints nothing and returns zero. Invalid names, zero/unknown window
numbers and malformed requests return a nonzero status before mutating names
or focus. The server validates the same constraints when CLI validation is
bypassed. Failed requests preserve interactive overlays and shortcut state.

Renaming preserves the active window, its selected pane, the target window's
remembered pane, last-window history, order, layout and owned processes. It does
not emit an application focus transition. Successful requests use the existing
mutating-control refresh path: History, help and prompts close, pending shortcut
handling returns to Locked mode and the bar redraws, including changes to an
inactive target or a request repeating the existing name. Pane input modes and
PTY dimensions continue through the existing runtime without a focus change.

The command works with an attached or detached server without acquiring the
interactive-client lease. Changed names are included in the next manual or
configured automatic snapshot, using the existing saving rules. They remain
ordinary metadata during restoration. A rename alone does not force a disk
write; restart persistence requires a subsequent save. `attach NAME --create`
or the Session Manager can restore a saved workspace into a new server.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

This increment follows reviewed terminal-device commit `4aaa820`, now merged
into `main-human`. `main` at `57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks
this scripting command, though interactive window renaming already exists in
both tracks. This extends the human track's control channel without importing
the other implementation or changing the displayed-client/save protocols.
An older running server rejects the new control action until it is restarted.

Read CLI/request construction and refresh classification in `src/control.rs`,
then shared number/name validation and mutation in `src/terminal/control.rs`.
The existing `Windows::rename` changes only metadata. Number lookup is shared
with `select-window`; optional string validation is shared with new/broken
windows. Existing transport size limits, deadlines and response bounds apply.

`tests/terminal_loop_window_rename.py` uses named servers, real attached terminals
and independent control clients. It checks active/inactive targets, Unicode
labels, duplicate names, reordered positions, the exact byte limit, invalid
names and raw wire requests, unchanged IDs/PIDs and shell variables, actual
keyboard routing, remembered pane/last-window focus, failed-request History
isolation, successful refresh and prefix reset, detached mutation, snapshot
focus/layout/names and restoration without executing a shell-like label.
A raw application checks absence of spurious focus bytes, application cursor
mode, unchanged process metadata and actual PTY dimensions. Client exits verify
restored outer terminal attributes.

This branch awaited owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 912 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 28 real PTY scenarios passed, including window rename, script focus,
  snapshot restoration, session management and the terminal-device fault probe.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
