# Backarrow Key

CSI ? 67 h enables DECBKM, making the outer terminal's Backspace key send BS
(`0x08`) to the child. CSI ? 67 l resets the mode, making Backspace send DEL
(`0x7f`). This affects keyboard encoding only; output BS continues to move the
model cursor left. See the
[XTerm control-sequence reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html).

Rustmux stores this as global per-pane input state. Cursor saves, alternate-screen
switches and resize preserve it; RIS and DECSTR reset it. The active pane's state
is synchronized to the outer terminal, and pane switches restore the newly active
request. Prompts and shortcut help request the reset form. Exit cleanup also emits
CSI ? 67 l. As with cursor and keypad modes, Rustmux forwards the resulting input
byte rather than translating incoming BS or DEL.

DECRQM reports mode 67 as set or reset. Model and rendering tests cover split
parsing, reset and replay behavior. The nested-PTY test models an outer terminal
by sending BS and DEL in response to the corresponding rendered mode request; it
does not simulate a physical Backspace key or override terminal-specific settings.
