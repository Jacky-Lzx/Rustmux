# Script pane resize

```sh
rustmux pane list -s work --toml
rustmux pane resize -s work --direction right
rustmux pane resize -s work -p 2 --direction down --cells 3
```

`pane resize` moves a pane's nearest ancestor separator of the matching
orientation. `left`/`right` move a column separator; `up`/`down` move a row
separator. Directions describe separator movement, as in interactive resizing:
`right` grows the left subtree and shrinks the right subtree, regardless of
which side contains the target. For a nested pane, resizing can therefore
change several sibling panes. A deepest matching separator at its limit does
not fall back to an outer separator.

`-p ID` targets a runtime pane, including one in an inactive window. Omission
targets the active pane. IDs survive this operation but must be enumerated again
after workspace restoration. `-s SESSION` defaults to `default`. `--direction`
is required; `--cells` defaults to one and accepts 1–65535. Larger movements
clamp to the largest legal displacement, retaining at least one content cell
in each dimension after pane borders. An oversized request uses bounded
geometry calculations rather than executing thousands of single-cell steps.

Success prints nothing and returns zero if a separator moves. Unknown IDs,
invalid amounts or directions, a missing matching separator, a zoomed target
window and an already reached boundary return nonzero without changing the
layout. The server independently rejects zero amounts and malformed wire
requests. Zoom must be turned off before resizing a window's tiled layout.
Failed requests preserve History, prompts and shortcut state.

Resizing preserves pane IDs, owned processes and screen contents through the
existing resize/reflow path. It preserves the active window, each window's
remembered pane, last-window history, order and input focus. Retained exited
panes remain exited. Successful mutating controls dismiss History/help/prompts,
reset shortcuts to Locked mode and redraw. They do not emit a focus transition
when the active pane remains the same.

Both attached and detached event loops synchronize the affected PTYs and screen
models, including inactive windows. A successful reply acknowledges accepted
geometry; it is not a barrier for PTY ioctl completion, application SIGWINCH
handling or rendering. Poll application output when coordinating asynchronous
commands. A synchronization I/O failure propagates through runtime cleanup;
the server cannot continue with partially committed PTY/screen sizes.

The changed proportions enter the next manual or configured automatic snapshot
under the existing saving rules. Resizing alone does not force a disk write.
After a save, `attach NAME --create` or the Session Manager can restore the
layout and remembered focus into fresh processes. Proportions adapt to the new
outer terminal dimensions. This command also works while the server is detached
without taking the interactive-client lease.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

This increment follows reviewed window-rename commit `c08c29b`, now merged into
`main-human`. `main` at `57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks this
script command. Interactive resizing exists in both tracks. The human track's
control channel is extended without changing client handshake or save formats.
An older running server rejects the new action until restarted.

Read CLI/request construction and refresh classification in `src/control.rs`,
then target/error handling in `src/terminal/control.rs`, separator selection in
`src/layout.rs`, border-aware candidate validation in `src/pane_set.rs` and
attached/detached synchronization in `src/terminal.rs`. Candidate layouts are
validated before committing geometry. At an outer border, a raw layout leaf of
one cell has zero PTY content; bounded binary search finds the largest safe
movement. Interactive keyboard resizing shares this content-size check.

Layout tests cover nested separator choice, both orientations, clamping without
fallback, unchanged focus and identities, proportional outer resize, invalid
targets, zero amounts and zoom. Pane ownership tests cover non-clone contents,
one-cell PTY minima and failed-operation isolation. CLI tests check defaults
and bounded positive amounts.

`tests/terminal_loop_pane_resize.py` uses owned named servers and real PTYs.
Raw applications report their actual kernel dimensions, process IDs and input
bytes. It checks nested/inactive windows, defaults, input routing, absence of
spurious focus events, cursor mode, remembered pane/last-window focus, malformed
wire requests, failed-request History isolation, successful refresh, zoom,
65535-cell clamping, detached mutation, saving and restored PTY dimensions.
Client exits also verify restored outer terminal attributes.

This branch awaited owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 918 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 29 real PTY scenarios passed, including targeted resizing, script focus,
  snapshot restoration, session management and the terminal-device fault probe.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
