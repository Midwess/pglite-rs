# Delta for Host Layer

## ADDED Requirements

### Requirement: Thread-confined engine

All C calls into the engine SHALL occur on one dedicated OS thread. The public `PGlite` handle SHALL be `Clone + Send + Sync` and communicate with the engine thread via `std::sync::mpsc` (commands in) and `futures::channel::oneshot` (results out). The crate SHALL NOT depend on tokio; all futures SHALL work on any executor.

#### Scenario: Query from any executor

- WHEN `query` is awaited from a tokio, smol, async-std, or `block_on` context
- THEN the command is serialized to the engine thread and typed rows are returned without blocking the caller's executor

#### Scenario: Engine thread death

- WHEN the engine thread terminates unexpectedly
- THEN pending and subsequent calls resolve to `Error::Closed` rather than hanging

### Requirement: Boot parity with the reference implementation

`PGlite::open` SHALL follow the reference boot sequence: extract share bundle (first run) → initdb phase (only when the datadir is uninitialized) → backend phase (`pgl_backend_main` returning 99) → `pgl_startPGlite` → startup packet → `ProcessStartupPacket` == 0 → `pgl_sendConnData` → first ReadyForQuery.

#### Scenario: Fresh datadir

- WHEN `open` is called with an empty or missing data directory
- THEN initdb runs once and the database boots to ReadyForQuery

#### Scenario: Existing datadir

- WHEN `open` is called on a previously initialized data directory
- THEN initdb is skipped and previously committed data is queryable

### Requirement: One instance per process

At most one `PGlite` SHALL be open per process. A second `open`/`open_temp` while one is live SHALL return `Error::AlreadyOpen`; after `close`, a new `open` SHALL succeed.

#### Scenario: Concurrent second open

- WHEN a `PGlite` is open AND `open` is called again
- THEN it returns `Error::AlreadyOpen` without disturbing the live instance

### Requirement: Typed queries and unified errors

`query` SHALL use the extended protocol with `postgres-types` parameter binding and row decoding; `exec` SHALL use the simple protocol supporting multi-statement SQL. Backend `ErrorResponse` messages SHALL map to `Error::Database { sqlstate, message, detail, hint }`.

#### Scenario: Parameterized round-trip

- WHEN `query("SELECT $1::int + 1", &[&41i32])` is awaited
- THEN `row.get::<i32>(0)` returns 42

#### Scenario: SQL error surfaces structured fields

- WHEN a statement violates a constraint
- THEN the returned `Error::Database` carries the SQLSTATE code and message from the wire ErrorResponse, and the connection remains usable

### Requirement: Transaction rollback-on-drop

`transaction()` SHALL hold exclusive query access for its lifetime; `commit`/`rollback` consume it; dropping uncommitted SHALL issue ROLLBACK.

#### Scenario: Drop without commit

- WHEN a `Transaction` is dropped without `commit`
- THEN subsequent queries observe none of its changes
