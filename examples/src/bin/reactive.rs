use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pglite::PGlite;

fn main() -> Result<(), pglite::Error> {
    futures::executor::block_on(async {
        let db = PGlite::open_temp().await?;
        db.exec(
            "CREATE TABLE tasks (id serial PRIMARY KEY, title text, done boolean DEFAULT false)",
        )
        .await?;

        let notified = Arc::new(AtomicUsize::new(0));
        let n = notified.clone();
        db.listen("task_events", move |payload| {
            println!("  notify received: {payload}");
            n.fetch_add(1, Ordering::SeqCst);
        })
        .await?;
        db.exec("NOTIFY task_events, 'manual ping'").await?;
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        println!("PASS LISTEN/NOTIFY");

        let refreshes = Arc::new(AtomicUsize::new(0));
        let r = refreshes.clone();
        let live = db
            .live_query(
                "SELECT title FROM tasks WHERE NOT done ORDER BY id",
                &[],
                move |rows| {
                    println!("  live snapshot: {} open tasks", rows.len());
                    r.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await?;

        db.exec("INSERT INTO tasks (title) VALUES ('write examples')")
            .await?;
        db.exec("INSERT INTO tasks (title) VALUES ('ship v1.2')")
            .await?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while refreshes.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(refreshes.load(Ordering::SeqCst) >= 3);
        println!("PASS live query re-ran on inserts");

        live.unsubscribe().await?;

        db.copy_in(
            "COPY tasks (title) FROM STDIN",
            b"bulk one\nbulk two\nbulk three\n",
        )
        .await?;
        let rows = db.query("SELECT count(*) FROM tasks", &[]).await?;
        assert_eq!(rows[0].get::<i64>(0)?, 5);
        let out = db
            .copy_out("COPY (SELECT title FROM tasks ORDER BY id) TO STDOUT")
            .await?;
        assert!(String::from_utf8_lossy(&out).contains("bulk two"));
        println!("PASS COPY in/out round-trip");

        db.close().await?;
        println!("reactive: all checks passed");
        Ok(())
    })
}
