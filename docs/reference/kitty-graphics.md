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
The normal runtime instead discards graphics commands without assembling or
retaining image data. It does not store, place, redraw or delete images, answer
graphics capability queries, or claim Yazi preview compatibility. Those are
later review increments, along with per-pane image ID isolation and lifecycle
cleanup.

Unit tests cover every two-chunk split of a command, ordinary output ordering,
non-graphics APCs, UTF-8/C1 ambiguity, oversized and cancelled commands, and
EOF recovery. Assembler tests cover chunk inheritance, raw byte counts, bounds,
unsupported media and recovery. Pane tests cover interleaved text, per-pane
isolation and command-output filtering. Run `cargo test --lib graphics::tests`,
`cargo test --lib graphics_transfer::tests` and `cargo test --test panes`.
