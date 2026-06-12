# Architecture Blueprint: v1-2-engine-parity

Generated: 2026-06-12 (code-architect agent)

## Design Summary

Three parity capabilities with zero FFI-waist/boot-path changes: (1) per-extension CI artifacts (`pglite-ext-<name>-<target>.tar.gz` on the engine tag) selected by per-name cargo features and merged into the runtime tar by `pglite/build.rs`; (2) `icu` feature swapping which engine-variant `libpglite.a` `pglite-sys` links (`pglite-icu-<target>.tar.gz`); (3) unflagged `live/` module reusing listen/pg_notify for full-rerun live queries.

## Design Decisions

| Decision | Chosen | Rationale |
|---|---|---|
| Ext distribution | per-ext artifact, client-side merge | no 2^N variant matrix; ABI via shared tag; dlopen path proven (plpgsql/dict_snowball) |
| ICU | engine variant artifact | configure-time flag; superset → feature-unification safe |
| Live flagging | unflagged | zero size/dep cost (project.md rule) |
| Live scope | full-rerun only | changes/incremental need state tables + diff CTEs |
| Locale surface | explicit `PGliteOptions.locale_provider` enum | per-datadir choice; ICU/libc datadirs incompatible |
| Trigger dedup | `Arc<Mutex<HashSet<(u32,u32)>>>` on PGlite | mirrors tableNotifyTriggersAdded; cross-query |
| Refresh coalescing | pending-flag + in-flight guard | runtime-agnostic, no timers |
| Ext merge point | `pglite/build.rs` (Option A) | owner of runtime tar; engine.rs untouched |

## Components

1. **`native/build-extensions.sh`** — `./native/build-extensions.sh <name>...`; submodule init for vector; PGXS `make install DESTDIR=<staging> PG_CONFIG=native/out/install/bin/pg_config`; tar relative paths + sha256. pgcrypto needs OpenSSL env on macOS (`PKG_CONFIG_PATH`).
2. **artifacts.yml jobs** — `build-extensions` (OpenSSL + submodule init) and `build-icu` (icu4c/libicu-dev, `WITH_ICU=1`) per target, uploading to the `engine-*` tag.
3. **`pglite/build.rs` merge** — `CARGO_FEATURE_PGVECTOR`/`PGCRYPTO`; same resolution chain (env → native/out → cache → download+sha256); extract base+exts → re-tar to OUT_DIR; no-feature fast path = current copy.
4. **`pglite-sys/build.rs` ICU branch** — `CARGO_FEATURE_ICU` → asset `pglite-icu-<target>.tar.gz`, isolated cache subdir (`.../icu/`), extra ICU link libs if dynamic.
5. **`src/live/`** — `tables.rs`: `WATCHED_TABLES_SQL` (pg_rewrite/pg_depend/pg_class walk) + `trigger_ddl(schema_oid, table_oid, ...)` emitting trigger fn + statement trigger with `pg_notify('table_change__<s>__<t>','')`. `mod.rs`: `LiveQuery` (Clone; fields id/pg/view_sql/callbacks/pending/in_flight/channels/dead per no-Inner rule) with `refresh`/`unsubscribe`; `impl PGlite { live_query }` — temp view in tx, discover tables, dedup-install triggers, listen per channel, initial run, return handle.
6. **`PGliteOptions.locale_provider`** — `LocaleProvider { Libc(default), Icu }`; `run_initdb` maps to `--locale-provider`; boot sets `ICU_DATA` when variant bundles data. Always-present field, runtime-validated.
7. **Tests** — per-process: `extensions.rs` (feature-gated CREATE EXTENSION + sample calls), `live.rs` (mutate→callback, unsubscribe→silence), `locale.rs` (icu-gated collation check).

## Interface Specifications

```toml
# pglite
[features]
pgcrypto = []
pgvector = []
icu = ["pglite-sys/icu"]
# pglite-sys
[features]
icu = []
```

```rust
pub type RowCallback = Box<dyn Fn(&[Row]) + Send + Sync>;
impl PGlite {
    pub async fn live_query<F>(&self, sql: &str, params: &[&(dyn ToSql + Sync)], callback: F)
        -> Result<LiveQuery, Error>
    where F: Fn(&[Row]) + Send + Sync + 'static;
}
impl LiveQuery {
    pub async fn refresh(&self) -> Result<(), Error>;
    pub async fn unsubscribe(self) -> Result<(), Error>;
}
```

Asset names on tag `engine-<sha>`: `pglite-<target>.tar.gz` (base), `pglite-icu-<target>.tar.gz`, `pglite-ext-pgcrypto-<target>.tar.gz`, `pglite-ext-pgvector-<target>.tar.gz` (+ .sha256 each).

Ext tar layout: `lib/postgresql/<name>.{so,dylib}`, `share/postgresql/extension/<name>.control`, `<name>--*.sql`.

## Phases

1. Local ext build proof (pgcrypto+pgvector; CREATE EXTENSION gate) — HIGH risk, first.
2. Feature scaffolding (Cargo.tomls).
3. build.rs merge + ext tests + CI feature leg.
4. Live queries (refresh scheduling FIRST — see risk #1).
5. ICU variant (WITH_ICU build, sys branch, LocaleProvider).
6. CI artifact jobs.
7. Pin runbook + docs.

## Risks

| Risk | L | I | Mitigation |
|---|---|---|---|
| Live refresh re-enters engine inside `process_response` notify dispatch (db.rs:384 fires callbacks synchronously) → deadlock on tx_lock | High | High | callback sets pending flag only; refresh after current roundtrip releases lock |
| pgcrypto OpenSSL linkage on CI | Med | High | local proof first; explicit deps + PKG_CONFIG_PATH |
| ext dylib symbol resolution | Low | High | proven path (`-undefined,dynamic_lookup`); verify in 1.3 |
| ICU/non-ICU datadir mix | Med | High | explicit provider; initdb failure as guard; docs |
| ext ABI drift on pin bump | Med | High | same-tag artifacts; runbook rebuilds all atomically |
| include_bytes growth per ext | Low | Low | accepted (~1-5MB/ext); revisit lazy download if it hurts |

## Open Questions

- ICU data: bundle (hermetic, bigger) vs system ICU — lean bundle; decide in 5.1.
- Refresh scheduling mechanism: post-roundtrip hook vs self-Exec via command channel — decide in 4.2 under no-tokio constraint.
- pgvector submodule sha vs PG17.5 compatibility — verify in 1.1.
- Confirm `share/postgresql/extension` lookup works under the relocated runtime root (expected: yes, sharepath-relative).

Confidence: design 88 / risks 85 / feasibility 90.
