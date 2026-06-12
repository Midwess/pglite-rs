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

Also included: `PGliteOptions` (custom user/database, relaxed durability, server params, locale provider), `db.listen` for LISTEN/NOTIFY, `db.live_query` for reactive queries that re-run on table changes, `db.copy_in`/`db.copy_out` for bulk data, and `db.dump_data_dir` / `PGlite::restore_data_dir` for tarball backups.

## Features

| Cargo feature | Adds |
|---|---|
| `pgvector` | the pgvector extension — `CREATE EXTENSION vector`, embedding columns, similarity search |
| `pgcrypto` | the pgcrypto extension — digests, encryption (needs OpenSSL at artifact-build time only) |
| `icu` | ICU engine variant — real Unicode collation via `locale_provider: Icu` (~+40MB, statically bundled) |
| `multiple-process` | `PGlite::open_multi_process` — a child postmaster with pooled connections for true concurrent sessions (parallel transactions, cross-session locks), same API, no networking |
| `socket` | `PGlite::serve_unix_socket` — a unix-socket gateway so unmodified ORMs (SQLx, SeaORM, Diesel) talk to the in-process engine |

Extension features require one linker flag in the consuming binary so the dlopen'd modules can resolve engine symbols — add to your crate's `build.rs`:

```rust
fn main() {
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-Wl,-export_dynamic");
    #[cfg(not(target_os = "macos"))]
    println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
}
```

ICU note: a data directory initialized with `locale_provider: Icu` can only be opened by `icu`-feature builds, and vice-versa for libc datadirs.

Runtime-agnostic: futures work on tokio, smol, async-std, or plain `futures::executor::block_on`. The crate depends on `futures`, never on a specific runtime.

## Using ORMs

ORMs speak the Postgres wire protocol over connections they open themselves, so pglite-rs meets them at a unix socket — a RAM kernel pipe with a filesystem nameplate, no TCP, no networking.

**Multi-process mode** (recommended for ORMs — real concurrent sessions): the child postmaster already listens on a private socket; hand its address to any driver.

```rust
let db = PGlite::open_multi_process("./data", MultiProcessOptions::default()).await?;
let url = db.connection_uri().unwrap();

let pool = sqlx::postgres::PgPoolOptions::new().connect(&url).await?;          // SQLx
let conn = sea_orm::Database::connect(&url).await?;                            // SeaORM
let mut pg = diesel::PgConnection::establish(&url)?;                           // Diesel
```

External clients get `extra_connections` postmaster slots (default 4) — size it to your ORM pool via `MultiProcessOptions`. The socket lives until `close()`/drop; connect after open, disconnect before close.

**In-process mode** (`socket` feature): no server exists, so a gateway thread fakes one.

```rust
let db = PGlite::open("./data").await?;
let gateway = db.serve_unix_socket().await?;
let pool = sqlx::postgres::PgPoolOptions::new()
    .max_connections(1)
    .connect(gateway.uri())
    .await?;
```

The engine holds a single session, so set the ORM pool size to exactly 1, and know that the ORM and the Rust API share that session: a `db.exec` issued while the ORM holds an open transaction executes inside it. `COPY ... TO STDOUT` works through the gateway; `COPY ... FROM STDIN` does not (in-process engine cannot pause mid-COPY) — use multi-process mode for bulk loads. The gateway cleans up fully on drop.

## How it works

```
┌─────────────────────────────────────┐
│ pglite        safe async API        │
│ pglite-sys    hand-written FFI      │
│ libpglite.a   patched Postgres 17   │
└─────────────────────────────────────┘
```

The engine is real PostgreSQL with PGlite's patches: the main loop is callable, socket IO is routed through in-memory callbacks, and exits/longjmps are contained in a C trampoline. All engine calls are confined to one dedicated thread; your async calls communicate with it through channels. Data crosses the FFI boundary only as Postgres wire-protocol bytes.

## Examples

Runnable proofs that full Postgres survives embedding — and that a stock ORM drives it. Every example runs its SQL through **SQLx over the socket gateway** (in-process engine, pool size 1): `cargo run -p pglite-examples --features orm --bin <name>`.

| Binary | Demonstrates |
|---|---|
| `basic` | open, SQLx queries with binds, transactions, rollback |
| `jsonb` | jsonb operators, GIN containment, `jsonb_agg`, `jsonb_set` |
| `analytics` | window functions, recursive CTEs, LATERAL, ROLLUP |
| `fulltext` | tsvector generated column, GIN search, `ts_rank`, `ts_headline` |
| `rich_types` | enums, domains with CHECK, arrays, ranges, uuid |
| `plpgsql` | plpgsql functions, row triggers, RAISE surfacing as `sqlx::Error::Database` |
| `ddl_power` | range partitioning, upsert, generated columns, materialized views |
| `reactive` | SQLx NOTIFY firing native `listen()` callbacks, SQLx inserts re-running native live queries, COPY via the native API |
| `multi_process` | SQLx pool of 4 over `connection_uri()` — concurrent backends, cross-connection MVCC (`--features orm,multiple-process`) |

The library itself stays tokio-free; sqlx/tokio are example-crate dependencies only.

The `examples/build.rs` carries the `export_dynamic` linker flag your own binaries need when using extensions or plpgsql (see Features).

## Building

The engine ships as a prebuilt static library downloaded by the build script (cached in `~/.cache/pglite-rs/`). To build it yourself:

```sh
./native/build-libpglite.sh     # needs clang, bison, flex, perl, make, zlib
cargo test --workspace
```

`PGLITE_LIB_DIR=/path/to/dir` overrides artifact resolution (expects `libpglite.a` + `pglite-runtime.tar`).

## v1 limits

- One open `PGlite` per process (`Error::AlreadyOpen`), and one engine boot per process lifetime — reopen after close requires a new process (`Error::ReopenUnsupported`). Reopening a data directory from a fresh process works fully.
- macOS + Linux. pg_dump and a psql-compatible socket bridge are planned (`pglite-socket`).

## License

Apache-2.0. PostgreSQL itself is under the [PostgreSQL License](./postgres-pglite/COPYRIGHT).
