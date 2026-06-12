# Codebase Analysis: implement-pglite-v1

Generated: 2026-06-12 (code-explorer agent)
Scope: Native libpglite.a build + Rust host layer; pgl_* FFI ABI contract; boot/query pump mechanics

## Similar Features Found

### 1. TS Host Layer (pglite.ts + initdb.ts)
- **Location**: `.dev/pglite/packages/pglite/src/pglite.ts`, `initdb.ts`
- **Pattern**: Callback registration via `mod.addFunction` + `_pgl_set_*` setters; single-shot `callMain` followed by an explicit pump loop; startup packet injected as a zero-first-byte message.
- **Relevance**: Direct 1:1 blueprint for the Rust host layer. Every Rust method maps to a TS counterpart.

### 2. pglitec.c libc shim
- **Location**: `postgres-pglite/pglite/src/pglitec/pglitec.c`
- **Pattern**: Compiled first, **without** the `-D__PGLITE__ -Drecv=… -Dsend=…` override flags (`build-pglite.sh:30`), then the rest of Postgres is compiled with those flags. This ensures pglitec.c itself calls the real `longjmp`, `popen`, etc.
- **Relevance**: The shim is the native-build artifact; its entire function set becomes `pglite_native.h`.

## Architecture Layers

| Layer | Directory | Pattern | Examples |
|-------|-----------|---------|----------|
| Engine (C) | `postgres-pglite/src/backend/` | Patched PG: unrolled main loop, socket → r/w callbacks, exit/longjmp contained | `tcop/postgres.c`, `libpq/pqcomm.c` |
| libc shim | `postgres-pglite/pglite/src/pglitec/` | Override slab compiled separately; function-pointer callbacks set at runtime | `pglitec.c` |
| Build pipeline | `postgres-pglite/build-pglite.sh` | 6-step emscripten; emscripten-specific flags are the delta vs. native | `build-pglite.sh` |
| TS reference host | `.dev/pglite/packages/pglite/src/` | Module factory + explicit callback wiring | `pglite.ts`, `initdb.ts`, `postgresMod.ts` |
| Rust host (planned) | `crates/pglite-sys/`, `crates/pglite/` | Engine thread + mpsc + oneshot futures | none yet |

## Findings

### 1. Where `__PGLITE__` is defined

`-D__PGLITE__` is **not** in `configure.ac`, `src/template/emscripten`, or any Makefile include. Injected entirely via CFLAGS in `build-pglite.sh:33-45`:

```
-D__PGLITE__
-Dsystem=pgl_system -Dpopen=pgl_popen -Dpclose=pgl_pclose
-Dgeteuid=pgl_geteuid -Dgetuid=pgl_getuid -Dgetpwuid=pgl_getpwuid
-Dexit=pgl_exit
-Dmunmap=pgl_munmap
-Dfcntl=pgl_fcntl
-Datexit=pgl_atexit
-Dsetsockopt=pgl_setsockopt -Dgetsockopt=pgl_getsockopt -Dgetsockname=pgl_getsockname
-Drecv=pgl_recv -Dsend=pgl_send -Dconnect=pgl_connect
-Dpoll=pgl_poll
-Dshmget=pgl_shmget -Dshmat=pgl_shmat -Dshmdt=pgl_shmdt -Dshmctl=pgl_shmctl
-Dlongjmp=pgl_longjmp -Dsiglongjmp=pgl_siglongjmp
```

Critical: `pglitec.o` is compiled at line 30 **before** this block — the shim calls real `longjmp`/`exit`/`popen`. Native build must replicate this ordering.

`__PGLITE__` guards (exhaustive): `src/backend/tcop/postgres.c:155,207,216,300,4777,4931`; `access/transam/xlog.c:2522`; `postmaster/checkpointer.c:950`; `utils/misc/guc.c:2650`; `utils/init/miscinit.c:378,398,419` (UID/permission checks disabled); `storage/file/fd.c:527,701` (`sync_file_range`→`fsync`); `port/posix_sema.c:297`; `interfaces/libpq/fe-exec.c:31`; `bin/pg_dump/*` (irrelevant for libpglite.a).

### 2. Exact Boot Sequence

**Phase A — initdb** (`initdb.ts:execInitdb`):
1. `pgl_freopen(pgstdinPath, "r", 0)` / `pgl_freopen(pgstdoutPath, "w", 1)` on the postgres module (`initdb.ts:165-171`)
2. `_pgl_set_system_fn` / `_pgl_set_popen_fn` / `_pgl_set_pclose_fn` registered (`initdb.ts:127,147,159`)
3. `initdb callMain(["--allow-group-access","--encoding","UTF8","--locale=C.UTF-8","--locale-provider=libc","--auth=trust"])` (`initdb.ts:202`)
4. initdb's `popen("/pglite/bin/postgres --single ...")` intercepted → host re-enters postgres main (`--single -j -c exit_on_error=false -D /pglite/data postgres`); `pclose` triggers deferred re-entry for "w"-mode popen

**Phase B — backend** (`pglite.ts:#startInSingleMode`):
1. `_pgl_setPGliteActive(1)` (`pglite.ts:571`)
2. `callMain([...defaultStartParams,"-D",PGDATA,dbname])` (`pglite.ts:1302`) → `PostgresMain` → `sigsetjmp(postgresmain_sigjmp_buf,1)` (`postgres.c:5113`) → `PostgresMainLoopOnce()` finds no data, sends ReadyForQuery, `exit(PGLITE_EXIT_ALIVE)` = **99**. Return `!= 99` is fatal (`pglite.ts:1304-1306`)
3. `_pgl_startPGlite()` (`pglite.ts:1288`): `whereToSendOutput=DestRemote`, `ExitOnAnyError=false`, `MyBackendType=B_BACKEND`, `IsUnderPostmaster=true`, `initDummyPort()`+`pq_init()`, loads HBA
4. `_pgl_set_rw_cbs(readCb, writeCb)` registered during init, before callMain (`pglite.ts:742`)

**Phase C — startup packet** (`pglite.ts:#processStartupPacket`):
1. `_pgl_getMyProcPort()` (`postgres.c:274`)
2. `_ProcessStartupPacket(port, true, true)` reads v3 startup packet from read-callback buffer, returns 0 on success (`backend_startup.c:467`)
3. `_pgl_sendConnData()` sends AuthenticationOk + GUC reports + BackendKeyData + ReadyForQuery (`postgres.c:278-299`)
4. `_pgl_pq_flush()`

### 3. The Query Pump (`execProtocolRawSync`, `pglite.ts:876,904-935`)

```
while (readOffset < message.length || pq_buffer_remaining_data() > 0) {
    try { PostgresMainLoopOnce() }
    catch (e) { if (e.status === 100) PostgresMainLongJmp(); /* else continue */ }
}
PostgresSendReadyForQueryIfNecessary();
pgl_pq_flush();
```

- Read side: `pgl_recv` → read callback copies bytes from host output buffer, advances offset
- Write side: `pgl_send` → write callback parses through ProtocolParser AND appends raw bytes
- Termination: `pq_buffer_remaining_data()` (`pqcomm.c:1126`) = bytes unread in PQ receive buffer
- Error path: SQL error → `siglongjmp` → `pgl_longjmp` intercepts, sets `send_ready_for_query=true`, `exit(100)`; host calls `PostgresMainLongJmp()` (aborts tx, emits ErrorResponse) and continues
- **Native difference**: exit(99)/exit(100) must be trapped via C-level `sigsetjmp` trampoline (`pgl_native_pump`), never unwinding Rust frames

### 4. pglitec.c Function Inventory (→ pglite_native.h)

**Host-callable**: `pgl_setPGliteActive(int)`, `pgl_set_system_fn`, `pgl_set_popen_fn`, `pgl_set_pclose_fn`, `pgl_set_rw_cbs(read,write)` (each `ssize_t(*)(void*,size_t)`), `pgl_freopen(path,mode,streamid)`, `pgl_run_atexit_funcs()`, `pgl_longjmp`, `pgl_siglongjmp`, `clear_setitimer`.

**Transparent libc overrides**: `pgl_system` (cb or 123), `pgl_popen`/`pgl_pclose` (cb or real), `pgl_geteuid`/`pgl_getuid` (uid 123), `pgl_getpwuid` (static "postgres"), `pgl_atexit` (32-slot store), `pgl_exit` (flush + real exit), `pgl_fcntl`/`pgl_setsockopt`/`pgl_getsockopt`/`pgl_getsockname`/`pgl_connect` (no-op 0), `pgl_recv`/`pgl_send` (route to callbacks), `pgl_poll` (returns nfds), `pgl_shm*` (malloc-backed), `pgl_munmap` (no-op).

**Non-emscripten `#else` branch**: only `#define EMSCRIPTEN_KEEPALIVE` empty — every function compiles verbatim natively.

**Vestigial (exclude from header)**: `pgl_sigsetjmp` (extern declared `postgres.c:220`, never defined/called), `pgl_set_pipe_fn` and `pgl_proc_exit` (declared in TS types, no C implementation).

### 5. Runtime Paths and Environment

Paths the engine expects (WASM layout): `/home/postgres/.pgpass` (0600), `/pglite/bin/{initdb,postgres}` (empty 0555 placeholders), `/pglite/share/postgresql/` (bki/timezone/conf templates from `make install`), `/pglite/data` (PGDATA), `/pglite/password`, `/pglite/pgstdin`+`/pglite/pgstdout` (initdb↔postgres IPC), `/pglite/locale-a` (read by `pgl_popen` when cmd == `locale -a`), `/pglite/icu/` (skipped in v1).

Env before main: `HOME=/home/postgres`, `USER`/`LOGNAME`=`postgres`, `PGDATA`, `PGUSER`/`PGDATABASE`=`postgres`, `LANG`/`LC_COLLATE`/`LC_CTYPE` (native v1: C.UTF-8), `TZ`/`PGTZ=UTC`, `PGCLIENTENCODING=UTF8`. `PGLITE_ENV` is emscripten-only.

### 6. Export List vs postgresMod.ts

All host-needed symbols in `included.pglite.exports` are already in `postgresMod.ts`. The remaining 700+ export entries serve the emscripten MAIN_MODULE/SIDE_MODULE dynamic-extension ABI — irrelevant for static native linking. **Complete native ABI = postgresMod.ts ∪ pglitec.c keepalive fns, minus vestigial three.**

### 7. .dev/db/

TanStack DB (`@tanstack/db`) — unrelated JS reactive-collections library. Ignore.

## Dependencies

- Internal: `tcop/postgres.c` (pgl_startPGlite, pgl_sendConnData, PostgresMainLoopOnce, PostgresMainLongJmp, PostgresSendReadyForQueryIfNecessary, exit codes), `libpq/pqcomm.c` (pq_buffer_remaining_data), `pglitec.c` (everything else)
- External (native v1): zlib only (ICU/libxml/libxslt/uuid dropped)

## Conventions to Follow

| Category | Convention |
|----------|------------|
| Struct ownership | Least New Definitions: attach to existing struct before creating new |
| Inline comments | None |
| Locks | Per-field `Arc<Mutex<T>>`; `&self`-only public API; no `XxxInner` |
| Errors | thiserror enum + `?` everywhere |
| unsafe | Confined to `pglite-sys` + `engine.rs` |
| longjmp | C trampoline owns setjmp boundary; never unwind Rust |

## Risks Discovered

| Risk | Impact | Mitigation |
|------|--------|------------|
| exit(99)/exit(100) escape Rust frames natively | UB | `pgl_native_pump` C trampoline owns sigsetjmp + the pump loop |
| `pgl_sigsetjmp` extern but undefined | Linker error if reached | Confirm dead; stub if linker complains |
| `pgl_set_pipe_fn`/`pgl_proc_exit` vestigial | Linker error if declared | Exclude from pglite_native.h |
| `munmap` no-op leaks | Slow leak | Accept v1; backlog |
| Hardcoded `/pglite/*` paths | Needs root | Path-relocation strategy (see blueprint) |
| initdb popen("/pglite/bin/postgres ...") would fork-exec natively | Boot failure | Register popen/system callbacks BEFORE pgl_initdb_main |
| `-m32`/`-s*`/`SUPPORT_LONGJMP` invalid natively | Build failure | Strip all; drop `--disable-spinlocks`, `--disable-largefile` |

## Confidence

Pattern 97 / Architecture 95 / Recommendation 95 — all key functions read at source level with line refs.
