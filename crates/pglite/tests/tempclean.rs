use futures::executor::block_on;
use pglite::PGlite;

#[test]
fn open_temp_cleans_datadir() {
    block_on(async {
        let temp_db = PGlite::open_temp().await.unwrap();
        temp_db.exec("SELECT 1").await.unwrap();
        temp_db.close().await.unwrap();
    });

    let leftovers: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("pglite-temp-{}-", std::process::id()))
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp datadirs not cleaned: {leftovers:?}"
    );
}
