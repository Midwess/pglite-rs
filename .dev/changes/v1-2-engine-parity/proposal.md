# Proposal: v1.2 Engine Parity

**Status**: approved

## Summary

Close the engine-level gaps with WASM PGlite: per-extension prebuilt artifacts gated by cargo features (pgvector, pgcrypto first), an `icu` engine-variant feature for real Unicode collation, an unflagged live-queries module on the v1.1 notify surface, and a documented engine-pin bump runbook.

## Motivation

v1.1 reached API parity, but the engine remains plain-SQL: no extensions (pgvector is the headline ask for embedded AI workloads), byte-order-only text sorting, no reactive queries. All of it rides infrastructure that already exists — the artifact pipeline, the runtime tar, and listen/notify.

## Scope

### In Scope

- `native/build-extensions.sh`: PGXS builds against `native/out/install`, emitting `pglite-ext-<name>-<target>.tar.gz` (pgcrypto from contrib + system OpenSSL; pgvector from the fork's uninitialized `vector` submodule)
- Cargo features: `pgcrypto`, `pgvector` (ext-tar merge in `pglite/build.rs`), `icu` (forwards to `pglite-sys`, selects `pglite-icu-<target>.tar.gz` engine variant)
- `PGliteOptions.locale_provider` (`Libc` default | `Icu`)
- `crates/pglite/src/live/` (unflagged — zero size cost): `PGlite::live_query` full-rerun variant, `LiveQuery` handle, trigger dedup set
- CI: `build-extensions` + `build-icu` jobs on the `engine-*` tag; feature-matrix test leg
- Engine-pin bump runbook (process doc, no code)

### Out of Scope

- **pg_dump** — analysis showed the WASM callback trick doesn't transfer: native pg_dump is a separate process needing a real socket. Moves to a later proposal together with `pglite-socket` (shared prerequisite).
- Socket bridge, multi-instance/dlopen (v2), live `changes`/`incrementalQuery` variants, windowed live queries, PostGIS/AGE/other extensions (follow the pgvector recipe later), Windows.

## Affected Areas

| Area | Impact |
|------|--------|
| `native/build-extensions.sh` | new — extension build + packaging |
| `native/build-libpglite.sh` | `WITH_ICU=1` branch |
| `crates/pglite/build.rs` | ext-tar download + merge into runtime tar |
| `crates/pglite-sys/build.rs` | ICU variant asset selection |
| `crates/pglite/src/live/` | new feature folder (mod.rs, tables.rs) |
| `crates/pglite/src/db.rs` | `LocaleProvider`, `live_triggers` set |
| `crates/pglite/src/engine.rs` | initdb locale-provider arg, `ICU_DATA` |
| Both `Cargo.toml`s | `[features]` sections |
| `.github/workflows/*` | extension/ICU artifact jobs, feature test leg |

## Dependencies

- v1 artifact pipeline + v1.1 listen/notify (shipped)
- Build hosts: OpenSSL headers (pgcrypto), icu4c/libicu-dev (ICU variant)
- pgvector submodule init (`pglite/other_extensions/vector`, sha 35ab919b)

## Risks

| Risk | Mitigation |
|------|------------|
| Live refresh re-enters engine synchronously from notify dispatch (deadlock) — `process_response` fires callbacks inline (`db.rs:384`) | Callback only sets a pending flag; refresh scheduled after the current roundtrip releases the lock |
| pgcrypto OpenSSL linkage on CI | Phase-1 local proof; explicit dep install + PKG_CONFIG_PATH |
| ICU vs non-ICU datadir incompatibility | Explicit `locale_provider`; documented; initdb failure is the runtime guard |
| Ext ABI drift vs engine pin | Ext artifacts live on the same `engine-<sha>` tag; runbook rebuilds all artifacts atomically |

## References

- `analysis.md`, `blueprint.md` (agent outputs), `design.md` (key decisions), `CONTEXT.md` (extension bundle, engine variant, live query terms)
