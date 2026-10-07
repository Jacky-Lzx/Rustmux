# Kitty file transfer

Enable the attachment-local OSC 5113 relay in `config.toml`:

```toml
file_transfer = true
```

The default is `false`, independently of `clipboard_read` and `clipboard_write`.
`default-config`, `check-config` and `show-config` expose the same Boolean
setting. It can be changed through live reload without restarting pane processes.
Disabled starts receive `EPERM`; detached starts receive `ENOSYS`. Requests are
never retained for a future attachment. Quiet level 2 suppresses local replies.

The outer terminal must support the [Kitty file transfer protocol](https://sw.kovidgoyal.net/kitty/file-transfer-protocol/).
Rustmux relays commands and responses; it does not open paths, write received
files, traverse directories, follow links, decompress data or apply rsync deltas.
These operations and permission approval belong to the child transfer tool and
the outer terminal. Enabling the relay does not certify host support.

## Wire routing

Use the actual serialized field names, including `ac`, `id`, `fid`, `n`, `d`,
`st`, `sz`, `pr`, `prm`, `mod`, `tt`, `zip`, `ft` and `q`. For example:

```sh
printf '\033]5113;ac=send;id=example\033\\'
```

A transfer's start receives an opaque, attachment-local outer ID. All subsequent
commands use that same outer ID; responses restore the original application ID.
Different panes can use identical IDs concurrently. Ownership follows the pane
and its process incarnation, preserving routing across focus changes, resizing,
pane movement and window changes. An old ID cannot address a respawned process
or a later attachment. Unknown and stale replies are consumed, rather than
becoming keyboard input in the focused pane.

Supported actions are `send`, `receive`, `file`, `data`, `end_data`, `status`,
`finish` and `cancel`. The relay preserves file IDs, path metadata, binary chunks,
compression and delta flags, and well-formed unknown extension fields. The host
still validates transfer ordering, authorization, individual files and content.
Data is streamed without buffering an entire transfer. Clipboard transactions
and file transfers have independent ownership and queues and can coexist.

Each command must carry a nonempty, safe session ID of at most 64 bytes. File
and parent IDs have the same size limit. Each packet has at most 32 unique keys,
with a 16 KiB framing limit and reserved room for ID substitution. Data chunks
are valid Base64 and decode to at most 4096 bytes; path, status and password
fields decode to UTF-8 with a 4096-byte limit. Integers and known enums are
validated. A receive start must specify `sz` between 1 and 1024. Unknown actions,
duplicate fields, malformed Base64 and over-limit frames are discarded.

Only top-level, seven-bit OSC commands are recognized. Child commands require
ST; host responses may use ST or BEL and are delivered with ST. Controls embedded
in DCS/APC/other OSC strings or bracketed pasted text cannot initiate transfers.
The display parser and saved-history replay continue to discard these controls.

The protocol's `pw` authorization is tied to the original session ID. This
relay cannot recreate that credential after isolating IDs, so a start with a
nonempty `pw` receives `ENOTSUP` and is not sent to the host. Use the host's normal
permission prompt for this increment.

## Lifetime and backpressure

One attachment permits up to eight live or finishing transfers. Another start
using an existing ID in the same process receives `EBUSY`; the original transfer
continues. Child observation queues at most two framed packets' worth of bytes,
plus one bounded partial frame.
The outgoing and incoming relay queues each have a 256 KiB limit, with reserved
capacity for one bounded input batch and cancellation controls. Polling pauses
at the reserve threshold and resumes as the queues drain. Packets enter the
outer queue and the child input queue only when they fit in full.

A child cancel discards staged data and undelivered replies, sends one complete
cancel command, and forwards only the matching cancellation acknowledgment.
A start canceled before entering the outer queue receives a local `CANCELED`.
Global host failures end their transfer; file-local statuses retain it.

Disabling the configuration or detaching cancels active transfers and discards
undelivered host replies. Host cancellations are queued before the detach or
Session Manager acknowledgment. Hiding, closing or respawning a source revokes
its ownership. Already emitted packets cannot be recalled. Valid activity
renews a 30-second idle deadline; expiry cancels an unfinished transfer and
reports `ETIMEDOUT` to its live source, subject to its quiet policy.

Kitty does not acknowledge a successful `finish`. A finishing ID remains reserved
for up to 30 seconds to route possible commit errors, then retires silently.
A process that exits immediately after writing `finish` retains its queued tail
through the final outer write. Normal final-pane exit flushes this tail, or
cancels an unfinished transfer, within a bounded 500 ms cleanup window; a stalled
output reports an error. Explicit shutdown cancels uncommitted work.

Transfers, queues, authorization and routing IDs are not saved in snapshots.

## Review and validation

This increment is based on rebased paste-events commit `5658702`, which includes
`main-human` CI repairs through `7353ca1`. The original paste commit `00cee69`
was rebased without conflicts; the rebased base passed all 1,076 tests across
52 targets, including 50 real nested-PTY scenarios, with 6 existing ignores.

Final validation with Rust 1.99.0 passed 1,087 tests across 52 targets, including
all 51 nested-PTY scenarios, with 6 existing ignores. Strict all-target,
all-feature Clippy, formatting, Python syntax, mdBook and diff checks passed.

Reading order: shared framing in `src/terminal_ipc.rs`, the observer and router
in `src/file_transfer.rs`, pane lifecycle in `src/pane.rs`, then forwarding and
exit handling in `src/terminal.rs`. The configuration is in `src/config.rs` and
its diagnostics module.

Unit tests cover fragmentation, validation, concurrent IDs, attachment/process
isolation, queue pressure, cancellation ordering, expiry, finishing cleanup,
quiet policy, credentials and resource limits. `tests/terminal_loop_file_transfer.py`
uses real processes and a simulated host to verify binary upload, a 256 KiB
received-file artifact, clipboard coexistence, movement, config reload, detach,
respawn and unnamed foreground cleanup. Physical Kitty prompts, real remote
filesystem operations and host compression/delta execution remain unverified.
