# Split Layout Model

`layout::Layout` is the first part of H06: a binary tree of pane rectangles.
It does not own PTYs, screen models or input queues and is not yet connected to
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

`geometry()` returns panes in first-subtree/second-subtree traversal order plus
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

The model does not yet support closing or zooming panes, directional selection,
manual separator adjustment, pane swapping, or PTY/model resize coordination.
Those remain subsequent steps. Ordinary allocation failure may abort as with
standard Rust collection allocation; logical validation failures are atomic.

## Verification

`cargo test --test layout` checks exact nested geometry, complete nonoverlapping
coverage across many sizes, asymmetric subtree minima, stable IDs/focus, failed
operations, the pane cap and maximum coordinate dimensions. A unit test checks
unknown selection and ID exhaustion without mutation. Existing window and CLI
behavior is unchanged because this module has not been wired into the event loop.
