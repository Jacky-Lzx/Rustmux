# Mouse hover feedback

Enable pointer feedback for Rustmux's window controls in the selected config:

```toml
mouse_hover_cursor = true
```

This option defaults to `false`. It is included in `default-config`, accepted by
`check-config --strict`, and reported as `[settings].mouse_hover_cursor` by
`check-config --toml` and `show-config`. A named server owns this setting; an
attaching client's config does not replace it. It follows the existing atomic
[hot-reload policy](config-reload.md), including detached updates, invalid-file
retention and safe input boundaries.

| Pointer position or operation | Displayed shape |
| --- | --- |
| A clickable window label or bottom shortcut key/action | `pointer` |
| A vertical pane separator | `ew-resize` |
| A horizontal pane separator | `ns-resize` |
| An active separator drag | `grabbing` until release |
| Other positions or the disabled option | The selected application's OSC 22 shape, or the terminal default |

Only actual clickable ranges count. Empty spaces, labels clipped out of the
bar, hidden footers and error text replacing the footer do not gain actions or
pointer feedback. The display uses the current layout, so resizing, changing
windows, renaming labels and zooming recompute hitboxes. Zoom hides separator
handles. Mouse positions are complete one-based cell reports; legacy and SGR
reports are both understood. Partial reports and mouse-like bytes inside a
bracketed paste do not update the remembered position.

When visible controls exist, the composed display requests any-motion tracking
(1003). The application's requested mouse encoding remains unchanged; legacy
coordinate limits still apply. Filtering continues to use the application's own
tracking mode: Off and Button do not receive motion, Drag receives held-button
motion, and Any receives unpressed motion with pane coordinates. Unpressed
movement cannot execute an action, change pane focus or leave a Rustmux keyboard
mode. Existing clicking and separator dragging keep their behavior.
Hover redraws follow normal frame pacing and wait for a synchronized-output batch
to finish or reach its existing timeout; movement cannot reveal an unfinished
application frame.

Overrides are applied to a cloned display screen. They never modify application
pointer stacks or query replies. Applications can change their shape while the
mouse is over a control; moving away or disabling hover restores the latest
shape. The renderer caches shapes and emits OSC 22 set/reset operations without
pushing onto the outer terminal's stack. Help, History and editing prompts retain
their existing default-pointer behavior; this increment does not add hover to
their internal controls or Session Manager. Closing those views clears remembered
position and restores the application's shape until the next mouse report.

Reconnect starts with no remembered position; application pointer state survives.
Exit and detach retain the cleanup from [Mouse Pointer Shapes](pointer-shapes.md).
Actual appearance requires an outer terminal supporting
[OSC 22](https://sw.kovidgoyal.net/kitty/pointer-shapes/). Reports follow
[XTerm tracking](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-Mouse-Tracking).

## Review and verification

Based on reviewed pointer-state commit `03f63cf`, with `main` reference fixed at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c`. The reference already has the default-off
`mouse_hover_cursor` option and equivalent shape choices. This implementation
uses the human track's screen composition and renderer cache. It reevaluates the
remembered position on redraw and reload, keeping child state independent.

Read config resolution and diagnostics in `src/config.rs` and
`src/config/diagnostics.rs`, then `src/terminal/hover.rs` and the complete-report,
redraw and runtime-config paths in `src/terminal.rs`. Model tests cover hitbox
boundaries, both separator axes, zoom, resize, hidden controls, child stack
preservation, tracking filters, local modes, fragmented legacy input and paste.
`tests/terminal_loop_mouse_hover.py` runs two real child probes through an
attached named server, checking config ownership, hover, query isolation,
input routing, synchronized-frame suppression, separator drag, shortcut clicks,
zoom, reload and reconnect.

Local cumulative validation on Rust 1.99.0: `cargo test --all-targets --locked
--offline -- --test-threads=4` passed 972 tests across 48 targets, with 6
pre-existing ignored tests. All 42 real-PTY scenarios passed. The added
synchronized-output regression reproduced an early intermediate-frame repaint
before the fix and passed afterward. Clippy with all targets/features and warnings
denied, formatting checks, `git diff --check` and the mdBook build also passed.

This branch has not been pushed. Linux CI and actual GUI pointer appearance have
not been verified. The change awaits owner review and does not update the shared
acceptance ledger.
