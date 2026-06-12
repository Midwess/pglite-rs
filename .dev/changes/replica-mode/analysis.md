# Codebase Analysis: replica-mode

Generated: 2026-06-12 by code-explorer
Scope: Add opt-in `replica` cargo feature — a dedicated OS-thread logical-replication client that keeps PGlite as a read-only replica of an upstream Postgres via pgoutput, with snapshot backfill, transactional apply, LSN watermark, standby feedback, and an in-process committed-transaction broadcast seam (`Replica::subscribe()`).

---

## Project Context

- **Tech Stack**: Rust 2021 edition, no tokio — async driven by `futures::executor::block_on` + `futures::channel::oneshot`, OS threads (`std::thread::Builder`), `std::sync::mpsc` for command queues, `futures::lock::Mutex` for the transaction lock, `postgres-protocol 0.6.11`, `thiserror 2`
- **Architecture Style**: Single-crate layered library (`pglite`). Engine runs on a dedicated OS thread (`pglite-engine`); all callers communicate through an `mpsc` command channel. The library is published and standalone — it does NOT depend on `core_services` or any workspace-external async runtime.
- **Key Directories for this change**: `crates/pglite/src/` (all source), `crates/pglite/tests/` (integration tests), `crates/pglite/Cargo.toml` (feature gates)

---

## Similar Features Found

### 1. `live` module — background subscription with lifecycle teardown (closest precedent)

- **Location**: `crates/pglite/src/live/mod.rs`, `live/tables.rs`
- **Pattern**: `LiveQuery` struct owns a `PGlite` clone, an `mpsc::Sender<()>` wake channel, an `Arc<AtomicBool>` done flag, and a vec of LISTEN token pairs. Background work runs on a named OS thread (`"pglite-live-refresh"`) that blocks on `wake_rx.recv()`. Teardown is explicit async `unsubscribe()` — sets `done=true`, sends a wake, then unlistens each token and drops the DDL trigger if no other `LiveQuery` watches the same table (ref-counted in `PGlite::live_triggers`). No Drop-based async teardown. No tokio. No cancellation token.
- **Relevance**: The replica applier thread follows exactly this shape — an OS thread blocked on a blocking channel/socket read, a done flag (`Arc<AtomicBool>`) for `stop()`, explicit `Replica::stop()` (not async Drop). The broadcast seam mirrors listen/callback.

### 2. Engine thread — dedicated OS thread with `mpsc` in + `oneshot` out

- **Location**: `crates/pglite/src/engine.rs:69-101`
- **Pattern**: `Engine::spawn()` returns `(mpsc::Sender<EngineCommand>, JoinHandle<()>, oneshot::Receiver<Result<(),Error>>)`. The thread runs a blocking `while let Ok(cmd) = cmd_rx.recv()` loop. Each command carries its own `oneshot::Sender` for the reply. Async callers await the oneshot; non-async code uses `futures::executor::block_on(rx)`.
- **Relevance**: Replication client thread uses the identical pattern — named `std::thread::Builder` spawn, boot oneshot to signal readiness or first-connect error.

### 3. `multiple-process` feature gate precedent (design, not yet implemented in src)

- **Location**: `.dev/changes/multiple-process-mode/proposal.md`, `design.md`, `specs/multiple-process/spec.md`
- **Pattern**: Module lives in `src/multiple_process/` (mod.rs + pool.rs + notify.rs). Single `#[cfg(feature = "multiple-process")]` gate in `lib.rs` and `db.rs`. Tests mirror `extensions.rs` gating: `#![cfg(feature = "multiple-process")]` at the top of the test file.
- **Relevance**: `replica` follows the same convention: `src/replica/` gated in `lib.rs` and `Cargo.toml` under `[features] replica = []`. Unlike MP, replica does NOT modify `PGlite`'s transport — it calls PGlite as a user.

### 4. `pgcrypto`/`pgvector`/`icu` feature gates — build-side-only features

- **Location**: `crates/pglite/Cargo.toml:24-28`; `crates/pglite/build.rs:8-11`; `crates/pglite-sys/build.rs:17,29`
- **Pattern**: Extension features affect only the build script. The `replica` feature is different: it adds Rust source, not build artifacts, so `replica = []` with no dependencies is the right form.

---

## Architecture Layers

| Layer | Directory/File | Pattern | Examples |
|-------|---------------|---------|---------|
| Public API | `src/lib.rs` | `pub use` re-exports only | `pub use db::{PGlite, PGliteOptions}`, `pub use live::LiveQuery` |
| Database handle | `src/db.rs` | `PGlite` struct, `Clone` via `Arc` fields | exec, query, transaction, listen, copy_in/out |
| Engine thread | `src/engine.rs` | Dedicated OS thread, `mpsc` in, `oneshot` out | `Engine::spawn`, `EngineCommand::Exec/Close` |
| Transaction | `src/transaction.rs` | Borrow of `&PGlite` + `MutexGuard` for serialization | `Transaction::begin/commit/rollback` |
| Live subscription | `src/live/mod.rs` | Background refresh OS thread, `AtomicBool` done flag | `LiveQuery`, `PGlite::live_query` |
| Errors | `src/error.rs` | `thiserror::Error` enum, `pub(crate)` constructor | `Error::Database`, `Error::Boot`, `Error::Protocol` |
| Feature modules | `src/{module}/` | `#[cfg(feature)]`-gated, module folder with `mod.rs` | `live/`, planned `replica/` |

---

## Public API Contact Points (exact signatures)

The applier is the only writer to PGlite, via a `PGlite` handle passed at construction:

```rust
// crates/pglite/src/db.rs:185
pub async fn transaction(&self) -> Result<crate::transaction::Transaction<'_>, Error>

// crates/pglite/src/transaction.rs:25-35
pub async fn exec(&self, sql: &str) -> Result<(), Error>
pub async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error>
pub async fn commit(mut self) -> Result<(), Error>
pub async fn rollback(mut self) -> Result<(), Error>

// db.rs:171,176
pub async fn exec(&self, sql: &str) -> Result<(), Error>
pub async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error>

// db.rs:331,339,511 — pub(crate)
pub(crate) async fn exec_unlocked(&self, sql: &str) -> Result<(), Error>
pub(crate) async fn query_unlocked(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error>
pub(crate) fn rollback_fire_and_forget(&self)
```

**How async is driven without tokio**: the replication thread is a blocking OS thread; it drives async PGlite calls with `futures::executor::block_on(...)` — the pattern used in `live/mod.rs:112` and `engine.rs:354,368,388`.

---

## Dependencies

### Internal
- `PGlite` (db.rs): the only writer — replica takes a `PGlite` handle at construction
- `Error` (error.rs): new replica variants added to the existing enum
- `futures::channel::oneshot`: boot/readiness signaling
- `std::sync::mpsc` + `Arc<AtomicBool>`: control/teardown, matching engine/live patterns

### External (confirmed in Cargo.toml / Cargo.lock)
- **`postgres-protocol 0.6.11`** (already a dependency) provides:
  - `message::backend::Message::parse` — regular connection messages (auth, ReadyForQuery, ErrorResponse, CopyData, CopyDone, …)
  - `authentication::sasl::{ScramSha256, ChannelBinding}` — complete SCRAM-SHA-256 client
  - `message::frontend::{startup_message, sasl_initial_response, sasl_response, password_message, query, terminate, CopyData}`
- **`bytes 1`** (already a dependency): `Bytes`/`BytesMut` for CopyData payload slicing

### Gaps in `postgres-protocol 0.6.11` (critical finding)

1. **`CopyBothResponse` (tag `W`) is absent** from the `Message` enum — `Message::parse` returns `Err("unknown message tag")` for it (backend.rs:262-267). The replication client must use `Header::parse` for tag+len framing after `START_REPLICATION` and dispatch manually by tag byte.
2. **XLogData (`w`) and PrimaryKeepalive (`k`)** replication submessages (inside CopyData payloads) must be hand-parsed.
3. **StandbyStatusUpdate (`r`)** must be hand-serialized (wrapped in `frontend::CopyData`).
4. **No `replication=database` startup helper** — passed manually via `startup_message` parameters.

The crate DOES have everything needed for the auth phase (SCRAM) and the regular-query phase (IDENTIFY_SYSTEM, CREATE_REPLICATION_SLOT issued as simple queries before CopyBoth).

### Data Dependencies
- A meta table in PGlite (e.g., `_pglite_replica_meta`) for the atomic LSN watermark — written inside each apply transaction; created during setup if absent.
- The upstream publication and replication slot are external; publication must pre-exist (per scope).

---

## Execution Flow (projected)

1. **Entry**: `Replica::start(db, config)` — validates config, creates meta table if absent, spawns the thread
2. **Thread boot**: `"pglite-replica"` OS thread, `std::net::TcpStream::connect`, `startup_message` with `replication=database`, SCRAM-SHA-256 handshake via `postgres-protocol::authentication::sasl`
3. **Snapshot backfill**: `CREATE_REPLICATION_SLOT ... LOGICAL pgoutput EXPORT_SNAPSHOT` (simple query) → snapshot name + consistent_point; COPY each published table out under that snapshot (second regular connection) and into PGlite
4. **Stream start**: `START_REPLICATION SLOT ... LOGICAL <lsn> (proto_version '1', publication_names '...')` → CopyBothResponse (hand-parsed) → XLogData stream
5. **Apply loop**: decode pgoutput (Relation/Begin/Insert/Update/Delete/Truncate/Commit); buffer per transaction; on Commit apply as one PGlite transaction with watermark update inside; then broadcast to subscribers
6. **Keepalive/feedback**: on PrimaryKeepalive with reply_requested, send StandbyStatusUpdate with the durable watermark
7. **Teardown**: `Replica::stop()` sets done flag; socket read timeout makes the blocking read interruptible

---

## Conventions to Follow

| Category | Convention | Example |
|----------|------------|---------|
| File naming | `src/replica/mod.rs` + submodules | `src/live/mod.rs` |
| Struct naming | PascalCase, no `XxxInner` anti-pattern | `Replica` |
| Background thread | `std::thread::Builder::new().name("pglite-replica")` | `"pglite-engine"`, `"pglite-live-refresh"` |
| Done flag | `Arc<AtomicBool>` set in `stop()` | `live/mod.rs:81` |
| Teardown | Explicit `stop(&self)`; no async Drop | `LiveQuery::unsubscribe()` |
| Async on blocking threads | `futures::executor::block_on(...)` | `live/mod.rs:112` |
| Error enum | New variants in `error.rs` with thiserror | `Error::Boot(String)` |
| Feature gating | `#[cfg(feature = "replica")] mod replica;` in lib.rs; `replica = []` in Cargo.toml | planned MP pattern |
| Test file | `tests/replica.rs` with `#![cfg(feature = "replica")]` | `tests/extensions.rs:1` |
| Lock encapsulation | `Arc<Mutex<T>>` fields directly on struct, never exposed | `db.rs:72-76` |
| No inline comments | Per CLAUDE.md | all of `engine.rs` |

---

## Constraints

### One-Instance Constraint and `OPEN`/`BOOTED` statics
`db.rs:20,49`: process-global one-way latches gate `open_inner`. The replica module never opens a second PGlite — it receives an already-open handle and never touches these statics. If the user closes PGlite while Replica runs, applier calls return `Error::Closed` — the natural stop signal; Replica must handle it gracefully (lifetime coupling documented).

### Threading and `tx_lock` serialization
`db.rs:72`: `tx_lock: Arc<futures::lock::Mutex<()>>` serializes all write operations. The applier holds it for the duration of each upstream transaction apply (via `transaction()`), blocking concurrent writers by design (applier is the only writer).

### No tokio
Confirmed: zero tokio anywhere in this crate's dependency tree. Replica thread is a plain OS thread calling `block_on`.

### Cancellation token
CLAUDE.md mandates `core_services::utils::cancellation::CancellationToken`, but this standalone published crate has no `core_services` dependency. The crate's actual pattern is `Arc<AtomicBool>` done flag + wake channel (`live/mod.rs:18,80`) and `CloseOnDrop` (`db.rs:51`). Replica follows the crate's actual pattern.

---

## OpenSpec Notes

Existing spec domains: `engine-build`, `ffi-abi`, `host-layer` (v1); `multiple-process` (MP change); `build-pipeline`, `host-api` (v1.2). The `replica` delta spec lives at `.dev/changes/replica-mode/specs/replica/spec.md` covering: feature-gated constructor, startup/auth, slot+snapshot backfill, transactional streaming apply, standby feedback, restart-as-resume, publication drift halt, committed-transaction broadcast.

---

## Risks and Considerations

| Risk | Impact | Mitigation |
|------|--------|------------|
| `Message::parse` errors on CopyBothResponse (tag `W`) | Stream fails right after START_REPLICATION | `Header::parse` framing + manual tag dispatch on the replication socket |
| `tx_lock` held for full apply duration | Blocks concurrent PGlite access during apply | By design (applier only writer); document |
| `BOOTED` static — PGlite cannot reopen after close | Closing PGlite mid-replication makes both inoperable | Applier detects `Error::Closed`, stops itself; document lifetime coupling |
| Blocking `TcpStream::read` cannot be interrupted by `stop()` | `stop()` hangs until next keepalive | `set_read_timeout` + done-flag check on timeout |
| pgoutput type mapping gaps (custom type OIDs) | Apply errors | Text-format apply makes this rare; surface as halt error |
| Backfill/stream window race | Data gap or overlap | Exported snapshot + consistent_point alignment; gap impossible by construction |

---

## Confidence Assessment

- Pattern confidence: 95 — all major patterns directly observable with citations
- Architecture understanding: 95 — engine thread, tx_lock, statics fully traced
- Dependency/gap analysis: 98 — postgres-protocol 0.6.11 source read; CopyBothResponse absence confirmed by negative grep
