use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use pglite::PGlite;

#[test]
fn listen_notify_round_trip() {
    block_on(async {
        let db = PGlite::open_temp().await.unwrap();

        let hits = Arc::new(AtomicUsize::new(0));
        let payloads = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let h = hits.clone();
        let p = payloads.clone();
        db.listen("changes", move |payload| {
            h.fetch_add(1, Ordering::SeqCst);
            p.lock().unwrap().push(payload.to_string());
        })
        .await
        .unwrap();

        db.exec("NOTIFY changes, 'hello'").await.unwrap();
        db.exec("SELECT 1").await.unwrap();

        assert_eq!(hits.load(Ordering::SeqCst), 1, "callback not invoked");
        assert_eq!(payloads.lock().unwrap()[0], "hello");

        db.unlisten("changes").await.unwrap();
        db.exec("NOTIFY changes, 'ignored'").await.unwrap();
        db.exec("SELECT 1").await.unwrap();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "callback fired after unlisten"
        );

        db.close().await.unwrap();
    });
}
