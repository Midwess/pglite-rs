# Proposal: Implement pglite-rs v1

**Status**: approved

## Summary

Implement pglite-rs v1: the `postgres-pglite` engine compiled natively into `libpglite.a`, wrapped in a safe, async-by-default, runtime-agnostic Rust API (`futures` only, no tokio) — in-process Postgres like SQLite. Six phases; every completed task is committed and pushed to `main`.

## Motivation

Rust has no true in-process Postgres. Apps wanting embedded full-Postgres SQL must run a server (Docker, pg-embed subprocess) or settle for SQLite. The `postgres-pglite` fork already solved the hard engine problems (unrolled main loop, callback IO, contained exits) for WASM; this change ports the host layer to Rust against a native build of the same engine.

## Scope

### In Scope

- Cargo workspace: `crates/pglite-sys` (hand-written FFI) + `crates/pglite` (safe async API)
- Native engine build: `native/build-libpglite.sh`, `native/pglite_native.h/.c` (exit trampoline, typed setters, path-relocation setters)
- Pure-C smoke gate (`native/smoke.c`): initdb + boot + SELECT 1 before any Rust
- Host layer: dedicated engine thread, boot sequence (initdb, backend, handshake), query pump
- Public API: `PGlite::open/open_temp/query/exec/transaction/close`, typed params/rows via `postgres-types`
- Share-bundle embed (`include_bytes!`) + extract on first open
- Prebuilt-artifact CI (`artifacts.yml`) + `build.rs` download/sha256/cache with `PGLITE_LIB_DIR` override
- Integration test suite; `ci.yml` fmt/clippy/test
- Process: each task ends with commit + push to `main` (identity `tiendang <tiendvlp@gmail.com>`, already configured)

### Out of Scope

- Extensions (pgvector, PostGIS, …)
- Live queries, dump/restore
- Windows targets
- Multi-instance per process
- ICU/libxml/libxslt/uuid parity (v1 = C locale, zlib only)
- Sync API, wasmtime backend

## Affected Areas

| Area | Impact |
|------|--------|
| `Cargo.toml`, `crates/**` | new — entire Rust workspace |
| `native/**` | new — ABI header, shim, build scripts, smoke test |
| `.github/workflows/**` | new — artifacts + CI pipelines |
| `postgres-pglite/` submodule | read-only source; possible minimal patches under `native/patches/` if native build requires |
| `.gitignore` | build-output exclusions |
| `README.md` | usage docs (task 6.4) |

## Dependencies

- `postgres-pglite` submodule checked out (done)
- Local toolchain for Phase 1: clang, bison, flex, perl, make, zlib headers
- GitHub repo (`Midwess/pglite-rs`) for releases + Actions

## Risks

| Risk | Mitigation |
|------|------------|
| Native `__PGLITE__` build untested upstream (primary) | Phase 1 is a pure-C de-risking gate; no Rust until `SELECT 1` passes |
| `pgl_exit` real-exit kills process instead of returning 99 | Trampoline interception verified in task 1.5; open question tracked in blueprint |
| Residual hardcoded `/pglite/*` paths | `pgl_set_runtime_root` + placeholder layout; grep audit in 1.4 |
| longjmp through Rust frames (UB) | Pump loop lives entirely in C `pgl_native_pump` |
| Cross-target prebuilt artifacts | Pinned triples, sha256 verify, `PGLITE_LIB_DIR` escape hatch |

## References

- `analysis.md` — codebase findings (file:line evidence)
- `blueprint.md` — component designs, interface drafts, phases
- `CONTEXT.md` — domain vocabulary
- `.dev/project.md` — settled design decisions
