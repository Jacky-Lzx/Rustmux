# Split Layout Model

`layout::Layout` is the first part of H06: a binary tree of pane rectangles.
It does not own PTYs, screen models or input queues. The
[PaneSet container](pane-ownership.md) connects its leaves to owned content values.
Neither is yet connected to
the CLI. Interactive windows still contain one shell each. This is not a claim
that interactive splitting or H06 acceptance is complete.

## Coordinates and splitting

Create a layout with nonzero `u16` rows and columns. Its initial pane fills the
content area. Coordinates are zero-based and exclude the top window bar; the
future CLI integration must add the bar offset when rendering or handling mouse
input. Geometry does not allocate screen cells or enforce the CLI's separate
65,536-cell limit.

`split_active(SplitAxis::Columns)` keeps the original pane on the left and places
a new active pane on the right. `SplitAxis::Rows` keeps the original above the
new pane. Each split reserves one full column or row as a separator. Each leaf
needs at least one cell in each dimension, so the active rectangle must span at
least three cells along the split axis. Pane IDs are stable within a layout and
independent of coordinates; `select(id)` changes focus without changing geometry.

`tiled_geometry()` returns panes in first-subtree/second-subtree traversal order plus
separator rectangles. Together they cover the content area exactly, with no
overlap. The layout contains at most 64 panes, bounding recursion and geometry
storage. Failed splits do not consume IDs or change focus or the tree.

## Resizing policy

Each split prefers equal halves after reserving its separator, with an odd extra
cell going to the second subtree. The partition is clamped to the minimum sizes
required by both subtrees. For example, a left subtree containing two side-by-side
panes needs at least three columns, while a single right pane needs only one;
a five-column parent therefore assigns three columns, a separator, and one column.

`minimum_size()` computes the full tree's requirements. A resize below that size
returns an error and leaves the entire layout unchanged. Valid resizing preserves
IDs, tree structure and active pane. Geometry is recomputed from the current size;
there are no stored user ratios or text reflow decisions here. The caller must
later decide how to handle outer terminals too small for an existing layout.

The model does not yet support manual separator adjustment, pane swapping,
or PTY/model resize coordination.
Those remain subsequent steps. Ordinary allocation failure may abort as with
standard Rust collection allocation; logical validation failures are atomic.

## Closing a pane

`close(id)` removes the pane and its parent's separator, promoting the sibling
subtree into the parent's position. The sibling keeps its structure and IDs,
and geometry is recalculated using the normal minimum-size rules. The outer
content dimensions remain unchanged. Minimum required dimensions can decrease,
allowing later resizing to a smaller terminal.

Closing an inactive pane preserves the active ID. Closing the active pane selects
the next surviving leaf in first-subtree/second-subtree traversal order, or the
previous leaf if it was last. The method returns the resulting active ID. Unknown
or already closed IDs return `NotFound`; removing the final pane returns
`InvalidInput`. Both leave the entire layout unchanged. The model remains nonempty;
the future window layer must handle closing a window when its last pane exits.

Closed IDs are never reused, and closing frees a slot under the 64-pane cap.
This operation changes geometry only: it does not close a PTY, discard input,
reap a process or resize screen models. Those remain the caller's responsibility
when interactive splitting is connected.

## Zooming the active pane

`toggle_zoom()` toggles a full-content-area view and returns the new zoom state;
`is_zoomed()` reads it. A single-pane layout stays unzoomed. Zoom does not replace
or resize the split tree: `geometry()` returns only the active pane, filling the
content area without separators, while `tiled_geometry()` still returns all
underlying pane rectangles. Unzooming restores those rectangles and stable IDs.

Selecting another pane while zoomed keeps zoom enabled and shows that pane instead.
Selection and structural operations use the full tree, including hidden panes.
Successful splits and closes exit zoom. Failed operations preserve both the tree
and zoom state. In particular, splitting checks the active pane's tiled dimensions,
not the larger zoomed rectangle. Closing a hidden pane preserves focus but also
returns to the full split view.

Resize keeps zoom enabled and recomputes both views for the new dimensions. It
still requires enough space for the underlying split tree, so unzooming can always
succeed. A too-small resize leaves dimensions and zoom state unchanged. The future
CLI must define its fallback for terminals smaller than this minimum.

This remains a geometry-only feature. There is no CLI zoom shortcut yet; PTY sizing,
rendering and input routing for visible versus hidden panes are not connected.

## Directional focus

`select_direction(Direction::Left | Right | Up | Down)` finds a target using the
underlying tiled rectangles and returns its ID. Left/right candidates must overlap
the active pane's row interval; up/down candidates must overlap its column interval.
Overlap must have positive length: diagonal panes and corner-only contact are
excluded. Candidates must lie wholly on the requested side.

Among candidates, prefer the smallest edge gap, then the largest perpendicular
overlap, then the nearest perpendicular center. Remaining ties use layout traversal
order, making the result deterministic. This policy depends on pane rectangles,
not the text cursor position. Because panes may have different sizes, moving in
one direction and back is not guaranteed to restore the original pane.

No candidate leaves the entire layout unchanged and returns `None`; there is no
edge wrapping. A successful move changes only the active ID. While zoomed, hidden
panes remain eligible using their tiled rectangles, and zoom stays enabled on the
new target. Resize and close automatically affect future selection through the
current geometry. `PaneSet::select_direction` delegates to this operation without
moving, cloning or recreating any owned content.

There is no CLI directional shortcut yet. This model operation supplies the focus
policy for later interactive splitting and is not H07 acceptance.

## Verification

`cargo test --test layout` checks exact nested geometry, complete nonoverlapping
coverage across many sizes, asymmetric subtree minima, stable IDs/focus, failed
operations, the pane cap and maximum coordinate dimensions. A unit test checks
unknown selection and ID exhaustion without mutation. Existing window and CLI
behavior is unchanged because this module has not been wired into the event loop.

Pane-close tests cover sibling-subtree promotion, all active/removed combinations
in a nested tree, full nonoverlapping coverage after removal and minimum-size
resize, stale IDs, final-pane rejection and capacity recovery without ID reuse.

Zoom tests check exact restoration, hidden-pane selection, resize and minimum-size
rejection, single-pane behavior, and successful/failed structural operations.

Directional tests cover unequal nested panes, distance/overlap preference, stable
ties, no-op boundaries, diagonal rejection, zoom, resize/close and content identity.
