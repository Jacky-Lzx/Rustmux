# Scrolling Regions

This H03 step adds vertical margins to each screen grid. Applications can keep a
header or status line in place while scrolling the rows between them. The parser
implements the [XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
below; the renderer continues to draw the resulting full grid.

| Sequence | Effect |
| --- | --- |
| CSI top ; bottom r (DECSTBM) | Set inclusive one-based margins and home the cursor |
| ESC D (IND), LF | Move down, or scroll upward at the bottom margin |
| ESC M (RI) | Move up, or scroll downward at the top margin |
| ESC E (NEL) | IND followed by moving to column zero |

Omitted or zero margins mean row 1 and the last row. Reversed, out-of-range or
single-row regions are ignored without changing the cursor or pending wrap.
A one-row screen accepts its full-height region. Extra parameters and private
or colon forms of DECSTBM are ignored.

## Scrolling and movement

LF, IND and RI preserve the column and cancel pending wrap. Delayed automatic
wrapping also scrolls at the bottom margin. Scrolling moves whole rows, including
wide-cell pairs, combining suffixes and styles; exposed rows use the current
background with default foreground and no decorations. Rows outside the margins
remain unchanged. Outside the region, IND and RI move toward the physical screen
edge without scrolling.

CUU/CUD stop at the relevant margin when approaching it from inside the region.
Above the top margin CUU can reach row zero; below the bottom margin CUD can
reach the last row. CUP/HVP still use absolute screen coordinates. Origin mode
(DECOM) is not implemented. Setting margins homes to the screen's upper-left
corner without erasing text or changing the writing style.

## Alternate screen and resize

Margins belong to each grid and are independent of DECSC/DECRC cursor saves.
Entering mode 1049 uses a fresh full-height alternate region; leaving restores
the main region. Repeated mode entry is a no-op. Any actual dimension change
resets both regions to full height; same-size and failed resizes preserve them.

## Verification and limits

Run `cargo test --test scroll_region` for exact header/footer contents, both
scroll directions, wrapping, invalid input, chunk boundaries, Unicode cell
integrity, renderer replay, alternate isolation and resize behavior.
The real CLI PTY test also checks a fixed header/footer through LF and RI.

[Line Editing](line-editing.md) adds IL/DL and explicit SU/SD scrolling.
[Character Editing](character-editing.md) adds ICH/DCH/ECH; horizontal margins
and terminal replies remain future work. This step does not claim complete
Neovim compatibility.
