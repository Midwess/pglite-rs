# Architecture Blueprint: replica-mode

Generated: 2026-06-12 by code-architect
Based on: `.dev/changes/replica-mode/analysis.md`

## Design Summary

Add an opt-in `replica` cargo feature: a new module `crates/pglite/src/replica/` whose `Replica` struct owns a dedicated `pglite-replica` OS thread that connects to an upstream Postgres over `std::net::TcpStream` using `postgres-protocol` 0.6 for startup/SCRAM/simple-query, then drives consistent snapshot backfill (COPY under an exported slot snapshot) and transactional `pgoutput` streaming apply into a single `PGlite` handle — with an atomic LSN watermark persisted in a `_pglite_replica` meta table, standby-status feedback, restart-as-resume, publication-drift halt, and an in-process committed-transaction broadcast via `Replica::subscribe()`. No tokio; mirrors the `live`/`engine` thread precedents exactly (`Arc<AtomicBool>` done flag + explicit `stop()`, `futures::executor::block_on` to drive async PGlite calls).

## Design Decisions

| Decision | Options Considered | Chosen | Rationale |
|----------|-------------------|--------|-----------|
| Concurrency model | tokio task; OS thread + blocking sockets | **OS thread + `std::net::TcpStream` + `block_on`** | Hard constraint; mirrors `engine.rs`/`live/mod.rs`; crate has zero tokio in its tree |
| Module decomposition | one big `mod.rs`; split by concern | **`mod.rs` (Replica, lifecycle, broadcast, applier) + `wire.rs` (conn/auth/framing) + `pgoutput.rs` (decode) + `meta.rs` (watermark/fingerprint) + `backfill.rs` (snapshot COPY)** | Each file one cohesive concern; only `Replica` + broadcast payload are public |
| Post-START_REPLICATION framing | `Message::parse`; manual `Header::parse` | **`Header::parse` + manual tag dispatch** | `Message::parse` errors on `CopyBothResponse` (tag `W`) and `w`/`k` submessages — confirmed in postgres-protocol 0.6.11 source |
| Auth/regular-query framing | hand-roll; reuse `Message::parse` | **Reuse `Message::parse` for startup→ReadyForQuery and all simple queries** | All auth/query message variants present; only the CopyBoth stream needs manual framing |
| Applier writer | direct engine cmd; PGlite public API | **PGlite public API only (`transaction()`, `tx.exec`, `tx.query`, `tx.commit`); `copy_in` for backfill** | Applier is a plain PGlite user; no `db.rs` changes |
| Apply SQL shape | upsert; plain statements | **Plain `INSERT` / `UPDATE ... WHERE pk` / `DELETE ... WHERE pk`** | Skip-by-watermark gives exactly-once, so no upsert needed; apply mirrors upstream events 1:1 |
| One txn = one PGlite txn | per-row autocommit; buffer per txn | **Buffer between `Begin`/`Commit`, apply as one `PGlite::transaction()` with watermark UPDATE inside** | Atomicity of watermark + data (constraint) |
| Watermark / LSN type | text only; u64 | **`Lsn(u64)` newtype, stored as canonical `X/Y` text in meta table; `Arc<AtomicU64>` mirror for cheap reads** | 64-bit value; human-readable persistence |
| Meta table | schema-qualified; single underscore table | **single `_pglite_replica` table, one row (`CHECK (id = 1)`)** | Simplicity First; matches `_notify_trigger_*` internal-object convention |
| Backfill apply path | row exec batches; `copy_in` | **`PGlite::copy_in` with COPY rows streamed in bounded chunks** | Native COPY path exists (`db.rs:230`); chunking bounds memory |
| Broadcast transport | NOTIFY; tokio broadcast; std mpsc fan-out | **`Vec<std::sync::mpsc::Sender<Arc<CommittedTransaction>>>` behind `Arc<Mutex<>>`, drop dead senders on send error** | std-only; lock-encapsulated per CLAUDE.md |
| Teardown of blocking read | unbounded read; read timeout | **`set_read_timeout` + done-flag check on `WouldBlock`/`TimedOut`** | `stop()` responsive without waiting for next keepalive |
| Schema bootstrap | pg_dump; introspection | **introspect `pg_publication_tables` + `pg_attribute`/`pg_constraint`; emit `CREATE TABLE` (PKs kept, FKs stripped)** | No external binary; FK-stripped for apply-order independence |
| Drift policy v1 | reconcile; halt | **halt loudly: fingerprint check at start + `Relation` mismatch mid-stream → `Error::ReplicaHalted`, done flag set** | Constraint |

## Component Design

### `Replica` (public) — `src/replica/mod.rs`
Public handle and lifecycle owner. Validates `ReplicaConfig`, ensures meta table, loads resume watermark, spawns/owns the `pglite-replica` thread (boot result via `oneshot`), owns the broadcast registry. The applier loop is a method (`Replica::run_stream`) on a `Replica` clone — no separate `Applier` struct (Least New Definitions: applier state is `db` + `watermark` + `subscribers`, already owned by `Replica`).

Fields (all `Arc`-wrapped directly, `#[derive(Clone)]`, no `XxxInner`):
```rust
db: PGlite
done: Arc<AtomicBool>
watermark: Arc<AtomicU64>
subscribers: Arc<std::sync::Mutex<Vec<mpsc::Sender<Arc<CommittedTransaction>>>>>
handle: Arc<JoinHandle<()>>
config: Arc<ReplicaConfig>
```

Public API:
```rust
pub async fn start(db: PGlite, config: ReplicaConfig) -> Result<Replica, Error>;
pub fn stop(&self);
pub fn watermark(&self) -> Lsn;
pub fn subscribe(&self) -> std::sync::mpsc::Receiver<Arc<CommittedTransaction>>;
pub fn is_halted(&self) -> bool;
```

### `ReplConn` (internal) — `src/replica/wire.rs`
Owns one upstream TCP connection and all wire framing. Constructed and used only on the replica thread (no locks). Functions:
- `connect_and_auth(config)` — TCP connect, `startup_message` with `replication=database`, SCRAM via `postgres_protocol::authentication::sasl::ScramSha256`, read to ReadyForQuery
- `simple_query(sql) -> Vec<Vec<Option<String>>>` — IDENTIFY_SYSTEM, CREATE_REPLICATION_SLOT, introspection
- `start_replication(slot, start_lsn, publication)` — sends START_REPLICATION, asserts CopyBothResponse (tag `W`) via `Header::parse`
- `read_copy_message() -> Result<Option<ReplMsg>, Error>` — CopyBoth framing loop; `None` on read timeout so caller checks done flag
- `send_standby_status(write, flush, apply, now, reply)` — hand-serialized `r` message in `frontend::CopyData`

Framing facts (verified in postgres-protocol 0.6.11 source):
- PG frame: `[tag:u8][len:i32_be][body]`; `len` excludes the tag → full frame = `1 + len` bytes; `Header::parse` does not consume — caller advances the buffer
- XLogData `w`: `[wal_start:i64][wal_end:i64][send_time:i64][wal_data...]`
- PrimaryKeepalive `k`: `[wal_end:i64][send_time:i64][reply_requested:u8]`
- StandbyStatusUpdate `r`: `[write:i64][flush:i64][apply:i64][client_time:i64][reply:u8]`

```rust
pub(crate) enum ReplMsg {
    XLogData { wal_start: u64, wal_end: u64, send_time: i64, data: Bytes },
    Keepalive { wal_end: u64, send_time: i64, reply_requested: bool },
    CopyDone,
}
```

### `pgoutput` decoder (internal) — `src/replica/pgoutput.rs`
Pure decode of XLogData `wal_data` into proto_version `1` messages — fully unit-testable offline from byte fixtures.

```rust
pub(crate) enum PgOutputMsg {
    Begin { final_lsn: u64, commit_ts: i64, xid: u32 },
    Commit { commit_lsn: u64, end_lsn: u64, commit_ts: i64 },
    Relation { rel_id: u32, namespace: String, name: String, replica_identity: u8, columns: Vec<RelColumn> },
    Insert { rel_id: u32, new: TupleData },
    Update { rel_id: u32, key: Option<TupleData>, old: Option<TupleData>, new: TupleData },
    Delete { rel_id: u32, key: Option<TupleData>, old: Option<TupleData> },
    Truncate { rel_ids: Vec<u32> },
    Other,
}
pub(crate) struct RelColumn { pub flags: u8, pub name: String, pub type_oid: u32, pub type_modifier: i32 }
pub(crate) enum CellValue { Null, UnchangedToast, Text(String) }
pub(crate) struct TupleData(pub Vec<CellValue>);
```

TupleData cells: `n` null / `u` unchanged-toast / `t` text (`[i32 len][bytes]`; pgoutput defaults to text format).

### `meta` / `Lsn` (internal) — `src/replica/meta.rs`
`Lsn(pub u64)` with `from_pg_str`/`to_pg_str` (`"16/B374D848"`), Copy/Ord. `ensure_meta_table`, `load_state -> Option<ReplicaState>`, `init_state`. Watermark UPDATE is issued inside the apply transaction via `tx.exec` (never its own transaction).

```sql
CREATE TABLE IF NOT EXISTS _pglite_replica (
    id            integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    slot_name     text NOT NULL,
    publication   text NOT NULL,
    watermark_lsn text NOT NULL,
    fingerprint   text NOT NULL,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
```

Resume: on `start`, if `load_state` matches config slot/publication → skip backfill, START_REPLICATION at stored watermark. Else first-run path.

### `backfill` (internal) — `src/replica/backfill.rs`
- `introspect_published_tables(conn) -> Vec<TableDef>` — `pg_publication_tables` + columns/PK; CREATE TABLE DDL (PK kept, FK stripped)
- `bootstrap_schema(db, tables)` — apply DDL into PGlite
- `copy_table(snapshot_conn, db, table)` — second regular connection: `BEGIN; SET TRANSACTION ISOLATION LEVEL REPEATABLE READ; SET TRANSACTION SNAPSHOT '<name>'; COPY t TO STDOUT` → stream chunks → `PGlite::copy_in` in bounded batches

Window correctness: `CREATE_REPLICATION_SLOT ... EXPORT_SNAPSHOT` returns `consistent_point` + `snapshot_name`; backfill under that snapshot, stream from `consistent_point` → no gap/overlap by construction.

### `CommittedTransaction` (public) — `src/replica/mod.rs`
```rust
pub struct CommittedTransaction {
    pub xid: u32,
    pub commit_lsn: Lsn,
    pub end_lsn: Lsn,
    pub commit_ts: i64,
    pub changes: Vec<RowChange>,
}
pub enum RowChange {
    Insert { schema: String, table: String, row: Vec<(String, Option<String>)> },
    Update { schema: String, table: String, key: Vec<(String, Option<String>)>, row: Vec<(String, Option<String>)> },
    Delete { schema: String, table: String, key: Vec<(String, Option<String>)> },
    Truncate { schema: String, table: String },
}
```
Broadcast is committed-transaction granularity only (streamed phase; backfill rows are not broadcast). No separate `ReplicaEvent` wrapper (Least New Definitions — single event kind in v1).

## File Blueprint

### Create
| File | Purpose | Phase |
|------|---------|-------|
| `crates/pglite/src/replica/mod.rs` | Replica, ReplicaConfig, CommittedTransaction/RowChange, lifecycle, broadcast, applier loop, resume/drift orchestration | 1,4,5,6 |
| `crates/pglite/src/replica/wire.rs` | ReplConn: startup/SCRAM, simple query, CopyBoth framing, ReplMsg, standby feedback | 2,4,5 |
| `crates/pglite/src/replica/pgoutput.rs` | pgoutput v1 decode (pure) | 4 |
| `crates/pglite/src/replica/meta.rs` | Lsn, meta DDL, load/init state, ReplicaState | 1,3,5 |
| `crates/pglite/src/replica/backfill.rs` | introspection, schema bootstrap, snapshot COPY → copy_in | 3 |
| `crates/pglite/tests/replica.rs` | `#![cfg(feature = "replica")]`; decode fixtures + env-gated integration | 2,4,6 |

### Modify
| File | Change | Phase |
|------|--------|-------|
| `crates/pglite/src/lib.rs` | `#[cfg(feature = "replica")] mod replica;` + gated `pub use replica::{Replica, ReplicaConfig, CommittedTransaction, RowChange, Lsn};` | 1 |
| `crates/pglite/Cargo.toml` | `[features] replica = []` (no new deps) | 1 |
| `crates/pglite/src/error.rs` | variants: `ReplicaConfig(String)`, `Upstream(String)`, `ReplicaHalted(String)`, `Lsn(String)` | 1 |
| `.github/workflows/ci.yml` | Linux job: `postgres:16` service container (`wal_level=logical`), `PGLITE_REPLICA_UPSTREAM_DSN`, run feature-gated replica tests | 6 |

### Review (expected: no change)
`db.rs` (`copy_in` L230, `transaction` L185, `exec` L171, `query` L176 — all pub, sufficient), `transaction.rs` (rollback-on-drop covers halt path), `build.rs`/`pglite-sys/build.rs` (`replica` must not touch native build).

## Interface Specifications

```rust
pub struct ReplicaConfig {
    pub host: String,
    pub port: u16,                              // default 5432
    pub user: String,
    pub password: String,
    pub database: String,
    pub publication: String,                    // pre-existing upstream
    pub slot_name: String,                      // module owns slot lifecycle
    pub application_name: String,               // default "pglite-replica"
    pub read_timeout: std::time::Duration,      // default 5s
    pub status_interval: std::time::Duration,   // default 10s
}
```

## Implementation Phases

Phase 1 Scaffolding (feature gate, errors, config, Lsn, meta, stubs) → verifiable: builds + clippy clean with `--features replica`, Lsn round-trip test green.
Phase 2 Wire connection (startup, SCRAM, simple query, IDENTIFY_SYSTEM) → verifiable: env-gated integration test authenticates and returns valid xlogpos.
Phase 3 Slot + backfill (EXPORT_SNAPSHOT, introspection, bootstrap, chunked COPY, fingerprint + init_state) → verifiable: PGlite tables equal upstream snapshot; meta row holds consistent_point.
Phase 4 pgoutput decode + streaming apply + watermark (Header framing, Relation cache, txn buffer, skip-by-watermark, toast/null mapping) → verifiable: offline decode fixtures green; live changes mirrored; watermark advances.
Phase 5 Feedback, resume, drift halt (StandbyStatusUpdate, read-timeout teardown, resume path, fingerprint halt) → verifiable: stop() prompt; restart resumes with no gaps/dupes; ALTER TABLE upstream → is_halted().
Phase 6 Broadcast + CI + docs (subscribe fan-out, CommittedTransaction build, CI service container, module docs, fmt/clippy) → verifiable: one CommittedTransaction per applied txn; CI green.

(Full task list in tasks.md — 28 tasks.)

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `Message::parse` errors on CopyBoth/`w`/`k` | Certain | High | `Header::parse` framing + manual dispatch on the CopyBoth socket only |
| `block_on` re-entrancy | Med | High | Replica thread is the only `block_on` site (same as `live/mod.rs:112`); never inside async callbacks |
| `tx_lock` starves user reads during big apply | Med | Med | By design (sole writer); held only during apply, not socket waits; document |
| Backfill memory on large tables | Med | High | Stream COPY chunks → `copy_in` per chunk; never buffer a table |
| LSN representation drift | Med | Med | `Lsn` newtype centralizes parse/format; text `X/Y` persisted, AtomicU64 mirror |
| Blocking read vs `stop()` | Med | Med | `set_read_timeout` + done-flag check |
| PGlite closed mid-replication (`BOOTED` no-reopen) | Low | Med | Applier sees `Error::Closed` → stops itself; lifetime coupling documented |
| TOAST/NULL on UPDATE | Med | Med | Omit `UnchangedToast` from SET; PK from key tuple; unknown cases halt |
| Backfill/stream gap | Low | High | Exported snapshot + consistent_point; impossible by construction |
| Published table without PK | Med | Med | Halt at backfill with clear error (UPDATE/DELETE need a key) |

## Resolved Open Questions (decisions for design.md)

1. **Apply-SQL value passing**: typed binds via `tx.query(sql, &params)` with explicit per-column casts derived from `Relation` type_oid (static OID→typename map for common types); unknown OIDs halt loudly. Avoids text-escaping bugs.
2. **Broadcast scope**: streamed transactions only; backfill is not broadcast.
3. **CI**: `postgres:16`, Linux job only; macOS skips replica integration tests (env unset → skip).
4. **Fingerprint granularity**: column names + type_oid per table (drift on add/drop/retype).

## Confidence

Design completeness 92, risk accuracy 90, feasibility 90 — all PGlite contact points verified `pub`; the only novel code (CopyBoth framing, pgoutput decode) is pure and offline-testable.
