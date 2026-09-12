# Text Styles

This H03 step adds `Cell`, `Style` and `Color` in `src/style.rs`. The parser
applies SGR (`ESC [ parameters m`) to the screen's current style. Written cells
copy that style, so later changes cannot restyle existing text. Cursor movement
and pending wrap are unaffected by SGR. The CLI still forwards bytes directly;
the standalone [renderer](rendering.md) can emit these stored styles, but is not
yet connected to the CLI.

## Supported subset

The codes follow the [XTerm SGR reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).
Parameters are applied from left to right; omitted parameters mean reset.

| Codes | Meaning |
| --- | --- |
| 0 or omitted | Reset all attributes and colors |
| 1 / 2 / 22 | Bold / dim / clear both |
| 3 / 23 | Italic on / off |
| 4 / 24 | Underline on / off |
| 5 / 25 | Blink on / off |
| 7 / 27 | Inverse on / off |
| 8 / 28 | Hidden on / off |
| 9 / 29 | Strikethrough on / off |
| 30–37 / 40–47 | Basic foreground / background |
| 90–97 / 100–107 | Bright foreground / background |
| 39 / 49 | Default foreground / background |
| 38;5;n / 48;5;n | Indexed foreground / background, n in 0–255 |
| 38;2;r;g;b / 48;2;r;g;b | RGB foreground / background, components in 0–255 |

Default colors remain symbolic and distinct from palette index zero. Indexed
colors are not converted to RGB, and bold does not implicitly select bright
colors. Inverse and other attributes are stored for the future renderer.

## Erase and scrolling

Overwriting a cell replaces its character and style together. Scrolling copies
both. Erasure and a newly exposed bottom row create spaces with the current
background, default foreground and no text decorations (including inverse).
They do not reset the current writing style. Ordinary written spaces, in contrast,
retain the full writing style.

## Invalid input and verification

SGR uses the parser's fixed 32-parameter buffer. Excess parameters, integer
overflow invalidate the whole command. Missing or out-of-range
extended-color components also leave the current style unchanged, including
attributes earlier in that command. Unknown simple codes are ignored. Unsupported
underline-color code 58 consumes a valid extended-color group without applying
its components as attributes. Double underline and other attributes remain outside this subset.

Colon groups support 38/48:5:n and 38/48:2:r:g:b, including an optional
empty or zero color-space slot (38:2::r:g:b). Unsupported subparameter
groups are ignored as a unit; malformed color groups reject the whole SGR.
The fixed buffer counts both parameters and subparameters toward its limit.

`tests/text_style.rs` checks style snapshots, individual/global resets, all 16
palette entries, indexed/RGB colors, malformed groups, parameter limits, wrapping,
overwrite, erase and scroll. Tests repeat each input at every two-chunk split
and byte by byte. Run `cargo test --test text_style`.
