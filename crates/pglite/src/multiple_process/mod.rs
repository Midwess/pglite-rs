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
    pub max_connections: usize,
    pub relaxed_durability: bool,
    pub start_params: Vec<String>,
    pub locale_provider: LocaleProvider,
}

impl Default for MultiProcessOptions {
    fn default() -> MultiProcessOptions {
        MultiProcessOptions {
            username: "postgres".into(),
            database: "postgres".into(),
            max_connections: 4,
            relaxed_durability: false,
            start_params: Vec::new(),
            locale_provider: LocaleProvider::default(),
        }
    }
}

pub(crate) struct Server {
    child: Mutex<Child>,
    pid: i32,
    sock_dir: PathBuf,
    pub(crate) sock_path: PathBuf,
}

impl Server {
    fn spawn(
        runtime_dir: &Path,
        data_dir: &Path,
        options: &MultiProcessOptions,
        pool_size: usize,
    ) -> Result<Arc<Server>, Error> {
        let sock_dir = std::env::temp_dir().join(format!(
            "pgl-{}-{}",
            std::process::id(),
            INSTANCE_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let sock_path = sock_dir.join(".s.PGSQL.5432");
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
            .args(["-c", "listen_addresses="])
            .args(["-c", &format!("max_connections={}", pool_size + 2)]);
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

        let pool_size = options.max_connections.max(2);
        let server = Server::spawn(&runtime_dir, &data_dir, &options, pool_size)?;
        let pool = Pool::start(server, pool_size, &options.username, &options.database)?;
        let db = PGlite::assemble(Backend::MultiProcess(Arc::new(pool)), data_dir);
        db.sweep_live_views().await?;
        Ok(db)
    }
}
