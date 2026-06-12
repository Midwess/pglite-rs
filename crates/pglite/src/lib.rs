mod db;
mod engine;
mod error;
mod live;
#[cfg(feature = "multiple-process")]
mod multiple_process;
mod row;
mod transaction;

pub use db::{LocaleProvider, PGlite, PGliteOptions};
pub use error::Error;
pub use live::LiveQuery;
#[cfg(feature = "multiple-process")]
pub use multiple_process::MultiProcessOptions;
pub use postgres_types::{FromSql, ToSql};
pub use row::{Column, Row};
pub use transaction::Transaction;

pub(crate) static RUNTIME_TAR: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pglite-runtime.tar"));
