use futures::executor::block_on;
use pglite::{Error, PGlite};

#[test]
fn transactions_and_errors() {
    block_on(async {
        let db = PGlite::open_temp().await.unwrap();
        db.exec("CREATE TABLE tx (v text)").await.unwrap();

        let tx = db.transaction().await.unwrap();
        tx.exec("INSERT INTO tx VALUES ('committed')")
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let tx = db.transaction().await.unwrap();
        tx.exec("INSERT INTO tx VALUES ('rolled back')")
            .await
            .unwrap();
        tx.rollback().await.unwrap();

        let tx = db.transaction().await.unwrap();
        tx.exec("INSERT INTO tx VALUES ('dropped')").await.unwrap();
        drop(tx);

        let rows = db.query("SELECT v FROM tx ORDER BY v", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "committed");

        let err = db.exec("SELECT * FROM does_not_exist").await.unwrap_err();
        match &err {
            Error::Database {
                sqlstate, message, ..
            } => {
                assert_eq!(sqlstate, "42P01");
                assert!(message.contains("does_not_exist"), "{message}");
            }
            other => panic!("expected Database error, got {other:?}"),
        }

        let rows = db.query("SELECT 'still alive'", &[]).await.unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "still alive");

        db.exec("CREATE TABLE uniq (id int PRIMARY KEY)")
            .await
            .unwrap();
        db.exec("INSERT INTO uniq VALUES (1)").await.unwrap();
        let err = db.exec("INSERT INTO uniq VALUES (1)").await.unwrap_err();
        match &err {
            Error::Database { sqlstate, .. } => assert_eq!(sqlstate, "23505"),
            other => panic!("expected Database error, got {other:?}"),
        }

        db.close().await.unwrap();
    });
}
