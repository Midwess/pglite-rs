#![cfg(feature = "icu")]

use futures::executor::block_on;
use pglite::{LocaleProvider, PGlite, PGliteOptions};

#[test]
fn icu_collation() {
    block_on(async {
        let db = PGlite::open_temp_with(PGliteOptions {
            locale_provider: LocaleProvider::Icu,
            ..Default::default()
        })
        .await
        .unwrap();

        let rows = db
            .query("SELECT 'a' < 'B' COLLATE \"en-x-icu\"", &[])
            .await
            .unwrap();
        assert!(
            rows[0].get::<bool>(0).unwrap(),
            "ICU collation should sort a before B"
        );

        db.close().await.unwrap();
    });
}
