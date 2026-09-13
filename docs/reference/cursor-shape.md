# Cursor Shape

CSI Ps SP q (DECSCUSR) changes cursor shape and blink preference. SP is a literal
space before q; CSI Ps q without it is a different command and remains ignored.

| Ps | Shape |
| --- | --- |
| Omitted, 0, 1 | Blinking block |
| 2 | Steady block |
| 3 / 4 | Blinking / steady underline |
| 5 / 6 | Blinking / steady bar |

The values follow [XTerm's DECSCUSR definition](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).
Other values (including XTerm's resource-specific 7), private prefixes, multiple
parameters and extra intermediates are ignored. C0 controls retain their existing
behavior inside CSI; CAN/SUB cancel it. This adds the specific space-q command,
not general support for all CSI intermediates.

## State and rendering

Screen stores a typed CursorShape, initially BlinkingBlock. Changing it does not
move the cursor, alter text styles or pending wrap, or show a hidden cursor.
It is global across main/alternate grids and is not part of saved cursor state.
Resize preserves it; Rustmux resets it to BlinkingBlock on RIS and DECSTR.

The renderer emits the selected shape on first use, changes or cache invalidation
while the cursor is hidden. Each frame restores
its requested visibility at frame end. Blink timing and actual appearance are
handled by the outer terminal; Rustmux does not schedule blink frames.
Cleanup sends CSI 0 SP q on normal exit and handled signals. This is a baseline
reset; the outer terminal's pre-existing cursor shape is not captured.

## Verification

Run `cargo test --test cursor_shape` for all values and split boundaries,
malformed sequences, state preservation, reset, hidden cursor and renderer replay.
The nested PTY test handshakes through all six shapes, checks the emitted bytes,
checks soft reset, and verifies normal-exit and SIGTERM cleanup from a bar cursor.
It does not measure a GUI terminal's blink timing or visual shape.
