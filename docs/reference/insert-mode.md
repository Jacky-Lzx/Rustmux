# Insert Mode

IRM changes how printable text is written, following the
[DEC insert/replace mode](https://vt100.net/docs/vt510-rm/IRM.html).

| Sequence | Effect |
| --- | --- |
| CSI 4 h | Enable insertion for subsequent printable characters |
| CSI 4 l | Return to replacement (the default) |

In insert mode, each positive-width character shifts the rest of the current
row right by its display width before being written. Text pushed past the right
edge is discarded. In replace mode, new text overwrites existing cells.
The cursor advances normally in either mode.

For example, writing XY at column 3 of abcdefgh yields abXYcdefgh in insert
mode, or abXYefgh in replace mode, assuming enough screen columns.

## Unicode and wrapping

Wide characters insert two columns. Combining suffixes attach to the previous
character without moving any cells. Partial wide glyphs at insertion or clipping
boundaries are cleared using the [character editing](character-editing.md)
rules. Complete moved cells retain their styles and combining suffixes; new
characters use the current writing style.

Delayed wrap and a wide character that cannot fit are handled before insertion.
This includes scrolling at the bottom margin. Toggling IRM does not move the
cursor, change text/style, or cancel delayed wrap. Explicit ICH/DCH/ECH commands
keep their existing behavior; enabling IRM does not apply them twice.

## State and parsing

IRM is a global mode. It is retained across resize and screen switching and is
not part of DECSC/DECRC or mode 1049's cursor snapshot. Its initial value is off.
A change made while the alternate screen is active therefore remains after exit.

IRM uses standard CSI h/l, not private CSI ? h/l. Combined mode lists are
processed in order, with unknown standard modes ignored. Colon groups and
numeric/parameter overflow invalidate the command before any mode changes.

## Verification and remaining work

Run `cargo test --test insert_mode`. Tests repeat input at every chunk split and
byte by byte and check mode changes, wide/combining characters, style retention,
delayed wrap, region scrolling, one-column screens, saved cursor/alternate/resize
behavior and interaction with explicit edits. A real CLI PTY test switches from
insertion to replacement and verifies the final visible row.

[Automatic Wrap Mode](auto-wrap.md) controls edge wrapping independently of IRM.
Custom tab stops, horizontal margins and terminal queries remain future work. Full Neovim compatibility is not yet claimed.
