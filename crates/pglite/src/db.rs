use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

pub(crate) struct CloseOnDrop {
    cmd_tx: mpsc::Sender<EngineCommand>,
    handle: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let (reply, _rx) = oneshot::channel();
        let _ = self.cmd_tx.send(EngineCommand::Close { reply });
        if let Some(handle) = self.handle.lock().unwrap().take() {
            let _ = handle.join();
        }
        OPEN.store(false, Ordering::SeqCst);
    }
}

pub(crate) type NotificationCallback = Box<dyn Fn(&str) + Send + Sync>;
pub(crate) type ListenerMap = HashMap<String, Vec<(u64, NotificationCallback)>>;

#[derive(Clone)]
pub(crate) enum Backend {
    InProcess {
        cmd_tx: mpsc::Sender<EngineCommand>,
        _close: Arc<CloseOnDrop>,
    },
    #[cfg(feature = "multiple-process")]
    MultiProcess(Arc<crate::multiple_process::pool::Pool>),
}

impl Backend {
    pub(crate) async fn roundtrip(&self, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        match self {
            Backend::InProcess { cmd_tx, .. } => {
                let (reply, rx) = oneshot::channel();
                cmd_tx
                    .send(EngineCommand::Exec { wire, reply })
                    .map_err(|_| Error::Closed)?;
                rx.await.map_err(|_| Error::Closed)?
            }
            #[cfg(feature = "multiple-process")]
            Backend::MultiProcess(pool) => pool.roundtrip(wire).await,
        }
    }

    pub(crate) fn fire_and_forget(&self, wire: Vec<u8>) {
        match self {
            Backend::InProcess { cmd_tx, .. } => {
                let (reply, _rx) = oneshot::channel();
                let _ = cmd_tx.send(EngineCommand::Exec { wire, reply });
            }
            #[cfg(feature = "multiple-process")]
            Backend::MultiProcess(pool) => pool.fire_and_forget(wire),
        }
    }

    pub(crate) async fn close(&self) -> Result<(), Error> {
        match self {
            Backend::InProcess { cmd_tx, .. } => {
                let (reply, rx) = oneshot::channel();
                cmd_tx
                    .send(EngineCommand::Close { reply })
                    .map_err(|_| Error::Closed)?;
                let _ = rx.await;
                OPEN.store(false, Ordering::SeqCst);
                Ok(())
            }
            #[cfg(feature = "multiple-process")]
            Backend::MultiProcess(pool) => {
                pool.server.shutdown();
                Ok(())
            }
        }
    }

    pub(crate) fn is_multi_process(&self) -> bool {
        match self {
            Backend::InProcess { .. } => false,
            #[cfg(feature = "multiple-process")]
            Backend::MultiProcess(_) => true,
        }
    }
}

#[derive(Clone)]
pub struct PGlite {
    backend: Backend,
    data_dir: Arc<std::path::PathBuf>,
    tx_lock: Arc<Mutex<()>>,
    listeners: Arc<std::sync::Mutex<ListenerMap>>,
    next_listener: Arc<AtomicU64>,
    live_triggers: Arc<std::sync::Mutex<HashMap<(u32, u32), usize>>>,
    _temp_dir: Option<Arc<TempDataDir>>,
}

impl PGlite {
    #[cfg(feature = "multiple-process")]
    pub(crate) fn assemble(backend: Backend, data_dir: std::path::PathBuf) -> PGlite {
        PGlite {
            backend,
            data_dir: Arc::new(data_dir),
            tx_lock: Arc::new(Mutex::new(())),
            listeners: Arc::new(std::sync::Mutex::new(HashMap::new())),
            next_listener: Arc::new(AtomicU64::new(0)),
            live_triggers: Arc::new(std::sync::Mutex::new(HashMap::new())),
            _temp_dir: None,
        }
    }

    pub(crate) async fn serial_guard(&self) -> Option<futures::lock::MutexGuard<'_, ()>> {
        if self.backend.is_multi_process() {
            None
        } else {
            Some(self.tx_lock.lock().await)
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Via<'v> {
    Backend(std::marker::PhantomData<&'v ()>),
    #[cfg(feature = "multiple-process")]
    Pin(&'v crate::multiple_process::pool::PinnedConn),
}

impl Via<'_> {
    pub(crate) fn backend() -> Via<'static> {
        Via::Backend(std::marker::PhantomData)
    }
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
            Ok(Ok(())) => {
                let db = PGlite {
                    backend: Backend::InProcess {
                        _close: Arc::new(CloseOnDrop {
                            cmd_tx: cmd_tx.clone(),
                            handle: std::sync::Mutex::new(Some(handle)),
                        }),
                        cmd_tx,
                    },
                    data_dir: Arc::new(data_dir),
                    tx_lock: Arc::new(Mutex::new(())),
                    listeners: Arc::new(std::sync::Mutex::new(HashMap::new())),
                    next_listener: Arc::new(AtomicU64::new(0)),
                    live_triggers: Arc::new(std::sync::Mutex::new(HashMap::new())),
                    _temp_dir: temp_dir,
                };
                db.sweep_live_views().await?;
                Ok(db)
            }
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
        let _guard = self.serial_guard().await;
        self.exec_unlocked(sql).await
    }

    pub async fn query(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        let _guard = self.serial_guard().await;
        self.query_unlocked(sql, params).await
    }

    pub async fn transaction(&self) -> Result<crate::transaction::Transaction<'_>, Error> {
        crate::transaction::Transaction::begin(self).await
    }

    pub async fn listen<F>(&self, channel: &str, callback: F) -> Result<u64, Error>
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        let _guard = self.tx_lock.lock().await;
        let token = self.next_listener.fetch_add(1, Ordering::SeqCst);
        self.listeners
            .lock()
            .unwrap()
            .entry(channel.to_lowercase())
            .or_default()
            .push((token, Box::new(callback)));
        let sql = format!("LISTEN \"{}\"", channel.replace('"', "\"\""));
        self.notify_command(&sql).await?;
        Ok(token)
    }

    pub async fn unlisten_token(&self, channel: &str, token: u64) -> Result<(), Error> {
        let _guard = self.tx_lock.lock().await;
        let empty = {
            let mut listeners = self.listeners.lock().unwrap();
            let key = channel.to_lowercase();
            if let Some(entries) = listeners.get_mut(&key) {
                entries.retain(|(t, _)| *t != token);
                if entries.is_empty() {
                    listeners.remove(&key);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if empty {
            self.notify_command(&format!("UNLISTEN \"{}\"", channel.replace('"', "\"\"")))
                .await?;
        }
        Ok(())
    }

    pub async fn copy_in(&self, sql: &str, data: &[u8]) -> Result<(), Error> {
        let _guard = self.serial_guard().await;
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
        let _guard = self.serial_guard().await;
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
        self.notify_command(&format!("UNLISTEN \"{}\"", channel.replace('"', "\"\"")))
            .await?;
        self.listeners
            .lock()
            .unwrap()
            .remove(&channel.to_lowercase());
        Ok(())
    }

    pub async fn close(self) -> Result<(), Error> {
        let _guard = self.tx_lock.lock().await;
        self.backend.close().await
    }

    pub(crate) async fn notify_command(&self, sql: &str) -> Result<(), Error> {
        #[cfg(feature = "multiple-process")]
        if let Backend::MultiProcess(pool) = &self.backend {
            return pool.notify_conn(&self.listeners)?.command(sql).await;
        }
        self.exec_unlocked(sql).await
    }

    pub(crate) async fn exec_unlocked(&self, sql: &str) -> Result<(), Error> {
        self.exec_via(Via::backend(), sql).await
    }

    pub(crate) async fn exec_via(&self, via: Via<'_>, sql: &str) -> Result<(), Error> {
        let mut wire = BytesMut::new();
        frontend::query(sql, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        let response = self.route(via, wire.to_vec()).await?;
        self.process_response(&response)?;
        Ok(())
    }

    pub(crate) async fn route(&self, via: Via<'_>, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        match via {
            Via::Backend(_) => self.backend.roundtrip(wire).await,
            #[cfg(feature = "multiple-process")]
            Via::Pin(pin) => pin.roundtrip(wire).await,
        }
    }

    pub(crate) async fn query_unlocked(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        self.query_via(Via::backend(), sql, params).await
    }

    pub(crate) async fn query_via(
        &self,
        via: Via<'_>,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error> {
        let mut wire = BytesMut::new();
        frontend::parse("", sql, std::iter::empty(), &mut wire)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        frontend::describe(b'S', "", &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        frontend::sync(&mut wire);

        let response = self.route(via, wire.to_vec()).await?;
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

        let response = self.route(via, wire.to_vec()).await?;
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

    pub(crate) fn live_triggers(&self) -> &std::sync::Mutex<HashMap<(u32, u32), usize>> {
        &self.live_triggers
    }

    pub(crate) async fn describe_param_types(&self, sql: &str) -> Result<Vec<Type>, Error> {
        let mut wire = BytesMut::new();
        frontend::parse("", sql, std::iter::empty(), &mut wire)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        frontend::describe(b'S', "", &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        frontend::sync(&mut wire);
        let response = self.roundtrip(wire.to_vec()).await?;
        self.process_response(&response)?;
        Ok(Self::parse_describe(&response)?.0)
    }

    pub(crate) async fn query_with_types(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
        types: &[Type],
    ) -> Result<Vec<Row>, Error> {
        let mut wire = BytesMut::new();
        frontend::parse("", sql, types.iter().map(|t| t.oid()), &mut wire)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        frontend::describe(b'S', "", &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        let values: Vec<(&(dyn ToSql + Sync), Type)> =
            params.iter().copied().zip(types.iter().cloned()).collect();
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
        let (_, columns) = Self::parse_describe(&response)?;
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

    pub(crate) async fn format_literals(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<String, Error> {
        if params.is_empty() {
            return Ok(sql.to_string());
        }
        let param_types = self.describe_param_types(sql).await?;
        if param_types.len() != params.len() {
            return Err(Error::Protocol(format!(
                "statement takes {} parameters, {} given",
                param_types.len(),
                params.len()
            )));
        }

        let mut subbed = String::with_capacity(sql.len());
        let mut chars = sql.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '$' && chars.peek().is_some_and(|n| n.is_ascii_digit()) {
                subbed.push('%');
                while let Some(d) = chars.peek().copied().filter(|d| d.is_ascii_digit()) {
                    subbed.push(d);
                    chars.next();
                }
                subbed.push('L');
            } else {
                subbed.push(c);
            }
        }

        let placeholders: Vec<String> = (0..params.len()).map(|i| format!("${}", i + 2)).collect();
        let select = format!("SELECT format($1, {})", placeholders.join(", "));
        let mut all_types = vec![Type::TEXT];
        all_types.extend(param_types);
        let mut all_params: Vec<&(dyn ToSql + Sync)> = vec![&subbed];
        all_params.extend_from_slice(params);

        let rows = self
            .query_with_types(&select, &all_params, &all_types)
            .await?;
        Ok(rows[0].get::<&str>(0)?.to_string())
    }

    pub(crate) async fn lock_for_transaction(&self) -> futures::lock::MutexGuard<'_, ()> {
        self.tx_lock.lock().await
    }

    pub(crate) fn rollback_fire_and_forget(&self) {
        if let Some(wire) = Self::rollback_wire() {
            self.backend.fire_and_forget(wire);
        }
    }

    pub(crate) fn rollback_wire() -> Option<Vec<u8>> {
        let mut wire = BytesMut::new();
        frontend::query("ROLLBACK", &mut wire).ok()?;
        Some(wire.to_vec())
    }

    #[cfg(any(feature = "multiple-process", feature = "socket"))]
    pub(crate) fn backend(&self) -> &Backend {
        &self.backend
    }

    async fn roundtrip(&self, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        self.backend.roundtrip(wire).await
    }

    #[cfg(feature = "socket")]
    pub(crate) fn dispatch_notifications(&self, response: &[u8]) {
        let mut buf = BytesMut::from(response);
        while let Ok(Some(message)) = Message::parse(&mut buf) {
            if let Message::NotificationResponse(body) = message {
                let (Ok(channel), Ok(payload)) = (body.channel(), body.message()) else {
                    continue;
                };
                let channel = channel.to_lowercase();
                if let Some(callbacks) = self.listeners.lock().unwrap().get(&channel) {
                    for (_, callback) in callbacks {
                        callback(payload);
                    }
                }
            }
        }
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
                        for (_, callback) in callbacks {
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
