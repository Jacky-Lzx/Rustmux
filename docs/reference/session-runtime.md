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

`terminal::serve_session` alternates between an attached frontend and a detached
runtime. While detached, it polls the session listener together with every
visible pane PTY, services one bounded read or write per ready pane and also
keeps the hidden undo pane moving. Output continues through the same parser and
screen model, so a child cannot fill the kernel PTY buffer merely because no
client is displaying it.

The detached runtime observes child exits and signals, removes finished panes
and returns when the final pane exits. A newly accepted socket must complete the
normal handshake before it becomes the next frontend; malformed or abandoned
connections do not terminate the existing session. When an attached session
finishes after its rendered output drains, the server sends the protocol `Exit`
status before closing the connection. Clicking the named-session footer instead
sends `OpenSessionManager` after rendered output drains, releases that client and
leaves the session runtime ready for its next attachment.

The CLI supervisor binds a named endpoint, forks before terminal mode changes,
starts the server in a new process session and connects the original process as
its first client. Later `attach` commands use the same runtime after Ctrl-B `d`
detaches the current client. Ctrl-B Ctrl-W or the clickable footer returns to the
client supervisor, which opens the Session Manager and reconnects the current
session when the manager is cancelled.
