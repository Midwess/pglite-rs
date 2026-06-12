use std::cell::RefCell;
use std::env;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::process::Command;

thread_local! {
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static INPUT: RefCell<(Vec<u8>, usize)> = const { RefCell::new((Vec::new(), 0)) };
}

unsafe extern "C" fn read_cb(buffer: *mut c_void, max_length: usize) -> isize {
    INPUT.with(|io| {
        let (buf, off) = &mut *io.borrow_mut();
        let n = (buf.len() - *off).min(max_length);
        unsafe {
            std::ptr::copy_nonoverlapping(buf.as_ptr().add(*off), buffer as *mut u8, n);
        }
        *off += n;
        n as isize
    })
}

unsafe extern "C" fn write_cb(buffer: *mut c_void, length: usize) -> isize {
    OUTPUT.with(|out| {
        let slice = unsafe { std::slice::from_raw_parts(buffer as *const u8, length) };
        out.borrow_mut().extend_from_slice(slice);
        length as isize
    })
}

fn set_input(bytes: Vec<u8>) {
    INPUT.with(|io| *io.borrow_mut() = (bytes, 0));
    OUTPUT.with(|out| out.borrow_mut().clear());
}

fn input_drained() -> bool {
    INPUT.with(|io| {
        let (buf, off) = &*io.borrow();
        *off >= buf.len()
    })
}

fn output_message_types() -> Vec<u8> {
    OUTPUT.with(|out| {
        let buf = out.borrow();
        let mut types = Vec::new();
        let mut off = 0usize;
        while off + 5 <= buf.len() {
            types.push(buf[off]);
            let len = u32::from_be_bytes([buf[off + 1], buf[off + 2], buf[off + 3], buf[off + 4]]);
            off += 1 + len as usize;
        }
        types
    })
}

fn startup_packet() -> Vec<u8> {
    let params = b"user\0postgres\0database\0postgres\0client_encoding\0UTF8\0\0";
    let total = (4 + 4 + params.len()) as u32;
    let mut pkt = Vec::with_capacity(total as usize);
    pkt.extend_from_slice(&total.to_be_bytes());
    pkt.extend_from_slice(&196608u32.to_be_bytes());
    pkt.extend_from_slice(params);
    pkt
}

fn simple_query(sql: &str) -> Vec<u8> {
    let total = (4 + sql.len() + 1) as u32;
    let mut msg = Vec::with_capacity(1 + total as usize);
    msg.push(b'Q');
    msg.extend_from_slice(&total.to_be_bytes());
    msg.extend_from_slice(sql.as_bytes());
    msg.push(0);
    msg
}

#[test]
fn boot_and_select_one() {
    let prefix = env::var("PGLITE_TEST_PREFIX").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../native/out/install")
    });
    let pgdata = env::temp_dir().join(format!("pglite-sys-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&pgdata);

    env::set_var("PGUSER", "postgres");
    env::set_var("PGDATABASE", "postgres");
    env::set_var("TZ", "UTC");
    env::set_var("PGTZ", "UTC");
    env::set_var("PGCLIENTENCODING", "UTF8");
    env::set_var("LANG", "C");

    let status = Command::new(prefix.join("bin/initdb"))
        .args([
            "--allow-group-access",
            "--encoding",
            "UTF8",
            "--locale=C",
            "--locale-provider=libc",
            "--auth=trust",
            "-D",
        ])
        .arg(&pgdata)
        .output()
        .expect("failed to run initdb");
    assert!(status.status.success(), "initdb failed: {}", String::from_utf8_lossy(&status.stderr));

    let bin = CString::new(prefix.join("bin/postgres").to_str().unwrap()).unwrap();
    let pgdata_c = CString::new(pgdata.to_str().unwrap()).unwrap();
    let args: Vec<CString> = [
        "--single",
        "-F",
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
    .map(|s| CString::new(*s).unwrap())
    .collect();

    let devnull = CString::new("/dev/null").unwrap();
    let rmode = CString::new("r").unwrap();

    let rc = unsafe {
        pglite_sys::pgl_native_setup();
        pglite_sys::pgl_freopen(devnull.as_ptr(), rmode.as_ptr(), 0);
        pglite_sys::pgl_set_rw_cbs(read_cb, write_cb);
        pglite_sys::pgl_setPGliteActive(1);

        let mut argv: Vec<*mut c_char> = Vec::new();
        argv.push(bin.as_ptr() as *mut c_char);
        argv.extend(args.iter().map(|a| a.as_ptr() as *mut c_char));
        argv.push(c"-D".as_ptr() as *mut c_char);
        argv.push(pgdata_c.as_ptr() as *mut c_char);
        argv.push(c"postgres".as_ptr() as *mut c_char);
        let argc = argv.len() as c_int;
        argv.push(std::ptr::null_mut());

        pglite_sys::pgl_native_call(pglite_sys::pgl_backend_main, argc, argv.as_mut_ptr())
    };
    assert_eq!(rc, 99, "backend main should return PGLITE_EXIT_ALIVE");

    unsafe { pglite_sys::pgl_startPGlite() };

    set_input(startup_packet());
    let rc = unsafe {
        pglite_sys::ProcessStartupPacket(pglite_sys::pgl_getMyProcPort(), true, true)
    };
    assert_eq!(rc, 0, "startup packet rejected");
    unsafe {
        pglite_sys::pgl_sendConnData();
        pglite_sys::pgl_pq_flush();
    }
    let types = output_message_types();
    assert!(types.contains(&b'R'), "missing AuthenticationOk: {types:?}");
    assert!(types.contains(&b'Z'), "missing ReadyForQuery: {types:?}");

    set_input(simple_query("SELECT 1;"));
    unsafe {
        while !input_drained() || pglite_sys::pq_buffer_remaining_data() > 0 {
            let rc = pglite_sys::pgl_native_pump();
            if rc == 100 {
                pglite_sys::PostgresMainLongJmp();
            }
        }
        pglite_sys::PostgresSendReadyForQueryIfNecessary();
        pglite_sys::pgl_pq_flush();
    }
    let types = output_message_types();
    assert!(types.contains(&b'T'), "missing RowDescription: {types:?}");
    assert!(types.contains(&b'D'), "missing DataRow: {types:?}");
    assert!(types.contains(&b'C'), "missing CommandComplete: {types:?}");
    assert!(types.contains(&b'Z'), "missing ReadyForQuery: {types:?}");

    let _ = std::fs::remove_dir_all(&pgdata);
}
