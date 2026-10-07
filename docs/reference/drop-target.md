# Kitty drop targets

```toml
drop_target = true
# Independently permit dragging files out:
drag_source = true
```

`drop_target` is a Boolean, defaults to `false`, and appears in `check-config`,
`show-config` and `default-config`. Live reload applies it to existing sessions.
The host must implement the [Kitty OSC 72 drag-and-drop protocol](https://sw.kovidgoyal.net/kitty/dnd-protocol/).
With an accepting child such as Yazi, files dragged from Finder can be copied
into the directory shown by the child. Yazi displays separate Copy and Move
areas during a hover; release over the intended area.

## Registration and ownership

Each attachment verifies host support with a fresh `t=q` probe. Child reads
pause for at most one second while probing, preserving startup query ordering.
Queries must echo the probe ID. Cell and pixel routing requires verified,
nonzero cell dimensions; environment variables do not establish support.
A child query receives an ordinary support reply when its enabled direction
has verified host support and geometry. Enabling both directions produces one
child reply; source and receiving probes use independent, never-reused outer IDs.

A child registers MIME types with `t=a` and unregisters with `t=A`.
One registration is retained per live pane process, up to 64 per attachment.
Optional machine-ID registration (`t=a:x=1`) is retained for that process even
when it precedes MIME registration. The visible active window's MIME lists
are combined for the host. Background registrations are remembered, but panes
hidden by zoom, closed panes and exited processes cannot receive a drop.

Hover `t=m` events select the pane under the pointer, independently of focus.
Both cell and pixel coordinates must lie inside its content rectangle; the
relay subtracts the pane origin, including Rustmux's header and pane title.
Crossing into another registered pane sends the old child a leave notification
and passes the host's cached MIME list to the new child. Borders and unregistered
panes reject the drop. Only Locked mode without History, Help or a prompt
advertises targets.

The mandatory `t=M` event pins the receiving process. Requests before this
physical drop, requests from another pane, and requests using the wrong nested
ID receive `EPERM`. Subsequent focus changes cannot redirect data. Hiding or
removing the owner cancels the drop. The child's optional integer `i` is replaced
with a fresh outer ID and restored on delivery, including its original spelling
or omission. Identical IDs in different panes and replacement processes remain
isolated. Omitted IDs and explicit zero share the protocol default. Retired or
unknown host replies are consumed without becoming keyboard input.

## Data and cleanup

Child accept/reject messages (`t=m:o=0/1/2`) reach the host only from the hovered
owner. Chunked MIME preferences are buffered within a bound and forwarded only
after completion. After `M`, child `t=r:x=<positive index>` requests are tracked
with their URI and directory-handle fields. Up to 64 requests may be pending.
Host `t=r` replies stream back to that owner, including metadata-free chunks,
opaque base64 data and an explicit empty end-of-data packet. `t=R` errors and
child `t=r:o=0/1/2` completion retire the drop and discard pending transfer data.
The next gesture uses a fresh outer registration ID. Error commands must fit one
packet; a chunked host error cancels with `EFBIG`, without leaving a child
waiting for an unfinished error string.

Rustmux does not open drop paths or fetch directories. The child performs the
filesystem operation; the host performs protocol file access. Machine IDs and
remote URI/directory requests are forwarded opaquely to the host for the selected
owner. Remote filesystem behavior has not been verified against a live remote
host. An internal source-to-target bridge between panes is not implemented.

Disable, detach, disconnect, mode changes, owner death, respawn and 30 seconds
without valid activity cancel active drops and discard undelivered packets.
A placed drop receives `t=R;ECANCELED`; a hover receives a leave notification.
The host receives rejection/completion and unregister commands. Detach flushes
cleanup before acknowledgment, and foreground exit uses the existing bounded
500 ms protocol cleanup window. Disabling or detaching clears child registrations;
surviving applications must register again after re-enabling or reattaching.
Registrations and gestures are not persisted in snapshots.

MIME registration and host MIME lists are ASCII and limited to 16 KiB; the
aggregate visible list has the same limit. Each frame has at most 16 unique
metadata keys, 512 bytes of metadata, 4096 bytes of payload and an 8192-byte
framing limit. The pane observer holds at most 24 KiB of complete commands plus
one partial frame. Malformed or overflowing child frames revoke that owner's
registration. Incoming and outgoing queues each have a 256 KiB cap; polling
pauses at 128 KiB to reserve room for bounded batches and cancellation. Large
payloads stream without an entire-transfer buffer. Whole packets wait for room
in the host or child input queue.

## Verification

The review branch is based on reviewed source-side fix `970c052`.
`src/drop_target.rs` implements bounded observation, registration, ownership,
chunking and cleanup. `src/terminal.rs` integrates routing with live pane geometry
and attachment policy; `src/pane.rs` observes raw child output.

Unit tests cover independent policies, verified capability, pointer routing,
leave/cache behavior, physical-drop gating, identical IDs, fragmentation,
machine registration, source/target coexistence, bounded queues and cleanup.
`tests/terminal_loop_drop_target.py` uses independent real pane processes and a
simulated host to verify reload, focus-independent delivery, respawn and detach,
and compares a decoded 256 KiB binary artifact with its source.

The opt-in `installed_yazi_receives_real_file_through_rustmux` compatibility test
launches the installed Yazi in a real PTY with the source permission both off
and on, sends a split URI list, waits for its
copy completion, and compares the resulting 128 KiB file byte for byte. This
verifies actual Yazi protocol and filesystem behavior; it does not automate a
physical Finder gesture. Existing sessions retain their original server binary:
use a fresh session when testing a rebuilt release.

Local Rust 1.99.0 validation passed 1,111 tests across 52 targets, including all
53 nested-PTY scenarios, with 8 optional ignores. The final 12 focused receiver
and configuration tests also verify that support queries do not echo unimplemented
optional claims. Formatting, strict all-target/all-feature Clippy, Python syntax,
mdBook and diff checks passed. Real installed-Yazi receive and source checks
passed with release binaries; physical Finder acceptance remains a separate
manual check because computer use cannot operate the Kitty window.
