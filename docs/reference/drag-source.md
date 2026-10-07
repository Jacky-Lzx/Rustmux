# Kitty drag sources

```toml
drag_source = true
```

This Boolean defaults to `false`, independently of clipboard and file-transfer
permissions. Configuration diagnostics, `default-config` and live reload expose
it. The outer terminal must implement the [Kitty OSC 72 drag-and-drop protocol](https://sw.kovidgoyal.net/kitty/dnd-protocol/).

This increment implements the **source side**: dragging data from the focused
pane into an external application. External drops into Rustmux (`t=a`, `m`, `M`,
`r`, `R`) and an internal bridge between panes are not implemented. A successful
`t=q` reply confirms verified outer protocol support and usable cell-pixel
geometry for this source relay; it does not advertise the missing drop side.
Applications that require both directions may not work with this increment.

## Registration and routing

Each attachment probes the host when `drag_source` becomes enabled. Child reads
pause for at most one second during this probe so a startup support query can
precede its following device-attributes response. Replies must echo the probe's
ID. No environment variable or graphics capability is used as proof of OSC 72
support. Only a successful probe plus known nonzero cell dimensions allows a
child support reply or an outer source registration. Future optional host claims
are not forwarded. A new attachment or disable/re-enable cycle probes again.

A child registers with `t=o:x=1` and unregisters with `t=o:x=2`; an optional
machine-ID payload is preserved. One source registration is retained per live
pane process, up to 64 registrations per attachment. Background registrations
are remembered, while only the focused pane's source is advertised. Registrations
and gestures are not persisted in snapshots, and disabled/detached registrations
are discarded. After reattachment or re-enabling, the child must register again.

The relay replaces the child's optional integer `i` with a checked, process-wide
outer ID. It restores the child's original ID (or its omission) on replies.
Outer IDs are never reused within a running server. Identical child IDs in
separate panes are isolated, including after a process is respawned. Client IDs
compare numerically; omitted `i` and explicit `i=0` share the protocol default,
while replies preserve the registration's original ID spelling or omission.

The host's `t=o` gesture must lie inside the currently advertised, visible pane.
Cell coordinates subtract the pane's column and row, including the outer header
row. Pixel coordinates subtract that origin multiplied by the verified cell
pixel dimensions. Events on borders, outside the pane, or with invalid pixel
coordinates are discarded. Starting a gesture pins its originating process;
focus changes and moving it between windows do not redirect subsequent replies.

Only in Locked mode, without History, Help or a prompt, are source gestures
advertised. Entering another mode cancels a gesture and temporarily unregisters
the outer source. The remembered pane registration is restored when eligible.
The host still decides whether an actual physical gesture can start a drag.
Unsolicited child offers receive `EPERM`; disabled offers receive `EPERM`, and
detached or unsupported offers receive `ENOSYS`.

## Streaming and cleanup

After a valid gesture, the relay forwards the offer (`o`), pre-sent data (`p`),
image/start controls (`P`), data responses (`e`), errors/cancellation (`E`) and
remote URI data (`k`). Host status/data requests (`e`, `E`, `k`) return to that
same process, even while another pane is focused. Starting permission requires
the host's `t=E;OK`. Completion (`t=e:x=4`) and errors retire the gesture; the next
gesture receives a fresh outer ID. Unknown or retired replies never become
keyboard input.

Payloads are opaque. Rustmux does not open paths, follow links, fetch directories,
cache an entire drag or synthesize an internal drop. The child and outer terminal
validate binary data, MIME types, thumbnails, remote machine IDs and filesystem
access. Ordinary data packets support metadata-free chunk continuations; every
outgoing continuation receives the same isolated outer ID. Nested-client IDs may
be omitted after the first chunk. Chunked host status/error commands are rejected with
`EFBIG` in this increment; ordinary status/error payloads fit in one packet.

Every packet has at most 16 unique single-letter metadata keys, 512 bytes of
metadata, 4096 bytes of payload and an 8192-byte framing limit. Known integers
must fit the protocol's signed/unsigned 32-bit range. Duplicate fields, control
characters, invalid UTF-8, nested string controls and pasted literal commands
cannot initiate a drag. Child OSC commands require ST; host replies may use BEL
or ST. Clipboard, file transfer and drag replies share a single host framer and
its existing 50 ms Escape timeout.

The per-pane observer retains two frames' worth of bytes plus one partial frame.
The incoming/outgoing queues each have a 256 KiB cap. Polling stops at 128 KiB,
reserving room for a bounded frontend batch, ID restoration and cancellations.
Whole packets wait for room in the outer or child input queue; large transfers
stream without a total-data buffer.

Disable, detach, hide, close, respawn, mode changes and 30 seconds without valid
gesture activity cancel the gesture, discard undelivered data and unregister
its outer source. A surviving child receives `t=E;ECANCELED`. An overflowing or
invalid child frame revokes the registration and reports `EFBIG` to an active
source. Explicit child cancellation is forwarded before unregistering. Detach
flushes cancellations before its acknowledgment. Final foreground exit uses the
existing bounded 500 ms protocol cleanup window and reports stalled output.

## Review and verification

The base is reviewed `main-human` commit `f90ff3b`, including rebased MIME paste
notifications `5658702`. Both were fast-forwarded without changing their signed
commits. This increment remains on its own review branch.

Read `src/drag_source.rs` for parsing, registration, gesture ownership, queues
and cleanup; `src/terminal_ipc.rs` for shared framing; then the pane observer
and event loop in `src/pane.rs` and `src/terminal.rs`.

Unit tests cover framing splits, nested/pasted input, policy changes, capability
verification, IDs, coordinates, process isolation, chunk continuations, bounded
queues, expiry and shared clipboard-cancellation behavior. The real nested-PTY
fixture `tests/terminal_loop_drag_source.py` exercises independent child processes,
a 256 KiB binary artifact decoded from the simulated host capture, fragmentation,
focus/window moves, reload, detach, reattachment, respawn and foreground cleanup.
Physical Kitty-to-Finder drag gestures and actual GUI file promises remain
unverified. This is protocol/PTY evidence, not GUI acceptance.

Final local validation with Rust 1.99.0 passed 1,097 tests across 52 targets,
including all 52 real nested-PTY scenarios, with 6 existing ignores. Strict
all-target/all-feature Clippy, formatting, Python syntax, mdBook and staged diff
checks passed. The feature commit is local and awaits owner review; it has not
been merged or pushed.
