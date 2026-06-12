use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = pglite::PGlite::open_temp().await?;
    let gateway = db.serve_unix_socket().await?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(gateway.uri())
        .await?;

    sqlx::query(
        "CREATE TABLE tasks (id serial PRIMARY KEY, title text, done boolean DEFAULT false)",
    )
    .execute(&pool)
    .await?;

    let notified = Arc::new(AtomicUsize::new(0));
    let n = notified.clone();
    db.listen("task_events", move |payload| {
        println!("  notify received: {payload}");
        n.fetch_add(1, Ordering::SeqCst);
    })
    .await?;
    sqlx::raw_sql("NOTIFY task_events, 'ping from SQLx'")
        .execute(&pool)
        .await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while notified.load(Ordering::SeqCst) < 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(notified.load(Ordering::SeqCst), 1);
    println!("PASS SQLx NOTIFY delivered to native listen() callback");

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

    sqlx::query("INSERT INTO tasks (title) VALUES ($1)")
        .bind("write examples")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO tasks (title) VALUES ($1)")
        .bind("ship v1.3")
        .execute(&pool)
        .await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while refreshes.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(refreshes.load(Ordering::SeqCst) >= 3);
    println!("PASS SQLx inserts re-ran the native live query");

    live.unsubscribe().await?;

    db.copy_in(
        "COPY tasks (title) FROM STDIN",
        b"bulk one\nbulk two\nbulk three\n",
    )
    .await?;
    let row = sqlx::query("SELECT count(*) FROM tasks")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.try_get::<i64, _>(0)?, 5);
    let out = db
        .copy_out("COPY (SELECT title FROM tasks ORDER BY id) TO STDOUT")
        .await?;
    assert!(String::from_utf8_lossy(&out).contains("bulk two"));
    println!("PASS native COPY visible to SQLx and vice versa");

    pool.close().await;
    db.close().await?;
    println!("reactive: all checks passed via SQLx + native API");
    Ok(())
}
