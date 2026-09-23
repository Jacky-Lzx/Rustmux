# Device Attributes

Rustmux answers primary, secondary and tertiary device attributes (DA1, DA2 and
DA3):

| Request | Reply |
| --- | --- |
| CSI c | CSI ? 1 ; 0 c |
| CSI 0 c | CSI ? 1 ; 0 c |
| ESC Z (legacy DECID) | CSI ? 1 ; 0 c |
| CSI > c | CSI > 0 ; 0 ; 0 c |
| CSI > 0 c | CSI > 0 ; 0 ; 0 c |
| CSI = c | DCS ! \| 00000000 ST |
| CSI = 0 c | DCS ! \| 00000000 ST |

[XTerm's table](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) identifies
this response as VT101 with no options, in the VT100 family. Rustmux uses it as
a conservative compatibility identity rather than advertising optional hardware
or a larger VT420/VT520 feature set. It is not a claim of complete VT101 emulation.
Applications may still need behavior outside the currently supported subset.
Consistently with omitting the printer option, the private printer-status query
reports not ready; Rustmux does not implement media-copy commands.
Likewise, omitting the user-defined-key option is paired with a locked UDK status;
Rustmux does not accept key-definition strings.
The DA2 response likewise identifies the VT100 terminal type, firmware version
zero and no ROM cartridge; it does not identify Rustmux as xterm or another
outer terminal.

The DA3 response is the VT400 Terminal Unit ID report. Like XTerm, Rustmux uses
zeros for both the site code and serial number. This stable value avoids exposing
host identifiers and does not claim a real hardware serial number.

Rustmux also answers XTerm's terminal-version query:

| Request | Reply |
| --- | --- |
| CSI > q | DCS > \| rustmux(0.1.0) ST |
| CSI > 0 q | DCS > \| rustmux(0.1.0) ST |

The response version comes from the package version at compile time. It identifies
Rustmux rather than imitating the outer terminal or XTerm. Only an omitted or zero
parameter is accepted.

The reply is constant and independent of the outer terminal or TERM environment.
It does not enumerate Rustmux's RGB, mouse or other extensions. Supported mode
states can be queried through [DECRQM](mode-queries.md). No capabilities are
inferred from Kitty or another outer terminal and passed through to the child.

Only zero or omitted DA1/DA2/DA3 parameters are accepted. Nonzero values, extra
parameters, colon groups, intermediates and overflow are ignored. An echoed DA1,
DA2 or DA3 response is not a request and produces no reply.
OSC/DCS payloads remain uninterpreted. C0 controls and cancellation keep the
existing parser rules.

Replies use the existing bounded input queue and leave screen state unchanged,
including cursor, style and pending wrap. They continue while synchronized
output pauses painting. There is no new queue or dependency.

## Verification

Run `cargo test --test device_attributes`. Tests cover all request forms, every
split boundary, bytewise parsing, unchanged state, invalid requests, response
echoes, cancellation and EOF. The nested PTY suite checks all forms and 10,000
grouped identity requests whose replies exceed the 64 KiB queue. It also checks
both queries during a synchronized batch. These tests verify the protocol path,
not universal terminal application compatibility.
