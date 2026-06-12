use std::collections::HashMap;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocaleProvider {
    #[default]
    Libc,
    Icu,
}

#[derive(Clone, Debug)]
pub struct PGliteOptions {
    pub username: String,
    pub database: String,
    pub relaxed_durability: bool,
    pub start_params: Vec<String>,
    pub locale_provider: LocaleProvider,
}

impl Default for PGliteOptions {
    fn default() -> PGliteOptions {
        PGliteOptions {
            username: "postgres".into(),
            database: "postgres".into(),
            relaxed_durability: false,
            start_params: Vec::new(),
            locale_provider: LocaleProvider::default(),
        }
    }
}
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

type NotificationCallback = Box<dyn Fn(&str) + Send + Sync>;
type ListenerMap = HashMap<String, Vec<NotificationCallback>>;

#[derive(Clone)]
pub struct PGlite {
    cmd_tx: mpsc::Sender<EngineCommand>,
    data_dir: Arc<std::path::PathBuf>,
    _handle: Arc<JoinHandle<()>>,
    tx_lock: Arc<Mutex<()>>,
    listeners: Arc<std::sync::Mutex<ListenerMap>>,
    live_triggers: Arc<std::sync::Mutex<std::collections::HashSet<(u32, u32)>>>,
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
        Self::open_inner(
            data_dir.as_ref().to_path_buf(),
            None,
            PGliteOptions::default(),
        )
        .await
    }

    pub async fn open_with(
        data_dir: impl AsRef<Path>,
        options: PGliteOptions,
    ) -> Result<PGlite, Error> {
        Self::open_inner(data_dir.as_ref().to_path_buf(), None, options).await
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
        Self::open_inner(
            dir.clone(),
            Some(Arc::new(TempDataDir(dir))),
            PGliteOptions::default(),
        )
        .await
    }

    pub async fn open_temp_with(options: PGliteOptions) -> Result<PGlite, Error> {
        let dir = std::env::temp_dir().join(format!(
            "pglite-temp-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Self::open_inner(dir.clone(), Some(Arc::new(TempDataDir(dir))), options).await
    }

    async fn open_inner(
        data_dir: std::path::PathBuf,
        temp_dir: Option<Arc<TempDataDir>>,
        options: PGliteOptions,
    ) -> Result<PGlite, Error> {
        if OPEN.swap(true, Ordering::SeqCst) {
            return Err(Error::AlreadyOpen);
        }
        if BOOTED.swap(true, Ordering::SeqCst) {
            OPEN.store(false, Ordering::SeqCst);
            return Err(Error::ReopenUnsupported);
        }
        let (cmd_tx, handle, boot_rx) = Engine::spawn(data_dir.clone(), options);
        match boot_rx.await {
            Ok(Ok(())) => Ok(PGlite {
                _close: Arc::new(CloseOnDrop {
                    cmd_tx: cmd_tx.clone(),
                }),
                cmd_tx,
                data_dir: Arc::new(data_dir),
                _handle: Arc::new(handle),
                tx_lock: Arc::new(Mutex::new(())),
                listeners: Arc::new(std::sync::Mutex::new(HashMap::new())),
                live_triggers: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
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

    pub async fn listen<F>(&self, channel: &str, callback: F) -> Result<(), Error>
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        let _guard = self.tx_lock.lock().await;
        self.listeners
            .lock()
            .unwrap()
            .entry(channel.to_lowercase())
            .or_default()
            .push(Box::new(callback));
        self.exec_unlocked(&format!("LISTEN \"{}\"", channel.replace('"', "\"\"")))
            .await
    }

    pub async fn copy_in(&self, sql: &str, data: &[u8]) -> Result<(), Error> {
        let _guard = self.tx_lock.lock().await;
        let mut wire = BytesMut::new();
        frontend::query(sql, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        let mut wire = wire.to_vec();
        wire.push(b'd');
        wire.extend_from_slice(&((4 + data.len()) as u32).to_be_bytes());
        wire.extend_from_slice(data);
        wire.push(b'c');
        wire.extend_from_slice(&4u32.to_be_bytes());

        let response = self.roundtrip(wire).await?;
        self.process_response(&response)?;
        if !Self::has_message(&response, |m| matches!(m, Message::CopyInResponse(_)))? {
            return Err(Error::Protocol("statement did not initiate COPY IN".into()));
        }
        Ok(())
    }

    pub async fn copy_out(&self, sql: &str) -> Result<Vec<u8>, Error> {
        let _guard = self.tx_lock.lock().await;
        let mut wire = BytesMut::new();
        frontend::query(sql, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        let response = self.roundtrip(wire.to_vec()).await?;
        self.process_response(&response)?;
        if !Self::has_message(&response, |m| matches!(m, Message::CopyOutResponse(_)))? {
            return Err(Error::Protocol(
                "statement did not initiate COPY OUT".into(),
            ));
        }

        let mut out = Vec::new();
        let mut buf = BytesMut::from(&response[..]);
        while let Some(message) =
            Message::parse(&mut buf).map_err(|e| Error::Protocol(e.to_string()))?
        {
            if let Message::CopyData(body) = message {
                out.extend_from_slice(body.data());
            }
        }
        Ok(out)
    }

    pub async fn dump_data_dir(&self, dest: impl AsRef<Path>) -> Result<(), Error> {
        let _guard = self.tx_lock.lock().await;
        self.exec_unlocked("CHECKPOINT").await?;
        let file = std::fs::File::create(dest.as_ref())?;
        let mut builder = tar::Builder::new(file);
        builder.append_dir_all(".", self.data_dir.as_ref())?;
        builder.finish()?;
        Ok(())
    }

    pub fn restore_data_dir(
        tar_path: impl AsRef<Path>,
        data_dir: impl AsRef<Path>,
    ) -> Result<(), Error> {
        let file = std::fs::File::open(tar_path.as_ref())?;
        std::fs::create_dir_all(data_dir.as_ref())?;
        let mut archive = tar::Archive::new(file);
        archive.unpack(data_dir.as_ref())?;
        Ok(())
    }

    fn has_message<F>(response: &[u8], pred: F) -> Result<bool, Error>
    where
        F: Fn(&Message) -> bool,
    {
        let mut buf = BytesMut::from(response);
        while let Some(message) =
            Message::parse(&mut buf).map_err(|e| Error::Protocol(e.to_string()))?
        {
            if pred(&message) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn unlisten(&self, channel: &str) -> Result<(), Error> {
        let _guard = self.tx_lock.lock().await;
        self.exec_unlocked(&format!("UNLISTEN \"{}\"", channel.replace('"', "\"\"")))
            .await?;
        self.listeners
            .lock()
            .unwrap()
            .remove(&channel.to_lowercase());
        Ok(())
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
        self.process_response(&response)?;
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
        self.process_response(&response)?;
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
        self.process_response(&response)?;

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

    pub(crate) fn live_triggers(&self) -> &std::sync::Mutex<std::collections::HashSet<(u32, u32)>> {
        &self.live_triggers
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

    fn process_response(&self, response: &[u8]) -> Result<(), Error> {
        let mut buf = BytesMut::from(response);
        while let Some(message) =
            Message::parse(&mut buf).map_err(|e| Error::Protocol(e.to_string()))?
        {
            match message {
                Message::ErrorResponse(body) => {
                    return Err(Error::from_error_fields(body.fields()));
                }
                Message::NotificationResponse(body) => {
                    let channel = body
                        .channel()
                        .map_err(|e| Error::Protocol(e.to_string()))?
                        .to_lowercase();
                    let payload = body
                        .message()
                        .map_err(|e| Error::Protocol(e.to_string()))?
                        .to_string();
                    if let Some(callbacks) = self.listeners.lock().unwrap().get(&channel) {
                        for callback in callbacks {
                            callback(&payload);
                        }
                    }
                }
                _ => {}
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
