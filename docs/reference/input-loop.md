# Input Forwarding Loop

This is the H02 implementation candidate, awaiting owner review. Read
`src/main.rs`, then `src/terminal.rs`, its queue tests, and
`tests/terminal_loop.py` (launched by `tests/terminal_loop.rs`).

## Scope and data flow

The CLI selects an executable from RUSTMUX_SHELL, SHELL or /bin/sh and starts
PtyShell with the initial outer-terminal dimensions. It requires terminal stdin
and stdout pointing to the same device. It opens that actual device separately:
this avoids modifying the parent's shared file status flags, and avoids polling
macOS's /dev/tty indirection. No shell command string is interpolated at startup.

The outer terminal enters raw mode so Ctrl-C and other input arrive as bytes.
An alternate screen preserves the previous screen contents. A single-threaded
poll loop transfers input to the PTY and output back to the terminal, without
parsing text or terminal control sequences. UTF-8 and paste bytes stay unchanged.
The inner PTY's line discipline and shell handle editing and keyboard signals.

Both descriptors are nonblocking. Each direction has a 64 KiB queue; input reads
pause when its destination queue is full. Poll watches writable events only when
there is pending data. Each read is at most 8 KiB, writes retain unsent tails,
and Interrupted/WouldBlock retry on subsequent iterations without losing bytes.
An idle loop waits up to 50 ms, which bounds checks for child exit and termination
signals; this is not a rendering frame interval and ready I/O is handled immediately.

## Exit and terminal restoration

The child status and PTY end-of-output are tracked independently. Normal exit
flushes queued output. If descendants retain the slave, the loop stops once the
direct shell has exited and currently available output is drained; it does not
wait for detached descendants. A live process retaining execution after closing
its PTY gets a one-second exit grace period after observable EOF, then an error
and cleanup. macOS may keep the controlling PTY alive until that process exits,
so closed standard streams alone do not necessarily produce EOF.

Raw termios is saved before modification and restored on normal return, errors
and unwinding. Cleanup also resets common mouse/bracketed-paste modes, text style,
cursor visibility and the alternate screen. Terminal control writes have a
500 ms deadline so a blocked output device cannot prevent termios restoration.
Cleanup cannot restore a physically disconnected device, nor recover from SIGKILL
or abort. The display reset assumes a conventional outer terminal; arbitrary
pre-existing private modes and nested alternate-screen state are not captured.

signal-hook 0.3 installs flag-only handlers for HUP, TERM, INT and QUIT, without
creating threads. The event loop returns 128 + signal, restores terminal state,
and then drops the PTY owner to stop/reap its direct child. This dependency avoids
handwritten signal handlers. Dynamic resize, suspend/resume and whole-process-tree
supervision remain out of scope. The public run function is intended for the CLI
process, with exclusive terminal ownership and single-threaded startup.

## Verification

The nested-PTY test drives the real binary: Chinese text, erase, Ctrl-C interrupting
sleep, 200 KB output, final output plus exit code 7, invalid shell startup,
non-terminal input rejection, SIGTERM, output backpressure, and a live process
closing its PTY. A supervisor retains the outer controlling session so macOS does
not revoke its terminal before attributes can be checked. All termios settings
are compared except the kernel-maintained PENDIN transient state.

Unit tests force partial writes, Interrupted and WouldBlock, queue saturation,
and an operation error while a raw-terminal guard is active, checking restoration.
The previous PTY lifecycle tests remain in CI. Python 3 and PTY/process permissions
are required. Local validation is on macOS; Linux results require the CI run.
