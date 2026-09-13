# Automatic Wrap Mode

DECAWM controls automatic wrapping at the right edge. It is enabled by default.
The basic behavior follows [DEC's description](https://vt100.net/docs/vt510-rm/DECAWM.html).

| Sequence | Effect |
| --- | --- |
| CSI ? 7 h | Enable automatic wrapping |
| CSI ? 7 l | Disable automatic wrapping |

With wrapping enabled, filling the last column keeps the cursor there until the
next positive-width character, which moves to the next row. At the bottom
scrolling margin, this scrolls the region. With wrapping disabled, subsequent
single-column characters overwrite the last column without scrolling.

Toggling the mode does not move the cursor or discard its right-edge state.
Re-enabling at an already filled edge lets the next printable character wrap.
Explicit LF, IND, RI, cursor movement and editing commands keep their existing
behavior; DECAWM controls only automatic wrapping.

## Unicode and insertion

Combining suffixes still attach to the character just written at the edge even
when wrapping is disabled. A fitting wide glyph occupies two columns normally.
With wrapping disabled, a wide glyph that cannot fit in the remaining columns
is ignored without moving the cursor or changing cells. A one-column screen
retains its existing replacement-character policy for wide glyphs.

Writing a narrow character over a wide continuation clears both halves before
writing. Insert mode shifts only the available columns; it cannot trigger
automatic wrapping while DECAWM is disabled.

## State and verification

DECSC/DECRC and mode 1049 save and restore DECAWM with the cursor state. Alternate
entry retains the current mode. Resize preserves current and saved modes but
discards right-edge state when dimensions change. Same-size/failed resize is a
no-op. The public `wrap_pending()` reports whether the next printable character
will actually wrap; internal edge state is retained separately from permission
to wrap so combining suffixes remain attached correctly.

Run `cargo test --test auto_wrap`. Tests cover chunk splits, edge overwrite,
re-enabling, Unicode, insert mode, region boundaries, saved cursor/alternate/
resize state and malformed commands. A real CLI PTY fixture verifies last-column
overwrite followed by wrapping after re-enabling.

[Tab Stops](tab-stops.md) adds configurable horizontal stops.
Horizontal margins and queries beyond standard DSR remain future work.
