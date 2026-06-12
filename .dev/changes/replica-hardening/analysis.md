# Codebase Analysis: replica-hardening

Generated: 2026-06-13. Agent re-exploration skipped deliberately: the replica module was authored in full during the `replica-mode` change (same session, all file/line knowledge current), and the external reference study is already written up with citations. This file records the delta-relevant facts and points at the two source documents.

## Analysis inputs

1. `.dev/changes/replica-mode/analysis.md` + `blueprint.md` — module structure, conventions, constraints (all still accurate; module unchanged since completion)
2. `.dev/changes/replica-mode/zero-reference.md` — how zero-cache handles each hardening concern, with file:line citations into `.dev/mono/packages/zero-cache`

## Current code facts relevant to this change

### Thread/loop structure (`crates/pglite/src/replica/mod.rs`)
- `Replica::thread_main` — single-shot: `prepare()` → boot ack → `stream_loop()` → on `Err` → `halt()` → thread exits. No retry of any kind; every transient error is terminal today.
- `Replica::prepare` → `first_run` (slot create + backfill) or resume from `meta::load_state`. Returns the live `ReplConn` for streaming.
- `Replica::stream_loop` — owns: fingerprint map, `rels` cache, `txn: Option<TxnBuf>`, `last_status: Instant`, fixed `config.status_interval` cadence (default 10s), `read_copy_message` timeout-driven wakeups.
- Empty upstream transactions currently flow through `apply()` — a durable BEGIN/UPDATE-watermark/COMMIT against the engine per empty txn.
- `halt(e)` — `Error::Closed` = clean stop; everything else sets `halted` + `halt_reason`.

### Wire layer (`crates/pglite/src/replica/wire.rs`)
- `ReplMsg::Keepalive { reply_requested }` — `wal_end` was removed as dead code during replica-mode; this change re-adds it (the ack-position feature is its consumer).
- `send_status(watermark, reply)` sends write=flush=apply=watermark.
- `connect_and_auth(config, replication: bool)` — reusable for reconnects as-is.
- `simple_query` available on the replication connection (`replication=database` sessions accept full SQL) — usable for `SET lock_timeout`, `pg_settings` reads, and decommission statements.
- `start_replication` surfaces upstream `ErrorResponse` as `Error::Database { sqlstate, .. }` — sqlstates needed: `55006` (object_in_use → retry), `55000` (object_not_in_prerequisite_state / invalidated slot → fatal), `42710` (duplicate slot, already handled in `first_run`).

### State surface (`mod.rs`)
- `watermark: Arc<AtomicU64>` — durable applied position mirror. The new `ack_pos: Arc<AtomicU64>` sits beside it (same pattern, no Inner struct).
- `done`/`stopped`/`halted`/`halt_reason` — teardown/halt flags; reconnect loop must check `done` inside backoff sleeps.

### Error enum (`error.rs`)
- `ReplicaConfig`, `Upstream`, `ReplicaHalted`, `Lsn`, `Database{sqlstate,..}`, `Io`, `Closed` — sufficient for transient/fatal classification; no new variants required.

### Tests (`crates/pglite/tests/replica.rs`)
- Env-gated single end-to-end test (`PGLITE_REPLICA_UPSTREAM_HOST`); upstream manipulated via `postgres` dev-dependency — `pg_terminate_backend` is callable from there, which makes the reconnect scenario testable without any network tricks.
- One PGlite per test process (BOOTED latch) — all new scenarios must extend the existing single test fn, sequentially.

### CI (`.github/workflows/ci.yml`)
- `replica` job: single `postgres:16` service container, `ALTER SYSTEM SET wal_level = logical` + `docker restart` trick, env-gated test run. Matrix-ready (image is a literal string today).

## Zero-reference facts driving the design (citations in zero-reference.md)

| Concern | Zero mechanism | Sized-down adoption |
|---|---|---|
| Reconnect | 25ms→10s ×2 backoff, reset on success, reconnect-in-place, resume from durable watermark | Same, inside `thread_main` loop |
| Slot in use | 5 retries on `PG_OBJECT_IN_USE` | Same shape (≈5 × 200ms) |
| Fatal classification | `AutoResetSignal` on invalidated slot / publication drift | `ReplicaHalted` with resync-required message |
| Empty txns / idle | Ack keepalive `wal_end` when caught up; durable position only on real commits | `ack_pos` beside `watermark` |
| Cadence | 75% of `wal_sender_timeout` from `pg_settings` | Same query; `min(config.status_interval, 3/4·wst)` |
| Slot creation hang | `lock_timeout` ≈ 29s on slot-creating session | One `SET` before `CREATE_REPLICATION_SLOT` |
| Teardown | `pg_terminate_backend` + `pg_drop_replication_slot` (decommission.ts) | `Replica::decommission(db, config)` + meta-row delete |
| Test matrix | testcontainers, PG 15–18 matrix | CI service-image matrix [15, 17] |

## Deliberately NOT adopted (the two overkill items)

- **Slot name pool + advisory-lock creation + 30s unclaimed-slot sweeper** — exists because zero-cache runs many shards with competing processes against one upstream. We have exactly one slot per replica; `decommission()` plus the slot-invalidated fatal error give equivalent safety.
- **Testcontainers harness with per-test databases** — our single env-gated container + CI image matrix covers the same regression surface for one integration test binary.

## Constraints carried over unchanged

- No tokio; backoff sleeping must be done-flag-aware (sliced `thread::sleep`, no async timers)
- Applier remains sole PGlite writer; `Error::Closed` remains the clean-stop signal
- No inline code comments (user directive, standing)
- `start()` stays fail-fast: reconnect machinery activates only after a first successful stream establishment
