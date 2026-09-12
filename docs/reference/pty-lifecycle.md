# PTY Lifecycle

`PtyShell` in `src/pty.rs` owns one interactive shell and the master side of its
pseudoterminal. This module implements H01. The CLI now uses it through the
[input forwarding loop](input-loop.md). The PTY module itself does not provide
screen rendering, a daemon or session persistence. Its `resize` method updates
nonzero row/column dimensions; the kernel notifies the foreground process group.

## Ownership and startup

Read `Cargo.toml`, `src/pty.rs`, then `tests/pty_lifecycle.rs`. The `nix` dependency
provides typed PTY allocation and descriptor operations; only `fs`, `term` and
`process` features are enabled. Rust's `Command` owns spawning and the synchronous
exec-error channel rather than introducing a handwritten fork/exec protocol.

Startup rejects zero rows/columns and allocates a PTY with an explicit initial
size. Both ends are duplicated using `OwnedFd::try_clone`. Its current Unix
implementation skips fd 0–2 and sets close-on-exec; the closed-standard-streams
integration test checks the master descriptor's number and flag, plus successful
shell startup. Skipping fd 0–2 is an implementation dependency rather than an
explicit guarantee in the method's public documentation. This also works
when the caller has closed stdin, stdout or stderr. The slave is connected to all
three child standard streams. Before exec, the safe `nix::unistd::setsid` wrapper creates the new session and
`TIOCSCTTY` makes the slave its controlling terminal. The hook performs only system
calls and allocation-free error construction; see Rust's
[pre_exec safety contract](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#tymethod.pre_exec).

The shell receives `-i`; its environment and working directory are inherited.
An explicit executable is required by the API; PATH lookup is handled by Command.
Invalid paths fail without fallback. User shell selection and config integration
belong to a later change. The parent drops every slave copy after spawn and retains
only the master. On spawn failure, descriptor ownership unwinds automatically and
Command reports the error after handling its failed child.

## Exit and I/O

`Read` and `Write` expose the master byte stream for future event-loop integration.
They are blocking; the tests use the borrowed master fd to enable nonblocking reads.
Linux PTY read EIO is normalized to end-of-file to match macOS hangup behavior.
`try_wait` reaps without blocking and caches the exit status; `wait` may block and
requires the caller to drain PTY output first. Shell exit can wait for terminal
output to drain even when the buffer is not full.

`terminate` closes the master, checks for exit, sends SIGKILL to a still-running
direct shell, then waits to reap it. It is idempotent and reports cleanup errors.
Drop performs the same cleanup best-effort, including during unwinding; errors
cannot be reported from Drop. Operations on the closed master return NotConnected.
No process-wide signal handlers or outer-terminal settings are changed.

## Limits to review

- Create PTYs during single-threaded startup. `openpty` has a short interval before
  the original descriptors are replaced with close-on-exec copies. Concurrent
  unrelated fork/exec operations could inherit those originals. Before adding a
  multithreaded process-launch path, replace this allocation strategy or establish
  coordinated spawning; the present API does not solve cross-library inheritance.
- Cleanup owns and reaps the direct shell. Closing the terminal causes normal
  terminal hangup behavior, but it does not guarantee termination of detached
  descendants. Whole-session process supervision is outside H01.
- SIGKILL cleanup is a final resource-reclamation operation, not a graceful user
  exit protocol. A child stuck in uninterruptible kernel I/O can delay waiting.
- macOS is locally validated. Linux validation is configured in CI but must be
  confirmed by a Linux run; compilation and behavior are not inferred from macOS.

## Verification

Run `cargo test --all-targets --locked`. The integration test checks invalid shell
paths and directories repeatedly without descriptor growth, zero-size rejection,
startup with closed standard descriptors, session and foreground process-group
ownership, three terminal streams, `/dev/tty` access, initial dimensions, exit code
7, cached wait status, explicit termination, closed-master errors, and Drop cleanup.
It checks that reaping is complete with ECHILD and that the master is closed.
Read and exit polling use five-second deadlines so ordinary regressions fail
instead of waiting indefinitely. The final destructor wait still has the kernel
blocking limitation described above.
