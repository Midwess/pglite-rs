#![cfg(any(feature = "pgcrypto", feature = "pgvector"))]

use futures::executor::block_on;
use pglite::PGlite;

#[test]
fn extensions_load_and_run() {
    block_on(async {
        let db = PGlite::open_temp().await.unwrap();

        #[cfg(feature = "pgcrypto")]
        {
            db.exec("CREATE EXTENSION pgcrypto").await.unwrap();
            let rows = db
                .query("SELECT encode(digest('x', 'sha256'), 'hex')", &[])
                .await
                .unwrap();
            assert_eq!(
                rows[0].get::<&str>(0).unwrap(),
                "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"
            );
        }

        #[cfg(feature = "pgvector")]
        {
            db.exec("CREATE EXTENSION vector").await.unwrap();
            db.exec("CREATE TABLE items (embedding vector(3))")
                .await
                .unwrap();
            db.exec("INSERT INTO items VALUES ('[1,2,3]'), ('[4,5,6]')")
                .await
                .unwrap();
            let rows = db
                .query(
                    "SELECT embedding::text FROM items ORDER BY embedding <-> '[1,2,4]' LIMIT 1",
                    &[],
                )
                .await
                .unwrap();
            assert_eq!(rows[0].get::<&str>(0).unwrap(), "[1,2,3]");
        }

        db.close().await.unwrap();
    });
}
