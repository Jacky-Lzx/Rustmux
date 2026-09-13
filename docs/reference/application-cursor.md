# Application Cursor Keys

CSI ? 1 h enables application cursor keys (DECCKM); CSI ? 1 l returns to normal
cursor keys. This changes input encoding, not cursor movement on the screen.
For unmodified keys, XTerm uses these sequences:

| Key | Normal | Application |
| --- | --- | --- |
| Up / Down | ESC [ A / ESC [ B | ESC O A / ESC O B |
| Right / Left | ESC [ C / ESC [ D | ESC O C / ESC O D |
| Home / End | ESC [ H / ESC [ F | ESC O H / ESC O F |

See [XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).
The outer terminal owns actual key encoding, including modifiers and any local
keyboard configuration. Rustmux does not translate incoming CSI/SS3 sequences.
[Application numeric keypad modes](application-keypad.md) (ESC = / ESC >)
are tracked independently.

## State and synchronization

The parser records the mode in Screen, initially disabled. The renderer sends
its value on first use, changes or cache invalidation; the raw input loop forwards resulting bytes
to the child. Mode synchronization follows normal frame scheduling and queued
output, as with bracketed paste.

This is global single-pane input state. Cursor save/restore, alternate-screen
switches and resize preserve it. Both RIS and DECSTR disable it, and saved
cursor restoration does not bring it back. Exit cleanup returns the outer
terminal to normal cursor keys, including after handled termination signals.
Pre-existing outer input modes are not captured or restored from a snapshot.

## Verification

Run `cargo test --test application_cursor` for split sequences, mode isolation,
reset and saved-state behavior, malformed commands and renderer replay.
The nested PTY suite checks emitted mode requests and injects the corresponding
unmodified arrow, Home and End bytes. The child checks every byte, including a
split SS3 prefix. It also verifies soft reset returning to normal encoding and
cleanup after normal exit and SIGTERM while application mode is enabled.
The harness models key encoding; it does not drive a physical keyboard or claim
compatibility with every outer terminal's key configuration.
