# Basic Control Sequence Parser

`rustmux::parser::Parser` incrementally applies UTF-8 text and a small CSI subset to
`Screen`. Keep one parser per output stream and call `advance` with the same
screen as chunks arrive. Incomplete sequences are retained between calls.
The CLI uses this parser for all PTY output before rendering the screen.

```rust
use rustmux::{parser::Parser, screen::Screen};

let mut screen = Screen::new(24, 80)?;
let mut parser = Parser::new();
parser.advance(&mut screen, b"hello\x1b[2;");
parser.advance(&mut screen, b"3HX"); // X at zero-based row 1, column 2.
# Ok::<(), std::io::Error>(())
```

## Supported commands

CSI below means the two bytes ESC followed by `[`. The command subset follows
[XTerm's control sequence reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

| Sequence | Effect |
| --- | --- |
| CSI n A / B / C / D | Move up / down / right / left; omitted or zero count means one |
| CSI row ; column H / f | Position relative to the active origin using one-based coordinates; omitted or zero values mean one |
| CSI n J | Erase display: 0 cursor through end, 1 start through cursor, 2 whole grid |
| CSI parameters m | Set text attributes and colors; see [Text Styles](text-styles.md) |
| CSI 4 h / l | Enable / disable [insert mode](insert-mode.md) |
| CSI ? 7 h / l | Enable / disable [automatic wrap](auto-wrap.md) |
| CSI ? 6 h / l | Enable / disable [origin mode](origin-mode.md) and home |
| CSI n G / ` / d | Set column / column / origin-relative row |
| CSI n E / F | Next / previous line, moving to column zero |
| CSI n @ / P / X | Insert / delete / erase columns; see [Character Editing](character-editing.md) |
| CSI n L / M | Insert / delete lines inside the region; see [Line Editing](line-editing.md) |
| CSI n S / T | Scroll the region up / down without moving the cursor |
| CSI top ; bottom r | Set vertical [scrolling margins](scrolling-regions.md) and home |
| ESC D / E / M | Index / next line / reverse index within scrolling margins |
| CSI n K | Erase line: 0 cursor through end, 1 start through cursor, 2 whole row |

Erase defaults to mode 0, includes the cursor cell, and leaves cursor coordinates
unchanged. Movement clamps without scrolling; vertical relative movement respects scrolling margins. Both movement and erase
cancel pending wrap. Screen methods use zero-based positions and `EraseMode`;
the parser handles protocol defaults and one-based conversion.

HT uses configurable [tab stops](tab-stops.md), initially eight columns apart,
without erasing or wrapping. ESC H sets a stop; CSI g clears current/all stops;
CSI n I/Z moves forward/backward through stops.
Printable ASCII and LF/CR/BS retain the screen model's existing behavior. Those
three controls also execute inside an incomplete ESC/CSI sequence without ending
it. CAN and SUB cancel a sequence; a new ESC restarts ESC/CSI parsing.

## Unsupported input and limits

Unknown CSI commands, unsupported private commands, intermediate bytes, non-SGR colon subparameters,
extra parameters and numeric overflow cause the command to be ignored through
its final byte. At most 32 optional usize parameters are stored. Cursor positioning still accepts
at most two; one-parameter commands reject extra parameters. Parser storage is constant regardless of
sequence length. Unsupported ESC sequences are consumed without printing their
sequence bytes.

OSC payload is discarded through BEL or ST (ESC followed by backslash).
DCS, SOS, PM and APC payloads are discarded through ST. These strings are not
interpreted or buffered; an unterminated string continues to discard input until
its terminator or cancellation. Other unsupported controls are ignored.
UTF-8 decoding and replacement are
described in [UTF-8 and Character Width](unicode.md). There is no 8-bit C1 command
support or terminal replies yet. Cursor visibility and ESC 7/8 are described in [Cursor State](cursor.md).
Mode 1049 is described in
[Alternate Screen](alternate-screen.md).

Unlike `Screen::write_ascii`, this streaming interface skips unsupported input;
it does not reject an entire chunk. Chunk boundaries have no semantic meaning.

## Verification

Fixtures assert exact rows and cursor positions, then repeat at every two-chunk
split and one byte at a time. They cover defaults, clamping, inclusive erase,
pending wrap, single-cell screens, cancellation, embedded controls, string
terminators and malformed commands. Megabyte parameter/string inputs check
recovery without buffering payload. Run `cargo test --lib parser::tests`.
