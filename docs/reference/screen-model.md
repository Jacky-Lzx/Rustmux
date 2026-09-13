# Basic Screen Model

This is the first part of H03, available as `rustmux::screen::Screen`. Read
`src/screen.rs` and its fixture tests. The CLI parses PTY output into this model and renders its active grid.

The model owns a resizable cell grid, initialized with default-style spaces, a zero-based
(row, column) cursor, and a pending-wrap flag. Dimensions must be nonzero and the
allocation size must fit. Rows can be borrowed read-only as `&[Cell]`, including trailing spaces. Each cell
contains a character and a copied `Style`; `Screen::style` is the current style
for future writes.

## Text and cursor rules

`write_ascii` accepts printable ASCII plus LF, CR and BS. Unsupported input,
including escape sequences and UTF-8, rejects the entire call without changing
state. This deliberately restricted API is not an escape-sequence parser.

- Printable characters overwrite cells and advance the cursor.
- With default automatic wrapping, filling the last column leaves the cursor
  there with a pending wrap. The next printable character wraps to column zero.
  [DECAWM](auto-wrap.md) can disable this behavior.
- LF advances one row while preserving the column. CR returns to column zero.
- BS moves left without erasing, stops at column zero and never crosses rows.
- LF, CR and BS cancel a pending wrap.
- At the bottom scrolling margin, LF and wrapping scroll that region upward and
  blank its bottom row. The default region covers the whole grid. There is no
  scrollback; the region's top row is discarded. See [Scrolling Regions](scrolling-regions.md).

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

The [Unicode layer](unicode.md) adds incremental decoding, wide cells and bounded
zero-width suffixes using unicode-width. [Alternate Screen](alternate-screen.md) adds an isolated
second grid and saved main state. [Screen Model Resize](screen-resize.md) defines
clipping and growth for both grids. The [renderer](rendering.md) emits full frames;
the CLI now queues these frames in its event loop.
