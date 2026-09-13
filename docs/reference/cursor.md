# Cursor State

The parser supports DEC cursor controls described in the
[XTerm reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html):

- `CSI ? 25 h` shows the cursor; `CSI ? 25 l` hides it.
- `ESC 7` saves cursor state (DECSC); `ESC 8` restores it (DECRC).

Visibility starts enabled and is a global mode. It survives screen switching and
resize, and is not part of a cursor save. The renderer hides the cursor while
painting and ends with the model's requested visibility. CLI exit cleanup still
shows the outer cursor.

Each grid has one explicit save slot, containing coordinates, current text style
and pending wrap. Saving again replaces it; restoring does not consume it. Restore
without a save is a no-op. Main and alternate slots are independent. A fresh
alternate visit has no explicit save, and its slot is discarded on exit. The
separate 1049 main-screen snapshot remains intact even when a program saves a
cursor inside the alternate screen.

Restoring state does not restore screen text or erase intervening output. Actual
resize clamps both explicit slots and the 1049 snapshot, clearing pending wrap;
a same-size resize leaves them unchanged.

This is the saved-state subset currently modeled. Origin/charset state, cursor
shape and blinking mode, standalone mode 1048 and ANSI CSI s/u are not supported.

`tests/cursor.rs` verifies position/style/wrap, slot replacement, alternate-screen
isolation, resize and rendered visibility, with every two-chunk split. The real
PTY test also drives cursor hide/show through the CLI. Run `cargo test --test cursor`.

[Origin Mode](origin-mode.md) is also included in saved cursor state. Restoring
a saved row clamps it against the current margins when that mode is enabled.
