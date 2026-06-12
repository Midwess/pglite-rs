//! In-process PostgreSQL for Rust — embedded like SQLite, full Postgres SQL,
//! async on any runtime.
//!
//! Built on [postgres-pglite](https://github.com/electric-sql/postgres-pglite),
//! the PostgreSQL fork powering [PGlite](https://pglite.dev/), compiled natively
//! and linked straight into your binary. No server, no Docker, no install step.
//!
//! The package is named `pglite-rs`; the library imports as `pglite`:
//!
//! ```no_run
//! use pglite::PGlite;
//!
//! # async fn demo() -> Result<(), pglite::Error> {
//! let db = PGlite::open("./mydata").await?;
//! db.exec("CREATE TABLE users (id serial PRIMARY KEY, name text)").await?;
//!
//! let rows = db.query("SELECT id, name FROM users WHERE id > $1", &[&0i32]).await?;
//! let name: &str = rows[0].get(1)?;
//!
//! let tx = db.transaction().await?;
//! tx.exec("INSERT INTO users (name) VALUES ('alice')").await?;
//! tx.commit().await?;
//!
//! db.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Runtime-agnostic: futures work on tokio, smol, async-std, or plain
//! `futures::executor::block_on`. See the README for Cargo features
//! (`pgvector`, `pgcrypto`, `icu`, `multiple-process`, `socket`, `replica`)
//! and ORM integration over a unix socket.

#[cfg(all(windows, feature = "socket"))]
compile_error!("the `socket` feature is unix-only (unix-socket gateway)");
#[cfg(all(windows, feature = "multiple-process"))]
compile_error!("the `multiple-process` feature is unix-only (child postmaster over unix sockets)");

mod db;
mod engine;
mod error;
mod live;
#[cfg(feature = "multiple-process")]
mod multiple_process;
#[cfg(feature = "replica")]
mod replica;
mod row;
#[cfg(feature = "socket")]
mod socket;
mod transaction;

pub use db::{LocaleProvider, PGlite, PGliteOptions};
pub use error::Error;
pub use live::LiveQuery;
#[cfg(feature = "multiple-process")]
pub use multiple_process::MultiProcessOptions;
pub use postgres_types::{FromSql, ToSql};
#[cfg(feature = "replica")]
pub use replica::{CommittedTransaction, Lsn, Replica, ReplicaConfig, RowChange};
pub use row::{Column, Row};
#[cfg(feature = "socket")]
pub use socket::SocketGateway;
pub use transaction::Transaction;

pub(crate) static RUNTIME_TAR: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pglite-runtime.tar"));
