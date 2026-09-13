# Pane Contents and Layout Ownership

`pane_set::PaneSet<T>` owns a `Layout` and exactly one `T` per pane ID. It is the
bridge between split geometry and future per-window terminal contents. `T` need
not implement `Clone`; it can hold the existing `Pane` with its PTY, parser,
screen and I/O state. The CLI still uses one `Pane` per window and has not yet
adopted this collection. Interactive splitting remains unavailable.

## Access and focus

`layout()` exposes a shared reference for geometry, dimensions, focus and zoom.
There is no mutable layout accessor, so callers cannot create an orphan leaf or
remove a leaf without handling its contents. `get` and `get_mut` access any pane
by stable ID, including hidden panes. `active` and `active_mut` return the selected
content directly because the collection always contains at least one pane.

`iter` and `iter_mut` visit all owned contents in creation order, regardless of
zoom. This is useful for future background PTY polling. Their order can differ
from layout traversal order: rendering must look up contents using IDs from
`geometry()`, not zip the two sequences together.

## Creating and removing panes

`new(rows, columns, content)` owns one initial value. Invalid dimensions return
an error and drop that supplied value. To avoid starting a process for an invalid
initial size, validate the size before constructing process-backed contents.

`split_with(axis, factory)` clones only the layout, validates the proposed split,
and reserves collection capacity before invoking the factory. The factory receives
the new pane ID and its candidate tiled rectangle and returns `io::Result<T>`.
An invalid split never invokes it. A returned error leaves existing contents,
layout, active ID and zoom unchanged and does not consume an ID. A successful
factory result commits the new value and candidate layout together. External
factory side effects cannot be rolled back by the container; the factory must
clean up resources on failure. Standard allocation failure while cloning or
building geometry may abort, as with the layout model's existing allocations.

`close(id)` follows the layout's focus/removal rules and returns the removed `T`
without dropping it. Unknown or last-pane removal leaves both layout and contents
unchanged. The caller can drop the returned value for normal RAII cleanup or
transfer it elsewhere. Dropping the collection drops all remaining values once.

## Geometry versus process state

Selection and zoom delegate to the layout. `resize` changes only geometry; it
does not resize PTYs or screens. Likewise a split or close changes rectangles of
existing panes without mutating their content values. Before the CLI uses this
container, a higher layer must coordinate screen allocation, PTY sizing, rendering
and input routing for the resulting geometry. This collection is not a transaction
covering those external operations and does not itself start or terminate processes.

## Verification

`cargo test --test pane_sets` covers rejected splits without factory execution,
factory failure while zoomed, ID reuse prevention, content access while hidden,
geometry changes without content mutation, and non-Clone ownership transfer.
Drop-counting fixtures check that failed operations do not destroy live contents,
close defers cleanup to the caller, and collection destruction releases survivors.
These are ownership/model tests, not interactive multi-PTY acceptance tests.
