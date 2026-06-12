use futures::executor::block_on;
use pglite::PGlite;

#[test]
fn crud_and_types() {
    block_on(async {
        let db = PGlite::open_temp().await.unwrap();

        db.exec(
            "CREATE TABLE t (
                i int, b bigint, f double precision, s text,
                ok boolean, bin bytea, ts timestamptz
            )",
        )
        .await
        .unwrap();

        let bin: Vec<u8> = vec![0u8, 1, 2, 255];
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        db.query(
            "INSERT INTO t VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[&42i32, &i64::MAX, &1.5f64, &"héllo", &true, &bin, &now],
        )
        .await
        .unwrap();

        let rows = db
            .query("SELECT i, b, f, s, ok, bin, ts FROM t", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.get::<i32>(0).unwrap(), 42);
        assert_eq!(row.get::<i64>(1).unwrap(), i64::MAX);
        assert_eq!(row.get::<f64>(2).unwrap(), 1.5);
        assert_eq!(row.get::<&str>(3).unwrap(), "héllo");
        assert!(row.get::<bool>(4).unwrap());
        assert_eq!(row.get::<&[u8]>(5).unwrap(), &[0u8, 1, 2, 255][..]);
        assert_eq!(row.get::<std::time::SystemTime>(6).unwrap(), now);

        let rows = db
            .query("SELECT s FROM t WHERE i = $1", &[&999i32])
            .await
            .unwrap();
        assert!(rows.is_empty());

        db.exec("UPDATE t SET s = 'updated'").await.unwrap();
        let rows = db.query("SELECT s FROM t", &[]).await.unwrap();
        assert_eq!(rows[0].try_get::<&str>("s").unwrap(), "updated");

        db.exec("DELETE FROM t").await.unwrap();
        let rows = db.query("SELECT count(*) FROM t", &[]).await.unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 0);

        db.close().await.unwrap();
    });
}
