# Architecture Blueprint: socket-orm-connectivity

Generated: 2026-06-13 (code-architect)

## Design Summary

(A) accessors + extra_connections + RAM-backed socket dir on multiple-process so ORMs connect to the native postmaster by URI; (B) new `socket` feature: one SocketGateway struct running a std doorman thread that synthesizes the Postgres startup handshake and pumps frames to backend.roundtrip via block_on, Drop-clean. No tokio, no new deps, one new struct.

## Design Decisions

| Decision | Chosen | Rationale |
|---|---|---|
| Gateway API | single serve_unix_socket(), auto RAM dir | Simplicity First; address via socket_path()/uri() |
| Lifecycle owner | new SocketGateway struct | Least New Definitions scan: Server is MP-only, NotifyConn pool-internal — no existing host |
| Engine calls from std thread | futures::executor::block_on | tokio-free mandate; precedent live/mod.rs:112 |
| Lock granularity | per-batch tx_lock | lets Rust API interleave; single-session semantics documented |
| Notify dispatch | new notify-only dispatch_notifications | process_response errors on ErrorResponse (normal ORM traffic) |
| serve_unix_socket on MP | Error::Protocol -> connection_uri() | MP has native socket; proxy is dead weight |
| Handshake bytes | hand-assembled | postgres-protocol has no backend-direction encoders |
| server_version | query SHOW server_version once at start | correct against pinned engine |
| NotifyConn leak | fix now (Shutdown::Both at Pool teardown) | repo-wide Drop-clean requirement |
| Socket dir | /dev/shm if linux+writable else temp_dir | RAM nameplate; shorter sun_path |

## Component Design

SocketGateway (socket/mod.rs): fields listener UnixListener, stop Arc<AtomicBool>, sock_dir PathBuf, doorman Option<JoinHandle>, _db PGlite clone. Methods: socket_path(), uri(), shutdown(self). Internal helpers: ram_backed_dir(), synth_startup_reply(server_version), read_startup(stream), next_batch_boundary(buf, in_copy_in).

PGlite additions: connection_uri()/socket_path() (MP-only Some), pub(crate) dispatch_notifications(&[u8]).

MultiProcessOptions: + extra_connections: usize (default 4); postmaster max_connections = pool_size + 2 + extra_connections.

## Files

CREATE: crates/pglite/src/socket/mod.rs (high), crates/pglite/tests/socket_gateway.rs (med)
MODIFY: multiple_process/mod.rs, pool.rs, notify.rs, db.rs, lib.rs, Cargo.toml, ci.yml, tests/multiprocess.rs, README.md, CONTEXT.md, .dev/project.md

## Phases

1. Part A foundations (extra_connections, RAM dir helper, NotifyConn shutdown fix)
2. Part A accessors + dispatch helper + MP tests
3. Part B gateway (skeleton, handshake, pump, Drop)
4. Tests + CI
5. Docs

## Risks and Mitigations

| Risk | L | I | Mitigation |
|---|---|---|---|
| COPY-IN boundary misdetection | Med | High | Z-tracking; only c/f flush mid-COPY; dedicated test |
| Driver rejects handshake | Med | Med | full ParameterStatus set; real driver-shaped test |
| accept() blocks Drop | Low | High | self-connect wake + 2s bounded join + asserts |
| API call joins ORM transaction | Med | Med | intentional; documented sharply |
| /dev/shm unwritable | Low | Med | probe + fallback |
| sun_path overflow | Low | High | existing ≤96 guard |
| Shutdown::Both racing command() | Low | Low | only at instance teardown; pending oneshots resolve Closed |

## Resolved Open Questions

- server_version: queried once at serve_unix_socket()
- doorman join bound: 2s
- BackendKeyData: sentinel pid=1 key=0 (CancelRequest unsupported in-process)
