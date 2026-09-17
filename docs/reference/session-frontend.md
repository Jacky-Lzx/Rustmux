# Session Frontend Adapter

`session::frontend::ServerFrontend` converts an accepted session connection
into bounded terminal input, resize updates and framed renderer output. The
background session server uses this adapter for every attached client.

The adapter now satisfies the terminal loop's private `Frontend` boundary. The
loop drains already decoded input before polling for more, requests socket reads
only while the adapter has capacity and stops socket output after detach or
disconnect. This keeps the local terminal and session socket on one event-loop
implementation.

The initial size from `Hello` is exposed as the first resize. Later `Resize`
messages are coalesced so the event loop applies only the newest dimensions.
`Input` payloads enter a bounded internal queue and can be drained without
exceeding the event loop's own input limit. `Detach` and a clean socket EOF are
reported as different states because detaching must leave the session alive.

Rendered bytes are split into protocol payloads of at most 64 KiB. A source
byte remains in the renderer queue until its complete `Output` frame has been
written, including the header. This preserves backpressure across partial
nonblocking writes and prevents either duplicated or missing terminal output.
After the final rendered frame drains, the server temporarily applies a bounded
blocking write to send the small `Exit` frame and then restores nonblocking
mode. It refuses to insert `Exit` into a partially written output frame.

Tests use real nonblocking Unix stream pairs for input, resize, detach, EOF and
multi-frame output. A deliberately short writer verifies the partial-write
ownership rule independently of socket buffer timing.
