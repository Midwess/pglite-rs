# Tasks: socket-orm-connectivity

## Progress: [12/14]

## 1. Part A foundations

- [x] 1.1 Add extra_connections option (default 4) + raise postmaster max_connections to pool_size + 2 + extra_connections
- [x] 1.2 RAM-backed socket dir helper (/dev/shm probe, temp_dir fallback) used by Server::spawn; expose Server.sock_dir pub(crate)
- [x] 1.3 NotifyConn::shutdown() (Shutdown::Both) called from Pool teardown so the notify reader thread exits (Drop-clean fix)

## 2. Part A accessors + dispatch + MP tests

- [x] 2.1 PGlite::connection_uri() + PGlite::socket_path() — Some only for MultiProcess
- [x] 2.2 PGlite::dispatch_notifications() notify-only helper (never errors on ErrorResponse)
- [x] 2.3 MP tests: connection_uri smoke via raw UnixStream + extra_connections honored via SHOW max_connections

## 3. Part B socket gateway

- [x] 3.1 socket feature + lib.rs gate + SocketGateway skeleton + serve_unix_socket (RAM dir, 0700, .s.PGSQL.5432, sun_path guard, Error::Protocol on MP backend)
- [x] 3.2 Handshake helpers: read_startup (SSLRequest 'N', v3 StartupMessage) + synth_startup_reply (AuthenticationOk, ParameterStatus set with live server_version, BackendKeyData sentinel, ReadyForQuery)
- [x] 3.3 Doorman session loop + frame pump (Q/F/S boundaries + COPY-IN c/f continuation) + per-batch tx_lock + verbatim responses + dispatch_notifications + Terminate
- [x] 3.4 Drop + shutdown(self): stop flag, self-connect wake, bounded 2s join, remove sock dir

## 4. Tests + CI

- [x] 4.1 tests/socket_gateway.rs: handshake, SELECT 1, extended protocol (Parse/Bind/Describe/Execute/Sync), COPY IN, Terminate, second client, Rust-API interleave
- [x] 4.2 Drop-clean asserts: sock dir gone, doorman joined
- [ ] 4.3 CI socket feature leg

## 5. Docs + analysis

- [ ] 5.1 README ORM section: SQLx/SeaORM/Diesel URI snippets (MP) + gateway snippet (pool-size-1 warning, transaction-sharing note)
- [ ] 5.2 CONTEXT.md + project.md Latest Analysis updates

---

## Notes

- Deviation: COPY FROM STDIN through the gateway is impossible for the in-process engine — when the pump input drains mid-COPY the read callback signals EOF and the backend aborts with "protocol synchronization was lost" (pre-existing engine semantics; our copy_in API batches Q+d+c in one roundtrip to avoid it). Spec amended: COPY OUT supported, COPY IN documented unsupported in-process; ORMs needing COPY use multi-process mode's native socket.
- Gateway tests share one #[test] (one in-process instance per test binary/process).

- Resolved defaults: extra_connections 4; doorman join timeout 2s; BackendKeyData sentinel pid=1 key=0 (CancelRequest unsupported); server_version queried once at serve_unix_socket().
