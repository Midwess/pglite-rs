# Architecture Blueprint: replica-hardening

Generated: 2026-06-13. Source designs: `zero-reference.md` (mechanisms) + `replica-mode/blueprint.md` (module structure). All changes confined to `crates/pglite/src/replica/` + tests + CI; no new modules, no new dependencies, no public-type removals.

## Design Summary

Restructure `Replica::thread_main` from single-shot into a reconnect loop with capped exponential backoff and transient/fatal error classification; split the feedback position into durable `watermark` (resume truth) and in-memory `ack_pos` (upstream confirmation, advances over data-free spans); derive status cadence from upstream's `wal_sender_timeout`; guard slot creation with `lock_timeout` and `START_REPLICATION` with a slot-in-use retry; add `Replica::decommission` for explicit teardown; matrix the CI replica job over Postgres versions.

## Component Changes

### 1. Reconnect loop — `mod.rs::thread_main` (restructure)

```
prepare(state) -> (conn, start_lsn, fingerprint)     [unchanged; fail-fast, boot_tx Err on failure]
boot_tx.send(Ok)
let mut conn = Some(conn); let mut backoff = Backoff::new();
loop {
    if done -> break Ok
    let c = match conn.take() { Some(c) => c, None => match ReplConn::connect_and_auth(&config, true) { Ok(c) => c, Err(e) if transient -> { backoff.sleep_done_aware(&done); continue } Err(e) -> { halt(e); break } } };
    match self.stream_loop(c, watermark(), &fingerprint, &backoff) {
        Ok(()) -> break                                  [done was set]
        Err(e) if Self::is_fatal(&e) -> { halt(e); break }
        Err(_) -> { ack_pos.store(watermark()); backoff.sleep_done_aware(&done); }
    }
}
stopped.store(true)
```

- `Backoff`: private struct in `mod.rs` — `next_delay()` 25ms→10s ×2 cap, `reset()`, `sleep_done_aware(&AtomicBool)` sleeping in ≤100ms slices. Pure logic unit-testable (delay sequence, cap, reset).
- Backoff reset signal: `stream_loop` resets backoff right after CopyBothResponse is accepted (successful stream establishment) — pass `&Backoff` in (least plumbing).
- `is_fatal(e: &Error) -> bool` (associated fn): `ReplicaHalted | ReplicaConfig | Lsn | Protocol` are fatal; `Database{sqlstate}` with `55000` is mapped to `ReplicaHalted` (resync-required message) before classification, connection-terminated classes (`57P01`, `57P02`, `57P03`) are transient, other Database errors fatal (unexpected SQL errors must not silently retry-loop); `Io | Upstream` transient; `Closed` is the clean-stop path (existing `halt()` behavior).
- Resume position after reconnect: durable `self.watermark()` (never `ack_pos`); `ack_pos` reset to watermark on every reconnect.

### 2. Ack position + empty transactions — `mod.rs` + `wire.rs`

- `Replica` field add: `ack_pos: Arc<AtomicU64>` (seeded = watermark at start, reset on reconnect).
- `wire.rs`: re-add `wal_end: u64` to `ReplMsg::Keepalive` (now consumed).
- `stream_loop` changes:
  - Commit with empty `TxnBuf` (`stmts.is_empty()`): skip `apply()` and broadcast entirely; `ack_pos = max(ack_pos, end_lsn)`.
  - Commit with data: `apply()` as today; `watermark = end`; `ack_pos = end`.
  - Keepalive: when `txn.is_none()` (caught up, nothing buffered): `ack_pos = max(ack_pos, wal_end)`; reply when `reply_requested` as today.
  - All `send_status` calls send `Lsn(max(watermark, ack_pos))`.
- Safety invariant (full statement in design.md): `ack_pos` may exceed `watermark` only across spans containing zero published-table changes, so WAL released by upstream past `watermark` is never needed for recovery.

### 3. Status cadence from `wal_sender_timeout` — `wire.rs` + `mod.rs`

- After `connect_and_auth` (replication conn), query via `simple_query`:
  `SELECT (EXTRACT(EPOCH FROM (setting || COALESCE(unit, 'ms'))::interval) * 1000)::bigint::text FROM pg_settings WHERE name = 'wal_sender_timeout'`
  wrapped as `ReplConn::wal_sender_timeout_ms() -> Result<Option<u64>, Error>` (None when 0/disabled or row missing).
- Effective cadence: `wst.map(|ms| min(config.status_interval, Duration::from_millis(ms * 3 / 4))).unwrap_or(config.status_interval)`.
- Stream read timeout: `min(config.read_timeout, effective_interval / 2).max(250ms)` so the loop wakes often enough to honor the cadence.

### 4. Slot operations — `mod.rs::first_run` + stream start

- `lock_timeout`: in `first_run`, before `CREATE_REPLICATION_SLOT`: `conn.simple_query("SET lock_timeout = '29s'")` (Zero's value; replication-session-local).
- Slot-in-use retry: at the `start_replication` call site — on `Error::Database { sqlstate == "55006" }` retry up to 5 times with 200ms done-aware sleeps; if still failing, treat as transient (falls into reconnect backoff).
- Slot invalidated: `Error::Database { sqlstate == "55000" }` from `start_replication` or the stream maps to `Error::ReplicaHalted("replication slot '<slot>' was invalidated (WAL retention exceeded); run Replica::decommission and restart for a full resync")` → fatal.

### 5. `Replica::decommission` — `mod.rs` (public associated fn)

```rust
pub async fn decommission(db: &PGlite, config: &ReplicaConfig) -> Result<(), Error>
```
1. `ReplConn::connect_and_auth(config, false)` (regular session)
2. `SELECT active_pid::text FROM pg_replication_slots WHERE slot_name = <lit>` → if some pid: `SELECT pg_terminate_backend(<pid>)`
3. `SELECT pg_drop_replication_slot(<lit>)` — tolerate `42704` undefined_object (already gone)
4. `meta::ensure_meta_table(db)` then `db.exec("DELETE FROM _pglite_replica")` — idempotent
5. terminate connection

Works with no `Replica` running (the common case); async signature for API consistency, drives the sync `ReplConn` inline (short-lived calls from app context).

### 6. CI matrix — `.github/workflows/ci.yml`

```yaml
  replica:
    strategy:
      fail-fast: false
      matrix:
        pg: [15, 17]
    services:
      postgres:
        image: postgres:${{ matrix.pg }}
```
Rest of the job unchanged.

## Files

| File | Change | Phase |
|---|---|---|
| `crates/pglite/src/replica/mod.rs` | Backoff struct, is_fatal, thread_main loop, ack_pos field, empty-txn skip, keepalive ack, cadence calc, decommission | 1,2,3 |
| `crates/pglite/src/replica/wire.rs` | Keepalive wal_end re-add, wal_sender_timeout_ms helper | 2 |
| `crates/pglite/tests/replica.rs` | Reconnect scenario (pg_terminate_backend), empty-txn churn scenario, decommission scenario | 4 |
| `.github/workflows/ci.yml` | replica job matrix [15, 17] | 4 |

No changes: `meta.rs`, `pgoutput.rs`, `backfill.rs`, `error.rs` (existing variants suffice), public API of existing methods.

## Implementation Phases

1. **Reconnect core** — Backoff (unit-tested), is_fatal (unit-tested), thread_main loop restructure, ack_pos reset on reconnect. Verifiable: unit tests green; existing integration test still green (no upstream interruption = single loop iteration).
2. **Feedback overhaul** — Keepalive wal_end, ack_pos advancement (empty txns + idle keepalives), max(watermark, ack_pos) in send_status, wal_sender_timeout query + effective cadence + read-timeout clamp. Verifiable: empty-txn churn upstream leaves `_pglite_replica.updated_at` unchanged while the replica stays healthy and later real txns apply.
3. **Slot ops** — lock_timeout, 55006 retry, 55000 fatal mapping, decommission(). Verifiable: decommission removes slot upstream + meta row locally; restart performs fresh backfill.
4. **Tests + CI + docs** — three integration scenarios appended to the existing test fn, CI matrix, change-doc notes, fmt/clippy clean.

## Risks

| Risk | Mitigation |
|---|---|
| Ack-beyond-durable releases WAL needed after crash | Invariant: ack_pos > watermark only over data-free spans; reconnects resume at watermark and at worst replay empties (no-ops) |
| Infinite reconnect masks a permanently-down upstream | Backoff caps at 10s; observable via `watermark()` stagnation; matches Zero's behavior; logging/metrics hook deferred |
| pg_terminate_backend reconnect test races walsender restart | Test polls watermark progression with generous timeout; slot-in-use retry absorbs the lingering-walsender window |
| `wal_sender_timeout` unit parsing differences across PG versions | Milliseconds computed inside Postgres (EXTRACT EPOCH); client receives plain integer text |
| Backoff sleep delays stop() | Done-aware sliced sleep (≤100ms granularity) |
