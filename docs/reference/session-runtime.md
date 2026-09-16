# Session Runtime State

`terminal::TerminalSession` owns the state that must outlive any one attached
frontend: the configured shell, windows, pane processes, screen and scrollback
models, the last outer terminal height and the one undoable closed pane. The
local terminal path now uses this owner too, so extraction does not introduce a
second multiplexer implementation.

Each call to the attachment loop creates fresh connection-local state such as
the renderer cache, undecoded keyboard queue, prefix state, prompts and the
current history view. Returning `Detached` leaves `TerminalSession` alive. A
later socket frontend starts with a full redraw while observing the same panes
and shell processes. Resize state is written back to the session rather than
remaining on the attachment's stack.

A real-PTY test sets a shell variable through one socket, detaches, connects a
second socket at a different size and exits according to the retained value.
This covers shell identity, ordered pending input and size persistence across
attachments.

This step does not yet service pane PTYs while no frontend is attached. The next
runtime step must keep parsing bounded PTY output in detached mode and accept a
new client from the session listener, so a noisy child cannot fill the kernel
PTY buffer while waiting for reattachment.
