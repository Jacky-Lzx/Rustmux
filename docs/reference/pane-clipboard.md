# Child Clipboard Writes

Enable child-process copying with this top-level configuration option, before
any TOML table header:

```toml
clipboard_write = true
```

It defaults to `false`. Explicit copying from Rustmux's History view remains
available regardless of this option. A pane application can write the clipboard
using [XTerm OSC 52](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html):

```sh
printf '\033]52;c;aGVsbG8=\007' # Copy "hello".
```

The attached outer terminal must allow OSC 52 clipboard writes. Rustmux emits
requests; it does not access the operating-system clipboard directly or confirm
that the outer terminal accepted them. This works in foreground unnamed sessions
and attached named sessions, including background panes and windows.

[Kitty rich clipboard reads](rich-clipboard.md) use a separate `clipboard_read`
option. This page describes OSC 52 writes.

## Supported requests

Requests accept BEL or ST, including arbitrary byte fragmentation. The selection
field may be empty or contain up to 12 characters from `cpqs01234567`; it is
preserved for the outer terminal. The payload must be valid padded or unpadded
Base64. Rustmux decodes and re-encodes it as canonical padded Base64, emitting one
complete BEL-terminated OSC 52. An explicitly empty payload is a supported clear
request. Invalid data never becomes an implicit clipboard clear.

Decoded data is limited to 32 KiB, matching built-in History copying. The encoded
payload and complete observed command have separate corresponding caps. A
request that exceeds either cap is discarded as a whole; no truncated clipboard
is emitted. Controls, bad selectors, noncanonical codes, extra fields, malformed
Base64, cancellation and unfinished strings produce no request.

Clipboard reads (`?`) are unsupported and receive no response. Rustmux does not
forward them to the outer terminal. Kitty OSC 5522 reads use the separate [rich clipboard policy](rich-clipboard.md);
rich writes and file transfer OSC 5113 remain unsupported. Clipboard-like sequences inside other
OSC/DCS/APC/SOS/PM strings never trigger a write.

## Lifetime and delivery

The policy supports the existing configuration hot reload. Invalid config retains
the last valid policy. `check-config`, `show-config` and the default template expose
`clipboard_write`; a named server's policy comes from its own config rather than
the attaching client's configuration.

Capture must be enabled when a string begins and when it completes. Disabling
invalidates incomplete and pending requests; re-enabling cannot revive them.
A new attachment also invalidates any old capture, so a request cannot straddle
two clients. Detached sessions and the live pane hidden for close undo discard
copy requests. They create no backlog for a later attachment or undo.

The observer belongs to the pane, retaining framing through resize, pane moves
and main/alternate switches. Respawn starts a fresh observer. The display parser,
History snapshots, screen redraw and saved-history restoration perform no
clipboard writes. Clipboard controls are not screen state or persisted profiles;
the separate raw-output API can still expose their original bytes as Base64.

At most one completed request is pending per pane. Within one bounded PTY read,
the latest valid request replaces earlier requests. A request is delivered through
the existing 64 KiB terminal-output queue only if the entire packet fits. Otherwise
it is dropped, without blocking the child, retrying or growing a clipboard queue.
Once queued, ordinary partial writes preserve transport progress. Each pane has
its own observer; completion order determines writes to the shared outer clipboard.
The existing child-reply reservation and session wire format are unchanged.

## Review and verification

Base: reviewed OSC 21 commit `32690a3`. Fixed `main` reference:
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c`. Both tracks already emit OSC 52 for
built-in copying (`main` uses `src/app.rs::copy_to_clipboard`). Neither forwards
child OSC 52 at that baseline. This increment fills that terminal-multiplexer gap
with opt-in, bounded writes rather than unrestricted control forwarding.

Reading order: `src/clipboard.rs`, the pane-owned observer in `src/pane.rs`,
attached/detached delivery in `src/terminal.rs`, hidden-pane servicing in
`src/closed_pane.rs`, config/diagnostics, then `tests/terminal_loop_clipboard.py`.

Unit tests check every split and bytewise input, both terminators, binary/Unicode
payloads, selectors, empty writes, reads, malformed and nested controls, policy
changes during capture, limits and overflow recovery, latest-only pending state,
display-parser independence and typed/default config diagnostics. The real PTY
scenario checks both session paths, independent child PIDs, background copying,
policy reload, invalid config preservation, maximum/oversized payloads, detached
and hidden-pane omission, no replay, cross-attachment partial strings, alternate
buffers, resize and output flooding followed by a fresh successful request.

Local Rust 1.99.0 validation passed 1,026 tests across 51 targets, including
all 46 real PTY scenarios, with 6 pre-existing ignored tests. All-target/all-feature
Clippy with warnings denied, formatting, Python syntax, `git diff --check` and
mdBook build passed. The first full run encountered a shell `setpgid` error in
the existing pane-size fixture; its isolated rerun and the subsequent complete
run passed. That fixture was not modified.

The shared acceptance ledger is unchanged; this branch awaits owner review and
has not been pushed. GitHub CI and actual GUI clipboard contents remain unverified.
