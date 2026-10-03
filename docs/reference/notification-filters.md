# Notification filters

```toml
[notifications]
enabled = true
long_command_bell = true
command_duration_seconds = 5
exclude_applications = ["yazi", "nvim", "lazygit"]
```

These are the built-in defaults. Rustmux times shell-integrated commands from
`OSC 133;C` until `OSC 133;D` or the next `OSC 133;A`. A command reaching the
positive whole-second threshold rings the attached terminal once and sets the
usual pane-specific activity marker, unless an observed foreground application
matches an exclusion. Heuristic Enter-based output capture does not generate
completion reminders. `enabled` must be true, together with at least one delivery option:
`long_command_bell` for terminal BEL or opt-in `desktop` for OSC 99 messages. See
[Desktop Command Notifications](desktop-notifications.md).

`exclude_applications` replaces the default list. Use `[]` to allow every
application, or `enabled = false` to disable automatic completion reminders.
These options do not suppress an application's ordinary BEL activity marker.
Terminal titles are independent of process names and cannot select an exclusion.

Entries match whole executable basenames with ASCII case-insensitive comparison;
there are no wildcards, substrings or command-line argument matches. Whitespace
is trimmed, paths such as `/usr/bin/nvim` become `nvim`, and duplicates retain
the first spelling. The list accepts at most 256 entries before deduplication;
each normalized name must contain 1–256 bytes without control characters.
Wrong types, empty names and invalid limits reject startup or retain the last
valid configuration during reload. Other unknown notification options retain
the configuration parser's ignored-option diagnostics.

## Runtime updates and observation

[Configuration hot reload](config-reload.md) updates existing, future and hidden
close-undo panes. A command keeps its observations across updates, then uses the
latest applied switch, threshold and exclusions when it completes. Adding an
exclusion while a quiet job runs therefore suppresses its reminder; removing an
exclusion permits the reminder. Applying settings does not restart child jobs.

`rustmux show-config -s work` exposes the applied values in
`[settings].notifications_enabled`, `long_command_bell`,
`command_duration_seconds` and `notification_excluded_applications`.
`check-config` and `default-config` expose the same settings through their
existing diagnostic/template interfaces.

Rustmux samples the PTY foreground process-group leader on command output and
on event-loop ticks, including quiet commands and detached servers. macOS uses
`proc_name`; Linux uses `/proc/PID/comm`. Startup jobs that replace the original
shell with `exec` are eligible. Any observed matching application suppresses
that command's generated reminder, even if the shell is foreground at completion.
State belongs to the pane and is cleared on command completion, cancellation or
another semantic start. Restored workspaces start fresh processes and fresh
observations.

This is best-effort process observation: a short-lived process between samples,
an unobserved member of a pipeline, a wrapper, a platform-truncated name or an
unavailable process name can evade a filter. Only the observed group leader is
matched; Rustmux does not enumerate descendants. Unknown names alone do not
suppress a reminder. Each command retains at most 64 distinct names. If that
bound is exceeded while exclusions are configured, its generated reminder is
suppressed; an empty exclusion list continues to allow reminders.

## Review and verification

The base is reviewed directional-focus commit `9ecff78`, merged into
`main-human`. This increment implements `main`'s `enabled` and
`exclude_applications` notification settings while retaining the human track's
five-second threshold and terminal bell/activity behavior. It does not add
additional notification delivery backends. The terminal handshake and snapshot
formats are unchanged.

Reading order: notification parsing and diagnostics in `src/config.rs` and
`src/config/diagnostics.rs`, per-command observations in `src/semantic.rs`, PTY
process lookup in `src/pty.rs`, then completion filtering in `src/pane.rs` and
quiet-job sampling in `src/terminal.rs` and hidden undo-pane maintenance in
`src/closed_pane.rs`. Immutable exclusion names are shared
through `Arc` when policies are cloned for pane creation or restoration.

Unit tests cover default and explicit lists, normalization, deduplication,
invalid values, bounds, command isolation, repeated starts and cancellation.
`tests/terminal_loop_notification_filter.py` exercises actual foreground jobs,
misleading application titles, existing and newly created panes, quiet in-flight
jobs, adding/removing filters, invalid-update isolation, the enabled switch,
ordinary BEL markers, startup jobs that exec over their shell, unchanged child
PIDs and detached completion.

This branch awaits owner review. Linux CI and installed-client validation have
not been performed; the shared acceptance ledger is unchanged.

Local cumulative verification on macOS, 2026-10-01, using Rust 1.99.0:

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  946 passed, zero failed, six existing ignored tests, across 47 test targets.
- All 38 real PTY scenarios passed, including the new application-filter scenario
  and the existing completion-reminder, reload, close/undo and snapshot scenarios.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`
  passed.
- `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.
