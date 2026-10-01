# Terminal device opening

Interactive foreground sessions, attached clients and the standalone Session
Manager open the user's terminal through `src/terminal_device.rs`. The opener
requires terminal input and output on the same device. Redirected streams and
different input/output terminals fail before entering raw mode.

On macOS, device paths now come from `fcntl(F_GETPATH)` on the input descriptor.
Other platforms retain `nix::unistd::ttyname`. Input/output identity is checked
using `fstat` filesystem and device numbers instead of resolving and comparing
two path names. The reopened file must still match that original identity.
The generic PTY master paths are rejected: reopening a master would create a
different PTY rather than reconnecting to the frontend's slave.

Rustmux opens the device with a separate nonblocking file description, allowing
polling without changing the parent shell's input/output file flags. Raw mode,
alternate-screen setup and restoration continue through the existing lifecycle.
Inspection, path resolution and open errors now include the failed operation;
an open failure also names the device path.

## Why change macOS path resolution?

The Apple libc implementations of
[`ttyname_r`](https://github.com/apple-oss-distributions/Libc/blob/main/gen/FreeBSD/ttyname.c)
and [`devname_r`](https://github.com/apple-oss-distributions/Libc/blob/main/gen/devname.c)
show that terminal path lookup scans `/dev`. `ttyname_r` maps an unsuccessful
device lookup to `ERANGE`, including lookup failure with an adequate buffer.
Simply enlarging the buffer does not address every such failure.

Earlier cumulative validations recorded two occasional client-attachment
failures with `Result too large (os error 34)` before the first terminal frame.
Those failures did not retain operation-level diagnostics. Their exact cause
remains unconfirmed. A separate local probe opening and resolving 12,000 slave
PTYs across six threads did not reproduce a spontaneous lookup failure.

This increment removes the macOS opener's dependency on that directory lookup.
A deterministic fault-injection scenario forces libc `ttyname_r` to return
`ERANGE`. The reviewed `d710efa` binary fails before its first foreground frame
under that injected fault. The updated binary starts, accepts shell input,
detaches and reattaches, and opens the standalone manager under the same fault.
This proves the handled failure mechanism; it does not retrospectively establish
the cause of either earlier occasional error.

## Review and verification

This increment follows reviewed script-focus commit `d710efa`, now merged into
`main-human`. It changes only terminal device opening and its validation, with
no new configuration or session protocol fields.

Start with `open_terminal` and the platform-specific `terminal_path` in
`src/terminal_device.rs`. Unit tests use independent PTY pairs to check redirected
streams, mismatched terminals, macOS master rejection, bidirectional slave I/O
and preservation of parent file flags while the reopened file is nonblocking.

`tests/terminal_loop_device.py` checks the real binary through the existing outer
PTY harness. On macOS it builds `tests/fixtures/ttyname_erange.c` into a local
interposer and a probe, verifies the probe's normal and injected results, then
tests foreground startup, three attach/detach cycles and manager-to-client
handoff. Each normal exit verifies restored terminal attributes. It filters the
manager by a unique session name and only cleans up the scenario's owned server.
The fixture requires a C compiler on macOS; non-macOS runs exercise the same
terminal lifecycle without Darwin fault injection.

This branch awaits owner review and does not update the shared acceptance ledger.
Linux CI and installed-client validation have not been performed.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 910 passed,
  zero failed, six existing ignored tests, across 47 test targets.
- All 27 real PTY scenarios passed, including the injected device-lookup failure,
  repeated attachment, manager handoff, History and snapshot restoration.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  `cargo fmt --check`, `git diff --check` and `mdbook build` passed.
- The first full run failed in the existing History scenario because `/bin/sh`
  printed `child setpgid ... Operation not permitted` between a marker and
  `stty size` output. The shell remained alive and reported the expected size,
  but the contiguous-text assertion failed. The isolated History test and final
  four-thread full run passed without source or test changes. That separate
  shell job-control error remains unexplained; it was not an `ERANGE` failure.
