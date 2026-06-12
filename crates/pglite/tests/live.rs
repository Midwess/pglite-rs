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
        db.exec(
            "CREATE TABLE todos (id serial PRIMARY KEY, title text, done boolean DEFAULT false)",
        )
        .await
        .unwrap();
        db.exec("INSERT INTO todos (title) VALUES ('first')")
            .await
            .unwrap();

        let snapshots: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let snaps = snapshots.clone();
        let live = db
            .live_query(
                "SELECT title FROM todos WHERE done = $1 ORDER BY id",
                &[&false],
                move |rows| {
                    let titles = rows
                        .iter()
                        .map(|r| r.get::<&str>(0).unwrap().to_string())
                        .collect();
                    snaps.lock().unwrap().push(titles);
                },
            )
            .await
            .unwrap();

        assert_eq!(
            snapshots.lock().unwrap().as_slice(),
            &[vec!["first".to_string()]]
        );

        let second_snaps: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let counts = second_snaps.clone();
        let live2 = db
            .live_query("SELECT count(*) FROM todos", &[], move |rows| {
                counts
                    .lock()
                    .unwrap()
                    .push(rows[0].get::<i64>(0).unwrap() as usize);
            })
            .await
            .unwrap();

        db.exec("INSERT INTO todos (title) VALUES ('second')")
            .await
            .unwrap();
        assert!(
            wait_for(
                || snapshots.lock().unwrap().len() >= 2 && second_snaps.lock().unwrap().len() >= 2
            ),
            "no refresh after insert: {:?} / {:?}",
            snapshots.lock().unwrap(),
            second_snaps.lock().unwrap()
        );
        assert_eq!(
            snapshots.lock().unwrap().last().unwrap().as_slice(),
            &["first".to_string(), "second".to_string()]
        );

        live.unsubscribe().await.unwrap();
        let trigger_count: i64 = db
            .query(
                "SELECT count(*) FROM pg_trigger WHERE tgname LIKE '_notify_trigger_%'",
                &[],
            )
            .await
            .unwrap()[0]
            .get(0)
            .unwrap();
        assert_eq!(
            trigger_count, 1,
            "trigger removed while live2 still watches"
        );

        let before = snapshots.lock().unwrap().len();
        db.exec("INSERT INTO todos (title) VALUES ('third')")
            .await
            .unwrap();
        assert!(
            wait_for(|| second_snaps.lock().unwrap().len() >= 3),
            "live2 stopped"
        );
        assert_eq!(
            snapshots.lock().unwrap().len(),
            before,
            "callback fired after unsubscribe"
        );

        live2.unsubscribe().await.unwrap();
        let trigger_count: i64 = db
            .query(
                "SELECT count(*) FROM pg_trigger WHERE tgname LIKE '_notify_trigger_%'",
                &[],
            )
            .await
            .unwrap()[0]
            .get(0)
            .unwrap();
        assert_eq!(
            trigger_count, 0,
            "trigger not torn down after last unsubscribe"
        );

        db.close().await.unwrap();
    });
}
