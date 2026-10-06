# OSC 8 Hyperlinks

Child applications can associate text with a URI using
[OSC 8](https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda).
Rustmux stores the association on each painted cell and reconstructs it during
rendering. Clicking and URI handling belong to the outer terminal. No Rustmux
configuration switch is required; the outer terminal must support hyperlinks.

For example, inside a pane:

```sh
printf '\033]8;id=docs;https://example.com\033\\Documentation\033]8;;\033\\\n'
```

## Behavior

The parser accepts `OSC 8 ; parameters ; URI ST`, with either ST (`ESC` followed
by a backslash) or BEL termination, including commands split across reads. An empty URI closes the
current link. Opening a link affects subsequent painted cells, leaving existing
cells unchanged. SGR reset and cursor save/restore do not change the current link.

A nonempty `id` and URI identify one group within the same pane. Different panes
receive different emitted IDs, including when their child ID and URI are equal.
Each anonymous opening, including `id=`, receives a new identity. Repainting,
focus changes and reconnect preserve identities while the server remains alive.
Unknown parameter keys are accepted and ignored. Rustmux emits only its own
namespaced `id`, using ST termination.

Links travel with cell contents through insert/delete operations, scrolling,
primary history, primary width reflow and alternate-screen clipping. Wide-cell
placeholders and late variation selectors retain the leader's link. Overwrite
replaces the association; erased cells and newly exposed blanks have no link.

Switching between primary and alternate grids closes the current open link;
retained cells keep their associations. DECSTR closes it without clearing cells.
RIS clears both grids, history and the link pool. This buffer/reset policy follows
[Kitty's screen implementation](https://github.com/kovidgoyal/kitty/blob/master/kitty/screen.c).

Pane borders, titles, window bars, footer controls, editing prompts and shortcut
help text do not inherit an application's open link. Live History displays retain
cell links even when search/selection changes colors. Capture, copying and saved
history remain text-only with their existing optional colors; OSC 8 controls are
never serialized into snapshots or replayed from saved history.

## Bounds and output recovery

URI input is limited to 2,083 bytes and parameters to 256 bytes. Both accept
printable non-space ASCII; URI-encode spaces and non-ASCII text. Malformed,
duplicate-ID, control-containing and overlong commands close the current link
without changing previously painted cells. CAN/SUB cancel an unfinished command
without applying it. The existing lack of 8-bit C1 command support still applies.

Each pane keeps at most 1,024 pooled links shared by cells and snapshots. At the
limit, it releases entries without external references; if all entries remain
live, a new opening is rejected and subsequent text is unlinked. Existing cells
remain intact. Old explicit identities may be regenerated after their last
references are gone and their pool entries have been reclaimed.

Only OSC 8 uses the parser's larger, bounded 2,342-byte payload buffer. Other
OSC/DCS operations retain their 64-byte limits and the existing reply bound.
The payload is stored outside the small parser state to avoid copying a URI for
each consumed byte. URI data is shared rather than duplicated per cell.

The renderer closes links at every emitted span/row boundary. Its partial-row
cost calculation includes hyperlink bytes; changes to links alone repaint cells.
Initial/invalidated frames and terminal entry/exit explicitly close any outer
link state. Output errors invalidate the render cache for a complete repaint.

If complete-row link transitions would exceed 1 MiB in a frame, that entire frame
renders as ordinary text. Changing into or out of this fallback forces all rows
to repaint, removing old physical links or restoring them. Model metadata remains
intact, and the existing overall frame cap is unchanged.

## Review and verification

Based on reviewed hover commit `8d5869b`, with `main` reference fixed at
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c`. The reference uses a separate
`HyperlinkTracker` and reconstructs pane-scoped IDs. This increment puts links in
the human track's actual `Cell` model, so existing edit/history/reflow operations
carry the metadata. Anonymous grouping, input/pool/output bounds and ignored
unknown parameters are intentional choices described above.

Reading order: `src/hyperlink.rs`, `src/style.rs`, OSC input in `src/parser.rs`,
write/reset/resize paths in `src/screen.rs`, then `src/render.rs` and interface
composition/cleanup. `tests/hyperlinks.rs` checks fragmented parsing, grouping,
invalid inputs, editing, width changes, history/reflow, pane composition,
link-only redraws, write-error recovery and output fallback. Unit tests cover
pool reclamation, parser bounds, prompts and text-only snapshot persistence.

`tests/terminal_loop_hyperlinks.py` runs two real child probes through a named
server, independently inspects OSC output, and checks pane isolation, interface
text, alternate buffers, History, capture, reconnect, resize, cleanup and unchanged
child PIDs.

Local cumulative validation on Rust 1.99.0 passed 991 tests across 49 targets,
with 6 pre-existing ignored tests. All 43 real-PTY scenarios passed. All-target,
all-feature Clippy with warnings denied, formatting, `git diff --check` and the
mdBook build passed.

GUI clicking and URI launching have not been tested. This branch has
not been pushed or tested in Linux CI and awaits owner review; it does not update
the shared acceptance ledger.
