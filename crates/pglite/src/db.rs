use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use bytes::BytesMut;
use fallible_iterator::FallibleIterator;
use futures::channel::oneshot;
use futures::lock::Mutex;
use postgres_protocol::message::backend::Message;
use postgres_protocol::message::frontend;
use postgres_types::{IsNull, ToSql, Type};

use crate::engine::{Engine, EngineCommand};
use crate::error::Error;
use crate::row::{Column, Row};

static OPEN: AtomicBool = AtomicBool::new(false);
static BOOTED: AtomicBool = AtomicBool::new(false);

struct CloseOnDrop {
    cmd_tx: mpsc::Sender<EngineCommand>,
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let (reply, _rx) = oneshot::channel();
        let _ = self.cmd_tx.send(EngineCommand::Close { reply });
        OPEN.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub struct PGlite {
    cmd_tx: mpsc::Sender<EngineCommand>,
    _handle: Arc<JoinHandle<()>>,
    tx_lock: Arc<Mutex<()>>,
    _temp_dir: Option<Arc<TempDataDir>>,
    _close: Arc<CloseOnDrop>,
}

struct TempDataDir(std::path::PathBuf);

impl Drop for TempDataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl PGlite {
    pub async fn open(data_dir: impl AsRef<Path>) -> Result<PGlite, Error> {
        Self::open_inner(data_dir.as_ref().to_path_buf(), None).await
    }

    pub async fn open_temp() -> Result<PGlite, Error> {
        let dir = std::env::temp_dir().join(format!(
            "pglite-temp-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Self::open_inner(dir.clone(), Some(Arc::new(TempDataDir(dir)))).await
    }

    async fn open_inner(
        data_dir: std::path::PathBuf,
        temp_dir: Option<Arc<TempDataDir>>,
    ) -> Result<PGlite, Error> {
        if OPEN.swap(true, Ordering::SeqCst) {
            return Err(Error::AlreadyOpen);
        }
        if BOOTED.swap(true, Ordering::SeqCst) {
            OPEN.store(false, Ordering::SeqCst);
            return Err(Error::ReopenUnsupported);
        }
        let (cmd_tx, handle, boot_rx) = Engine::spawn(data_dir);
        match boot_rx.await {
            Ok(Ok(())) => Ok(PGlite {
                _close: Arc::new(CloseOnDrop {
                    cmd_tx: cmd_tx.clone(),
                }),
                cmd_tx,
                _handle: Arc::new(handle),
                tx_lock: Arc::new(Mutex::new(())),
                _temp_dir: temp_dir,
            }),
            Ok(Err(e)) => {
                OPEN.store(false, Ordering::SeqCst);
                Err(e)
            }
            Err(_) => {
                OPEN.store(false, Ordering::SeqCst);
                Err(Error::Boot("engine thread died during boot".into()))
            }
        }
    }

    pub async fn exec(&self, sql: &str) -> Result<(), Error> {
        let _guard = self.tx_lock.lock().await;
        self.exec_unlocked(sql).await
    }

    pub async fn query(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        let _guard = self.tx_lock.lock().await;
        self.query_unlocked(sql, params).await
    }

    pub async fn transaction(&self) -> Result<crate::transaction::Transaction<'_>, Error> {
        crate::transaction::Transaction::begin(self).await
    }

    pub async fn close(self) -> Result<(), Error> {
        let _guard = self.tx_lock.lock().await;
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::Close { reply })
            .map_err(|_| Error::Closed)?;
        let _ = rx.await;
        OPEN.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub(crate) async fn exec_unlocked(&self, sql: &str) -> Result<(), Error> {
        let mut wire = BytesMut::new();
        frontend::query(sql, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        let response = self.roundtrip(wire.to_vec()).await?;
        Self::check_errors(&response)?;
        Ok(())
    }

    pub(crate) async fn query_unlocked(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        let mut wire = BytesMut::new();
        frontend::parse("", sql, std::iter::empty(), &mut wire)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        frontend::describe(b'S', "", &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        frontend::sync(&mut wire);

        let response = self.roundtrip(wire.to_vec()).await?;
        Self::check_errors(&response)?;
        let (param_types, columns) = Self::parse_describe(&response)?;
        if param_types.len() != params.len() {
            return Err(Error::Protocol(format!(
                "statement takes {} parameters, {} given",
                param_types.len(),
                params.len()
            )));
        }

        let mut wire = BytesMut::new();
        let values: Vec<(&(dyn ToSql + Sync), Type)> = params
            .iter()
            .copied()
            .zip(param_types.iter().cloned())
            .collect();
        frontend::bind(
            "",
            "",
            Some(1),
            values.iter(),
            |(value, ty), buf| match value.to_sql_checked(ty, buf) {
                Ok(IsNull::No) => Ok(postgres_protocol::IsNull::No),
                Ok(IsNull::Yes) => Ok(postgres_protocol::IsNull::Yes),
                Err(e) => Err(e),
            },
            Some(1),
            &mut wire,
        )
        .map_err(|e| match e {
            frontend::BindError::Conversion(e) => Error::Protocol(e.to_string()),
            frontend::BindError::Serialization(e) => Error::Protocol(e.to_string()),
        })?;
        frontend::execute("", 0, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        frontend::sync(&mut wire);

        let response = self.roundtrip(wire.to_vec()).await?;
        Self::check_errors(&response)?;

        let columns = Arc::new(columns);
        let mut rows = Vec::new();
        let mut buf = BytesMut::from(&response[..]);
        while let Some(message) =
            Message::parse(&mut buf).map_err(|e| Error::Protocol(e.to_string()))?
        {
            if let Message::DataRow(body) = message {
                rows.push(Row::new(columns.clone(), body)?);
            }
        }
        Ok(rows)
    }

    pub(crate) async fn lock_for_transaction(&self) -> futures::lock::MutexGuard<'_, ()> {
        self.tx_lock.lock().await
    }

    pub(crate) fn rollback_fire_and_forget(&self) {
        let mut wire = BytesMut::new();
        if frontend::query("ROLLBACK", &mut wire).is_ok() {
            let (reply, _rx) = oneshot::channel();
            let _ = self.cmd_tx.send(EngineCommand::Exec {
                wire: wire.to_vec(),
                reply,
            });
        }
    }

    async fn roundtrip(&self, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(EngineCommand::Exec { wire, reply })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    fn check_errors(response: &[u8]) -> Result<(), Error> {
        let mut buf = BytesMut::from(response);
        while let Some(message) =
            Message::parse(&mut buf).map_err(|e| Error::Protocol(e.to_string()))?
        {
            if let Message::ErrorResponse(body) = message {
                return Err(Error::from_error_fields(body.fields()));
            }
        }
        Ok(())
    }

    fn parse_describe(response: &[u8]) -> Result<(Vec<Type>, Vec<Column>), Error> {
        let mut param_types = Vec::new();
        let mut columns = Vec::new();
        let mut buf = BytesMut::from(response);
        while let Some(message) =
            Message::parse(&mut buf).map_err(|e| Error::Protocol(e.to_string()))?
        {
            match message {
                Message::ParameterDescription(body) => {
                    param_types = body
                        .parameters()
                        .map(|oid| Ok(Column::type_from_oid(oid)))
                        .collect()
                        .map_err(|e: std::io::Error| Error::Protocol(e.to_string()))?;
                }
                Message::RowDescription(body) => {
                    columns = body
                        .fields()
                        .map(|f| {
                            Ok(Column::new(
                                f.name().to_string(),
                                Column::type_from_oid(f.type_oid()),
                            ))
                        })
                        .collect()
                        .map_err(|e: std::io::Error| Error::Protocol(e.to_string()))?;
                }
                _ => {}
            }
        }
        Ok((param_types, columns))
    }
}
