# Design: socket-orm-connectivity

## Overview

ORMs integrate at the wire-protocol layer, not the API layer. Multi-process mode publishes its existing postmaster socket; in-process mode gains a doorman thread that impersonates the server side of the v3 protocol and shuttles frames into the engine through the existing roundtrip seam.

## Key Decisions

### Decision 1: Single gateway entry, no path parameter
`serve_unix_socket()` picks its own RAM-backed dir (shared helper with MP placement). One method, no path-validation branches; address read back via `socket_path()`/`uri()`.

### Decision 2: Per-batch tx_lock, not per-session
The doorman holds `tx_lock` only for one frontend-batch -> engine-response exchange. Rust API calls interleave between batches. Consequence (documented, intentional): the engine has ONE session, so a Rust `db.exec` issued mid-ORM-transaction executes inside that transaction.

### Decision 3: Forward verbatim, side-channel notifications
`process_response` is unusable in the pump (it errors on ErrorResponse — normal ORM traffic). New notify-only `dispatch_notifications` scans responses and feeds the listeners map; bytes to the client are untouched.

### Decision 4: COPY-IN continuation by Z-tracking
Boundary detection is stateful: if the last engine response did not end with ReadyForQuery, the session is mid-COPY-IN and only CopyDone/CopyFail flush the batch. COPY OUT needs nothing (full response arrives in one roundtrip).

### Decision 5: Drop-clean as the lifecycle contract
Drop alone reclaims everything: stop flag -> self-connect to unblock accept() -> join (2s bound) -> remove sock dir. `shutdown(self)` only adds error visibility. Same contract retrofitted to NotifyConn (reader-thread leak fixed via Shutdown::Both at Pool teardown). No tokio: all cleanup is synchronous std.

### Decision 6: Synthesized handshake values
AuthenticationOk + ParameterStatus(server_version from live SHOW query, client_encoding=UTF8, standard_conforming_strings=on, integer_datetimes=on, DateStyle=ISO) + BackendKeyData(pid=1, key=0) + ReadyForQuery('I'). Backend-direction bytes hand-assembled (postgres-protocol only encodes frontend direction).

## Security Considerations

Socket dirs are 0700; trust auth applies only within the owning user. The gateway accepts any local same-user client — identical exposure to the MP postmaster socket. No TCP anywhere.
