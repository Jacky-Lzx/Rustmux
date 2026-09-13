# Character Editing

This H03 step adds column operations for local edits within a row. Commands use
the [XTerm control sequence syntax](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

| Sequence | Effect |
| --- | --- |
| CSI n @ (ICH) | Insert blank columns at the cursor and shift the rest right |
| CSI n P (DCH) | Delete columns at the cursor and shift the rest left |
| CSI n X (ECH) | Erase columns at the cursor without shifting text |

Omitted or zero counts mean one. Counts clamp to the columns remaining in the
row before any movement. Cursor coordinates and writing style stay unchanged;
successful operations cancel delayed wrap. Private forms, extra parameters,
colon groups and numeric overflow are ignored. The public Screen methods treat
a zero count as a no-op.

Operations affect the current row even outside vertical scrolling margins.
Other rows and the inactive screen are untouched. Content pushed past the right
edge is discarded; no wrap, scrolling or reflow occurs. New blanks use the current
background with default foreground and no decorations.

## Wide characters and combining suffixes

Counts refer to terminal columns, not Unicode scalars or bytes. Complete moved
cells retain their styles and combining suffixes. Editing through a double-width
glyph clears both halves. Insertion also clears a glyph cut by the right edge.
This may blank one neighboring column beyond the requested range, but deletion
still shifts by the requested number of columns. No orphan continuation or wide
leader is retained.

For example, deleting one column at the second half of a wide glyph removes that
column, blanks the first half, and shifts the following content left by one.
Movement uses existing cell storage without cloning suffix allocations.

## Verification and limits

Run `cargo test --test character_edit`. Tests cover default/large counts,
malformed commands, pending wrap, every split in input, all column/count
combinations across mixed wide and narrow text, style and suffix preservation,
renderer replay, one-cell grids and alternate isolation. The real CLI PTY test
checks ICH, DCH and ECH on visible text.

Horizontal margins and terminal queries remain
unsupported. These operations improve full-screen application compatibility but
do not imply complete Neovim support.
