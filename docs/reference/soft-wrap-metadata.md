# Soft Wrap Metadata

`Screen::row_continued(row)` reports whether a physical row continues its
predecessor through automatic wrapping. `history_row_continued(index)` exposes
the same flag for retained scrollback. Invalid indices return `None`.
This is metadata used by primary logical-line reflow, not reflow itself.

A flag becomes true when the next printable character actually wraps, including
a wide character that cannot fit at the right edge. Merely filling the final
cell and leaving delayed wrap pending does not mark the next row. Combining
suffixes do not wrap. Explicit LF/IND reaches a row with a false flag; disabled
autowrap does not create continuations. On a one-row terminal, the new visible row
can continue the last row just archived into history.

Both grids have independent flags. Alternate-screen entry clears its flags;
leaving restores the primary flags. RIS clears them, while DECSTR retains them.
Whole-row scrolling moves flags with cells. Full primary upward scrolling stores
flags alongside history rows, and height shrink/archive and growth/restore retain
them. History capacity eviction still removes complete rows and their flags.
The first retained row may have a true flag whose predecessor was evicted; a future
consumer must treat the start of available data as a boundary.

## Conservative invalidation

Partial scrolling and line insertion/deletion break links at changed boundaries.
Character insertion/deletion/erasure and line erasure sever links into and out of
the edited row. Display erasure severs links into and out of the erased rows,
while preserving links between untouched rows. In particular, clearing below a
prompt does not prevent earlier wrapped output from rejoining when widened. Printing in insert mode
preserves the incoming continuation established by automatic wrapping.
Cursor movement alone does not rewrite metadata. Flags describe physical-row
provenance rather than the application's intent after arbitrary cursor editing.

Primary width changes use [reflow](reflow.md), rebuilding continuation flags for
the new physical rows. Alternate clipping clears visible flags. Same-width height
changes retain flags with archived/restored rows.

## Scope and validation

The flag and [row extents](line-extents.md) are inputs to primary reflow. They do
not infer application intent after arbitrary cursor editing.

`cargo test --test soft_wrap_metadata` checks delayed and wide-character wrapping,
explicit line feeds, disabled wrap, single-row scrolling, history and height round
trips, alternate-screen/reset isolation, edits, partial margins and insert mode.
Existing rendering, PTY and history tests remain regression checks.
