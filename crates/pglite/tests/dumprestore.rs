use futures::executor::block_on;
use pglite::PGlite;

#[test]
fn dump_and_restore() {
    if std::env::var("PGLITE_TEST_RESTORE_CHILD").is_ok() {
        return;
    }

    let base = std::env::temp_dir().join(format!("pglite-dump-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let tar_path = base.join("backup.tar");
    let restore_dir = base.join("restored");

    block_on(async {
        let db = PGlite::open(base.join("source")).await.unwrap();
        db.exec("CREATE TABLE snapshot (v text)").await.unwrap();
        db.exec("INSERT INTO snapshot VALUES ('frozen state')")
            .await
            .unwrap();
        db.dump_data_dir(&tar_path).await.unwrap();
        db.close().await.unwrap();
    });

    PGlite::restore_data_dir(&tar_path, &restore_dir).unwrap();

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "restore_child",
            "--nocapture",
            "--include-ignored",
        ])
        .env("PGLITE_TEST_DATA_DIR", &restore_dir)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "child restore-open failed:\n{}\n{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[ignore]
fn restore_child() {
    let data_dir = std::env::var("PGLITE_TEST_DATA_DIR").expect("child needs data dir");
    block_on(async {
        let db = PGlite::open(&data_dir).await.unwrap();
        let rows = db.query("SELECT v FROM snapshot", &[]).await.unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "frozen state");
        db.close().await.unwrap();
    });
}
