# UTF-8 and Character Width

This H03 step extends `Parser` and `Screen` to store Unicode text. The CLI still
forwards bytes directly; the model is not yet connected to rendering.

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
- `combining`: up to 16 zero-width scalars attached to a leading cell.
- `style`: the existing copied text attributes.

Rows remain read-only. Cells are cloneable but no longer Copy. Consumers must
skip width-zero placeholders and append a leader's combining suffix when reading
text. The existing `write_ascii` API still rejects unsupported input atomically;
`print` accepts decoded characters and ignores control characters.

A zero-width scalar attaches to the preceding cell in the current row, or the
character at the pending-wrap position. A trailing placeholder resolves to its
leader. At column zero with no pending wrap it is ignored. Suffixes retain the
base cell's style and do not move the cursor. Beyond 16 scalars, suffix input is
ignored to keep per-cell storage bounded.

## Boundaries and editing

A two-column character wraps before writing if only one column remains, clearing
the unused final cell. Filling the right edge sets delayed wrap as before. On a
one-column screen, a wide character is replaced by U+FFFD; scalar widths greater
than two use the same policy.

Writing or erasing either half of a wide character clears both halves, including
when the cursor was explicitly positioned on the trailing cell. Erase can
therefore extend one cell beyond the requested range. Scroll moves complete rows
of cells, preserving text, suffixes and styles. Newly blank cells retain the
existing active-background policy.

## Limits and verification

This is scalar-width handling, not full grapheme-cluster shaping. Emoji ZWJ and
modifier sequences, variation-selector width changes, flags, script ligatures
and bidirectional layout are not implemented. Individual wide emoji scalars
work, but complete emoji sequences may occupy a different width from an outer
terminal. Normalization is not performed.

`tests/unicode_screen.rs` covers multi-byte input at every chunk split, single-byte
feeds, cell-pair invariants, style preservation, wide wrap/scroll, partial erasure,
combining limits, malformed UTF-8 and end-of-stream handling. Run
`cargo test --test unicode_screen`.
