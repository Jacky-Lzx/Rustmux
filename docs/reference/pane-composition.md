# Composing Pane Screens

`pane_view::compose(layout, screens)` builds a content-area `Screen` for the
existing renderer. This is a building block for interactive splitting; the CLI
still uses one pane per window and does not yet call this compositor.

Supply `(PaneId, &Screen)` pairs in any order. Every visible pane must have a
screen exactly matching its rectangle from `layout.geometry()`. Missing visible
screens, duplicate IDs, unknown IDs and mismatched dimensions return an error.
The composed content area is limited to 65,536 cells. Source screens are never
changed. Hidden panes may be omitted; if supplied, their dimensions are ignored.
After unzooming, every newly visible pane must again have its tiled dimensions.

The compositor copies complete cells, including styles, wide-character leaders
and continuations, and combining suffixes. Exact dimension matching avoids
clipping a wide glyph at a pane boundary. Separator cells use box-drawing lines
and junctions with the same Catppuccin Mocha inactive style as the window bar.

The active pane supplies cursor visibility, shape and supported terminal input
modes. Its local cursor coordinates are translated by the pane rectangle's origin.
Inactive cursor/mode state does not control the physical terminal. Logical pending
wrap is cleared only on the disposable composed view; the original pane is
untouched. As with the existing window-bar composition, this view is for rendering,
not a replacement parser model.

Coordinates exclude the top window bar. A future CLI path must compose this view
with that bar, route input to the active pane, translate mouse coordinates, and
honor per-pane synchronized-output holds. This function does not perform those
operations, resize PTYs/screens, allocate new processes, or flush terminal output.
The total physical frame including the bar must also obey the CLI cell limit.
Composition clones the active screen and copies visible cells each time; it adds
allocation/copy work even though the renderer can encode changes incrementally.

`cargo test --test pane_view` checks nested geometry, separator junctions including
one-cell segments, styled Unicode cells, active cursor/modes, source preservation,
zoom visibility, invalid input, and full/incremental renderer output replayed
through the parser into an outer screen.
