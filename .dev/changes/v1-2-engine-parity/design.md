# Design: v1-2-engine-parity

## Overview

Three additions that never touch the FFI waist or boot path: extensions = more bytes composed into the runtime tar; ICU = a different engine artifact selected at link time; live queries = SQL + the existing notify surface. Full component detail in `blueprint.md`.

## Key Decisions

### Decision 1: Per-extension artifacts composed client-side (not engine-variant matrix)

**Context:** Extensions must be ABI-matched to the engine; users want small binaries.
**Options:**
1. Bake all extensions into the base runtime tar — simple, but every user pays ~10MB+ for extensions they don't use
2. Prebuilt engine variants per extension-set — 2^N artifact explosion
3. One artifact per extension on the same `engine-<sha>` tag; `pglite/build.rs` merges enabled ones into the runtime tar
**Decision:** Option 3. Additive cargo features map 1:1 to additive tar merges; CI builds each extension once; ABI safety comes from sharing the engine tag.

### Decision 2: ICU as engine variant, not extension

ICU is a configure-time flag (`--with-icu`) baked into `libpglite.a` — it cannot load dynamically. So `icu` is the one feature that switches which engine artifact `pglite-sys` links (`pglite-icu-<target>.tar.gz`). Safe under cargo feature unification because the ICU engine is a strict superset (runs libc-provider datadirs fine). The reverse is not true: an ICU-provider datadir opened by a non-ICU engine fails at initdb/boot — guarded by the explicit `PGliteOptions.locale_provider` and documented.

### Decision 3: Live queries ship unflagged

project.md convention: flags gate measurable cost only. Live queries add pure Rust and SQL strings — no artifact, no dependency, negligible code size. Folder convention still applies (`src/live/`).

### Decision 4: Live refresh is scheduled, never inline

`process_response` (db.rs:384) dispatches notification callbacks synchronously while the response is being parsed and the tx_lock is held by the in-flight call. A live-query callback that re-ran the query inline would deadlock on tx_lock or re-enter the engine channel from the wrong context. Therefore the notify callback only sets `pending: AtomicBool`; the refresh executes afterward (scheduled through the command channel / after lock release). This is the highest-risk item in the change and gets solved first in task 4.2.

### Decision 5: pg_dump descoped

WASM pg_dump runs as a second WASM module wired into the engine via `pgl_set_rw_cbs` — an in-memory trick. A native pg_dump binary is a separate OS process; libpq needs a real socket. The socket listener is exactly what `pglite-socket` will build, so pg_dump moves to that future proposal instead of forcing a half-solution here.

## API Changes

```rust
// new
PGlite::live_query(sql, params, callback) -> LiveQuery   // unflagged
LiveQuery::{refresh, unsubscribe}
PGliteOptions.locale_provider: LocaleProvider             // Libc | Icu

// features (Cargo.toml)
pgcrypto = []
pgvector = []
icu = ["pglite-sys/icu"]
```

## Security Considerations

- Extension artifacts sha256-verified like the engine artifact, same release tag.
- pgcrypto links system OpenSSL at build time only (CI hosts); runtime ships the produced dylib.
- Live-query trigger DDL interpolates only catalog-sourced OIDs (integers) into SQL — no user-string interpolation; user SQL enters only via CREATE TEMP VIEW with the user's own query text, same trust level as exec().

## Testing

- Phase-1 C-free local gate: CREATE EXTENSION proofs before any Rust
- Feature-gated `tests/extensions.rs`, `tests/locale.rs`; unflagged `tests/live.rs` (per-process binaries)
- CI feature-matrix leg exercising `--features pgcrypto,pgvector`
