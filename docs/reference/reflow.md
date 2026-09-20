# Primary Screen Reflow

Changing the number of columns reflows the primary screen and its retained
scrollback together. Rows marked as [soft continuations](soft-wrap-metadata.md)
are joined into logical lines using their [meaningful extents](line-extents.md).
Hard line breaks remain separate. Explicit spaces, styles, wide-character pairs
and combining suffixes are preserved; unused right-edge and wide-wrap padding
are excluded from the logical text.

Each logical line is then packed at the new width. A wide glyph moves to the next
row when only one column remains. At width one, wide glyphs become the replacement
character, retaining style and combining suffixes; that substitution cannot be
reversed on later growth. Rebuilt physical rows get new extents and continuation
flags. Rows emitted only to position a cursor remain part of their logical line.

Current and saved primary cursor positions map through their logical-line offsets.
A pending wrap at the new right edge remains pending, so the next printable
character does not overwrite the last glyph. When the mapped insertion point is
inside a wider row, pending wrap clears. A non-pending cursor at an exact boundary
moves to the following row. Blank columns needed to locate either cursor are
included in the logical line.

## Viewport and limits

Trailing unused screen rows below all content and both cursor positions are not
reflowed. The viewport normally shows the last screen-height rows, with earlier
rows entering history. If that would hide the current cursor, the viewport moves
up to keep it visible. Content below that viewport is discarded in this case;
there is no separate buffer for content below the screen. Saved cursors outside
the final viewport clamp vertically, as in other resize operations.

History still observes the configured physical-row limit and the fixed 65,536-cell
limit. Reflow can
increase the row count and evict older data; evicted text cannot return after
widening. Sparse packed rows avoid allocating a full-width grid for every
intermediate row. Only retained history rows and the visible grid are padded.
Temporary storage scales with the existing history/grid contents and row count,
not the product of old row count and new width. New padding uses the primary
writing background. Allocation exhaustion can abort as with existing history
capture; reported invalid dimensions leave the original model unchanged.

The hidden primary grid also reflows while an application uses the alternate
screen. The alternate screen keeps its existing top-left clipping policy and does
not write history. Same-width height-only changes continue to archive/restore
physical rows. Display composition never invokes reflow. Existing frozen history
snapshots keep their original content; an outer resize exits interactive browsing.

This applies to outer terminal resizing, splits, pane removal and zoom/unzoom
through the shared `Screen::resize` path. No new shortcut is required.

## Verification and remaining scope

`cargo test --test reflow` checks repeated width round trips, styled Unicode and
spaces, hard breaks, pending/saved cursor mapping, alternate/primary isolation,
width-one replacement, bounded history expansion and rejected resize.
The nested-PTY suite changes actual terminal width through 32, 53 and 80 columns,
checks an entire long output line, then verifies continued shell input and cleanup.

Conservative metadata invalidation after arbitrary line editing can split a former
logical line. Reflow cannot recover text already lost under earlier clipping,
history eviction or a width-one substitution. This is not full terminal-emulator
compatibility or a guarantee to reconstruct an application's intended paragraphs.
