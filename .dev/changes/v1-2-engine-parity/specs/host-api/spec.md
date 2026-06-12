# Delta for Host API

## ADDED Requirements

### Requirement: Live full-rerun queries

`PGlite` SHALL provide `live_query(sql, params, callback)` that installs statement-level notify triggers on every base table of the query (discovered via system catalogs through a temp view), listens on per-table channels, and re-runs the query invoking the callback with fresh rows when any base table is mutated. Trigger installation SHALL be deduplicated process-wide per (schema oid, table oid). Refresh SHALL never execute inside notification dispatch.

#### Scenario: Callback fires after mutation

- WHEN a live query watches table `t` AND a row is inserted into `t` AND pending notifications are delivered
- THEN the callback receives the updated result rows exactly once per coalesced change burst

#### Scenario: Unsubscribe stops callbacks

- WHEN `unsubscribe` is called on a `LiveQuery`
- THEN subsequent mutations to its base tables no longer invoke the callback

#### Scenario: Shared base table

- WHEN two live queries watch the same table
- THEN the notify trigger for that table is installed at most once

### Requirement: Configurable locale provider

`PGliteOptions` SHALL expose `locale_provider` (`Libc` default, `Icu`), mapped to initdb `--locale-provider`. The default behavior SHALL remain byte-identical to v1.1 (`libc`, `--locale=C`).

#### Scenario: ICU provider on ICU engine

- WHEN `locale_provider = Icu` is used with the `icu` feature enabled
- THEN initdb creates an ICU-collated cluster and Unicode-aware ordering is observable

#### Scenario: ICU provider on base engine

- WHEN `locale_provider = Icu` is used against the non-ICU engine
- THEN open fails with a database/boot error rather than silently degrading
