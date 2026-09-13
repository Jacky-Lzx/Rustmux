# Renderer

`rustmux::render::render(&screen, &mut output)` writes a complete ANSI frame for
the active screen. The caller supplies any `std::io::Write`, such as a byte
buffer. The renderer borrows the model without changing it and does not flush.
The CLI uses `Renderer::render` with a bounded frame queue in its event loop.
This stateful API compares rows with the last successfully queued frame and
paints only changed rows; `render` remains the standalone full-frame API.
Renderer remembers the last queued focus-reporting and mouse modes and synchronizes them only
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

## Changed-row rendering

Renderer keeps a copy of the last active grid, comparing complete cells including
style, width and combining suffixes. A changed row is replaced in full, including
blank cells; identical rows emit no drawing commands. Cursor position, shape,
visibility and supported input modes are still synchronized even when no rows
change. This is row-level comparison, not a per-cell damage tracker.

First render and dimension changes repaint every row. The CLI calls
`Renderer::invalidate` after valid resize notifications, including unchanged
reported dimensions. Call it after external screen damage or discarded output.
A rendering error invalidates the cache automatically, since partly written rows
may no longer match either model. Successful frames must be delivered in order.

Only changed rows are copied into the cache during ordinary updates. Comparison
still scans the whole active grid; the optimization reduces terminal bytes rather
than making model comparison constant-time. The CLI's 65,536-cell limit bounds
the cached grid. Dimension changes discard old row buffers. No scrollback or
inactive-grid snapshot is retained by Renderer.

The CLI keeps the frame size cap and scheduling described in
[Input and Rendering Loop](input-loop.md), including synchronized-output pauses.
It does not send terminal scroll commands or guarantee physical atomic display.

## Verification

`tests/render.rs` checks exact ANSI bytes for a full grid, style reset and Unicode,
then replays frames to check stale-content replacement, alternate grids and resize.
A short writer checks retryable interruptions and error propagation. Run
`cargo test --test render`. These tests validate emitted output, not a live
terminal's visual appearance.

`cargo test --test incremental_render` checks one-row updates, cursor-only
frames, style-only changes, wide/combining text, erase, scroll, alternate grids,
resize, invalidation and recovery after partial output. In a 24x80 fixture,
one changed row emits less than one fifth of the full-frame bytes. The PTY suite
retains unchanged rows when decoding partial frames and checks that a real CLI
one-row update leaves another row intact without re-emitting it.
