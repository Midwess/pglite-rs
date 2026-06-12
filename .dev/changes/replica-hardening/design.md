# Design: replica-hardening

## Overview

Every mechanism here is a transfer from zero-cache (citations in `replica-mode/zero-reference.md`), valid because both systems are logical-replication consumers with a durable applied-LSN. The two places Zero's solutions were NOT transferred are recorded as explicit decisions (4, 5) — their machinery solves multi-shard contention we don't have.

## Key Decisions

### Decision 1: Reconnect semantics — infinite capped retry, fail-fast boot

**Context:** Today any error after boot halts permanently.
**Options:**
1. Bounded retries then halt — predictable termination, but picks an arbitrary N and turns long upstream outages into permanent halts requiring app intervention
2. Infinite retry with 10s cap (Zero's behavior) — replica always self-heals when upstream returns; "down" is observable via watermark stagnation
**Decision:** Option 2 for the streaming phase. `Replica::start` itself stays fail-fast (misconfiguration should surface immediately at boot, not spin silently); the reconnect machinery arms only after the first successful stream establishment. Backoff resets on success, sleeps are done-aware (≤100ms slices) so `stop()` stays responsive.

### Decision 2: Error taxonomy

**Context:** Retrying everything would loop forever on drift; halting on everything is today's bug.
**Decision:** Fatal = `ReplicaHalted` (drift, invalidated slot), `ReplicaConfig`, `Lsn`, `Protocol` (decode errors imply corruption — never retry into them), and unexpected `Database` errors. Transient = `Io`, `Upstream`, and Postgres connection-termination sqlstates (`57P01/57P02/57P03`). `Closed` (PGlite gone) remains the clean-stop path. Sqlstate `55000` is mapped to `ReplicaHalted` with a decommission-and-resync message before classification; `55006` gets its own bounded retry at the `START_REPLICATION` site before degrading to transient.

### Decision 3: Ack position — the safety invariant

**Context:** Empty upstream transactions currently cost one durable PGlite transaction each; but acking upstream beyond the durable watermark lets Postgres free WAL we might appear to need after a crash.
**Decision:** Maintain `ack_pos: AtomicU64` beside `watermark`. **Invariant: ack_pos may exceed watermark only across spans containing zero published-table changes.** Consequences: (a) WAL between watermark and ack_pos contains only empty transactions, so losing it loses nothing; (b) resume always uses watermark — if upstream replays the span, the empty transactions re-advance ack_pos in memory and are never applied; if upstream starts later (it freed that WAL), nothing was missed by construction. ack_pos resets to watermark on every reconnect (conservative; costs at most re-acking a span). This is exactly Zero's "ack keepalive when caught up" semantics made explicit.

### Decision 4: Decommission instead of slot pools + sweepers (deliberate non-adoption)

**Context:** Zero names slots from a pool under advisory locks and sweeps unclaimed slots every 30s — because many shards and competing processes share one upstream.
**Decision:** We have exactly one slot per replica with one owner process. A public `Replica::decommission(db, config)` (terminate walsender → drop slot → clear meta) plus the invalidated-slot fatal error delivers the same WAL-bloat safety in ~30 lines. Adopting the pool/sweeper would add concurrency machinery with no concurrency to manage.

### Decision 5: CI image matrix instead of testcontainers (deliberate non-adoption)

**Context:** Zero runs per-test Docker containers across PG 15–18 with per-test databases.
**Decision:** One integration test binary, one env-gated container: matrix the existing CI replica job over `postgres:[15, 17]` (oldest-supported and current). Equivalent regression surface for our scale; zero new test infrastructure. PG16 remains covered by local dev runs and can join the matrix any time for one line.

### Decision 6: Cadence and read-timeout coupling

**Context:** Status cadence must beat `wal_sender_timeout`, and the loop only wakes on read timeouts.
**Decision:** Effective cadence = `min(config.status_interval, 3/4 × wal_sender_timeout)` with the timeout read once at stream start via server-side `EXTRACT(EPOCH ...)` (no client unit parsing). Stream read timeout clamped to `min(config.read_timeout, cadence/2)` floored at 250ms, guaranteeing at least two wakeups per cadence window. `wal_sender_timeout = 0` (disabled) falls back to the configured interval unchanged.

## API Changes

Additive only:
```rust
impl Replica {
    pub async fn decommission(db: &PGlite, config: &ReplicaConfig) -> Result<(), Error>;
}
```
Behavioral changes: transient errors no longer halt (`is_halted()` stays false through reconnect cycles); empty upstream transactions no longer produce `_pglite_replica` writes or broadcasts (they never produced broadcasts).

## Security Considerations

- `pg_terminate_backend` in decommission targets only the pid bound to the replica's own slot (queried by slot name); slot name comes from config, literal-escaped as elsewhere
- No new credential surfaces; reconnect reuses the same `ReplicaConfig`

## Performance Considerations

- Empty-txn churn drops from 3 engine roundtrips per transaction to zero
- Reconnect adds no steady-state cost; backoff state is thread-local to the replica thread
