# Composing Pane Screens

`pane_view::compose(layout, screens)` builds a content-area `Screen` for the
existing renderer. The CLI calls it for every visible pane in the active window,
then composes the top window bar and optional prompt.

Supply `(PaneId, &Screen)` pairs in any order. Every visible pane must have a
screen exactly matching its rectangle from `layout.content_geometry()`. Missing visible
screens, duplicate IDs, unknown IDs and mismatched dimensions return an error.
The composed content area is limited to 65,536 cells. Source screens are never
changed. Hidden panes may be omitted; if supplied, their dimensions are ignored.
After unzooming, every newly visible pane must again have its tiled dimensions.

The compositor copies complete cells, including styles, wide-character leaders
and continuations, and combining suffixes. Exact dimension matching avoids
clipping a wide glyph at a pane boundary. Pane frames use box-drawing lines with
the same Catppuccin Mocha inactive style as the window bar.

The active pane supplies cursor visibility, shape and supported terminal input
modes. Its local cursor coordinates are translated by the pane rectangle's origin.
Inactive cursor/mode state does not control the physical terminal. Logical pending
wrap is cleared only on the disposable composed view; the original pane is
untouched. Each pane's complete box frame is drawn independently; the two cells
between adjacent contents remain separate borders and can carry different focus
styles. As with the existing window-bar composition, this view is for rendering,
not a replacement parser model.

Coordinates exclude the top window bar. The CLI adds that bar and routes input
to its active pane, translating both mouse coordinates. Left-clicking any visible
pane frame or content selects that pane; the selection click is consumed locally.
Normal composite updates
wait for every visible pane to release its synchronized-output hold, subject to the
existing timeout. Explicit focus/layout changes force a redraw. These policies
belong to the event loop; this function does not resize PTYs/screens, allocate
processes, or flush terminal output.
The total physical frame including the bar must also obey the CLI cell limit.
Composition clones the active screen and copies visible cells each time; it adds
allocation/copy work even though the renderer can encode changes incrementally.

`cargo test --test pane_view` checks nested geometry, adjacent independent borders,
styled Unicode cells, active cursor/modes, source preservation,
zoom visibility, invalid input, and full/incremental renderer output replayed
through the parser into an outer screen.
