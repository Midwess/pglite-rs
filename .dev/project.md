# Project Context

## Overview

**pglite-rs** — in-process PostgreSQL for Rust, packaged like SQLite. Embeds ElectricSQL's `postgres-pglite` fork (the engine behind PGlite/WASM) compiled **natively** as a static library, wrapped in a safe, async-by-default, runtime-agnostic Rust API.

Status: v1 implemented (all 26 tasks complete, 2026-06-12). Workspace builds, all tests green on macOS arm64; CI + artifact pipelines pushed (Linux first run pending).

## Tech Stack

### Languages
- Rust (workspace, two crates — planned)
- C (engine: `postgres-pglite` submodule; native shim in `native/`)

### Frameworks / Key Dependencies (planned)
- `futures` / `futures-util` — async primitives, **no tokio dependency** (runtime-agnostic)
- `postgres-protocol` — Postgres wire protocol parse/serialize (tokio-free)
- `postgres-types` — `ToSql`/`FromSql` typed params and rows (tokio-free)
- `thiserror` — unified error enum

### Testing
- Cargo integration tests (`crates/pglite/tests/integration.rs`) against the real engine: CRUD, type round-trips, transactions, error mapping, reopen-data, drop-rollback

### Build Tools
- `native/build-libpglite.sh` — configure → make → `ar libpglite.a` → share-bundle tarball (runs in CI per target)
- `build.rs` in `pglite-sys` — `PGLITE_LIB_DIR` override, else prebuilt download + sha256 verify + cache
- GitHub Actions: `artifacts.yml` (per-target engine builds → releases), `ci.yml` (fmt, clippy, test)

## Key Directories

| Directory | Purpose |
|-----------|---------|
| `crates/pglite-sys/` | (planned) unsafe FFI: extern "C" decls mirroring `pglite_native.h`, build.rs linking |
| `crates/pglite/` | (planned) safe async API — the published crate |
| `native/` | (planned) C shim (`pglite_native.h/.c` — exit trampoline, typed callback setters) + engine build script |
| `postgres-pglite/` | git submodule — ElectricSQL Postgres fork; all engine changes behind `__PGLITE__` ifdefs |
| `.dev/pglite/` | reference: PGlite TS monorepo (host-layer reference implementation, `packages/pglite/src/pglite.ts`) |
| `.dev/db/` | reference repo |
| `.dev/specs/` | source-of-truth specifications |
| `.dev/changes/` | active change proposals |
| `.dev/archive/` | completed changes |

## Architecture

### Style
Three concentric layers; data crosses boundaries as Postgres wire-protocol bytes only:

| Layer | Location | Purpose |
|-------|----------|---------|
| C engine | `libpglite.a` (built from `postgres-pglite/`) | patched Postgres: unrolled main loop (`PostgresMainLoopOnce`), socket calls → host callbacks, longjmp/exit containment |
| FFI waist | `pglite-sys` ↔ `native/pglite_native.h` | ~25 functions + 6 registered callbacks; the single authoritative ABI contract |
| Async host | `crates/pglite` | engine confined to one dedicated OS thread; `std::sync::mpsc` in, `futures::channel::oneshot` out; protocol pump mirrors `pglite.ts` `execProtocol` |

### Key Patterns
- **Engine-thread confinement**: engine is `!Send` by nature (Postgres globals) — one OS thread owns all C calls; public handle is `Clone + Send + Sync` via message passing
- **longjmp containment**: `sigsetjmp` trampoline lives entirely in C (`pgl_native_pump`) — longjmp never unwinds Rust frames (UB otherwise)
- **Callback registration over symbol export**: C→Rust via `pgl_set_*` function-pointer setters; state in `thread_local!` (engine thread is sole caller)
- **Reference-implementation parity**: every host-layer behavior question answered by `.dev/pglite/packages/pglite/src/pglite.ts`
- **No bindgen**: ABI hand-written in `pglite-sys`, pinned to `pglite_native.h`

### Hard Constraints
- **One open PGlite instance per process** (Postgres global state) — enforced, returns `Error::AlreadyOpen`
- Native configure drops WASM workarounds (`--disable-spinlocks`, `--disable-largefile`, `-m32`) and keeps lite-profile flags (`--without-openssl --without-pam --without-readline --without-llvm`)
- v1 trims ICU/libxml/uuid (C-locale collation only); parity restored later
- v1 out of scope: extensions (pgvector…), live queries, Windows, multi-instance

## Conventions

### Naming / Code Style
- See `CLAUDE.md` (authoritative): Least New Definitions > Struct-First, strict placement, no inline comments, locks fully encapsulated, `&self`-only public APIs, no `XxxInner` structs
- `unsafe` confined to `pglite-sys` + `engine.rs`

### Error handling
- Single `thiserror` enum `pglite::Error`: `Database{sqlstate, message, detail, hint}`, `AlreadyOpen`, `Closed`, `Io`, `Protocol`; `?` everywhere

### Git
- `main` branch; submodule pin = engine version = CI artifact tag

## Build Commands

| Command | Purpose |
|---------|---------|
| `cargo build` | Build workspace (needs `PGLITE_LIB_DIR` or network for prebuilt download) |
| `cargo test -p pglite` | Integration tests against real engine |
| `cargo fmt --all && cargo clippy --all-targets` | Lint |
| `native/build-libpglite.sh` | Local engine build (needs clang, bison, flex, perl, make) |

## Notes

Settled design decisions (2026-06-12 session):
1. **Engine**: native static link of `postgres-pglite` submodule (not WASM-in-wasmtime). Upstream comment in `build-pglite.sh` confirms native `libpglite` is an intended target.
2. **API**: async by default, runtime-agnostic (`futures` only, no tokio). `PGlite::open/open_temp/query/exec/transaction/close`.
3. **Distribution**: prebuilt `libpglite.a` + share bundle per target from GitHub releases; `build.rs` downloads/verifies/caches; `PGLITE_LIB_DIR` override. Share bundle (postgres.bki, timezones) embedded via `include_bytes!`, extracted on first open.
4. **FFI**: hand-written extern block (no bindgen); `pglite_native.h` is the single ABI contract.
5. Primary risk: native `__PGLITE__` build untested upstream — first milestone proves `libpglite.a` + initdb + SELECT 1.

Design record lives in `.dev/changes/implement-pglite-v1/{design.md,blueprint.md}` (supersedes the earlier plan for a docs/superpowers spec).

## Latest Analysis

Last updated: 2026-06-12 — change `implement-pglite-v1`

### Architecture Summary
C engine (`postgres-pglite` submodule) compiled as `libpglite.a` with ~25 libc-override functions in `pglitec.c`; Rust host drives it via registered read/write callbacks and the C trampoline `pgl_native_pump`, which owns `sigsetjmp` and returns exit codes 99 (alive) / 100 (longjmp) as plain ints.

### Key Patterns Discovered
- `-D__PGLITE__` + all socket/libc overrides injected via CFLAGS in `build-pglite.sh:33-45` (NOT configure.ac); `pglitec.o` compiled first WITHOUT those defines so the shim calls real libc
- Boot: `pgl_backend_main` returns 99 (`PGLITE_EXIT_ALIVE`) → `pgl_startPGlite()` → `ProcessStartupPacket` + `pgl_sendConnData` → first ReadyForQuery
- Pump: `while (read_offset < len || pq_buffer_remaining_data() > 0) PostgresMainLoopOnce();` code 100 → `PostgresMainLongJmp()` + continue; then `PostgresSendReadyForQueryIfNecessary()` + `pgl_pq_flush()`
- Path relocation: Postgres derives share dirs from `my_exec_path` (`path.c:903`) — native shim adds `pgl_set_exec_path`/`pgl_set_runtime_root`
- initdb natively: compile with `-Dmain=pgl_initdb_main` into the same archive; its popen of `postgres --single` re-enters `pgl_backend_main` via registered callbacks
- Vestigial ABI entries excluded from header: `pgl_set_pipe_fn`, `pgl_proc_exit`, `pgl_sigsetjmp`
- `.dev/db/` = TanStack DB, unrelated reference — ignore
