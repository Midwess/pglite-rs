# Codebase Analysis: v1-2-engine-parity

Generated: 2026-06-12 (code-explorer agent)

## 1. Extension build recipes

- pgvector: TWO submodule entries in `postgres-pglite/.gitmodules` (`pglite/other_extensions/vector` and `/pgvector`, both → pgvector/pgvector.git, sha 35ab919b, both UNINITIALIZED). Makefile SUBDIRS uses `vector` (`other_extensions/Makefile:6`).
- WASM pattern (`other_extensions/Makefile:26-35`): stage via `make -C <ext> install DESTDIR=<staging>` (PGXS), tar relative paths → `lib/postgresql/<ext>.so`, `share/postgresql/extension/<ext>.control`, `<ext>--*.sql`. WASM-only flags (`-sSIDE_MODULE`, build-pgcrypto.sh:4) irrelevant natively.
- Native: standard PGXS against `native/out/install` (`PG_CONFIG=.../bin/pg_config`); output paths exactly match existing runtime-tar layout (`build-libpglite.sh:99`).
- pgcrypto (contrib): `contrib/pgcrypto/Makefile:64` `SHLIB_LINK += $(filter -lcrypto -lz, $(LIBS))` → needs system OpenSSL at build (apt libssl-dev / brew openssl + PKG_CONFIG_PATH).

## 2. Contrib vs external inventory

- `contrib/` (native-buildable directly): pgcrypto, hstore, ltree, citext, pg_trgm, btree_gin/gist, unaccent, fuzzystrmatch, pg_stat_statements, dblink, +50 more.
- `other_extensions/` (submodules, uninitialized): vector, pg_ivm, pgtap, pg_uuidv7, pg_hashids, pg_textsearch, age, postgis (postgis = special build, heavy deps).

## 3. ICU

- WASM: ICU 76.1 built from source (Dockerfile:182-212), minimal collation-only data at `pglite/static/minimal-icu/76.1/icudt76l/coll/`; full data = separate npm package loaded via `icuDataDir`.
- Native `--with-icu`: configure uses `pkg-config icu-uc icu-i18n` (brew icu4c / libicu-dev); links shared system ICU. `ICU_DATA` env (TS sets `/pglite/icu`, initdb.ts:119) → native equivalent set in `engine.rs` boot if data bundled.
- initdb with ICU: `--locale-provider=icu [--icu-locale=...]`; current native hardcodes `--locale-provider=libc --locale=C` (engine.rs run_initdb).

## 4. pg_dump — descope driver

- Fork's `__PGLITE__` guards in pg_dump.c:72 / pg_backup_archiver.c:49 only redirect encoding fns to `_private` bridges for the WASM SIDE_MODULE split — irrelevant natively.
- pg_dump links libpq (`src/bin/pg_dump/Makefile:25` `$(libpq_pgport)`); connects via real socket. WASM drives it as a second WASM instance whose rw callbacks bridge into `pg.execProtocolRawSync` (`pglite-tools/src/pg_dump.ts:71`) — an in-memory trick with NO cross-process native equivalent.
- Native path would need a temporary unix-socket server → overlaps pglite-socket work → DESCOPED to that future proposal.

## 5. Live queries (reference `.dev/pglite/packages/pglite/src/live/index.ts`)

- `live.query`: CREATE OR REPLACE TEMP VIEW `live_query_<id>_view` AS <sql>; discover base tables via recursive pg_rewrite/pg_depend/pg_class walk (index.ts:719-790); per table install plpgsql trigger fn + `AFTER INSERT OR UPDATE OR DELETE ... FOR EACH STATEMENT` → `pg_notify('table_change__<schemaOid>__<tableOid>','')` (index.ts:797-832); LISTEN channels; on notify FULL RE-RUN + callback. Dedup via `tableNotifyTriggersAdded` set. Channel names use OIDs (rename-safe).
- `live.changes` (alternating state tables + diff CTE) and `incrementalQuery` (client-side ordered map) — skip for v1; `query` variant is self-contained on the shipped listen/notify surface.

## 6. Engine pin

- Ours: `06c837c6a303` ("Tdrz/be new libicu (#75)") = submodule HEAD locally.
- npm pin not locally determinable (`.dev/pglite` monorepo sha ≠ fork sha; fork commit not present locally). Pin bump = runbook with upstream fetch, not a code task.

## 7. Runtime tar composition point

- `pglite-sys/build.rs`: PGLITE_LIB_DIR → native/out → cache → download (`ENGINE_TAG` const); links static pglite + z.
- `pglite/build.rs`: same resolution for `pglite-runtime.tar` → copy to OUT_DIR; `lib.rs` include_bytes!; `engine.rs::extract_runtime` unpacks once (stamped dir).
- Merge point chosen: Option A — `pglite/build.rs` extracts base + enabled ext tars to temp, re-tars to OUT_DIR. engine.rs unchanged.

## Conventions

Feature folders under `crates/pglite/src/` with cfg gates only in lib.rs (project.md); Least New Definitions (live state hangs off PGlite + one LiveQuery handle struct); locks encapsulated (`listeners` map pattern db.rs); thiserror + `?`.

## Risks (carried into blueprint)

OpenSSL on CI; submodule init; ext-ABI = engine tag; ICU/non-ICU datadir incompat; **notify dispatch is synchronous inside `process_response` (db.rs:384) — live refresh must be scheduled, never inline**; include_bytes growth per ext (~1-5MB each, accepted).
