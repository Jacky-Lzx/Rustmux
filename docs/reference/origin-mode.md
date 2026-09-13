# Origin Mode and Positioning

DECOM selects the origin used by terminal cursor addressing. With mode 6 enabled,
row 1 is the scrolling region's top row and the cursor cannot leave that region.
With mode 6 disabled (the default), row 1 is the screen's top row. See the
[DEC description](https://vt100.net/docs/vt510-rm/DECOM.html).

| Sequence | Effect |
| --- | --- |
| CSI ? 6 h / l | Enable / disable origin mode and home the cursor |
| CSI row ; column H / f | Position relative to the active origin |
| CSI n G / CSI n ` | CHA/HPA: set column, retaining the physical row |
| CSI n d | VPA: set row relative to the origin, retaining the column |
| CSI n E / F | CNL/CPL: move down/up and return to column zero |

Coordinates and counts are one-based; omitted or zero parameters mean one.
Extra parameters, colon groups and unsupported private forms are ignored.
Large coordinates clamp to the active bounds without scrolling or overflow.
Vertical relative movement retains the scrolling-margin behavior described in
[Scrolling Regions](scrolling-regions.md). Successful positioning cancels wrap.

Changing origin mode homes even if the requested value is already active.
Setting valid scrolling margins homes at the active origin. Text and writing
style are unaffected. The Screen API distinguishes physical `move_to` from
origin-relative `position`; `cursor()` always reports physical coordinates.
The renderer continues to emit physical row positions.

## Saved state and resize

DECSC/DECRC saves and restores origin mode together with coordinates, style and
pending wrap. Scrolling margins themselves are not saved by these commands.
If changed margins clamp a restored physical row, pending wrap is discarded.

Mode 1049 entry retains the current origin mode on the fresh full-height
alternate grid. Exit restores the saved main mode and coordinates along with
the main margins. Actual resize retains current and saved origin modes but
resets both scrolling regions to full height; saved positions still clamp.
Same-size and failed resizes remain no-ops.

## Verification and remaining work

Run `cargo test --test origin_mode`. Tests cover chunk boundaries, defaults,
clamping, repeated mode changes, relative/absolute positioning, malformed
commands, saved state, alternate switching, resize, one-cell grids and renderer
replay. The real CLI PTY fixture verifies visible row placement with DECOM,
VPA and CHA, followed by returning to screen-relative coordinates.

Horizontal margins and queries beyond standard DSR remain future work.
