# Delta for multiple-process

## ADDED Requirements

### Requirement: External ORM connection URI

When built with `multiple-process`, the system SHALL expose `PGlite::connection_uri()` and `PGlite::socket_path()` returning `Some` only for a multi-process instance; `connection_uri` SHALL be of the form `postgresql://{user}@localhost/{db}?host={socket_dir}`.

#### Scenario: URI usable by a raw client

- WHEN an external client connects using the host directory from `connection_uri()`
- THEN it completes a v3 startup and runs `SELECT 1` against the shared postmaster

#### Scenario: Accessors are None in-process

- WHEN the instance was opened with `PGlite::open`
- THEN `connection_uri()` and `socket_path()` return `None`

## MODIFIED Requirements

### Requirement: True concurrency with pooled connections

The system SHALL pool `max_connections` (default 4, minimum 2) backend connections AND SHALL provision `extra_connections` (default 4) additional postmaster slots for external ORM clients; the postmaster `max_connections` SHALL be `pool_size + 2 + extra_connections`.

#### Scenario: Headroom honored

- WHEN opened with `extra_connections = 1`
- THEN `SHOW max_connections` reports at least pool_size + 3

### Requirement: Private socket directory placement

The private 0700 socket directory SHALL be created under a RAM-backed location (`/dev/shm` when present and writable on Linux, else the OS temp dir) keeping `sun_path` within the platform limit.

#### Scenario: Fallback when /dev/shm unavailable

- WHEN `/dev/shm` is absent or unwritable
- THEN the socket directory is created under the OS temp dir and the instance works unchanged
