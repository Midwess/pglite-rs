# Tasks: replica-hardening

## Progress: [17/17]

## 1. Reconnect core

- [x] 1.1 `Backoff` struct in `replica/mod.rs`: 25ms initial, ×2, 10s cap, `reset()`, done-aware sliced sleep — with unit tests for the delay sequence, cap, and reset
- [x] 1.2 `Replica::is_fatal(&Error) -> bool` classification (ReplicaHalted/ReplicaConfig/Lsn/Protocol fatal; 55000→ReplicaHalted mapping; 57P01/57P02/57P03 transient; Io/Upstream transient; Closed = clean stop) — with unit tests
- [x] 1.3 Restructure `thread_main` into the reconnect loop (conn reuse on first iteration, reconnect via `connect_and_auth` after errors, resume from durable watermark, halt only on fatal)
- [x] 1.4 Backoff reset on successful stream establishment (after CopyBothResponse accepted in `stream_loop`)

## 2. Feedback overhaul

- [x] 2.1 Re-add `wal_end: u64` to `ReplMsg::Keepalive` in `wire.rs`
- [x] 2.2 Add `ack_pos: Arc<AtomicU64>` to `Replica` (seeded from watermark at start; reset to watermark on every reconnect)
- [x] 2.3 Empty-transaction path: Commit with no statements skips `apply()` and broadcast, advances `ack_pos` only
- [x] 2.4 Keepalive path: advance `ack_pos = max(ack_pos, wal_end)` when no transaction is buffered
- [x] 2.5 All `send_status` calls send `max(watermark, ack_pos)`
- [x] 2.6 `ReplConn::wal_sender_timeout_ms()` (pg_settings EXTRACT EPOCH query); effective cadence = min(config.status_interval, 3/4·wst); stream read timeout clamped to min(read_timeout, cadence/2).max(250ms)

## 3. Slot operations

- [x] 3.1 `SET lock_timeout = '29s'` on the replication session before `CREATE_REPLICATION_SLOT` in `first_run`
- [x] 3.2 Slot-in-use retry: 55006 on `START_REPLICATION` → up to 5 retries × 200ms done-aware, then fall through to reconnect backoff
- [x] 3.3 Slot-invalidated: 55000 → `Error::ReplicaHalted` with decommission-and-resync message (fatal)
- [x] 3.4 `Replica::decommission(db, config)`: terminate active walsender, drop slot (tolerate 42704), ensure+clear `_pglite_replica` — idempotent

## 4. Tests, CI, docs

- [x] 4.1 Integration: reconnect scenario — `pg_terminate_backend` on the walsender upstream, assert replica resumes and a subsequent insert applies with no duplicates and `is_halted() == false`
- [x] 4.2 Integration: empty-txn churn — writes to an unpublished table leave `_pglite_replica.updated_at` unchanged while the replica stays healthy and a following published write applies
- [x] 4.3 Integration: decommission — after stop: slot absent upstream, meta row gone; fresh `Replica::start` performs a full backfill
- [x] 4.4 CI: replica job `strategy.matrix.pg: [15, 17]`, `image: postgres:${{ matrix.pg }}`, fail-fast off
- [x] 4.5 `cargo fmt` + `cargo clippy --features replica --all-targets -- -D warnings` clean; update change Notes with deviations/findings

---

## Notes

- Phases strictly ordered 1→2→3→4; 4.1 depends on 1.x+3.2, 4.2 on 2.x, 4.3 on 3.4
- All integration scenarios extend the existing single test fn in `tests/replica.rs` (one PGlite per process)
- Implementation notes (2026-06-13, all 17 tasks complete):
  - VERIFIED LIVE (postgres:16 docker): walsender killed via pg_terminate_backend -> replica reconnected and applied the next insert with no duplicates and is_halted()==false; unpublished-table churn left _pglite_replica.updated_at untouched while the replica stayed healthy; decommission removed slot+meta and the following start ran a fresh 4-column backfill picking up the post-drift row. Full suite 16/16 result-lines ok; clippy -D warnings clean.
  - DEVIATION from blueprint: wal_sender_timeout read as plain `SELECT setting FROM pg_settings` (the GUC's native unit is ms on all supported versions) instead of the EXTRACT(EPOCH ...) interval round-trip - simpler and version-stable.
  - wal_sender_timeout is queried BEFORE START_REPLICATION (simple queries are unavailable once CopyBoth begins).
  - is_fatal treats 55006 as transient so exhausted slot-in-use retries degrade into the reconnect backoff rather than halting.
  - Closed remains is_fatal=true by design: halt() already special-cases it as a clean stop (no halted flag).
