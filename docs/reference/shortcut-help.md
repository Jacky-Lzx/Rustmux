# Shortcut Help

Press Ctrl-B followed by `?`, or click `? Help` in the NORMAL footer, to open an
actionable reference for the shortcuts implemented by this branch. The panel is
drawn over a composed copy of the active view; it does not modify any child
screen, cursor, terminal mode or scrollback state.

The Help hint is reserved when the NORMAL footer is laid out. On a narrow
terminal, complete middle hints may be omitted so that `? Help` remains visible;
no hint is split. On terminals too narrow for the complete Help hint, the normal
left-to-right fallback still applies.

The default Normal panel groups commands under `Window`, `Pane`, `History` and `General`
headings. A heading is repeated when its group continues in another column or
on another page; terminals with only one content row omit headings so a command
remains actionable. Pressing a displayed key closes Help and sends its action
through the same dispatch table as a normal Ctrl-B shortcut. Clicking either the
key or its label does the same; in grouped entries such as `n/p`, clicking the
exact key selects that action while clicking the label selects the first action.
A drag cancels the click.

The panel follows the footer's Catppuccin Mocha visual language: shortcut keys
are bold Pink, action labels use sentence case in Lavender, and the Lavender
border surrounds a Pink title. Pagination and close guidance remain muted on a
Surface background so they do not compete with the actionable rows.

Named sessions also show Ctrl-W for the Session Manager; local unnamed processes
omit that entry. Ctrl-h/j/k/l in the panel means the control-modified directional
keys used to resize the nearest pane separator.

When every command does not fit, Left/Right, Page Up/Page Down, the mouse wheel,
or the panel's page controls wrap through the available pages. Resizing recomputes
the page size and clamps the current page.

Press Esc, `q` or `?` to close Help without an action. Escape is delayed briefly
so complete CSI, SS3 and mouse reports are consumed as one sequence. Unknown
keys and bracketed-paste contents remain inside the modal panel and never reach
a child process.

Unit tests cover grouped column continuation, bounded pagination, exact
grouped-key clicks, drag cancellation, session-specific content, paste isolation,
delayed Escape and correspondence between displayed actions and the normal
dispatch table. The nested-PTY test executes New through both a panel click and
keyboard input, verifies modal input isolation, and confirms the shell still
receives ordinary commands afterward.

## Binding visibility

`display` controls presentation independently of binding execution in Locked,
Normal, Pane, Resize, Move, Tab, Session and History:

| Value | Footer | Help | Keyboard binding |
| --- | --- | --- | --- |
| `always` | Shown when a complete hint fits | Shown | Active |
| `help` | Omitted | Shown | Active |
| `hidden` | Omitted | Omitted | Active |

Omitting `display` keeps the binding visible. With no display overrides, existing
curated footer hints and the Normal help layout are preserved. In a mode with
display overrides, explicit entries appear before implicit defaults; width still
limits the footer. `help-menu` and `never` are compatibility aliases for `help`
and `hidden`. Invalid values or types fail configuration loading and
`check-config --strict`. Supported display metadata does not suppress warnings
about unsupported actions.

For example, this hides a default shortcut from the footer, preserves it in Help,
and keeps its existing action:

```toml
[keybinds.normal]
c = { display = "help" }
```

An override without `actions` requires an existing supported binding. With
`clear_defaults = true`, configure that binding's actions explicitly.

```toml
[keybinds.normal]
N = { actions = ["new-window", { action = "switch-mode", mode = "locked" }], display = "help" }
"?" = { actions = ["show-help", { action = "switch-mode", mode = "locked" }], display = "always" }

[keybinds.pane]
"?" = { actions = ["show-help"], display = "always" }

[keybinds.history]
H = { actions = ["show-help"], display = "always" }
y = { actions = ["copy-history"], display = "help" }
```

Pane, Resize, Move, Tab, Session and History accept a single `show-help` action.
Their panel lists that mode's actual supported bindings, including arrow keys
and alternate bindings. A displayed key or label click closes Help and executes
the original physical binding in its original mode. Hidden physical shortcuts
also continue to work in the panel. Esc, `q` and `?` remain panel close keys;
Left/Right and Page Up/Page Down remain pagination controls. Arrow rows can still
be clicked to execute their bindings.

Closing a mode-specific panel resumes that mode. Normal Help retains its existing
behavior of returning to Locked on cancellation, regardless of display metadata. History Help keeps the same frozen snapshot,
search and selection. While typing a search query, action-like characters and
pasted text remain query input. A terminal resize closes the History snapshot
and its Help together. Unnamed processes omit session-only actions.

Visible footer hints and their click targets are generated from the same list,
so `help`/`hidden` entries have no leftover footer click target. Applying a config
reload also updates the display metadata together with the bindings.
