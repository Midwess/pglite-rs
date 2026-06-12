# Delta for FFI ABI

## ADDED Requirements

### Requirement: Single authoritative ABI contract

`native/pglite_native.h` SHALL be the single authoritative declaration of every symbol crossing the FFI waist. `crates/pglite-sys/src/lib.rs` SHALL hand-mirror it exactly (no bindgen), and SHALL NOT declare the vestigial symbols `pgl_set_pipe_fn`, `pgl_proc_exit`, `pgl_sigsetjmp`.

#### Scenario: Header and extern block stay in lockstep

- WHEN `pglite_native.h` changes
- THEN `pglite-sys/src/lib.rs` is updated by hand in the same task, and the linkage test (boot + SELECT 1 via raw FFI) passes

#### Scenario: Vestigial symbol referenced

- WHEN any code declares or calls a vestigial symbol
- THEN the build fails at link time rather than silently misbehaving

### Requirement: Exit trampoline contract

`pgl_native_pump` SHALL own the `sigsetjmp` landing site and the pump loop, returning plain integer status codes to Rust: 99 (`PGLITE_EXIT_ALIVE`), 100 (`POSTGRES_MAIN_LONGJMP`), other values = fatal. A `longjmp` or overridden `exit` SHALL never unwind a Rust stack frame.

#### Scenario: Backend error during query

- WHEN a query triggers the backend error path (`siglongjmp` → `pgl_longjmp` → exit 100)
- THEN `pgl_native_pump` returns 100, the host calls `PostgresMainLongJmp()`, pumping continues, and the ErrorResponse bytes reach the write callback

#### Scenario: Keep-alive exit during boot

- WHEN `pgl_backend_main` reaches its keep-alive exit
- THEN the call returns 99 to the host and the process remains alive with engine state intact

### Requirement: Callback registration surface

C→Rust crossings SHALL use the registered function-pointer setters (`pgl_set_rw_cbs`, `pgl_set_system_fn`, `pgl_set_popen_fn`, `pgl_set_pclose_fn`) with FFI-safe signatures. Callback state SHALL live in `thread_local!` storage on the engine thread; pointers received in callbacks SHALL be copied before return.

#### Scenario: Engine sends result bytes

- WHEN the engine calls the overridden `send`
- THEN the registered write callback appends the bytes to the engine thread's IO buffer and returns the full length

#### Scenario: Callback panic safety

- WHEN a registered Rust callback would panic
- THEN the panic does not unwind into C (abort per `extern "C"` semantics, or `catch_unwind` for nontrivial callbacks)
