use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::executor::block_on;
use pglite::PGlite;

fn wait_for<F: Fn() -> bool>(cond: F) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn live_query_full_rerun() {
    block_on(async {
        let db = PGlite::open_temp().await.unwrap();
        db.exec("CREATE TABLE todos (id serial PRIMARY KEY, title text)")
            .await
            .unwrap();
        db.exec("INSERT INTO todos (title) VALUES ('first')")
            .await
            .unwrap();

        let snapshots: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let snaps = snapshots.clone();
        let live = db
            .live_query("SELECT title FROM todos ORDER BY id", move |rows| {
                let titles = rows
                    .iter()
                    .map(|r| r.get::<&str>(0).unwrap().to_string())
                    .collect();
                snaps.lock().unwrap().push(titles);
            })
            .await
            .unwrap();

        assert_eq!(
            snapshots.lock().unwrap().as_slice(),
            &[vec!["first".to_string()]]
        );

        db.exec("INSERT INTO todos (title) VALUES ('second')")
            .await
            .unwrap();
        assert!(
            wait_for(|| snapshots.lock().unwrap().len() >= 2),
            "no refresh after insert: {:?}",
            snapshots.lock().unwrap()
        );
        assert_eq!(
            snapshots.lock().unwrap().last().unwrap().as_slice(),
            &["first".to_string(), "second".to_string()]
        );

        live.unsubscribe().await.unwrap();
        let before = snapshots.lock().unwrap().len();
        db.exec("INSERT INTO todos (title) VALUES ('third')")
            .await
            .unwrap();
        db.exec("SELECT 1").await.unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            snapshots.lock().unwrap().len(),
            before,
            "callback fired after unsubscribe"
        );

        db.close().await.unwrap();
    });
}
