# Proposal: Multiple-Process Mode

**Status**: approved

## Summary

Add the feature-gated `multiple-process` mode: `PGlite::open_multi_process(dir, MultiProcessOptions)` spawns the bundled `bin/postgres` as a child postmaster on a private unix socket (`listen_addresses=''` — zero networking) and pools N connections behind the unchanged PGlite API, delivering true multi-connection Postgres (parallel sessions, real lock table, MVCC across backends) while in-process mode stays the untouched default.

## Motivation

The in-process engine is one backend: one session, serialized execution, one transaction at a time. Apps needing parallel sessions or concurrent transactions currently have no path that preserves the library experience (no install, no ports, one API). The postmaster we already ship in the runtime tar is the battle-tested orchestrator for exactly this — this change rents it as an invisible child.

## Scope

### In Scope

- Engine patch 0004 (showstopper): NULL-guard `pgl_recv`/`pgl_send` falling back to libc `recv`/`send` — spawned backends crash on first socket IO without it (pglitec.c:453/463 call NULL fn pointers; initdb's `--single` child only survived because single mode uses stdin)
- Engine patch 0005: runtime-gate (`is_pglite_active`) the xlog/checkpointer skip patches so spawned backends checkpoint normally (prevents WAL bloat); in-process behavior unchanged
- `multiple-process` cargo feature: single `#[cfg]` gate in lib.rs, module `src/multiple_process/` (mod.rs lifecycle, pool.rs, notify.rs); without the feature the API and code do not exist
- `Backend` enum seam in db.rs (`InProcess` / `MultiProcess`) — `roundtrip()` is the single choke point; in-process path zero behavior change
- Postmaster lifecycle: short 0700 socket dir (sun_path limit), connect-retry readiness (no pg_ctl shipped), SIGINT fast-shutdown on close, SIGTERM process-group on drop
- Pool: thread-per-connection blocking UnixStream + std mpsc / futures oneshot (engine.rs pattern ×N, no tokio); checkout/checkin; `Transaction` pins one connection
- Dedicated notify connection (try_clone'd read/write halves): holds all LISTENs, background reader dispatches NotificationResponse into the shared listeners map
- Live-query unification: non-temp views (TEMP is per-session — breaks under pool routing) + startup sweep of orphaned `live_query_%_view`; applied to both modes for one code path
- MP exempt from OPEN/BOOTED statics (multiple instances coexist)
- Feature-gated tests incl. parallel-writes, advisory-lock cross-session, pg_stat_activity backend count; example; CI leg; ENGINE_TAG bump (patches change engine bytes)

### Out of Scope

- pg_dump wrapper and public socket exposure for external clients (ride this plumbing in a later proposal)
- Windows; thread+dlopen in-process multi-connection research (v2)
- Pool auto-resize, connection recovery beyond Phase-7 hardening

## Affected Areas

| Area | Impact |
|------|--------|
| `native/patches/0004,0005` + `build-libpglite.sh` | new engine patches; `-D__PGLITE__` on pglitec.o compile |
| `crates/pglite/src/db.rs` | `Backend` enum refactor; tx_lock split; listen routing |
| `crates/pglite/src/multiple_process/` | new feature folder (mod/pool/notify) |
| `crates/pglite/src/transaction.rs` | connection pinning |
| `crates/pglite/src/live/mod.rs` | non-temp views + sweep |
| `crates/pglite/src/engine.rs` | pub(crate) reuse of startup_packet/run_initdb/extract_runtime |
| `crates/pglite/src/error.rs` | `PostmasterStart` variant |
| tests/examples/CI | gated multiprocess.rs, example, feature leg, tag bump |

## Dependencies

- v1/v1.1/v1.2 shipped (runtime tar already contains `bin/postgres`)
- Engine artifact rebuild + new `engine-*` tag (patches 0004/0005 change bytes)

## Risks

| Risk | Mitigation |
|------|------------|
| Other socket overrides also misbehave under postmaster (poll/setsockopt no-ops) | Phase 1 smoke proof exercises real socket IO before any Rust; extend 0004 if surfaced |
| sun_path overflow | short socket dir + length assert with early error |
| Orphaned postmaster on SIGKILL of host | process_group + datadir lock prevents double-open; documented caveat |
| Response framing bugs (COPY, multi-Z) | frame on 5-byte headers, terminate on ReadyForQuery; COPY wire already ends in Sync |
| Pool starvation (Transaction holds last conn) | enforce min pool size 2; Phase-7 checkout timeout |

## References

`analysis.md` (explorer, file:line), `blueprint.md` (architect), `design.md` (key decisions), CONTEXT.md (multi-process mode, connection pinning, notify connection).
