# Pane output logging and subscriptions

`read-pane-output`, `subscribe-pane` and `log-pane` expose the bytes read from a
pane's PTY, before terminal parsing. This includes escape sequences, binary
payloads, shell echo and prompts. For plain screen text, use `capture-pane`.
No terminal replies or input queue bytes are added to this stream.

```sh
# In config.toml, enable retention for panes whose final output must be logged:
# remain_on_exit = true

rustmux read-pane-output -s work -p 1
rustmux read-pane-output -s work -p 1 --after 0
rustmux subscribe-pane -s work -p 1 > pane.raw
rustmux log-pane -s work -p 1 --output ./pane.raw
```

The continuous commands require `remain_on_exit = true`, either globally or in
the project's pane entry. They check this before starting. Retention lets the
client read the final output after PTY EOF, including when this is the last
pane. A pane's explicit `remain_on_exit = false` overrides the global setting.
One-shot reads work without retention while the pane still exists.

The default starting point is the current output cursor, so logging includes
future output only. `--after CURSOR` resumes at that byte offset; `--after 0`
requests the available raw tail from the start. Raw output is runtime state and
is not restored from saved sessions or terminal history.

`read-pane-output` returns TOML with these fields:

| Field | Meaning |
| --- | --- |
| `pane` | Stable runtime pane ID, resolved from the current focus if omitted |
| `server_pid` | PID of the server serving the stream |
| `generation` | Process generation, initially zero; increases on respawn |
| `start` | Offset of the first returned byte |
| `next` | Cursor to pass as `--after` in the next request |
| `dropped` | Bytes between the requested cursor and the oldest retained byte |
| `complete` | The server observed PTY EOF |
| `bytes_base64` | Exact raw bytes in standard base64, possibly empty |

An omitted cursor returns the current cursor and no bytes. A cursor ahead of
the stream is rejected. Decoding and joining chunks preserves split UTF-8
sequences and graphics payloads without involving the terminal renderer.

Each pane keeps at most 64 KiB of raw output. Old bytes are evicted. A reader
that falls behind receives `dropped > 0`; continuous clients instead stop with
an error before writing the incomplete chunk. Existing log bytes remain in the
file. A burst larger than the buffer can therefore interrupt logging even if
average output is small. The stream has a fixed memory bound, not a lossless
backpressure guarantee. With 16 windows of 64 panes, raw tail buffers account
for at most 64 MiB, plus one floating pane, one hidden undo pane and transient
response buffers.

Continuous clients poll the bounded control interface every 50 ms after each
read/write. They release the control connection after each request, and never
hold the interactive session lease. Multiple clients can subscribe independently.
Slow stdout or filesystem writes block only that client. The terminal service
continues handling PTYs and input; readers detect any resulting buffer loss.
These are client-side subscriptions, not a server push channel or a persistent
server-side logger.

`log-pane` exclusively creates a new file with permissions restricted to 0600.
It rejects existing files, symlinks and existing special files rather than
appending or truncating. It flushes each output batch, reports write errors and
leaves partial logs available after failure. It does not rotate logs or promise
`fsync` durability. The logging client must remain running; stopping it stops
logging, while detaching the interactive client does not.

Both continuous commands pin the initially resolved pane ID. Moving the pane
between windows or changing focus preserves the stream. They finish successfully
only after reading through PTY EOF. Closing the pane, killing the server or a
transport failure is an error. Respawn clears the raw tail, preserves monotonic
byte offsets and increments `generation`; continuous readers reject a changed
generation or server PID instead of silently switching to another process.
Use a new subscription after respawn. One-shot consumers should check both
server identity and generation when resuming cursors.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

Read `src/pane_output.rs` for bounded storage and cursor semantics, then
`src/pane.rs` for the PTY hook and respawn transition. `src/terminal/control.rs`
resolves IDs and validates retention; `src/control.rs` implements client polling
and file creation. The output PTY scenario checks exact binary bytes and split
UTF-8, final output, focus and pane moves, log permissions and exclusive creation,
slow readers, detached and attached use, retention checks, and respawn cursors.

Local cumulative verification on macOS, 2026-10-01:

- `cargo test --all-targets --locked --offline -- --test-threads=4`: 860 passed,
  zero failed, six existing tests ignored by default, across 46 test targets.
- All 18 real PTY scenarios passed, including the new output scenario.
- `cargo clippy --all-targets --all-features --locked --offline -- -D warnings`,
  Rust formatting, `git diff --check` and the mdBook build passed.

This implementation awaited the owner's review. Linux CI and installed-client
validation were not performed. Logging is opt-in through an explicit running
client; automatic server-side logging and log rotation remain future work.
