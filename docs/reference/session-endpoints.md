# Session Endpoints

`session::SessionEndpoint` is the first, noninteractive part of H13. It reserves
a local Unix socket for one named session. The CLI does not start, attach to or
list persistent sessions yet; those behaviors require a server protocol and
terminal ownership transfer in later changes.

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
the endpoint value.

Unit tests cover name validation, directory and socket permissions, nonblocking
accept, duplicate live endpoints, stale-socket recovery, ordinary-file
preservation, cleanup and replacement identity. These tests establish local
endpoint ownership only; they do not claim detached process persistence or H13
acceptance.
