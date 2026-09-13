# Soft Wrap Metadata

`Screen::row_continued(row)` reports whether a physical row continues its
predecessor through automatic wrapping. `history_row_continued(index)` exposes
the same flag for retained scrollback. Invalid indices return `None`.
This is metadata for future logical-line reflow, not reflow itself.

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
the edited row; display erasure clears visible flags. Printing in insert mode
preserves the incoming continuation established by automatic wrapping.
Cursor movement alone does not rewrite metadata. Flags describe physical-row
provenance rather than the application's intent after arbitrary cursor editing.

Width changes clear visible flags because current resize still clips/pads instead
of reflowing. Restoring history rows with differing widths also clears visible
flags. Existing retained history keeps its original widths and flags. This avoids
joining a clipped row to the next row based on obsolete geometry.

## Scope and validation

Visible output and shortcuts are unchanged. Width shrink can still lose right-edge
content. Full reflow also needs rules for meaningful trailing spaces, wide-glyph
padding, logical-line cursor mapping and bounded history growth; those are later
steps and must not be inferred from this flag alone.

`cargo test --test soft_wrap_metadata` checks delayed and wide-character wrapping,
explicit line feeds, disabled wrap, single-row scrolling, history and height round
trips, alternate-screen/reset isolation, edits, partial margins and insert mode.
Existing rendering, PTY and history tests remain regression checks.
