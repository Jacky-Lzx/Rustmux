# Normal Focus Arrows

Normal mode accepts configured arrow bindings that move pane focus:

```toml
[keybinds.normal]
left  = { actions = ["focus-left"],  display = "help" }
down  = { actions = ["focus-down"],  display = "help" }
up    = { actions = ["focus-up"],    display = "help" }
right = { actions = ["focus-right"], display = "help" }
```

Enter Normal with the configured prefix (Ctrl-B by default). Each arrow moves
focus to the geometric neighbor on that axis and keeps Normal active, allowing
several moves without repeating the prefix. If no neighbor exists, focus stays
where it is and Normal remains active. Existing one-shot letter focus bindings
retain their configured behavior.

An arrow may target a different direction, and an explicit Locked transition
ends Normal after the move:

```toml
[keybinds.normal]
right = { actions = ["focus-left", { action = "switch-mode", mode = "locked" }] }
```

Supported chains consist of one `focus-left`, `focus-down`, `focus-up` or
`focus-right` action, optionally followed by `switch-mode` to `locked`.
Unsupported chains remain ignored and are reported by `check-config --strict`;
Rustmux does not execute a supported fragment of such a chain.

`display = "always"` (also the default when omitted) exposes the binding in the
footer and Help, `help` limits it to Help, and `hidden` hides both presentations
while keeping the physical key active. Footer and Help clicks dispatch the same
binding, including its configured destination and mode transition. Configured
Normal left/right arrows also execute from Help; PageUp/PageDown and the pager
click targets remain available for navigating Help pages.

Legacy CSI arrows, application cursor SS3 arrows, and unmodified Kitty arrow
press/repeat events use the binding. Release events do not move focus. Modified
arrows and arrows without a binding retain Normal's existing literal-prefix
fallback. In Locked mode, arrows continue to go to the child application.
Arrows are not added to the default keymap implicitly. `clear_defaults = true`
works with explicit arrow bindings; an arrow-only Normal map is also valid for
`default_mode = "normal"` with a configured Locked-to-Normal entry.

Hot reload updates these bindings at the existing safe Locked boundary. Pane
identities and child processes remain unchanged. The real PTY regression uses
four live processes in a grid, checks repeated moves, Help/footer actions,
visibility, remapping after reload, and a child input handshake proving that
local arrow events were consumed by Rustmux.
