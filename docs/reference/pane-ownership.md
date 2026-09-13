# Pane Contents and Layout Ownership

`pane_set::PaneSet<T>` owns a `Layout` and exactly one `T` per pane ID. It is the
bridge between split geometry and per-window terminal contents. `T` need
not implement `Clone`; it can hold the existing `Pane` with its PTY, parser,
screen and I/O state. The CLI now owns `Windows<PaneSet<Pane>>`, creating one initial
pane per window and adding panes through interactive splitting.

## Access and focus

`layout()` exposes a shared reference for geometry, dimensions, focus and zoom.
There is no mutable layout accessor, so callers cannot create an orphan leaf or
remove a leaf without handling its contents. `get` and `get_mut` access any pane
by stable ID, including hidden panes. `active` and `active_mut` return the selected
content directly because the collection always contains at least one pane.

`iter` and `iter_mut` visit all owned contents in creation order, regardless of
zoom. This is useful for background PTY polling. Their order can differ
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
existing panes without mutating their content values. The concrete `PaneSet<Pane>::synchronize_sizes` operation below applies resulting
geometry to screens and PTYs. The CLI routes all pane output, input and lifecycle operations through the container. This collection is not a transaction
covering those external operations and does not itself start or terminate processes.

## Preparing and committing a real pane resize

`Pane::prepare_resize(rows, columns)` validates dimensions and prepares resized
screen grids without touching the live pane or PTY. The returned
`PreparedPaneResize` exclusively borrows the pane: output cannot be parsed into
it between preparation and commit, preventing an old screen snapshot from
replacing newer output. Dropping the preparation cancels it without changing
screen, parser, I/O metadata or PTY size. Same-size preparation avoids cloning
the screen; changed sizes temporarily retain both old and prepared grids.
Reported validation/allocation errors preserve the pane; standard cloning
allocation failure can still abort as with other Rust collections.

`commit()` resizes a live PTY first and then installs the prepared screen. A PTY
error leaves this pane's model and I/O metadata untouched. Known EOF or cached
child exit skips the ioctl and updates only the model. A successful commit clears
the synchronized-output hold and schedules redraw, including for same-size
notifications. Parser state and queued input/replies remain attached to the pane.
Callers must keep EOF/exit observations current, as the CLI does.

The CLI now prepares all window screens before issuing any resize ioctl, then
commits them sequentially. A preparation error changes no pane or PTY. A later
PTY error may follow earlier successful commits; the CLI returns the error,
restores the outer terminal and cleans up its children. It does not attempt to
undo PTY changes or SIGWINCH already observed by applications. This is per-pane
commit consistency, not an all-or-nothing operating-system transaction.

This API supplies the resize primitive used by `PaneSet<Pane>::synchronize_sizes`.
Layout edits and PTY sizing remain separate operations.

## Synchronizing a set of real panes

After a layout edit, `PaneSet<Pane>::synchronize_sizes()` computes every pane's
required terminal dimensions. Normally each pane uses its tiled rectangle. In
zoom, the active pane uses the full content area and hidden panes keep their tiled
sizes. Changing the zoomed target therefore shrinks the previous target and grows
the new one. Closing a pane expands the surviving subtree's screens and terminals.

The method checks the full content-area cell limit before touching panes, refreshes
child exit observations, and prepares all changed screens before committing any
PTY changes. Known exited/EOF panes receive model updates without ioctl. Panes
whose model dimensions already match are skipped, so a no-op sync or ordinary
focus change preserves their synchronized-output hold. The method assumes raw PTY
access has not independently changed sizes behind the screen models.

Call this after split, close, layout resize, or a focus/zoom change that affects
visible sizes, and before rendering or routing input with the new geometry. It
does not perform the layout edit itself or roll it back. If preparation fails,
no screen/PTY dimensions have changed; if a later commit fails, earlier commits
may already have resized their children. The caller must stop using the set and
clean up on such errors, rather than render an inconsistent frame. There is no
rollback of PTY changes or SIGWINCH received by child applications.

The CLI calls this after split and pane removal. Outer resize retains batch
preparation across every pane in every window, including same-size SIGWINCH
handling.

## Verification

`cargo test --test pane_sets` covers rejected splits without factory execution,
factory failure while zoomed, ID reuse prevention, content access while hidden,
geometry changes without content mutation, and non-Clone ownership transfer.
Drop-counting fixtures check that failed operations do not destroy live contents,
close defers cleanup to the caller, and collection destruction releases survivors.
These are ownership/model tests, not interactive multi-PTY acceptance tests.

`cargo test --test panes` also checks preparation cancellation, rejected sizes,
real child-observed dimensions after commit, pending parser input, same-size
hold release, PTY failure without model mutation, and model-only resize after EOF.
The full nested-PTY suite exercises the CLI's revised multi-window resize path.

The real-pane test also uses two shells to check child-observed sizes after split,
zoom, zoom-target change, outer resize, unzoom and close; stable PIDs, unchanged-size
hold preservation, content-area limit rejection and model resizing after exit are
covered. This verifies the collection API, not interactive CLI split shortcuts.

## CLI collection integration

The CLI constructs one-pane sets for startup and new windows. Keyboard queues are
accessed through the selected set's active content. PTY polling records both
`WindowId` and `PaneId`, since pane IDs are local to each set and often have the
same numeric value in different windows. Returned readiness events therefore
resolve the owning window first, then its pane, preserving per-child queues and
parser state.

Frames now pass through `pane_view::compose`, followed by top window-bar and prompt
composition. Closing a window releases its owned set; explicit close terminates
its owned direct children before removing it. Natural pane exit drains its final
output, removes that leaf and resizes surviving panes; the last pane closes the
window. Frames and synchronized-output scheduling include visible panes in the active
window. See [Interactive Splits](interactive-splits.md) for shortcuts and limits.
Ctrl-B `Z` toggles zoom of the selected pane.

The full existing nested-PTY suite is the regression check for this integration,
including window creation and selection, background replies, modes, resize,
rename/close prompts, final output, exit codes and direct-child reclamation.
