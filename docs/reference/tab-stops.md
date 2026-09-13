# Tab Stops

The screen keeps a global set of horizontal tab stops, initially at zero-based
columns 8, 16, 24 and so on. The protocol follows the
[XTerm control sequence reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

| Sequence | Effect |
| --- | --- |
| HT (Tab) | Move to the next stop |
| ESC H (HTS) | Set a stop at the current column |
| CSI 0 g or CSI g (TBC) | Clear the stop at the current column |
| CSI 3 g (TBC) | Clear all stops |
| CSI n I (CHT) | Move forward n stops |
| CSI n Z (CBT) | Move backward n stops |

Forward/backward counts default to one when omitted or zero. If there are fewer
stops than requested, movement ends at the corresponding screen edge. A stop at
the current column is not counted. Searching is bounded by screen width even
for very large counts.

Movement retains the row, does not erase or wrap, and cancels delayed wrap.
Setting/clearing stops does not move the cursor or change its edge state.
Stops can be placed on either half of a wide glyph without changing the glyph.
Other TBC modes, private variants, extra parameters, colon groups and numeric
overflow are ignored.

## State and resize

Stops are shared across both grids and are not saved/restored with the cursor.
Resize preserves stops in retained columns. Shrinking discards settings beyond
the new width; growing initializes new columns to the default eight-column
spacing, so discarded custom stops do not return. This also means growing after
clearing all stops introduces default stops in newly added columns. Same-size
and failed resizes leave settings unchanged.

Storage is proportional to the current width and is allocated before resize
mutates existing state. The model's zero-count forward/backward methods are
no-ops; protocol defaulting belongs to the parser.

## Verification

Run `cargo test --test tab_stops`. Tests repeat byte streams at every split and
byte by byte, checking custom/default stops, clearing, boundary counts, invalid
commands, wide cells, origin mode, alternate sharing, resize and one-cell grids.
The real CLI PTY test checks custom alignment and backward tabulation.

Tab-stop reset extensions, horizontal margins and terminal queries remain
outside this subset.
