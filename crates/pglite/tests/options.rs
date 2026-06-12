use futures::executor::block_on;
use pglite::{PGlite, PGliteOptions};

#[test]
fn options_apply() {
    block_on(async {
        let db = PGlite::open_temp_with(PGliteOptions {
            username: "appuser".into(),
            relaxed_durability: true,
            start_params: vec!["work_mem=8MB".into()],
            ..Default::default()
        })
        .await
        .unwrap();

        let rows = db.query("SELECT current_user", &[]).await.unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "appuser");

        let rows = db.query("SHOW fsync", &[]).await.unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "off");

        let rows = db.query("SHOW work_mem", &[]).await.unwrap();
        assert_eq!(rows[0].get::<&str>(0).unwrap(), "8MB");

        db.close().await.unwrap();
    });
}
