# Browsing History

Ctrl-B `[` opens a read-only snapshot of the active pane's retained primary
history and current screen. It initially moves up one pane-height, clamped to
available history. Empty history and alternate-screen applications ignore entry.
Other panes continue displaying live output. The window bar shows `History`, the
number of rows above the snapshot's bottom, and the snapshot history length.
The cursor is hidden and mouse reporting is disabled while browsing.

| Key in history mode | Action |
| --- | --- |
| `k` / `j`, Up / Down | One row older / newer |
| Ctrl-U / Ctrl-D | Half a pane older / newer |
| Page Up / Page Down | One pane older / newer |
| `g` / `G` | Oldest retained row / bottom of the snapshot |
| `q` / Ctrl-C | Exit to the live screen |

Navigation stops at both ends. `G` stays in history mode; it does not resume the
live display. Window/pane shortcuts are unavailable until exit. Other input is
consumed locally, and bracketed paste is ignored, including navigation and exit
characters in its payload. Plain unbracketed paste cannot be distinguished from
keypresses. Escape sequences are consumed with a 64-byte bound; standalone Esc
is not an exit key in this version.

## Live processes and lifecycle

Shells keep running, parsing output and answering terminal queries while the
snapshot is open. Newly arriving output does not move or update the snapshot;
leaving history restores the latest live screen and its terminal input modes.
The snapshot shares immutable history rows, so eviction from the live screen
does not change the snapshot. It can retain up to one additional history budget
until exit. The snapshot also owns copies of its screen grids.

A nonzero outer resize exits history before resizing panes. Removal of any pane
in the current window also exits history because it changes layout; the active
pane's EOF restores its live final frame before removal. Automatic exits discard
already staged navigation input rather than sending it to a shell. Global termination uses
normal terminal cleanup. Previously queued output frames always finish first.

## Current limits

Rows retain their original widths: shorter rows are padded and longer rows are
clipped to the pane width, with clipped wide characters replaced by blank cells.
There is no text reflow, text selection, copying, search, mouse-wheel navigation
or disk persistence yet. This does not recover content previously discarded by
resize. A one-row outer terminal has no bar, so the history indicator is hidden.

Unit tests cover snapshot independence, navigation, wide-cell clipping and paste
isolation. The nested-PTY suite checks browsing inside a split, the other pane's
continued visibility, new background output while frozen, snapshot-bottom versus
live output, navigation/paste isolation and return to live input after resize.
