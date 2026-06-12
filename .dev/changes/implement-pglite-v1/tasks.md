# Tasks: implement-pglite-v1

## Progress: [22/26]

**Process rule**: every task ends with `git commit` + `git push origin main` (identity `tiendang <tiendvlp@gmail.com>`, already configured locally). Commit messages: `phase-N: <what>`.

## 1. Native engine proof (pure C — de-risking gate, NO Rust)

- [x] 1.1 `native/build-libpglite.sh` step 1: compile `pglitec.c` clean (no `-D` overrides) + native `./configure` lite profile (`--without-openssl --without-pam --without-readline --without-llvm --without-icu --with-zlib`, relocatable `--prefix`); commit+push
- [x] 1.2 Add `make`/`make install` with full `-D` override list from `build-pglite.sh:33-45` (drop `-m32`, all `-s*`, `SUPPORT_LONGJMP`, `--disable-spinlocks`, `--disable-largefile`); backend objects build; commit+push
- [x] 1.3 Compile `src/backend/main/main.c` with `-Dmain=pgl_backend_main` and `src/bin/initdb/initdb.c`+`findtimezone.c` with `-Dmain=pgl_initdb_main`; `ar rcs libpglite.a` over object set; tar `share/postgresql` → `pglite-share.tar`; commit+push
- [x] 1.4 Write `native/pglite_native.h` (full ABI, exclude vestigial `pgl_set_pipe_fn`/`pgl_proc_exit`/`pgl_sigsetjmp`) + `native/pglite_native.c` (`pgl_native_pump` sigsetjmp trampoline, `pgl_set_runtime_root`, `pgl_set_exec_path`); grep engine for residual `/pglite` literals; compile into archive; commit+push
  - Note: path setters dropped — zero `/pglite` literals exist in engine C (TS-only constants); argv[0] handles exec-path relocation natively. Added `pgl_native_call` generic trampoline; `pglitec.o` compiled with `-Dexit=pgl_native_exit`. `pq_buffer_remaining_data` returns ssize_t.
- [x] 1.5 `native/smoke.c`: runtime-root layout (pgstdin/pgstdout/.pgpass/bin placeholders), env setup, static-buffer callbacks, initdb phase → backend phase (verify return 99 — resolves exit-interception open question) → handshake to first ReadyForQuery; commit+push
- [x] 1.6 Extend `smoke.c`: push `SELECT 1` simple-query through pump; assert RowDescription+DataRow("1")+ReadyForQuery; `native/build-smoke.sh`; **GATE: must pass before Phase 2**; commit+push

## 2. FFI waist (pglite-sys)

- [x] 2.1 Root `Cargo.toml` workspace + `crates/pglite-sys/Cargo.toml` (`links="pglite"`, dep `libc`) + `.gitignore` additions + `rust-toolchain.toml`; commit+push
- [x] 2.2 `build.rs`: `PGLITE_LIB_DIR` branch (download branch stubbed); link directives `static=pglite` + `z`; commit+push
- [x] 2.3 `src/lib.rs`: extern block mirroring `pglite_native.h` 1:1 + callback typedefs + opaque `Port`; confirm `pq_buffer_remaining_data` return type against source; commit+push
- [x] 2.4 Linkage test: raw-FFI boot + `SELECT 1` (port of smoke.c) as `#[test]`; `PGLITE_LIB_DIR=... cargo test -p pglite-sys` green; commit+push

## 3. Engine host layer

- [x] 3.1 `crates/pglite/Cargo.toml` (futures, postgres-protocol, postgres-types, thiserror, libc) + `lib.rs` skeleton + `error.rs` thiserror enum; commit+push
- [x] 3.2 `engine.rs`: `thread_local!` `EngineIo` buffers + five `extern "C"` callbacks (read/write/system/popen/pclose); commit+push
- [x] 3.3 `Engine::boot`: share-bundle extract, runtime root + exec path, initdb phase (popen re-entry), backend phase (==99), handshake; commit+push
- [x] 3.4 `Engine::exec_protocol`: `pgl_native_pump` loop (100 → `PostgresMainLongJmp` + continue; drain `pq_buffer_remaining_data`); commit+push
- [x] 3.5 Engine thread spawn + `std::sync::mpsc` command loop + `futures::channel::oneshot` replies; `include_bytes!` share bundle; off-thread round-trip test; commit+push

## 4. Public API

- [x] 4.1 `db.rs`: `PGlite::open/open_temp/close`; global `AtomicBool` → `Error::AlreadyOpen`; commit+push
- [x] 4.2 `exec` (simple protocol, multi-statement) + `query` (extended protocol, params via `postgres-protocol` frontend + `postgres-types` ToSql); commit+push
- [x] 4.3 `row.rs`: RowDescription/DataRow parse, `get`/`try_get` via FromSql; ErrorResponse → `Error::Database{sqlstate,message,detail,hint}`; commit+push
- [x] 4.4 `transaction.rs`: BEGIN/commit/rollback + Drop-rollback; `lib.rs` re-exports; commit+push

## 5. Integration tests

- [x] 5.1 CRUD + type round-trips (int/text/bool/float/bytea/timestamp, C locale); commit+push
- [x] 5.2 Transactions (commit, rollback, drop-rollback) + error mapping assertions; commit+push
- [x] 5.3 Reopen-data persistence; `AlreadyOpen` enforcement; `open_temp` cleanup; commit+push

## 6. Distribution & CI

- [ ] 6.1 `.github/workflows/artifacts.yml`: target matrix (x86_64-linux-gnu, aarch64-apple-darwin, x86_64-apple-darwin) → `build-libpglite.sh` → release upload (`libpglite.a`+header+share tar+sha256, tag = submodule pin); commit+push
- [ ] 6.2 `build.rs` download branch: release URL by target, sha256 verify, cache; commit+push
- [ ] 6.3 `.github/workflows/ci.yml`: fmt/clippy/`cargo test -p pglite` consuming artifact; commit+push
- [ ] 6.4 README usage + `PGLITE_LIB_DIR` docs; finalize submodule pin = release tag; commit+push

---

## Notes

- Phase 5 note: in-process engine RE-BOOT (open after close in same process) hits unbounded Postgres static-state resets (shmem, fd cache, xlog fds, pgstat dsm — 4 fixed, more behind). v1 CONSTRAINT ADDED: one engine boot per process lifetime; second open returns Error::ReopenUnsupported. Reopen across processes fully works (verified by child-process test in tests/persistence.rs). Partial reset groundwork kept: patches 0002 (pgl_shmem_reset) + 0003 (pgl_fd_reset/pgl_xlog_fd_reset) + pgl_native_reset in shim. Tests split per-process: crud/transactions/persistence/tempclean.
- Phase 4 note: two native memory-safety bugs found and fixed during stress runs: (1) Postgres ITIMER_REAL/SIGALRM handler ran on arbitrary threads corrupting the heap — engine code now compiled with `-Dsetitimer=pgl_native_setitimer` no-op (single-connection needs no preemptive timers, matches WASM behavior); (2) Postgres retains argv pointers for process lifetime — boot CStrings + argv array now stored in Engine for engine lifetime. Runtime extraction made concurrent-safe (staging dir + atomic rename, stamp in dir name).
- Phase 1 COMPLETE (2026-06-12). Deviations from blueprint, all verified by passing smoke run:
  - initdb runs as a real subprocess (native has processes; avoids WASM heap-snapshot emulation and backend main re-entry risk). `pgl_initdb_main` still in archive for future use.
  - Path setters (`pgl_set_runtime_root`/`pgl_set_exec_path`) dropped: zero `/pglite` literals in engine C; argv[0] handles relocation.
  - `pglitec.o` compiled with `-Dexit=pgl_native_exit`; trampoline stack in `pglite_native.c` returns 99/100 as ints. `pgl_native_call` added for entry mains; `pgl_native_setup` provides non-blocking postmaster death pipe (kqueue rejects fd -1 natively).
  - Fork patch required: `ProcessStartupPacket` export guard was `__EMSCRIPTEN__`-only (`native/patches/0001-*.patch`, upstreamable). Build script applies idempotently; submodule working tree stays locally patched.
  - `io_method=sync` GUC absent in this fork pin; dropped from start params.

- Phase 1 completion criteria: `./native/build-libpglite.sh && ./native/build-smoke.sh && ./native/smoke` prints `1` from a real engine query, no root required.
- Phase 6 completion criteria: fresh checkout without `PGLITE_LIB_DIR` → `cargo test -p pglite` green via downloaded prebuilt.
- Open questions tracked in `blueprint.md` resolve inside tasks 1.4-1.6 and 2.3.
