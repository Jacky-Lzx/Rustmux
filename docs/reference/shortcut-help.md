# Shortcut Help

Press Ctrl-B followed by `?`, or click `? Help` in the NORMAL footer, to open a
read-only reference for the shortcuts implemented by this branch. The panel is
drawn over a composed copy of the active view; it does not modify any child
screen, cursor, terminal mode or scrollback state.

The Help hint is reserved when the NORMAL footer is laid out. On a narrow
terminal, complete middle hints may be omitted so that `? Help` remains visible;
no hint is split. On terminals too narrow for the complete Help hint, the normal
left-to-right fallback still applies.

The panel lists window, pane, history and editing actions. Named sessions also
show Ctrl-W for the Session Manager; local unnamed processes omit that entry.
Ctrl-h/j/k/l in the panel means the control-modified directional keys used to
resize the nearest pane separator.

Press Esc, `q` or `?` to close Help. Escape is delayed briefly so complete CSI,
SS3 and mouse reports are consumed as one sequence. All other bytes, including
bracketed-paste contents and mouse reports already in flight, remain inside the
modal panel and never reach a child process.

Unit tests cover bounded rendering, session-specific content, narrow dimensions,
paste isolation and delayed Escape. The nested-PTY test opens Help from both the
clickable footer and Ctrl-B `?`, verifies modal input isolation, closes it through
both `q` and `?`, and then confirms the shell still receives ordinary commands.
