# Tasks: v1-2-engine-parity

## Progress: [16/16]

**Process rule**: every task ends with `git commit` + `git push origin main`. Short messages, no co-author.

## 1. Native extension build proof (local, before any Rust)

- [x] 1.1 `native/build-extensions.sh`: init `vector` submodule; PGXS-build pgcrypto (system OpenSSL) → `pglite-ext-pgcrypto-<target>.tar.gz`; verify tar layout
- [x] 1.2 PGXS-build pgvector → `pglite-ext-pgvector-<target>.tar.gz`
- [x] 1.3 Merge ext tars onto a local runtime tar; prove `CREATE EXTENSION pgcrypto` + `digest()` and `CREATE EXTENSION vector` + `'[1,2,3]'::vector` via scratch run. GATE for Rust work

## 2. Feature scaffolding

- [x] 2.1 `[features]` in both Cargo.tomls: `pgcrypto`, `pgvector`, `icu = ["pglite-sys/icu"]`; confirm `CARGO_FEATURE_*` reaches build scripts

## 3. build.rs merge + extension tests

- [x] 3.1 `pglite/build.rs`: per-feature ext-tar resolution (env → native/out → cache → download+sha256), extract base+exts, re-tar to OUT_DIR; keep no-feature fast path
- [x] 3.2 `tests/extensions.rs` feature-gated smoke tests; CI feature leg in ci.yml

## 4. Live queries (unflagged)

- [x] 4.1 `live/tables.rs`: watched-tables catalog SQL + `trigger_ddl`; `live_triggers: Arc<Mutex<HashSet<(u32,u32)>>>` on PGlite
- [x] 4.2 `live/mod.rs`: `LiveQuery` + `PGlite::live_query`; refresh scheduling via pending-flag (NEVER roundtrip inside notify dispatch)
- [x] 4.3 `mod live` + re-exports in lib.rs; `tests/live.rs` (mutate → callback with fresh rows; unsubscribe → silence)

## 5. ICU variant + locale provider

- [x] 5.1 `build-libpglite.sh` `WITH_ICU=1` branch (`--with-icu`, pkg-config, bundle ICU data) → `pglite-icu-<target>.tar.gz` locally
- [x] 5.2 `pglite-sys/build.rs` ICU branch: asset name, isolated cache subdir, ICU link libs
- [x] 5.3 `LocaleProvider` enum + `PGliteOptions.locale_provider`; `run_initdb` maps provider; `ICU_DATA` in boot; `tests/locale.rs` gated by `icu`

## 6. CI artifact jobs

- [x] 6.1 `artifacts.yml` `build-extensions` job (OpenSSL, submodule init) uploading `pglite-ext-*` to `engine-*` tag
- [x] 6.2 `artifacts.yml` `build-icu` job (icu4c/libicu-dev) uploading `pglite-icu-*`

## 7. Runbook + docs

- [x] 7.1 Engine-pin bump runbook in project.md (fetch fork → review → bump submodule → bump ENGINE_TAG consts → retag → all artifacts rebuild)
- [x] 7.2 README: features table, ICU/non-ICU datadir caveat, extension usage example

---

## Notes

- Phase 5 notes: ICU statically folded into libpglite.a (icu4c@78 .a archives; 55MB vs 15MB base) — no runtime ICU dep, no ICU_DATA needed. Shim split into pglite_native.c (exe-safe: trampoline, exit, setitimer) + pglite_reset.c (archive-only: setup/reset/pump referencing backend globals) — fresh-build zic/frontend links were broken by backend refs in LDFLAGS_EX (latent bug, would have hit CI). ICU build needs pkg-config (pkgconf brew formula).
- Phase 4 notes (post-fix): live_query(sql, params, callback) — params rendered to SQL literals server-side via Postgres format() with Parse/Describe-derived types (port of reference formatQuery). listen() returns a token; unlisten_token() removes one subscription and UNLISTENs when the channel empties. Notify triggers are refcounted per (schema_oid, table_oid); last unsubscribe drops trigger + function. Refresh runs on a dedicated per-query thread woken by notify dispatch (never inline).
- Phase 1 findings: (a) rustc dead-strips extension-facing engine API on macOS → fixed via `static:+whole-archive=pglite` in pglite-sys + `-Wl,-export_dynamic` link-arg at package level (user apps need the same one-liner — document in 7.2); (b) obsolete initdb_bundle.o removed from archive (subprocess initdb made it dead weight, broke whole-archive); (c) pgcrypto OpenSSL via LDFLAGS_SL (CLI SHLIB_LINK clobbers BE_DLLLIBS — don't).

- pg_dump descoped: native pg_dump = separate process requiring a real socket; ships later with pglite-socket.
- Highest-risk item: live-query refresh re-entrancy (blueprint Risks #1). Resolve in 4.2 before wiring callbacks.
