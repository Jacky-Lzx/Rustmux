# Bracketed Paste

Applications enable bracketed paste with CSI ? 2004 h and disable it with
CSI ? 2004 l. When enabled, a supporting outer terminal wraps pasted text in
ESC [ 200 ~ and ESC [ 201 ~. These input markers let the application distinguish
pasted text from individual keystrokes. See the
[XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

## State and data flow

The mode starts disabled. The parser records the child's request in Screen;
each rendered frame synchronizes the outer terminal's mode. Synchronization
therefore follows the existing frame scheduling and output queue. The input loop
forwards markers and payload unchanged to the child PTY, including UTF-8 and
newlines. It does not generate markers or interpret the pasted content.

This is a global input mode for the current single pane. Cursor save/restore,
alternate-screen switches, resize and soft reset preserve it. RIS clears it.
Terminal cleanup disables it on normal exit and handled termination signals,
using the existing bounded restoration path. Arbitrary pre-existing outer
terminal modes are not captured.

## Verification

Run `cargo test --test bracketed_paste` for parsing across input boundaries,
malformed commands, state transitions and renderer mode synchronization.
The nested PTY suite enables the mode, injects split paste markers around
multiline Chinese text, and checks the child's exact received bytes. It also
checks disabling, re-enabling and cleanup after exit while enabled. This verifies
the byte path; it does not exercise a GUI clipboard or application paste policy.
