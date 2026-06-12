mod db;
mod engine;
mod error;
mod row;
mod transaction;

pub use db::PGlite;
pub use error::Error;
pub use postgres_types::{FromSql, ToSql};
pub use row::{Column, Row};
pub use transaction::Transaction;

pub(crate) static RUNTIME_TAR: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pglite-runtime.tar"));
