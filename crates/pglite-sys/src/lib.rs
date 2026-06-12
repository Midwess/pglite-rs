//! Raw FFI bindings to `libpglite`, the in-process PostgreSQL engine from
//! [postgres-pglite](https://github.com/electric-sql/postgres-pglite).
//!
//! The build script links a prebuilt static `libpglite.a` (downloaded from this
//! repository's GitHub releases, built locally via `native/build-libpglite.sh`,
//! or pointed to with `PGLITE_LIB_DIR`). Use the safe
//! [`pglite-rs`](https://crates.io/crates/pglite-rs) crate unless you need the
//! raw engine entry points.

use libc::{c_char, c_int, c_void, size_t, ssize_t, FILE};

#[repr(C)]
pub struct Port {
    _private: [u8; 0],
}

pub type PglReadCb = unsafe extern "C" fn(buffer: *mut c_void, max_length: size_t) -> ssize_t;
pub type PglWriteCb = unsafe extern "C" fn(buffer: *mut c_void, length: size_t) -> ssize_t;
pub type PglSystemCb = unsafe extern "C" fn(command: *const c_char) -> ssize_t;
pub type PglPopenCb =
    unsafe extern "C" fn(command: *const c_char, mode: *const c_char) -> *mut FILE;
pub type PglPcloseCb = unsafe extern "C" fn(stream: *mut FILE) -> c_int;
pub type PglMainFn = unsafe extern "C" fn(argc: c_int, argv: *mut *mut c_char) -> c_int;

extern "C" {
    pub fn pgl_set_rw_cbs(read_cb: PglReadCb, write_cb: PglWriteCb);
    pub fn pgl_set_system_fn(system_fn: PglSystemCb);
    pub fn pgl_set_popen_fn(popen_fn: PglPopenCb);
    pub fn pgl_set_pclose_fn(pclose_fn: PglPcloseCb);

    pub fn pgl_freopen(pathname: *const c_char, mode: *const c_char, streamid: c_int) -> *mut FILE;

    pub fn pgl_setPGliteActive(new_value: c_int) -> c_int;
    pub fn pgl_startPGlite();
    pub fn pgl_run_atexit_funcs();
    pub fn pgl_shmem_reset();
    pub fn pgl_native_reset();
    pub fn clear_setitimer();

    pub fn pgl_backend_main(argc: c_int, argv: *mut *mut c_char) -> c_int;

    pub fn pgl_getMyProcPort() -> *mut Port;
    pub fn ProcessStartupPacket(port: *mut Port, ssl_done: bool, gss_done: bool) -> c_int;
    pub fn pgl_sendConnData();

    pub fn PostgresMainLoopOnce();
    pub fn PostgresMainLongJmp();
    pub fn PostgresSendReadyForQueryIfNecessary();
    pub fn pq_buffer_remaining_data() -> ssize_t;
    pub fn pgl_pq_flush();
    pub fn IsTransactionBlock() -> bool;

    pub fn pgl_native_setup();
    pub fn pgl_native_exit(status: c_int) -> !;
    pub fn pgl_native_call(entry: PglMainFn, argc: c_int, argv: *mut *mut c_char) -> c_int;
    pub fn pgl_native_pump() -> c_int;
}
