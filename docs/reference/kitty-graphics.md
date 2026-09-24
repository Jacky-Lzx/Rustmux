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
and image-number allocation `I`; it preserves PNG bytes without decoding them.
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
but the model does not track visibility, scrollback references, or the other
delete selectors.

For the pane opt-in path, cursor-anchored placements now record a zero-based
cell row and column, plus whether they belong to the alternate screen. A
chunked `a=T` records the cursor when its final chunk arrives; `a=p` records
the cursor at that command. Explicit `c`/`r` cell extents, signed `z` index,
and `C=1` no-move request are parsed and retained. Missing extents remain
unknown until pixel-cell sizing is available. Virtual and relative placements
do not get a cursor anchor. This is metadata only: image placement does not yet
move the cursor, compose pixels, redraw, or send graphics replies.

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
shifts, resize/reflow relocation, pixel-level clipping, and scrollback rendering
remain out of scope. These choices follow the scrolling rules in the
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
dimension checks and the output-size bound.
Anchor tests cover interleaved text, final-chunk position, alternate-screen
identity, explicit layout options, malformed metadata, and named replacement.
Screen-lifecycle tests cover split `CSI 2 J`, non-clearing text erasures,
alternate-buffer transitions, RIS, and commands after a clear in the same read.
Scroll tests cover full-screen primary/alternate movement, retained scrollback
references, margin clipping, reverse index, unknown height, and event overflow.
