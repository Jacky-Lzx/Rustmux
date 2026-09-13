# Application Keypad

ESC = (DECKPAM) requests application numeric-keypad encoding; ESC > (DECKPNM)
returns to numeric encoding. This mode is separate from application cursor keys.
For example, keypad 0, 1 and Enter can send ESC O p, ESC O q and ESC O M in
application mode, instead of digits and carriage return. See
[XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

The parser stores this global input mode in Screen, initially disabled. The renderer
synchronizes it on first use, changes or cache invalidation. The input loop forwards
resulting bytes unchanged; it does not infer keypad keys from ordinary digits.
Actual encoding, modifiers and NumLock overrides remain the outer terminal's
responsibility. Mode changes follow the existing frame schedule and output queue.

Cursor save/restore, alternate-screen switching and resize preserve keypad mode.
RIS and DECSTR disable it. Exit cleanup emits ESC >, including after handled
termination signals. Cleanup resets to numeric mode rather than capturing the
outer terminal's prior keypad setting. CSI ? 66 h/l aliases are not implemented.

## Verification

Run `cargo test --test application_keypad` for parsing at every input split,
independence from cursor-key and paste modes, reset/state preservation, ignored
escape sequences and renderer replay. The nested PTY suite models keypad
0, 1, 9, decimal and Enter in both modes, checks exact child input bytes, and
verifies normal-exit and SIGTERM cleanup while enabled. These are byte-path
tests, not physical keypad or NumLock configuration tests.
