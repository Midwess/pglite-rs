#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error {sqlstate}: {message}")]
    Database {
        sqlstate: String,
        message: String,
        detail: Option<String>,
        hint: Option<String>,
    },
    #[error("a PGlite instance is already open in this process")]
    AlreadyOpen,
    #[error("the PGlite instance is closed")]
    Closed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("boot failed: {0}")]
    Boot(String),
}
