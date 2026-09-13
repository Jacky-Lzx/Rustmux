# Changed-cell Rendering Measurements

Local macOS arm64 comparison against row rendering at `8d75e26`, using the same
`examples/render_bench.rs` on both revisions. Run with:

```sh
cargo run --release --example render_bench --locked --offline
```

The benchmark uses a 24x120 grid. Each case alternates between two prepared
screens for 4,000 frames, repeats five times, and reports median nanoseconds per
frame. Timing includes row comparison, plan selection, byte encoding into a
reused Vec and cache updates. Setup/parsing is outside the timed loop. It does
not measure a PTY, terminal emulator, display latency or interactive frame rate.

| Case | Row bytes/frame | Cell bytes/frame | Row µs/frame | Cell µs/frame |
| --- | ---: | ---: | ---: | ---: |
| single | 175 | 57 | 19.06 | 19.46 |
| status | 176 | 76 | 18.43 | 22.65 |
| scattered | 176 | 176 | 18.19 | 23.77 |
| scroll | 3088 | 3088 | 14.34 | 21.13 |

- `single`: one character in the middle of a line changes.
- `status`: two separated text spans on the last row change.
- `scattered`: every fourth cell on one row changes; whole-row fallback wins.
- `scroll`: all rows change to the next row's fill character, modeling dense scrolling.

Sparse changes reduce output by about 67% and 57% in these fixtures. Dense cases
retain the row baseline's output size. CPU timings in this run do not show a
general improvement; planning adds work and results also vary with system load.
The benefit is reduced terminal traffic, with a CPU tradeoff. This is a local
microbenchmark, not a cross-platform performance guarantee. Keep the harness for
future comparisons before introducing model-level dirty-cell tracking.

## SSH input-to-frame experiment

`scripts/ssh-render-latency.py` is a manual experiment, separate from `cargo test`
and CI. It runs both release binaries through real SSH connections to a temporary
loopback `sshd`, with Rustmux running **on the server side** on a 24x120 PTY. An
asyncio proxy adds a fixed propagation delay and caps encrypted TCP payload bytes
per second independently in each direction. SSH compression is disabled.
Temporary keys, a pinned host key and an isolated server configuration are used;
the script does not change system SSH configuration or `~/.ssh`.

Build the row baseline (`8d75e26`) and the cell candidate (`76c7b6e`) in separate
checkouts with `cargo build --release --locked`, then run from the cell checkout:

```sh
/usr/bin/python3 scripts/ssh-render-latency.py \
  --row-binary /absolute/path/to/row/target/release/rustmux \
  --cell-binary "$PWD/target/release/rustmux" \
  --output /tmp/ssh-render-latency.json
```

Python 3.8+, OpenSSH client/server and `ssh-keygen` are required, along with
permission to create PTYs, launch processes and listen on loopback sockets.
The recorded run used macOS arm64 and Homebrew OpenSSH. Use `--ssh`, `--sshd`
and `--keygen` to specify executable paths if they are absent from `PATH`.
`--profile local` runs only the local profile; `--samples` and `--interval-ms`
change the default 12 inputs at 50ms intervals. The complete run can take several
minutes because every case starts a fresh SSH connection.

Timing starts when the client submits a numbered input and ends when the client
receives and decrypts a complete Rustmux frame containing that number. Connection
setup and the initial screen are excluded. This measures input-to-frame reception,
**not physical screen presentation**. The ASCII fixture decoder retains unchanged
cells and waits for the renderer's final cursor/visibility sequence. Its framing
assumes the current renderer's output format and is not a general terminal parser.

Each fixture also updates a counter on the first row, allowing inputs and frames
to be paired. That extra row differs from the microbenchmark above: even a dense
scroll or fragmented row can save bytes through the counter row. Traffic counts
include encrypted SSH payload framing/padding during the measured interval, but
exclude TCP/IP headers and ACKs. They are not plaintext renderer byte counts.

Only individually observed counters contribute to latency percentiles; the JSON
also reports missing/coalesced updates. Such updates must be considered alongside
latency when comparing runs. With only 12 samples, the reported p95 is the maximum
sample and is descriptive rather than a stable tail-latency estimate. The proxy
models a controlled byte-rate queue and fixed delay, not WAN packet loss, jitter,
or a full congestion-control simulation. Local scheduling and SSH overhead remain
in the measurement. A remote application inside a **local** Rustmux would place
this renderer after the SSH link and would not yield these network byte savings.

### Recorded results

Measured on 2026-09-13 with release builds of `8d75e26` and `76c7b6e`.
All 32 cases observed all 12 inputs; none were coalesced.
The [complete JSON results](ssh-render-latency.json) include p95 and traffic counts.
Each table entry is input-to-frame p50 in milliseconds, row → cell.

| Added RTT / per-direction rate | Single | Status | Scattered | Scroll |
| --- | ---: | ---: | ---: | ---: |
| 0ms / 1,000,000 B/s | 1.2 → 1.1 | 1.3 → 0.9 | 1.1 → 1.0 | 10.5 → 10.2 |
| 150ms / 1,000,000 B/s | 152.3 → 152.0 | 152.4 → 152.1 | 152.6 → 152.4 | 162.0 → 161.6 |
| 100ms / 32,000 B/s | 113.7 → 105.8 | 113.8 → 106.1 | 113.7 → 109.8 | 524.2 → 465.8 |
| 100ms / 4,000 B/s | 388.8 → 136.9 | 388.4 → 140.9 | 388.3 → 193.5 | 5387.6 → 4898.2 |

With sufficient bandwidth, the 150ms propagation budget dominates both
renderers. At 4,000 B/s, sparse updates avoid much of the queue built by row
rendering. Dense scrolling still queues heavily. The counter row explains part
of the savings in the scattered and scrolling fixtures; these are not evidence
that cell spans compress fully changed rows. These single runs are illustrative,
not repeated statistical comparisons. They support keeping automatic byte-cost
selection as the default and deferring a user-facing granularity setting.
