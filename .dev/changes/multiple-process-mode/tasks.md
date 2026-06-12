# Tasks: multiple-process-mode

## Progress: [12/15]

**Process rule**: every task ends with `git commit` + `git push origin main`. Short messages, no co-author.

## 1. Engine patches + spawn smoke proof (GATE — before any Rust refactor)

- [x] 1.1 Patch 0004: NULL-guard `pgl_recv`/`pgl_send` → libc `recv`/`send` (`#ifdef __PGLITE__`); add `-D__PGLITE__` to the standalone pglitec.o compile in build-libpglite.sh; rebuild archive + runtime tar
- [x] 1.2 Patch 0005: wrap xlog.c:2530 RequestCheckpoint skip + checkpointer.c:950 inline-checkpoint force in `if (is_pglite_active)`; rebuild; full existing test sweep stays green
- [x] 1.3 Spawn smoke proof: initdb temp dir, spawn `bin/postgres -D dir -k sock -c listen_addresses= -c max_connections=6`, raw UnixStream handshake + SELECT 1 → T/D/C/Z, clean SIGINT shutdown. Commit as the seed of gated tests/multiprocess.rs. GATE for all Rust work

## 2. Feature scaffolding

- [x] 2.1 `multiple-process` feature in Cargo.toml; `#[cfg]` mod gate + re-exports in lib.rs; stub `MultiProcessOptions` + `open_multi_process`; both feature states compile

## 3. Backend seam + postmaster lifecycle

- [x] 3.1 `Backend` enum in db.rs replacing cmd_tx/_handle/_close trio; route roundtrip/close/rollback_fire_and_forget; pub(crate) engine reuse fns (startup_packet/run_initdb/extract_runtime/runtime_dir); ZERO in-process behavior change — full default suite green
- [x] 3.2 `Server` lifecycle in multiple_process/mod.rs: short 0700 sock dir (+len assert), spawn with process_group(0) + server flags (NO single-mode params; fsync=off from relaxed_durability; start_params passthrough), connect-retry readiness, SIGINT close / SIGTERM-group Drop / sockdir cleanup; `PostmasterStart` error; single-conn placeholder pool; MP exempt from OPEN/BOOTED (two instances coexist); SELECT 1 works

## 4. Pool + transaction pinning

- [x] 4.1 pool.rs: N worker threads (blocking UnixStream, handshake once, frame-until-Z), checkout/checkin, `PinnedConn` (Drop=checkin); Backend::MultiProcess routes roundtrip via pool; MP plain queries drop the global tx_lock
- [ ] 4.2 transaction.rs pinning: BEGIN/stmts/COMMIT/ROLLBACK/Drop on one pinned conn under MP; in-process unchanged; parallel-writes proof (2 threads, Arc<PGlite>, interleaved pinned transactions)

## 5. Notify connection + live unification

- [x] 5.1 notify.rs: dedicated conn, try_clone halves, blocking reader dispatching NotificationResponse into shared listeners map, LISTEN/UNLISTEN via writer half with pending-oneshot acks; MP listen/unlisten route here
- [x] 5.2 live/mod.rs: CREATE VIEW (drop TEMP) unified both modes; `sweep_live_views()` at every open dropping stray `live_query\_%\_view`; live test green in BOTH modes

## 6. Tests, example, CI, docs

- [x] 6.1 tests/multiprocess.rs (gated): parity CRUD, parallel writes, advisory-lock cross-session, pg_stat_activity shows N backends, COPY under MP, live + notify across connections, teardown leaves no orphan
- [x] 6.2 examples: multi_process bin (feature-gated in examples crate) demonstrating parallel queries
- [ ] 6.3 CI: `--features multiple-process` test leg; ENGINE_TAG bump in both build.rs (patches changed engine bytes) + retag per runbook; README features table row
- [ ] 6.4 project.md Latest Analysis + runbook patch list (0004/0005); CONTEXT.md final pass

## 7. Hardening

- [ ] 7.1 Dead-conn handling (worker IO error → conn marked broken, error surfaced, pool survives); checkout timeout → `PoolExhausted`; module docs incl. orphan caveat

---

## Notes

- 3.2 findings: runtime-extraction stamp was tar byte-length — tar block padding absorbed the patched binary (same length, stale unpatched runtime → backend segfaults). Stamp replaced with FNV-1a content fingerprint baked into the runtime dir NAME. Postmaster stderr now logs to <sockdir>.log permanently.
- 1.3 findings: 0004 grew beyond recv/send — fcntl/poll/setsockopt/getsockopt/getsockname/connect dummies now gate on is_pglite_active (real syscalls when 0). The fcntl no-op left the postmaster death pipe blocking → children deaf → fast shutdown stalled (and my probe orphaned a checkpointer). poll fallback routed via pgl_native_poll in pglite_native.c (pglitec.c defines its own struct pollfd; cannot include poll.h). Build script now force-relinks bin/postgres (pglitec.o is LDFLAGS_EX, not a make prerequisite — stale-link hazard).

- Resolved defaults: pool default 4 (min 2 enforced); server max_connections = N+2; LISTEN acks via notify-reader pending-oneshot; socket path private this change; smoke committed as gated test.
- dump_data_dir under MP is crash-consistent, not point-in-time — document in 6.4.
