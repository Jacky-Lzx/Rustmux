# Alternate Screen

The model supports private modes 47, 1047 and 1049 for selecting the alternate
screen. Mode 47 preserves its contents across visits. Mode 1047 clears the
alternate display when returning to the main screen. Mode 1049 saves the main
cursor state, clears the alternate screen on entry, and restores the main state
on exit. These are the modes described in the
[XTerm reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).
The CLI parses these switches and renders the active model grid.

`Screen` allocates two equal-sized grids at construction. Modes 47 and 1047
preserve the inactive grid and carry the current coordinates and writing state
into the selected buffer while cancelling pending wrap. Mode 1049 instead
snapshots the main cursor state and clears the selected alternate grid using the
active background, then restores that snapshot on exit. Applications can
explicitly home the cursor with CUP. Reads and edits target the active grid;
`is_alternate` reports which one is active.

Mode 1049 exit restores main content and saved state, discarding alternate
content and combining suffixes. Its next entry starts blank. Modes 47 and 1047
do not implicitly save or restore cursor state; coordinates and writing state
therefore carry across the switch. Repeated entry or exit through any alias is a
no-op: the alternate screen is not a nested stack. Switching reuses allocated
storage and cannot fail allocation; creating a Screen reserves two grids.

The parser accepts a leading private-mode `?` marker and processes all three in
semicolon-separated h/l mode lists. Unknown modes in those lists are ignored.
Other private commands remain unsupported; malformed prefixes, parameter overflow
and intermediates invalidate the command as before. Standalone mode 1048 shares
each screen's explicit cursor-save slot;
[Cursor State](cursor.md) describes its interaction with ESC 7/8 and CSI s/u.
[Screen Model Resize](screen-resize.md) adjusts both grids and the saved cursor.
Saved cursor state includes coordinates, writing style, pending wrap, origin and
automatic-wrap modes, and G0/G1 character-set state.

`tests/alternate_screen.rs` checks each mode's content and cursor rules, restoration
after scrolling/erasing, Unicode cell isolation, repeated toggles, unsupported
modes and every two-chunk split plus byte-at-a-time input. Run
`cargo test --test alternate_screen`.
