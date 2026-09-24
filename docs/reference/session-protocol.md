# Session Protocol

`session::protocol` defines the bounded local framing used by persistent session
clients and servers.

Each frame has a one-byte message type, a four-byte big-endian payload length
and the payload. A payload is limited to 64 KiB. Decoders accept fragmented and
coalesced reads while retaining no more than one maximum frame. An oversized,
unknown, malformed or truncated frame returns an error instead of allocating
from an untrusted length.

The client-to-server messages are:

- `Hello`: protocol version, nonzero terminal rows and columns, and reported
  pixel width and height (zero when unknown).
- `Input`: arbitrary terminal input bytes.
- `Resize`: new nonzero rows and columns plus the latest reported pixel width
  and height (zero when unknown).
- `Detach`: an explicit request to disconnect without stopping the session.

The server-to-client messages are:

- `Attached`: the accepted protocol version, effective LOCKED-to-NORMAL prefix byte, and whether legacy client-side session shortcuts remain enabled.
- `Output`: arbitrary bytes for the outer terminal.
- `Exit`: the server's signed process status.
- `OpenSessionManager`: a zero-payload terminal control request.
- `Detach`: a zero-payload request for the client to restore its terminal and leave the session running.
- `Rejected`: a UTF-8 reason limited to 1 KiB.

Protocol version 6 adds two 16-bit pixel dimensions to `Hello` and `Resize`.
The local client forwards `TIOCGWINSZ` values without guessing from row and
column counts; the server preserves them in its frontend resize event. It does
not yet distribute cell pixel sizes to panes or enable image rendering. Old
clients and servers need to restart to use this incompatible frame layout.
Version 5 added the legacy-shortcut flag to `Attached`. A server
with `clear_defaults = true` clears it so the attached client forwards
otherwise unbound prefix-`d` and prefix-Ctrl-W for the server to decide.
Version 4 added the prefix byte; version 3 added
server-requested `Detach` after version 2's `OpenSessionManager`. The version is
carried explicitly so the handshake can reject an
incompatible peer before forwarding terminal bytes. Encoding validates sizes
and limits just like decoding. Unit tests exercise every-byte fragmentation,
multiple frames in one read, binary payloads, maximum-size frames, malformed
fixed fields, invalid UTF-8, truncation and decoder reuse.
