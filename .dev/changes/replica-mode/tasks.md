# Tasks: replica-mode

## Progress: [37/37]

## 1. Scaffolding, config, feature gate, errors

- [x] 1.1 Add `replica = []` to `crates/pglite/Cargo.toml [features]`; add `#[cfg(feature = "replica")] mod replica;` + gated `pub use replica::{Replica, ReplicaConfig, CommittedTransaction, RowChange, Lsn};` to `lib.rs`
- [x] 1.2 Add `Error` variants `ReplicaConfig(String)`, `Upstream(String)`, `ReplicaHalted(String)`, `Lsn(String)` to `error.rs` (thiserror)
- [x] 1.3 `ReplicaConfig` + `Default` (port 5432, app name, 5s read timeout, 10s status interval) and `Lsn(u64)` with `from_pg_str`/`to_pg_str` + `Copy`/`Ord`
- [x] 1.4 `Replica` struct (Clone, direct `Arc` fields, no Inner) + `CommittedTransaction`/`RowChange` types
- [x] 1.5 `meta`: `ensure_meta_table` (`_pglite_replica` single-row DDL), `load_state`, `init_state`, `ReplicaState`
- [x] 1.6 Stub `start`/`stop`/`watermark`/`subscribe` (named thread that exits on done flag)

## 2. Wire connection (startup, SCRAM, simple query)

- [x] 2.1 `ReplConn::connect_and_auth`: `TcpStream::connect`, `startup_message` with `user`/`database`/`replication=database`/`application_name`
- [x] 2.2 SCRAM-SHA-256 loop via `postgres_protocol::authentication::sasl::ScramSha256` + `sasl_initial_response`/`sasl_response`
- [x] 2.3 `read_until_ready` buffer loop driving `Message::parse`; surface `ErrorResponse` as `Error::Upstream`
- [x] 2.4 `simple_query(sql) -> Vec<Vec<Option<String>>>` (RowDescription/DataRow/CommandComplete)
- [x] 2.5 `IDENTIFY_SYSTEM` path returning systemid/timeline/xlogpos
- [x] 2.6 Env-gated integration test (`PGLITE_REPLICA_UPSTREAM_DSN`): auth + IDENTIFY_SYSTEM round-trip; skips cleanly when unset

## 3. Slot lifecycle + snapshot backfill

- [x] 3.1 `CREATE_REPLICATION_SLOT <slot> LOGICAL pgoutput EXPORT_SNAPSHOT`; parse `consistent_point` + `snapshot_name`; handle slot-already-exists (resume path skips create)
- [x] 3.2 `introspect_published_tables`: `pg_publication_tables` + `pg_attribute`/`pg_constraint` → `TableDef`s + `CREATE TABLE` DDL (PK kept, FK stripped)
- [x] 3.3 `bootstrap_schema` into PGlite
- [x] 3.4 `copy_table`: second regular connection, `SET TRANSACTION SNAPSHOT`, `COPY TO STDOUT` → bounded chunks → `PGlite::copy_in`
- [x] 3.5 Publication fingerprint (column names + type_oid per table); `init_state` writes slot/publication/watermark(=consistent_point)/fingerprint
- [x] 3.6 Backfill integration test: rows created upstream appear in PGlite with equal counts/values

## 4. pgoutput decode + transactional streaming apply + watermark

- [x] 4.1 `pgoutput::decode`: Begin/Commit/Relation/Insert/Update/Delete/Truncate + `TupleData` (`n`/`u`/`t` cells)
- [x] 4.2 `start_replication` + `read_copy_message`: `Header::parse` framing (full frame = 1+len), CopyBothResponse assert, `w`/`k` → `ReplMsg`
- [x] 4.3 `Replica::run_stream`: Relation cache (rel_id → columns) + per-transaction event buffer
- [x] 4.4 Commit apply: skip if `end_lsn <= watermark`; else one `PGlite::transaction()` — INSERT / UPDATE-by-PK / DELETE-by-PK / TRUNCATE + `UPDATE _pglite_replica SET watermark_lsn` inside, commit, update `AtomicU64` mirror
- [x] 4.5 Cell mapping: `UnchangedToast` → omit from SET; `Null` → NULL; `Text` → typed bind with explicit cast from Relation type_oid; unknown OID → halt
- [x] 4.6 Unit tests: decode captured byte fixtures per message type (offline)
- [x] 4.7 Integration test: upstream INSERT/UPDATE/DELETE mirrored live; watermark advances monotonically

## 5. Standby feedback, resume, drift halt

- [x] 5.1 StandbyStatusUpdate on `Keepalive{reply_requested}` and on `status_interval` cadence (write=flush=apply=durable watermark)
- [x] 5.2 `set_read_timeout(read_timeout)`; on timeout check done flag → clean exit, else periodic status
- [x] 5.3 Resume path: `load_state` match → skip backfill, `START_REPLICATION` at stored watermark
- [x] 5.4 Drift halt: fingerprint check at boot + per incoming `Relation`; mismatch → `Error::ReplicaHalted`, done set, `is_halted() == true`
- [x] 5.5 `stop()` responsiveness test: thread joins within ~read_timeout
- [x] 5.6 Resume integration test: stop mid-stream, restart, continues from watermark, no gaps or duplicates

## 6. Broadcast, CI, docs, hardening

- [x] 6.1 `subscribe()` registers `mpsc::Sender`, returns `Receiver`; fan out `Arc<CommittedTransaction>` after each commit; drop dead senders on send error
- [x] 6.2 Build `CommittedTransaction.changes` from buffered events + Relation column names
- [x] 6.3 Lock-encapsulation review: `subscribers` mutated only via `&self`; brief lock scopes
- [x] 6.4 CI: `postgres:16` service container (`wal_level=logical`, publication setup step) on Linux job; run `cargo test -p pglite --features replica`
- [x] 6.5 Module docs: usage example (start → subscribe loop), lifetime coupling (PGlite close stops replica), halt behavior, scope boundaries, slot-drop guidance on decommission
- [x] 6.6 `cargo fmt --check` + `cargo clippy --features replica -- -D warnings` clean; no inline comments

---

## Notes

- Phases 1→2→3→4→5 are strictly ordered; 6.1–6.3 need Phase 4, 6.4–6.6 close out
- Milestone after Phase 4: PGlite provably mirrors a live Postgres (diff tables under load) — demoable before feedback/resume exist

### Implementation notes (2026-06-13, all 37 tasks complete)

- VERIFIED LIVE against postgres:16 (docker, wal_level=logical): backfill, streaming insert/update/delete, broadcast events in commit order, stop responsiveness (< read_timeout), restart-resume with zero duplicates, drift halt on ALTER TABLE ADD COLUMN with row-after-drift NOT applied. `tests/replica.rs` green in ~2.7s; workspace 29/29 suites green; clippy -D warnings clean.
- DEVIATION from blueprint Decision 5: apply uses literal quoting (quote-doubled text literals, untyped → coerced by assignment context), not typed binds with casts. Rationale: untyped literals coerce identically to the column type via input functions, one engine roundtrip per statement instead of two (describe+bind), and no OID→typename map needed. design.md updated.
- DEVIATION per user instruction: no code comments anywhere, including module docs — task 6.5's usage documentation lives in proposal.md/design.md instead of `//!` docs.
- Env gate for integration tests is `PGLITE_REPLICA_UPSTREAM_HOST` (+ `_PORT`/`_USER`/`_PASSWORD`/`_DB`), not the single `_DSN` var the blueprint sketched — ReplicaConfig takes discrete fields.
- Added `postgres = "0.19"` as dev-dependency only (test upstream setup); runtime tree stays tokio-free.
- `Replica` exposes `is_stopped()` and `halt_reason()` beyond the spec'd surface — needed by tests to observe teardown and drift cause.
- Tuple cells: pgoutput 'K' and 'O' tuples arrive full-width (non-identity columns as NULL/toast markers); where-clause uses flagged (key) columns for 'K'/new-fallback and all-columns IS NOT DISTINCT FROM for 'O' (REPLICA IDENTITY FULL).
- Empty upstream transactions (touching only unpublished tables) still apply a watermark-only PGlite transaction — keeps durable resume exact; revisit if upstream churn on unpublished tables becomes a measured cost.
- PRE-EXISTING ENGINE QUIRK (not replica-caused, observed while testing): dropping the last PGlite handle (implicit Close via CloseOnDrop) after replica activity can hit `FATAL: could not access status of transaction 0` (missing `pg_multixact/offsets/0000`) inside the shutdown checkpoint run by `pgl_run_atexit_funcs`, which escapes the exit trampoline and calls real `exit(1)` — silently killing the host process. Explicit `db.close().await` (the convention every test suite already follows) avoids it. Worth a separate investigation/change against the engine close path.
- Auto-reconnect on upstream connection loss is NOT implemented (halt + restart `Replica::start` is the v1 recovery path) — matches spec; candidate follow-up.
- Runnable demo added post-completion: `examples/src/bin/replica_sync.rs` + `examples/run-replica-demo.sh` (spins postgres:16 in docker, wal_level=logical, port 5433, auto-teardown). Exercises: multi-type schema (uuid/numeric/text[]/jsonb/timestamptz, FK upstream stripped on replica), 200-row backfill, atomic multi-statement transaction (one event, 6 changes), 50-row bulk update, 66-row delete, unicode/quoting fidelity, TRUNCATE, read-your-writes LSN barrier via `watermark()`, and a per-row md5 consistency audit comparing both sides. Examples crate gained optional `postgres` dep behind its `replica` feature.
