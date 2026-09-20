# Kitty Keyboard Protocol

Rustmux virtualizes the
[Kitty progressive keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
for each pane. A
child may set flags with `CSI = flags ; mode u`, push flags with
`CSI > flags u`, pop them with `CSI < count u`, and query the active flags with
`CSI ? u`. Query replies use `CSI ? flags u` and travel through the pane's
bounded reply queue.

Only the five currently defined enhancement flags are retained. The set mode is
replace by default, `2` adds flags, and `3` removes flags. A missing pop count is
one; popping an empty stack restores zero. Each main and alternate screen has an
independent stack, bounded to 32 entries. When the bound is reached, the oldest
saved entry is discarded. RIS clears both stacks and their active flags.

## Pane and outer-terminal behavior

The active pane's flags are synchronized to the outer terminal with
`CSI = flags u`. Rustmux uses set rather than push so pane switches and redraws
cannot grow the outer terminal's stack. Switching panes or main/alternate
screens restores that view's requested flags. Exit cleanup explicitly sends
`CSI = 0 u`.

Rustmux-owned input modes, including the prefix-command state, prompts, history
and help, temporarily request flags zero from the outer terminal. This does not
change the child's saved request; the active flags are restored when the local
UI closes.

When the outer terminal sends Kitty-encoded key events, an encoded Ctrl-B still
acts as Rustmux's prefix. The following encoded shortcut is interpreted by the
multiplexer, including its shifted alternate code point. Prefix key releases
are consumed locally. In locked mode, other encoded input remains byte-for-byte
input for the child. After a prefix, an unrecognized encoded shortcut preserves
the literal Ctrl-B and the original sequence.

This support depends on an outer terminal that implements Kitty's keyboard
protocol. Rustmux does not synthesize encoded events when the outer terminal
continues to send legacy key bytes.

## Verification

Parser tests cover replace/add/remove, push/pop bounds, query replies,
main/alternate isolation, malformed commands and RIS. Renderer tests replay
flag changes and verify mode caching. The nested-PTY test checks a real child
query, encoded Ctrl-B shortcut handling, pane-switch synchronization and cleanup.
