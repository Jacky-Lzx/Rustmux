# Browsing History

Ctrl-B `[` opens a read-only snapshot of the active pane's retained primary
history and current screen. It initially moves up one pane-height, clamped to
available history. When no rows have scrolled out yet, it opens at `History 0/0`
and still freezes the current visible screen. Alternate-screen applications ignore entry.
Other panes continue displaying live output. The window bar shows `History`, the
number of rows above the snapshot's bottom, and the snapshot history length.
The browsed pane's complete border changes to Catppuccin Mocha Peach and returns
to Catppuccin Mocha Green on exit.
The cursor is hidden while browsing and shown in the bar while editing a query.
History mode enables drag-event mouse reporting with SGR
coordinates; exiting restores the live application's mouse modes.

| Key in history mode | Action |
| --- | --- |
| `k` / `j`, Up / Down | One row older / newer |
| Mouse wheel up / down inside the active pane | Three rows older / newer |
| Left-button drag inside the active pane | Select and copy text through OSC 52 on release |
| Ctrl-U / Ctrl-D | Half a pane older / newer |
| Page Up / Page Down | One pane older / newer |
| `g` / `G` | Oldest retained row / bottom of the snapshot |
| `/` / `?` | Search toward newer / older text |
| `n` / `N` | Repeat in the submitted search direction / opposite direction |
| `y` | Copy the current search match, or the visible snapshot when no match is active, through OSC 52 |
| `v` | Start or cancel keyboard text selection |
| `q` / Ctrl-C / Esc | Exit to the live screen |

Wheel events over the bar, pane borders or another pane are ignored. A click
without movement neither highlights nor copies a cell. Modified vertical wheel
events also scroll; horizontal wheel events are ignored. Legacy mouse reports
are consumed as complete reports so their payload cannot become keypresses.

Navigation stops at both ends. `G` stays in history mode; it does not resume the
live display. Window/pane shortcuts are unavailable until exit. Other input is
consumed locally. Bracketed paste is ignored while browsing; inside the query
editor it inserts text as described below. Plain unbracketed paste cannot be distinguished from
keypresses. Escape sequences are consumed with a 64-byte bound. A standalone Esc
is distinguished from a CSI/SS3 sequence with a 30 ms timeout; incomplete
multi-byte sequences are discarded when that timeout expires.

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
Press `y` to copy only the selected match through OSC 52 while retaining the
query, result number and highlight. The ordinary visible-viewport copy remains
available whenever no search match is active, including after a no-match query.

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
in the search row; it is hidden again on submission or cancellation. Ctrl-C or
Ctrl-G cancels editing and retains the previous search, selected result and
direction. Esc clears both the editor and any active query or result highlight,
but remains in history mode.
Enter with an empty query clears search highlighting. While editing, `q`, `j`, `k`, `n`, and other printable keys
are query text. Up/Down recall search terms; page sequences and mouse events are ignored.
Press Esc again after the search state clears to exit history. Searches and query
edits never reach a shell.

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

## Keyboard text selection

Press `v` to anchor a selection at the first meaningful cell in the viewport's
top row. `h/j/k/l` or the arrow keys move its active end by one cell or row;
horizontal movement wraps across row boundaries and skips wide-character
placeholder cells. Ctrl-U/Ctrl-D and Page Up/Page Down move by one pane height,
while `g`/`G` extend to the snapshot's first/last row. Movement scrolls the
frozen viewport as needed. Wheel events and unrelated history/search commands
are ignored until the selection is completed or cancelled.

The endpoints are inclusive and either end may precede the anchor. Selected
cells are highlighted instead of the current search result. Press `y` to copy
the selected text through OSC 52, end selection and remain in history mode;
press `v` again to cancel without copying. `q` or Ctrl-C exits history directly.
After selection ends, plain `y` again copies the complete visible viewport.

Selection copies complete wide glyphs and combining suffixes. It joins rows
created by soft wrapping without a newline and inserts `\n` across explicit hard
line boundaries. Unused padding, clipped glyphs, styles, the bar and other panes
are omitted. The same 32 KiB all-or-nothing limit applies.

## Mouse text selection

Drag the left mouse button across the active pane in history mode to select
text. Selection endpoints are inclusive and may be dragged in either direction.
Releasing the button copies the selected text immediately through OSC 52 and
clears its highlight. The bar reports `Copy sent to terminal` after a successful
keyboard or mouse copy, then restores the ordinary history status after one
second. Navigation also clears the confirmation immediately. A press must begin
inside the active pane; dragging beyond its content clamps the active endpoint
to the nearest edge. Drag motion above or below the
pane scrolls the frozen snapshot one row per report and continues extending the
selection. Holding the pointer beyond either edge continues scrolling one row
every 50 ms without requiring more motion reports, until the pointer returns,
the button is released or the corresponding history boundary is reached.

Mouse selection uses the same logical-text rules and 32 KiB limit as keyboard
selection. Wide-character placeholder columns select the complete character,
soft-wrapped rows are joined, and hard line boundaries insert a newline. A
single-cell click is treated as a click rather than a selection and sends no
clipboard request. Mouse reports are ignored while editing a search query or
extending a keyboard selection.

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

Without an active search match, `y` copies only the active pane's visible
snapshot rows, separated by newlines (including soft-wrapped rows). With an
active match, it copies only that match, joining soft wraps and retaining hard
line breaks. Explicit trailing spaces are preserved; unused padding and clipped
wide glyphs are omitted. Styling, the bar and other panes are not copied. The
operation stays in history mode.

Copy text is limited to 32 KiB of UTF-8 before Base64 encoding. An oversized
view displays `Copy too large` and sends no clipboard request, rather than
silently copying a prefix. Ordinary navigation dismisses this message. Pending
terminal output is drained before more history input is processed, so repeated
copy keys cannot accumulate an unbounded output queue. Terminal clipboard
acceptance is not acknowledged by this operation.

Rows retain their original widths: shorter rows are padded and longer rows are
clipped to the pane width, with clipped wide characters replaced by blank cells.
There is no snapshot text reflow or disk persistence yet. Copying
uses the outer terminal's OSC 52 clipboard support; terminals or multiplexers
that disable OSC 52 will ignore it.
This does not recover content previously discarded by resize. A one-row outer terminal has no bar, so the history indicator is hidden.

Unit tests cover snapshot independence, navigation, wide-cell clipping, paste
isolation, logical-line and Unicode search, overlapping results, highlighting,
Unicode cursor editing, narrow query views and bounded query recall with draft
restoration, keyboard and mouse selection, both search
directions and their row anchors, cancellation, result
wraparound, wheel bounds, malformed mouse reports and modal
input isolation. The nested-PTY suite checks browsing inside a split, the other pane's
continued visibility, new background output while frozen, snapshot-bottom versus
live output, viewport and keyboard/mouse-selection OSC 52 copying, forward/backward search submission/result cycling/no-match/cancellation,
query recall, middle editing, cursor visibility, query paste and draft restoration,
navigation/paste
isolation, wheel routing within a split, mouse-mode restoration and return to live
input after resize.
