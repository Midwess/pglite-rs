# Codebase Analysis: multiple-process-mode

Generated: 2026-06-12 (code-explorer agent; condensed, file:line evidence)

## 1. Transport seam (db.rs)

`roundtrip()` (db.rs:526) is the single choke point: `cmd_tx.send(EngineCommand::Exec{wire,reply})` + await. Transport-agnostic already: exec/query/query_with_types/describe_param_types/format_literals/copy_in/copy_out (all build wire bytes then roundtrip). Engine-specific: `close` (db.rs:320, sends EngineCommand::Close), `rollback_fire_and_forget` (db.rs:515, raw cmd_tx), `CloseOnDrop` (db.rs:51). `OPEN`/`BOOTED` statics gate `open_inner` (db.rs:138-144) — MP constructor bypasses both (exempt by design). `tx_lock` currently serializes ALL public methods — wrong for MP (would defeat the pool); split required: MP plain queries lock-free, Transaction pins. `process_response` (db.rs:534) is pure bytes→dispatch — reusable; in MP the notify-conn reader feeds notifications instead.

Seam recommendation: `enum Backend { InProcess{cmd_tx,..}, #[cfg(feature)] MultiProcess(Arc<Pool>) }` field on PGlite (Least New Definitions: no trait object).

## 2. Wire reuse over socket

`startup_packet()` (engine.rs:272-290): bare v3 StartupMessage, no SSLRequest — works as-is: backend accepts bare startup over unix socket (`backend_startup.c:566-583` handles SSLRequest only IF sent). `--auth=trust` sets BOTH local+host lines (initdb.c:3231 `authmethodlocal = authmethodhost`) → unix-socket clients get AuthenticationOk immediately. No OpenSSL needed for trust.

## 3. CRITICAL showstopper + server flags

**pgl_recv/pgl_send NULL crash**: pglitec.c:408/414 declare `pgl_read`/`pgl_write` fn pointers (BSS-zero); pglitec.c:453-465 call them UNCONDITIONALLY (no NULL guard, no is_pglite_active guard). All backend code compiled with `-Drecv=pgl_recv -Dsend=pgl_send` (build-libpglite.sh COPT). A spawned postmaster's forked backend doing socket IO → `pgl_read(NULL)` → SIGSEGV. initdb's `postgres --single` child survived only because single mode = InteractiveBackend (stdin/stdout), never SocketBackend. Fix: pglitec.c compiled WITHOUT the renames (only `-Dexit=`) → real `recv`/`send` callable → NULL-guard fallback. Patch 0004 + `-D__PGLITE__` added to the pglitec.o compile line.

GUCs confirmed in pin: `listen_addresses` (guc_tables.c:4427, '' = no TCP), `unix_socket_directories`/-k (postmaster.c:680), socket path = `{dir}/.s.PGSQL.5432` (pqcomm.h:47 UNIXSOCK_PATH), `max_connections` (guc_tables.c:2205). Server mode must NOT get single-mode params (--single/-O/-j/max_worker_processes=0 family — server wants bgworkers/autovacuum). `relaxed_durability` → `-c fsync=off`.

`is_pglite_active=0` in spawned processes keeps all runtime pglite branches dormant (pgl_longjmp falls through to real longjmp; Terminate → proc_exit(0)). Compile-time `__PGLITE__` patches audited: safe EXCEPT `xlog.c:2530` (skips RequestCheckpoint on WAL-segment fill → WAL bloat under postmaster) and `checkpointer.c:950` (forces inline checkpoint) → patch 0005 runtime-gates both on `is_pglite_active`. Shared memory fine: SHMEM_TYPE_MMAP default → real MAP_SHARED across fork; only the small header goes through pgl_shmget.

## 4. Lifecycle prior art

extract_runtime (engine.rs:293-315; stamp + staging rename) and run_initdb (engine.rs:318-340; --auth=trust, -U, locale args) reusable as-is — make pub(crate). Runtime tar ships `bin/postgres` + `bin/initdb`; **pg_ctl NOT shipped** → readiness must be connect-retry. MultiProcessOptions reuses username/database/relaxed_durability/start_params/locale_provider semantics.

## 5. Readiness + shutdown

Socket appears after postmaster.pid → retry `UnixStream::connect` + full handshake. Signals: SIGTERM smart / SIGINT fast / SIGQUIT immediate. Orphan prevention: `CommandExt::process_group(0)` at spawn + Drop kills `-pgid`; PR_SET_PDEATHSIG Linux-only; macOS has no equivalent → documented caveat (datadir lockfile prevents double-open).

## 6. Test layout

Mirror `tests/extensions.rs` gating (`#![cfg(feature = ...)]`). MP exempt from one-boot → multi-instance + std::thread concurrency tests in ONE process (no child-process pattern needed).

## 7. Session-state conflicts

- **live_query**: `CREATE TEMP VIEW` (live/mod.rs:42) is per-session → under pool routing the refresh SELECT lands on another backend → "relation does not exist". Fix chosen: non-temp view, unified for both modes, + startup sweep of stray `live_query_%_view`. WATCHED_TABLES_SQL unaffected (matches relkind='v').
- **listen**: LISTEN is session state → must live entirely on one dedicated notify connection with a background reader; full-duplex via UnixStream::try_clone (reader half blocking loop; writer half sends LISTEN/UNLISTEN; acks recognized by reader via pending-oneshot).
- **copy_in/out**: single-roundtrip wire (Query+CopyData+CopyDone ends in Z) → pool-safe; inside transactions uses the pinned conn.
- **dump_data_dir**: CHECKPOINT+tar is crash-consistent but not point-in-time under concurrent writers — document.

## Dependencies

std::process::Command + std::os::unix::net::UnixStream + existing mpsc/oneshot/postgres-protocol. Zero new external crates.

## Risks

See proposal.md table; dominant: 0004 completeness (other overrides), sun_path length, framing, orphan story.
