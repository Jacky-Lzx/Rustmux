# Named Session Commands

Rustmux keeps its existing foreground mode when started without arguments. Five
`clap` subcommands expose persistent sessions:

```sh
rustmux new work
rustmux new --detached background
rustmux attach work
rustmux attach
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
parser. Ctrl-B followed by Ctrl-W also detaches the client, then opens the Session
Manager with the current session selected. Enter attaches the selected session;
Esc or `q` reconnects the session that opened the manager. The top window bar
prefixes its window labels with the session name and shows the same identity after
reattachment. When horizontal space is limited, the prefix is clipped before the
active window label. Starting Rustmux without a subcommand retains the original
foreground lifetime, has no detachable background server and does not show a
session prefix.

`attach` validates the private runtime directory and requires the socket to be
owned by the effective user with no group or other permissions. It then performs
the normal versioned handshake before changing terminal modes. A session accepts
one displayed client at a time; panes, screen state and scrollback continue while
no client is attached. A second `attach` exits immediately with an error while
the first client holds the session's advisory lock. The kernel releases that lock
if the client exits or crashes, so reconnecting does not depend on manual cleanup.

When `attach` has no name, it reports an error if there are no live sessions and
connects directly if there is exactly one. With several sessions it opens a
centered session window on the temporary alternate screen. The manager groups
attached sessions before detached sessions and orders names within each group;
the script-oriented `list` command remains purely name-sorted. The table reports
the reliable metadata available from the current endpoint format: connection
state and server PID. Up/Down and `j`/`k` move cyclically, Enter attaches, and
Esc, `q` or Ctrl-C cancels without starting a client. `/` enters a bounded,
case-insensitive name search. Printable bytes are
search text in that mode, including `j`, `k`, `a`, `d` and `q`; Up/Down select
results, Tab completes the selected name, Enter attaches it, and Esc returns to
the complete table. An empty result leaves Enter and Tab inactive. As in the
`main` Session Manager, `a` opens a bounded session-name editor; Enter creates
and attaches the new session, while Esc returns to the table. Pressing `d` once
arms termination for the selected session and changes the footer to a warning.
Only an immediately following `d` terminates it; every other key clears the
pending confirmation. After termination, the refreshed table remains open. The
window follows terminal resizes and restores the previous terminal modes and
screen before attaching or returning.

The same manager is available from an attached session with Ctrl-B Ctrl-W. The
client releases the current session lock before showing it, so selecting another
detached session switches the terminal without stopping either server or its
panes. That current session remains first, is selected initially and uses a
distinct `[CURRENT]` status; other attached sessions follow, then detached
sessions. Killing the session that opened the manager removes the cancel target;
closing the manager then returns to the outer terminal.

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
