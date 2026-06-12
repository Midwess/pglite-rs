#![cfg(feature = "socket")]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use futures::executor::block_on;
use pglite::PGlite;

fn startup_packet() -> Vec<u8> {
    let params = b"user\0postgres\0database\0postgres\0client_encoding\0UTF8\0\0";
    let total = (4 + 4 + params.len()) as u32;
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&total.to_be_bytes());
    pkt.extend_from_slice(&196608u32.to_be_bytes());
    pkt.extend_from_slice(params);
    pkt
}

fn read_until_ready(stream: &mut UnixStream) -> Vec<(u8, Vec<u8>)> {
    let mut frames = Vec::new();
    loop {
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).unwrap();
        let len = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        let mut body = vec![0u8; len - 4];
        stream.read_exact(&mut body).unwrap();
        let tag = header[0];
        frames.push((tag, body));
        if tag == b'Z' {
            return frames;
        }
    }
}

fn simple_query(stream: &mut UnixStream, sql: &str) -> Vec<(u8, Vec<u8>)> {
    let total = (4 + sql.len() + 1) as u32;
    let mut msg = vec![b'Q'];
    msg.extend_from_slice(&total.to_be_bytes());
    msg.extend_from_slice(sql.as_bytes());
    msg.push(0);
    stream.write_all(&msg).unwrap();
    read_until_ready(stream)
}

fn connect(gateway_path: &std::path::Path) -> UnixStream {
    let mut stream = UnixStream::connect(gateway_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(&startup_packet()).unwrap();
    let frames = read_until_ready(&mut stream);
    assert_eq!(frames[0].0, b'R');
    assert!(frames.iter().any(|(t, _)| *t == b'S'));
    assert!(frames.iter().any(|(t, _)| *t == b'K'));
    stream
}

#[test]
fn gateway_end_to_end() {
    block_on(async {
        let db = PGlite::open_temp().await.unwrap();
        serves_wire_clients(&db).await;
        extended_protocol_and_copy(&db).await;
        drop_unblocks_connected_session(&db).await;
        db.close().await.unwrap();
    });
}

async fn serves_wire_clients(db: &PGlite) {
    {
        let gateway = db.serve_unix_socket().await.unwrap();
        assert!(gateway.uri().starts_with("postgresql://"));

        let mut client = connect(gateway.socket_path());

        let frames = simple_query(&mut client, "SELECT 1 AS one");
        assert!(frames.iter().any(|(t, _)| *t == b'D'));
        assert!(frames.iter().any(|(t, _)| *t == b'C'));

        db.exec("CREATE TABLE gw (id INT, label TEXT)")
            .await
            .unwrap();
        let frames = simple_query(&mut client, "INSERT INTO gw VALUES (1, 'from-wire')");
        assert!(frames.iter().any(|(t, _)| *t == b'C'));
        let rows = db.query("SELECT label FROM gw", &[]).await.unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "from-wire");

        let mut terminate = vec![b'X'];
        terminate.extend_from_slice(&4u32.to_be_bytes());
        client.write_all(&terminate).unwrap();
        drop(client);

        let mut second = connect(gateway.socket_path());
        let frames = simple_query(&mut second, "SELECT count(*) FROM gw");
        assert!(frames.iter().any(|(t, _)| *t == b'D'));
        drop(second);

        let sock_dir = gateway.socket_path().parent().unwrap().to_path_buf();
        drop(gateway);
        assert!(!sock_dir.exists(), "socket dir must be removed on drop");
    }
}

async fn extended_protocol_and_copy(db: &PGlite) {
    {
        db.exec("CREATE TABLE ext (id INT, label TEXT)")
            .await
            .unwrap();
        db.exec("INSERT INTO ext VALUES (7, 'seven')")
            .await
            .unwrap();
        let gateway = db.serve_unix_socket().await.unwrap();
        let mut client = connect(gateway.socket_path());

        let mut batch = Vec::new();
        let query = b"SELECT label FROM ext WHERE id = 7\0";
        batch.push(b'P');
        batch.extend_from_slice(&((4 + 1 + query.len() + 2) as u32).to_be_bytes());
        batch.push(0);
        batch.extend_from_slice(query);
        batch.extend_from_slice(&0u16.to_be_bytes());
        batch.push(b'B');
        batch.extend_from_slice(&((4 + 1 + 1 + 2 + 2 + 2) as u32).to_be_bytes());
        batch.push(0);
        batch.push(0);
        batch.extend_from_slice(&0u16.to_be_bytes());
        batch.extend_from_slice(&0u16.to_be_bytes());
        batch.extend_from_slice(&0u16.to_be_bytes());
        batch.push(b'D');
        batch.extend_from_slice(&((4 + 1 + 1) as u32).to_be_bytes());
        batch.push(b'P');
        batch.push(0);
        batch.push(b'E');
        batch.extend_from_slice(&((4 + 1 + 4) as u32).to_be_bytes());
        batch.push(0);
        batch.extend_from_slice(&0u32.to_be_bytes());
        batch.push(b'S');
        batch.extend_from_slice(&4u32.to_be_bytes());
        client.write_all(&batch).unwrap();

        let frames = read_until_ready(&mut client);
        let tags: Vec<u8> = frames.iter().map(|(t, _)| *t).collect();
        assert!(tags.contains(&b'1'), "ParseComplete missing: {tags:?}");
        assert!(tags.contains(&b'2'), "BindComplete missing: {tags:?}");
        assert!(tags.contains(&b'D'), "DataRow missing: {tags:?}");
        let row = frames.iter().find(|(t, _)| *t == b'D').unwrap();
        assert!(String::from_utf8_lossy(&row.1).contains("seven"));

        db.exec("CREATE TABLE cp (name TEXT, legs INT)")
            .await
            .unwrap();
        db.copy_in("COPY cp FROM STDIN", b"rex\t4\ntweety\t2\n")
            .await
            .unwrap();
        let frames = simple_query(&mut client, "COPY cp TO STDOUT");
        let copied: Vec<u8> = frames
            .iter()
            .filter(|(t, _)| *t == b'd')
            .flat_map(|(_, body)| body.clone())
            .collect();
        assert_eq!(copied, b"rex\t4\ntweety\t2\n");

        let frames = simple_query(&mut client, "SELECT 1");
        assert!(frames.iter().any(|(t, _)| *t == b'D'));

        let mut bad = Vec::new();
        let query = b"SELECT no_such_column\0";
        bad.push(b'P');
        bad.extend_from_slice(&((4 + 1 + query.len() + 2) as u32).to_be_bytes());
        bad.push(0);
        bad.extend_from_slice(query);
        bad.extend_from_slice(&0u16.to_be_bytes());
        bad.push(b'S');
        bad.extend_from_slice(&4u32.to_be_bytes());
        client.write_all(&bad).unwrap();
        let frames = read_until_ready(&mut client);
        assert!(frames.iter().any(|(t, _)| *t == b'E'));
        assert_eq!(frames.iter().filter(|(t, _)| *t == b'Z').count(), 1);

        let mut good = Vec::new();
        let query = b"SELECT 2\0";
        good.push(b'P');
        good.extend_from_slice(&((4 + 1 + query.len() + 2) as u32).to_be_bytes());
        good.push(0);
        good.extend_from_slice(query);
        good.extend_from_slice(&0u16.to_be_bytes());
        good.push(b'S');
        good.extend_from_slice(&4u32.to_be_bytes());
        client.write_all(&good).unwrap();
        let frames = read_until_ready(&mut client);
        assert_eq!(
            frames[0].0,
            b'1',
            "first frame after an error batch must be ParseComplete, got {:?}",
            frames.iter().map(|(t, _)| *t as char).collect::<Vec<_>>()
        );

        drop(client);
        gateway.shutdown().unwrap();
    }
}

async fn drop_unblocks_connected_session(db: &PGlite) {
    {
        let gateway = db.serve_unix_socket().await.unwrap();
        let sock_dir = gateway.socket_path().parent().unwrap().to_path_buf();

        let client = connect(gateway.socket_path());
        let started = std::time::Instant::now();
        drop(gateway);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "drop must return within the bounded join window"
        );
        assert!(!sock_dir.exists());
        drop(client);
    }
}
