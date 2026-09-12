# Basic Screen Model

This is the first part of H03, available as `rustmux::screen::Screen`. Read
`src/screen.rs` and its fixture tests. The CLI still forwards output directly;
this model is not yet connected to the PTY loop.

The model owns a fixed-size cell grid, initialized with default-style spaces, a zero-based
(row, column) cursor, and a pending-wrap flag. Dimensions must be nonzero and the
allocation size must fit. Rows can be borrowed read-only as `&[Cell]`, including trailing spaces. Each cell
contains a character and a copied `Style`; `Screen::style` is the current style
for future writes.

## Text and cursor rules

`write_ascii` accepts printable ASCII plus LF, CR and BS. Unsupported input,
including escape sequences and UTF-8, rejects the entire call without changing
state. This deliberately restricted API is not an escape-sequence parser.

- Printable characters overwrite cells and advance the cursor.
- Filling the last column leaves the cursor there with a pending wrap. Only the
  next printable character wraps to column zero of the next row.
- LF advances one row while preserving the column. CR returns to column zero.
- BS moves left without erasing, stops at column zero and never crosses rows.
- LF, CR and BS cancel a pending wrap.
- Advancing beyond the bottom scrolls the whole grid upward by one row and blanks
  the bottom row. There is no scrollback; the top row is discarded.

These rules also apply to a one-row or one-column screen. Valid input split across
multiple calls produces the same state as a single call.

## Verification and next steps

Fixtures check exact rows and cursor positions for overwrites, control characters,
delayed wrap, bottom scrolling, a single-cell screen, split input and rejection
without mutation. Run `cargo test --lib screen::tests` for the model tests.

The [basic parser](control-sequences.md) now applies cursor movement and erase
commands through the screen API. Movement clamps to the grid; erase replaces
cells with spaces without moving the cursor. Both cancel pending wrap.

The [text style layer](text-styles.md) adds SGR attributes and colors. Erased cells
and newly exposed rows use the active background without text decorations.

Unicode decoding and character width, alternate screens,
resize policy and rendering are subsequent work. No dependencies are added by this
step, and interactive CLI behavior is unchanged.
