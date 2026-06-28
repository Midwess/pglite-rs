//! Multi-process mode: spawns the bundled `bin/postgres` as a child postmaster on a
//! private unix socket (no networking) and pools N connections behind the same
//! `PGlite` API, giving true concurrent sessions with the shared lock table and
//! cross-session MVCC of a regular Postgres.
//!
//! Pool connections self-heal: a worker that hits an IO error reconnects, and if the
//! server is gone the connection leaves the rotation. Acquire waits are bounded by a
//! 30s timeout surfacing [`Error::PoolExhausted`](crate::Error).
//!
//! Orphan caveat: if the host process is SIGKILLed, the child postmaster's process
//! group receives no signal; the next `open_multi_process` on the same data dir relies
//! on Postgres' stale `postmaster.pid` handling, and abandoned socket dirs under the
//! OS temp dir persist until the OS cleans them.

pub(crate) mod notify;
pub(crate) mod pool;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::db::{Backend, LocaleProvider, PGlite};
use crate::engine::Engine;
use crate::error::Error;
use pool::Pool;

static INSTANCE_COUNTER: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Debug)]
pub struct MultiProcessOptions {
    pub username: String,
    pub database: String,
    pub min_connections: usize,
    pub max_connections: usize,
    pub extra_connections: usize,
    pub idle_ttl: Duration,
    pub relaxed_durability: bool,
    pub start_params: Vec<String>,
    pub locale_provider: LocaleProvider,
    pub listen_addresses: Option<String>,
    pub port: Option<u16>,
}

impl Default for MultiProcessOptions {
    fn default() -> MultiProcessOptions {
        MultiProcessOptions {
            username: "postgres".into(),
            database: "postgres".into(),
            min_connections: 0,
            max_connections: 5,
            extra_connections: 4,
            idle_ttl: Duration::from_secs(300),
            relaxed_durability: false,
            start_params: Vec::new(),
            locale_provider: LocaleProvider::default(),
            listen_addresses: None,
            port: None,
        }
    }
}

pub(crate) struct Server {
    child: Mutex<Child>,
    pid: i32,
    pub(crate) sock_dir: PathBuf,
    pub(crate) sock_path: PathBuf,
}

impl Server {
    fn spawn(
        runtime_dir: &Path,
        data_dir: &Path,
        options: &MultiProcessOptions,
        pool_size: usize,
    ) -> Result<Arc<Server>, Error> {
        let sock_dir = Engine::ram_backed_dir().join(format!(
            "pgl-{}-{}",
            std::process::id(),
            INSTANCE_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let port = options.port.unwrap_or(5432);
        let sock_path = sock_dir.join(format!(".s.PGSQL.{port}"));
        if sock_path.as_os_str().len() > 96 {
            return Err(Error::PostmasterStart(format!(
                "socket path too long: {}",
                sock_path.display()
            )));
        }
        std::fs::create_dir_all(&sock_dir)?;
        std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700))?;

        let mut cmd = Command::new(runtime_dir.join("bin/postgres"));
        cmd.arg("-D")
            .arg(data_dir)
            .arg("-k")
            .arg(&sock_dir)
            .args([
                "-c",
                &format!(
                    "listen_addresses={}",
                    options.listen_addresses.clone().unwrap_or_default()
                ),
            ])
            .args(["-c", &format!("port={port}")])
            .args([
                "-c",
                &format!(
                    "max_connections={}",
                    pool_size + 2 + options.extra_connections
                ),
            ]);
        if options.relaxed_durability {
            cmd.args(["-c", "fsync=off"]);
        }
        for param in &options.start_params {
            cmd.args(["-c", param]);
        }
        let log = std::fs::File::create(sock_dir.with_extension("log"))?;
        cmd.stdout(Stdio::null()).stderr(log);
        cmd.process_group(0);
        let child = cmd
            .spawn()
            .map_err(|e| Error::PostmasterStart(e.to_string()))?;
        let pid = child.id() as i32;

        let server = Arc::new(Server {
            child: Mutex::new(child),
            pid,
            sock_dir,
            sock_path,
        });

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match pool::connect_and_handshake(
                &server.sock_path,
                &options.username,
                &options.database,
            ) {
                Ok(_) => break,
                Err(_) if Instant::now() < deadline => {
                    if let Ok(Some(status)) = server.child.lock().unwrap().try_wait() {
                        return Err(Error::PostmasterStart(format!(
                            "postmaster exited during startup: {status}"
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    server.terminate();
                    return Err(Error::PostmasterStart(format!(
                        "postmaster never became ready: {e}"
                    )));
                }
            }
        }
        Ok(server)
    }

    pub(crate) fn shutdown(&self) {
        unsafe { libc::kill(self.pid, libc::SIGINT) };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.lock().unwrap().try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    self.terminate();
                    break;
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.sock_dir);
    }

    fn terminate(&self) {
        unsafe { libc::kill(-self.pid, libc::SIGTERM) };
        std::thread::sleep(Duration::from_millis(200));
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self
            .child
            .lock()
            .unwrap()
            .try_wait()
            .ok()
            .flatten()
            .is_none()
        {
            self.terminate();
        }
        let _ = std::fs::remove_dir_all(&self.sock_dir);
    }
}

impl PGlite {
    pub fn socket_path(&self) -> Option<&Path> {
        match self.backend() {
            Backend::MultiProcess(pool) => Some(&pool.server.sock_path),
            _ => None,
        }
    }

    pub fn connection_uri(&self) -> Option<String> {
        match self.backend() {
            Backend::MultiProcess(pool) => Some(format!(
                "postgresql://{}@localhost/{}?host={}",
                pool.credentials.0,
                pool.credentials.1,
                pool.server.sock_dir.display()
            )),
            _ => None,
        }
    }

    pub async fn open_multi_process(
        data_dir: impl AsRef<Path>,
        options: MultiProcessOptions,
    ) -> Result<PGlite, Error> {
        let data_dir = data_dir.as_ref().to_path_buf();
        let runtime_dir = Engine::runtime_dir();
        Engine::extract_runtime_to(&runtime_dir)?;
        Engine::run_initdb_at(
            &runtime_dir,
            &data_dir,
            &options.username,
            options.locale_provider,
        )?;

        let pool_size = options.max_connections.max(1);
        let server = Server::spawn(&runtime_dir, &data_dir, &options, pool_size)?;
        let config = pool::PoolConfig {
            min: options.min_connections.min(pool_size),
            max: pool_size,
            idle_ttl: options.idle_ttl,
        };
        let pool = Pool::start(server, config, &options.username, &options.database)?;
        let db = PGlite::assemble(Backend::MultiProcess(Arc::new(pool)), data_dir);
        db.sweep_live_views().await?;
        Ok(db)
    }
}
