# Line Editing

This H03 step implements insertion/deletion of rows and explicit scrolling.
Applications can update part of a text view while keeping headers and status
lines outside the scrolling region unchanged.

| Sequence | Operation |
| --- | --- |
| CSI n L (IL) | Insert blank lines at the cursor; move remaining rows downward |
| CSI n M (DL) | Delete lines at the cursor; move remaining rows upward |
| CSI n S (SU) | Scroll the whole scrolling region upward |
| CSI n T (SD) | Scroll the whole scrolling region downward |

Counts default to one when omitted or zero. Counts larger than the affected
height are clamped before multiplication, so work is bounded by the grid size.
Private forms, extra parameters, colon groups and numeric overflow are ignored.

## Cursor and boundaries

IL/DL affect only the cursor row through the bottom margin. Rows above the cursor
and outside the scrolling region remain unchanged. They move the cursor to the
start of its current row and cancel delayed wrap; outside the region they have
no effect. This follows [XTerm's line operations](https://github.com/ThomasDickey/xterm-snapshots/blob/master/util.c).

SU/SD affect the entire region even when the cursor is outside it. They retain
cursor coordinates and cancel delayed wrap. These CSI commands are distinct
from ESC M (reverse index), which scrolls only at the top margin.

The public Screen methods accept a count of zero as a no-op; protocol defaulting
belongs to the parser. All operations move complete cells, preserving wide
character pairs, combining suffixes and styles. New blank rows use the current
background, default foreground and no decorations. The current writing style
does not change, and text pushed out of the affected range is discarded.

## Verification and remaining work

Run `cargo test --test line_edit`. Fixtures cover every two-chunk split and
byte-by-byte input, exact rows, counts, malformed commands, cursor and wrap,
Unicode, alternate-screen isolation and one-cell grids. The real CLI PTY test
checks all four commands with a fixed header and footer.

[Character Editing](character-editing.md) adds ICH/DCH/ECH. Horizontal margins and terminal replies remain unsupported. Full Neovim compatibility is not yet claimed.
