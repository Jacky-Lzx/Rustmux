# Session Protocol

`session::protocol` defines the bounded local framing used by persistent session
clients and servers.

Each frame has a one-byte message type, a four-byte big-endian payload length
and the payload. A payload is limited to 64 KiB. Decoders accept fragmented and
coalesced reads while retaining no more than one maximum frame. An oversized,
unknown, malformed or truncated frame returns an error instead of allocating
from an untrusted length.

The client-to-server messages are:

- `Hello`: protocol version plus nonzero terminal rows and columns.
- `Input`: arbitrary terminal input bytes.
- `Resize`: new nonzero rows and columns.
- `Detach`: an explicit request to disconnect without stopping the session.

The server-to-client messages are:

- `Attached`: the accepted protocol version.
- `Output`: arbitrary bytes for the outer terminal.
- `Exit`: the server's signed process status.
- `OpenSessionManager`: a zero-payload terminal control request.
- `Detach`: a zero-payload request for the client to restore its terminal and leave the session running.
- `Rejected`: a UTF-8 reason limited to 1 KiB.

Protocol version 3 adds server-requested `Detach` after version 2's
`OpenSessionManager`. The version is carried explicitly
so the handshake can reject an
incompatible peer before forwarding terminal bytes. Encoding validates sizes
and limits just like decoding. Unit tests exercise every-byte fragmentation,
multiple frames in one read, binary payloads, maximum-size frames, malformed
fixed fields, invalid UTF-8, truncation and decoder reuse.
