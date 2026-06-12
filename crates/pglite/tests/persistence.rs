use futures::executor::block_on;
use pglite::{Error, PGlite};

#[test]
fn persistence_and_instance_guard() {
    if std::env::var("PGLITE_TEST_REOPEN_CHILD").is_ok() {
        return;
    }

    let data_dir = std::env::temp_dir().join(format!("pglite-persistence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);

    block_on(async {
        let db = PGlite::open(&data_dir).await.unwrap();

        match PGlite::open_temp().await {
            Err(Error::AlreadyOpen) => {}
            other => panic!("expected AlreadyOpen, got {:?}", other.map(|_| ())),
        }

        db.exec("CREATE TABLE persisted (v text)").await.unwrap();
        db.exec("INSERT INTO persisted VALUES ('survives reopen')")
            .await
            .unwrap();
        db.close().await.unwrap();

        match PGlite::open(&data_dir).await {
            Err(Error::ReopenUnsupported) => {}
            other => panic!("expected ReopenUnsupported, got {:?}", other.map(|_| ())),
        }
    });

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "reopen_child",
            "--nocapture",
            "--include-ignored",
        ])
        .env("PGLITE_TEST_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "child reopen failed:\n{}\n{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
#[ignore]
fn reopen_child() {
    let data_dir = std::env::var("PGLITE_TEST_DATA_DIR").expect("child needs data dir");
    block_on(async {
        let db = PGlite::open(&data_dir).await.unwrap();
        let rows = db.query("SELECT v FROM persisted", &[]).await.unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "survives reopen");
        db.close().await.unwrap();
    });
}
