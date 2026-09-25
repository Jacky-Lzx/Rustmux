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
complete transfers with explicit nonzero `i` IDs or numbered `I` uploads in a
pane-local `ImageStore`.
The running multiplexer now uses that bounded store for live, detached and
temporarily closed/undoable panes; the public `Pane::process_output` method
still discards graphics unless its caller opts in.
Replacement and explicit removal update byte accounting; oldest entries are
evicted at 32 MiB or 256 images per pane. The store rejects query action `a=q`;
the ordinary store path preserves PNG bytes without decoding them.
An upload with `I=<nonzero-number>` and no `i` receives the smallest free
positive image ID, reported as `i=<assigned>,I=<number>` when replies are
enabled. Repeating a number creates another image rather than replacing its
predecessor. Existing explicit IDs are never overwritten by allocation;
`a=p,I=<number>` now places the newest live image created with that number.
Older images remain available through their assigned `i` values; removing the
newest image falls back to the next newest. Replacing an older numbered image
by its `i` retains its number without changing this creation order. The
`d=i/I` delete selectors still require `i`; `d=n/N` is not yet implemented.
Specifying both identity keys, or a zero image number, leaves the store unchanged.
It is cleared when a stopped foreground job resets its pane or its PTY reaches
EOF.

`StoredImage::decode_rgba()` now provides an optional, bounded pixel-decoding
boundary. It converts raw RGB/RGBA and static PNG (including paletted PNG) to
eight-bit RGBA with dimensions. The decoder checks PNG integrity and optional
declared dimensions, rejects animation, and caps each decoded pixel buffer at
32 MiB. PNG decoding also uses a 32 MiB internal-allocation limit. It does not
cache the decoded pixels or render them; callers must opt in to decoding.
Color-profile conversion is not yet implemented.

The running multiplexer validates PNG data before replacing a pane image.
Corrupt, unsupported, oversized-decoded, or dimension-mismatched PNG transfers
leave the existing image, placements, revision, and cursor unchanged. Successful
PNG dimensions are cached for sized placement inference, avoiding a second
decode at insertion time. Raw transfers were already byte-count checked by the
assembler. The public opt-in image-store methods retain their earlier
deferred-decoding behavior for callers that only need the original bytes.

The opt-in store now tracks placement *references* for `a=T` and a strict
`a=p` subset with either `i=<id>` or `I=<number>`, and optional `p=<id>`.
A named placement replaces the same `(i,p)`
reference; an absent or zero `p` creates an anonymous reference. Re-transmitting
an image ID drops its old references. The supported ID-delete subset is
`a=d,d=i/I,i=<id>[,p=<id>]`: lowercase removes matching references but keeps
data, while uppercase also releases data once no references remain.
The pane and low-level store paths also accept
`a=d,d=r/R,x=<first-id>,y=<last-id>`.
This selects the inclusive image-ID range across both screens and scrollback,
regardless of placement geometry; it needs no cursor or viewport. Lowercase
removes references but retains data. Uppercase also releases every matching
unreferenced image, including data-only uploads. Bounds are unsigned 32-bit
values (`x=0` is valid); a reversed range is a no-op. Missing, malformed,
overflowing, or extra controls leave the store unchanged.
The pane path also accepts `a=d` (default `d=a`) and explicit `d=a/A` with no ID: these
remove modeled placement rectangles intersecting the current screen. Lowercase
retains data; uppercase frees data only for images with no remaining references.
The other screen, fully off-screen scrollback placements, and unanchored
references are untouched. This viewport-dependent selector is unavailable to
the low-level store APIs that have no screen dimensions. Image eviction drops
its references. There are at most 1024 references per pane. These rules follow
the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

The pane path also accepts `a=d,d=c/C` with no image ID. It removes each
cursor-anchored placement whose modeled cell rectangle covers the current
cursor cell on the active screen. Lowercase retains image data; uppercase
releases it only after the image has no remaining references. Neither form
moves the cursor. Off-screen history, the other screen, and unanchored or
unresolved-size placements are left untouched. The low-level store API with
an explicit cursor anchor supports this selector, but the anchorless API does
not. Extra keys for `d=c/C` are rejected rather than silently broadening the
match.

The pane path also accepts `a=d,d=p/P,x=<column>,y=<row>`. Both coordinates
are required, one-based (the top-left cell is `x=1,y=1`), and must lie within
the active screen. Invalid or extra controls leave the store unchanged. The
selector removes modeled placements intersecting that cell, independently of
the current cursor position; lowercase keeps image data and uppercase releases
it only after the last reference disappears. It does not move the cursor or
touch another screen, fully off-screen history, or unresolved-size placements.
The low-level APIs without a viewport do not accept this selector.
The pane path also accepts `a=d,d=q/Q,x=<column>,y=<row>,z=<layer>`.
It requires the same valid one-based screen cell and a signed 32-bit `z`;
only placements at that z-index intersecting the cell are removed. Lowercase
retains image data; uppercase releases data only when no references remain.
Invalid or extra controls leave the store and cursor unchanged. The other
screen, fully off-screen history, and unresolved-size placements are untouched.
The low-level APIs without a viewport do not accept this selector.
`a=d,d=z/Z,z=<layer>` removes every modeled placement at the specified signed
32-bit z-index on the active screen, including its scrollback, but not the other
screen. Unlike the cell-scoped selectors, it requires a screen identity but no
viewport or known cell extents; unanchored placements with no recorded z-index
are not matched. The anchorless low-level API does not accept this selector.
Lowercase retains image data; uppercase releases it only after its last
reference disappears. A missing, malformed, or out-of-range z-index, or an
extra control key, leaves the store unchanged.
The pane path also accepts `a=d,d=x/X,x=<column>` for every modeled placement
intersecting a one-based column on the active screen or its scrollback. The
column must be within the viewport; missing, malformed, out-of-range, or
extra controls leave the store unchanged. Lowercase keeps image data and
uppercase releases it only after the final reference disappears. The other
screen, unanchored references, and placements with unknown width are untouched;
the row extent need not be known.
`a=d,d=y/Y,y=<row>` similarly selects a one-based row on the active screen.
It matches placements whose modeled, scroll-clipped portion intersects that row,
including placements partly scrolled above the screen. Fully off-screen
scrollback, the other screen, unanchored references, and placements with
unknown height are untouched; the column extent need not be known. The row
must be within the viewport. Lowercase retains image data; uppercase releases
it only after the last reference disappears. Missing, malformed, out-of-range,
or extra controls leave the store unchanged. The low-level APIs without a
viewport accept neither the column nor row selector. Other delete selectors
remain unimplemented.

For the pane opt-in path, cursor-anchored placements now record a zero-based
cell row and column, plus whether they belong to the alternate screen. A
chunked `a=T` records the cursor when its final chunk arrives; `a=p` records
the cursor at that command. Source pixel rectangle keys `x/y/w/h`, first-cell
pixel offsets `X/Y`, explicit `c`/`r` cell extents, signed `z` index, and
`C=1` no-move request are parsed and retained. The source rectangle intersects
the decoded image; an omitted or zero `w/h` selects the remaining width/height.
Missing extents remain unknown on the ordinary opt-in path. Virtual and
relative placements do not get a cursor anchor. The opt-in store records
metadata; runtime composition, redraw, and child replies are described below.

In the opt-in pane path, a successful `a=T` or `a=p` placement with both
resolved `c` and `r` moves the cursor right by `c` cells and down by `r`
cells before subsequent text is parsed. `C=1` suppresses the move. The screen
model clamps an out-of-bounds destination, which the protocol leaves undefined.
Without supplied pixel-cell geometry, a missing extent leaves the cursor
unchanged; failed, virtual, or non-placement commands also do not move it.
The runtime applies this modeled motion for supported placements and displays
all three pixel stacking bands described below.
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
not composed yet. The compositor is not called by normal redraw. The ordering
follows the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

`Pane::compose_image_snapshot` now connects those opt-in stages for the current
screen. Given a verified physical cell size, it derives the pane's pixel
viewport, decodes each cursor-anchored image, recalculates placement layout,
resamples and scroll-clips it, then blends the visible layers. Placements on
the other screen and unanchored references are omitted. Invalid selected-screen
image data or geometry fails the whole snapshot rather than returning a partial
image. The returned canvas is transparent where no image is placed; the store
is unchanged. It remains image-only and is not called by the normal renderer.

`Pane::compose_image_planes` provides an alternate opt-in snapshot that keeps
Kitty's three stacking bands separate: `z < -1073741824` is behind non-default
cell backgrounds, `-1073741824 <= z < 0` is behind text but above backgrounds,
and `z >= 0` is above text. Each populated band is independently composed in
image z/image-ID order; absent bands allocate no canvas. The three outputs
share the existing 64 MiB clipped-input budget and a new 64 MiB combined
canvas budget. No cell colors, glyphs, or outer-terminal graphics commands are
drawn by this snapshot function. The runtime composes each band separately so
an invalid or over-budget band does not suppress the others. The
bands follow the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

`ImageStore::revision` now changes when retained image data or placement
metadata changes, including scroll shifts and screen clears, but not for
rejected commands or no-op deletions. The value belongs to one store instance;
it is a runtime invalidation hint, not a persistent image identity. Unchanged
revisions avoid retransmitting the same pane's visible bands.

`write_kitty_rgba_placement` is an opt-in output encoder for a caller that has
already established outer-terminal Kitty support and positioned its cursor.
It validates one decoded RGBA image before writing, then streams `a=T,f=32`
direct-data APCs with a nonzero image ID, requested `z`, `C=1`, and `q=2`.
Each Base64 chunk is at most 4096 bytes; all continuation APCs carry only
`m`. The encoder itself does not manage replacement/deletion; the runtime
calls it only after preflighting the remaining 16 MiB frame budget. An image
too large for that frame is skipped. A failed write may leave a partial
transfer. The chunk format follows the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

`kitty_rgba_placement_len` now preflights the exact encoded byte count,
including every APC wrapper, without allocating a Base64 image. The bounded
`write_kitty_rgba_placement_with_limit` rejects a placement before writing
when its complete transfer would exceed the caller's remaining output budget.
The runtime uses this preflight before each band upload.

`GraphicsCapabilityProbe` is a separate opt-in outer-terminal detection
boundary. It generates a one-pixel, direct-RGB `a=q` query followed by primary
device attributes. Its bounded incremental filter consumes the matching
graphics reply and DA reply while forwarding unrelated input unchanged.
A matching graphics reply (including an error) before DA means supported; DA
first means unsupported. Timeout or disconnect before either reply is
inconclusive, and `finish` releases any unfinished input candidate. The caller
must reserve the query image ID and avoid a concurrent primary-DA request.
The server now queues this query once per local run or client attachment,
before its first rendered frame. It filters only newly received input,
preserving queued user bytes; it records the result per attachment and resets
it on reconnect. After 500 ms without a DA barrier, it releases partial input;
a prior graphics reply remains supported, while silence remains unknown.
The runtime displays supported images only when the probe confirms support
and an exact physical cell size is available.
This ordering follows the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

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

At initial attach and each resize, the local or detached runtime accepts a
physical cell size only if the reported outer terminal pixels form an exact
grid. It uses that size for subsequent pane output, including while detached
or temporarily closed. A new attachment invalidates the old cell size until
its first resize is processed. Missing or inexact pixel reports use the
unsized store path; no size is guessed from row/column counts alone. This
follows the sizing rules in the
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

The runtime probes Kitty graphics support separately for each outer-terminal
attachment. When support and an exact physical cell size are known, it
composites and displays all three image bands for visible panes. It uses
representative outer z-values below the background boundary, below text, and
above text while preserving source-image order inside each band. Output is
bounded by the frame queue; unchanged image revisions are not retransmitted.
A band that fits an empty frame but misses the current frame budget is retried
on the next frame without resending bands that already succeeded.
Switching windows, moving/resizing panes, deleting placements, or opening an
overlay removes the runtime-owned images. Unknown/unsupported terminals and
inexact cell sizes receive no image commands. Scrollback images, unsupported
actions and transfers remain unimplemented;
this does not yet claim Yazi preview compatibility.

A child running inside a pane may probe graphics with `a=q`. On a currently
attached outer terminal whose Kitty graphics probe succeeded and whose physical
cell size is exact, Rustmux replies to a completed, valid direct-data query
using the same nonzero image ID. The reply is delivered through the pane's PTY
before a subsequent primary-DA reply, so a child can detect this supported
subset. Completed transfers with invalid image data or unsupported controls
receive a bounded error reply; `q=1` suppresses success and `q=2` suppresses
all replies. A query never inserts or replaces an image. Without confirmed
display support, or while detached, Rustmux stays silent on graphics queries;
a following DA reply still reaches the child. File/shared-memory media,
compressed or malformed transfers, and image-number references in queries or
deletes remain unsupported. Under the same attachment and
sizing conditions, a completed direct-data `a=t` upload with an explicit
nonzero `i` or a nonzero `I` receives one reply
after its final chunk: `OK` only after successful storage, otherwise a bounded
error. Corrupt PNG data and controls outside the data-only subset are rejected
without replacing an existing image. `q=1` suppresses success and `q=2`
suppresses all replies; absent display support means no graphics reply.
Malformed or unsupported transfers that never finish assembly still get no
reply. A well-formed `a=p` placement with a nonzero `i` or `I` now replies
after the store result under the same attachment and sizing conditions. A
found image returns `OK`, a missing ID returns `ENOENT`, and an unsupported
control or invalid placement geometry returns `EINVAL`. Numbered placements
reply with both the resolved `i` and requested `I`; a missing number returns
`ENOENT`. Unparseable controls cannot be correlated and remain silent. A valid
nonzero `p` is echoed in the
reply; absent or zero `p` stays anonymous. `q=1` suppresses success, and
`q=2` suppresses all replies. The placement and cursor remain unchanged on
failure. A completed direct-data `a=T` with nonzero `i` or `I` now receives one
reply after its final chunk and the store result. Successful storage and
placement return `OK`, with a valid nonzero `p` echoed; invalid geometry
returns `EINVAL:invalid placement`, while rejected image data or unsupported
controls return `EINVAL:invalid image`. Transfers exceeding the assembler's
16 MiB bound never complete, so they receive no reply.
Unknown display controls are rejected before replacing an existing image, and
`q=1`/`q=2` retain the same reply suppression rules. Incomplete or malformed
transfers, anonymous image IDs, and unsupported outer graphics capability
remain silent. These limits mean a query, upload, or placement response is not
a claim of full Kitty graphics compatibility.
The query/DA ordering follows the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

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
Runtime upload tests cover atomic rejection of corrupt PNG replacements and
acceptance of valid PNG with inferred placement extent.
Scroll-clip tests verify top/bottom pixel rows and a pane-driven margin scroll.
Composition tests cover `z`/image-ID order, straight-alpha blending, offsets,
malformed layers, and output/work limits.
Snapshot tests cover the pane-level pipeline, screen selection, invalid image
data, and oversized viewports.
Plane tests cover the two exact z boundaries, empty/alternate-screen bands,
within-band blending, and the aggregate output limit.
Runtime tests cover PTY output storage with known and unknown physical cell
size, plus cell-size invalidation when a detached client reconnects.
Query tests cover reply/DA ordering, conditional silence, quiet modes, invalid
image data, unknown controls, and a child PTY round trip.
Upload acknowledgement tests cover chunk completion, reply/DA ordering,
rejected replacements, quiet modes, and a child PTY round trip.
Image-number upload tests cover smallest-free-ID allocation, repeated numbers,
mutually exclusive identities, final-chunk replies, failed uploads, and a child
PTY round trip.
Placement acknowledgement tests cover named and anonymous IDs, missing images,
invalid geometry, quiet modes, reply/DA ordering, and a child PTY round trip.
Numbered-placement tests cover newest-image selection, assigned-ID access to
older images, fallback after deletion, ambiguous identity rejection, replies,
and a child PTY round trip.
Transmit-and-place acknowledgement tests cover final-chunk timing, placement
identity, failed replacement, quiet modes, reply/DA ordering, and a child PTY
round trip.
Visible-delete tests cover current-screen scoping, off-screen scrollback,
partial overlap, lowercase/hard data lifetime, malformed selectors, and a
child PTY round trip.
Cursor-delete tests cover overlapping and non-overlapping placements,
current-screen and scrollback scoping, lowercase/hard data lifetime, unchanged
cursor position, invalid keys, and a child PTY round trip.
Coordinate-delete tests cover one-based coordinates, overlapping placements,
invalid/out-of-range controls, current-screen and scrollback scoping,
lowercase/hard data lifetime, and a child PTY round trip.
Z-filtered cell-delete tests cover overlapping layers, default and negative z,
signed bounds, malformed controls, screen/scrollback scoping, data lifetime,
and a child PTY round trip.
Z-delete tests cover active/other-screen isolation, scrollback, signed bounds,
malformed controls, shared-reference lifetime, and a child PTY round trip.
Column-delete tests cover partial overlap, one-based bounds, data lifetime,
active-screen and scrollback scoping, malformed controls, and a child PTY round
trip.
Row-delete tests cover partial overlap, one-based bounds, data lifetime,
active-screen and scrollback scoping, unknown width, malformed controls, and a
child PTY round trip.
Image-ID range-delete tests cover inclusive and reversed bounds, 32-bit limits,
data-only uploads, malformed controls, both screens, and a child PTY round trip.
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
