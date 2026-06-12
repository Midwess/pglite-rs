# Tasks: replica-mode

## Progress: [0/28]

## 1. Scaffolding, config, feature gate, errors

- [ ] 1.1 Add `replica = []` to `crates/pglite/Cargo.toml [features]`; add `#[cfg(feature = "replica")] mod replica;` + gated `pub use replica::{Replica, ReplicaConfig, CommittedTransaction, RowChange, Lsn};` to `lib.rs`
- [ ] 1.2 Add `Error` variants `ReplicaConfig(String)`, `Upstream(String)`, `ReplicaHalted(String)`, `Lsn(String)` to `error.rs` (thiserror)
- [ ] 1.3 `ReplicaConfig` + `Default` (port 5432, app name, 5s read timeout, 10s status interval) and `Lsn(u64)` with `from_pg_str`/`to_pg_str` + `Copy`/`Ord`
- [ ] 1.4 `Replica` struct (Clone, direct `Arc` fields, no Inner) + `CommittedTransaction`/`RowChange` types
- [ ] 1.5 `meta`: `ensure_meta_table` (`_pglite_replica` single-row DDL), `load_state`, `init_state`, `ReplicaState`
- [ ] 1.6 Stub `start`/`stop`/`watermark`/`subscribe` (named thread that exits on done flag)

## 2. Wire connection (startup, SCRAM, simple query)

- [ ] 2.1 `ReplConn::connect_and_auth`: `TcpStream::connect`, `startup_message` with `user`/`database`/`replication=database`/`application_name`
- [ ] 2.2 SCRAM-SHA-256 loop via `postgres_protocol::authentication::sasl::ScramSha256` + `sasl_initial_response`/`sasl_response`
- [ ] 2.3 `read_until_ready` buffer loop driving `Message::parse`; surface `ErrorResponse` as `Error::Upstream`
- [ ] 2.4 `simple_query(sql) -> Vec<Vec<Option<String>>>` (RowDescription/DataRow/CommandComplete)
- [ ] 2.5 `IDENTIFY_SYSTEM` path returning systemid/timeline/xlogpos
- [ ] 2.6 Env-gated integration test (`PGLITE_REPLICA_UPSTREAM_DSN`): auth + IDENTIFY_SYSTEM round-trip; skips cleanly when unset

## 3. Slot lifecycle + snapshot backfill

- [ ] 3.1 `CREATE_REPLICATION_SLOT <slot> LOGICAL pgoutput EXPORT_SNAPSHOT`; parse `consistent_point` + `snapshot_name`; handle slot-already-exists (resume path skips create)
- [ ] 3.2 `introspect_published_tables`: `pg_publication_tables` + `pg_attribute`/`pg_constraint` → `TableDef`s + `CREATE TABLE` DDL (PK kept, FK stripped)
- [ ] 3.3 `bootstrap_schema` into PGlite
- [ ] 3.4 `copy_table`: second regular connection, `SET TRANSACTION SNAPSHOT`, `COPY TO STDOUT` → bounded chunks → `PGlite::copy_in`
- [ ] 3.5 Publication fingerprint (column names + type_oid per table); `init_state` writes slot/publication/watermark(=consistent_point)/fingerprint
- [ ] 3.6 Backfill integration test: rows created upstream appear in PGlite with equal counts/values

## 4. pgoutput decode + transactional streaming apply + watermark

- [ ] 4.1 `pgoutput::decode`: Begin/Commit/Relation/Insert/Update/Delete/Truncate + `TupleData` (`n`/`u`/`t` cells)
- [ ] 4.2 `start_replication` + `read_copy_message`: `Header::parse` framing (full frame = 1+len), CopyBothResponse assert, `w`/`k` → `ReplMsg`
- [ ] 4.3 `Replica::run_stream`: Relation cache (rel_id → columns) + per-transaction event buffer
- [ ] 4.4 Commit apply: skip if `end_lsn <= watermark`; else one `PGlite::transaction()` — INSERT / UPDATE-by-PK / DELETE-by-PK / TRUNCATE + `UPDATE _pglite_replica SET watermark_lsn` inside, commit, update `AtomicU64` mirror
- [ ] 4.5 Cell mapping: `UnchangedToast` → omit from SET; `Null` → NULL; `Text` → typed bind with explicit cast from Relation type_oid; unknown OID → halt
- [ ] 4.6 Unit tests: decode captured byte fixtures per message type (offline)
- [ ] 4.7 Integration test: upstream INSERT/UPDATE/DELETE mirrored live; watermark advances monotonically

## 5. Standby feedback, resume, drift halt

- [ ] 5.1 StandbyStatusUpdate on `Keepalive{reply_requested}` and on `status_interval` cadence (write=flush=apply=durable watermark)
- [ ] 5.2 `set_read_timeout(read_timeout)`; on timeout check done flag → clean exit, else periodic status
- [ ] 5.3 Resume path: `load_state` match → skip backfill, `START_REPLICATION` at stored watermark
- [ ] 5.4 Drift halt: fingerprint check at boot + per incoming `Relation`; mismatch → `Error::ReplicaHalted`, done set, `is_halted() == true`
- [ ] 5.5 `stop()` responsiveness test: thread joins within ~read_timeout
- [ ] 5.6 Resume integration test: stop mid-stream, restart, continues from watermark, no gaps or duplicates

## 6. Broadcast, CI, docs, hardening

- [ ] 6.1 `subscribe()` registers `mpsc::Sender`, returns `Receiver`; fan out `Arc<CommittedTransaction>` after each commit; drop dead senders on send error
- [ ] 6.2 Build `CommittedTransaction.changes` from buffered events + Relation column names
- [ ] 6.3 Lock-encapsulation review: `subscribers` mutated only via `&self`; brief lock scopes
- [ ] 6.4 CI: `postgres:16` service container (`wal_level=logical`, publication setup step) on Linux job; run `cargo test -p pglite --features replica`
- [ ] 6.5 Module docs: usage example (start → subscribe loop), lifetime coupling (PGlite close stops replica), halt behavior, scope boundaries, slot-drop guidance on decommission
- [ ] 6.6 `cargo fmt --check` + `cargo clippy --features replica -- -D warnings` clean; no inline comments

---

## Notes

- Phases 1→2→3→4→5 are strictly ordered; 6.1–6.3 need Phase 4, 6.4–6.6 close out
- Milestone after Phase 4: PGlite provably mirrors a live Postgres (diff tables under load) — demoable before feedback/resume exist
- Implementation notes will be added here during development
