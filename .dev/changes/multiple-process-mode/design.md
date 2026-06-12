# Design: multiple-process-mode

## Overview

Same library, second residence for the engine: in-process (default, unchanged) or a child postmaster behind a pool — selected per `open` call, available only when the `multiple-process` feature compiles the option in.

```
            PGlite (one public API)
                 │ Backend enum
   ┌─────────────┴──────────────┐
   InProcess (default)       MultiProcess (feature)
   engine thread + FFI       child postmaster, private unix socket
   one session, serialized   pool of N backends, parallel sessions
```

## Key Decisions

### 1. Engine patch 0004 first — the showstopper
All backend code compiles with `-Dsend=pgl_send -Drecv=pgl_recv`; the overrides call host-callback fn pointers that are NULL in any spawned process. Single-user children never noticed (stdin path). A postmaster backend's first socket read would SIGSEGV. Fix where the real syscalls are still reachable: pglitec.c (compiled without the renames) gains NULL-guard fallbacks. Without this patch the entire feature is dead on arrival — hence Phase 1, gated by a spawn smoke proof.

### 2. Patch 0005 — make one binary correct in both modes
Two compile-time `__PGLITE__` patches assume single-user mode: skipping WAL-triggered checkpoint requests and forcing inline checkpoints. Under a real postmaster they cause WAL bloat / bypass the checkpointer. Gating them at runtime on `is_pglite_active` (1 only in our in-process boot) preserves in-process behavior bit-for-bit while restoring stock behavior in spawned backends.

### 3. Backend enum, not trait
`roundtrip()` is already the single transport choke point; an enum field keeps one `PGlite`, static dispatch, zero cost for in-process, and honors Least New Definitions. Lock semantics split at the same seam: the global tx_lock serializes only the in-process backend; the pool replaces it for MP plain queries; `Transaction` pins one pooled connection (BEGIN→COMMIT on one backend, by construction).

### 4. Session state gets explicit homes
Pool routing breaks anything session-scoped. Two casualties found and re-homed: live-query views become non-temporary (unified across modes + startup sweep of crash orphans), and LISTEN state lives entirely on one dedicated notify connection whose blocking reader streams notifications into the existing listeners map (try_clone'd socket halves; LISTEN/UNLISTEN acks matched via pending-oneshot in the reader).

### 5. Lifecycle = our only real job
Postgres owns everything inside; we own: short 0700 socket dir (sun_path limits), connect-retry readiness (no pg_ctl shipped), SIGINT fast-shutdown on close, SIGTERM to the process group on Drop, socket-dir removal. Orphan-on-SIGKILL documented (datadir lock prevents double-open; no PDEATHSIG on macOS).

## API Changes

`MultiProcessOptions` + `PGlite::open_multi_process` (feature-gated). Existing API surface unchanged; live views non-temp in both modes (behavioral note: crash leaves a view until next open's sweep).

## Security Considerations

No networking: `listen_addresses=''` disables TCP entirely; the unix socket lives in a 0700 directory. Trust auth is local-only by construction. Child processes inherit nothing sensitive beyond the datadir they serve.

## Testing

Phase-1 spawn smoke (committed, gated); parity suite; concurrency proofs (parallel writes, advisory locks, pg_stat_activity); live + notify under pool; teardown/orphan checks.
