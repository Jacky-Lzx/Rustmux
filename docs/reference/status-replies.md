# Terminal Status Replies and Default Colors

Rustmux now answers the standard
[XTerm device status reports](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
from its screen model.

| Request | Reply |
| --- | --- |
| CSI 5 n | CSI 0 n (ready) |
| CSI 6 n | CSI row ; column R (cursor position) |
| CSI ? 6 n | CSI ? row ; column R (DECXCPR) |
| OSC 4 ; index ; value ... BEL/ST | Query or set one or more pane-local palette entries |
| OSC 104 ; index ... / OSC 104 BEL/ST | Reset selected / all palette entries |
| OSC 10 ; ? BEL/ST | OSC 10 ; current foreground BEL/ST |
| OSC 11 ; ? BEL/ST | OSC 11 ; current background BEL/ST |
| OSC 12 ; ? BEL/ST | OSC 12 ; current cursor color BEL/ST |
| OSC 10 ; color BEL/ST | Set this pane's default foreground |
| OSC 11 ; color BEL/ST | Set this pane's default background |
| OSC 12 ; color BEL/ST | Set this pane's cursor color |
| OSC 10 ; value ... BEL/ST | Apply successive values to foreground, background and cursor |
| OSC 110 / 111 / 112 BEL/ST | Reset this pane's foreground / background / cursor color |

Cursor coordinates are one-based. With DECOM enabled, the reported row is
relative to the scrolling region's top. Otherwise it is relative to the screen.
Replies use the active grid and the state at the instant the query completes,
including a query split across reads. A cursor at a filled right edge reports
the last column without triggering pending wrap.

Queries do not change cells, cursor, modes or style. Private DSR supports only
DECXCPR (`CSI ? 6 n`); other values, omitted or extra parameters, colon groups
and numeric overflow produce no reply. CSI-like bytes inside OSC/DCS payloads
are not interpreted.

OSC 10, 11 and 12 report Rustmux's current pane foreground, background and cursor
colors. Their initial values are Catppuccin Mocha text `rgb:cdcd/d6d6/f4f4`, base
`rgb:1e1e/1e1e/2e2e` and rosewater `rgb:f5f5/e0e0/dcdc`. Pane cells using SGR
39/49 or the initial default style are rendered with those same text colors. The
active pane's cursor color is synchronized to the outer terminal, so replies
match visible state rather than the attaching terminal's theme. Replies preserve
the query's BEL or ST terminator.

OSC 10/11/12 setters accept `#RRGGBB` and X-style `rgb:R/G/B`, with one to four hex
digits per component. The values are normalized to eight-bit RGB and remain local
to the pane across alternate-screen changes, resize, detach/attach and RIS. OSC
110/111/112 restore the initial Mocha defaults. Existing cells retain symbolic
default colors, so changing a text default recolors them on the next frame;
explicitly indexed or RGB-colored cells are unchanged.

Following XTerm's successive-parameter convention, OSC 10 accepts up to three
values targeting foreground, background and cursor; OSC 11 accepts background
and cursor; OSC 12 accepts cursor only. Any value can be `?`, and replies are
emitted in parameter order with the request's terminator. Empty, malformed or
extra values make the entire operation an atomic no-op, including suppressing
any query replies.

OSC 4 accepts decimal indices from 0 through 255 paired with a color or `?` value.
The initial table uses XTerm's conventional 16 ANSI colors, 6x6x6 color cube and
24 grayscale entries. Setters use the same RGB formats as OSC 10/11. OSC 104
resets each supplied index, or the entire table when no index is supplied. Palette
changes are pane-local and recolor existing indexed foreground, background and
underline colors on the next frame. OSC 4 operations execute in parameter order,
so a later query can observe an earlier setter for the same index. The whole
command is validated first: an odd, empty, malformed or overflowing field makes
all setters and query replies atomic no-ops. The bounded 64-byte OSC payload holds
at most 15 minimum-length pairs.

## Parser API and CLI delivery

`Parser::advance_with_replies` takes a synchronous callback receiving each reply
as bytes, in stream order. The callback must consume or copy them before returning.
The parser does not retain a reply queue. The original `advance` API still parses
for display only and discards replies, which is useful for frame replay.

The CLI appends replies to the same 64 KiB queue as keyboard input. It reserves
worst-case reply space before each PTY read, using `MAX_REPLY_BYTES` per input
byte. This accounts for a single final byte completing a previously buffered
palette request and emitting up to 15 replies. When capacity is unavailable, PTY
reads pause while queued writes drain.
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

Tertiary device attributes, other private DSR values, dynamic colors after the
text cursor, named colors
and general terminal capability queries remain unsupported.
[Mode queries](mode-queries.md) support the explicitly listed ANSI/private modes.

[Primary device attributes](device-attributes.md) provide a conservative DA1 reply.
