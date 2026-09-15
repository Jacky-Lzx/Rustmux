# Browsing History

Ctrl-B `[` opens a read-only snapshot of the active pane's retained primary
history and current screen. It initially moves up one pane-height, clamped to
available history. Empty history and alternate-screen applications ignore entry.
Other panes continue displaying live output. The window bar shows `History`, the
number of rows above the snapshot's bottom, and the snapshot history length.
The cursor is hidden while browsing and shown in the bar while editing a query.
History mode enables button-event mouse reporting with SGR
coordinates; exiting restores the live application's mouse modes.

| Key in history mode | Action |
| --- | --- |
| `k` / `j`, Up / Down | One row older / newer |
| Mouse wheel up / down inside the active pane | Three rows older / newer |
| Ctrl-U / Ctrl-D | Half a pane older / newer |
| Page Up / Page Down | One pane older / newer |
| `g` / `G` | Oldest retained row / bottom of the snapshot |
| `/` / `?` | Search toward newer / older text |
| `n` / `N` | Repeat in the submitted search direction / opposite direction |
| `q` / Ctrl-C | Exit to the live screen |

Wheel events over the bar, separators or another pane are ignored. Clicking does
not change focus or select text. Modified vertical wheel events also scroll;
horizontal wheel, motion and release events are ignored. Legacy mouse reports
are consumed as complete reports so their payload cannot become keypresses.

Navigation stops at both ends. `G` stays in history mode; it does not resume the
live display. Window/pane shortcuts are unavailable until exit. Other input is
consumed locally. Bracketed paste is ignored while browsing; inside the query
editor it inserts text as described below. Plain unbracketed paste cannot be distinguished from
keypresses. Escape sequences are consumed with a 64-byte bound; standalone Esc
is not an exit key in this version.

## Literal search

Press `/` to search toward newer text or `?` to search toward older text, type a
query, and press Enter. Search is case-sensitive literal text,
without regular expressions or Unicode normalization. Soft-wrapped rows are
joined using their meaningful extents, including across the history/screen
boundary. Hard line breaks stay separate; unused padding and wide-character
placeholders do not become searchable spaces. Explicit spaces and combining
suffixes are searchable. Overlapping matches are included.

Both directions start from the viewport's top row. `/` selects the first match
starting on that row or below it, wrapping to the earliest match if none exists.
`?` selects the last match starting on that row or above it, wrapping to the latest
match if none exists. With multiple matches on the top row, `/` chooses the
leftmost and `?` the rightmost. There is no text cursor in this mode.

`n` repeats in the submitted search direction and `N` goes in the opposite
direction; both wrap at either end. The matched row moves to the top where
possible, clamped at the snapshot's bottom. The bar shows the current result
number in oldest-to-newest order, total count and `/` or `?`, or `no match`.
Only the selected match is highlighted by reversing its cell colors, including
both columns of wide glyphs and entire
cells when matching a combining suffix. The original snapshot is unchanged.
A match taller than the pane is only partially visible; use ordinary navigation
to inspect the remainder. Navigation preserves the selected result.

The query editor accepts at most 128 UTF-8 bytes. Left/Right (or Ctrl-B/Ctrl-F)
move by Unicode scalar; Ctrl-Left/Ctrl-Right move by word. Home/End (or
Ctrl-A/Ctrl-E) move to the start/end. Common CSI word-motion forms are accepted
as well, so the behavior follows terminals that encode Ctrl-Left as `ESC[1;5D`.
Typing inserts at the cursor. Backspace deletes the preceding scalar; Delete
(or Ctrl-D) deletes the following one. Ctrl-W deletes the preceding word,
including its separating whitespace; Ctrl-K deletes from the cursor to the end.
Ctrl-U clears the whole query.
Alt-Backspace (`ESC DEL`) is another spelling of Ctrl-W. Alt-D (`ESC d`) deletes
the next word and its following separator. At the beginning or end of the query,
these operations are no-ops.
Combining marks are separate scalars, not grapheme clusters. Long queries scroll
horizontally to keep the insertion cursor visible. A steady bar cursor appears
in the search row; it is hidden again on submission or cancellation. Ctrl-C or Ctrl-G cancels editing and retains
the previous search, selected result and direction; it does not exit history.
Enter with an empty query clears search highlighting. While editing, `q`, `j`, `k`, `n`, and other printable keys
are query text. Up/Down recall search terms; page sequences and mouse events are ignored.
Use Ctrl-C to cancel rather than Esc. Searches and query edits never reach a shell.

### Pasting a query

Bracketed paste inserts printable UTF-8 text at the query cursor without running
editor shortcuts. CR, LF, Tab, Backspace, Delete and other control characters are
discarded; line breaks are not converted to spaces. Supported escape reports are
consumed without navigation or editing. Pasted `q`, `j`, `k`, `/` and `?` are
ordinary query text. A newline inside the paste never submits a search: press
Enter after pasting to search, or Ctrl-C/Ctrl-G to cancel.

The existing 128-byte limit applies to the combined query, including pasted text.
Characters that do not fit are ignored whole; invalid or incomplete UTF-8 cannot
become part of the query. Paste is processed incrementally rather than buffered
as an unbounded string. Pasting into a recalled term creates an editable draft;
only explicit submission records it in query history. This behavior requires
bracketed-paste delimiters from the outer terminal; unbracketed text is handled
as ordinary keystrokes.

### Reusing search terms

Inside the query editor, Up recalls older submitted terms and Down recalls newer
ones. Both ordinary and application-mode arrow keys work. The first Up saves the
current draft and cursor position; Down past the newest term restores both.
Recalled terms initially place the cursor at the end. Up stops at the oldest term.
Recall changes only the input text: Enter runs the search, using the direction
chosen when opening the editor (`/` or `?`). Cancelling leaves the active search
and its direction unchanged.

Each frozen history view stores at most 20 distinct, nonempty submitted terms,
including searches with no matches. Resubmitting a term moves it to the newest
position. Empty submissions and cancelled edits are not recorded. Each term
retains the existing 128-byte UTF-8 limit. Editing a recalled term creates a new
draft without changing the stored entry; the next Up starts from the newest term
again, and Down can restore that edited draft. Records are discarded on leaving
history, including automatic exits; they are not shared across panes or persisted.

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
There is no snapshot text reflow, text selection, copying or disk persistence yet.
This does not recover content previously discarded by resize. A one-row outer terminal has no bar, so the history indicator is hidden.

Unit tests cover snapshot independence, navigation, wide-cell clipping, paste
isolation, logical-line and Unicode search, overlapping results, highlighting,
Unicode cursor editing, narrow query views and bounded query recall with draft
restoration, both search
directions and their row anchors, cancellation, result
wraparound, wheel bounds, malformed mouse reports and modal
input isolation. The nested-PTY suite checks browsing inside a split, the other pane's
continued visibility, new background output while frozen, snapshot-bottom versus
live output, forward/backward search submission/result cycling/no-match/cancellation,
query recall, middle editing, cursor visibility, query paste and draft restoration,
navigation/paste
isolation, wheel routing within a split, mouse-mode restoration and return to live
input after resize.
