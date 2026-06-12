# pglite-rs

In-process PostgreSQL for Rust — embedded like SQLite, full Postgres SQL, async on any runtime.

Built on [postgres-pglite](https://github.com/electric-sql/postgres-pglite), the PostgreSQL fork powering [PGlite](https://pglite.dev/), compiled natively and linked straight into your binary. No server, no Docker, no install step.

```rust
use pglite::PGlite;

let db = PGlite::open("./mydata").await?;          // initdb runs on first open
db.exec("CREATE TABLE users (id serial PRIMARY KEY, name text)").await?;

let rows = db.query("SELECT id, name FROM users WHERE id > $1", &[&0i32]).await?;
let name: &str = rows[0].get(1)?;

let tx = db.transaction().await?;                  // rollback on drop
tx.exec("INSERT INTO users (name) VALUES ('alice')").await?;
tx.commit().await?;

db.close().await?;
```

Runtime-agnostic: futures work on tokio, smol, async-std, or plain `futures::executor::block_on`. The crate depends on `futures`, never on a specific runtime.

## How it works

```
┌─────────────────────────────────────┐
│ pglite        safe async API        │
│ pglite-sys    hand-written FFI      │
│ libpglite.a   patched Postgres 17   │
└─────────────────────────────────────┘
```

The engine is real PostgreSQL with PGlite's patches: the main loop is callable, socket IO is routed through in-memory callbacks, and exits/longjmps are contained in a C trampoline. All engine calls are confined to one dedicated thread; your async calls communicate with it through channels. Data crosses the FFI boundary only as Postgres wire-protocol bytes.

## Building

The engine ships as a prebuilt static library downloaded by the build script (cached in `~/.cache/pglite-rs/`). To build it yourself:

```sh
./native/build-libpglite.sh     # needs clang, bison, flex, perl, make, zlib
cargo test --workspace
```

`PGLITE_LIB_DIR=/path/to/dir` overrides artifact resolution (expects `libpglite.a` + `pglite-runtime.tar`).

## v1 limits

- One open `PGlite` per process (`Error::AlreadyOpen`), and one engine boot per process lifetime — reopen after close requires a new process (`Error::ReopenUnsupported`). Reopening a data directory from a fresh process works fully.
- C locale only (built without ICU); no extensions yet; macOS + Linux.

## License

Apache-2.0. PostgreSQL itself is under the [PostgreSQL License](./postgres-pglite/COPYRIGHT).
