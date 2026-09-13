# Window Model

This is the first model-only part of H05. `window::Windows<T>` owns an ordered
collection of named windows, each containing a caller-supplied value. The CLI
still runs one shell: this module does not yet create PTYs, intercept shortcuts,
draw a window bar or implement an interactive rename prompt.

## Identity and focus

- Creation appends a window and makes it active.
- `WindowId` is stable within its originating collection, independent of the
  window's position and name. IDs start at zero and are never reused, even after
  all windows have been closed. They are not cross-collection or persistent IDs.
- `select` focuses an ID. `select_next` and `select_previous` wrap in creation
  order. An empty collection returns `None`; a single window remains selected.
- Renaming preserves identity, order, contents and focus. Names are opaque
  metadata: empty and duplicate names are allowed. A future UI must handle
  control-character escaping and interactive validation before rendering names.
- Closing an inactive window preserves the active window's identity. Closing
  the active window selects its successor, or its predecessor if it was last.
  Closing the only window leaves the collection empty; creating again works.
- Selecting, renaming or closing an unknown ID returns `NotFound` without changing
  the collection. ID exhaustion fails creation without wrapping or changing focus;
  the supplied content is dropped on this error, as documented by `create`.

## Ownership boundary

`T` has no Clone requirement. It can later contain a PTY, parser and screen.
Switching and renaming operate on metadata and never recreate or clone that
content. `get_mut(id)` lets the event loop process background output without
changing focus. `active_mut()` accesses the currently selected content.

`close` returns the removed `Window<T>` to the caller. It does not kill a process
or wait for it; the caller owns that cleanup decision. Dropping the returned
window drops its content normally, and dropping the collection drops remaining
contents. `into_content` transfers the removed content without cloning it.
No terminal output or other I/O occurs inside this model.

This intentionally separates the current single-pane implementation from the
broader `main` window/pane implementation, which also includes layouts, floating
terminals and persistent sessions. This model is not an H05 feature-acceptance
claim. The next integration must attach per-window PTY/parser/screen state,
continue reading inactive windows, route keyboard input to the active window,
and synchronize display and modes when focus changes.

## Verification

`cargo test --test windows` covers cyclic selection, stale IDs, Unicode names,
all three-window focus/removal combinations, empty collection reuse and ownership
transfer. A parser/screen fixture verifies that an incomplete UTF-8 sequence,
private input modes and background output stay with their originating windows.
The unit test in `src/window.rs` exercises the last available ID and exhaustion.
These are model tests, not interactive multi-window or process-preservation tests.
