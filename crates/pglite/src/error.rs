use fallible_iterator::FallibleIterator;
use postgres_protocol::message::backend::ErrorFields;

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
    #[error(
        "PGlite cannot reopen after close within the same process; spawn a new process to reopen"
    )]
    ReopenUnsupported,
    #[error("the PGlite instance is closed")]
    Closed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("boot failed: {0}")]
    Boot(String),
    #[error("postmaster failed to start: {0}")]
    PostmasterStart(String),
    #[error("connection pool exhausted: no connection became free within the acquire timeout")]
    PoolExhausted,
    #[error("replica config error: {0}")]
    ReplicaConfig(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("replica halted: {0}")]
    ReplicaHalted(String),
    #[error("invalid lsn: {0}")]
    Lsn(String),
}

impl Error {
    pub(crate) fn from_error_fields(mut fields: ErrorFields<'_>) -> Error {
        let mut sqlstate = String::new();
        let mut message = String::new();
        let mut detail = None;
        let mut hint = None;
        while let Ok(Some(field)) = fields.next() {
            match field.type_() {
                b'C' => sqlstate = String::from_utf8_lossy(field.value_bytes()).into_owned(),
                b'M' => message = String::from_utf8_lossy(field.value_bytes()).into_owned(),
                b'D' => detail = Some(String::from_utf8_lossy(field.value_bytes()).into_owned()),
                b'H' => hint = Some(String::from_utf8_lossy(field.value_bytes()).into_owned()),
                _ => {}
            }
        }
        Error::Database {
            sqlstate,
            message,
            detail,
            hint,
        }
    }
}
