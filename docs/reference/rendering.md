# Renderer

`rustmux::render::render(&screen, &mut output)` writes a complete ANSI frame for
the active screen. The caller supplies any `std::io::Write`, such as a byte
buffer. The renderer borrows the model without changing it and does not flush.
The CLI uses `Renderer::render` with a bounded frame queue in its event loop.
This stateful API compares rows with the last successfully queued frame and
paints changed cell spans; `render` remains the standalone full-frame API.
Renderer remembers the last queued paste, cursor-key, keypad, cursor-shape,
focus-reporting and mouse states. It synchronizes each only on the first frame,
a change or cache invalidation. The standalone `render` function always includes
that synchronization. See [Focus Reporting](focus-reporting.md). A Renderer is
specific to one ordered output stream; recreate it if queued output is discarded.

## Output

A standalone full frame hides the cursor, resets attributes and synchronizes the outer
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

## Changed-cell rendering

Renderer keeps a copy of the last active grid, comparing complete cells including
style, width and combining suffixes. Identical rows emit no drawing commands.
Within a changed row, contiguous differing cells form spans. Span boundaries
expand to include complete old and new wide glyphs, and overlapping/touching
spans merge. This ensures replacement of both halves and never starts output on
a wide trailing placeholder. Blank cells and changed combining suffixes are
written explicitly. Cursor position, shape,
visibility and supported input modes are still synchronized even when no rows
change. The model still uses full-grid comparison, not per-cell dirty flags.

For each changed row, Renderer encodes the span candidate and counts the bytes
of a whole-row candidate, including CUP, SGR, UTF-8 and combining suffixes from
the current output style. It chooses spans only when strictly smaller; ties use
the whole row. A completely changed row goes straight to whole-row output.
Before comparing against the whole row, each unchanged gap is considered for
bridging: compare rewriting its complete cells and switching to the next span's
first style against CUP plus switching directly to that style. Bridge only when
strictly cheaper; ties retain separate spans. Both alternatives reach the same
next cell with the same style, so the rest of that span has identical cost.
This includes UTF-8, combining suffixes and SGR transitions, rather than using a
fixed gap-length threshold. For example, three unchanged ASCII letters may cost
less than another cursor move, while one differently colored cell may cost more.
The spans already end/start at complete glyph boundaries, so bridging cannot
split a wide character. Each gap is counted once, keeping planning linear in the
row width. This remains a per-row decision, not a globally optimal frame plan.
Style state is updated according to the selected candidate.

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
Range metadata and candidate bytes are temporary and limited to one row at a
time. They add allocation/planning work; byte reduction is not a guarantee of
lower CPU time. See the [measured comparison](rendering-performance.md).

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

`cargo test --test cell_render` verifies exact span positioning, separated edits,
whole-row fallback, old/new wide-glyph overlap, combining suffixes, style-only
changes, blank erasure and 600 deterministic edits replayed after every frame.
The PTY decoder tracks cell columns (including the CJK/combining fixtures) and
retains untouched cells within each row.

Gap-bridging tests also cover chains of short gaps, equal-cost separation,
expensive RGB transitions, and unchanged wide/combining glyphs inside a gap.

## Output mode cache

Ordinary incremental frames omit unchanged mode commands. Text-only changes do
not resend paste, keyboard or cursor-shape settings. Mode-only changes are still
emitted even when all cells are identical. The cache advances only after the
entire render call succeeds; every write error invalidates both grid and mode
state. `invalidate()` forces complete synchronization on the next call. Merely
changing Screen dimensions repaints the grid; the CLI also explicitly invalidates
on resize. The standalone `render` function always synchronizes every mode.

Cursor hiding, SGR resets and final cursor position/visibility remain in every
frame. They provide the painting boundaries and are not treated as persistent
modes. The renderer requires exclusive ownership of its ordered output stream;
after external mode changes or lost output, invalidate it before drawing again.

`cargo test --test render_modes` checks unchanged-frame bytes, text-only updates,
mode changes and resets replayed into a screen, and failure at every byte boundary
of a mode-changing frame followed by full synchronization.

The window rename prompt renders a temporary screen clone through the same
Renderer. It replaces the reserved window-bar row without changing the child model. Entering
and leaving the prompt invalidates the renderer so display modes are synchronized
with the editor or the active child respectively.

The CLI composes a reserved bottom window-bar row below the child grid. Its
physical frame dimensions include that row, while each PTY receives the reduced
content height. The standalone renderer API still paints exactly its supplied
grid and does not add UI rows. See [Windows](windows.md#window-bar-and-content-area).
