# Scrollback Storage

Each `Screen` owns bounded primary-screen history. `history_len()` reports its
retained physical rows, and `history_row(index)` returns a read-only cell slice
in oldest-to-newest order. Invalid indices return `None`. Rows preserve their
original width, spaces, colors, wide-character pairs and combining suffixes.
Each pane has its own screen, so history is isolated between panes and windows.

Full-height upward scrolling on the primary screen captures the departing rows.
This includes LF/IND, automatic wrapping and explicit CSI S. Scroll counts are
clamped to the visible height, so even a very large count records only actual
rows. Blank rows are retained too. Partial scrolling regions, alternate-screen
output, downward scrolling, line insertion/deletion and erasure do not add
history. Entering or leaving the alternate screen preserves primary history.

## Bounds and snapshots

At most 1,000 rows and 65,536 cells are retained per screen, whichever limit is
reached first. These bounds are separate from the visible-grid limit and currently
fixed rather than configurable. Appending evicts complete oldest rows until both
bounds fit. A row wider than the cell budget clears older history and is omitted;
the CLI's existing grid limit prevents that case in normal use. Cell limits are
not byte limits: cells also carry styles and bounded combining suffixes.

Screen clones share immutable history through reference-counted storage. Updating
one snapshot copies the deque metadata when necessary and shares its existing
rows; it never changes another snapshot. New captured rows copy their cells before
the grid scrolls. Standard allocation failure can abort, as with existing model
operations that allocate without a fallible return value.

## Resize and reset

Resize preserves existing history at its original row widths. Rejected resize
leaves history unchanged. Height shrink archives departing top rows when moving
the primary grid upward to keep its cursor visible; see [Screen Resize](screen-resize.md).
Right-edge and remaining bottom clipping are not recorded. Height growth consumes
the newest history rows and restores them above the old grid, preserving order.
Restored rows are clipped/padded to the new width; text is not reflowed. Physical rows now
carry [soft-wrap metadata](soft-wrap-metadata.md), but logical-line reflow is not yet implemented.

`clear_history()` discards history without changing grids, cursor or terminal
modes. RIS clears history with the screen reset; DECSTR and normal display erasure
preserve it. No history-clear escape sequence is added in this step.

Ctrl-B `[` opens the [history browser](history-view.md). Mouse-wheel navigation,
copy mode, persistence and resize/reflow remain subsequent steps.

`cargo test --test scrollback` checks exact Unicode/style preservation, LF and
wrap capture, explicit scrolling, editing exclusion, margins and alternate-screen
isolation, mixed-width history after resize, snapshot independence, reset behavior,
invalid resize, oldest-row eviction and both storage limits.
