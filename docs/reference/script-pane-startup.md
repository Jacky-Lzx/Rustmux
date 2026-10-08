# Script pane startup

```sh
rustmux window new -s work --name checks --cwd /absolute/project --command 'cargo test'
rustmux pane split -s work -p 2 --down --cwd /absolute/project --command 'make watch'
rustmux window new -s work --cwd /absolute/project
```

`window new` and `pane split` accept optional `--command` and `--cwd` arguments.
The new pane gains focus and its runtime ID is printed on success, following
the existing creation behavior. Splitting a pane in an inactive window selects
that window and the new pane. Omitted `-p ID` targets the active pane. Each
command accepts `-s SESSION`, defaulting to `default`, and works with attached
or detached servers without acquiring the interactive-client lease.

`--command` runs through the running server's configured shell with `-c`.
It accepts a nonblank string of at most 4096 UTF-8 bytes without NUL, with the
same validation as project commands and respawn overrides. Quote the command
to keep it one CLI argument. Shell expansions and operators are intentional
parts of the command. An omitted command starts an interactive shell with `-i`.

`--cwd` must name an absolute existing directory accessible to the server.
It is passed as the child's working directory, not evaluated as shell code.
Relative paths, missing paths and regular files are rejected. A command's
`cd` can subsequently change its working directory. When `--cwd` is omitted,
new windows inherit the active pane's directory and splits inherit the target
pane's directory using the existing OSC 7/process fallback rules. The control
client's current directory is not a source for inheritance.

Names, startup fields, targets and split geometry are validated before launch.
Invalid requests or a shell exec failure return nonzero while preserving the
existing layout, pane IDs/processes, active window, remembered selection and
interactive overlays. Failed attempts may consume an unused runtime ID;
IDs are opaque and need not be consecutive. A successful creation dismisses
overlays and resets shortcuts to Locked mode through the existing control
refresh path. Existing pane/size limits and temporary-editor restrictions apply.

Success means the shell was launched and the pane was created, not that the
startup command completed successfully. For example, an unknown executable
inside a valid shell command can produce an exited pane even though creation
returned zero. The ordinary `remain_on_exit` server setting determines whether
completed jobs close automatically or remain available for capture/inspection.
It defaults to false. With retention enabled, `pane respawn -p ID` reruns the
recorded command at the pane's current dimensions using its tracked directory.
Use `pane list --toml` to observe exit status and output completion, and poll
output markers rather than treating creation as an application-ready barrier.

These commands and directories enter the next manual or configured automatic
snapshot through the existing saving rules. Restoration starts fresh processes
and reruns recorded startup commands; the control client and an external
project layout file are not needed. Ordinary commands later typed or sent with
`pane send-keys` do not become startup commands. Omitting `--command` does not inherit
the source pane's command. Creation alone does not force a snapshot disk write.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

This increment follows reviewed pane-resize commit `5ac76bc`, now merged into
`main-human`. `main` at `57d598657ad7acf00d6a0ddf734fba8f48d50e4c` also lacks these
creation arguments, though project layouts already support startup commands.
The implementation reuses the human track's `Pane::spawn_with_startup`, shell
exec handling, split factory, retained-job lifecycle and snapshot metadata.

Read CLI/request fields in `src/control.rs`, then startup validation and creation
in `src/terminal/control.rs`. The optional wire fields preserve old creation
requests; an older server rejects requests containing the new fields. The
interactive handshake, save format and existing no-argument commands remain
compatible. No startup command is injected into a shell's input queue.

CLI tests check optional defaults and preservation of spaces/operators in command
and directory arguments. Wire tests accept old requests without startup fields.
`tests/terminal_loop_pane_startup.py` uses owned named servers, an attached client,
real command processes and actual PTY dimensions. It checks inherited and explicit
directories, inactive-window splits, preserved source process state, directory
metacharacters, missing/relative/file paths, invalid commands and wire NULs,
unknown targets, rejected split geometry, real shell exec failures, remembered
selection/History isolation, retained job exit and respawn, detached creation,
saved metadata and command replay at restored sizes. Typed commands are verified
to stay outside startup metadata. Client exits check restored terminal attributes.

This branch awaited owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 921 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 30 real PTY scenarios passed, including command startup, pane resizing,
  script focus, snapshot restoration, session management and terminal-device faults.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
