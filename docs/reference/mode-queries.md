# Terminal Mode Queries

DECRQM asks for one ANSI mode with CSI Ps $ p, or one DEC private mode with
CSI ? Ps $ p. Rustmux replies with CSI Ps ; Pm $ y or CSI ? Ps ; Pm $ y,
respectively. Pm is 1 for enabled, 2 for disabled, or 0 for an unrecognized mode.
The protocol also defines permanent states 3 and 4; Rustmux does not use them.
See [XTerm DECRQM/DECRPM](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

## Reported modes

| Namespace | Number | Mode |
| --- | --- | --- |
| ANSI | 4 | Insert mode |
| DEC private | 1 | Application cursor keys |
| DEC private | 6 | Origin mode |
| DEC private | 7 | Automatic wrapping |
| DEC private | 25 | Cursor visibility |
| DEC private | 1004 | Focus reporting |
| DEC private | 1049 | Alternate screen |
| DEC private | 2004 | Bracketed paste |

All other numbers report 0, including unsupported DECSET aliases such as 66,
mouse modes and synchronized output mode 2026. Keypad ESC = / ESC > and cursor
shape commands remain supported separately; that does not imply support for
other mode-number aliases. ANSI and private numbers are distinct namespaces.
An omitted Ps is treated as 0, which is unrecognized.

## Parsing and delivery

Each request accepts one numeric parameter. Parameter lists, colon subparameters,
extra intermediates, unsupported prefixes and overflow are silently ignored.
C0 controls retain their existing behavior inside CSI; CAN/SUB cancel it.
Queries do not change the screen. Replies reflect state when the final p is
consumed, including intervening set/reset commands in the same output chunk.

The existing `advance_with_replies` callback sends responses to the child, not
the outer terminal. One response per query fits MAX_REPLY_BYTES even for the
largest usize mode number. The CLI reserves this capacity before reading PTY
output and queues replies alongside keyboard input, as described in
[Terminal Status Replies](status-replies.md). There is no new unbounded queue.

## Verification

Run `cargo test --test mode_queries` for both states of every supported mode,
namespace separation, unknown and maximum-size numbers, every split boundary,
reset/stream ordering, malformed queries, EOF and unchanged screen state.
The real PTY test checks mode changes and a burst of 10,000 pairs of mode and
DSR queries, with replies exceeding the 64 KiB queue while the child reads them.
