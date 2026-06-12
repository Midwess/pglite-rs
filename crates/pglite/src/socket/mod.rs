use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::db::PGlite;
use crate::engine::Engine;
use crate::error::Error;

static GATEWAY_COUNTER: AtomicU32 = AtomicU32::new(0);

pub struct SocketGateway {
    stop: Arc<AtomicBool>,
    sock_dir: PathBuf,
    sock_path: PathBuf,
    uri: String,
    doorman: Option<JoinHandle<()>>,
    _db: PGlite,
}

impl PGlite {
    pub async fn serve_unix_socket(&self) -> Result<SocketGateway, Error> {
        if self.backend().is_multi_process() {
            return Err(Error::Protocol(
                "multi-process instances expose a native socket; use connection_uri()".into(),
            ));
        }
        let rows = self
            .query(
                "SELECT current_setting('server_version'), current_user, current_database()",
                &[],
            )
            .await?;
        let server_version = rows[0].get::<&str>(0)?.to_string();
        let user = rows[0].get::<&str>(1)?.to_string();
        let database = rows[0].get::<&str>(2)?.to_string();

        let sock_dir = Engine::ram_backed_dir().join(format!(
            "pgl-gw-{}-{}",
            std::process::id(),
            GATEWAY_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let sock_path = sock_dir.join(".s.PGSQL.5432");
        if sock_path.as_os_str().len() > 96 {
            return Err(Error::Protocol(format!(
                "socket path too long: {}",
                sock_path.display()
            )));
        }
        std::fs::create_dir_all(&sock_dir)?;
        std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700))?;
        let listener = UnixListener::bind(&sock_path)?;

        let uri = format!(
            "postgresql://{}@localhost/{}?host={}",
            user,
            database,
            sock_dir.display()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let startup_reply = synth_startup_reply(&server_version);
        let doorman_db = self.clone();
        let doorman_stop = stop.clone();
        let doorman = std::thread::Builder::new()
            .name("pglite-socket".into())
            .spawn(move || doorman_loop(doorman_db, listener, doorman_stop, startup_reply))
            .map_err(Error::Io)?;

        Ok(SocketGateway {
            stop,
            sock_dir,
            sock_path,
            uri,
            doorman: Some(doorman),
            _db: self.clone(),
        })
    }
}

impl SocketGateway {
    pub fn socket_path(&self) -> &Path {
        &self.sock_path
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn shutdown(mut self) -> Result<(), Error> {
        self.stop_doorman();
        std::fs::remove_dir_all(&self.sock_dir)?;
        Ok(())
    }

    fn stop_doorman(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.sock_path);
        if let Some(handle) = self.doorman.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for SocketGateway {
    fn drop(&mut self) {
        self.stop_doorman();
        let _ = std::fs::remove_dir_all(&self.sock_dir);
    }
}

fn doorman_loop(db: PGlite, listener: UnixListener, stop: Arc<AtomicBool>, startup_reply: Vec<u8>) {
    while !stop.load(Ordering::SeqCst) {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let _ = serve_client(&db, stream, &stop, &startup_reply);
    }
}

fn serve_client(
    _db: &PGlite,
    _stream: UnixStream,
    _stop: &AtomicBool,
    _startup_reply: &[u8],
) -> Result<(), Error> {
    Ok(())
}

fn synth_startup_reply(_server_version: &str) -> Vec<u8> {
    Vec::new()
}
