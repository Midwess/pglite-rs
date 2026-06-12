# Design: implement-pglite-v1

## Overview

Three concentric layers; data crosses each boundary only as Postgres wire-protocol bytes:

```
┌────────────────────────────────────────────┐
│ crates/pglite — safe async API             │
│   PGlite / Row / Transaction / Error       │
│   futures channels, any executor           │
├────────────────────────────────────────────┤
│ FFI waist — pglite-sys ↔ pglite_native.h   │
│   ~25 fns + 5 registered callbacks         │
├────────────────────────────────────────────┤
│ libpglite.a — postgres-pglite native build │
│   unrolled loop, callback IO, contained    │
│   exits (trampoline)                       │
└────────────────────────────────────────────┘
```

Full component/interface detail in `blueprint.md`. This file records the decisions with real alternatives.

## Key Decisions

### Decision 1: Native static link, not WASM-in-wasmtime

**Context:** The engine ships as an Emscripten WASM module for JS. A Rust crate could embed that artifact in a WASM runtime or compile the same sources natively.
**Options:**
1. wasmtime + WASM blob — multi-instance, sandboxed; but Emscripten ABI requires reimplementing the JS syscall/FS layer in Rust, ~2-3x slower, 4GB cap
2. Native static link — full native speed, real filesystem, zero runtime deps; but one instance per process and the native build path is unproven upstream
**Decision:** Native static link. Use case is embedded app database + general-purpose lib — performance and SQLite-like ergonomics dominate. Upstream comment in `build-pglite.sh` ("allow native builds (like libpglite)") confirms the fork intends this. One-instance constraint accepted and enforced (`Error::AlreadyOpen`); dlopen-per-instance is the future escape hatch. **Hard to reverse** — the host layer is written against the C ABI, which both options share, limiting the blast radius if revisited.

### Decision 2: Async-by-default, runtime-agnostic (no tokio)

**Context:** Engine is blocking single-threaded C; the API must be async without binding users to a runtime.
**Options:**
1. tokio-native API (spawn_blocking etc.) — idiomatic for the majority, but forces tokio on all users
2. Sync rusqlite-style API — simplest, but user mandate is async-first
3. Dedicated engine OS thread + `futures::channel` message passing — runtime-agnostic, `Send` futures on any executor
**Decision:** Option 3 (user requirement). `std::sync::mpsc` into the engine thread (blocking recv needs no executor); `futures::channel::oneshot` out (sync send, async recv).

### Decision 3: Exit/longjmp containment via C trampoline

**Context:** Postgres signals errors by `siglongjmp` and "exits" via overridden `exit(99/100)`. Emscripten converts these to JS exceptions; natively they would either kill the process or unwind Rust frames (UB).
**Options:**
1. Rust-side `setjmp` via libc crate — setjmp/longjmp across Rust frames is UB
2. Signal-based interception — fragile, platform-specific
3. C trampoline `pgl_native_pump`: owns `sigsetjmp`, wraps the whole pump loop, returns plain int codes
**Decision:** Option 3. Rust never has a frame between jump and target. Codes: 99 = alive (boot), 100 = longjmp (host calls `PostgresMainLongJmp`, continues), other = boot failure.

### Decision 4: Path relocation over hardcoded `/pglite`

**Context:** WASM build hardcodes `/pglite/*` and `/home/postgres` inside a virtual FS; natively those are root-owned real paths.
**Options:**
1. Require `/pglite` on the real FS — needs root, hostile to a library
2. Patch every hardcoded path in the engine — large submodule diff to maintain
3. Lean on Postgres's own prefix relocation (`make_relative_path` from `my_exec_path`, `path.c:903`) + two new shim setters (`pgl_set_exec_path`, `pgl_set_runtime_root`) + recreate the few fixed placeholder files under a per-instance runtime root
**Decision:** Option 3 — minimal patch surface, no privileges. PGDATA always a user path via `-D`.

### Decision 5: initdb linked in via `-Dmain=` rename

**Context:** JS ships initdb as a second WASM module; natively two `main()` symbols cannot coexist in one archive.
**Options:**
1. Ship separate initdb binary, spawn as child — breaks single-process model and the popen→backend re-entry contract
2. Reimplement initdb in Rust — enormous, fragile against engine versions
3. Compile `initdb.c` with `-Dmain=pgl_initdb_main` (and backend `main.c` as `pgl_backend_main`) into the archive
**Decision:** Option 3 — same `-D` override discipline the fork already uses for libc functions; host drives the identical orchestration as `initdb.ts`.

### Decision 6: Prebuilt artifacts, not build-from-source

**Context:** Compiling Postgres needs bison/flex/perl/clang and minutes; source exceeds crates.io 10MB cap so vendoring is impossible anyway.
**Decision:** CI builds per-target `libpglite.a` + share bundle, publishes to GitHub releases keyed by submodule pin; `build.rs` downloads + sha256-verifies + caches; `PGLITE_LIB_DIR` overrides for dev/offline. (Settled in design session; alternatives in `.dev/project.md`.)

## Data Model

No Rust-visible Postgres structs — `Port` is opaque; rows are parsed from wire `RowDescription`/`DataRow` messages by `postgres-protocol`, typed by `postgres-types`.

## API Changes

New public API (crate `pglite`): `PGlite::{open, open_temp, query, exec, transaction, close}`, `Row::{get, try_get, columns}`, `Transaction::{query, exec, commit, rollback}`, `Error`. All async `&self`, all executors supported.

## Security Considerations

- No network surface at all — engine IO is in-process byte buffers; TLS/auth modules compiled out (`--without-openssl --without-pam`, trust auth) which is correct for a single-process embedded DB.
- `build.rs` downloads are pinned by sha256 checked into the crate; `PGLITE_LIB_DIR` bypasses network entirely.
- Datadir permissions: initdb runs `--allow-group-access`; documented that the datadir inherits user's umask.

## Testing

- Phase-1 C smoke gate (no Rust): initdb + boot + SELECT 1
- pglite-sys linkage test (raw FFI round-trip)
- Integration suite: CRUD, type round-trips, transactions (incl. drop-rollback), error mapping (sqlstate/detail/hint), reopen persistence, AlreadyOpen, open_temp cleanup
- CI on macOS arm64 + Linux x86_64 with prebuilt artifacts
