use futures::executor::block_on;
use pglite::PGlite;

#[test]
fn copy_in_and_out() {
    block_on(async {
        let db = PGlite::open_temp().await.unwrap();
        db.exec("CREATE TABLE people (id int, name text)")
            .await
            .unwrap();

        db.copy_in("COPY people FROM STDIN", b"1\talice\n2\tbob\n")
            .await
            .unwrap();

        let rows = db.query("SELECT count(*) FROM people", &[]).await.unwrap();
        assert_eq!(rows[0].get::<i64>(0).unwrap(), 2);

        let out = db.copy_out("COPY people TO STDOUT").await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("alice"), "{text}");
        assert!(text.contains("bob"), "{text}");

        let err = db.copy_in("SELECT 1", b"").await.unwrap_err();
        assert!(matches!(err, pglite::Error::Protocol(_)), "{err:?}");

        let rows = db.query("SELECT 'alive'", &[]).await.unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "alive");

        db.close().await.unwrap();
    });
}
