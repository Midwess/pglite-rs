# Proposal: Replica Mode — PGlite as a Read-Only Logical Replica of an Upstream Postgres

**Status**: approved

## Summary

Add an opt-in `replica` cargo feature: a new `replica` module that keeps the local PGlite continuously, provably in sync with an external real PostgreSQL ("upstream") via a hand-rolled, runtime-agnostic logical-replication client — consistent snapshot backfill, transactional streaming apply with an atomic LSN watermark, standby feedback, restart-as-resume, publication-drift halt, and an in-process committed-transaction broadcast seam.

## Motivation

Apps want fast local reads and change feeds from an embedded Postgres while the source of truth stays a real external Postgres. This is the architecture of Zero's zero-cache (Postgres → replica → subscribers), but their replica is SQLite, forcing a permanent dialect-translation layer. PGlite *is* Postgres: decoded WAL applies natively (text-format tuples round-trip exactly), schema bootstraps in the same dialect, and query semantics match upstream by construction. The capture pipeline (slot + exported snapshot + pgoutput streaming) is the one part of such systems that is settled engineering — this change implements exactly that pipeline and nothing above it.

The applier is the **only** writer to PGlite in replica mode; application writes go to upstream and loop back through the WAL. The cache's single invariant: *PGlite state is exactly upstream's state as of the watermark LSN.*

## Scope

### In Scope

- New module `crates/pglite/src/replica/` behind cargo feature `replica = []` (Rust-only feature, no new dependencies, no native-build impact)
- Hand-rolled replication client: dedicated `pglite-replica` OS thread + `std::net::TcpStream` + existing `postgres-protocol` crate (SCRAM-SHA-256 auth); NO tokio — mirrors the engine-thread precedent
- Slot lifecycle owned by the module: `CREATE_REPLICATION_SLOT ... LOGICAL pgoutput EXPORT_SNAPSHOT`
- Schema bootstrap by introspection (no pg_dump binary): published tables recreated in PGlite with PKs kept, FKs stripped
- Consistent backfill: COPY each published table under the exported snapshot, streamed in bounded chunks into `PGlite::copy_in`
- Streaming apply: pgoutput v1 decode (manual CopyBoth framing — `postgres-protocol` lacks CopyBothResponse/XLogData), per-transaction buffering, one upstream transaction = one PGlite transaction, watermark (`_pglite_replica` meta table) updated inside that same transaction, skip-by-watermark idempotency on resume, TOAST-unchanged column handling
- Standby-status feedback (keepalive replies + periodic), so upstream can free WAL
- Restart-as-resume from the persisted watermark; publication/fingerprint drift check at boot and per Relation message — halt loudly (v1 policy)
- `Replica::subscribe()` — in-process fan-out of committed transactions (`CommittedTransaction`), the seam for a future serving layer
- Feature-gated tests: offline pgoutput decode fixtures + env-gated integration tests (`PGLITE_REPLICA_UPSTREAM_DSN`); CI Linux job with a `postgres:16` service container (`wal_level=logical`)

### Out of Scope

- Client-facing serving layer: HTTP/SSE subscriptions, LSN-cookie protocol, TanStack DB adapter (future change, plugs into `subscribe()`)
- CVR / IVM / per-client server state (Zero's upper layers — deliberately skipped)
- DDL propagation (drift = halt in v1), sequence replication, publication auto-creation (publication must pre-exist upstream)
- Read-only enforcement wrappers for application queries (serving-layer concern)
- in-progress transaction streaming (pgoutput `streaming=on`), binary-format tuples

## Affected Areas

| Area | Impact |
|------|--------|
| `crates/pglite/src/replica/` (new: mod, wire, pgoutput, meta, backfill) | The entire feature |
| `crates/pglite/src/lib.rs` | `#[cfg(feature = "replica")]` mod + pub use |
| `crates/pglite/src/error.rs` | New variants: `ReplicaConfig`, `Upstream`, `ReplicaHalted`, `Lsn` |
| `crates/pglite/Cargo.toml` | `[features] replica = []` |
| `crates/pglite/tests/replica.rs` (new) | Decode fixtures + env-gated integration |
| `.github/workflows/ci.yml` | Postgres service container + replica test step (Linux) |
| `crates/pglite/src/db.rs`, `transaction.rs` | None expected — replica consumes the existing public API (`transaction`, `exec`, `query`, `copy_in`) |

## Dependencies

- Upstream Postgres with `wal_level=logical`, a pre-existing publication, and credentials permitting replication connections
- `postgres-protocol` 0.6 (already a dependency) for startup/SCRAM/simple-query messages; replication submessages (XLogData, PrimaryKeepalive, StandbyStatusUpdate, CopyBothResponse) are hand-framed — confirmed gap in the crate
- No new crate dependencies

## Risks

| Risk | Mitigation |
|------|------------|
| `Message::parse` cannot parse the CopyBoth stream (missing tag `W`, `w`/`k` submessages) | `Header::parse` tag+len framing with manual dispatch, only on the replication socket |
| Backfill memory on large tables | Stream COPY in bounded chunks; never buffer a whole table |
| `stop()` blocked by a socket read | `set_read_timeout` + done-flag check (matches live-module teardown discipline) |
| `tx_lock` held during apply blocks concurrent access | By design — applier is the sole writer; documented |
| PGlite closed while replica runs (`BOOTED` one-way latch) | Applier treats `Error::Closed` as stop signal; lifetime coupling documented |
| Published table without a usable PK | Halt at backfill with a clear error |
| Slot left unconsumed bloats upstream WAL | Honest standby feedback; documented slot-drop guidance on decommission |

## References

- `analysis.md` — codebase analysis (patterns, API contact points, postgres-protocol gaps)
- `blueprint.md` — component designs, signatures, phases
- `design.md` — key decisions and rationale
- `CONTEXT.md` — Replica mode, Upstream, Replication client, Applier, Watermark, Backfill, Change broadcast definitions
