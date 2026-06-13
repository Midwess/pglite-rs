<p align="center">
  <img src="https://raw.githubusercontent.com/Midwess/pglite-rs/main/.github/assets/elephant.png" alt="pglite-rs logo" width="140" />
</p>

# pglite-rs

> In-process PostgreSQL for Rust — embedded like SQLite, full Postgres SQL, async on any runtime.

Built on [postgres-pglite](https://github.com/electric-sql/postgres-pglite), a single-process PostgreSQL fork, compiled natively and linked straight into your binary. No server, no Docker, no install step.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust: 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](rust-toolchain.toml)
[![Edition: 2021](https://img.shields.io/badge/edition-2021-orange.svg)](Cargo.toml)

## Table of Contents

- [Background](#background)
- [Memory footprint](#memory-footprint)
- [Install](#install)
- [Usage](#usage)
- [Features](#features)
- [Using ORMs](#using-orms)
- [How It Works](#how-it-works)
- [Examples](#examples)
- [Building](#building)
- [API](#api)
- [Contributing](#contributing)
- [License](#license)

## Background

`postgres-pglite` is a fork of PostgreSQL that runs the whole engine in a single process with no postmaster, background workers, or sockets. `pglite-rs` compiles that fork to a native static library and exposes a safe async Rust API around it. The result is real PostgreSQL semantics — types, transactions, MVCC, extensions, wire protocol — embedded directly in your process, with the same embeddable footprint people expect from SQLite.

The crate is runtime-agnostic. It depends on `futures`, not on `tokio`, `smol`, or `async-std`; pick whichever executor your application already uses.

## Memory footprint

Because the engine runs as one in-process backend — no separate server, no postmaster, and with parallel/background workers disabled — its memory use is a small fraction of a standalone PostgreSQL server. And because it is compiled to a native static library rather than WebAssembly, you get native execution speed with none of the WASM memory tax: no linear-memory heap that can only grow, no `initdb` bootstrap stranded inside the module for the life of the process, and memory that is actually returned to the OS.

The numbers below are measured, not estimated — each example program in [`examples`](examples) runs the same workload (open, `CREATE TABLE`, insert, query, transaction rollback) under `/usr/bin/time -l` with RSS sampled every 50 ms on macOS (Apple Silicon, release build):

| Mode | Steady-state RSS | Peak RSS (during init) |
| --- | --- | --- |
| Single in-process backend (`open_temp`) | **~34 MB** | ~48 MB |
| Multi-process pool, 4 live connections (`open_multi_process`) | ~101 MB across 15 processes¹ | ~101 MB |

For comparison, the WebAssembly build of PGlite (`@electric-sql/pglite` 0.5.2) running the identical workload:

| Host | Steady-state RSS | Peak RSS (during init) |
| --- | --- | --- |
| Node 25 | ~490–510 MB | ~1.6 GB |
| Chrome renderer | ~710 MB | ~1.15 GB |

That is roughly a **15–20× smaller steady-state footprint** for the same database, with native query speed and a multi-connection mode the WASM build cannot offer. A single embedded backend adds little to your process beyond Postgres's own shared buffers; the shared memory is emulated on the heap and released when the engine closes, and there are no idle worker processes sitting resident. That makes it practical to embed in CLIs, desktop apps, tests, and edge/serverless workloads where both a full Postgres server and a 700 MB WASM instance would be far too heavy.

¹ Summed RSS over the whole Postgres process tree; this over-counts because every backend maps the same shared-buffers segment, so the true physical footprint is lower. The single in-process number is near-exact.

## Install

```sh
cargo add pglite-rs
```

The package is `pglite-rs`; the library imports as `pglite` (`use pglite::PGlite`). To pull every optional extension in one go, use the `full` feature:

```sh
cargo add pglite-rs --features full
```

For extensions (`pgvector`, `pgcrypto`, `plpgsql`) you must export dynamic symbols in the consuming binary so the dlopen'd modules can resolve engine symbols. Add this to your crate's `build.rs`:

```rust
fn main() {
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-Wl,-export_dynamic");
    #[cfg(not(target_os = "macos"))]
    println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
}
```

## Usage

```rust
use pglite::PGlite;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = PGlite::open("./mydata").await?;          // initdb runs on first open
    db.exec("CREATE TABLE users (id serial PRIMARY KEY, name text)").await?;

    let rows = db.query("SELECT id, name FROM users WHERE id > $1", &[&0i32]).await?;
    let name: &str = rows[0].get(1)?;

    let tx = db.transaction().await?;                  // rollback on drop
    tx.exec("INSERT INTO users (name) VALUES ('alice')").await?;
    tx.commit().await?;

    db.close().await?;
    Ok(())
}
```

## Features

| Cargo feature | Adds |
|---|---|
| `pgvector` | the pgvector extension — `CREATE EXTENSION vector`, embedding columns, similarity search |
| `pgcrypto` | the pgcrypto extension — digests, encryption (needs OpenSSL at artifact-build time only) |
| `icu` | ICU engine variant — real Unicode collation via `locale_provider: Icu` (~+40MB, statically bundled) |
| `multiple-process` | `PGlite::open_multi_process` — child postmaster with pooled connections for true concurrent sessions (parallel transactions, cross-session locks), same API, no networking |
| `socket` *(default)* | `PGlite::unix_uri` — unix-socket gateway so unmodified ORMs (SQLx, SeaORM, Diesel) talk to the in-process engine (unix-only; inert on Windows) |
| `replica` | logical-replication consumer — stream committed changes from a remote Postgres via `PGlite::start_replica` |

> **Note:** A data directory initialized with `locale_provider: Icu` can only be opened by `icu`-feature builds, and vice-versa for libc datadirs.

## Using ORMs

ORMs speak the Postgres wire protocol over connections they open themselves, so `pglite-rs` meets them at a unix socket — a RAM kernel pipe with a filesystem nameplate, no TCP, no networking. One call covers every mode:

```rust
let db = PGlite::open("./data").await?;
let url = db.unix_uri().await?;

let pool = sqlx::postgres::PgPoolOptions::new()
    .max_connections(1)
    .connect(&url)
    .await?;                                                                    // SQLx
let conn = sea_orm::Database::connect(&url).await?;                            // SeaORM
let mut pg = diesel::PgConnection::establish(&url)?;                           // Diesel
```

In-process, the first `unix_uri()` call lazily starts a gateway thread that fakes a server; it lives inside `PGlite` and is cleaned up on `close()`/drop. The engine holds a single session, so set the ORM pool size to exactly 1, and know that the ORM and the Rust API share that session: a `db.exec` issued while the ORM holds an open transaction executes inside it. `COPY ... TO STDOUT` works through the gateway; `COPY ... FROM STDIN` does not (in-process engine cannot pause mid-COPY) — use multi-process mode for bulk loads.

### Multi-process mode (recommended for ORM pools)

Real concurrent sessions, parallel transactions, cross-session locks — `unix_uri()` returns the child postmaster's native socket, so the same code works with a real pool:

```rust
use pglite::{PGlite, MultiProcessOptions};

let db = PGlite::open_multi_process("./data", MultiProcessOptions::default()).await?;
let url = db.unix_uri().await?;
let pool = sqlx::postgres::PgPoolOptions::new().connect(&url).await?;
```

External clients get `extra_connections` postmaster slots (default 4) — size it to your ORM pool via `MultiProcessOptions`. The socket lives until `close()`/drop; connect after open, disconnect before close.

## How It Works

```
┌─────────────────────────────────────┐
│ pglite        safe async API        │
│ pglite-sys    hand-written FFI      │
│ libpglite.a   patched Postgres 17   │
└─────────────────────────────────────┘
```

The engine is real PostgreSQL with postgres-pglite's patches: the main loop is callable, socket IO is routed through in-memory callbacks, and exits/longjmps are contained in a C trampoline. All engine calls are confined to one dedicated thread; your async calls communicate with it through channels. Data crosses the FFI boundary only as Postgres wire-protocol bytes.

## Examples

Runnable proofs that full Postgres survives embedding — and that a stock ORM drives it. Every example runs its SQL through **SQLx over the socket gateway** (in-process engine, pool size 1):

```sh
cargo run -p pglite-examples --features orm --bin <name>
```

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
| `multi_process` | SQLx pool of 4 over `unix_uri()` — concurrent backends, cross-connection MVCC (`--features orm,multiple-process`) |

The library itself stays tokio-free; sqlx/tokio are example-crate dependencies only. The `examples/build.rs` carries the `export_dynamic` linker flag your own binaries need when using extensions or plpgsql (see [Install](#install)).

## Building

The engine ships as a prebuilt static library downloaded by the build script (cached in `~/.cache/pglite-rs/`). To build it yourself:

```sh
./native/build-libpglite.sh     # needs clang, bison, flex, perl, make, zlib
cargo test --workspace
```

`PGLITE_LIB_DIR=/path/to/dir` overrides artifact resolution (expects `libpglite.a` + `pglite-runtime.tar`).

## API

| Type | Purpose |
|---|---|
| `PGlite` | Main entrypoint. `open` / `open_with_options` / `open_multi_process`. `exec`, `query`, `transaction`, `listen`, `live_query`, `copy_in`, `copy_out`, `unix_uri`, `dump_data_dir`, `close`. |
| `PGliteOptions` | username, database, `relaxed_durability`, `start_params`, `locale_provider`. |
| `Transaction` | `exec` / `query` / `commit` / `rollback`. Rollback on drop if not committed. |
| `Row` / `Column` | Postgres-protocol-shaped row accessors. `row.get::<T, _>(idx)`, `row.try_get`. |
| `LiveQuery` | Reactive query handle that re-runs on subscribed table changes. |
| `MultiProcessOptions` | `extra_connections`, postmaster tuning, ready timeout. |
| `Replica` / `ReplicaConfig` | Logical-replication consumer over a unix socket. |
| `Error` | Flat `thiserror` enum. `Database` carries `sqlstate`. `AlreadyOpen`, `Closed`, `Boot`, `PoolExhausted`, `ReplicaHalted`, etc. |

## Contributing

We really appreciate any contribution — issues and pull requests are always welcome.

## License

[MIT](LICENSE). PostgreSQL itself is under the [PostgreSQL License](https://github.com/electric-sql/postgres-pglite/blob/main/COPYRIGHT).
