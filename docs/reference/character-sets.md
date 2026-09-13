# Character Sets

This step supports ASCII and DEC Special Graphics in the G0/G1 slots. Legacy
terminal applications can draw borders without emitting Unicode directly.
The model translates selected ASCII characters into Unicode cells before
normal width handling and rendering.

| Sequence | Effect |
| --- | --- |
| ESC ( B / ESC ( 0 | Designate G0 as ASCII / DEC Special Graphics |
| ESC ) B / ESC ) 0 | Designate G1 as ASCII / DEC Special Graphics |
| SI (0x0f) | Invoke G0 |
| SO (0x0e) | Invoke G1 |

Both slots start as ASCII; G0 is initially invoked. Designating a slot does not
invoke it. Character-set changes do not move the cursor, erase text, change style
or cancel pending wrap. SI/SO execute within incomplete ESC/CSI sequences but
are ignored inside OSC/DCS payloads.

## Mapping and state

The special set maps ASCII 0x5f through 0x7e to the DEC graphics repertoire.
For example, lqqk becomes ┌──┐ and x becomes │. It also includes junctions,
scan lines, a diamond, checkerboard, degree/plus-minus signs, mathematical
symbols and visible control pictures. Bytes outside that range and decoded
non-ASCII Unicode text are unchanged. See
[XTerm's character-set reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

Mapped glyphs use the current style and existing Unicode width, combining,
insertion and wrapping rules. The screen API stores translated characters;
the renderer emits Unicode rather than forwarding character-set modes to the
outer terminal.

DECSC/DECRC and mode 1049 save/restore both designations and the invoked slot.
Alternate entry retains the current settings. Resize preserves them along with
saved cursor state. Unknown designators and unsupported extended designations
are consumed without changing settings; cancellation and split-input handling
follow the existing parser rules.

## Verification and limits

Run `cargo test --test character_sets`. Tests cover boxes, G0/G1 selection,
ASCII restoration, symbols, UTF-8, colors, combining suffixes, renderer replay,
cursor/alternate/resize restoration, malformed input and insertion/wrapping.
Fixtures are repeated at every two-chunk split and byte by byte. A real CLI
PTY test checks visible Unicode borders and SI/SO switching.

G2/G3, national replacement sets, single shifts and 8-bit character-set invocation
remain unsupported. This is not full ISO-2022 emulation.
