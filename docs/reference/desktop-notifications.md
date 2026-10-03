# Desktop command notifications

```toml
[notifications]
enabled = true
desktop = true
long_command_bell = false
command_duration_seconds = 5
exclude_applications = ["yazi", "nvim", "lazygit"]
```

`desktop` is a Boolean, defaulting to `false`. Enabling it sends OSC 99 desktop
notification requests to the attached terminal when a qualifying OSC 133 command
completes. `long_command_bell` independently controls terminal BEL and defaults
to `true`. Both options share `enabled`, the positive duration threshold and
[application exclusions](notification-filters.md). Setting `enabled = false`
or disabling both delivery options stops generated completion reminders.
An application's ordinary BEL activity marker is unaffected.

The displayed title is `rustmux: command finished`; the body identifies the
window name, runtime pane ID, application title and elapsed time. Text is bounded
to 80 non-control Unicode characters per supplied name/title, and encoded as
Base64. No captured command output or shell command line is included. The paired
title/body messages share a generated identifier; only the final body completes
the notification. Repeated qualifying records in one PTY read coalesce into one
reminder, matching the existing completion-bell behavior.

The wire format follows the [Kitty OSC 99 specification](https://sw.kovidgoyal.net/kitty/desktop-notifications/).
It requests no activation/close reports or focus action, so clicks do not inject
input into a pane or select a Rustmux window. Notification Center/banner display
and desktop sounds depend on the outer terminal and OS settings; turning off
terminal BEL does not control OS notification sounds. This increment emits
requests without capability discovery. Enable it with an outer terminal that
supports OSC 99; unsupported terminals may ignore the requests. Graphics support,
cell dimensions and environment variables are not used as notification capability
signals. This does not implement arbitrary child OSC 99 forwarding, buttons,
progress updates or click routing.

## Reload and session lifetime

Successful configuration reloads apply to existing and future panes, including
a command already running. Its latest policy is captured at completion; invalid
updates retain the entire last valid configuration. `show-config -s NAME` and
configuration diagnostics expose `[settings].desktop_notifications`. The default
configuration template includes `desktop = false`.

Both foreground and named sessions can deliver to an attached terminal. Commands
completed while detached retain their ordinary activity marker but discard
pending delivery; reconnecting does not replay desktop messages or completion
BEL. The hidden pane retained for interactive close undo also discards delivery
while hidden. Future commands after reattachment or undo remain eligible.
Restoration starts fresh processes and observations. The existing session handshake
and snapshot formats are unchanged.

Desktop delivery uses the normal terminal output queue. A message is dropped if
it would exceed the existing queue limit; the activity marker remains. There is
no retry or desktop-notification backlog. Notification IDs use a process/attachment
time prefix and checked serial; serial exhaustion suppresses further messages.

## Review and verification

The base is the signed merge of reviewed notification filters `05ed354` and
CI fix `4ffdd29`. Both original commits remain in `main-human` history.
`main` at `57d5986` already emits OSC 99 completion notifications. This human
increment makes desktop delivery opt-in, separates terminal BEL from desktop
requests, bounds text/output and discards detached delivery.

Reading order: `desktop` parsing and diagnostics in `src/config.rs` and
`src/config/diagnostics.rs`, completion-policy capture in `src/pane.rs`, the
encoder in `src/notification.rs`, then attached/detached routing in
`src/terminal.rs` and hidden-pane maintenance in `src/closed_pane.rs`.

Unit tests cover independent delivery settings, validation, bounded Unicode and
control-character input, framing and identifier exhaustion. The real PTY fixture
`tests/terminal_loop_desktop_notifications.py` decodes the actual outer OSC 99
stream and checks title/body identity, Unicode pane titles, desktop-only and
combined delivery, short commands, in-flight enabling/disabling, exclusions,
invalid-update isolation, unchanged child PIDs, detached activity and no replay.

The branch awaits owner review. Linux CI and real desktop banner/Notification
Center display have not been verified for this revision. The shared acceptance
ledger is unchanged.

Local cumulative verification on macOS, 2026-10-03, using Rust 1.99.0:

- `cargo +1.99.0 test --all-targets --locked --offline -- --test-threads=4`:
  950 passed, zero failed, six existing ignored tests, across 47 targets.
- All 39 real PTY scenarios passed, including desktop reminders, notification
  filters and the repaired window-close readiness check.
- `cargo +1.99.0 clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo +1.99.0 fmt --all --check`, `git diff --check` and `mdbook build` passed.
