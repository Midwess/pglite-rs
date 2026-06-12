# Delta for Replica

## ADDED Requirements

### Requirement: Automatic reconnection with capped backoff

The system SHALL, after the first successful stream establishment, treat connection-level upstream failures as transient: it SHALL reconnect in place with exponential backoff starting at 25ms and capped at 10 seconds, reset the backoff after each successful stream establishment, resume streaming from the durable watermark, and continue indefinitely until stopped. Fatal conditions (schema drift, configuration mismatch, protocol violations, invalidated slot) SHALL still halt the replica.

#### Scenario: Transient connection loss is survived

- WHEN the upstream walsender is terminated while the replica is streaming
- THEN the replica reconnects, resumes from the durable watermark without duplicate application, and `is_halted()` remains false

#### Scenario: Fatal errors still halt

- WHEN a published table's schema drifts upstream during a reconnect cycle
- THEN the replica halts with the drift error rather than retrying

#### Scenario: Stop interrupts backoff promptly

- WHEN `stop()` is called while the replica is sleeping between reconnect attempts
- THEN the thread exits within approximately one backoff slice, not the full backoff delay

### Requirement: Slot-in-use retry on stream start

The system SHALL retry `START_REPLICATION` a bounded number of times with short delays when the slot is reported in use by another process, covering the window where a previous walsender is still terminating.

#### Scenario: Restart while old walsender lingers

- WHEN the replica restarts immediately after an unclean disconnect and upstream still holds the slot active
- THEN streaming starts successfully once the old walsender exits, without surfacing an error to the application

### Requirement: Ack position for data-free spans

The system SHALL maintain an in-memory ack position alongside the durable watermark. Empty upstream transactions and idle keepalives SHALL advance and acknowledge the ack position without writing to PGlite; the durable watermark SHALL move only when a transaction containing changes is applied. Standby feedback SHALL always report the maximum of the two. The ack position SHALL never exceed the watermark across a span containing published-table changes, and SHALL reset to the watermark on reconnect.

#### Scenario: Empty transactions cost no engine writes

- WHEN upstream commits transactions touching only unpublished tables
- THEN the replica acknowledges their LSNs upstream while `_pglite_replica` receives no writes

#### Scenario: Crash after acking an empty span

- WHEN the replica restarts after acknowledging LSNs beyond the durable watermark for data-free transactions
- THEN resume from the watermark loses no published data

### Requirement: Feedback cadence derived from upstream timeout

The system SHALL read `wal_sender_timeout` from upstream settings at stream start and send standby status at least every three quarters of that interval (bounded above by the configured status interval), preventing upstream-initiated disconnects on servers with short timeouts.

#### Scenario: Short upstream timeout

- WHEN upstream is configured with `wal_sender_timeout` lower than the replica's configured status interval
- THEN the replica sends status frequently enough that upstream never times the connection out during idle periods

### Requirement: Guarded slot creation

The system SHALL set a bounded `lock_timeout` on the replication session before creating the replication slot, so slot creation fails with a clear error instead of hanging indefinitely behind long-running upstream transactions.

#### Scenario: Slot creation blocked by a long transaction

- WHEN `CREATE_REPLICATION_SLOT` would block beyond the lock timeout
- THEN `Replica::start` fails with an upstream error rather than hanging

### Requirement: Invalidated-slot halt

The system SHALL detect an invalidated replication slot (upstream WAL retention exceeded) and halt with an error directing the operator to decommission and restart for a full resync.

#### Scenario: Slot lost to WAL retention

- WHEN upstream invalidates the slot because `max_slot_wal_keep_size` was exceeded
- THEN the replica halts, `is_halted()` is true, and the halt reason names the slot and the resync remedy

### Requirement: Decommission

The system SHALL provide `Replica::decommission(db, config)` which terminates any active walsender on the slot, drops the replication slot (idempotently), and clears the local replica meta state, so that no orphaned slot retains upstream WAL and the next start performs a fresh backfill.

#### Scenario: Clean teardown

- WHEN `decommission` is called after a replica is stopped
- THEN the slot no longer exists upstream, the meta row is gone locally, and a subsequent `Replica::start` runs a full backfill

#### Scenario: Idempotent decommission

- WHEN `decommission` is called and the slot or meta row is already absent
- THEN it succeeds without error

## MODIFIED Requirements

### Requirement: Standby status feedback

The system SHALL send StandbyStatusUpdate messages carrying the maximum of the durable watermark and the in-memory ack position, in reply to keepalives that request a reply, on a cadence of at least every three quarters of upstream's `wal_sender_timeout` (bounded above by the configured status interval), and after reconnections, so upstream can advance the slot and free WAL even across long data-free periods.

#### Scenario: Reply to keepalive

- WHEN the upstream sends a PrimaryKeepalive with reply_requested set
- THEN the replica promptly sends a StandbyStatusUpdate carrying the maximum of watermark and ack position

#### Scenario: Idle stream still advances the slot

- WHEN no published-table changes occur for an extended period while unpublished churn continues upstream
- THEN upstream's confirmed position keeps advancing via ack-position feedback and WAL is not retained for the idle replica

## REMOVED Requirements

(none)
