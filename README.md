# pglite-rs

In-process PostgreSQL for Rust.

`pglite-rs` embeds the `postgres-pglite` engine in a Rust process. The crate
package is `pglite-rs`; the Rust library import is `pglite`.

Full docs: https://midwess.com/pglite-rs

## Install

```bash
cargo add pglite-rs
```

Use optional features when needed:

```bash
cargo add pglite-rs --features multiple-process,replica
```

## First query

```rust
use pglite::PGlite;

# async fn run() -> Result<(), pglite::Error> {
let db = PGlite::open("./data").await?;

db.exec("CREATE TABLE users (id serial PRIMARY KEY, name text)").await?;

let rows = db
    .query("SELECT id, name FROM users WHERE id > $1", &[&0i32])
    .await?;

let name: &str = rows[0].get(1)?;

db.close().await?;
# Ok(())
# }
```

## Why use it

- Real PostgreSQL SQL, types, MVCC, transactions, and wire-protocol rows.
- No external server in single-process mode.
- Async API that does not require a specific runtime.
- Unix socket URI for SQLx, SeaORM, Diesel, and other Postgres clients.
- Optional multi-process mode for concurrent sessions.
- Optional logical-replication consumer.

## Features

| Feature | Enables |
| --- | --- |
| `socket` | Default feature. Adds `PGlite::unix_uri()` on Unix. |
| `multiple-process` | Adds `PGlite::open_multi_process()` and `MultiProcessOptions`. |
| `replica` | Adds logical replication types and `Replica::start()`. |
| `pgvector` | Bundled pgvector extension. |
| `pgcrypto` | Bundled pgcrypto extension. |
| `icu` | ICU locale provider. |
| `full` | Enables all optional features. |

## Transactions

```rust
let tx = db.transaction().await?;
tx.exec("INSERT INTO users (name) VALUES ('alice')").await?;
tx.commit().await?;
```

If a transaction is dropped before `commit()` or `rollback()`, it rolls back.

## ORMs

Use the Unix socket URI:

```rust
let db = pglite::PGlite::open("./data").await?;
let uri = db.unix_uri().await?;

let pool = sqlx::postgres::PgPoolOptions::new()
    .max_connections(1)
    .connect(&uri)
    .await?;
```

In single-process mode, use pool size `1`. Use multi-process mode for real
concurrent ORM connections:

```rust
use pglite::{MultiProcessOptions, PGlite};

let db = PGlite::open_multi_process("./data", MultiProcessOptions::default()).await?;
let uri = db.unix_uri().await?;
```

## LISTEN / NOTIFY

```rust
let token = db
    .listen("changes", |payload| {
        println!("payload: {payload}");
    })
    .await?;

db.exec("NOTIFY changes, 'ready'").await?;
db.unlisten_token("changes", token).await?;
```

## Logical replication

```rust
use pglite::{PGlite, Replica, ReplicaConfig, SslMode};

let db = PGlite::open_multi_process("./replica", Default::default()).await?;

let replica = Replica::start(
    db.clone(),
    ReplicaConfig {
        host: "127.0.0.1".into(),
        port: 5432,
        user: "postgres".into(),
        password: "postgres".into(),
        database: "app".into(),
        publication: "app_pub".into(),
        slot_name: "app_slot".into(),
        sslmode: SslMode::Disable,
        ..Default::default()
    }
).await?;
```

## Extension linker flag

When using bundled extensions such as `pgvector`, `pgcrypto`, or `plpgsql`,
export symbols from the host binary:

```rust
// build.rs
fn main() {
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-Wl,-export_dynamic");

    #[cfg(not(target_os = "macos"))]
    println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
}
```

## License

MIT
