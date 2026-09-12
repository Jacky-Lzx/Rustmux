# Screen Model Resize

`Screen::resize(rows, columns)` resizes the active and inactive grids together.
This is separate from `PtyShell::resize`, which changes the operating system PTY
size. The CLI now applies both operations when the outer terminal changes size.

## Policy

The top-left overlap is retained, including styles and combining suffixes.
There is no text reflow: shrinking discards right-hand columns and bottom rows;
growing later cannot recover them. New cells use the relevant grid's writing
background with default foreground and no decorations. While alternate is active,
the saved main style supplies the main grid's background.

A wide character cut in half at the right edge is replaced by a blank, so no
orphan leader or continuation remains. Both current and saved main cursor
coordinates clamp to the new bounds. Any actual dimension change clears both
pending-wrap flags. Resizing to the same dimensions changes nothing.

Alternate mode remains active across resize. Leaving it restores the resized main
grid and the clamped saved cursor/style. Re-entering still starts a blank alternate
grid at the new dimensions.

## Failure and storage

Zero dimensions, size overflow and reported allocation failures return an error
without changing the model. Both destination grids are allocated before moving
any old cells. Cell contents, including suffix allocations, are moved into the
new grids without cloning. Old and new grids coexist briefly, so resize requires
more peak memory than steady-state storage. As with other allocations, process
termination by the OS under memory pressure cannot be recovered here.

## Verification

`tests/screen_resize.rs` checks growth, shrink/grow data loss, style/suffix
preservation, clipped wide characters, alternate/main restoration, clamped
cursors, pending-wrap policy and unchanged state on invalid dimensions or a
capacity-limit error. Run `cargo test --test screen_resize`.
