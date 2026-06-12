# Delta for Replica

## ADDED Requirements

### Requirement: Opt-in feature gate

The system SHALL expose replica functionality only when the `replica` cargo feature is enabled, with no effect on default builds and no new crate dependencies or native-build impact.

#### Scenario: Default build excludes replica

- WHEN the crate is built without `--features replica`
- THEN `Replica`, `ReplicaConfig`, `CommittedTransaction`, `RowChange`, and `Lsn` are absent from the public API and no replica code is compiled

#### Scenario: Feature build compiles cleanly

- WHEN the crate is built with `--features replica`
- THEN the build and clippy pass with no new dependencies added to the dependency tree

### Requirement: Upstream connection and authentication

The system SHALL connect to the upstream Postgres over TCP with `replication=database` in the startup message and authenticate via SCRAM-SHA-256 using the existing `postgres-protocol` crate, on a dedicated OS thread without any tokio dependency.

#### Scenario: Successful authenticated startup

- WHEN `Replica::start` is called with valid upstream credentials
- THEN the replica thread authenticates and `IDENTIFY_SYSTEM` returns a valid current LSN

#### Scenario: Authentication failure surfaces

- WHEN `Replica::start` is called with invalid credentials
- THEN `start` returns `Error::Upstream` carrying the server's error message and no thread is left running

### Requirement: Slot lifecycle and consistent snapshot backfill

The system SHALL create and own the replication slot with an exported snapshot, introspect the pre-existing publication's tables (primary keys kept, foreign keys stripped), bootstrap their schema into PGlite, and backfill each table via COPY executed under that exported snapshot before streaming begins.

#### Scenario: Backfill matches the snapshot exactly

- WHEN backfill completes for populated upstream tables
- THEN PGlite table contents equal the upstream state at the slot's consistent point, with no row missing and no row duplicated

#### Scenario: Published table without a primary key

- WHEN a published table has no usable primary key
- THEN the replica halts at backfill with a clear error before any streaming starts

### Requirement: Transactional streaming apply with atomic watermark

The system SHALL apply each upstream transaction as exactly one PGlite transaction and update the `_pglite_replica` watermark row within that same transaction. The applier SHALL be the only writer to PGlite in replica mode.

#### Scenario: Atomic apply

- WHEN an upstream transaction containing multiple row changes commits
- THEN all its changes plus the watermark update are committed atomically in PGlite, or none are

#### Scenario: TOAST-unchanged columns preserved

- WHEN an upstream UPDATE omits unchanged TOAST columns
- THEN the applied UPDATE leaves those columns' existing PGlite values untouched

### Requirement: Restart-as-resume with exactly-once apply

The system SHALL persist the watermark LSN durably with applied data, resume streaming from it after restart, and skip any transaction whose end LSN is at or below the watermark.

#### Scenario: Resume without duplication

- WHEN the replica is stopped (or crashes) and is started again with the same config
- THEN backfill is skipped, streaming resumes from the stored watermark, and previously applied transactions are not re-applied

#### Scenario: Crash between apply and acknowledgment

- WHEN the process dies after a transaction committed in PGlite but before upstream feedback was sent
- THEN on restart the re-sent transaction is skipped by the watermark check and applied exactly once overall

### Requirement: Standby status feedback

The system SHALL send StandbyStatusUpdate messages carrying only the durably committed watermark, both in reply to keepalives that request a reply and on a configurable periodic interval, so upstream can advance the slot and free WAL.

#### Scenario: Reply to keepalive

- WHEN the upstream sends a PrimaryKeepalive with reply_requested set
- THEN the replica promptly sends a StandbyStatusUpdate carrying the current durable watermark

### Requirement: Publication drift halt

The system SHALL record a fingerprint (column names and type OIDs per published table) and halt loudly — applying no further changes and reporting a halted state — when the boot-time check or an incoming Relation message conflicts with it.

#### Scenario: Schema drift detected mid-stream

- WHEN a published table's column set changes upstream
- THEN the replica stops applying, `is_halted()` returns true, and the error identifies the drifted relation

### Requirement: In-process committed-transaction broadcast

The system SHALL fan out each applied (streamed) transaction to all active `subscribe()` receivers as a `CommittedTransaction` (xid, commit/end LSN, commit timestamp, row changes), dropping receivers whose channel is closed. Backfill rows SHALL NOT be broadcast.

#### Scenario: Subscriber receives committed transaction

- WHEN an upstream transaction is applied to PGlite
- THEN every active subscriber receives exactly one `CommittedTransaction` describing its changes

#### Scenario: Dead subscriber pruned

- WHEN a subscriber's receiver has been dropped
- THEN the next fan-out removes its sender without affecting other subscribers

### Requirement: Responsive teardown

The system SHALL stop the replica thread promptly via an explicit `stop()` using a done flag and a socket read timeout, following the crate's existing teardown pattern (no async Drop). Closing the PGlite instance SHALL cause the replica to stop itself gracefully.

#### Scenario: Prompt stop

- WHEN `stop()` is called
- THEN the replica thread exits within approximately the configured read timeout

#### Scenario: PGlite closed underneath the replica

- WHEN the PGlite instance is closed while the replica is applying
- THEN the applier observes the closed error and stops itself without panicking
