# Kitty Graphics Input

The first Kitty graphics increment is a bounded stream framer, not image
display. `GraphicsFramer` recognizes complete ESC _ G … ESC \ commands (and
their 8-bit APC/ST form) across arbitrary PTY read boundaries. It emits graphics
commands and ordinary terminal bytes as ordered events. Other APCs pass through
unchanged. A UTF-8 continuation byte cannot be mistaken for an 8-bit APC.

Each retained command is limited to 16 KiB, enough for Kitty's documented
4 KiB encoded direct-data chunk plus control fields. An oversized, cancelled,
incorrectly terminated or unfinished graphics command is discarded without exposing its
payload as terminal text. The framer does not concatenate separate Kitty
transfer chunks; each APC is one event.

`DirectTransferAssembler` is the next, separate data boundary. It parses
complete APC G commands, decodes Base64 direct-data chunks, and combines them
until `m=0`. Subsequent chunks may contain only `m` and optional `q`; an invalid
or interrupted transfer is discarded, and the next independent transfer can
start cleanly. It preserves the first chunk's control fields and the final
chunk's optional `q` override. It checks the byte count for raw RGB/RGBA data;
PNG bytes remain opaque until a later image decoder validates them.

One encoded chunk is limited to 4096 bytes and one assembled transfer to
16 MiB. Only uncompressed direct data (`t=d`, `a=t/T/q`) is accepted by this
assembler. File, temporary-file and shared-memory media are not read, and
compressed data is not decompressed. The chunk and continuation rules follow
the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

Every pane now passes PTY output through its own framer before the text parser
and OSC observer. Graphics bytes cannot appear as screen text or shell-command
output; the hidden close/undo path uses the same pane pipeline. Callers can opt
in to complete direct transfers with `Pane::process_output_with_graphics`.
An additional opt-in path, `Pane::process_output_with_image_store`, retains
complete transfers with explicit nonzero `i` IDs in a pane-local `ImageStore`.
Replacement and explicit removal update byte accounting; oldest entries are
evicted at 32 MiB or 256 images per pane. The store rejects query action `a=q`
and image-number allocation `I`; the ordinary store path preserves PNG bytes
without decoding them.
It is cleared when a stopped foreground job resets its pane or its PTY reaches
EOF.

`StoredImage::decode_rgba()` now provides an optional, bounded pixel-decoding
boundary. It converts raw RGB/RGBA and static PNG (including paletted PNG) to
eight-bit RGBA with dimensions. The decoder checks PNG integrity and optional
declared dimensions, rejects animation, and caps each decoded pixel buffer at
32 MiB. PNG decoding also uses a 32 MiB internal-allocation limit. It does not
cache the decoded pixels or render them; callers must opt in to decoding.
Color-profile conversion is not yet implemented.

The opt-in store now tracks placement *references* for `a=T` and a strict
`a=p,i=<id>[,p=<id>]` subset. A named placement replaces the same `(i,p)`
reference; an absent or zero `p` creates an anonymous reference. Re-transmitting
an image ID drops its old references. The supported delete subset is
`a=d,d=i/I,i=<id>[,p=<id>]`: lowercase removes matching references but keeps
data, while uppercase also releases data once no references remain. Image
eviction drops its references. There are at most 1024 references per pane.
These rules follow the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/),
but the model does not track visibility or the other delete selectors.

For the pane opt-in path, cursor-anchored placements now record a zero-based
cell row and column, plus whether they belong to the alternate screen. A
chunked `a=T` records the cursor when its final chunk arrives; `a=p` records
the cursor at that command. Source pixel rectangle keys `x/y/w/h`, first-cell
pixel offsets `X/Y`, explicit `c`/`r` cell extents, signed `z` index, and
`C=1` no-move request are parsed and retained. The source rectangle intersects
the decoded image; an omitted or zero `w/h` selects the remaining width/height.
Missing extents remain unknown on the ordinary opt-in path. Virtual and
relative placements do not get a cursor anchor. This is metadata only: image
placement does not yet
compose pixels, redraw, or send graphics replies.

In the opt-in pane path, a successful `a=T` or `a=p` placement with both
resolved `c` and `r` moves the cursor right by `c` cells and down by `r`
cells before subsequent text is parsed. `C=1` suppresses the move. The screen
model clamps an out-of-bounds destination, which the protocol leaves undefined.
Without supplied pixel-cell geometry, a missing extent leaves the cursor
unchanged; failed, virtual, or non-placement commands also do not move it.
The normal runtime still discards these commands and leaves its cursor alone.
This follows the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

`Pane::process_output_with_image_store_sized` is a separate opt-in path for a
caller that has verified nonzero physical cell width and height in pixels.
It validates and caches each image's decoded dimensions, then derives missing
`c`/`r` using ceiling cell coverage of the intersected source rectangle and
its aspect ratio when only one extent is specified. The resolved extent is
stored with the placement and used for cursor motion and later row-shift
bookkeeping. When inference is needed, invalid image data, an empty source
intersection, or an unrepresentable
computed extent leaves the original metadata and cursor unchanged. Replacing
or evicting an image invalidates its dimension cache.

The placement also retains the original sizing intent separately from the
resolved cell extent: neither `c` nor `r` (or zero values) means natural pixel
size, only `c` means fit width, only `r` means fit height, and both mean fit
within the specified cell box. This distinction survives cell-extent inference
so a future pixel renderer can choose the correct scaling behavior; it does
not render or scale pixels yet.

`PlacementGeometry::pixel_layout` now provides the next opt-in, pure layout
step when a caller supplies decoded image dimensions and a verified physical
cell size. It intersects the source crop, computes the current cell rectangle,
and returns the scaled content rectangle relative to the anchor cell. Natural
placements keep their source pixel size; a single requested axis sets that
pixel dimension and derives the other from the source aspect ratio. A two-axis
box preserves aspect ratio and centers the content in any letterbox or
pillarbox space. `X/Y` shift the content origin without enlarging the cell
rectangle. The calculation recomputes inferred extents for the supplied cell
size and returns no layout for an empty crop, invalid offset, or overflowing
pixel geometry. It does not decode, resample, clip, or composite pixels.

After decoding, `DecodedImage::resample_placement` can now produce the cropped
RGBA content for that layout with bounded nearest-neighbor sampling. It keeps
the content's destination offset and alpha bytes, but does not allocate empty
letterbox space. Invalid pixel buffers or rectangles are rejected, and output
is capped at 32 MiB. Nearest-neighbor is this initial implementation choice,
not a Kitty protocol requirement. `ResampledPlacement::clip_to_viewport` then
accepts a signed anchor-cell pixel position and pane viewport dimensions.
`PlacementGeometry::pixel_anchor` derives that position from its cell anchor,
tracked row displacement, and verified physical cell size. Clipping returns
only the visible RGBA rows and columns in viewport coordinates.
Negative positions after scrolling and fully off-screen placements are handled
without unsigned wraparound. The opt-in
`clip_to_viewport_with_scroll_clip` path also applies the stored permanent
top/bottom cell-row clips before extracting visible pixels. It limits only
edges that were actually clipped, so an `X/Y` offset is not mistaken for an
extra placement row. `compose_image_layers` can now blend these clipped RGBA
images onto a transparent pane-sized canvas. It orders by `z`, then image ID
(lower values underneath), with stable input order for the protocol's undefined
equal-key tie. The output is capped at 32 MiB, cumulative layer input at
64 MiB, and the layer count at the pane's 1024-placement limit. Negative `z`
is ordered among images, but its relationship to text and cell backgrounds is
not composed yet. Redraw and the normal runtime are unchanged. The ordering
follows the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

`X/Y` position an image within its first cell; they are not added to `c/r` or
cursor movement. On a sized opt-in call, either offset must be smaller than
its cell pixel dimension. An invalid offset rejects the placement before an
cursor-anchored `a=T` replacement or `a=p` reference can mutate the store.
Without supplied cell pixels the offsets are retained but cannot yet be
range-checked.

`CellPixelSize::from_terminal_size` accepts reported terminal rows, columns and
pixel dimensions only when they describe an exact, nonzero cell grid. The
opt-in `Pane::process_output_with_image_store_for_terminal` uses that check;
zero or non-divisible pixel dimensions fall back to the unsized store behavior
without guessing through window padding. The supplied size describes the
outer terminal, not one split pane.

The local/detached runtime does not yet propagate trustworthy pixel cell size
to panes, so it does not use this path automatically. The detached client now
transports reported terminal pixel dimensions to the server frontend, but pane
cell sizes are not derived from them yet; no size is guessed from row/column
counts alone. This follows the sizing rules in the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

The opt-in pane path now removes cursor-anchored placement references when
`CSI 2 J` clears their screen, when RIS resets both screens, and when an
alternate screen is cleared by mode 1049 entry/exit or mode 1047 exit.
Mode 47 preserves its alternate placements on exit; other text erasures do
not clear graphics. Stored image data remains available for a later `a=p`.
Unanchored virtual/relative references are not classified as visible. These
clear rules follow the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

Cursor-anchored placements now follow physical vertical row shifts caused by
line feed/wrap, reverse index, `CSI S/T`, and insert/delete lines. The screen
model passes bounded, ordered row-shift events to the pane store. Explicit `r` height
allows permanent top/bottom clipping at scroll margins; placements crossing a
margin before a shift stay in place. Full-screen main-buffer upward scrolling
retains references as they enter scrollback, until their known row extent falls
out of retained history. If height is unknown, only full-screen shifts move its
anchor; its visibility and expiry cannot yet be determined. An overflowing
event batch safely drops anchored references but keeps image data. Horizontal
shifts, resize/reflow relocation, automatic on-screen composition, and
scrollback rendering remain out of scope. These choices follow the scrolling
rules in the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

The normal runtime still discards graphics commands without assembling or
retaining image data. It does not place, redraw or delete visible images,
answer graphics capability queries, or claim Yazi preview compatibility.
Those are later review increments.

Unit tests cover every two-chunk split of a command, ordinary output ordering,
non-graphics APCs, UTF-8/C1 ambiguity, oversized and cancelled commands, and
EOF recovery. Assembler tests cover chunk inheritance, raw byte counts, bounds,
unsupported media and recovery. Pane tests cover interleaved text, per-pane
isolation and command-output filtering. Run `cargo test --lib graphics::tests`,
`cargo test --lib graphics_transfer::tests` and `cargo test --test panes`.
Store tests also cover replacement, isolation, eviction, explicit removal,
named and anonymous references, and soft versus hard deletion.
Decoder tests cover raw and PNG formats, palette transparency, corrupted PNGs,
dimension checks and the output-size bound. Resampling tests cover cropped
nearest-neighbor enlargement, reduction, alpha preservation, invalid geometry,
and the raster output limit. Viewport tests cover all four clipped edges,
negative and disjoint anchors, malformed buffers, and oversized geometry.
Scroll-clip tests verify top/bottom pixel rows and a pane-driven margin scroll.
Composition tests cover `z`/image-ID order, straight-alpha blending, offsets,
malformed layers, and output/work limits.
Anchor tests cover interleaved text, final-chunk position, alternate-screen
identity, explicit layout options, malformed metadata, and named replacement.
Screen-lifecycle tests cover split `CSI 2 J`, non-clearing text erasures,
alternate-buffer transitions, RIS, and commands after a clear in the same read.
Scroll tests cover full-screen primary/alternate movement, retained scrollback
references, margin clipping, reverse index, unknown height, and event overflow.
Cursor tests cover `a=T` and `a=p` ordering, final-chunk anchoring, `C=1`,
unknown extents, failed placements, and bounded destinations.
Sized-path tests cover RGB and PNG dimensions, aspect-ratio inference,
source cropping, first-cell pixel offset validation, invalid PNG recovery,
exact terminal-cell validation, and the unchanged unsized path.
