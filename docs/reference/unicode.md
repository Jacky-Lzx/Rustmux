# UTF-8 and Character Width

This H03 step extends `Parser` and `Screen` to store Unicode text. The CLI uses this model
to render decoded PTY output.

## Streaming decoding

The parser keeps at most four bytes of a UTF-8 scalar across `advance` calls.
Invalid prefixes produce U+FFFD, then remaining bytes are processed again so an
ASCII character or ESC command is not swallowed. UTF-8 surrogate encodings,
overlong encodings and values above U+10FFFF are invalid. Raw 8-bit C1 bytes do
not become terminal commands.

Call `Parser::finish` only when the output stream ends. It replaces a remaining
incomplete UTF-8 prefix once and discards an unfinished control sequence. Never
call it at ordinary read boundaries. Ignored control-string payload remains
ignored, including any Unicode inside it.

## Cells and width

`Screen::print(char)` uses `unicode-width` 0.2.2's non-CJK scalar width function.
Ambiguous-width characters use the narrow convention. `Cell` now contains:

- `character`: the base scalar (a space for a trailing placeholder).
- `width`: 1 for an ordinary cell, 2 for a wide leading cell, 0 for its next cell.
- `combining`: up to 16 combining marks or supported emoji suffix scalars attached
  to a leading cell.
- `style`: the existing copied text attributes.

Rows remain read-only. Cells are cloneable but no longer Copy. Consumers must
skip width-zero placeholders and append a leader's combining suffix when reading
text. The existing `write_ascii` API still rejects unsupported input atomically;
`print` accepts decoded characters and ignores control characters.

A zero-width scalar attaches to the preceding cell in the current row, or the
character at the pending-wrap position. A trailing placeholder resolves to its
leader. At column zero with no pending wrap it is ignored. Suffixes retain the
base cell's style. Ordinary suffixes do not move the cursor. Beyond 16 scalars,
suffix input is ignored to keep per-cell storage bounded.

VS15 (U+FE0E) and VS16 (U+FE0F) are exceptions: the retained base and suffix
sequence is measured with `unicode-width`'s non-CJK string width. A valid text
or emoji presentation sequence can change its cell span between one and two
columns. The trailing placeholder, cursor and delayed wrap are updated together,
even when the selector arrives in a later PTY read. Invalid selector/base pairs
do not widen arbitrary text. Growing a glyph in insert mode inserts the additional
column; shrinking releases the old trailing cell without shifting later text.

Emoji skin-tone modifiers (U+1F3FB through U+1F3FF) stay with a valid modifier
base, using the dependency's string-width tables to recognize a two-column
sequence. Invalid bases and repeated modifiers remain separate scalars. Two
adjacent regional indicators (U+1F1E6 through U+1F1FF) form one two-column flag
cell; a third indicator starts another cell. Intervening combining marks prevent
flag pairing. These suffixes retain the base's style and hyperlink, including
when SGR or OSC 8 changes between reads. They use the same 16-scalar suffix limit.

The complete sequence is rendered, copied, searched and saved as one cell span.
Reflow and edits preserve it as a unit. A flag's second indicator can widen its
initial one-column base; delayed wrap and insert mode use the selector growth
rules above. With wrapping disabled, a suffix that would widen past the right
edge is ignored. On a one-column screen, flag growth uses U+FFFD.

## Incremental Emoji ZWJ sequences

A positive-width scalar after U+200D joins the preceding cell when the
`unicode-width` string tables recognize the resulting sequence as two columns
and it is narrower than the separately occupied components. This supports
`👩‍💻`, `👨‍👩‍👧‍👦`, `👩🏽‍💻`, `🧑🏻‍🤝‍🧑🏿`, `🏳️‍🌈` and `❤️‍🔥`, even
when every component arrives in a separate PTY read. Each joined component can
have its own valid skin-tone modifier. SGR and OSC 8 changes retain the original
leader's metadata; the next independent cell uses the current metadata.

Joined components leave delayed wrap and insert mode at the complete cell's
width. Erasure, copying, search, reflow and history persistence use that whole
cell. The 16-scalar suffix bound still applies. A joiner reserves a slot for its
next component; when no slot remains it is ignored, rather than emitting an
unfinished join that the outer terminal could extend beyond the stored cell.
Recognized suffixes beyond the bound are ignored.

A late VS15 that breaks the joined sequence into separate presentation
components splits the final component into its own cell, preserving its original
style and hyperlink. Its width, insertion and wrapping follow ordinary printing.
With wrapping off, the selector is ignored when the separated component cannot
fit.
This follows the [Unicode Emoji text-presentation boundary](https://www.unicode.org/reports/tr51/#def_emoji_zwj_sequence).

This increment joins only sequences recognized when the positive-width
component arrives. A text-default component after a joiner that needs a later
VS16 (for example the heart inside `👩‍❤️‍👩`) remains in its own cell;
retroactive merging across already occupied cells is not implemented. Regional
indicator pairs and keycap sequences as joined components are also outside this
step. These boundaries can still differ from the outer terminal's width.

## Boundaries and editing

A two-column character wraps before writing if only one column remains, clearing
the unused final cell. Filling the right edge sets delayed wrap as before. On a
one-column screen, a wide character is replaced by U+FFFD; scalar widths greater
than two use the same policy.

A selector that widens a glyph at the final column moves the complete glyph to
the next row with automatic wrap enabled. With wrapping disabled, that selector
is ignored and the narrow base remains. A one-column screen uses U+FFFD instead.

Writing or erasing either half of a wide character clears both halves, including
when the cursor was explicitly positioned on the trailing cell. Erase can
therefore extend one cell beyond the requested range. Scroll moves complete rows
of cells, preserving text, suffixes and styles. Newly blank cells retain the
existing active-background policy.

## Limits and verification

This combines scalar-width handling with the specific emoji sequences above,
not full grapheme-cluster shaping. Only the incremental ZWJ subset above is
implemented; script ligatures and bidirectional layout are not implemented. The
History query editor still moves and clips by scalar rather than complete emoji
sequences. Other sequences may
occupy a different width from an outer terminal. Normalization is not performed.

`tests/unicode_screen.rs` covers multi-byte input at every chunk split, single-byte
feeds, cell-pair invariants, style preservation, wide wrap/scroll, partial erasure,
combining limits, malformed UTF-8 and end-of-stream handling. Run
`cargo test --test unicode_screen`.
The selector cases also cover style retention, insert mode and right-edge
policies. `tests/incremental_render.rs` independently models a two-column
warning emoji, modifier sequences, flags and supported ZWJ sequences to verify
that erasing a shorter replacement leaves no trailing character in the external
terminal. History
snapshot tests cover plain/styled restoration at a narrower width; search tests
check that matching a suffix selects the whole cell span.
The real child-PTY input scenario queries the cursor between sequence components
and after right-edge flag growth and pending-wrap ZWJ joining, checking the
application-visible geometry.
