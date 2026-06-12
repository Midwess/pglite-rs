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
    #[error("the PGlite instance is closed")]
    Closed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("boot failed: {0}")]
    Boot(String),
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
        Error::Database { sqlstate, message, detail, hint }
    }
}
