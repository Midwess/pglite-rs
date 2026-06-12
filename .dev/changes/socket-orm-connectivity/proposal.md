# Proposal: ORM connectivity over unix sockets

**Status**: approved

## Summary

Let any unmodified Rust ORM (SQLx, SeaORM, Diesel, diesel-async) connect to pglite-rs with zero networking: publish the existing postmaster socket in multi-process mode via `connection_uri()`, and add a feature-gated in-process socket gateway (`socket` feature) that speaks the server side of the Postgres v3 protocol over a RAM-backed unix socket. Every opened resource is reclaimed by Drop alone; `shutdown()`/`close()` stay optional.

## Motivation

ORMs only know how to open a connection themselves and speak Postgres wire bytes through it — they cannot consume a `PGlite` handle. The universal integration point is therefore a unix socket: a RAM kernel pipe with a filesystem nameplate that every Postgres driver supports via `host=<dir>`. Multi-process mode already owns such a socket (unpublished); in-process mode has none.

## Scope

### In Scope

- `multiple-process`: `PGlite::connection_uri()` + `PGlite::socket_path()` accessors (Some only for MP instances)
- `MultiProcessOptions.extra_connections` (default 4) — postmaster headroom for external ORM clients; `max_connections = pool_size + 2 + extra_connections`
- RAM-backed socket dir placement: `/dev/shm` when Linux + writable, else OS temp dir (shared helper, both modes)
- New `socket = []` feature, module `crates/pglite/src/socket/`: `PGlite::serve_unix_socket() -> SocketGateway` — UnixListener + doorman thread, synthesized startup handshake, frame pump at Q/F/Sync boundaries with COPY-IN continuation, per-batch tx_lock, responses forwarded verbatim, NotificationResponse side-channeled to Rust listeners
- Drop-clean guarantee: SocketGateway Drop stops the doorman (flag + self-connect + bounded 2s join) and removes the socket dir; also fixes the existing NotifyConn reader-thread leak (Shutdown::Both on Pool teardown)
- Tests both features; CI `socket` leg; README ORM section; CONTEXT.md/project.md records

### Out of Scope

- TCP listener (networking stays nonexistent)
- Multi-session in-process gateway (engine is single-session; ORM pool must be 1)
- tokio-based duplex adapter crate (deferred)
- Linux abstract-namespace sockets (`@name`) — driver support too spotty
- pg_dump-style tooling over the gateway

## Affected Areas

| Area | Impact |
|------|--------|
| `crates/pglite/src/multiple_process/mod.rs` | extra_connections option; RAM-backed sock dir; sock_dir pub(crate) |
| `crates/pglite/src/multiple_process/pool.rs` | notify shutdown on teardown (Drop-clean fix) |
| `crates/pglite/src/multiple_process/notify.rs` | `shutdown()` to unblock reader thread |
| `crates/pglite/src/db.rs` | connection_uri/socket_path accessors; dispatch_notifications helper |
| `crates/pglite/src/socket/mod.rs` (new) | SocketGateway: doorman, handshake, pump, Drop |
| `crates/pglite/src/lib.rs`, `Cargo.toml` | `socket` feature gate |
| `tests/multiprocess.rs`, `tests/socket_gateway.rs` (new) | coverage |
| `.github/workflows/ci.yml`, `README.md`, `CONTEXT.md`, `.dev/project.md` | CI leg + docs |

## Dependencies

- No new crates. std `UnixListener` + existing `futures`/`postgres-protocol`/`bytes`.
- Engine artifacts unchanged (no C-side work, no ENGINE_TAG bump).

## Risks

| Risk | Mitigation |
|------|------------|
| Mid-COPY-IN boundary misdetection deadlocks session | Track "last response ended with Z"; only CopyDone/CopyFail flush while mid-COPY; dedicated test |
| Driver rejects synthesized handshake (missing ParameterStatus) | Emit full standard set; server_version queried live from engine; real driver-shaped byte-sequence test |
| Doorman accept() never unblocks on Drop | Self-connect wake + bounded 2s join; Drop-clean test asserts dir gone + thread joined |
| Rust API call during ORM transaction joins it (single session) | Intentional per-batch lock; documented sharply in README/CONTEXT |
| /dev/shm present but unwritable (containers) | Probe writability, fall back to temp_dir |
