# Soft Terminal Reset

CSI ! p (DECSTR) restores supported terminal modes without erasing cells or
moving the current cursor. It is distinct from [RIS](terminal-reset.md).

| State | After DECSTR |
| --- | --- |
| Visible cells and their stored styles | Preserved in both grids |
| Current cursor coordinates and active grid | Preserved |
| Cursor visibility | Enabled |
| Cursor shape | Blinking block |
| Synchronized output | Disabled |
| Insert, origin, application cursor key and keypad modes | Disabled |
| Automatic wrapping | Enabled (Rustmux default, following XTerm) |
| Pending wrap | Cleared |
| Current writing style | Default |
| G0/G1 and invocation | ASCII, G0 |
| Active scrolling region | Full height |
| Current grid's saved cursor | Home, default style and saved modes |
| Tab stops, bracketed paste, focus reporting and mouse modes | Preserved |

The [DEC manual](https://vt100.net/docs/vt510-rm/DECSTR.html) specifies that
autowrap is disabled by soft reset. Rustmux follows
[XTerm's compatibility behavior](https://github.com/ThomasDickey/xterm-snapshots/blob/master/charproc.c)
instead and restores its default enabled setting.

## Alternate screen and runtime

The inactive grid, its margins and saved cursor, and mode 1049's main snapshot
are retained. Leaving the alternate screen can restore those saved main cursor
modes. Insert mode and cursor visibility are global, however, so their reset
values remain in effect after leaving the alternate screen.

Resetting the saved cursor is not the same as homing the current cursor. A later
DECRC goes to home with default saved modes, while text immediately following
DECSTR is written at the unchanged current position.

No allocation or PTY operation is needed. Dimensions, queued input/replies and
the running child remain unchanged. Parsing continues after the command.

## Parsing and verification

Only the parameterless CSI ! p form is accepted. Parameters, private prefixes,
additional intermediates or other final bytes are ignored. C0 controls retain
their existing immediate behavior inside the sequence; CAN/SUB cancel it.
The supported intermediate forms are DECSTR, [DECSCUSR](cursor-shape.md) and
[DECRQM](mode-queries.md);
other CSI intermediate commands remain unsupported.

Run `cargo test --test soft_reset`. Fixtures cover mode restoration, cell and
cursor preservation, custom tabs, saved cursor defaults, alternate-screen
state, resize, malformed commands, queries and every input split. A real CLI
PTY test verifies preserved text, ASCII restoration and visible cursor after
reset, then returns to a shell prompt.
