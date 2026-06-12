# Tasks: socket-orm-connectivity

## Progress: [0/14]

## 1. Part A foundations

- [ ] 1.1 Add extra_connections option (default 4) + raise postmaster max_connections to pool_size + 2 + extra_connections
- [ ] 1.2 RAM-backed socket dir helper (/dev/shm probe, temp_dir fallback) used by Server::spawn; expose Server.sock_dir pub(crate)
- [ ] 1.3 NotifyConn::shutdown() (Shutdown::Both) called from Pool teardown so the notify reader thread exits (Drop-clean fix)

## 2. Part A accessors + dispatch + MP tests

- [ ] 2.1 PGlite::connection_uri() + PGlite::socket_path() — Some only for MultiProcess
- [ ] 2.2 PGlite::dispatch_notifications() notify-only helper (never errors on ErrorResponse)
- [ ] 2.3 MP tests: connection_uri smoke via raw UnixStream + extra_connections honored via SHOW max_connections

## 3. Part B socket gateway

- [ ] 3.1 socket feature + lib.rs gate + SocketGateway skeleton + serve_unix_socket (RAM dir, 0700, .s.PGSQL.5432, sun_path guard, Error::Protocol on MP backend)
- [ ] 3.2 Handshake helpers: read_startup (SSLRequest 'N', v3 StartupMessage) + synth_startup_reply (AuthenticationOk, ParameterStatus set with live server_version, BackendKeyData sentinel, ReadyForQuery)
- [ ] 3.3 Doorman session loop + frame pump (Q/F/S boundaries + COPY-IN c/f continuation) + per-batch tx_lock + verbatim responses + dispatch_notifications + Terminate
- [ ] 3.4 Drop + shutdown(self): stop flag, self-connect wake, bounded 2s join, remove sock dir

## 4. Tests + CI

- [ ] 4.1 tests/socket_gateway.rs: handshake, SELECT 1, extended protocol (Parse/Bind/Describe/Execute/Sync), COPY IN, Terminate, second client, Rust-API interleave
- [ ] 4.2 Drop-clean asserts: sock dir gone, doorman joined
- [ ] 4.3 CI socket feature leg

## 5. Docs + analysis

- [ ] 5.1 README ORM section: SQLx/SeaORM/Diesel URI snippets (MP) + gateway snippet (pool-size-1 warning, transaction-sharing note)
- [ ] 5.2 CONTEXT.md + project.md Latest Analysis updates

---

## Notes

- Resolved defaults: extra_connections 4; doorman join timeout 2s; BackendKeyData sentinel pid=1 key=0 (CancelRequest unsupported); server_version queried once at serve_unix_socket().
