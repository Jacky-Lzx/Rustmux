# Screen Model Resize

`Screen::resize(rows, columns)` resizes the active and inactive grids together.
This is separate from `PtyShell::resize`, which changes the operating system PTY
size. The CLI now applies both operations when the outer terminal changes size.

## Policy

The primary grid keeps the cursor's row visible. If its zero-based row would be
outside the new height, resize moves the grid upward by `cursor_row + 1 - new_rows`
and appends the departing top rows to bounded [scrollback](scrollback.md), at their
original widths with full cell styles and combining suffixes. The cursor moves
up by the same amount. If it already fits, the grid keeps its top-left origin.

For example, shrinking a four-row primary screen to two rows with the cursor on
the last row retains rows 3–4 and archives rows 1–2. Ctrl-B `[` can browse them.
This applies to outer resize, splits and zoom/unzoom through the existing pane
resize path. While the alternate screen is active, the hidden main grid uses its
saved main cursor to make the same decision. The alternate grid itself retains
its top-left overlap and never contributes rows to history.

When height grows, the primary grid restores up to the added number of rows
from the newest end of scrollback. They appear at the top in their original
chronological order, and the old visible grid plus its current/saved cursors move
down together. Restored rows are removed from history, avoiding duplicates.
If history has fewer rows than the added height, the remaining bottom rows are
blank. The same policy applies to the hidden main grid while alternate is active;
the alternate grid never restores history.

Thus a same-width height shrink/grow cycle can recover archived rows while they
remain within the history limits. Previously frozen history views keep their own
snapshot and are unaffected by consumption of live history. Repeated same-size
notifications do not consume history.

There is no text reflow. Right-hand columns and any remaining bottom rows outside
the retained rectangle are still discarded. Restored rows use the new width:
short rows are padded; long rows are clipped, removing any cut wide-character
pair. The clipped part is not kept in history after the row is restored.
History that remains stored keeps its original widths and normal eviction limits. New cells use the relevant grid's writing
background with default foreground and no decorations. While alternate is active,
the saved main style supplies the main grid's background.

A wide character cut in half at the right edge is replaced by a blank, so no
orphan leader or continuation remains. Current and saved cursors translate with their own grid, then clamp to the new
bounds. A saved cursor in an archived row clamps to the top row. Any actual dimension change clears both
pending-wrap flags and resets both scrolling regions to full height. Resizing to the same dimensions changes nothing.

Alternate mode remains active across resize. Leaving it restores the resized main
grid and the clamped saved cursor/style. Re-entering still starts a blank alternate
grid at the new dimensions.

Display composition uses `resize_display`, which preserves top-left overlap and
never archives/restores rows or translates cursors with history. Adding a bar or
composing a larger pane canvas therefore cannot accidentally consume history or
move the displayed cursor.

## Failure and storage

Zero dimensions, size overflow and reported allocation failures return an error
without changing the model. Both destination grids are allocated before moving
any old cells. Retained cells, including suffix allocations, move into the new grids. Archived
rows copy their cells into destination history before moving the grid; old history
is shared with snapshots. History allocation can abort on exhaustion, like normal
scrollback capture. Old and new grids coexist briefly, so resize requires
more peak memory than steady-state storage. As with other allocations, process
termination by the OS under memory pressure cannot be recovered here.

## Verification

`tests/screen_resize.rs` checks growth, cursor-preserving shrink and archived rows, remaining clipping, style/suffix
preservation, clipped wide characters, alternate/main restoration, clamped
cursors, pending-wrap policy and unchanged state on invalid dimensions or a
capacity-limit error. Run `cargo test --test screen_resize`.

The nested-PTY suite checks unzooming a vertically split shell: recent output and
the prompt remain visible, archived top rows can be browsed, re-zoom restores them
to the visible grid, and input still works. Model tests additionally cover newest-row
ordering, partial restoration, saved cursors, hidden primary restoration, wide-cell
clipping, snapshot isolation and repeated cycles. A compositor regression checks
that enlarging the render canvas does not restore history or shift the cursor.
