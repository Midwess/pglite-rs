# Zero-Cache Reference: How Zero Handles Our Hardening Backlog

Source: `.dev/mono/packages/zero-cache` (read 2026-06-13). Per backlog item: Zero's mechanism → adopt/adapt/skip verdict for pglite-rs replica mode.

## 1. Auto-reconnect / retry

**Zero**: reconnect-in-place loop, exponential backoff 25ms → 10s cap, ×2 per attempt, no jitter; `resetBackoff()` on success (`running-state.ts:8-9,153,182`; `change-streamer-service.ts:373-388`). Resume position = durable `lastWatermark` (`storer.ts:205`). Error taxonomy: transient (network, generic) → backoff; fatal (`AutoResetSignal` on slot invalidation / publication drift) → stop + full resync; special case "slot in use by another process" (`PG_OBJECT_IN_USE`) → 5 retries × 10ms before failing (`stream.ts:168-203`).

**Verdict — ADOPT**, near verbatim:
- Backoff 25ms→10s ×2, reset on successful stream establishment; loop inside the existing `pglite-replica` thread around `connect_and_auth` + `start_replication` + `stream_loop`.
- Error classification: `Error::Io`/`Upstream(connection closed)` → transient; `ReplicaHalted` (drift) / slot-invalidated sqlstate (`55000` object_not_in_prerequisite_state) → fatal halt (resync required); `42710` family already handled.
- The slot-in-use retry matters for us specifically: after a crash, upstream's old walsender lingers seconds; without the 5×10ms (we should use a few × 200ms) retry, restart-after-crash fails spuriously.

## 2. Slot lifecycle / WAL retention

**Zero**: slot created under `pg_advisory_xact_lock` from a name pool (`<app>_<shard>_<a..z>`); periodic `dropUnclaimedSlots()` sweep every 30s; decommission = `pg_terminate_backend(active_pid)` + `pg_drop_replication_slot` (`decommission.ts:18-34`); WAL bloat protection delegated to Postgres `max_slot_wal_keep_size` — invalidated slot (`walStatus='lost'`) detected at startup → `AutoResetSignal` → fresh full resync (`change-source.ts:238-243`).

**Verdict — ADAPT (subset)**:
- Skip the pool + 30s sweeper (multi-shard machinery; we have exactly one slot per replica).
- Adopt: detect slot-invalidated on start/stream → halt with explicit "slot lost, resync required: delete meta row or call decommission" message.
- Adopt: `Replica::decommission(config)` associated fn = terminate backend + drop slot (today orphaned slots silently bloat upstream WAL).
- Document `max_slot_wal_keep_size` as the operator-side guard, exactly as Zero does.

## 3. Standby feedback / empty-transaction watermark

**Zero**: ack per-commit immediately; keepalives acked with their `wal_end` whenever downstream is caught up — even with zero data flowing (`change-source.ts:622-641`); manual keepalive at **75% of `wal_sender_timeout`** read from `pg_settings` (`stream.ts:77-104`); `skipAck` flag for transactions that must not advance confirmation.

**Verdict — ADOPT, and it deletes our empty-txn cost**:
- Current: every empty upstream transaction costs a durable watermark-only PGlite transaction. Zero's pattern: for empty txns / idle keepalives, advance the **in-memory** confirmed position and ack that to upstream; durable watermark only moves with real data. Resume then replays a tail of empty txns — harmless and convergent (skip-by-watermark applies them as cheap no-data writes only until the first real txn re-anchors).
- Adopt: query `wal_sender_timeout` once at stream start; send status at 75% of it instead of our fixed 10s `status_interval` default (keep config override). Eliminates disconnect risk on servers configured below our interval.

## 4. Shutdown / crash recovery

**Zero**: SIGTERM drain order user-facing → supporting workers (`life-cycle.ts:66-79`); storer drains queue to a `stop` sentinel, aborts any uncommitted transaction (`storer.ts:357-385`); crash-mid-apply recovery = durable watermark + upstream replay (identical to ours); SQLite close is implicitly checkpointing and exception-safe.

**Verdict — ALREADY EQUIVALENT, one lesson**: our buffered-txn-dropped-on-stop and watermark-resume match. The lesson is negative space: Zero never worries about close because their replica DB close cannot take down the process. Reinforces the engine close-path fix (`pg_multixact` FATAL → real `exit(1)`) as the highest-priority follow-up — it's the one place we're structurally weaker than the reference implementation.

## 5. Initial-sync crash / partial backfill

**Zero**: no incremental recovery — marker row (`replicationState`) written only after snapshot copy completes; crashed sync leaves an inactive slot that gets dropped; next attempt = fresh slot + full re-sync (`replication-slots.ts:156-167,230-285`).

**Verdict — VALIDATED, no change**: our design is identical (meta row after backfill; slot-exists → drop + recreate). Independent confirmation the approach is the industry one.

## 6. TLS / connection config

**Zero**: `sslmode` from URL (`disable`/`no-verify`/default `prefer`, `pg.ts:367-377`); `application_name` always set; **`lock_timeout` ≈ 29s on the slot-creating session** so `CREATE_REPLICATION_SLOT` cannot hang behind a long-running transaction (`replication-slots.ts:72-74`); replication pool `max: 1`.

**Verdict — ADOPT two cheap wins now, TLS later**:
- `lock_timeout` on our replication connection before `CREATE_REPLICATION_SLOT` — one `SET` statement, removes a real production hang.
- TLS: follow their mode taxonomy (`disable` / `require` / `verify`) when we add rustls; default plaintext-with-documented-limitation until then.

## 7. Live-Postgres test infra

**Zero**: testcontainers per `.pg.test.ts` file; CI matrix over PG 15/16/17/18 images with `wal_level=logical`; per-test database with `DROP DATABASE ... WITH (FORCE)` + slot cleanup (`pg-container-setup.ts`, `js.yml:145-166`).

**Verdict — ADAPT lightly**: our env-gated single docker container is the right weight for one integration test. Worth stealing: CI matrix over postgres image versions (15/16/17) on the existing replica job — a one-line `strategy.matrix.image` change; and per-run unique slot/table names if tests ever parallelize.

## Priority order implied by this review

1. Engine close fix (item 4's lesson) — separate change, publish blocker.
2. Reconnect-with-backoff + slot-in-use retry + lock_timeout + keepalive-ack/75%-interval (items 1, 3, 6) — one cohesive "replica-hardening" change, all in the replica thread loop.
3. `Replica::decommission` + slot-invalidated detection (item 2) — small follow-up.
4. CI postgres version matrix (item 7) — anytime.
