#![cfg(feature = "multiple-process")]

use futures::executor::block_on;
use pglite::{MultiProcessOptions, PGlite};

#[test]
fn write_txn_does_not_block_concurrent_read() {
    let dir = std::env::temp_dir().join(format!("pglite-mp-concurrency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    block_on(async {
        let db = PGlite::open_multi_process(&dir, MultiProcessOptions::default())
            .await
            .unwrap();

        db.query("CREATE TABLE t (id int PRIMARY KEY, v text)", &[])
            .await
            .unwrap();
        db.query("INSERT INTO t VALUES (1, 'one')", &[])
            .await
            .unwrap();

        let tx = db.transaction().await.unwrap();
        tx.query("INSERT INTO t VALUES (2, 'two')", &[])
            .await
            .unwrap();

        let during = db.query("SELECT count(*)::int FROM t", &[]).await.unwrap();
        assert_eq!(
            during[0].get::<i32>(0).unwrap(),
            1,
            "concurrent read blocked on, or saw the uncommitted write of, the open transaction"
        );

        tx.commit().await.unwrap();

        let after = db.query("SELECT count(*)::int FROM t", &[]).await.unwrap();
        assert_eq!(after[0].get::<i32>(0).unwrap(), 2);

        db.close().await.unwrap();
    });

    let _ = std::fs::remove_dir_all(&dir);
}
