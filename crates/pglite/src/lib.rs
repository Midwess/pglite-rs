mod engine;
mod error;

pub use error::Error;

pub(crate) static RUNTIME_TAR: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pglite-runtime.tar"));
