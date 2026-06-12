use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use futures::channel::oneshot;
use postgres_protocol::message::backend::Message;
use postgres_protocol::message::frontend;

use super::pool::connect_and_handshake;
use crate::db::ListenerMap;
use crate::error::Error;

pub(crate) struct NotifyConn {
    writer: Mutex<UnixStream>,
    pending: Arc<Mutex<VecDeque<oneshot::Sender<Option<Error>>>>>,
}

impl NotifyConn {
    pub(crate) fn start(
        sock_path: &Path,
        username: &str,
        database: &str,
        listeners: Arc<Mutex<ListenerMap>>,
    ) -> Result<Arc<NotifyConn>, Error> {
        let stream = connect_and_handshake(sock_path, username, database)?;
        let reader = stream.try_clone().map_err(Error::Io)?;
        let conn = Arc::new(NotifyConn {
            writer: Mutex::new(stream),
            pending: Arc::new(Mutex::new(VecDeque::new())),
        });
        let pending = conn.pending.clone();
        std::thread::Builder::new()
            .name("pglite-notify".into())
            .spawn(move || Self::reader_loop(reader, pending, listeners))
            .map_err(Error::Io)?;
        Ok(conn)
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.writer.lock().unwrap().shutdown(Shutdown::Both);
    }

    pub(crate) async fn command(&self, sql: &str) -> Result<(), Error> {
        let mut wire = BytesMut::new();
        frontend::query(sql, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        {
            let mut writer = self.writer.lock().unwrap();
            self.pending.lock().unwrap().push_back(tx);
            writer.write_all(&wire).map_err(Error::Io)?;
        }
        match rx.await {
            Ok(None) => Ok(()),
            Ok(Some(err)) => Err(err),
            Err(_) => Err(Error::Closed),
        }
    }

    fn reader_loop(
        mut stream: UnixStream,
        pending: Arc<Mutex<VecDeque<oneshot::Sender<Option<Error>>>>>,
        listeners: Arc<Mutex<ListenerMap>>,
    ) {
        let mut staged: Option<Error> = None;
        loop {
            let mut header = [0u8; 5];
            if stream.read_exact(&mut header).is_err() {
                break;
            }
            let len = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
            let mut frame = header.to_vec();
            frame.resize(5 + len.saturating_sub(4), 0);
            if stream.read_exact(&mut frame[5..]).is_err() {
                break;
            }
            let mut buf = BytesMut::from(&frame[..]);
            match Message::parse(&mut buf) {
                Ok(Some(Message::NotificationResponse(body))) => {
                    let channel = match body.channel() {
                        Ok(c) => c.to_lowercase(),
                        Err(_) => continue,
                    };
                    let payload = match body.message() {
                        Ok(p) => p.to_string(),
                        Err(_) => continue,
                    };
                    if let Some(callbacks) = listeners.lock().unwrap().get(&channel) {
                        for (_, callback) in callbacks {
                            callback(&payload);
                        }
                    }
                }
                Ok(Some(Message::ErrorResponse(body))) => {
                    staged = Some(Error::from_error_fields(body.fields()));
                }
                Ok(Some(Message::ReadyForQuery(_))) => {
                    if let Some(tx) = pending.lock().unwrap().pop_front() {
                        let _ = tx.send(staged.take());
                    }
                }
                _ => {}
            }
        }
        let mut pending = pending.lock().unwrap();
        while let Some(tx) = pending.pop_front() {
            let _ = tx.send(Some(Error::Closed));
        }
    }
}
