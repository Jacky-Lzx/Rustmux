# Kitty Structured Color Control

Rustmux handles a bounded subset of [Kitty OSC 21](https://sw.kovidgoyal.net/kitty/color-stack/#setting-and-querying-colors)
inside each pane. It operates on the same profile as OSC 4/10/11/12 and the
[Kitty color stack](color-stack.md).

```sh
printf '\033]21;foreground=#123456;1=#abcdef\033\\'
printf '\033]21;foreground=?;1=?\033\\'
printf '\033]21;foreground;1\033\\'
```

## Operations

Supported keys are `foreground`, `background`, `cursor` and decimal palette
indices `0` through `255`. A color value sets an override, `?` queries its current
RGB value, and a bare key resets its override to current outer-terminal inheritance.
Both BEL and ST terminate a command, including fragmented input.

Setters accept the existing `#RRGGBB` and `rgb:R/G/B` formats, with one to four
hex digits per RGB component. Queries return eight-bit `rgb:rr/gg/bb`, grouped in
one OSC 21 reply in operation order with the request's terminator. A query after
a setter observes the changed value. Resets track the currently attached client's
profile, including subsequent reattachments.

Unrecognized keys generate `unknown=` fields with their names encoded as
unpadded Base64, including setters and bare keys. This also applies to selection
colors and `cursor_text`, which have no corresponding pane-model state.

Empty values requesting dynamic colors and named colors are unsupported. A
malformed supported field, empty key, non-ASCII/control byte or empty field makes
the entire command a no-op, suppressing setters and all responses. The command
is validated before executing operations. The existing 64-byte OSC payload cap
includes `21;`; oversized, cancelled and incomplete strings never apply partial
updates. Unknown field values are ignored while their key names are acknowledged.

## Pane behavior

Structured and legacy color commands share overrides and inheritance bindings.
Main/alternate buffers, color-stack push/pop, resize, reset and saved-history
behavior follow the existing profile lifecycle. Colors remain isolated between
panes; restoring a profile recolors existing symbolic cell styles. The active
pane's cursor color uses the existing outer synchronization path. Rustmux's
interface theme is separate.

Requests and responses are consumed locally. Queries return to the originating
child, including background panes and detached sessions, and never reach the
outer terminal. Display-only parser callers discard responses. No additional
response queue is introduced. The existing 420-byte worst-case per-input-byte
reservation covers the aggregate response, even when one terminator completes
a command buffered in earlier reads.

## Review and verification

Base: reviewed color-stack commit `70f8d1e`. Fixed `main` reference:
`57d598657ad7acf00d6a0ddf734fba8f48d50e4c`,
`src/terminal.rs::TerminalOscTracker::apply_kitty_colors`.

This increment implements its foreground/background/cursor/palette subset in the
human track. It additionally supports bare-key resets, unknown-key responses,
request terminator preservation and atomic validation. The reference silently
skips unsupported fields, malformed entries and resets. The human track retains
its inherited outer profile and 64-byte command limit rather than the reference's
64 KiB observer buffer. Empty dynamic colors, selection/cursor-text state, named
colors and additional hexadecimal color formats remain outside this increment.

Reading order: `src/structured_colors.rs`, OSC dispatch in `src/parser.rs`,
`tests/structured_colors.rs`, then `tests/terminal_loop_structured_colors.py`.

Model tests cover both terminators at every split, ordered setters/queries,
all 256 palette entries, legacy interoperability, unknown fields, unchanged
screen state, reset/stack/resize/buffer lifecycle, atomic malformed commands,
cancellation, incomplete and oversized strings, and maximum response expansion.
The real PTY scenario checks two independent child processes, detached queries,
background-pane routing, legacy-stack interoperability, alternate buffers,
resets across two outer profiles, resize, unchanged PIDs and a reply burst
larger than the child-input queue. It checks that OSC 21 never leaks outside.

Local validation on Rust 1.99.0 passed 1,014 tests across 51 targets, including
all 45 real PTY scenarios, with 6 pre-existing ignored tests. The final focused
PTY run also passed, including an explicitly checked 84,000-byte response burst.
All-target/all-feature Clippy with warnings denied, formatting, Python syntax,
`git diff --check` and the mdBook build passed. The shared acceptance ledger is
unchanged. This branch awaits owner review and has not
been pushed; GitHub CI and GUI color appearance remain unverified.
