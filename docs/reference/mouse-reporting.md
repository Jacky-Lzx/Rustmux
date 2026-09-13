# Mouse Reporting

The single pane can request these DEC private modes with CSI ? Ps h/l:

| Ps | Behavior |
| --- | --- |
| 1000 | Button press/release and wheel events |
| 1002 | Button events plus motion while a button is held |
| 1003 | Button events plus all pointer motion |
| 1006 | SGR encoding, independently of the tracking mode |

Tracking modes are mutually exclusive: the last enabled mode wins. Resetting any
supported tracking mode turns tracking off. Resetting 1006 changes only encoding.
All start disabled. The model preserves them across cursor saves, grid switches,
resize and soft reset; RIS clears them. DECRQM reports the selected tracking
mode and SGR encoding state. Focus reporting remains independent.

## Event path

Renderer synchronizes mouse settings on the first frame and subsequent changes,
clearing old tracking and selecting encoding before enabling the new mode.
Ordinary redraws do not toggle tracking. The existing exit cleanup disables all
supported tracking modes and SGR encoding on normal exit and handled signals.

The outer terminal generates events. The input loop forwards bytes unchanged;
there is no coordinate parsing, filtering or translation because the pane fills
the outer terminal. SGR uses CSI < button ; column ; row M for presses/motion,
and final m for release; coordinates are one-based. Without SGR, the legacy
CSI M format and its coordinate limitations apply. See
[XTerm mouse tracking](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-Mouse-Tracking).

This does not add pane hit testing, mouse-based selection, alternate scrolling,
X10 mode 9, highlight tracking, UTF-8/urxvt encoding or pixel coordinates. Arbitrary
pre-existing outer modes are not captured. Already queued input is not discarded
when an application disables reporting.

## Verification

Run `cargo test --test mouse_reporting`. Tests cover exclusive transitions,
independent encoding, split parsing, malformed commands, resets, resize, mode
queries, renderer replay and unchanged-frame suppression. The nested PTY suite
injects SGR press/release, drag, motion and wheel events, then legacy button
reports; the child checks exact bytes. Normal-exit and SIGTERM cleanup are also
checked. Physical mouse handling by a GUI terminal is not automated by this test.
