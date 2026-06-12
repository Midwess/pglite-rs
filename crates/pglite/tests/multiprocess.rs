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

use futures::executor::block_on;
use pglite::{MultiProcessOptions, PGlite};

#[test]
fn open_multi_process_basic() {
    block_on(async {
        let base = std::env::temp_dir().join(format!("pgl-mp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let db = PGlite::open_multi_process(base.join("a"), MultiProcessOptions::default())
            .await
            .unwrap();

        db.exec("CREATE TABLE t (id serial PRIMARY KEY, v text)")
            .await
            .unwrap();
        db.exec("INSERT INTO t (v) VALUES ('one'), ('two')")
            .await
            .unwrap();
        let rows = db
            .query("SELECT v FROM t WHERE id = $1", &[&2i32])
            .await
            .unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "two");

        let db2 = PGlite::open_multi_process(base.join("b"), MultiProcessOptions::default())
            .await
            .unwrap();
        let rows = db2.query("SELECT 41 + 1", &[]).await.unwrap();
        assert_eq!(rows[0].get::<i32>(0).unwrap(), 42);
        db2.close().await.unwrap();

        let rows = db.query("SELECT count(*) FROM t", &[]).await.unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 2);
        db.close().await.unwrap();

        let _ = std::fs::remove_dir_all(&base);
    });
}

#[test]
fn parallel_pinned_transactions() {
    block_on(async {
        let base = std::env::temp_dir().join(format!("pgl-mptx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let db = PGlite::open_multi_process(&base, MultiProcessOptions::default())
            .await
            .unwrap();
        db.exec("CREATE TABLE items (id INT PRIMARY KEY, label TEXT)")
            .await
            .unwrap();

        let tx1 = db.transaction().await.unwrap();
        tx1.exec("INSERT INTO items VALUES (1, 'first')")
            .await
            .unwrap();

        let rows = db.query("SELECT count(*) FROM items", &[]).await.unwrap();
        assert_eq!(
            rows[0].get::<i64>(0).unwrap(),
            0,
            "uncommitted tx1 row must be invisible to pool connections"
        );

        let tx2 = db.transaction().await.unwrap();
        tx2.exec("INSERT INTO items VALUES (2, 'second')")
            .await
            .unwrap();
        tx2.commit().await.unwrap();

        let rows = db.query("SELECT count(*) FROM items", &[]).await.unwrap();
        assert_eq!(
            rows[0].get::<i64>(0).unwrap(),
            1,
            "tx2 committed while tx1 still open"
        );

        let rows = db
            .query(
                "SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend'",
                &[],
            )
            .await
            .unwrap();
        assert!(rows[0].get::<i64>(0).unwrap() >= 2);

        tx1.commit().await.unwrap();
        let rows = db.query("SELECT count(*) FROM items", &[]).await.unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 2);

        let tx3 = db.transaction().await.unwrap();
        tx3.exec("INSERT INTO items VALUES (3, 'third')")
            .await
            .unwrap();
        drop(tx3);

        let rows = db.query("SELECT count(*) FROM items", &[]).await.unwrap();
        assert_eq!(
            rows[0].get::<i64>(0).unwrap(),
            2,
            "dropped transaction must roll back on its pinned connection"
        );

        db.close().await.unwrap();
        let _ = std::fs::remove_dir_all(&base);
    });
}

#[test]
fn notify_across_connections() {
    block_on(async {
        let base = std::env::temp_dir().join(format!("pgl-mpnotify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let db = PGlite::open_multi_process(&base, MultiProcessOptions::default())
            .await
            .unwrap();

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = received.clone();
        let token = db
            .listen("updates", move |payload| {
                sink.lock().unwrap().push(payload.to_string());
            })
            .await
            .unwrap();

        db.exec("NOTIFY updates, 'first'").await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while received.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(received.lock().unwrap().as_slice(), ["first"]);

        db.unlisten_token("updates", token).await.unwrap();
        db.exec("NOTIFY updates, 'second'").await.unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            received.lock().unwrap().as_slice(),
            ["first"],
            "unlistened channel must stop delivering"
        );

        db.close().await.unwrap();
        let _ = std::fs::remove_dir_all(&base);
    });
}
