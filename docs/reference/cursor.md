# Cursor State

The parser supports DEC cursor controls described in the
[XTerm reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html):

- `CSI ? 25 h` shows the cursor; `CSI ? 25 l` hides it.
- `ESC 7` saves cursor state (DECSC); `ESC 8` restores it (DECRC).
- Parameterless `CSI s` / `CSI u` provide the SCO/ANSI save/restore forms.
  Rustmux treats them as aliases of `ESC 7` / `ESC 8`, sharing the same save slot
  and saved state, rather than a separate position-only slot. The forms can be
  mixed, and the last save replaces the slot regardless of syntax.
- `CSI ? 1048 h` saves and `CSI ? 1048 l` restores through that same slot, as
  specified by XTerm. It can likewise be mixed with either syntax above.

Only parameterless CSI s/u are accepted. Explicit numeric parameters (including
zero), private prefixes and intermediates are not cursor restores. This keeps
parameterized CSI u keyboard sequences and CSI Pl ; Pr s margin commands
distinct. Left/right margin mode (DECLRMM) is not implemented. Parameterized
keyboard commands are handled separately; see
[Kitty Keyboard Protocol](kitty-keyboard.md).

Visibility starts enabled and is a global mode. It survives screen switching and
resize, and is not part of a cursor save. The renderer hides the cursor while
painting and ends with the model's requested visibility. CLI exit cleanup still
shows the outer cursor.

Each grid has one explicit save slot, containing coordinates, current text style,
pending wrap, origin/autowrap modes and G0/G1 character-set state. Saving again replaces it; restoring does not consume it. Restore
without a save is a no-op. Main and alternate slots are independent. A fresh
mode-1049 visit has no explicit alternate save, and that slot is discarded on
1049 exit. Modes 47 and 1047 retain the alternate slot across switches. The
separate 1049 main-screen snapshot remains intact even when a program saves a
cursor inside the alternate screen.

Restoring state does not restore screen text or erase intervening output. Actual
resize clamps both explicit slots and the 1049 snapshot, clearing pending wrap;
a same-size resize leaves them unchanged.

Cursor shape/blinking preference and global input modes are not in the saved
state. [Cursor shapes](cursor-shape.md) are controlled separately.

`tests/cursor.rs` and `tests/ansi_cursor.rs` verify position/style/wrap, slot replacement, alternate-screen
isolation, resize and rendered visibility, with every two-chunk split. The real
PTY test also drives cursor hide/show and mode 1048 save/restore through the CLI.
Run `cargo test --test cursor` and `cargo test --test ansi_cursor`.

[Origin Mode](origin-mode.md) is also included in saved cursor state. Restoring
a saved row clamps it against the current margins when that mode is enabled.

[Automatic Wrap Mode](auto-wrap.md) is also saved and restored with the cursor.

[Character-set designations and invocation](character-sets.md) are included in
saved cursor state.

`cargo test --test ansi_cursor` covers mixed save/restore forms, shared-slot
replacement, saved modes/style/wrap, repeated restore, main/alternate isolation,
resize, RIS and rejection of parameterized forms, at every input split. The real
PTY suite checks restored coordinates through DSR, including a DEC save followed
by an ANSI restore.
