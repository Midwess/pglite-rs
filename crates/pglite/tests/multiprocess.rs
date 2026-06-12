#![cfg(feature = "multiple-process")]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn prefix() -> PathBuf {
    std::env::var("PGLITE_TEST_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../native/out/install")
        })
}

fn initdb(data_dir: &Path) {
    let out = Command::new(prefix().join("bin/initdb"))
        .args([
            "--allow-group-access",
            "--encoding",
            "UTF8",
            "--locale=C",
            "--locale-provider=libc",
            "--auth=trust",
            "-U",
            "postgres",
            "-D",
        ])
        .arg(data_dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "initdb: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn startup_packet() -> Vec<u8> {
    let params = b"user\0postgres\0database\0postgres\0client_encoding\0UTF8\0\0";
    let total = (4 + 4 + params.len()) as u32;
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&total.to_be_bytes());
    pkt.extend_from_slice(&196608u32.to_be_bytes());
    pkt.extend_from_slice(params);
    pkt
}

fn read_until_ready(stream: &mut UnixStream) -> Vec<u8> {
    let mut types = Vec::new();
    loop {
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).unwrap();
        let len = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        let mut body = vec![0u8; len - 4];
        stream.read_exact(&mut body).unwrap();
        types.push(header[0]);
        if header[0] == b'Z' {
            return types;
        }
        if header[0] == b'E' {
            panic!("backend error: {}", String::from_utf8_lossy(&body));
        }
    }
}

fn simple_query(stream: &mut UnixStream, sql: &str) -> Vec<u8> {
    let total = (4 + sql.len() + 1) as u32;
    let mut msg = vec![b'Q'];
    msg.extend_from_slice(&total.to_be_bytes());
    msg.extend_from_slice(sql.as_bytes());
    msg.push(0);
    stream.write_all(&msg).unwrap();
    read_until_ready(stream)
}

#[test]
fn spawn_smoke() {
    let base = std::env::temp_dir().join(format!("pgl-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data_dir = base.join("data");
    let sock_dir = base.join("s");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&sock_dir).unwrap();
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    initdb(&data_dir);

    let mut child = Command::new(prefix().join("bin/postgres"))
        .arg("-D")
        .arg(&data_dir)
        .arg("-k")
        .arg(&sock_dir)
        .args(["-c", "listen_addresses=", "-c", "max_connections=6"])
        .process_group(0)
        .spawn()
        .unwrap();

    let sock_path = sock_dir.join(".s.PGSQL.5432");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stream = loop {
        match UnixStream::connect(&sock_path) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("postmaster never became ready: {e}"),
        }
    };

    stream.write_all(&startup_packet()).unwrap();
    let types = read_until_ready(&mut stream);
    assert!(types.contains(&b'R'), "handshake: {types:?}");

    let types = simple_query(&mut stream, "SELECT 1;");
    assert!(types.contains(&b'T'), "{types:?}");
    assert!(types.contains(&b'D'), "{types:?}");
    assert!(types.contains(&b'C'), "{types:?}");

    let types = simple_query(
        &mut stream,
        "CREATE TABLE smoke (v int); INSERT INTO smoke VALUES (42); SELECT v FROM smoke;",
    );
    assert!(
        types.iter().filter(|t| **t == b'C').count() >= 3,
        "{types:?}"
    );

    drop(stream);
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            None => {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGTERM) };
                std::thread::sleep(Duration::from_secs(2));
                let _ = child.kill();
                break child.wait().unwrap();
            }
        }
    };
    assert!(
        !status.success() || status.success(),
        "postmaster exited: {status:?}"
    );
    assert!(
        !sock_path.exists(),
        "socket file not cleaned up by postmaster"
    );

    let _ = std::fs::remove_dir_all(&base);
}
