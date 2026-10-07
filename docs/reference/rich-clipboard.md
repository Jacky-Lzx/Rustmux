# Rich Clipboard Transactions

Enable pane applications to query MIME types and read binary clipboard data with
this top-level option, before any TOML table header:

```toml
clipboard_read = true
```

The default is `false`, independently of `clipboard_write`. This applies to
foreground unnamed sessions and attached named sessions, including background
panes and inactive windows. `default-config`, `check-config` and the running
server's `show-config -s NAME` report the option. Live reload updates the policy.
The outer terminal must implement the [Kitty clipboard protocol](https://sw.kovidgoyal.net/kitty/clipboard/)
and remains responsible for permissions. Rustmux relays the request; it neither
reads the OS clipboard itself nor bypasses the terminal's permission prompt.

A protocol-level example lists available MIME types:

```sh
printf '\033]5522;type=read:id=example;Lg==\033\\'
```

The application must read the resulting OSC replies from its own terminal input.
Replies are protocol bytes, not screen output. `loc=primary`, `name`, `pw` and
other valid metadata are retained for the outer terminal to interpret. Read
requests use Base64 payloads; `Lg==` represents the MIME-list query `.`.

## Ownership and serialization

A request receives a new outer ID scoped to the current attachment and request.
The router restores the application's original ID in each reply, or removes the
outer ID if the application supplied none. Different panes may reuse the same
application ID. Missing IDs also work. A reply is never routed by current focus.

Ownership uses the pane's server-wide identity and process incarnation. Moving
a live pane within or across windows, resizing it or changing its screen does
not change ownership. Respawn keeps the scripting pane ID but changes the
incarnation; late data for the former process is discarded. Closing/hiding the
source also invalidates its lease. Requests are never part of session snapshots,
History, screen state or restored raw output.

Only one read or write is active per attachment. A competing request gets a local `EBUSY`
reply with its own original ID, including repeated requests from the same pane.
One completed read request can wait per pane until the runtime takes it; further
requests within that PTY read are rejected rather than growing a request queue.
The lease remains held until the terminal's final reply has entered the source
pane's input queue, so a slow source cannot be overwritten by another request.

## Read replies, limits and lifecycle

The router accepts `OK`, followed by validated `DATA` packets and `DONE`, or an
`ENOSYS`, `EPERM` or `EBUSY` error. Payloads and MIME values must be valid padded
Base64; decoded data chunks are at most 4096 bytes. Binary data, multiple MIME
types and arbitrarily many chunks can stream without an aggregate payload copy.
Unknown and stale outer IDs, malformed replies and invalid ordering are
consumed without reaching another child. Untagged replies are accepted only as
validated paste notifications to a pane that requested mode 5522 (below).
Ordinary keys, other control strings and bracketed pasted text retain their
normal input path. An isolated Escape is released after 50 ms.

Storage is bounded:

- Each framing buffer retains at most 8192 packet-body bytes; metadata is limited
  to 1024 bytes, and application IDs to 64 allowed ASCII characters.
- A decoded request's MIME list is at most 4096 bytes. Malformed or over-limit
  requests are discarded; no partial packet is sent to the outer terminal.
- A relay queue is limited to 256 KiB. It reserves space for a complete bounded
  frontend read and pauses frontend/pane reads when source delivery is blocked.
- Source and outer queues retain their existing 64 KiB limits. Complete packets
  must fit before enqueueing; a read that cannot start receives `EBUSY`.
- An unfinished lease expires after 30 seconds without a valid matching reply,
  returning `EBUSY`. Delayed replies remain tied to the expired ID and are dropped.
  A completed response waiting for source queue space does not acquire a second
  timeout that could append an error after `DONE`.

When reading is disabled, new attached requests receive `EPERM`. Disabling during
an active read drops undelivered outer responses and queues `EPERM` to its owner.
The cancellation remains queued through source backpressure. A disabled partial
request cannot resume after reading is enabled again. Invalid config retains the
last valid policy through the existing reload mechanism.

Detached/hidden reads receive `ENOSYS`. Detach or disconnect discards relay state
and attempts a bounded `EBUSY` cancellation to the old source. If its input queue
is already full, this disconnect cancellation is best effort. A new attachment
uses different outer IDs; old requests and partial packets are never replayed.
The terminal may still finish its permission prompt after a timeout/disconnect;
its eventual reply cannot enter a replacement request.

## Scope and review

Rich writes use `clipboard_write`, independently of `clipboard_read`, as described
below. [OSC 52 writes](pane-clipboard.md) use the same write policy. OSC 52 reads,
Kitty file transfer OSC 5113 and nested tmux wrappers remain unsupported.
Paste events additionally require the application to enable private mode 5522.

Base: reviewed XTGETTCAP commit `5318f20`. Fixed `main` reference:
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c`. The reference forwards OSC 5522 together
with OSC 5113 using a pane tag. This implementation additionally owns a single
lease, distinguishes process incarnations and applies bounded delivery.

Reading order: `src/rich_clipboard.rs`, the pane observer/incarnation in
`src/pane.rs`, attached/detached handling and input filtering in `src/terminal.rs`,
then config diagnostics and `tests/terminal_loop_rich_clipboard.py`.

Protocol tests cover fragmentation, binary payloads, absent/reused IDs, malformed
and nested packets, byte limits, reply ordering, keyboard/paste preservation,
permission changes, timeout, source disappearance, attachment renewal and slow
source delivery. The real nested-PTY test supplies a simulated outer terminal
and actual child processes. It verifies detached/default rejection, concurrent
panes, focus changes, cross-window source movement, a roughly 350 KiB transfer,
live disabling, detach/reconnect, respawn and an unnamed foreground session.
It does not certify a physical Kitty permission prompt or the OS clipboard.

## Rich writes

Enable MIME/binary writes using the existing top-level option:

```toml
clipboard_write = true
```

Its default remains `false`. No separate rich-write setting is necessary. Reads
can remain disabled. Attached disabled writes receive `EPERM`; detached/hidden
writes receive `ENOSYS`. Following data from a rejected start is discarded.
The outer terminal still decides permission and clipboard/primary support.

A write consists of `type=write`, followed by `type=wdata:mime=BASE64_MIME`
packets carrying Base64 payloads, optional `type=walias:mime=BASE64_TARGET`
packets carrying a Base64 space-separated alias list, then `type=wdata` without
MIME or payload. The final terminal reply has `type=write:status=DONE` or a
protocol error. For example, this requests copying plain text `hello`:

```sh
printf '\033]5522;type=write:id=copy\033\\'
printf '\033]5522;type=wdata:mime=dGV4dC9wbGFpbg==;aGVsbG8=\033\\'
printf '\033]5522;type=wdata\033\\'
```

The application must read the final reply from its own terminal input. Only a
matching outer reply acknowledges the write; enqueueing data is not success.
Original IDs are restored, including absent IDs. Focus changes and pane moves
cannot change the recipient. A read and a write contend for the same attachment
lease and receive `EBUSY` when occupied. A new start from the same source can
replace its own still-unrelayed write, returning `EBUSY` for the old write.
Continuation IDs may be omitted; if present they must match the initial ID.
A continuation from a different process cannot modify the active transaction.

### Validation and storage

Rustmux validates the entire write before emitting any of it to the outer
terminal. Decoded bytes are spooled in a private temporary file (Unix mode 0600,
automatically removed), never in session state, snapshots or History. Framing,
reply and source buffers retain the read transaction limits above. Additional
write limits are:

- At most 4096 decoded bytes per incoming data packet and 64 MiB per transaction.
- At most 128 distinct MIME entries and 128 distinct aliases; each name is at most
  512 printable non-space ASCII bytes. Alias lists decode to at most 4096 bytes.
- Base64 can be split at arbitrary byte boundaries between adjacent packets of
  the same MIME. Only the concatenated stream is padded, with padding at its end.
  Changing MIME or ending requires a complete valid stream.
- Returning to a previous MIME replaces its previous value. Aliases can precede
  their target; original alias order is retained and the last assignment wins.
  Empty writes and entries with empty data are valid.
- `name` and `pw`, when present, must decode to UTF-8. Malformed frames/metadata,
  invalid Base64 and incomplete streams abort locally with `EINVAL`. Aggregate
  payload, MIME-count and alias-count limits return `EFBIG`; temporary-file
  failures return `EIO`.

Once validated, canonical packets stream through the existing output queue.
Intermediate data chunks contain 4095 decoded bytes, so their Base64 has no
padding; only the final chunk of a MIME is padded. A blocked output queue keeps
one whole packet pending. Cancellation can append one bounded abort control
packet beyond the ordinary queue budget, so it follows all previously queued
data before a later transaction or explicit History copy. The temporary file
is released after the end packet is queued, or on error/cancellation. A collecting write, relaying write or write
awaiting the outer acknowledgment expires after 30 seconds without progress.
A final reply blocked on the source queue retains the lease without a timeout.

### Cancellation and clipboard ordering

Disabling writes, timeout, hiding/closing/respawning the source, detach and
attachment loss cancel its lease. Before relay this only removes private staging.
During relay, if the end packet has not been queued, cancellation appends an
invalid Base64 data packet that makes the outer terminal discard its staging.
It never sends an end packet to cancel, because an end packet commits data.
Explicit detach/session-manager switching flushes this abort before disconnecting.
The named client waits for the server acknowledgment while draining final output;
it accepts EOF from older servers and falls back to local detach after five seconds
if the peer/output consumer stalls. That fallback has transport-loss guarantees.
Unexpected transport loss cannot guarantee delivery of an abort; no remaining
write packets are replayed on a new attachment, whose IDs are fresh.

Once the end packet is queued, the outer terminal may commit, including after a
later permission prompt or disconnect. Rustmux cannot revoke that clipboard
change. Stale acknowledgments are nevertheless discarded and cannot enter a
new pane process or another attachment.

Pane OSC 52 writes are best effort and are dropped while a rich read/write lease
is active, avoiding clipboard changes mixed into a rich transaction. Explicit
History copying cancels the lease and queues its abort before the user's OSC 52
copy. Built-in copying remains available when `clipboard_write` is disabled.

Write review base: signed rich-read commit `5c6d323`. Implementation:
`src/rich_clipboard/write.rs`, the shared router/observer in
`src/rich_clipboard.rs`, runtime lifecycle in `src/terminal.rs`, detach handling
in `src/session/client.rs` and `src/session/frontend.rs`, then
`tests/terminal_loop_rich_clipboard_writes.py`. Unit tests cover fragmented
Base64, bounds (including exactly 64 MiB), private spool permissions, MIME
replacement, aliases, queue pressure, acknowledgments, read/write contention,
framing errors and lifecycle cancellation. The nested-PTY scenario uses real
child processes and a simulated outer terminal for background ownership, pane
movement, large transfers, malformed writes, early host errors, cancellation
under output backpressure, abort flushing on detach, live reload, reconnect and
unnamed sessions. Physical Kitty permission prompts and OS clipboard contents
remain outside this local validation.

Local write validation (Rust 1.99.0): 1,063 tests passed across 52 targets,
including all 49 nested-PTY scenarios; 6 pre-existing tests remained ignored.
All-target/all-feature Clippy with warnings denied, formatting, Python syntax,
`git diff --check` and mdBook build passed. After Clippy's equivalent condition
cleanup, all 12 terminal lifecycle unit tests and both rich clipboard PTY
scenarios passed again. This review branch has not been pushed; GitHub CI and
physical Kitty/OS clipboard behavior remain unverified.


## Paste events

With `clipboard_read = true`, an application can enable Kitty MIME paste
notifications with `CSI ? 5522 h` and disable them with `CSI ? 5522 l`.
`CSI ? 5522 $ p` reports 1 when requested, 2 when reset, or 4 when reading is
disabled by configuration. This reports Rustmux's configured capability; it does
not probe or certify support in the outer terminal. The host must implement
[Kitty paste notifications](https://sw.kovidgoyal.net/kitty/clipboard/).

The active, live pane controls the outer 5522 mode. Background panes retain their
own requests but do not select the host mode. Kitty gives this mode precedence
over bracketed paste (2004); Rustmux retains the child's 2004 request so normal
text pasting resumes when 5522 is reset. Window/pane control modes, History,
help, editing prompts and retained exited panes reset outer 5522 until ordinary
child input resumes. Detach and terminal cleanup also reset it. A live process's
request survives detach/reattach, resizing, alternate-screen changes and DECSTR;
RIS, respawn, hiding and disabling `clipboard_read` reset it. Re-enabling the
configuration requires a fresh application DECSET. Reset followed by set in the
same child output read still cancels older undelivered events. Modes and events are not
restored from saved sessions.

A physical paste produces three untagged `type=read` packets: `status=OK`,
`status=DATA:mime=Lg==` with a Base64 MIME listing, then `status=DONE`.
Optional `loc=primary` and Base64 UTF-8 `pw` metadata are preserved. A password
or location repeated on subsequent packets must match the initial value.
Rustmux validates and stages the complete listing before delivering any of it.
Ownership is fixed when the initial OSC prefix arrives; focus changes during
fragmented packets cannot redirect the event to a different process. Subsequent
content reads use the existing read lease and process identity, preserving
`loc`, `pw` and the `name` field. A typical application uses the granted password
and `name=UGFzdGUgZXZlbnQ=` (Base64 `Paste event`) to request selected MIME types;
the outer terminal remains responsible for its grant and permission checks.

Notification staging is limited to 16 KiB of wire data, 4096 decoded MIME-list
bytes, 128 names, 512 bytes per name and 512 decoded UTF-8 password bytes. Empty
listings and ASCII whitespace between names, including Kitty's trailing newline,
are accepted. DATA chunks must each contain complete valid Base64. Host packets
can use ST or BEL; child delivery is normalized to ST, while application requests
continue to require ST. Existing framing and metadata limits still apply.
Notifications do not occupy the read/write lease; a subsequent content request
can receive `EBUSY` if another transaction already owns that lease.

Missing starts, wrong MIME/password/location, malformed frames and over-limit
notifications are discarded in full. Incomplete events expire after 30 seconds
without valid progress. Source disappearance, respawn, hiding, config disabling,
local UI modes and attachment loss discard undelivered notifications. A completed
listing waiting for child queue space stays ordered and bounded, with no second
timeout. No partial notification or clipboard grant is replayed on a later
attachment. Already delivered protocol input cannot be revoked.

Paste review base: signed rich-write commit `06faadf`. Implementation starts at
`src/rich_clipboard/paste.rs`, then the attachment router, screen/parser mode,
renderer and terminal runtime. Unit tests cover every two-part wire split,
owner capture at the prefix, validation, limits, timeout, policy cancellation,
queue pressure and coexistence with an explicit read. The new real nested-PTY
scenario `tests/terminal_loop_rich_clipboard_paste.py` verifies default/detached
queries, background mode isolation, a fragmented event across a focus change,
paste-granted content reads, local UI suppression, live reload, reconnect,
respawn, unnamed sessions and terminal cleanup. The host is simulated; physical
Kitty paste gestures and OS clipboard contents remain unverified.

Local paste validation (Rust 1.99.0): 1,076 tests passed across 52 targets,
including all 50 nested-PTY scenarios; 6 pre-existing tests remained ignored.
The full run used the CI setting `--test-threads=4`. All-target/all-feature
Clippy with warnings denied, formatting, Python syntax, mdBook build and
`git diff --check` passed. An initial run with unrestricted test concurrency
passed the new paste and existing rich read/write scenarios but failed the
manager visibility assertion because concurrent session rows placed its helper
outside the viewport; the complete CI-concurrency rerun passed. This branch
has not been pushed; physical Kitty gestures and OS clipboard remain unverified.
