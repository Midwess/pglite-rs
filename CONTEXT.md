# CONTEXT

Domain vocabulary for pglite-rs. Use these terms precisely in specs, proposals, and code discussion.

| Term | Definition |
|------|------------|
| **Engine** | The `postgres-pglite` fork compiled natively into `libpglite.a`. Patched Postgres: unrolled main loop, callback-based IO, contained exits. Lives in the `postgres-pglite/` submodule. |
| **Host layer** | The Rust code replicating what `pglite.ts` does in TypeScript: initdb orchestration, startup handshake, wire-protocol pump, public API. Lives in `crates/pglite`. |
| **FFI waist** | The ~25-function `pgl_*` C ABI between engine and host (`pgl_startPGlite`, `PostgresMainLoopOnce`, `pgl_set_rw_cbs`, …). The only crossing point; data crosses as wire-protocol bytes. Declared authoritatively in `native/pglite_native.h`, mirrored by hand in `crates/pglite-sys` (no bindgen). |
| **Native shim** | `native/pglite_native.c` — C code we own: the exit trampoline plus typed callback setters. Compiled into the engine artifact. |
| **Exit trampoline** | `sigsetjmp` wrapper (`pgl_native_pump`) containing every Postgres `longjmp`/fake-`exit` inside C, returning an ordinary status code to Rust. Replaces Emscripten's JS-exception containment. longjmp must never unwind Rust frames. |
| **Engine thread** | The single dedicated OS thread owning all C calls into the engine. Host communicates via `std::sync::mpsc` (in) and `futures::channel::oneshot` (out). Engine state is thread-confined; callbacks use `thread_local!`. |
| **Share bundle** | Tarball of `share/postgresql` runtime data (postgres.bki, timezones) the engine needs to initdb/run. Native twin of `pglite.data`. Embedded in the Rust binary via `include_bytes!`, extracted on first open. |
| **Prebuilt artifact** | Per-target release asset: `libpglite.a` + headers + share bundle, built by CI from the pinned submodule commit. Downloaded and sha256-verified by `pglite-sys/build.rs`; `PGLITE_LIB_DIR` overrides. |
| **Reference implementation** | `.dev/pglite/packages/pglite/src/pglite.ts` and friends — the production TS host. Behavior questions about the host layer are answered by reading it. |
| **One-instance constraint** | Hard v1 limits: at most one open `PGlite` per process (`Error::AlreadyOpen`) AND at most one engine boot per process lifetime — open after close returns `Error::ReopenUnsupported`; reopen requires a new process (cross-process reopen fully supported). |
| **Extension bundle** | Per-extension CI artifact (`pglite-ext-<name>-<target>.tar.gz`): the `.so/.dylib` + SQL/control files for one Postgres extension, built against the pinned engine. Cargo feature `<name>` makes `build.rs` download it and merge it into the runtime tar. |
| **Engine variant** | A differently-configured `libpglite.a` build. v1.2 adds the `icu` variant (`--with-icu` + ICU data); the cargo `icu` feature selects which variant artifact `build.rs` downloads. Variants are supersets — feature unification stays safe. |
| **Live query** | Host-layer subscription: triggers on watched tables fire NOTIFY; host re-runs the query and pushes fresh rows to the callback. Built entirely on the v1.1 listen/notify surface. |
| **Wire protocol** | Postgres frontend/backend message protocol (v3, stable since PG 7.4). The data format crossing the FFI waist; parsed with `postgres-protocol`. |
