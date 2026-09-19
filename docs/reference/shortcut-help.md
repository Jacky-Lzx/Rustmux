# Shortcut Help

Press Ctrl-B followed by `?`, or click `? Help` in the NORMAL footer, to open an
actionable reference for the shortcuts implemented by this branch. The panel is
drawn over a composed copy of the active view; it does not modify any child
screen, cursor, terminal mode or scrollback state.

The Help hint is reserved when the NORMAL footer is laid out. On a narrow
terminal, complete middle hints may be omitted so that `? Help` remains visible;
no hint is split. On terminals too narrow for the complete Help hint, the normal
left-to-right fallback still applies.

The panel lists window, pane, history and editing actions. Pressing a displayed
key closes Help and sends its action through the same dispatch table as a normal
Ctrl-B shortcut. Clicking either the key or its label does the same; in grouped
entries such as `n/p`, clicking the exact key selects that action while clicking
the label selects the first action. A drag cancels the click.

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

Unit tests cover bounded pagination, exact grouped-key clicks, drag cancellation,
session-specific content, paste isolation, delayed Escape and correspondence
between displayed actions and the normal dispatch table. The nested-PTY test
executes New through both a panel click and keyboard input, verifies modal input
isolation, and confirms the shell still receives ordinary commands afterward.
