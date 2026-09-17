# Named Session Commands

Rustmux keeps its existing foreground mode when started without arguments. Three
`clap` subcommands expose persistent sessions:

```sh
rustmux new work
rustmux attach work
rustmux list
```

`new` validates and binds the private endpoint before forking. The child creates
a new session, leaves the launching terminal's process session with `setsid`,
redirects its standard streams to `/dev/null`, closes inherited descriptors and
owns the listener until the final pane exits. The parent closes its listener copy
without unlinking the child's socket, then enters the ordinary session client.
An already active name is rejected before another server starts.

While attached to a named session, Ctrl-B followed by `d` sends the protocol
`Detach` message and restores the outer terminal. Input earlier in the same read
is delivered first. The shortcut is disabled inside bracketed paste, and all
other prefix combinations remain byte-for-byte input for the server-side command
parser. Starting Rustmux without a subcommand retains the original foreground
lifetime and has no detachable background server.

`attach` validates the private runtime directory and requires the socket to be
owned by the effective user with no group or other permissions. It then performs
the normal versioned handshake before changing terminal modes. A session accepts
one displayed client at a time; panes, screen state and scrollback continue while
no client is attached. A second `attach` exits immediately with an error while
the first client holds the session's advisory lock. The kernel releases that lock
if the client exits or crashes, so reconnecting does not depend on manual cleanup.

`list` prints one live session name per line in sorted order, making its output
suitable for shell scripts. It does not enter terminal mode. An exclusive client
lock proves an attached session is live without touching its socket. For an
unlocked session, `list` makes a short local connection that the detached server
accepts and discards as an incomplete handshake. Invalid files, insecure
endpoints and sockets left behind by terminated servers are omitted.

The nested-PTY integration test creates a named session, records shell state,
detaches, attaches through a second terminal, observes the retained state, exits
the shell and checks terminal restoration and endpoint cleanup.
