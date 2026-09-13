# Terminal Reset

ESC c (RIS) returns the implemented terminal model to its initial state at the
current dimensions. It follows the reset command in
[XTerm's control sequence reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

The reset clears both cell grids, releases combining suffixes and makes the
main grid active. It removes all saved cursor state and restores:

- Cursor at row 1, column 1, visible, with no pending wrap.
- Default text style and colors.
- Full-height scrolling regions.
- Origin mode off, automatic wrapping on, insert mode off.
- ASCII in G0/G1, with G0 invoked.
- Default tab stops every eight columns.

The operation reuses existing grid and tab-stop storage. Dimensions are retained,
including after resize. Repeating RIS is harmless. Old text or saved modes cannot
reappear through DECRC or leaving the alternate screen.

## Parsing and runtime behavior

The parser discards unfinished control-sequence state and continues parsing
bytes after RIS normally. Status/cursor queries immediately observe reset state.
ESC c inside an OSC/DCS payload is ignored as payload; CSI c is a different,
currently unsupported device-attributes query.

RIS changes the screen model only. It does not restart the shell, change PTY
size or termios, clear queued keyboard input/replies, or reset the outer terminal.
The renderer draws the cleared model in the usual way; the CLI remains active.

## Verification and limits

Run `cargo test --test terminal_reset`. Tests compare the entire reset model
against a newly initialized screen, starting with populated grids and altered
modes. They cover both grids, saved state, repeated reset, resize, malformed
input, every chunk boundary, immediate query responses and renderer replay.
A real CLI PTY test resets from an alternate graphics screen and then returns
to an interactive shell prompt.

[Soft reset (DECSTR)](soft-reset.md) restores modes without erasing text.
Device-attribute replies and terminal capability queries remain future work. Hardware-terminal power-on behavior is outside this model.

RIS also disables bracketed paste and application cursor keys. The next rendered
frame synchronizes these input modes with the outer terminal.
