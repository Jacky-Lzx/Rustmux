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
