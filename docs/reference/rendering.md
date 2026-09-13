# Basic Renderer

`rustmux::render::render(&screen, &mut output)` writes a complete ANSI frame for
the active screen. The caller supplies any `std::io::Write`, such as a byte
buffer. The renderer borrows the model without changing it and does not flush.
The CLI uses `Renderer::render` with a bounded frame queue in its event loop.
Renderer remembers the last queued focus-reporting mode and synchronizes it only
on the first frame or a change. The standalone `render` function always includes
that synchronization. See [Focus Reporting](focus-reporting.md). A Renderer is
specific to one ordered output stream; recreate it if queued output is discarded.

## Output

The frame hides the cursor, resets attributes and synchronizes the outer
terminal’s [bracketed paste mode](bracketed-paste.md) and
[application cursor keys](application-cursor.md) and
[application keypad](application-keypad.md), then positions explicitly
at the start of each row. It also emits the [cursor shape](cursor-shape.md)
while the cursor is hidden. All cells, including spaces, are drawn to remove stale
content. Wide trailing placeholders are skipped; leaders and their stored
zero-width suffixes are encoded once as UTF-8. Style changes start with SGR reset,
then encode the enabled attributes and indexed/RGB colors. Adjacent equal styles
share the same SGR state.

The frame ends with an attribute reset, an explicit cursor move to the model
position and the model's cursor visibility. It uses no newlines, avoiding a line-feed scroll
at the bottom-right corner. Physical delayed wrap is cancelled by the final
cursor move; the model's logical pending wrap remains unchanged.

The active grid is drawn regardless of main/alternate mode. The renderer does
not switch the outer terminal's screen buffer; terminal setup owns that decision.

## Caller responsibilities and limits

Use a terminal matching the grid's dimensions, with normal origin mode, the full
scrolling region and compatible Unicode width rules. The current model's emoji
and grapheme limitations still apply. Cursor visibility at frame end follows [Cursor State](cursor.md).

The caller owns raw mode, alternate-screen setup, restoration and output queuing.
Write failures are returned and may leave a partial frame or hidden cursor; the
caller must restore terminal state or redraw. For nonblocking output, first
render into a buffer and queue its bytes rather than restarting rendering after
a partial write. Short writes and Interrupted are handled by Write's write_all
path. The renderer does not retry WouldBlock or flush.

This first version redraws every cell. The CLI schedules and queues frames as
described in [Input and Rendering Loop](input-loop.md). Changed-cell rendering
is subsequent work.

## Verification

`tests/render.rs` checks exact ANSI bytes for a full grid, style reset and Unicode,
then replays frames to check stale-content replacement, alternate grids and resize.
A short writer checks retryable interruptions and error propagation. Run
`cargo test --test render`. These tests validate emitted output, not a live
terminal's visual appearance.
