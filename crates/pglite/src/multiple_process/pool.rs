use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use futures::channel::oneshot;
use futures::FutureExt;

const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

use super::Server;
use crate::engine::Engine;
use crate::error::Error;

pub(crate) enum ConnCmd {
    Roundtrip {
        wire: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<u8>, Error>>,
    },
}

struct PoolState {
    idle: Vec<usize>,
    waiters: Vec<oneshot::Sender<usize>>,
}

pub(crate) struct Pool {
    pub(crate) server: Arc<Server>,
    conns: Vec<mpsc::Sender<ConnCmd>>,
    alive: Vec<Arc<AtomicBool>>,
    state: Mutex<PoolState>,
    pub(crate) credentials: (String, String),
    notify: Mutex<Option<Arc<super::notify::NotifyConn>>>,
    _threads: Vec<JoinHandle<()>>,
}

pub(crate) fn read_response(stream: &mut UnixStream) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    loop {
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).map_err(Error::Io)?;
        let len = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        let mut body = vec![0u8; len.saturating_sub(4)];
        stream.read_exact(&mut body).map_err(Error::Io)?;
        out.extend_from_slice(&header);
        out.extend_from_slice(&body);
        if header[0] == b'Z' {
            return Ok(out);
        }
    }
}

pub(crate) fn connect_and_handshake(
    sock_path: &Path,
    username: &str,
    database: &str,
) -> Result<UnixStream, Error> {
    let mut stream = UnixStream::connect(sock_path).map_err(Error::Io)?;
    stream
        .write_all(&Engine::build_startup_packet(username, database))
        .map_err(Error::Io)?;
    let response = read_response(&mut stream)?;
    if response.first() == Some(&b'E') {
        return Err(Error::Protocol(format!(
            "server rejected connection: {}",
            String::from_utf8_lossy(&response)
        )));
    }
    Ok(stream)
}

impl Pool {
    pub(crate) fn start(
        server: Arc<Server>,
        size: usize,
        username: &str,
        database: &str,
    ) -> Result<Pool, Error> {
        let mut conns = Vec::with_capacity(size);
        let mut alive = Vec::with_capacity(size);
        let mut threads = Vec::with_capacity(size);
        for i in 0..size {
            let mut stream = connect_and_handshake(&server.sock_path, username, database)?;
            let (cmd_tx, cmd_rx) = mpsc::channel::<ConnCmd>();
            let flag = Arc::new(AtomicBool::new(true));
            let worker_flag = flag.clone();
            let sock_path = server.sock_path.clone();
            let user = username.to_string();
            let dbname = database.to_string();
            let handle = std::thread::Builder::new()
                .name(format!("pglite-pool-{i}"))
                .spawn(move || {
                    while let Ok(ConnCmd::Roundtrip { wire, reply }) = cmd_rx.recv() {
                        let result = stream
                            .write_all(&wire)
                            .map_err(Error::Io)
                            .and_then(|_| read_response(&mut stream));
                        let failed = result.is_err();
                        let _ = reply.send(result);
                        if failed {
                            match connect_and_handshake(&sock_path, &user, &dbname) {
                                Ok(fresh) => stream = fresh,
                                Err(_) => {
                                    worker_flag.store(false, Ordering::SeqCst);
                                    return;
                                }
                            }
                        }
                    }
                })
                .map_err(Error::Io)?;
            conns.push(cmd_tx);
            alive.push(flag);
            threads.push(handle);
        }
        Ok(Pool {
            server,
            conns,
            alive,
            state: Mutex::new(PoolState {
                idle: (0..size).collect(),
                waiters: Vec::new(),
            }),
            credentials: (username.to_string(), database.to_string()),
            notify: Mutex::new(None),
            _threads: threads,
        })
    }

    async fn acquire(&self) -> Result<usize, Error> {
        loop {
            let rx = {
                let mut state = self.state.lock().unwrap();
                while let Some(idx) = state.idle.pop() {
                    if self.alive[idx].load(Ordering::SeqCst) {
                        return Ok(idx);
                    }
                }
                let (tx, rx) = oneshot::channel();
                state.waiters.push(tx);
                rx
            };
            let (deadline_tx, deadline_rx) = oneshot::channel::<()>();
            std::thread::spawn(move || {
                std::thread::sleep(ACQUIRE_TIMEOUT);
                let _ = deadline_tx.send(());
            });
            futures::select! {
                idx = rx.fuse() => {
                    if let Ok(idx) = idx {
                        if self.alive[idx].load(Ordering::SeqCst) {
                            return Ok(idx);
                        }
                    }
                }
                _ = deadline_rx.fuse() => return Err(Error::PoolExhausted),
            }
        }
    }

    pub(crate) fn release(&self, idx: usize) {
        if !self.alive[idx].load(Ordering::SeqCst) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        while let Some(waiter) = state.waiters.pop() {
            if waiter.send(idx).is_ok() {
                return;
            }
        }
        state.idle.push(idx);
    }

    pub(crate) async fn roundtrip_on(&self, idx: usize, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        let (reply, rx) = oneshot::channel();
        self.conns[idx]
            .send(ConnCmd::Roundtrip { wire, reply })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    pub(crate) async fn roundtrip(&self, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        let idx = self.acquire().await?;
        let result = self.roundtrip_on(idx, wire).await;
        self.release(idx);
        result
    }

    pub(crate) async fn checkout(self: &Arc<Self>) -> Result<PinnedConn, Error> {
        let idx = self.acquire().await?;
        Ok(PinnedConn {
            pool: self.clone(),
            idx,
        })
    }

    pub(crate) fn notify_conn(
        &self,
        listeners: &Arc<Mutex<crate::db::ListenerMap>>,
    ) -> Result<Arc<super::notify::NotifyConn>, Error> {
        let mut slot = self.notify.lock().unwrap();
        if let Some(conn) = slot.as_ref() {
            return Ok(conn.clone());
        }
        let conn = super::notify::NotifyConn::start(
            &self.server.sock_path,
            &self.credentials.0,
            &self.credentials.1,
            listeners.clone(),
        )?;
        *slot = Some(conn.clone());
        Ok(conn)
    }

    pub(crate) fn fire_and_forget(&self, wire: Vec<u8>) {
        let idx = {
            let mut state = self.state.lock().unwrap();
            state.idle.pop()
        };
        match idx {
            Some(idx) => {
                let (reply, _rx) = oneshot::channel();
                let _ = self.conns[idx].send(ConnCmd::Roundtrip { wire, reply });
                self.release(idx);
            }
            None => {
                let (reply, _rx) = oneshot::channel();
                let _ = self.conns[0].send(ConnCmd::Roundtrip { wire, reply });
            }
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        if let Some(conn) = self.notify.lock().unwrap().take() {
            conn.shutdown();
        }
    }
}

pub(crate) struct PinnedConn {
    pool: Arc<Pool>,
    idx: usize,
}

impl PinnedConn {
    pub(crate) async fn roundtrip(&self, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        self.pool.roundtrip_on(self.idx, wire).await
    }

    pub(crate) fn fire_and_forget(&self, wire: Vec<u8>) {
        let (reply, _rx) = oneshot::channel();
        let _ = self.pool.conns[self.idx].send(ConnCmd::Roundtrip { wire, reply });
    }
}

impl Drop for PinnedConn {
    fn drop(&mut self) {
        self.pool.release(self.idx);
    }
}
