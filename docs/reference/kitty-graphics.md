# Kitty Graphics Input

The first Kitty graphics increment is a bounded stream framer, not image
display. `GraphicsFramer` recognizes complete ESC _ G … ESC \ commands (and
their 8-bit APC/ST form) across arbitrary PTY read boundaries. It emits graphics
commands and ordinary terminal bytes as ordered events. Other APCs pass through
unchanged. A UTF-8 continuation byte cannot be mistaken for an 8-bit APC.

Each retained command is limited to 128 KiB of encoded data plus 4 KiB of
control/framing space. This accommodates installed `kitten icat`, which sends
larger chunks than Kitty's documented 4 KiB limit. An oversized, cancelled,
incorrectly terminated or unfinished graphics command is discarded without exposing its
payload as terminal text. The framer does not concatenate separate Kitty
transfer chunks; each APC is one event.

The crate-private `graphics::command` module shares complete-command framing
and control-field parsing between the assembler, store, and child replies.
Data commands require a semicolon separating controls from payload; control-only
commands allow one trailing semicolon but no payload. Duplicate fields and
malformed keys or values are rejected. Action, format, identity, and geometry
validation remain with the corresponding consumers.

`DirectTransferAssembler` is the next, separate data boundary. It parses
complete APC G commands, decodes Base64 direct-data chunks, and combines them
until the final chunk (`m=0` or an omitted `m`). Subsequent chunks may contain
`m`, optional `q`, and a repeated matching action, as sent by `kitten icat`;
an invalid or interrupted transfer is discarded, and the next independent transfer can
start cleanly. It preserves the first chunk's control fields and the final
chunk's optional `q` override. It checks the byte count for raw RGB/RGBA data;
PNG bytes remain opaque until a later image decoder validates them.

One encoded chunk is limited to 128 KiB to accommodate installed `kitten icat`
output (the published protocol specifies 4096 bytes); an assembled raw transfer
is limited to 16 MiB, while direct PNG file bytes may use the pane's 32 MiB
image-store budget. The final chunk may omit Base64 padding. Direct data
(`t=d`, `a=t/T/q`) may be uncompressed or use `o=z` zlib compression.
Compressed input follows the same format-specific limit. Raw RGB/RGBA whose
declared expansion exceeds 16 MiB is kept compressed and validated a
row at a time, up to 256 MiB expanded and a 32 MiB row budget; smaller raw
transfers retain the 16 MiB expansion limit, while compressed PNG file bytes
may expand to 32 MiB. Raw RGB/RGBA output must match its dimensions, and
compressed direct PNG requires
`S=<uncompressed-byte-count>`. Malformed streams, mismatched sizes,
and trailing compressed bytes are discarded before they can replace an image.
The private `graphics::transfer::shared_memory` module owns POSIX shared-memory
input (`t=s`), including name validation, descriptor ownership, bounded mapping,
and unlinking. The transfer module retains the crate-private entry point and
common image-size and compression validation. Shared-memory transfers are read
through a bounded mapping, then unlinked and closed. The Base64 payload must be
a single POSIX shared-memory name; `S` and `O` select a byte range. RGB/RGBA
without `S` uses its declared dimensions as the exact length, since macOS
reports page-rounded shared-memory
sizes. Shared-memory bytes then use the same image validation, storage and
query rules as direct data. Compressed PNG over shared memory is not supported.
The pane store also accepts regular-file (`t=f`) and temporary-file (`t=t`)
transfers. The Base64 payload is a filesystem path; symlinks are resolved before
opening, and only regular
files are read. The file is opened without blocking on a replaced FIFO and its
descriptor type is checked again. `O` is the byte offset and optional positive
`S` selects the stored byte count; without `S`, the rest of the file is read.
Reads are bounded to 16 MiB for raw/compressed-raw data and 32 MiB for PNG data.
Zlib raw data retains the existing expanded-size bounds; zlib PNG expansion is
separately bounded to 32 MiB. File bytes reuse the image-validation, query,
storage and placement paths, and `t=f` never removes the source file.
Read failures for both media use one `EBADF:Failed to read image file` reply.
For `t=t`, an opened regular file is cleaned up after reading, including range,
read, decompression, and later image-validation failures. Cleanup requires the
resolved path to contain `tty-graphics-protocol` and be under a recognized
temporary directory (`TMPDIR`/the platform temporary directory, `/tmp`,
`/var/tmp`, or `/dev/shm`). Directory membership uses path components rather
than string prefixes. Final symlinks and their targets are retained; unmarked
or out-of-directory files may still be read but are not removed. Malformed
commands and files that were never opened are not removed. Cleanup pins the
parent directory and checks the entry's device/inode before unlinking, retaining
a replacement detected after reading. Cleanup failure does not turn a valid
image transfer into a failure.
Neither paths nor shared-memory names appear in replies;
`q=2` and unidentifiable or incomplete requests stay silent.
The chunk and continuation rules follow the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

Every pane now passes PTY output through its own framer before the text parser
and OSC observer. Graphics bytes cannot appear as screen text or shell-command
output; the hidden close/undo path uses the same pane pipeline. Callers can opt
in to complete direct transfers with `Pane::process_output_with_graphics`.
An additional opt-in path, `Pane::process_output_with_image_store`, retains
complete transfers with explicit nonzero `i` IDs or numbered `I` uploads in a
pane-local `ImageStore`.
An `a=T` upload without `i/I`, or with `i=0`, now creates a distinct anonymous
display. Its internal store key is never a child-visible image ID: a child
cannot place or delete it by guessing that key, and an explicit upload with a
colliding ID moves the anonymous image to another private key. The requested
`p` is ignored for an anonymous image. When its last placement is removed,
its data is freed; it receives no child acknowledgement. Data-only `a=t`
still requires an explicit ID or a nonzero image number.
The running multiplexer now uses that bounded store for live, detached and
temporarily closed/undoable panes; the public `Pane::process_output` method
still discards graphics unless its caller opts in.
Replacement and explicit removal update byte accounting. At 32 MiB or 256
images per pane, quota eviction chooses the oldest image without placement
references first, falling back to the oldest placed image only when necessary.
Among unplaced images, uploads with the `N=1` transient bit are evicted first;
each class retains first-in-first-out order. The hint has no effect while an
image still has a placement, and retransmission replaces the previous hint.
References on either screen or in scrollback still count as placements. The
store rejects query action `a=q`; the ordinary store path preserves PNG bytes
without decoding them.
An upload with `I=<nonzero-number>` and no `i` receives the smallest free
positive image ID, reported as `i=<assigned>,I=<number>` when replies are
enabled. Repeating a number creates another image rather than replacing its
predecessor. Existing explicit IDs are never overwritten by allocation;
`a=p,I=<number>` now places the newest live image created with that number.
Older images remain available through their assigned `i` values; removing the
newest image falls back to the next newest. Replacing an older numbered image
by its `i` retains its number without changing this creation order. The
`d=i/I` delete selectors still require `i`. The `d=n/N` selectors instead
require `I=<number>` and target only the newest live image with that number.
An optional `p=<id>` limits deletion to one placement; lowercase `n` keeps
image data, while uppercase `N` frees it once no placements refer to it,
including for a data-only image. Deleting that image makes the next newest
image with the number available again.
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

The private `graphics::decode::png_reader` module owns complete PNG decoding
and row-by-row PNG reads, including decoder allocation limits, transformations,
dimension checks, and complete-tail validation. The public `StoredImage` methods
retain their existing entry points. Both PNG reading paths keep their existing
format rules, including bounded full decoding for interlaced PNGs and rejection
of interlacing by the row reader.

`StoredImage::decode_png_thumbnail(width, height)` is a separate opt-in
boundary for non-interlaced static PNGs, including those whose full RGBA
expansion exceeds 32 MiB. It validates the complete PNG while reading one
transformed row at a time, samples into a caller-chosen nearest-neighbor
thumbnail of at most 32 MiB, and bounds decoder allocation and row size.
`StoredImage::resample_png_placement(layout)` uses the same bounded row decoder
to sample an explicit source crop directly into destination pixels, including
upscaling, without expanding the whole source image. Both paths validate the
entire PNG, including its tail. Runtime upload validates non-interlaced PNGs
through this bounded path first, without allocating full RGBA; bounded
interlaced PNGs retain a full-decode fallback. Regular placements sample only
their viewport-visible PNG pixels, including for small images, so a small
on-screen fragment does not require a full source or destination raster.
Bounded interlaced PNGs retain full decoding and clipping. Virtual placements
also sample only placeholder-referenced PNG cells, including for small images;
bounded interlaced images retain the full-decode fallback. PNG validation still
reads through the final row and tail, including when only a fragment is visible.
Uncompressed RGB/RGBA regular placements likewise sample only the visible
destination pixels after bounded source decoding, so enlarging a small image
beyond the 32 MiB placement limit does not prevent a small fragment from
appearing in the viewport.

The private `graphics::decode::raw_reader` module handles raw RGB/RGBA buffers
and zlib-compressed pixel rows. It retains raw byte-count validation, expanded
size and row limits, crop bounds, and rejection of truncated or trailing zlib
data. Public `StoredImage` decoding and placement methods keep their existing
entry points; zlib reading reuses the shared row sampler.

Large `o=z` RGB/RGBA transfers also remain compressed in the pane store.
Runtime upload validates their complete zlib stream, and a regular placement
samples only its visible source-backed destination pixels; invisible pixels
do not consume the 32 MiB placement budget. For virtual placeholders, the
screen's referenced source cells are collected first; PNG and compressed raw
images are sampled once into their bounding source-backed rectangle rather
than a full virtual raster. When references are too far apart for a bounded
rectangle, a single zlib or PNG row-decoder pass samples their disjoint cells
directly. Both paths limit the aggregate RGBA output to 32 MiB and still
validate the complete source stream. Adjacent output columns advance their
pixel-center source coordinate by quotient and remainder, avoiding a 128-bit
division for each sampled output pixel.
The private `graphics::snapshot::placeholder_raster` module owns placeholder
identity resolution, referenced-region collection, bounded or sparse raster
selection, and per-cell clipping. It shares the snapshot's dimension cache
and cumulative layer-input budget with ordinary placements, retaining
insertion-order identity selection and the bounded Adam7 fallback.
The private `graphics::decode::sampling` module owns these shared sample
coordinates, sparse row scheduling, row-to-RGBA conversion, and bounded output
allocation. PNG and zlib readers retain their source-bound checks and complete
stream validation. Decoded RGBA placement sampling uses the same pixel-center
coordinate function.
The private `graphics::decode::stream_placement` module coordinates PNG and
zlib placement requests, sharing destination-relative region conversion and
result wrapping. Existing `StoredImage::resample_*` methods and stream error
type paths remain available. Format checks still precede layout checks, region
results retain request order and destination coordinates, and empty region
requests still validate the complete source stream.
An optional local-file check is
`RUSTMUX_COMPAT_IMAGE=/absolute/path/to/image.png cargo test --lib user_png_streams_to_thumbnail -- --ignored`.

The running multiplexer validates PNG data before replacing a pane image.
Corrupt, unsupported, over-budget streamed, or dimension-mismatched PNG transfers
leave the existing image, placements, revision, and cursor unchanged. Successful
PNG dimensions are cached for sized placement inference, avoiding a second
decode at insertion time. Raw transfers were already byte-count checked by the
assembler. The public opt-in image-store methods retain their earlier
deferred-decoding behavior for callers that only need the original bytes.
When those callers later request inferred placement extents, dimension lookup
uses the same complete streaming validation for PNG and compressed raw data,
with the bounded interlaced-PNG fallback. This allows regular and virtual
placements to infer dimensions even when full RGBA expansion exceeds 32 MiB.
Plain raw data reuses its assembler-checked dimensions. Validation results are
cached until the image is replaced or removed; a corrupt stream cannot supply
dimensions from its header alone.
If an opt-in caller creates a placement without cell pixels, image-only
snapshots resolve missing dimensions through that validation as needed. A
snapshot reuses the result across its placements without changing the store's
deferred state. Large PNG and compressed raw images can then be sampled into
their visible region without expanding a full RGBA source buffer.

The opt-in store now tracks placement *references* for `a=T` and a strict
`a=p` subset with either `i=<id>` or `I=<number>`, and optional `p=<id>`.
A named placement replaces the same `(i,p)`
reference; an absent or zero `p` creates an anonymous reference. Re-transmitting
an image ID drops its old references. The supported ID-delete subset is
`a=d,d=i/I,i=<id>[,p=<id>]`: lowercase removes matching references but keeps
data, while uppercase also releases data once no references remain.
The numbered equivalent is `a=d,d=n/N,I=<number>[,p=<id>]` with the same
reference and data lifetime rules, scoped to the newest image with that number.
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
Missing extents remain unknown on the ordinary opt-in path. `U=1` on `a=T`
or `a=p` now creates an invisible virtual placement with a nonzero `i` or
`I`, nonzero `c/r` when supplied, its source crop, pixel offset, and z-index
retained. It has no cursor anchor and never moves the cursor. A named `(i,p)`
virtual placement can be replaced and deleted by image ID, number, or ID
range; cell/row/column/z-index selectors do not match it. `U=0` remains an
ordinary placement. Placeholder
cells can now be decoded from `U+10EEEE`, the complete row/column diacritic
table, foreground image ID, and optional underline-color placement ID;
omitted coordinates inherit from the adjacent placeholder when the protocol's
color and row conditions hold. The pane snapshot now fits a virtual prototype
once, clips its pixels to each matching placeholder cell, and places those
cells wherever the current text grid contains them. Overwriting, erasing,
scrolling, or moving the placeholder updates the displayed image without a
new graphics command. Runtime redraw tracks placeholder changes separately
from image-store revisions, and the child's placeholder glyphs are replaced
with blank display cells before output to the outer terminal. This is an
image-only composition path, not full Kitty Unicode-placeholder compatibility;
the optional Yazi smoke below verifies only its PNG preview path. Relative
placements remain unsupported. The opt-in store records metadata; runtime composition, redraw,
and child replies are described below.

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

The `graphics::geometry` module owns cell/pixel types, source-rectangle
intersection, extent inference, anchor conversion, and placement layout.
The existing `graphics_store` type paths remain available through re-exports.
Virtual-placement state and command validation remain in the store and reuse
the pure geometry calculations.

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

The private `graphics::decode::placement` module handles already-decoded RGBA
sampling, viewport intersection, and permanent scroll clipping. Placement
types and errors remain available through `graphics_decode`, and the existing
methods remain on `DecodedImage` and `ResampledPlacement`. Full-buffer and
streamed decoders share the same pixel-center sampling math and size checks.

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
64 MiB, and the layer count at 1024 placements plus 65536 placeholder cells.
Negative `z` is ordered among images; text and cell backgrounds are handled
by the runtime's separate stacking bands, not by this image-only compositor.
The runtime calls the compositor during graphics redraw. The ordering
follows the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

`Pane::compose_image_snapshot` connects those opt-in stages for the current
screen. Given a verified physical cell size, it derives the pane's pixel
viewport, decodes each cursor-anchored image, recalculates placement layout,
resamples and scroll-clips it, then blends the visible layers. It also decodes
current-screen Unicode placeholders and draws only the matching virtual
placement's referenced cell pixels; the prototype itself remains invisible.
Placements on the other screen are omitted. Invalid selected-screen image data
or geometry fails the whole snapshot rather than returning a partial image.
The returned canvas is transparent where no image is placed; the store is
unchanged. It remains image-only; normal runtime rendering uses the separate
stacking-band path.

The private `graphics::snapshot::raster` module collects visible placement
pixels before composition. It owns ordinary viewport sampling and scroll
clipping, the shared dimension-validation cache and cumulative input budget,
and invokes placeholder rasterization with that same state. The common
full-placement helper retains streamed zlib and large-PNG sampling as well as
bounded full decoding. `graphics::snapshot` keeps the existing snapshot and
stacking-band entry points, viewport validation, and composition.

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
calls it only after preflighting the remaining 16 MiB frame budget. Before
preflight, the runtime crops transparent margins from each composed band to the
smallest cell-aligned
rectangle containing visible pixels, preserving its pane-relative origin.
This avoids charging empty pane space against the frame budget, especially in
large outer windows. If a cropped band still exceeds one frame, the runtime
splits it into cell-aligned placements targeting at most 8 MiB raw RGBA each.
A single physical cell larger than that target is still considered when its
actual encoded placement fits the 16 MiB frame; the encoder's exact preflight
remains the final limit. It uploads missing tiles over successive frames and
deletes every owned tile ID
when the band changes or clears. Deletions are queued ahead of new uploads if
the current frame has no room; their IDs remain tracked until the deletion
commands have been written in later frames. A tile that cannot fit by itself
is skipped. A failed write may leave a partial transfer. The chunk format follows the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

`kitty_rgba_placement_len` now preflights the exact encoded byte count,
including every APC wrapper, without allocating a Base64 image. The bounded
`write_kitty_rgba_placement_with_limit` rejects a placement before writing
when its complete transfer would exceed the caller's remaining output budget.
For a fully opaque tile, `kitty_rgb_placement_len` and
`write_kitty_rgb_placement_with_limit` instead preflight and stream `f=24`
pixels, converting only one Base64-sized chunk at a time. A tile containing
any transparency retains `f=32`; both paths reject an over-budget transfer
before writing. The runtime uses the shorter eligible raw transfer.

The private `graphics::output::png_output` module owns PNG preparation,
encoded-size preflight, and `f=100` chunked transmission, including the
source-PNG passthrough path. `graphics::output` retains the existing entry
points and owns raw RGB/RGBA transmission; both paths share the same chunk
limits and image validation.

`EncodedKittyPng::from_rgba` is a bounded output boundary. It
validates the RGBA dimensions, encodes a static 8-bit PNG with a bounded
32 MiB output buffer using fast compression, and retains it so callers can
preflight the exact Kitty placement length without repeating compression.
Fully opaque input uses RGB PNG, converting one row at a time; any transparency
keeps RGBA PNG so alpha is preserved.
`write_with_limit` sends
`a=T,f=100` direct-data APCs with the same 4096-byte Base64 chunk limit and
refuses an over-budget placement before writing. PNG dimensions are carried
by the file itself, so the APC omits `s` and `v`. For composed overlays of at
least 256 KiB raw RGBA, the runtime selects PNG only when its complete wire
transfer is smaller than the eligible `f=24` or `f=32` raw transfer.
Both choices receive the same exact 16 MiB frame preflight. PNG encoding or
size-validation failures fall back to that raw transfer.
If a PNG tile is prepared but the current frame has insufficient space, the
runtime retains at most one encoded tile (bounded by the same 32 MiB PNG
limit) for the next frame. It reuses that transfer only while the pane,
revision, geometry, cell size, image ID, band, and tile position still match;
scene changes and attachment cleanup discard it. Other deferred tiles can be
prepared on later frames without retaining unbounded compressed copies.

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
Virtual prototypes survive these clears, but their displayed images vanish
when the placeholder text cells are cleared. Relative references remain
unsupported. These
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

The crate-private `graphics::outer` module owns crop/encoding preparation and
the attachment's image cache, deferred retries, shared-memory lifetimes, and
placement deletion. `terminal` coordinates capability probes and PTY polling;
both text and image output share the bounded queue in `render::frame`.

The runtime probes Kitty graphics support separately for each outer-terminal
attachment. When support and an exact physical cell size are known, it
composites and displays populated image bands for visible panes. It uses
representative outer z-values below the background boundary, below text, and
above text while preserving source-image order inside each band. Output is
bounded by the frame queue; unchanged image revisions are not retransmitted.
After the ordinary graphics probe, a separate one-pixel shared-memory query
must receive `OK` before the outer shared-memory fast path is enabled. A
single RGB, RGBA, or validated PNG image in the above-text band can bypass
pane-sized RGBA composition and PNG encoding when its Unicode placeholders
cover one complete contiguous rectangle and its source is neither cropped nor
offset. On a cache miss, Rustmux uploads the stored raw pixels or original PNG
bytes through a new POSIX shared-memory object and remaps the image ID. It
retains up to eight exact source images or 32 MiB per attachment. Repeating a
source uses `a=p` with a new placement ID instead of another upload; deleting
an old placement with `d=i,p=...` preserves its image data. The least recently
used inactive image is freed with `d=I` when the cache is full. An outer error
reply invalidates the cached image and triggers a fresh upload. Raw previews
fit the same `c` by `r` cell rectangle; natural-size PNG previews keep their source
pixel dimensions. Without outer shared-memory support, eligible PNGs use a
bounded direct-data transfer of the original bytes. The outer terminal unlinks
the object after reading it; Rustmux retains linked objects until then,
bounds outstanding objects to 64 MiB, and cleans them up on attachment exit.
While a sole eligible virtual placement has an incomplete placeholder
rectangle, Rustmux waits for the next child update instead of repeatedly
encoding partial composites. Complete rectangles take the fast path
immediately. A rectangle still incomplete after 350 ms without changes uses
the composed PNG/raw fallback.
Overlapping placements, unsupported media, failed shared-memory creation, or
an unsuccessful outer query also use that fallback.

The private `graphics::snapshot::source_placement` module recognizes these
complete and incomplete rectangles and checks source format, validated
dimensions, crop, offset, and placement identity. After dimension validation,
it returns borrowed source bytes without resampling or re-encoding them.
`graphics::snapshot` retains the existing entry points; `graphics::outer`
owns deferred retries, transmission, and caching.

A band or tile that fits an empty frame but misses the current frame budget is
retried on the next frame without resending uploads that already succeeded.
Switching windows, moving/resizing panes, or deleting placements removes their
outer placements. Opening an overlay or leaving graphics mode clears the
attachment's cached image data. Unknown/unsupported terminals and
inexact cell sizes receive no image commands. Scrollback images, unsupported
actions and transfers remain unimplemented;
this is not a claim of complete Yazi or Kitty graphics compatibility.
For virtual `U=1` direct-data uploads, omitted `c`/`r` cell extents are inferred
from the decoded or declared image dimensions and exact cell-pixel size. A later
`a=p,U=1` placement can infer them from the stored image, including a validated
PNG and its source crop. Without an exact cell size or valid image dimensions,
the new placement is rejected without replacing an existing one. The pane
retains which extent axes were explicitly requested: inferred axes are
recalculated from the source dimensions when the exact cell-pixel size changes,
while explicit `c`/`r` axes keep their cell counts. Placeholder cells outside
the current extent no longer contribute image pixels after a resize. The pane
rejects out-of-range placeholder cells before resampling the virtual image;
explicit `c`/`r` bounds can also be checked before image decoding. An in-range
reference to invalid image data remains a snapshot error on opt-in paths. Raw
image dimensions and previously validated PNG dimensions now allow inferred
bounds to be checked before decoding too; unvalidated PNG declarations are
not trusted for this shortcut. The pane answers `CSI 16t` with
`CSI 6;height;width t` when that cell size is known. It also answers `CSI 14t`
with `CSI 4;height;width t` for the current pane text area, computed from its
grid and the exact cell size. `CSI 15t` reports that pane's logical screen with
the same pixel dimensions and the distinct `CSI 5;height;width t` reply.
Otherwise it does not invent a pixel-size reply.

An optional installed-Yazi smoke test exercises the actual preview path. After
updating Yazi, run `cargo compat`. This alias runs the opt-in tests in
`tests/compat.rs`, so future compatibility checks can join the same command.
It starts Rustmux and Yazi in isolated PTYs with two generated PNGs, then
decodes Rustmux's composed Kitty RGBA uploads. It checks both four-color
previews, their relative pixel positions, and deletion of the previous outer
image after Yazi moves to the second file. A pixel-only cell-width resize with
the same text grid must replace the outer image and preserve the second preview's
pixels. The test also checks that child placeholders do not leak to the outer
terminal. It is excluded from normal
`cargo test` and CI because Yazi is not a project dependency; when Yazi is
absent, it reports `SKIP`. The smoke passed with Yazi 26.9.1, but only covers
this PNG/Kitty preview path, not every Yazi feature or terminal.

The same optional `cargo compat` target also runs an installed `kitten icat`
smoke when `kitten` is on `PATH`. It checks a small generated PNG with Unicode
placeholders and another generated image through the default auto-detect,
multi-chunk command, without overriding window size. A named-session case
also checks a generated image in a large pixel viewport where an uncropped
pane canvas would exceed the output frame budget. All must produce a
composed outer image. When the attached terminal supplies an exact cell size,
Rustmux writes the current pane's pixel dimensions to its child PTY, including
after text-grid or cell-pixel changes. If that size is unknown or exceeds the
PTY's 16-bit pixel fields, both pixel fields remain zero. This smoke covers these
PNG paths, not all `kitten icat` features.

To test a specific local image after a viewer or image-format update, run
`RUSTMUX_COMPAT_IMAGE=/absolute/path/to/image.png cargo compat`. Without that
variable, the user-image case reports `SKIP`; when it is set, a missing file
or missing `kitten` fails the test. It runs the real `kitten icat` command in
a named Rustmux PTY, waits for the child shell to regain its prompt, and
verifies every complete outer `f=24`, `f=32`, or `f=100` tile. The image path is not
stored in the repository. This remains a fake-Kitty PTY protocol check, not
a claim that a particular GUI terminal displayed the pixels.

`cargo compat` also runs a synthetic large-PNG smoke without requiring Kitty,
Yazi, or another installed image viewer. A named Rustmux session receives the
PNG through its child PTY; a fake Kitty-capable outer PTY captures the tiled
uploads. The test checks their cell-aligned positions, reconstructed height,
sampled colors, unique image IDs, and all corresponding deletion commands.
It then checks that a small opaque image uses `f=24` with correct pixels.
Replacing it with a translucent image must use `f=32` and preserve both
tested alpha values in the outer upload.
Finally, it sends a deliberately incompressible 16–32 MiB PNG in bounded
Kitty chunks through the child PTY and checks the downsampled outer `f=24`
image's size and corner pixels. This covers the raised direct-PNG input limit
through the named-session bridge, not just the in-process decoder.
It is opt-in because it captures tens of MiB of outer-terminal output.

A child running inside a pane may probe graphics with `a=q`. On a currently
attached outer terminal whose Kitty graphics probe succeeded and whose physical
cell size is exact, Rustmux replies to a completed, valid direct-data query
using the same nonzero image ID. An uncompressed direct query may include an
`S` byte count when it matches the payload, as `kitten icat` does. The reply is
delivered through the pane's PTY
before a subsequent primary-DA reply, so a child can detect this supported
subset. PNG queries and pane uploads first use bounded full-stream validation;
small interlaced PNGs retain a full-decode fallback. Large zlib-compressed raw
queries also validate the entire decompressed stream without allocating its
full image.
Completed transfers with invalid image data or unsupported controls
receive a bounded error reply; `q=1` suppresses success and `q=2` suppresses
all replies. A query never inserts or replaces an image. Without confirmed
display support, or while detached, Rustmux stays silent on graphics queries;
a following DA reply still reaches the child. Malformed transfers
and image-number references in queries remain unsupported. File and shared-memory
queries are answered only after their bytes and image have been validated.
Numbered deletes use `d=n/N` as described above. Under the same attachment and
sizing conditions, a completed direct-data `a=t` upload with an explicit
nonzero `i` or a nonzero `I` receives one reply
after its final chunk: `OK` only after successful storage, otherwise a bounded
error. Corrupt PNG data and controls outside the data-only subset are rejected
without replacing an existing image. `q=1` suppresses success and `q=2`
suppresses all replies; absent display support means no graphics reply.
The same control-subset checks apply to the opt-in image-store APIs, including
`a=T` uploads: unsupported placement controls or malformed virtual placements
cannot silently replace a stored image even when PNG decoding is deferred.
Other malformed or unsupported transfers that never finish assembly still get
no reply. A well-formed `a=p` placement with a nonzero `i` or `I` now replies
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
format-specific 16 or 32 MiB bound never complete, so they receive no
reply.
Unknown display controls are rejected before replacing an existing image, and
`q=1`/`q=2` retain the same reply suppression rules. Incomplete or malformed
transfers, anonymous image acknowledgements, and unsupported outer graphics
capability remain silent. These limits mean a query, upload, or placement
response is not a claim of full Kitty graphics compatibility.
The query/DA ordering follows the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).

Unit tests cover every two-chunk split of a command, ordinary output ordering,
non-graphics APCs, UTF-8/C1 ambiguity, oversized and cancelled commands, and
EOF recovery. Assembler tests cover chunk inheritance, raw byte counts,
format-specific 16/32 MiB bounds, a valid PNG above the raw-transfer limit
reaching the validated pane store, unsupported media and recovery. Pane tests
cover interleaved text, per-pane
isolation and command-output filtering. Run `cargo test --lib graphics::tests`,
`cargo test --lib graphics::transfer::tests` and `cargo test --test panes`.
`cargo test --lib graphics::transfer::shared_memory::tests` covers bounded reads,
unlinking after success or read failure, and malformed names.
`cargo test --lib graphics::transfer::file::tests` covers file ranges, source
retention, symlinks, special-file rejection, size limits, and compressed streams.
It also covers temporary-file cleanup after failed reads, directory/marker
eligibility, canonical path escapes, replacement inodes and renamed parents.
The child-PTY graphics scenario checks file queries/uploads, reply ordering,
failed replacement, uniform read errors, and quiet modes. The installed-kitten
smoke also exercises `--transfer-mode=file` with Unicode placeholders, and a
mirrored image that emits `t=t`; captured Kitten commands confirm that temporary
transfers are used and their sources are removed while the input PNG is retained.
`cargo test --lib graphics::command::tests` checks framing spellings, separator
rules, malformed/duplicate fields, quiet validation, and independent size limits.
`cargo test --lib graphics::geometry::tests` covers exact cell grids, source
intersection, aspect-ratio sizing, anchor overflow, and pixel-layout bounds.
`cargo test --lib graphics::outer::tests` covers crop/encoding decisions, cache
reuse, frame-budget retries, stacking bands, and image/placement cleanup.
`cargo test --lib graphics::snapshot::placeholder_raster::tests` checks
letterboxed source-cell clipping, destination coordinates, and bounded-region
equivalence.
Store tests also cover replacement, isolation, transient/unplaced quota eviction,
numbered-image fallback, explicit removal, named and anonymous references,
and soft versus hard deletion.
Decoder tests cover raw and PNG formats, palette transparency, corrupted PNGs,
dimension checks and the output-size bound. Resampling tests cover cropped
nearest-neighbor enlargement, reduction, alpha preservation, invalid geometry,
and the raster output limit. Run
`cargo test --lib graphics::decode::placement::tests` for the decoded-pixel
sampling and clipping cases. A sparse-sampling regression compares PNG and
compressed RGB/RGBA regions against a separately resampled full RGBA image
across crop, scaling, region-order and PNG color-type cases. Viewport tests
cover all four clipped edges,
negative and disjoint anchors, malformed buffers, and oversized geometry.
`cargo test --lib graphics::decode::sampling::tests` checks incremental sample
coordinates at extreme source and destination extents.
`cargo test --lib streamed_regions` covers offset region coordinates, existing
error classification, and complete-source validation for empty requests.
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
