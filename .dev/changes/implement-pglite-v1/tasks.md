# Tasks: implement-pglite-v1

## Progress: [0/26]

**Process rule**: every task ends with `git commit` + `git push origin main` (identity `tiendang <tiendvlp@gmail.com>`, already configured locally). Commit messages: `phase-N: <what>`.

## 1. Native engine proof (pure C — de-risking gate, NO Rust)

- [ ] 1.1 `native/build-libpglite.sh` step 1: compile `pglitec.c` clean (no `-D` overrides) + native `./configure` lite profile (`--without-openssl --without-pam --without-readline --without-llvm --without-icu --with-zlib`, relocatable `--prefix`); commit+push
- [ ] 1.2 Add `make`/`make install` with full `-D` override list from `build-pglite.sh:33-45` (drop `-m32`, all `-s*`, `SUPPORT_LONGJMP`, `--disable-spinlocks`, `--disable-largefile`); backend objects build; commit+push
- [ ] 1.3 Compile `src/backend/main/main.c` with `-Dmain=pgl_backend_main` and `src/bin/initdb/initdb.c`+`findtimezone.c` with `-Dmain=pgl_initdb_main`; `ar rcs libpglite.a` over object set; tar `share/postgresql` → `pglite-share.tar`; commit+push
- [ ] 1.4 Write `native/pglite_native.h` (full ABI, exclude vestigial `pgl_set_pipe_fn`/`pgl_proc_exit`/`pgl_sigsetjmp`) + `native/pglite_native.c` (`pgl_native_pump` sigsetjmp trampoline, `pgl_set_runtime_root`, `pgl_set_exec_path`); grep engine for residual `/pglite` literals; compile into archive; commit+push
- [ ] 1.5 `native/smoke.c`: runtime-root layout (pgstdin/pgstdout/.pgpass/bin placeholders), env setup, static-buffer callbacks, initdb phase → backend phase (verify return 99 — resolves exit-interception open question) → handshake to first ReadyForQuery; commit+push
- [ ] 1.6 Extend `smoke.c`: push `SELECT 1` simple-query through pump; assert RowDescription+DataRow("1")+ReadyForQuery; `native/build-smoke.sh`; **GATE: must pass before Phase 2**; commit+push

## 2. FFI waist (pglite-sys)

- [ ] 2.1 Root `Cargo.toml` workspace + `crates/pglite-sys/Cargo.toml` (`links="pglite"`, dep `libc`) + `.gitignore` additions + `rust-toolchain.toml`; commit+push
- [ ] 2.2 `build.rs`: `PGLITE_LIB_DIR` branch (download branch stubbed); link directives `static=pglite` + `z`; commit+push
- [ ] 2.3 `src/lib.rs`: extern block mirroring `pglite_native.h` 1:1 + callback typedefs + opaque `Port`; confirm `pq_buffer_remaining_data` return type against source; commit+push
- [ ] 2.4 Linkage test: raw-FFI boot + `SELECT 1` (port of smoke.c) as `#[test]`; `PGLITE_LIB_DIR=... cargo test -p pglite-sys` green; commit+push

## 3. Engine host layer

- [ ] 3.1 `crates/pglite/Cargo.toml` (futures, postgres-protocol, postgres-types, thiserror, libc) + `lib.rs` skeleton + `error.rs` thiserror enum; commit+push
- [ ] 3.2 `engine.rs`: `thread_local!` `EngineIo` buffers + five `extern "C"` callbacks (read/write/system/popen/pclose); commit+push
- [ ] 3.3 `Engine::boot`: share-bundle extract, runtime root + exec path, initdb phase (popen re-entry), backend phase (==99), handshake; commit+push
- [ ] 3.4 `Engine::exec_protocol`: `pgl_native_pump` loop (100 → `PostgresMainLongJmp` + continue; drain `pq_buffer_remaining_data`); commit+push
- [ ] 3.5 Engine thread spawn + `std::sync::mpsc` command loop + `futures::channel::oneshot` replies; `include_bytes!` share bundle; off-thread round-trip test; commit+push

## 4. Public API

- [ ] 4.1 `db.rs`: `PGlite::open/open_temp/close`; global `AtomicBool` → `Error::AlreadyOpen`; commit+push
- [ ] 4.2 `exec` (simple protocol, multi-statement) + `query` (extended protocol, params via `postgres-protocol` frontend + `postgres-types` ToSql); commit+push
- [ ] 4.3 `row.rs`: RowDescription/DataRow parse, `get`/`try_get` via FromSql; ErrorResponse → `Error::Database{sqlstate,message,detail,hint}`; commit+push
- [ ] 4.4 `transaction.rs`: BEGIN/commit/rollback + Drop-rollback; `lib.rs` re-exports; commit+push

## 5. Integration tests

- [ ] 5.1 CRUD + type round-trips (int/text/bool/float/bytea/timestamp, C locale); commit+push
- [ ] 5.2 Transactions (commit, rollback, drop-rollback) + error mapping assertions; commit+push
- [ ] 5.3 Reopen-data persistence; `AlreadyOpen` enforcement; `open_temp` cleanup; commit+push

## 6. Distribution & CI

- [ ] 6.1 `.github/workflows/artifacts.yml`: target matrix (x86_64-linux-gnu, aarch64-apple-darwin, x86_64-apple-darwin) → `build-libpglite.sh` → release upload (`libpglite.a`+header+share tar+sha256, tag = submodule pin); commit+push
- [ ] 6.2 `build.rs` download branch: release URL by target, sha256 verify, cache; commit+push
- [ ] 6.3 `.github/workflows/ci.yml`: fmt/clippy/`cargo test -p pglite` consuming artifact; commit+push
- [ ] 6.4 README usage + `PGLITE_LIB_DIR` docs; finalize submodule pin = release tag; commit+push

---

## Notes

- Phase 1 completion criteria: `./native/build-libpglite.sh && ./native/build-smoke.sh && ./native/smoke` prints `1` from a real engine query, no root required.
- Phase 6 completion criteria: fresh checkout without `PGLITE_LIB_DIR` → `cargo test -p pglite` green via downloaded prebuilt.
- Open questions tracked in `blueprint.md` resolve inside tasks 1.4-1.6 and 2.3.
