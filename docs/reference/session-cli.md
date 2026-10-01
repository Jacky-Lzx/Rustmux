# Named Session Commands

Rustmux keeps its existing foreground mode when started without arguments. Named-session
`clap` subcommands expose persistent sessions:

```sh
rustmux new work
rustmux new --detached background
rustmux attach work
rustmux attach
rustmux list
rustmux list --long
rustmux kill work
rustmux kill-all --yes
rustmux save-session work
```

`save-session` saves the running workspace; `new` restores its snapshot after the
server stops. History and autosave are optional. See [Session Snapshots](session-snapshots.md)
for configuration, storage and the distinction between reattachment and starting
fresh restored shells.

Additional [script control commands](script-control.md) query panes, send input,
capture text, create windows and splits, and move panes while preserving processes.

`--config PATH` (or `-c PATH`) selects the configuration for foreground startup
and newly created named sessions. It can appear before or after a subcommand:

```sh
rustmux --config ~/.config/rustmux/config-dev.toml
rustmux new work --config ./config-dev.toml
rustmux --config ./config-dev.toml new --detached background
rustmux attach work --config ./config-dev.toml
```

Relative paths resolve from the launching working directory. An explicit file
must exist and contain valid supported configuration; read or validation errors
include its path. Without this option, Rustmux uses
`$XDG_CONFIG_HOME/rustmux/config.toml`, or `~/.config/rustmux/config.toml` when
`XDG_CONFIG_HOME` is unset. A missing default file still uses built-in defaults.
The option does not change `XDG_CONFIG_HOME` for pane processes.

Existing named sessions keep the configuration loaded by their server at creation.
An attaching client's `--config` selection applies when it creates or restores a
session from the Session Manager; it also remains selected when switching between sessions.
`list`, `kill` and `kill-all` do not read configuration files.

`new` validates and binds the private endpoint before forking. The child creates
a new session, leaves the launching terminal's process session with `setsid`,
redirects its standard streams to `/dev/null`, closes inherited descriptors and
owns the listener until the final pane exits. The parent closes its listener copy
without unlinking the child's socket, then enters the ordinary session client.
An already active name is rejected before another server starts.

`new --detached` (or `new -d`) creates the same server without putting the
calling terminal into raw or alternate-screen mode. An internal client using the
saved dimensions (or `24×80` for a fresh session)
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
Manager with the current session selected. Enter attaches a live session or
restores a saved workspace;
Esc or `q` reconnects the session that opened the manager. The top window bar
prefixes its window labels with the session name and shows the same identity after
reattachment. When horizontal space is limited, the prefix is clipped before the
active window label. Starting Rustmux without a subcommand retains the original
foreground lifetime, has no detachable background server and does not show a
session prefix.

`list`/`ls` and the manager include saved workspaces after the server exits.
Detailed listings label them `SAVED`; live and saved copies of the same name
appear only once. `kill-all` ignores saved-only entries. The manager offers
`Restore` for these entries, with its `dd` kill action disabled.

`attach` validates the private runtime directory and requires the socket to be
owned by the effective user with no group or other permissions. It then performs
the normal versioned handshake before changing terminal modes. A session accepts
one displayed client at a time; panes, screen state and scrollback continue while
no client is attached. A second `attach` exits immediately with an error while
the first client holds the session's advisory lock. The kernel releases that lock
if the client exits or crashes, so reconnecting does not depend on manual cleanup.

When `attach` has no name, it reports an error if there are no running or saved
sessions and connects or restores directly if there is exactly one. With several
sessions it opens a centered session window on the temporary alternate screen.
The manager groups attached sessions before detached sessions, then saved
workspaces, and orders live groups by newest connection, using the name as a
stable tie-breaker; the script-oriented `list`
command remains purely name-sorted. The table reports connection state and server
PID, and adds `LAST CONNECTED` when width permits. Current and attached sessions
show `Now`; detached sessions show a compact elapsed time or `—` before their
first user attachment. Saved workspaces have no PID or connection time.
Up/Down and `j`/`k` move cyclically, Enter attaches or restores, and
Esc, `q` or Ctrl-C cancels without starting a client. `/` enters a bounded,
case-insensitive name search. Printable bytes are
search text in that mode, including `j`, `k`, `a`, `d` and `q`; Up/Down select
results, Tab completes the selected name, Enter attaches or restores it, and Esc
returns to the complete table. An empty result leaves Enter and Tab inactive. As in the
`main` Session Manager, `a` opens a bounded session-name editor; Enter creates
and attaches the new session, while Esc returns to the table. Pressing `d` once
arms termination for a selected live session and changes the footer to a warning.
Only an immediately following `d` terminates it; every other key clears the
pending confirmation. After termination, the refreshed table remains open. The
window follows terminal resizes and restores the previous terminal modes and
screen before attaching or returning.

The same manager is available from an attached session with Ctrl-B Ctrl-W. The
client releases the current session lock before showing it, so selecting another
detached session switches the terminal without stopping either server or its
panes. That current session remains first, is selected initially and uses a
distinct `[CURRENT]` status; other attached sessions follow, then detached
sessions, with recent connections first in both groups, then saved workspaces.
Killing the session that opened the manager removes the cancel target;
closing the manager then returns to the outer terminal.

Each successful user attachment writes a private per-session timestamp record.
The internal handshake used by `new --detached` does not create one. Starting a
fresh server with an old name clears stale connection metadata, and normal
session cleanup removes the record with the socket, lock and PID sidecars.

`list` prints each running or saved session name once per line in sorted order,
making its output suitable for shell scripts. `list --long` (or `ls -l`) instead prints an aligned
`SESSION`, `STATUS`, `PID` and `LAST CONNECTED` table. It uses the Session
Manager order: attached sessions first, then detached sessions, then saved
workspaces, with recent connections first in the live groups. Attached sessions show `Now`; sessions without
a recorded connection show `—`. Neither form enters terminal mode. An exclusive
client lock proves an attached session is live without touching its socket. For an
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
