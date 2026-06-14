use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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

#[derive(Clone, Copy)]
pub(crate) struct PoolConfig {
    pub(crate) min: usize,
    pub(crate) max: usize,
    pub(crate) idle_ttl: Duration,
}

struct Conn {
    cmd_tx: mpsc::Sender<ConnCmd>,
    alive: Arc<AtomicBool>,
    last_used: Instant,
    worker: Option<JoinHandle<()>>,
}

impl Conn {
    fn spawn(sock_path: &Path, username: &str, database: &str) -> Result<Conn, Error> {
        let mut stream = connect_and_handshake(sock_path, username, database)?;
        let (cmd_tx, cmd_rx) = mpsc::channel::<ConnCmd>();
        let alive = Arc::new(AtomicBool::new(true));
        let worker_flag = alive.clone();
        let sock_path = sock_path.to_path_buf();
        let user = username.to_string();
        let dbname = database.to_string();
        let worker = std::thread::Builder::new()
            .name("pglite-pool".into())
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
        Ok(Conn {
            cmd_tx,
            alive,
            last_used: Instant::now(),
            worker: Some(worker),
        })
    }

    fn send(&self, wire: Vec<u8>) -> Result<oneshot::Receiver<Result<Vec<u8>, Error>>, Error> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(ConnCmd::Roundtrip { wire, reply })
            .map_err(|_| Error::Closed)?;
        Ok(rx)
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn close(self) {
        let Conn { cmd_tx, worker, .. } = self;
        drop(cmd_tx);
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

struct PoolState {
    idle: VecDeque<Conn>,
    waiters: Vec<oneshot::Sender<Conn>>,
    live: usize,
}

pub(crate) struct Pool {
    pub(crate) server: Arc<Server>,
    state: Arc<Mutex<PoolState>>,
    pub(crate) credentials: (String, String),
    sock_path: PathBuf,
    config: PoolConfig,
    notify: Mutex<Option<Arc<super::notify::NotifyConn>>>,
    stop: Arc<AtomicBool>,
    reaper: Mutex<Option<JoinHandle<()>>>,
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
        config: PoolConfig,
        username: &str,
        database: &str,
    ) -> Result<Pool, Error> {
        let sock_path = server.sock_path.clone();
        let mut idle = VecDeque::new();
        for _ in 0..config.min {
            idle.push_back(Conn::spawn(&sock_path, username, database)?);
        }
        let live = idle.len();
        let state = Arc::new(Mutex::new(PoolState {
            idle,
            waiters: Vec::new(),
            live,
        }));
        let stop = Arc::new(AtomicBool::new(false));

        let reaper = {
            let state = state.clone();
            let stop = stop.clone();
            let ttl = config.idle_ttl;
            let min = config.min;
            std::thread::Builder::new()
                .name("pglite-pool-reaper".into())
                .spawn(move || Pool::reap_loop(state, stop, ttl, min))
                .map_err(Error::Io)?
        };

        Ok(Pool {
            server,
            state,
            credentials: (username.to_string(), database.to_string()),
            sock_path,
            config,
            notify: Mutex::new(None),
            stop,
            reaper: Mutex::new(Some(reaper)),
        })
    }

    fn reap_loop(state: Arc<Mutex<PoolState>>, stop: Arc<AtomicBool>, ttl: Duration, min: usize) {
        let tick = (ttl / 2).max(Duration::from_secs(1));
        let step = Duration::from_millis(500);
        let mut since = Duration::ZERO;
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(step);
            since += step;
            if since < tick {
                continue;
            }
            since = Duration::ZERO;

            let now = Instant::now();
            let mut doomed = Vec::new();
            {
                let mut st = state.lock().unwrap();
                while st.idle.len() > min {
                    let expired = st
                        .idle
                        .front()
                        .map(|c| !c.is_alive() || now.duration_since(c.last_used) >= ttl)
                        .unwrap_or(false);
                    if !expired {
                        break;
                    }
                    if let Some(conn) = st.idle.pop_front() {
                        st.live -= 1;
                        doomed.push(conn);
                    }
                }
            }
            for conn in doomed {
                conn.close();
            }
        }
    }

    async fn acquire(&self) -> Result<Conn, Error> {
        loop {
            enum Action {
                Got(Conn),
                Create,
                Wait(oneshot::Receiver<Conn>),
            }
            let action = {
                let mut st = self.state.lock().unwrap();
                let mut got = None;
                while let Some(conn) = st.idle.pop_back() {
                    if conn.is_alive() {
                        got = Some(conn);
                        break;
                    }
                    st.live -= 1;
                }
                if let Some(conn) = got {
                    Action::Got(conn)
                } else if st.live < self.config.max {
                    st.live += 1;
                    Action::Create
                } else {
                    let (tx, rx) = oneshot::channel();
                    st.waiters.push(tx);
                    Action::Wait(rx)
                }
            };
            match action {
                Action::Got(conn) => return Ok(conn),
                Action::Create => {
                    match Conn::spawn(&self.sock_path, &self.credentials.0, &self.credentials.1) {
                        Ok(conn) => return Ok(conn),
                        Err(e) => {
                            self.state.lock().unwrap().live -= 1;
                            return Err(e);
                        }
                    }
                }
                Action::Wait(rx) => {
                    let (deadline_tx, deadline_rx) = oneshot::channel::<()>();
                    std::thread::spawn(move || {
                        std::thread::sleep(ACQUIRE_TIMEOUT);
                        let _ = deadline_tx.send(());
                    });
                    futures::select! {
                        conn = rx.fuse() => {
                            if let Ok(conn) = conn {
                                if conn.is_alive() {
                                    return Ok(conn);
                                }
                                self.state.lock().unwrap().live -= 1;
                            }
                        }
                        _ = deadline_rx.fuse() => return Err(Error::PoolExhausted),
                    }
                }
            }
        }
    }

    fn release(&self, mut conn: Conn) {
        if !conn.is_alive() {
            self.state.lock().unwrap().live -= 1;
            return;
        }
        conn.last_used = Instant::now();
        let mut st = self.state.lock().unwrap();
        while let Some(waiter) = st.waiters.pop() {
            match waiter.send(conn) {
                Ok(()) => return,
                Err(returned) => conn = returned,
            }
        }
        st.idle.push_back(conn);
    }

    pub(crate) async fn roundtrip(&self, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        let conn = self.acquire().await?;
        let result = match conn.send(wire) {
            Ok(rx) => rx.await.map_err(|_| Error::Closed).and_then(|r| r),
            Err(e) => Err(e),
        };
        self.release(conn);
        result
    }

    pub(crate) async fn checkout(self: &Arc<Self>) -> Result<PinnedConn, Error> {
        let conn = self.acquire().await?;
        Ok(PinnedConn {
            pool: self.clone(),
            conn: Some(conn),
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
        let conn = {
            let mut st = self.state.lock().unwrap();
            match st.idle.pop_back() {
                Some(conn) => Some(conn),
                None if st.live < self.config.max => {
                    st.live += 1;
                    None
                }
                None => return,
            }
        };
        let conn = match conn {
            Some(conn) => conn,
            None => match Conn::spawn(&self.sock_path, &self.credentials.0, &self.credentials.1) {
                Ok(conn) => conn,
                Err(_) => {
                    self.state.lock().unwrap().live -= 1;
                    return;
                }
            },
        };
        let _ = conn.send(wire);
        self.release(conn);
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.reaper.lock().unwrap().take() {
            let _ = handle.join();
        }
        if let Some(conn) = self.notify.lock().unwrap().take() {
            conn.shutdown();
        }
        let drained: Vec<Conn> = self.state.lock().unwrap().idle.drain(..).collect();
        for conn in drained {
            conn.close();
        }
    }
}

pub(crate) struct PinnedConn {
    pool: Arc<Pool>,
    conn: Option<Conn>,
}

impl PinnedConn {
    pub(crate) async fn roundtrip(&self, wire: Vec<u8>) -> Result<Vec<u8>, Error> {
        let conn = self.conn.as_ref().ok_or(Error::Closed)?;
        let rx = conn.send(wire)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    pub(crate) fn fire_and_forget(&self, wire: Vec<u8>) {
        if let Some(conn) = self.conn.as_ref() {
            let _ = conn.send(wire);
        }
    }
}

impl Drop for PinnedConn {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.release(conn);
        }
    }
}
