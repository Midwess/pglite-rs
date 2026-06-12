# Codebase Analysis: socket-orm-connectivity

Generated: 2026-06-13 (code-explorer)

## Project Context

- Tech Stack: Rust 2021, futures 0.3 (no tokio), postgres-protocol 0.6, postgres-types 0.2, thiserror 2, bytes 1, libc 0.2, tar 0.4, embedded engine libpglite.a
- Architecture: engine confined to one OS thread (in-process) or spawned postmaster (multi-process); wire bytes are the only cross-layer currency; PGlite handle Clone+Send+Sync via Arc + mpsc/oneshot

## Similar Features Found

1. **multiple-process module** — feature-gated module folder pattern (lib.rs:5-6, 15-16); Server owns socket dir + child, Pool owns worker threads; both impl Drop. The `socket` feature must mirror this exactly.
2. **NotifyConn** (multiple_process/notify.rs) — UnixStream try_clone halves, named background std thread, Mutex<UnixStream> writer, pending VecDeque of oneshots; reader loop is the structural template for the gateway session loop. connect_and_handshake (pool.rs:56) + read_response (pool.rs:40, frames until 'Z') reusable for tests.
3. **Replica wire.rs** — blocking read_exact frame loop, Message::parse over BytesMut; confirms postgres-protocol's frontend module encodes only client->server messages — synthesized server responses must be hand-assembled raw bytes.
4. **TempDataDir / CloseOnDrop** (db.rs:51-61, 178-183) — Arc-held RAII newtypes whose Drop does cleanup with no caller involvement; the gateway must follow.

## Key Findings

- `tx_lock: Arc<futures::lock::Mutex<()>>` (db.rs:135) — gateway batches MUST hold it; in-process backend is strictly single-session.
- `process_response` (db.rs:664) errors on ErrorResponse — unusable for gateway traffic; notification dispatch needs a separate notify-only helper feeding `listeners` (db.rs:136).
- block_on precedent for engine calls from std threads: live/mod.rs:112.
- max_connections accounting: pool_size = max_connections.max(2); postmaster gets pool_size + 2 (notify conn + reserve) at mod.rs:91.
- Server.sock_dir private (mod.rs:59), sock_path pub(crate) (mod.rs:60); Pool.credentials (username, database) at pool.rs:35.
- sun_path guard ≤96 pattern at mod.rs:76-81.
- Drop-clean audit: Server, PinnedConn, TempDataDir, CloseOnDrop all Drop-clean; **NotifyConn reader thread leaks** (blocks on read_exact, stream never shut down) — fix in scope.
- open_inner sets data_dir unconditionally (db.rs:233); BOOTED latch means gateway must hold a PGlite clone to keep engine alive.

## Risks Identified

| Risk | Severity | Note |
|---|---|---|
| tx_lock interleaving (gateway batch vs API caller) | Critical | hold per batch |
| COPY-IN frames arrive after CopyInResponse — accumulate-until-Sync insufficient | High | Z-tracking continuation: only 'c'/'f' flush while mid-COPY |
| Notification double-dispatch / missed Rust callbacks | Med | verbatim forward + notify-only side-channel |
| block_on re-entrancy | Low | doorman is its own thread; never re-entrant |
| Named prepared statements from ORMs vs anonymous Parse("") from db.query | Low | coexist safely in one session |
| sun_path >104 | High | reuse existing guard |
| accept() blocks Drop | High | self-connect wake |
| /dev/shm unwritable in containers | Med | probe + temp_dir fallback |

## Conventions to Follow

- Feature gate: `#[cfg(feature = "x")] mod x;` + gated pub use in lib.rs; `x = []` in Cargo.toml; `#![cfg(feature = "x")]` test files
- impl PGlite methods live inside the feature module, not db.rs
- Thread naming `pglite-{role}`; stop via Arc<AtomicBool> (live/mod.rs:81)
- thiserror Error variants; ? everywhere; no inline comments; locks hidden; no XxxInner structs; futures-only no tokio
