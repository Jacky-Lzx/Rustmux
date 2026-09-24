# Kitty Graphics Framing

The first Kitty graphics increment is a bounded stream framer, not image
display. GraphicsFramer recognizes complete ESC _ G … ESC \ commands (and
their 8-bit APC/ST form) across arbitrary PTY read boundaries. It emits graphics
commands and ordinary terminal bytes as ordered events. Other APCs pass through
unchanged. A UTF-8 continuation byte cannot be mistaken for an 8-bit APC.

Each retained command is limited to 16 KiB, enough for Kitty's documented
4 KiB encoded direct-data chunk plus control fields. An oversized, cancelled,
incorrectly terminated or unfinished graphics command is discarded without exposing its
payload as terminal text. The framer does not concatenate separate Kitty
transfer chunks; each APC is one event.

The runtime does not yet use these events to transmit, place, redraw or delete
images. It does not answer graphics capability queries or claim Yazi preview
compatibility. Those are later review increments, along with per-pane image ID
isolation and lifecycle cleanup. Until then, the existing display parser
continues to ignore APC content.

Unit tests cover every two-chunk split of a command, ordinary output ordering,
non-graphics APCs, UTF-8/C1 ambiguity, oversized and cancelled commands, and
EOF recovery. Run cargo test --lib graphics::tests.
