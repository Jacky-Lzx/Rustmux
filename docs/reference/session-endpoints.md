# Session Endpoints

`session::SessionEndpoint` reserves a local Unix socket for one named session.
The `new` and `attach` commands use it as the ownership boundary for the
persistent server.

Session names contain 1–64 ASCII letters, numbers, `-` or `_`. Restricting the
name keeps it as one path component and bounds Unix socket paths. Endpoints live
under `/tmp/rustmux-<effective uid>`, whose ownership is checked and permissions
are reset to `0700`. Socket permissions are `0600`.

Binding rejects an endpoint that accepts connections and never replaces an
ordinary file or symlink. A socket that refuses connections is treated as a
stale endpoint left by an unclean exit and removed before binding. The listener
is nonblocking for later event-loop polling.

`SessionEndpoint` records the socket device and inode. Dropping it removes the
path only while that identity still matches, so an older owner cannot unlink a
replacement endpoint. The listener and socket path otherwise remain owned by
the endpoint value. After `fork`, the client-side copy relinquishes unlink
ownership while the server keeps its copy. Attaching rechecks that the path is a
socket owned by the effective user and that group and other permission bits are
clear.

Unit tests cover name validation, directory and socket permissions, secure
connection, nonblocking accept, duplicate live endpoints, stale-socket recovery,
ordinary-file preservation, cleanup, fork relinquishment and replacement
identity. Detached process persistence is covered by the nested-PTY CLI test.
