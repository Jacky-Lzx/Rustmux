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
| `/` | Enter a search query |
| `n` / `N` | Next / previous match, wrapping at either end |
| `q` / Ctrl-C | Exit to the live screen |

Navigation stops at both ends. `G` stays in history mode; it does not resume the
live display. Window/pane shortcuts are unavailable until exit. Other input is
consumed locally, and bracketed paste is ignored, including navigation and exit
characters in its payload. Plain unbracketed paste cannot be distinguished from
keypresses. Escape sequences are consumed with a 64-byte bound; standalone Esc
is not an exit key in this version.

## Literal search

Press `/`, type a query, and press Enter. Search is case-sensitive literal text,
without regular expressions or Unicode normalization. Soft-wrapped rows are
joined using their meaningful extents, including across the history/screen
boundary. Hard line breaks stay separate; unused padding and wide-character
placeholders do not become searchable spaces. Explicit spaces and combining
suffixes are searchable. Overlapping matches are included.

The first result starts at or below the current viewport's top row; if none does,
search wraps to the earliest result. `n` advances toward newer text and `N` toward
older text, wrapping through the retained snapshot. The matched row moves to the
top where possible, clamped at the snapshot's bottom. The bar shows the current
result number and total count, or `no match`. Only the selected match is highlighted
by reversing its cell colors, including both columns of wide glyphs and entire
cells when matching a combining suffix. The original snapshot is unchanged.
A match taller than the pane is only partially visible; use ordinary navigation
to inspect the remainder. Navigation preserves the selected result.

The query editor accepts at most 128 UTF-8 bytes. Backspace deletes one Unicode
scalar and Ctrl-U clears the query. Long queries show their trailing characters
in the bar so the latest input stays visible. Ctrl-C or Ctrl-G cancels editing and retains
the previous search; it does not exit history. Enter with an empty query clears
search highlighting. While editing, `q`, `j`, `k`, `n`, and other printable keys
are query text. Arrow/page escape sequences and bracketed paste are ignored.
Use Ctrl-C to cancel rather than Esc. Searches and query edits never reach a shell.

Search runs only on submission, over the frozen snapshot. It temporarily creates
a text/cell index proportional to snapshot content and retains matching cell
ranges until the next query or exit. New shell output is not searched until a
new history snapshot is opened.

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
There is no snapshot text reflow, text selection, copying, mouse-wheel navigation
or disk persistence yet. This does not recover content previously discarded by
resize. A one-row outer terminal has no bar, so the history indicator is hidden.

Unit tests cover snapshot independence, navigation, wide-cell clipping, paste
isolation, logical-line and Unicode search, overlapping results, highlighting,
query editing and result wraparound. The nested-PTY suite checks browsing inside a split, the other pane's
continued visibility, new background output while frozen, snapshot-bottom versus
live output, search submission/result cycling/no-match/cancellation, navigation/paste
isolation and return to live input after resize.
