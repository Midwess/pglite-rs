# Proposal: Replica Hardening — Zero-Derived Resilience Patterns, Sized for a Single Replica

**Status**: approved

## Summary

Make replica mode survive real-world upstream conditions by adopting the five production patterns identified in the zero-cache reference study (`replica-mode/zero-reference.md`): reconnect with capped backoff, slot-in-use retry, keepalive-driven ack position for empty transactions, `wal_sender_timeout`-derived feedback cadence, and `lock_timeout`-guarded slot creation — plus the deliberately sized-down replacements for Zero's two multi-shard mechanisms: `Replica::decommission` (instead of slot pools + sweepers) and a CI Postgres version matrix (instead of a testcontainers harness).

## Motivation

Today any transient upstream hiccup — network blip, Postgres restart, failover — permanently halts the replica; recovery requires application code to notice and call `Replica::start` again. Every empty upstream transaction (activity on unpublished tables) costs a durable engine write. A `wal_sender_timeout` below our fixed 10s status interval disconnects us. `CREATE_REPLICATION_SLOT` can hang forever behind a long-running upstream transaction. Restart-after-crash can fail spuriously while the old walsender lingers. Orphaned slots silently bloat upstream WAL after teardown. Zero-cache solved each of these in production; the mechanisms transfer mechanically because both systems are logical-replication consumers with a durable applied-LSN.

## Scope

### In Scope

- Reconnect-in-place loop in the replica thread: exponential backoff 25ms→10s (×2, reset on successful stream establishment), infinite retry for transient errors, done-flag-aware sleeping
- Transient/fatal error classification (`is_fatal`): drift, config, protocol, invalidated-slot → halt; IO/connection-terminated → retry
- Slot-in-use (`55006`) retry on `START_REPLICATION` (5 × 200ms) for the lingering-walsender window
- **Ack position**: in-memory confirmed LSN beside the durable watermark; empty transactions and idle keepalives advance and ack it without touching PGlite; durable watermark moves only with real data
- Status cadence = min(config, 75% of upstream `wal_sender_timeout` read from `pg_settings`); read-timeout clamped to honor it
- `SET lock_timeout = '29s'` before `CREATE_REPLICATION_SLOT`
- Slot-invalidated (`55000`) → fatal halt with explicit "decommission and restart for full resync" message
- `Replica::decommission(db, config)`: terminate walsender, drop slot (idempotent), clear meta row
- CI replica job matrixed over `postgres:[15, 17]`
- Integration scenarios: reconnect-after-`pg_terminate_backend`, empty-txn churn (no durable writes), decommission + fresh re-backfill

### Out of Scope

- Engine close-path fix (`pg_multixact` FATAL → `exit(1)`) — separate prioritized change
- TLS to upstream (rustls) — follow-up; mode taxonomy reserved per Zero (`disable`/`require`/`verify`)
- Retry during initial `Replica::start` — fail-fast at boot stays (reconnect machinery arms only after first successful stream)
- Zero's slot name pools, advisory-lock slot creation, periodic unclaimed-slot sweeper — multi-shard machinery, replaced by `decommission` (recorded as deliberate non-adoption)
- Zero's testcontainers per-test-database harness — replaced by CI image matrix on the existing env-gated test
- Logging/metrics for reconnect attempts — no logging facility in the crate yet

## Affected Areas

| Area | Impact |
|------|--------|
| `crates/pglite/src/replica/mod.rs` | thread_main reconnect loop, Backoff, is_fatal, ack_pos, empty-txn skip, cadence, decommission |
| `crates/pglite/src/replica/wire.rs` | Keepalive `wal_end` re-added, `wal_sender_timeout_ms()` helper |
| `crates/pglite/tests/replica.rs` | Three new scenarios appended to the end-to-end test |
| `.github/workflows/ci.yml` | replica job: image matrix [15, 17] |
| `meta.rs`, `pgoutput.rs`, `backfill.rs`, `error.rs` | No changes |

## Dependencies

- `replica-mode` change complete (it is)
- No new crate dependencies

## Risks

| Risk | Mitigation |
|------|------------|
| Acking beyond the durable watermark releases WAL needed after a crash | Invariant: ack position exceeds watermark only across data-free spans; reconnect resumes at watermark, worst case replays empty transactions (no-ops) |
| Infinite reconnect hides a dead upstream | Capped 10s backoff; `watermark()` stagnation observable by the app; identical trade-off Zero ships |
| Reconnect test flaky around walsender teardown | Slot-in-use retry absorbs the window; test polls with generous timeouts |
| Cross-version `pg_settings` unit handling | Milliseconds computed server-side via EXTRACT(EPOCH ...) |

## References

- `zero-reference.md` (in `replica-mode/`) — mechanism citations into zero-cache
- `analysis.md`, `blueprint.md`, `design.md` — this change
- `CONTEXT.md` — Ack position, Decommission definitions
