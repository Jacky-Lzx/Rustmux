# Rich Clipboard Reads

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

Only one read is active per attachment. A competing request gets a local `EBUSY`
reply with its own original ID, including repeated requests from the same pane.
One completed request can wait per pane until the runtime takes it; further
requests within that PTY read are rejected rather than growing a request queue.
The lease remains held until the terminal's final reply has entered the source
pane's input queue, so a slow source cannot be overwritten by another request.

## Replies, limits and lifecycle

The router accepts `OK`, followed by validated `DATA` packets and `DONE`, or an
`ENOSYS`, `EPERM` or `EBUSY` error. Payloads and MIME values must be valid padded
Base64; decoded data chunks are at most 4096 bytes. Binary data, multiple MIME
types and arbitrarily many chunks can stream without an aggregate payload copy.
Unknown, stale and missing outer IDs, unsolicited OSC 5522 packets, malformed
replies and invalid ordering are consumed without reaching another child.
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

This increment implements the read transaction only. Rich clipboard `type=write`
starts get `ENOSYS`; following write data is ignored. [OSC 52 writes](pane-clipboard.md)
keep their independent policy and behavior. OSC 52 reads, rich paste events,
DEC private paste-event mode 5522, Kitty file transfer OSC 5113 and nested tmux
wrappers remain unsupported. The mode query continues to report paste events as
unsupported; read transactions do not imply paste-event support.

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
