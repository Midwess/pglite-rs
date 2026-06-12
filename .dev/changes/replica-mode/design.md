# Design: replica-mode

## Overview

PGlite as the read model of a CQRS split: real Postgres is the single source of truth and write model; the local PGlite is a read-only logical replica fed by one ordered stream (logical replication, pgoutput). All complexity reduces to protecting one invariant: **PGlite state is exactly upstream's state as of the watermark LSN.** Every design choice below either constructs that invariant or refuses to endanger it.

```
real PG: publication → slot(+exported snapshot) ──┬─ backfill (COPY @snapshot, chunked → copy_in)
                                                  └─ stream (pgoutput txns, CopyBoth)
                                                            │  pglite-replica OS thread
                                                            ▼
                                  per-txn buffer → one PGlite txn [rows + watermark] → standby feedback
                                                            │
                                                            ▼
                                          subscribe(): Arc<CommittedTransaction> fan-out
```

## Architecture

One new module `crates/pglite/src/replica/` (mod, wire, pgoutput, meta, backfill), one dedicated OS thread, zero new dependencies, zero changes to `db.rs`/`transaction.rs` (replica consumes the existing public API: `transaction`, `exec`, `query`, `copy_in`). Threading, teardown, and async-driving all mirror existing precedents (`engine.rs` spawn pattern; `live/mod.rs` done-flag + `block_on`).

## Key Decisions

### Decision 1: Hand-rolled replication client (no tokio)

**Context:** The crate is deliberately runtime-agnostic (futures only); every off-the-shelf replication client (tokio-postgres, pg_replicate) drags in tokio.
**Options:**
1. Hand-rolled: std TcpStream + `postgres-protocol` (already a dep, has SCRAM) — more code (~1-2k lines), zero new deps, fits the crate's hand-owned-thin-layer ethos (FFI mirrored by hand, no bindgen)
2. tokio-postgres behind the `replica` feature — battle-tested, far less code, but feature users inherit a runtime and the crate's core identity erodes
**Decision:** Option 1 (user-confirmed). The replication phase needs hand-framing anyway (see Decision 2), which removes much of tokio-postgres's advantage.

### Decision 2: Two framing regimes on one connection

**Context:** `postgres-protocol` 0.6.11's `Message::parse` covers startup/auth/simple-query completely but errors on `CopyBothResponse` (tag `W`) and knows nothing of XLogData/keepalive submessages — verified in crate source.
**Decision:** Use `Message::parse` up to and including all simple queries (IDENTIFY_SYSTEM, CREATE_REPLICATION_SLOT, introspection); switch to `Header::parse` tag+len framing with manual dispatch the moment START_REPLICATION is sent. Framing fact that must not be gotten wrong: `len` excludes the tag byte — a complete frame is `1 + len` bytes; `Header::parse` does not consume.

### Decision 3: Watermark committed inside the apply transaction

**Context:** Resume correctness requires knowing exactly which upstream transactions are already applied, across crashes.
**Decision:** `UPDATE _pglite_replica SET watermark_lsn = <end_lsn>` is executed inside the same PGlite transaction as the row changes. Consequences, in order: (a) after any crash the watermark is exactly true; (b) resume = skip transactions with `end_lsn <= watermark` — exactly-once apply from at-least-once delivery with one comparison; (c) standby feedback may only ever report this value (feedback honesty: early confirmation risks unrecoverable gaps, late confirmation only costs upstream WAL retention).

### Decision 4: Backfill under the slot's exported snapshot

**Context:** Initial copy and stream start must align with zero gap and zero overlap.
**Decision:** `CREATE_REPLICATION_SLOT ... EXPORT_SNAPSHOT` yields `consistent_point` + `snapshot_name` born at the same instant; a second regular connection runs `SET TRANSACTION SNAPSHOT` and COPYs each table (streamed in bounded chunks into `PGlite::copy_in` — never a whole table in memory); streaming then starts at `consistent_point`. Gap-free by construction — no reconciliation logic exists because no gap can exist.

### Decision 5: Plain INSERT/UPDATE/DELETE apply, untyped literals (revised at implementation)

**Context:** Apply SQL could be defensive (upserts) or mirror events 1:1; values arrive as pgoutput text cells.
**Decision:** Plain statements mirroring upstream events (skip-by-watermark already guarantees exactly-once, so upsert defensiveness would only mask bugs). Values are embedded as quote-doubled text literals; untyped string literals in assignment context coerce to the column type through the same input functions binds would use, so fidelity is identical, while costing one engine roundtrip per statement instead of two (describe + bind) and requiring no OID→typename map. The original blueprint proposed typed binds with casts; revised during implementation for the roundtrip and simplicity win — `ident()`/`lit()` quoting is covered by unit tests. `UnchangedToast` cells are omitted from the SET list; UPDATE/DELETE target the replica-identity key ('K' tuple or flagged columns of the new tuple), with REPLICA IDENTITY FULL old tuples matched via column-wise `IS NOT DISTINCT FROM`.

### Decision 6: Halt-loudly drift policy

**Context:** Logical replication does not carry DDL; schema drift would silently corrupt the cache.
**Decision:** Record a fingerprint (column names + type_oid per table) at backfill; verify at boot and against every incoming Relation message; on mismatch set done, surface `Error::ReplicaHalted`, `is_halted() == true`. The Relation message makes the stream self-describing — drift is detected before the first row that would corrupt the cache. Auto-ALTER for additive changes is explicitly deferred.

### Decision 7: std-mpsc broadcast, committed-transaction granularity

**Context:** The serving layer (future change) needs a seam; no tokio broadcast available.
**Decision:** `subscribe()` returns `std::sync::mpsc::Receiver<Arc<CommittedTransaction>>`; senders live in `Arc<Mutex<Vec<Sender>>>` (lock encapsulated, `&self` API, brief scopes); dead senders pruned on send failure. Granularity = one message per applied upstream transaction — the same atomicity boundary as upstream commit and PGlite apply, end to end. Backfill is not broadcast.

### Decision 8: Crate-local teardown pattern, not CancellationToken

**Context:** Workspace CLAUDE.md mandates `core_services::utils::cancellation::CancellationToken`, but this crate is standalone/published with no `core_services` dependency.
**Decision:** Follow the crate's established pattern (`Arc<AtomicBool>` done flag, explicit `stop()`, socket `set_read_timeout` so the blocking read observes the flag promptly; `Error::Closed` from PGlite doubles as a stop signal). Recorded here so the deviation from workspace CLAUDE.md is explicit and intentional.

## Data Model

Created at runtime in the user's PGlite (not Rust types):

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

Plus replica copies of each published table (PK kept, FK stripped, created during bootstrap).

## API Changes

All additive, feature-gated:

```rust
pub struct ReplicaConfig { host, port, user, password, database, publication, slot_name, application_name, read_timeout, status_interval }
impl Replica {
    pub async fn start(db: PGlite, config: ReplicaConfig) -> Result<Replica, Error>;
    pub fn stop(&self);
    pub fn watermark(&self) -> Lsn;
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<Arc<CommittedTransaction>>;
    pub fn is_halted(&self) -> bool;
}
pub struct Lsn(pub u64);                       // from_pg_str / to_pg_str
pub struct CommittedTransaction { xid, commit_lsn, end_lsn, commit_ts, changes: Vec<RowChange> }
pub enum RowChange { Insert {..}, Update {..}, Delete {..}, Truncate {..} }
```

New `Error` variants: `ReplicaConfig`, `Upstream`, `ReplicaHalted`, `Lsn`.

## Security Considerations

- Upstream credentials live in `ReplicaConfig` in memory; never persisted to the meta table or logs
- SCRAM-SHA-256 via `postgres-protocol`'s implementation; no custom crypto
- TLS is out of scope for v1 (plaintext TCP) — acceptable for localhost/private-network upstreams; documented limitation, `rustls` integration is a candidate follow-up
- The replica honors upstream's REPLICA IDENTITY and publication row filters — data never visible to the publication never reaches the cache
- Applier SQL uses typed binds (no string interpolation of row values); identifiers from introspection are quote_ident-escaped

## Performance Considerations

- Apply holds PGlite's `tx_lock` only for the duration of each transaction apply, not during socket waits — user reads proceed between transactions
- Backfill streams COPY chunks (bounded memory) directly into the native `copy_in` path
- `watermark()` reads an `AtomicU64` mirror — no SQL round-trip
- Single PGlite backend serializes apply and user reads by design; this is the embedded/single-node trade accepted for v1
