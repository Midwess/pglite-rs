# Architecture Blueprint: implement-pglite-v1

Generated: 2026-06-12 (code-architect agent)

## Design Summary

pglite-rs v1 native-links the `postgres-pglite` fork into `libpglite.a` and wraps it in a runtime-agnostic async Rust API (`futures` only, no tokio). Two crates: `pglite-sys` (hand-written FFI, no bindgen, mirrors `native/pglite_native.h`) and `pglite` (safe API). All C calls confined to one dedicated **engine OS thread**; public `PGlite` handle is `Clone + Send + Sync`, talking over `std::sync::mpsc` (commands in) / `futures::channel::oneshot` (results out). Data crosses the FFI waist only as Postgres wire-protocol bytes (`postgres-protocol`).

### Load-bearing decisions

**1. Path relocation (no root-owned `/pglite`).** Postgres already supports prefix relocation: `src/port/path.c:903` derives share/sysconf dirs relative to `my_exec_path` via `make_relative_path`. Strategy: relocatable `--prefix` at build; ship `share/postgresql` in the share bundle; at runtime extract to `$runtime_root/share/postgresql` and call new setter `pgl_set_exec_path($runtime_root/bin/postgres)` before first main. Residual hardcoded `/pglite/*` literals (pgstdin/pgstdout/.pgpass/bin placeholders) handled by new `pgl_set_runtime_root(const char*)` helper in `pglite_native.c`. PGDATA always user-specified via `-D`. Rejected: keep `/pglite` + container root (hostile to a library); chroot (privileges, not portable); patch every path (large submodule blast radius).

**2. initdb linking.** WASM ships initdb as a separate emscripten module; natively two `main()`s can't coexist. Compile `src/bin/initdb/initdb.c` (+`findtimezone.c`) with `-Dmain=pgl_initdb_main` and backend `main.c` with `-Dmain=pgl_backend_main` into the same archive. Host drives the same orchestration as `initdb.ts:70-160`: call `pgl_initdb_main(args)`; its internal popen of `postgres --single` is intercepted by registered callbacks which re-enter `pgl_backend_main`. Rejected: separate initdb binary spawned as a child process (breaks single-process + popen re-entry contract); reimplement initdb in Rust (enormous, fragile).

### Boot sequence (engine thread)
- **initdb phase**: `pgl_freopen` stdin/stdout to `$root/pgstdin`,`$root/pgstdout`; register system/popen/pclose callbacks; `pgl_initdb_main([--allow-group-access, --encoding UTF8, --locale=C.UTF-8, --locale-provider=libc, --auth=trust, -D, pgdata])`; popen re-entry into `pgl_backend_main` returns 99.
- **backend phase**: `pgl_setPGliteActive(1)`; register rw callbacks; `pgl_backend_main([...startParams, -D, pgdata, postgres])` must return 99 (`PGLITE_EXIT_ALIVE`, `postgres.c:217,4779`) else fatal; `pgl_startPGlite()`.
- **handshake**: push v3 startup packet into read buffer; `pgl_getMyProcPort()`; `ProcessStartupPacket(port,1,1)==0`; `pgl_sendConnData()` emits AuthenticationOk+GUC+BackendKeyData+ReadyForQuery; `pgl_pq_flush()`.

### Query pump (inside C trampoline `pgl_native_pump`)
`while (read_offset < msg.len || pq_buffer_remaining_data() > 0) { PostgresMainLoopOnce(); }` — error longjmp fires exit code 100 → trampoline returns 100 → host calls `PostgresMainLongJmp()` and continues; then `PostgresSendReadyForQueryIfNecessary()` + `pgl_pq_flush()`. longjmp/exit never unwind Rust frames.

## Design Decisions

| Decision | Options Considered | Chosen | Rationale |
|----------|-------------------|--------|-----------|
| Path prefix | absolute `/pglite`; chroot; patch all paths; relocatable prefix | Relocatable `--prefix` + `pgl_set_exec_path` + `pgl_set_runtime_root` | Postgres relocates from `my_exec_path` (path.c:903); minimal patch; no root |
| initdb linking | separate binary; Rust reimplementation; `-Dmain=` rename | `-Dmain=pgl_initdb_main` / `-Dmain=pgl_backend_main` | Mirrors existing `-D` discipline; preserves popen re-entry; single process |
| FFI binding | bindgen; hand-written | Hand-written extern mirroring `pglite_native.h` | ~25 fns, primitives only; no libclang dep |
| Engine ownership | tokio task; pool; dedicated thread | One dedicated `std::thread` | Postgres globals `!Send`; thread_local callback state |
| Channels | crossbeam; tokio; std+futures | `std::sync::mpsc` in, `futures::channel::oneshot` out | Runtime-agnostic; blocking recv engine-side |
| longjmp containment | catch_unwind; signals; C trampoline | C `pgl_native_pump` owns sigsetjmp | longjmp through Rust = UB |
| Share bundle | download; dir; embed | `include_bytes!` + extract on first open | Self-contained binary |
| Engine artifact | source build; prebuilt | Prebuilt per-target GH release; sha256; `PGLITE_LIB_DIR` override | No bison/flex/perl for users |
| One-instance | type-level; runtime | Process-global `AtomicBool` returning `Error::AlreadyOpen` | Postgres global state |
| Typing | raw; postgres-types | `postgres-types` + `postgres-protocol` | tokio-free, mature |

## Component Design

### `PGlite` (`crates/pglite/src/db.rs`)
Public handle, `Clone + Send + Sync`. Enforces one-instance; owns engine thread handle + command sender; share-bundle extraction trigger.
```rust
impl PGlite {
    pub async fn open(data_dir: impl AsRef<Path>) -> Result<PGlite, Error>;
    pub async fn open_temp() -> Result<PGlite, Error>;
    pub async fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error>;
    pub async fn exec(&self, sql: &str) -> Result<(), Error>;
    pub async fn transaction(&self) -> Result<Transaction<'_>, Error>;
    pub async fn close(self) -> Result<(), Error>;
}
```
Fields: `cmd_tx: std::sync::mpsc::Sender<EngineCommand>`, `Arc<JoinHandle<()>>`, static `OPEN: AtomicBool`. No Inner struct.

### `Engine` (`crates/pglite/src/engine.rs`) — unsafe lives here + pglite-sys only
Owns all C interaction on its thread: share-bundle extract, runtime root + exec path, boot (initdb then backend then handshake), `thread_local!` IO buffers, pump via `pgl_native_pump`, teardown.
```rust
impl Engine {
    fn spawn(boot: BootConfig) -> (mpsc::Sender<EngineCommand>, JoinHandle<()>);
    fn boot(&mut self) -> Result<(), Error>;
    fn exec_protocol(&mut self, msg: &[u8]) -> Result<Vec<u8>, Error>;
}
thread_local! { static IO: RefCell<EngineIo>; }
extern "C" fn pgl_read_cb(buf: *mut c_void, max: usize) -> isize;
extern "C" fn pgl_write_cb(buf: *const c_void, len: usize) -> isize;
extern "C" fn pgl_system_cb(cmd: *const c_char) -> isize;
extern "C" fn pgl_popen_cb(cmd: *const c_char, mode: *const c_char) -> *mut FILE;
extern "C" fn pgl_pclose_cb(stream: *mut FILE) -> c_int;
```

### `Row` (`crates/pglite/src/row.rs`)
Parses `RowDescription`+`DataRow` (`postgres-protocol::message::backend`); `get<T: FromSql>(idx)`, `try_get(name)`, `columns()`.

### `Transaction` (`crates/pglite/src/transaction.rs`)
BEGIN on create; `query`/`exec`/`commit`/`rollback`; `Drop` issues fire-and-forget ROLLBACK if uncommitted. Borrows `&PGlite`.

### `Error` (`crates/pglite/src/error.rs`)
thiserror: `Database { sqlstate, message, detail, hint }`, `AlreadyOpen`, `Closed`, `Io`, `Protocol`, `Boot`.

### `pglite-sys` (`crates/pglite-sys/src/lib.rs`)
One extern block hand-mirroring `pglite_native.h`; callback typedefs; opaque `Port`/`FILE`; `links = "pglite"`; dep `libc` only.

### `pglite_native.h` / `pglite_native.c` (`native/`)
Authoritative ABI header + native shim: `pgl_native_pump` (exit trampoline, net-new — replaces Emscripten's JS-exception containment), `pgl_set_runtime_root`, `pgl_set_exec_path`. Excludes vestigial `pgl_set_pipe_fn`/`pgl_proc_exit`/`pgl_sigsetjmp`.

### `build-libpglite.sh` (`native/`)
Compile `pglitec.c` clean first, then native `./configure` (lite profile: `--without-openssl --without-pam --without-readline --without-llvm --without-icu --with-zlib`, relocatable `--prefix`; drop `-m32`, all `-s*`, `--disable-spinlocks`, `--disable-largefile`, `SUPPORT_LONGJMP`), then `make`/`make install` with full `-D` override list, then `-Dmain=` compiles of initdb/backend mains, then `pglite_native.c`, then `ar rcs libpglite.a`, then tar share bundle.

### `build.rs` (`crates/pglite-sys/`)
`PGLITE_LIB_DIR` override first; else download per-target GH release asset (tag = submodule pin), sha256 verify, cache; emit link directives (`static=pglite`, `z`).

### CI (`.github/workflows/`)
`artifacts.yml`: matrix (x86_64-linux-gnu, aarch64-apple-darwin, x86_64-apple-darwin) builds and uploads to release. `ci.yml`: fmt/clippy/test with cached artifact.

## Files to Create

| File | Purpose |
|------|---------|
| `Cargo.toml` | workspace |
| `crates/pglite-sys/{Cargo.toml,build.rs,src/lib.rs}` | FFI crate |
| `crates/pglite/Cargo.toml` | deps: pglite-sys, futures, postgres-protocol, postgres-types, thiserror, libc |
| `crates/pglite/src/{lib.rs,db.rs,engine.rs,row.rs,transaction.rs,error.rs}` | safe API |
| `crates/pglite/tests/integration.rs` | integration suite |
| `native/pglite_native.h` | ABI contract |
| `native/pglite_native.c` | trampoline + setters |
| `native/build-libpglite.sh` | engine build |
| `native/smoke.c` + `native/build-smoke.sh` | Phase-1 C gate |
| `.github/workflows/{artifacts.yml,ci.yml}` | CI |
| `.gitignore` additions | `/target`, `native/out/`, `native/*.o`, `native/libpglite.a`, `native/pglite-share.tar`, `*.sha256` |
| `rust-toolchain.toml` | pin toolchain |

## Interface Specifications

### `native/pglite_native.h` (draft)
```c
#ifndef PGLITE_NATIVE_H
#define PGLITE_NATIVE_H
#include <stddef.h>
#include <stdio.h>
#include <sys/types.h>

struct Port;

typedef ssize_t (*pgl_read_t)(void *buffer, size_t max_length);
typedef ssize_t (*pgl_write_t)(const void *buffer, size_t length);
typedef ssize_t (*pglite_system_t)(const char *command);
typedef FILE*   (*pglite_popen_t)(const char *command, const char *mode);
typedef int     (*pglite_pclose_t)(FILE *stream);

void pgl_set_rw_cbs(pgl_read_t read_cb, pgl_write_t write_cb);
void pgl_set_system_fn(pglite_system_t system_fn);
void pgl_set_popen_fn(pglite_popen_t popen_fn);
void pgl_set_pclose_fn(pglite_pclose_t pclose_fn);

FILE *pgl_freopen(const char *pathname, const char *mode, int streamid);

int  pgl_setPGliteActive(int newValue);
void pgl_startPGlite(void);
void pgl_run_atexit_funcs(void);

int  pgl_initdb_main(int argc, char **argv);
int  pgl_backend_main(int argc, char **argv);

struct Port *pgl_getMyProcPort(void);
int  ProcessStartupPacket(struct Port *port, int ssl_done, int gss_done);
void pgl_sendConnData(void);

void PostgresMainLoopOnce(void);
void PostgresMainLongJmp(void);
void PostgresSendReadyForQueryIfNecessary(void);
int  pq_buffer_remaining_data(void);
void pgl_pq_flush(void);

int  pgl_native_pump(void);
void pgl_set_runtime_root(const char *root);
void pgl_set_exec_path(const char *postgres_bin_path);

#endif
```

### `crates/pglite-sys/src/lib.rs` (extern draft)
```rust
use libc::{c_char, c_int, c_void, size_t, ssize_t, FILE};

#[repr(C)] pub struct Port { _private: [u8; 0] }

pub type PglReadCb   = unsafe extern "C" fn(*mut c_void, size_t) -> ssize_t;
pub type PglWriteCb  = unsafe extern "C" fn(*const c_void, size_t) -> ssize_t;
pub type PglSystemCb = unsafe extern "C" fn(*const c_char) -> ssize_t;
pub type PglPopenCb  = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut FILE;
pub type PglPcloseCb = unsafe extern "C" fn(*mut FILE) -> c_int;

extern "C" {
    pub fn pgl_set_rw_cbs(read_cb: PglReadCb, write_cb: PglWriteCb);
    pub fn pgl_set_system_fn(f: PglSystemCb);
    pub fn pgl_set_popen_fn(f: PglPopenCb);
    pub fn pgl_set_pclose_fn(f: PglPcloseCb);
    pub fn pgl_freopen(pathname: *const c_char, mode: *const c_char, streamid: c_int) -> *mut FILE;
    pub fn pgl_setPGliteActive(new_value: c_int) -> c_int;
    pub fn pgl_startPGlite();
    pub fn pgl_run_atexit_funcs();
    pub fn pgl_initdb_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
    pub fn pgl_backend_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
    pub fn pgl_getMyProcPort() -> *mut Port;
    pub fn ProcessStartupPacket(port: *mut Port, ssl_done: c_int, gss_done: c_int) -> c_int;
    pub fn pgl_sendConnData();
    pub fn PostgresMainLoopOnce();
    pub fn PostgresMainLongJmp();
    pub fn PostgresSendReadyForQueryIfNecessary();
    pub fn pq_buffer_remaining_data() -> c_int;
    pub fn pgl_pq_flush();
    pub fn pgl_native_pump() -> c_int;
    pub fn pgl_set_runtime_root(root: *const c_char);
    pub fn pgl_set_exec_path(postgres_bin_path: *const c_char);
}
```

### Engine channel protocol
```rust
enum EngineCommand {
    Exec  { wire: Vec<u8>, reply: oneshot::Sender<Result<Vec<u8>, Error>> },
    Close { reply: oneshot::Sender<Result<(), Error>> },
}
```
Flow: PGlite method serializes wire bytes, sends `Exec` over mpsc; engine thread blocking-recv, sets out_buf, runs `pgl_native_pump` loop, replies in_buf via oneshot; caller parses to Rows/Error. Close: `pgl_setPGliteActive(0)`, Terminate msg, `pgl_run_atexit_funcs()`, thread returns, `OPEN=false`. Pump codes: 99 = alive (boot), 100 = longjmp (call `PostgresMainLongJmp`, continue), other = `Error::Boot`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Native `__PGLITE__` build untested upstream | High | High | Phase 1 = pure-C smoke gate before any Rust |
| `-Dmain=` rename collision/missed ref | Med | High | Resolve at `ar` time in 1.3; smoke exercises both entries |
| Residual hardcoded `/pglite/*` paths | Med | High | `pgl_set_runtime_root` + exact placeholder layout under root; grep for literals in 1.4 |
| longjmp unwinds Rust frame | Low | High | Pump loop entirely inside `pgl_native_pump`; Rust sees ints only |
| Thread-local IO aliasing | Med | Med | Single engine thread; tight RefCell scopes; no re-entrant pump |
| No-ICU collation surprise | Med | Med | initdb forces `--locale=C.UTF-8 --locale-provider=libc`; documented v1 cut |
| Cross-target artifact mismatch | Med | Med | Pinned triples; sha256; `PGLITE_LIB_DIR` escape hatch |
| Share bundle drift vs engine | Low | High | Bundle built in same CI job from same submodule pin; tag = pin |

## Open Questions (resolve in Phase 1)

- Does `pgl_backend_main` return 99 natively, or does `pgl_exit`'s real `exit()` kill the process? Trampoline must intercept — verify the `-Dexit=pgl_exit` override path vs trampoline interaction in 1.5.
- `pgl_set_exec_path` sufficient, or also `PGSYSCONFDIR`/`PGLOCALEDIR` env? Empirical in 1.5.
- `pq_buffer_remaining_data` exact return type natively — confirm before finalizing extern block.
- Target triple matrix (linux-gnu x86_64 + macOS arm64/x86_64 confirmed; musl?).
- `open_temp` cleanup on `Drop` vs `close` only (default: Drop removes if not closed).

## Confidence

Design completeness 88 / Risk accuracy 82 / Feasibility 85 — uncertainty concentrated in Phase 1 native engine proof.
