# Delta for Engine Build

## ADDED Requirements

### Requirement: Native lite-profile compilation

The build SHALL compile the `postgres-pglite` submodule into a native static archive `libpglite.a` using configure flags `--without-openssl --without-pam --without-readline --without-llvm --without-icu --with-zlib` with a relocatable prefix, applying the full `-D` libc-override list from `build-pglite.sh:33-45`, and omitting all emscripten/WASM workaround flags (`-m32`, every `-s*` flag, `SUPPORT_LONGJMP`, `--disable-spinlocks`, `--disable-largefile`, `--with-template=emscripten`).

#### Scenario: Successful target build

- WHEN `native/build-libpglite.sh` runs on a supported target with clang, bison, flex, perl, make installed
- THEN it emits `libpglite.a`, `pglite_native.h`, and `pglite-share.tar` (containing `share/postgresql`)

#### Scenario: Shim compiled without overrides

- WHEN the build compiles `pglitec.c`
- THEN it is compiled BEFORE and WITHOUT the `-D` override defines, so the shim calls real libc `longjmp`/`exit`/`popen`

### Requirement: Dual entry points

The build SHALL expose both program entry points in one archive by compiling `src/backend/main/main.c` with `-Dmain=pgl_backend_main` and `src/bin/initdb/initdb.c` (+ `findtimezone.c`) with `-Dmain=pgl_initdb_main`.

#### Scenario: initdb drives the backend in-process

- WHEN the host calls `pgl_initdb_main(args)` with popen/system callbacks registered
- THEN initdb completes by re-entering `pgl_backend_main` through the intercepted popen, and the backend run returns exit code 99 without terminating the process

#### Scenario: Symbol collision

- WHEN `ar rcs libpglite.a` assembles the object set
- THEN no duplicate `main` (or other) symbol collisions occur

### Requirement: Relocatable runtime prefix

The engine SHALL resolve `share/postgresql` and fixed auxiliary paths relative to host-provided locations, never requiring a root-owned `/pglite` directory.

#### Scenario: Non-root boot

- WHEN the host calls `pgl_set_exec_path` and `pgl_set_runtime_root` pointing into a user-writable directory containing the extracted share bundle and placeholder layout
- THEN initdb and backend boot succeed without elevated privileges

#### Scenario: Missing share bundle

- WHEN the runtime root lacks `share/postgresql`
- THEN boot fails with a diagnosable error (not a crash) surfaced as `Error::Boot`
