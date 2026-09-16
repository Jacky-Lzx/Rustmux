# Session Handshake

`session::handshake` applies the session protocol to a connected Unix stream.
Both sides use a two-second read and write timeout while negotiating, then
return the accepted stream to nonblocking mode for event-loop integration.

The client sends `Hello` with protocol version and nonzero terminal dimensions.
The server requires it to be the first message and responds with `Attached` only
for the exact supported version. Invalid first messages and incompatible
versions receive a bounded `Rejected` reason before the connection is closed.
A second `Hello` or `Attached` is invalid after negotiation.

One socket read may contain both the handshake and later frames. The peer types
retain those later messages instead of dropping them at the ownership boundary.
They also retain their incremental decoders so partial frames can continue in
the socket frontend or client bridge.

Tests use real `UnixStream` pairs to cover successful negotiation, initial
terminal size, messages coalesced with `Hello`, incompatible versions, invalid
first messages and repeated handshakes. Background process creation remains the
responsibility of the command-line orchestration layer.
