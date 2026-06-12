# Delta for socket

## ADDED Requirements

### Requirement: Feature-gated in-process socket gateway

When built with `socket`, the system SHALL expose `PGlite::serve_unix_socket() -> SocketGateway` for an in-process instance, listening on a RAM-backed 0700 directory with socket file `.s.PGSQL.5432`; on a multi-process instance it SHALL return `Error::Protocol`. Without the feature the gateway API SHALL NOT exist and its code SHALL NOT compile.

#### Scenario: ORM driver handshakes and queries

- WHEN a driver connects using the gateway's `socket_path()` directory as host
- THEN the gateway answers a synthesized AuthenticationOk, ParameterStatus set, BackendKeyData and ReadyForQuery, then serves `SELECT 1`

#### Scenario: SSLRequest answered

- WHEN a client opens with an SSLRequest
- THEN the gateway replies 'N' and proceeds with the plain startup

#### Scenario: Multi-process rejects the gateway

- WHEN `serve_unix_socket` is called on a multi-process instance
- THEN it returns `Error::Protocol` directing the caller to `connection_uri()`

### Requirement: Frame pump and session semantics

The gateway SHALL serve one client at a time, forwarding accumulated frontend frames to the engine at simple-query, function-call and extended-protocol Sync boundaries, with mid-COPY-IN continuation bounded by CopyDone/CopyFail; it SHALL hold the engine lock per batch (not per session), forward responses verbatim, and side-channel NotificationResponse frames to registered Rust listeners. Terminate SHALL end the session and the gateway SHALL accept the next client.

#### Scenario: Extended protocol round-trip

- WHEN a client sends Parse/Bind/Describe/Execute/Sync
- THEN the gateway returns the full extended-protocol response

#### Scenario: COPY IN completes

- WHEN a client issues COPY FROM STDIN then CopyData and CopyDone frames
- THEN the rows are stored and the response ends with ReadyForQuery

#### Scenario: Second client after disconnect

- WHEN the first client sends Terminate and disconnects
- THEN a subsequent client connects and queries successfully

### Requirement: Drop-clean gateway lifecycle

Dropping a `SocketGateway` SHALL reclaim every resource by Drop alone: stop the doorman, unblock the accept loop, join the thread within a bounded time, and remove the socket directory. `shutdown(self)` SHALL be optional and surface IO errors only.

#### Scenario: Resources reclaimed on drop

- WHEN a `SocketGateway` is dropped after serving clients
- THEN the socket directory no longer exists and the doorman thread has joined
