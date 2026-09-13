# Meaningful Row Extents

`Screen::row_used_columns(row)` returns the end of meaningful content in a visible
physical row; `history_row_used_columns(index)` exposes the stored history value.
Both return `None` outside their range. The extent is a column count, including
wide-character continuation cells, and never exceeds the row's physical width.
It is metadata for primary reflow, not the cursor position or a trimmed string.

New default-background rows start at zero. Printing extends the length through
the written character, including explicitly printed spaces. Moving the cursor or
setting a tab stop does not extend it; printing after a cursor movement includes
the gap before the new character. Combining suffixes retain the base character's
width. A wide glyph that wraps early excludes the unused final-column padding
from the previous row's extent, even if the padding has a writing background.

## Editing and storage

Default-background erasure reaching the content end shortens the extent to the
erased boundary; erasing an interior range preserves the end. A nondefault writing
background makes erased cells meaningful so future layout must preserve their
appearance. Extents describe a contiguous prefix, not individual occupied cells.
They do not attempt to infer the application's intent after arbitrary editing.

Character insertion/deletion adjusts the extent with shifted content and clamps
it at the physical edge. Whole-row edits and scrolling move lengths with their
rows. Newly exposed rows have zero extent on the default background, or full width
when their blank cells carry a nondefault background. Alternate and primary grids
have independent lengths. RIS clears them; DECSTR retains them.

History captures original row lengths alongside cells and soft-wrap flags.
Same-width height shrink/growth transfers these values with the rows. Clipping
limits lengths to retained cells and removes clipped wide leaders; growth padding
with a nondefault background is meaningful. Immutable history snapshots retain
their original extents when live history is consumed or changed.

## Scope and tests

[Primary reflow](reflow.md) uses these extents with soft-wrap metadata to rebuild
logical lines without trimming explicit spaces or joining wide-character padding
into the text. Alternate-screen clipping retains its existing behavior.

`cargo test --test line_extents` covers spaces, cursor gaps, combining characters,
early wide wrapping, colored erasure, character/line editing, wide-cell clipping,
primary/alternate isolation, history restoration and snapshot independence.
Renderer replay tests compare cells, cursor and forwarded input modes instead of
internal extents: an encoded padding space looks identical but is received as an
explicit write by the outer terminal's parser. Model tests still compare full state.
