# Terminal Status Replies

Rustmux now answers the standard
[XTerm device status reports](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
from its screen model.

| Request | Reply |
| --- | --- |
| CSI 5 n | CSI 0 n (ready) |
| CSI 6 n | CSI row ; column R (cursor position) |
| CSI ? 6 n | CSI ? row ; column R (DECXCPR) |
| OSC 10 ; ? BEL/ST | OSC 10 ; rgb:cdcd/d6d6/f4f4 BEL/ST |
| OSC 11 ; ? BEL/ST | OSC 11 ; rgb:1e1e/1e1e/2e2e BEL/ST |

Cursor coordinates are one-based. With DECOM enabled, the reported row is
relative to the scrolling region's top. Otherwise it is relative to the screen.
Replies use the active grid and the state at the instant the query completes,
including a query split across reads. A cursor at a filled right edge reports
the last column without triggering pending wrap.

Queries do not change cells, cursor, modes or style. Private DSR supports only
DECXCPR (`CSI ? 6 n`); other values, omitted or extra parameters, colon groups
and numeric overflow produce no reply. CSI-like bytes inside OSC/DCS payloads
are not interpreted.

OSC 10 and OSC 11 report Rustmux's default pane foreground and background,
respectively. Pane cells using SGR 39/49 or the initial default style are rendered
with those same Catppuccin Mocha colors, so the reported values match the visible
defaults rather than the attaching terminal's theme. Replies preserve the query's
BEL or ST terminator. Dynamic color setters and multi-color queries are ignored.

## Parser API and CLI delivery

`Parser::advance_with_replies` takes a synchronous callback receiving each reply
as bytes, in stream order. The callback must consume or copy them before returning.
The parser does not retain a reply queue. The original `advance` API still parses
for display only and discards replies, which is useful for frame replay.

The CLI appends replies to the same 64 KiB queue as keyboard input. It reserves
worst-case reply space before each PTY read, using `MAX_REPLY_BYTES` per input
byte. This accounts for a single final byte completing a previously buffered
query. When capacity is unavailable, PTY reads pause while queued writes drain.
Partial writes and retryable errors retain the existing queue behavior.

Replies are sent to the child, not to the outer terminal. Once the direct child
has exited, remaining output is parsed without queuing replies; final screen
draining must not wait for a process that can no longer consume them.

## Verification and limits

Run `cargo test --test status_replies`. Tests cover exact replies, ordering,
every input split, origin/alternate coordinates, unchanged screen state,
malformed queries, incomplete-stream handling and the reply-size bound.
The real CLI PTY test checks both queries and 20,000 status requests, producing
more reply bytes than fit in the queue while the child reads concurrently.

Tertiary device attributes, other private DSR values, dynamic OSC color setters,
palette/cursor color queries, and general terminal capability queries remain unsupported.
[Mode queries](mode-queries.md) support the explicitly listed ANSI/private modes.

[Primary device attributes](device-attributes.md) provide a conservative DA1 reply.
