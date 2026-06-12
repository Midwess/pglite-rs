# Architecture Blueprint: multiple-process-mode

Generated: 2026-06-12 (code-architect agent; condensed)

## Design Summary

Opt-in `multiple-process` feature spawns the bundled `bin/postgres` as a child postmaster on a private 0700 unix-socket dir (`listen_addresses=''`), pools N thread-per-connection blocking `UnixStream` workers behind the unchanged PGlite API. One `Backend` enum on PGlite replaces the cmd_tx/_handle/_close trio; `roundtrip()` is the seam. In-process mode untouched; MP exempt from OPEN/BOOTED.

Settled sub-decisions: (1) live views unified to NON-TEMP + startup sweep (TEMP breaks under pool routing); (2) patch 0005 runtime-gates xlog/checkpointer skips on `is_pglite_active` (same binary correct in both modes; prevents WAL bloat under postmaster); (3) close = SIGINT+wait, Drop = SIGTERM to process group (+kill fallback, sockdir removal).

## Design Decisions

| Decision | Chosen | Rationale |
|---|---|---|
| Engine socket crash | 0004: NULL-guard pgl_recv/pgl_send → libc recv/send | spawned backends have NULL ptrs; pglitec.c compiled w/o renames so real syscalls callable |
| Seam | `enum Backend{InProcess{..}, #[cfg] MultiProcess(Arc<Pool>)}` | single choke point db.rs:526; static dispatch; one PGlite |
| Concurrency | thread-per-conn blocking UnixStream + std mpsc/oneshot | engine.rs precedent ×N; no tokio |
| tx_lock | SPLIT: MP plain queries lock-free; Transaction pins a conn | global lock would defeat the pool |
| live views | non-temp, unified, startup sweep | one code path; session-independence |
| LISTEN | dedicated notify conn; try_clone halves; pending-oneshot acks | NOTIFY only reaches the listening session |
| Socket dir | short temp_dir()/pgl-<pid>-<ctr>, assert len | sun_path 104/108 limit |
| Readiness | connect-retry + full handshake | pg_ctl not shipped |

## Components

1. **0004 patch**: pglitec.c pgl_recv/pgl_send — `if (pgl_read == NULL) return recv(fd,buf,n,flags);` under `#ifdef __PGLITE__`; add `-D__PGLITE__` (+`-include sys/socket.h` if needed) to standalone pglitec.o compile; rebuild.
2. **0005 patch**: xlog.c:2530 + checkpointer.c:950 — wrap skips in `if (is_pglite_active)` (extern volatile int, pglitec.c:36); spawned backends (active=0) checkpoint normally.
3. **Backend enum (db.rs)**: replaces trio; methods: `roundtrip`, `roundtrip_pinned`, `rollback_fire_and_forget`, `close`. Wire-builders untouched. tx_lock gated per backend.
4. **multiple_process/mod.rs**: `MultiProcessOptions{username,database,max_connections(4),relaxed_durability,start_params,locale_provider}`; `open_multi_process`; `Server{child,pgid,sock_dir,data_dir}` — extract_runtime+run_initdb reuse (pub(crate)), spawn `postgres -D <data> -k <sock> -c listen_addresses= -c max_connections=N+2 [...]` with `process_group(0)`; readiness loop; SIGINT close / SIGTERM-group Drop.
5. **multiple_process/pool.rs**: `Pool{conns: Vec<ConnHandle>, idle}`; ConnCmd::Roundtrip per worker; worker: handshake once, then write wire → read frames until ReadyForQuery(Z) → reply. `checkout() -> PinnedConn` (Drop=checkin); `PinnedConn::roundtrip`.
6. **multiple_process/notify.rs**: `NotifyConn{writer half, pending-oneshot queue}`; reader thread parses frames: NotificationResponse → listeners map (shared Arc); command acks → pending oneshot. listen/unlisten route here under MP.
7. **transaction.rs**: optional pin (Some under MP): BEGIN/stmts/COMMIT/ROLLBACK/Drop all on the pinned conn; in-process path unchanged (tx_lock guard).
8. **live/mod.rs**: `CREATE VIEW` (non-temp); `sweep_live_views()` at open (both modes) drops stray `live_query\_%\_view`; listen via Backend routing.
9. **error.rs**: `PostmasterStart(String)`; `PoolExhausted` (Phase 7).
10. **tests/multiprocess.rs** (gated): parity, parallel writes (threads + Arc<PGlite>), advisory-lock cross-session, pg_stat_activity N backends, live under pool, notify across conns, spawn smoke.

## Interface Specifications

```rust
pub struct MultiProcessOptions {
    pub username: String, pub database: String,
    pub max_connections: usize,        // pool N (default 4, min 2); server gets N+2
    pub relaxed_durability: bool,      // -c fsync=off
    pub start_params: Vec<String>, pub locale_provider: LocaleProvider,
}
#[cfg(feature = "multiple-process")]
impl PGlite { pub async fn open_multi_process(dir, MultiProcessOptions) -> Result<PGlite, Error>; }

enum Backend { InProcess{cmd_tx, _handle, _close}, #[cfg(feature)] MultiProcess(Arc<Pool>) }
struct PinnedConn { pool: Arc<Pool>, idx: usize }  // Drop → checkin
```

Framing: 5-byte header (tag+i32 len) → body; response complete at backend `Z`. COPY wire already terminates in Sync→Z.

## Phases (summary; full tasks in tasks.md)

1. Engine patches 0004+0005 + spawn smoke proof (socket SELECT 1 from spawned postmaster) — GATE
2. Feature scaffolding (both builds compile)
3. Backend enum refactor (zero in-process change; all tests green) + Server lifecycle + single-conn MP works
4. Pool + Transaction pinning (parallel proof)
5. NotifyConn + live unification/sweep
6. Tests, example, CI leg, ENGINE_TAG bump
7. Hardening (dead-conn recovery, checkout timeout)

## Risks

| Risk | L | I | Mitigation |
|---|---|---|---|
| Other overrides misbehave under postmaster (poll/setsockopt dummies) | Med | High | Phase-1 smoke exercises real socket end-to-end; extend 0004 |
| sun_path overflow | Med | High | short dir + length assert |
| framing bugs (COPY/multi-Z) | Med | High | header framing, Z-terminated; COPY test under MP |
| notify duplex race | Low | Med | clone halves; single writer; pending-oneshot acks |
| orphan postmaster on SIGKILL | Low | Med | process_group + datadir lock; doc caveat |
| 0005 in-process drift | Low | Low | active=1 preserves old behavior exactly |
| pool starvation | Low | Med | min N=2; Phase-7 timeout → PoolExhausted |

## Resolved defaults (from open questions)

max_connections default 4, server N+2; enforce N≥2; LISTEN acks via notify-reader pending-oneshot; socket path private this change; Phase-1 smoke committed as gated test.

Confidence: design 90 / risks 85 / feasibility 90.
