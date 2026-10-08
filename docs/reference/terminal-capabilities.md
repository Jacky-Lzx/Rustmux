# Terminal Capability Queries

Rustmux answers a small subset of XTerm's
[XTGETTCAP](https://invisible-island.net/xterm/ctlseqs/ctlseqs.pdf) capability
query. Send `DCS + q hex-names ST`, where DCS is ESC P, ST is ESC backslash,
and names are hexadecimal byte pairs separated by semicolons.

| Name | Encoded name | Value | Meaning |
| --- | --- | --- | --- |
| `Co` | `436f` | `256` | Indexed colors |
| `colors` | `636f6c6f7273` | `256` | Terminfo alias of `Co` |
| `RGB` | `524742` | `8` | Eight bits per direct RGB component |

For example:

```text
Request: ESC P + q 436f;524742 ESC \
Reply:   ESC P 1 + r 436f=323536;524742=38 ESC \
```

Names accept uppercase or lowercase hex digits, and replies preserve the
request's encoded spelling. Decoded capability names are case sensitive. Values
are hexadecimal ASCII (`323536` encodes `256`, and `38` encodes `8`). Multiple
supported names, including duplicates, receive one response in request order.

Processing ends at the first unknown or malformed name. A preceding supported
prefix receives a successful response containing only that prefix. If the first
name is unknown, malformed or empty, the response is `DCS 0 + r ST`.

## Capability boundary

The allowlist describes Rustmux's existing indexed and direct-color parser and
renderer. It does not load the host terminfo database, inspect `TERM`, query the
outer terminal, or forward requests. `TN` and `name` are unsupported because the
repository does not provide a Rustmux terminfo entry. Use the existing
[terminal-version query](device-attributes.md) for the Rustmux product name.
Keyboard strings, `Tc`, arbitrary terminfo strings, XTSETTCAP and XTGETXRES are
also outside this increment.

Queries leave cells, styles, cursor, saved cursor, wrap and modes unchanged.
They work in primary and alternate buffers, background panes and windows,
synchronized-output batches, and detached sessions. The reply belongs to the
querying pane, regardless of the currently selected pane. Display-only parsing
and saved-output replay discard replies and retain no deferred response.

DCS accepts ST only. BEL does not terminate it. CAN/SUB cancel incomplete strings;
EOF discards them. Response echoes are not requests. Nonterminating ESC and nested
controls retain the display parser's existing discard behavior.

## Bounds and transport

The existing DCS limit is 64 retained payload bytes, including `+q`. An oversized
string is consumed without a response until termination or cancellation; its
payload cannot grow parser storage. At most twelve shortest supported names fit.
The largest response still fits the existing `MAX_REPLY_BYTES` reservation,
including when a single byte completes a query begun in a previous PTY read.
No reply limit, queue, dependency or session wire format changes.

Replies use the querying pane's existing bounded input queue alongside ordinary
input. Child output and reply delivery continue under backpressure. Nothing is
emitted to the outer clipboard or outer terminal as an XTGETTCAP response.

## Review and verification

> Historical record: the checks, branch names and review status in this section
> describe the original implementation revision. They are not the current
> branch or deployment status. See [Branches and Compatibility](documentation-status.md#historical-verification-records).

Base: `main-human` commit `1a3d8a9`. Fixed `main` comparison:
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c`. No explicit XTGETTCAP handler exists
in that reference. This increment fills a terminal capability-query gap.

Reading order: `src/capability.rs`, the DCS dispatch in `src/parser.rs`,
`tests/capability_queries.rs`, then `tests/terminal_loop_capabilities.py`.

The protocol tests cover exact replies and ordering, both hex cases, aliases,
unknown/malformed names, every split and bytewise parsing, unchanged screen state,
maximum-length queries, single-byte reply capacity, cancellation, incomplete
strings, response echoes, display-only replay and megabyte overflow recovery.
The real PTY scenario checks foreground unnamed, detached and attached background
panes. It splits ST across writes, verifies no premature reply, answers queries
while painting is synchronized, and checks 4,000 grouped replies exceeding the
64 KiB input queue without requiring an outer-terminal response. Named child PIDs
remain unchanged.

Local Rust 1.99.0 validation passed 1,034 tests across 52 targets, including
all 47 real PTY scenarios, with 6 pre-existing ignored tests. All-target/all-feature
Clippy with warnings denied, formatting, Python syntax, `git diff --check` and
mdBook build passed. The existing cursor-shape test's old unsupported `+q`
expectation was updated to the new invalid-capability response; its other DCS
rejection checks remain covered.

The shared acceptance ledger is unchanged; this feature awaited owner review.
The review branch had not been pushed. GitHub CI and live Vim/Neovim behavior
for this new feature remain unverified.
