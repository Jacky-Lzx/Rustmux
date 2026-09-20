# Focus Reporting

CSI ? 1004 h enables focus reports; CSI ? 1004 l disables them. A supporting
outer terminal sends ESC [ I when it gains focus and ESC [ O when it loses
focus. See [XTerm focus events](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-FocusIn_FocusOut).

Screen stores the requested mode, initially disabled. It is global across
cursor saves and main/alternate grids. Resize and DECSTR preserve it; RIS clears
it. This is a reporting request, not a stored observation of actual window focus.
[Mouse reporting](mouse-reporting.md) uses independent modes.

## Runtime synchronization

The CLI uses Renderer to send the mode on the first frame and on changes only.
An ordinary redraw does not re-enable reporting: enabling may cause an outer
terminal to report its current focus. Frames retain the existing bounded queue
and scheduling. After a successful render the queued frame must be sent fully
before the next frame; failed rendering does not advance the mode cache.

Physical focus bytes are forwarded unchanged. When a keyboard command or mouse
click changes the active pane, Rustmux additionally queues ESC [ O for the old
pane and ESC [ I for the new pane when each pane requested focus reporting.
Window changes, splits, restores and focus fallback use the same transition path.
Bytes preceding a focus command remain queued for the old child and later bytes
target the new child. Already queued events may still arrive after a child
requests disabling. Output-side CSI I retains its existing forward-tabulation
meaning. The input and output paths are distinct.

Normal exit and handled signals disable reporting through terminal cleanup.
Pre-existing outer focus reporting settings are not captured.

## Verification

Run `cargo test --test focus_reporting`. Tests cover split parsing, reset and
mode preservation, standalone renderer replay, unchanged redraw suppression,
and failed-write retry. The nested PTY suite injects physical focus-in/out events,
checks exact child input, enable/disable output, ordinary redraws, pane transition
events, and normal-exit and SIGTERM cleanup. It does not automate GUI window
focus changes.
