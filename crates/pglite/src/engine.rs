use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread::JoinHandle;

use futures::channel::oneshot;

use crate::db::{LocaleProvider, PGliteOptions};
use crate::error::Error;

thread_local! {
    static IO: RefCell<EngineIo> = const {
        RefCell::new(EngineIo { input: Vec::new(), input_off: 0, output: Vec::new() })
    };
}

struct EngineIo {
    input: Vec<u8>,
    input_off: usize,
    output: Vec<u8>,
}

unsafe extern "C" fn read_cb(buffer: *mut c_void, max_length: usize) -> isize {
    IO.with(|io| {
        let io = &mut *io.borrow_mut();
        let n = (io.input.len() - io.input_off).min(max_length);
        unsafe {
            std::ptr::copy_nonoverlapping(
                io.input.as_ptr().add(io.input_off),
                buffer as *mut u8,
                n,
            );
        }
        io.input_off += n;
        n as isize
    })
}

unsafe extern "C" fn write_cb(buffer: *mut c_void, length: usize) -> isize {
    IO.with(|io| {
        let slice = unsafe { std::slice::from_raw_parts(buffer as *const u8, length) };
        io.borrow_mut().output.extend_from_slice(slice);
        length as isize
    })
}

pub(crate) enum EngineCommand {
    Exec {
        wire: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<u8>, Error>>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}

pub(crate) struct Engine {
    data_dir: PathBuf,
    runtime_dir: PathBuf,
    options: PGliteOptions,
    boot_strings: Vec<CString>,
    boot_argv: Vec<*mut c_char>,
}

impl Engine {
    pub(crate) fn spawn(
        data_dir: PathBuf,
        options: PGliteOptions,
    ) -> (
        mpsc::Sender<EngineCommand>,
        JoinHandle<()>,
        oneshot::Receiver<Result<(), Error>>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (boot_tx, boot_rx) = oneshot::channel();
        let handle = std::thread::Builder::new()
            .name("pglite-engine".into())
            .spawn(move || {
                let mut engine = Engine {
                    data_dir,
                    runtime_dir: Self::runtime_dir(),
                    options,
                    boot_strings: Vec::new(),
                    boot_argv: Vec::new(),
                };
                match engine.boot() {
                    Ok(()) => {
                        let _ = boot_tx.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = boot_tx.send(Err(e));
                        return;
                    }
                }
                engine.run(cmd_rx);
            })
            .expect("failed to spawn pglite engine thread");
        (cmd_tx, handle, boot_rx)
    }

    pub(crate) fn runtime_dir() -> PathBuf {
        std::env::var("PGLITE_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::temp_dir().join(format!(
                    "pglite-rs-runtime-{}-{:x}",
                    env!("CARGO_PKG_VERSION"),
                    Self::runtime_fingerprint()
                ))
            })
    }

    #[cfg(any(feature = "multiple-process", feature = "socket"))]
    pub(crate) fn ram_backed_dir() -> PathBuf {
        if cfg!(target_os = "linux") {
            let shm = Path::new("/dev/shm");
            if shm.is_dir()
                && shm
                    .metadata()
                    .map(|m| !m.permissions().readonly())
                    .unwrap_or(false)
            {
                return shm.to_path_buf();
            }
        }
        std::env::temp_dir()
    }

    fn runtime_fingerprint() -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for chunk in crate::RUNTIME_TAR.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            hash ^= u64::from_le_bytes(word);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn run(&mut self, cmd_rx: mpsc::Receiver<EngineCommand>) {
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                EngineCommand::Exec { wire, reply } => {
                    let result = self.exec_protocol(&wire);
                    let _ = reply.send(result);
                }
                EngineCommand::Close { reply } => {
                    let _ = self.exec_protocol(&[b'X', 0, 0, 0, 4]);
                    unsafe {
                        pglite_sys::pgl_setPGliteActive(0);
                        pglite_sys::pgl_run_atexit_funcs();
                        pglite_sys::pgl_native_reset();
                    }
                    let _ = reply.send(());
                    return;
                }
            }
        }
    }

    fn exec_protocol(&mut self, wire: &[u8]) -> Result<Vec<u8>, Error> {
        IO.with(|io| {
            let io = &mut *io.borrow_mut();
            io.input.clear();
            io.input.extend_from_slice(wire);
            io.input_off = 0;
            io.output.clear();
        });

        unsafe {
            loop {
                let drained = IO.with(|io| {
                    let io = io.borrow();
                    io.input_off >= io.input.len()
                });
                if drained && pglite_sys::pq_buffer_remaining_data() <= 0 {
                    break;
                }
                let rc = pglite_sys::pgl_native_pump();
                if rc == 100 {
                    pglite_sys::PostgresMainLongJmp();
                }
            }
            pglite_sys::PostgresSendReadyForQueryIfNecessary();
            pglite_sys::pgl_pq_flush();
        }

        Ok(IO.with(|io| std::mem::take(&mut io.borrow_mut().output)))
    }

    fn boot(&mut self) -> Result<(), Error> {
        self.extract_runtime()?;
        self.run_initdb()?;

        std::env::set_var("PGUSER", &self.options.username);
        std::env::set_var("PGDATABASE", &self.options.database);
        std::env::set_var("TZ", "UTC");
        std::env::set_var("PGTZ", "UTC");
        std::env::set_var("PGCLIENTENCODING", "UTF8");
        std::env::set_var("LANG", "C");

        self.boot_strings
            .push(CString::new(self.runtime_dir.join("bin/postgres").to_str().unwrap()).unwrap());
        self.boot_strings
            .push(CString::new(self.data_dir.to_str().unwrap()).unwrap());
        let devnull = CString::new("/dev/null").unwrap();
        let rmode = CString::new("r").unwrap();

        let mut arg_strings: Vec<String> = [
            "--single",
            "-O",
            "-j",
            "-c",
            "search_path=public",
            "-c",
            "exit_on_error=false",
            "-c",
            "log_checkpoints=false",
            "-c",
            "max_worker_processes=0",
            "-c",
            "max_parallel_workers=0",
            "-c",
            "max_parallel_workers_per_gather=0",
            "-c",
            "max_parallel_maintenance_workers=0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if self.options.relaxed_durability {
            arg_strings.push("-F".into());
        }
        for param in &self.options.start_params {
            arg_strings.push("-c".into());
            arg_strings.push(param.clone());
        }
        arg_strings.push("-D".into());
        arg_strings.push(self.data_dir.to_str().unwrap().into());
        arg_strings.push(self.options.database.clone());
        self.boot_strings.extend(
            arg_strings
                .iter()
                .map(|a| CString::new(a.as_str()).unwrap()),
        );
        let bin = &self.boot_strings[0];
        let args = &self.boot_strings[2..];

        let rc = unsafe {
            pglite_sys::pgl_native_setup();
            pglite_sys::pgl_freopen(devnull.as_ptr(), rmode.as_ptr(), 0);
            pglite_sys::pgl_set_rw_cbs(read_cb, write_cb);
            pglite_sys::pgl_setPGliteActive(1);

            self.boot_argv.push(bin.as_ptr() as *mut c_char);
            self.boot_argv
                .extend(args.iter().map(|a| a.as_ptr() as *mut c_char));
            let argc = self.boot_argv.len() as c_int;
            self.boot_argv.push(std::ptr::null_mut());

            pglite_sys::pgl_native_call(
                pglite_sys::pgl_backend_main,
                argc,
                self.boot_argv.as_mut_ptr(),
            )
        };
        if rc != 99 {
            return Err(Error::Boot(format!(
                "backend main returned {rc}, expected 99"
            )));
        }

        unsafe { pglite_sys::pgl_startPGlite() };

        IO.with(|io| {
            let io = &mut *io.borrow_mut();
            io.input.clear();
            io.input.extend_from_slice(&self.startup_packet());
            io.input_off = 0;
            io.output.clear();
        });
        let rc = unsafe {
            pglite_sys::ProcessStartupPacket(pglite_sys::pgl_getMyProcPort(), true, true)
        };
        if rc != 0 {
            return Err(Error::Boot(format!("startup packet rejected: {rc}")));
        }
        unsafe {
            pglite_sys::pgl_sendConnData();
            pglite_sys::pgl_pq_flush();
        }
        IO.with(|io| io.borrow_mut().output.clear());
        Ok(())
    }

    fn startup_packet(&self) -> Vec<u8> {
        Self::build_startup_packet(&self.options.username, &self.options.database)
    }

    pub(crate) fn build_startup_packet(username: &str, database: &str) -> Vec<u8> {
        let mut params = Vec::new();
        for (k, v) in [
            ("user", username),
            ("database", database),
            ("client_encoding", "UTF8"),
        ] {
            params.extend_from_slice(k.as_bytes());
            params.push(0);
            params.extend_from_slice(v.as_bytes());
            params.push(0);
        }
        params.push(0);
        let total = (4 + 4 + params.len()) as u32;
        let mut pkt = Vec::with_capacity(total as usize);
        pkt.extend_from_slice(&total.to_be_bytes());
        pkt.extend_from_slice(&196608u32.to_be_bytes());
        pkt.extend_from_slice(&params);
        pkt
    }

    fn extract_runtime(&self) -> Result<(), Error> {
        Self::extract_runtime_to(&self.runtime_dir)
    }

    pub(crate) fn extract_runtime_to(runtime_dir: &Path) -> Result<(), Error> {
        if runtime_dir.join(".extracted").exists() {
            return Ok(());
        }
        let staging = runtime_dir.with_file_name(format!(
            "{}.staging-{}",
            runtime_dir.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let mut archive = tar::Archive::new(crate::RUNTIME_TAR);
        archive.unpack(&staging)?;
        std::fs::write(staging.join(".extracted"), b"")?;
        if std::fs::rename(&staging, runtime_dir).is_err() {
            let _ = std::fs::remove_dir_all(&staging);
            if !runtime_dir.join(".extracted").exists() {
                return Err(Error::Boot(
                    "runtime extraction race lost and target invalid".into(),
                ));
            }
        }
        Ok(())
    }

    fn run_initdb(&self) -> Result<(), Error> {
        Self::run_initdb_at(
            &self.runtime_dir,
            &self.data_dir,
            &self.options.username,
            self.options.locale_provider,
        )
    }

    pub(crate) fn run_initdb_at(
        runtime_dir: &Path,
        data_dir: &Path,
        username: &str,
        locale_provider: LocaleProvider,
    ) -> Result<(), Error> {
        if data_dir.join("PG_VERSION").exists() {
            return Ok(());
        }
        let locale_args: &[&str] = match locale_provider {
            LocaleProvider::Libc => &["--locale=C", "--locale-provider=libc"],
            LocaleProvider::Icu => &["--locale=C", "--locale-provider=icu", "--icu-locale=en"],
        };
        let output = Command::new(runtime_dir.join("bin/initdb"))
            .args(["--allow-group-access", "--encoding", "UTF8"])
            .args(locale_args)
            .args(["--auth=trust", "-U", username, "-D"])
            .arg(data_dir)
            .env("TZ", "UTC")
            .output()?;
        if !output.status.success() {
            return Err(Error::Boot(format!(
                "initdb failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_round_trip_off_thread() {
        let data_dir =
            std::env::temp_dir().join(format!("pglite-engine-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);

        let (cmd_tx, handle, boot_rx) = Engine::spawn(data_dir.clone(), PGliteOptions::default());
        futures::executor::block_on(boot_rx).unwrap().unwrap();

        let mut wire = vec![b'Q'];
        let sql = b"SELECT 1;\0";
        wire.extend_from_slice(&((4 + sql.len()) as u32).to_be_bytes());
        wire.extend_from_slice(sql);

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(EngineCommand::Exec {
                wire,
                reply: reply_tx,
            })
            .unwrap();
        let response = futures::executor::block_on(reply_rx).unwrap().unwrap();

        let types: Vec<u8> = {
            let mut t = Vec::new();
            let mut off = 0usize;
            while off + 5 <= response.len() {
                t.push(response[off]);
                let len = u32::from_be_bytes(response[off + 1..off + 5].try_into().unwrap());
                off += 1 + len as usize;
            }
            t
        };
        assert!(types.contains(&b'T'), "{types:?}");
        assert!(types.contains(&b'D'), "{types:?}");
        assert!(types.contains(&b'Z'), "{types:?}");

        let (close_tx, close_rx) = oneshot::channel();
        cmd_tx
            .send(EngineCommand::Close { reply: close_tx })
            .unwrap();
        futures::executor::block_on(close_rx).unwrap();
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&data_dir);
    }
}
