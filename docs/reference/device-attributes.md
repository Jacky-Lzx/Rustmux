# Device Attributes

Rustmux answers primary device attributes (DA1):

| Request | Reply |
| --- | --- |
| CSI c | CSI ? 1 ; 0 c |
| CSI 0 c | CSI ? 1 ; 0 c |
| ESC Z (legacy DECID) | CSI ? 1 ; 0 c |

[XTerm's table](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) identifies
this response as VT101 with no options, in the VT100 family. Rustmux uses it as
a conservative compatibility identity rather than advertising optional hardware
or a larger VT420/VT520 feature set. It is not a claim of complete VT101 emulation.
Applications may still need behavior outside the currently supported subset.

The reply is constant and independent of the outer terminal or TERM environment.
It does not enumerate Rustmux's RGB, mouse or other extensions. Supported mode
states can be queried through [DECRQM](mode-queries.md). No capabilities are
inferred from Kitty or another outer terminal and passed through to the child.

Only zero or omitted DA1 parameters are accepted. Nonzero values, extra
parameters, colon groups, intermediates, overflow, and DA2/DA3 prefixes are
ignored. An echoed DA1 response is not a request and produces no reply.
OSC/DCS payloads remain uninterpreted. C0 controls and cancellation keep the
existing parser rules.

Replies use the existing bounded input queue and leave screen state unchanged,
including cursor, style and pending wrap. They continue while synchronized
output pauses painting. There is no new queue or dependency.

## Verification

Run `cargo test --test device_attributes`. Tests cover all request forms, every
split boundary, bytewise parsing, unchanged state, invalid requests, response
echoes, cancellation and EOF. The nested PTY suite checks all forms and 10,000
DA1 requests whose replies exceed the 64 KiB queue. It also checks DA1 during a
synchronized batch. These tests verify the protocol path, not universal terminal
application compatibility.
