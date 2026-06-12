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

### Feature modules (v1.1+ convention)
- Every feature-flagged capability gets its own cohesive module folder under `crates/pglite/src/` (e.g., `live/`, `extensions/`, `pg_dump/`): all of the feature's structs, SQL generation, and bundle logic live inside it. `lib.rs` holds the single `#[cfg(feature = "...")] mod` gate; core modules stay flag-free except thin delegating methods. Single-file domains stay single `.rs` files until they grow. Socket bridge = separate crate `pglite-socket`.
- Flags gate measurable cost (binary size, engine variants, bundled binaries): per-extension flags, `icu`, `pg-dump`. Pure-API features ship unflagged — with one decided exception: `serve` (served mode) is feature-flagged for complexity quarantine. Without `serve`: `open_served`/`ServeOptions`/`max_connections` do not exist in the API, and none of the pool/lifecycle code compiles (`#[cfg]` gate in lib.rs, module folder `src/serve/`). With `serve`: mode selection stays a runtime constructor choice (`open` vs `open_served`).

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

## Engine Pin Bump Runbook

1. `git -C postgres-pglite fetch origin && git -C postgres-pglite log --oneline HEAD..origin/main` — review fork changes
2. `git -C postgres-pglite checkout <new-sha>` ; rebuild + full test: `./native/build-libpglite.sh && ./native/build-extensions.sh pgcrypto pgvector && WITH_ICU=1 ./native/build-libpglite.sh && cargo test --workspace && cargo test -p pglite --features pgcrypto,pgvector,icu`
3. Re-apply check: `native/patches/*.patch` must still apply (build script reports); refresh patches if drifted
4. Update `ENGINE_TAG` const in BOTH `crates/pglite-sys/build.rs` and `crates/pglite/build.rs` to `engine-<new-12char-sha>`
5. Commit submodule pin + consts; `git tag engine-<sha> && git push origin engine-<sha>` — CI rebuilds base + icu + all extension artifacts atomically on that tag
6. Caveats: ICU and libc datadirs are mutually incompatible (locale_provider is per-datadir); extension artifacts are only valid against their tag's engine

## Latest Analysis

Last updated: 2026-06-12 — change `v1-2-engine-parity` (previous: implement-pglite-v1)

### Architecture Summary
C engine (`postgres-pglite` submodule) compiled as `libpglite.a` with ~25 libc-override functions in `pglitec.c`; Rust host drives it via registered read/write callbacks and the C trampoline `pgl_native_pump`, which owns `sigsetjmp` and returns exit codes 99 (alive) / 100 (longjmp) as plain ints.

### v1.2 Analysis Additions
- Extensions: PGXS `make install DESTDIR` against `native/out/install` → tar matching runtime layout; pgcrypto needs system OpenSSL (contrib/pgcrypto/Makefile:64); pgvector = uninitialized submodule `pglite/other_extensions/vector`
- Live queries reference: temp view + pg_rewrite/pg_depend table walk + statement triggers firing `pg_notify('table_change__<schemaOid>__<tableOid>')` + full re-run (live/index.ts)
- CRITICAL: `process_response` dispatches notify callbacks synchronously (db.rs:384) — live refresh must be scheduled, never inline
- pg_dump natively requires a real socket (WASM uses cross-module rw-callback trick) — descoped to future pglite-socket proposal

### Key Patterns Discovered
- `-D__PGLITE__` + all socket/libc overrides injected via CFLAGS in `build-pglite.sh:33-45` (NOT configure.ac); `pglitec.o` compiled first WITHOUT those defines so the shim calls real libc
- Boot: `pgl_backend_main` returns 99 (`PGLITE_EXIT_ALIVE`) → `pgl_startPGlite()` → `ProcessStartupPacket` + `pgl_sendConnData` → first ReadyForQuery
- Pump: `while (read_offset < len || pq_buffer_remaining_data() > 0) PostgresMainLoopOnce();` code 100 → `PostgresMainLongJmp()` + continue; then `PostgresSendReadyForQueryIfNecessary()` + `pgl_pq_flush()`
- Path relocation: Postgres derives share dirs from `my_exec_path` (`path.c:903`) — native shim adds `pgl_set_exec_path`/`pgl_set_runtime_root`
- initdb natively: compile with `-Dmain=pgl_initdb_main` into the same archive; its popen of `postgres --single` re-enters `pgl_backend_main` via registered callbacks
- Vestigial ABI entries excluded from header: `pgl_set_pipe_fn`, `pgl_proc_exit`, `pgl_sigsetjmp`
- `.dev/db/` = TanStack DB, unrelated reference — ignore
