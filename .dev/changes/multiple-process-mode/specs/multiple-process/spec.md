# Delta for Multiple-Process

## ADDED Requirements

### Requirement: Feature-gated multi-process constructor

When built with the `multiple-process` feature, the system SHALL expose `PGlite::open_multi_process(dir, MultiProcessOptions)` spawning the bundled `bin/postgres` as a child postmaster on a private 0700 unix-socket directory with `listen_addresses=''`. Without the feature, the API and module SHALL NOT exist or compile.

#### Scenario: Feature off

- WHEN the crate is built without `multiple-process`
- THEN `open_multi_process` and `MultiProcessOptions` are absent from the API and none of the module's code is compiled

#### Scenario: No networking

- WHEN a multi-process instance is running
- THEN the postmaster has no TCP listeners and its unix socket is reachable only through the private directory

### Requirement: Spawned backends survive socket IO

Backends forked by the spawned postmaster SHALL perform socket IO correctly: the engine's `pgl_recv`/`pgl_send` overrides SHALL fall back to the real `recv`/`send` syscalls when host callbacks are unset.

#### Scenario: First query over the socket

- WHEN a client connects to the spawned postmaster and runs `SELECT 1`
- THEN the backend serves RowDescription/DataRow/ReadyForQuery without crashing

### Requirement: Checkpointing under postmaster

WAL-triggered checkpoint requests and checkpointer delegation SHALL behave stock in spawned backends (runtime-gated on `is_pglite_active`), while in-process behavior is unchanged.

#### Scenario: WAL segment fill under load

- WHEN sustained writes fill WAL segments in multi-process mode
- THEN checkpoints occur and WAL is recycled rather than accumulating without bound

### Requirement: True concurrency with pooled connections

The system SHALL pool `max_connections` (default 4, minimum 2) backend connections; independent statements SHALL execute in parallel across backends sharing one lock table and MVCC state.

#### Scenario: Parallel writers

- WHEN two threads sharing one instance run interleaved transactions
- THEN both commit correctly and `pg_stat_activity` reports multiple backends

#### Scenario: Cross-session lock visibility

- WHEN one session takes `pg_advisory_lock(k)`
- THEN a second session blocks on the same key until release

### Requirement: Transaction connection pinning

A `Transaction` SHALL pin exactly one pooled connection for its lifetime; BEGIN, statements, and COMMIT/ROLLBACK (including rollback-on-drop) SHALL execute on that backend, and the connection SHALL return to the pool afterward.

#### Scenario: Pinned lifecycle

- WHEN a transaction runs statements and commits under multi-process mode
- THEN all of them execute on one backend and the pool regains the connection

### Requirement: Dedicated notify connection

All LISTEN state SHALL live on one dedicated connection whose background reader dispatches NotificationResponse to registered listeners.

#### Scenario: Notify across connections

- WHEN NOTIFY is issued from any pooled connection on a subscribed channel
- THEN the listener callback fires exactly once per notification

### Requirement: One-boot exemption and coexistence

Multi-process instances SHALL NOT consult or set the in-process OPEN/BOOTED guards; multiple multi-process instances (distinct datadirs) MAY coexist, including alongside one in-process instance.

#### Scenario: Two instances

- WHEN two `open_multi_process` calls target different datadirs in one process
- THEN both succeed without `AlreadyOpen`/`ReopenUnsupported`

### Requirement: Clean teardown

`close()` SHALL fast-shutdown the postmaster and wait; dropping the handle SHALL terminate the process group and remove the socket directory, leaving no orphaned backend.

#### Scenario: Drop reaps the tree

- WHEN a multi-process instance is dropped without close
- THEN postmaster and backends terminate and the socket directory is removed

## MODIFIED Requirements

### Requirement: Live full-rerun queries (host-api)

`live_query` SHALL use non-temporary views (dropped on unsubscribe) so view visibility is independent of which pooled connection serves a statement; every `open` SHALL sweep orphaned `live_query_%_view` relations. Trigger discovery, refcounted trigger teardown, refresh scheduling, and callback semantics are unchanged. Applies uniformly to in-process and multi-process modes.

#### Scenario: Live under pool routing

- WHEN a live query's refresh SELECT is served by a different pooled connection than created the view
- THEN the refresh succeeds and the callback receives fresh rows
