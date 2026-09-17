# Named Session Commands

Rustmux keeps its existing foreground mode when started without arguments. Five
`clap` subcommands expose persistent sessions:

```sh
rustmux new work
rustmux new --detached background
rustmux attach work
rustmux list
rustmux kill work
rustmux kill-all --yes
```

`new` validates and binds the private endpoint before forking. The child creates
a new session, leaves the launching terminal's process session with `setsid`,
redirects its standard streams to `/dev/null`, closes inherited descriptors and
owns the listener until the final pane exits. The parent closes its listener copy
without unlinking the child's socket, then enters the ordinary session client.
An already active name is rejected before another server starts.

`new --detached` (or `new -d`) creates the same server without putting the
calling terminal into raw or alternate-screen mode. An internal `24×80` client
performs the initial handshake, requests detach and waits until the server has
created its shell and entered detached operation. The first real attachment
replaces that bootstrap size with its current terminal dimensions.

Every pane process inherits `RUSTMUX=1`. Invoking foreground Rustmux, ordinary
`new`, or `attach` under that marker fails with `nested Rustmux sessions are not
supported` before opening or changing a terminal. `list`, `kill`, `kill-all` and
`new --detached` remain usable because they do not attach another interactive
client to the current pane.

While attached to a named session, Ctrl-B followed by `d` sends the protocol
`Detach` message and restores the outer terminal. Input earlier in the same read
is delivered first. The shortcut is disabled inside bracketed paste, and all
other prefix combinations remain byte-for-byte input for the server-side command
parser. The top window bar prefixes its window labels with the session name and
shows the same identity after reattachment. When horizontal space is limited,
the prefix is clipped before the active window label. Starting Rustmux without a
subcommand retains the original foreground lifetime, has no detachable background
server and does not show a session prefix.

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

`kill` terminates the named server whether its client is attached or detached.
It verifies that the private PID record is currently locked by that server before
sending `SIGTERM`, then waits for the socket and its sidecars to be removed. A
stale PID file is rejected, so its numeric contents cannot target an unrelated
process after a PID has been reused.

`kill-all` applies the same verified termination path to every live session.
Without an option it prints the number of running sessions and accepts only `y`
or `yes` as confirmation; EOF and every other response abort without changing a
session. `--yes` (or `-y`) skips the prompt for scripts, and `ka` is a short alias.
The command attempts every session even if one termination fails and reports the
individual failures. With no live sessions it prints `no sessions` and succeeds.

The nested-PTY integration test creates a named session, records shell state,
detaches, attaches through a second terminal, observes the retained state, exits
the shell and checks terminal restoration and endpoint cleanup. It also kills
attached and detached sessions, verifying client restoration and complete
endpoint cleanup.
