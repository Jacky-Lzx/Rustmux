# Mouse pointer shapes (OSC 22)

Rustmux now stores application mouse pointer shapes independently for every pane,
with separate main- and alternate-screen stacks. The selected pane's current
shape is sent to the attached terminal by the renderer. Background panes keep
their own state and cannot change the displayed pointer. Applications do not need
mouse reporting enabled to use this protocol.

The wire format follows [Kitty's OSC 22 specification](https://sw.kovidgoyal.net/kitty/pointer-shapes/).
For example, a shell can request a pointing hand:

```sh
printf '\033]22;pointer\033\\'
```

| OSC 22 payload | Effect |
| --- | --- |
| `pointer` or `=pointer` | Set the current shape, replacing the top stack entry |
| `>wait,pointer` | Push each known shape in order; the final entry is current |
| `<` | Pop one entry; extra names after `<` are ignored |
| Empty or `=` | Replace the current entry with an explicit default reset |
| `?__current__` | Reply with the current shape name, or `0` when none is set |
| `?pointer,unknown,wait` | Reply `1,0,1` for recognized canonical names |
| `?__default__,__grabbed__` | Reply `default,default`, Rustmux's virtual defaults |

Queries may combine special names and support checks in one comma-separated
list. Replies always use OSC 22 with ST (`ESC` backslash), and are queued only to
the requesting child, even when its pane is unfocused or the server is detached.
They never depend on a response from the outer terminal.

All 30 canonical shape names in the specification are supported by the virtual
terminal. Unknown set names are ignored. Unknown names in a push list are skipped;
empty push entries explicitly request a reset. Legacy platform-specific aliases
are not accepted. Each stack holds at most 16 entries; pushing onto a full stack
evicts its oldest entry. Setting a shape keeps previous pushes intact, so a pop
can restore them. An empty reset also retains lower stack entries.

The parser retains its existing 64-byte OSC payload limit, including `22;`.
Oversized controls are consumed without applying a partial operation or replying.
Invalid UTF-8 is ignored. BEL-terminated OSC 22 is also consumed, while replies
use canonical ST. Query output fits the existing per-input-byte reply capacity.

## Display and lifecycle

Focus/window changes, resizing and reconnecting use the selected pane's current
shape. Rendering sends a set or empty reset; it never forwards application
push/pop commands to the outer terminal. Cached shapes suppress duplicate output
on unchanged redraws. Cloning or resizing a screen retains both stacks; saved
text cursors do not own pointer state. Main/alternate switching selects the
corresponding stack, including when the alternate text grid is cleared.
RIS and DECSTR empty both stacks.

Help, History and editing/confirmation prompts display the default pointer,
without changing the underlying application's stacks. Retained exited panes also
display the default. Closing these overlays restores the live pane's shape.
Terminal entry resets the alternate-screen pointer; normal exit, detach and
Session Manager transitions reset it before returning to the main screen.
The outer terminal's main-screen pointer stack is preserved by its screen switch.

Actual pointer appearance requires an outer terminal implementing OSC 22.
The virtual defaults and support replies describe Rustmux's model; they do not
report the emulator's configured default/grabbed shapes or prove that its GUI
supports a requested shape. Optional window-control overrides are documented in
[Mouse Hover Feedback](mouse-hover.md).
Pointer stacks are live terminal state and are not serialized into workspace
snapshots; restored workspaces start fresh child programs with empty stacks.

## Review and verification

Based on reviewed interface-theme commit `94179dd` and the fixed `main` reference
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c`. `main` already supports pointer tracking.
Its set path clears the entire stack; this implementation replaces the top entry,
following [Kitty's implementation](https://github.com/kovidgoyal/kitty/blob/master/kitty/screen.c).
It additionally answers mixed special queries and uses the existing screen model
for ordered buffer switches and resets.

Read `src/pointer.rs`, the two stacks and resize/reset paths in `src/screen.rs`,
then OSC dispatch in `src/parser.rs`. `src/render.rs` caches the shape together
with other output modes. Overlay and retained-pane rendering clear only cloned
state. `src/terminal_device.rs` handles entry/exit reset sequences.

Model tests cover stack eviction, set/pop/reset semantics, malformed and oversized
controls, every split position, reply bounds, main/alternate isolation, resize,
text-cursor save/restore, resets, overlays and renderer caching/pane ownership.
`tests/terminal_loop_pointer_shapes.py` uses actual PTYs, two running probes,
private control sockets and reconnecting clients to check originating-pane
replies, detached/background queries, focus, unchanged process IDs, push/set/pop,
buffer switches, Help/History, resets and detach terminal restoration.

Local validation on Rust 1.99.0: `cargo test --all-targets --locked --offline`
passed 965 tests across 48 targets, with 6 pre-existing ignored tests. All 41
real-PTY scenarios passed, including the new pointer lifecycle scenario. Exact
renderer byte fixtures include the initial default-pointer reset.
Clippy with all targets/features and warnings denied, formatting checks,
`git diff --check` and the mdBook build also passed. After replacing one
test-only temporary vector with an array for Clippy, all four pointer model
tests were rerun successfully.

This review branch has not been pushed. Linux CI and actual GUI pointer appearance
have not been verified; the local tests inspect protocol bytes and runtime state.
This change awaits owner review and does not update the shared acceptance ledger.
