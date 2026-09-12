# Alternate Screen

The model supports `CSI ? 1049 h` to enter and `CSI ? 1049 l` to leave the
alternate screen. This is the save/switch/restore mode described in the
[XTerm reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).
The CLI parses these switches and renders the active model grid.

`Screen` allocates two equal-sized grids at construction. Entry preserves the
main grid and saves its cursor coordinates, current writing style and pending
wrap. The alternate grid is cleared using the active background. Coordinates and
writing style carry over on entry, while pending wrap is cleared. Applications
can explicitly home the cursor with CUP. Reads and edits target the active grid;
`is_alternate` reports which one is active.

Exit restores main content and saved state, discarding alternate content and
combining suffixes. The next entry starts blank. Repeated entry or exit is a
no-op: mode 1049 is not a nested screen stack. Switching reuses allocated storage
and cannot fail allocation; creating a Screen now reserves two grids.

The parser accepts a leading private-mode `?` marker and processes 1049 in
semicolon-separated h/l mode lists. Unknown modes in those lists are ignored.
Other private commands remain unsupported; malformed prefixes, parameter overflow
and intermediates invalidate the command as before. Modes 47, 1047 and standalone
1048 and general cursor save/restore are later work.
[Screen Model Resize](screen-resize.md) adjusts both grids and the saved cursor.
This implements the currently modeled subset of saved cursor state, not charset
or origin modes that the model does not yet support.

`tests/alternate_screen.rs` checks clearing, style/cursor preservation, restoration
after scrolling/erasing, Unicode cell isolation, repeated toggles, unsupported
modes and every two-chunk split plus byte-at-a-time input. Run
`cargo test --test alternate_screen`.
