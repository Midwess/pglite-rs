use std::time::Instant;

use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::temp_dir().join(format!("pglite-example-mp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    let db =
        pglite::PGlite::open_multi_process(&base, pglite::MultiProcessOptions::default()).await?;
    let url = db.connection_uri().unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;

    sqlx::query("CREATE TABLE accounts (id INT PRIMARY KEY, balance INT NOT NULL)")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO accounts SELECT g, 100 FROM generate_series(1, 4) g")
        .execute(&pool)
        .await?;

    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE accounts SET balance = balance - 30 WHERE id = 1")
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query("SELECT balance FROM accounts WHERE id = 1")
        .fetch_one(&pool)
        .await?;
    println!(
        "other SQLx connections still see balance {} while the transaction is open",
        row.try_get::<i32, _>(0)?
    );
    assert_eq!(row.try_get::<i32, _>(0)?, 100);
    tx.commit().await?;

    let started = Instant::now();
    let (a, b, c, d) = tokio::join!(
        sqlx::query("SELECT pg_backend_pid(), pg_sleep(0.4)").fetch_one(&pool),
        sqlx::query("SELECT pg_backend_pid(), pg_sleep(0.4)").fetch_one(&pool),
        sqlx::query("SELECT pg_backend_pid(), pg_sleep(0.4)").fetch_one(&pool),
        sqlx::query("SELECT pg_backend_pid(), pg_sleep(0.4)").fetch_one(&pool),
    );
    let elapsed = started.elapsed();
    let mut pids: Vec<i32> = [a?, b?, c?, d?]
        .iter()
        .map(|r| r.try_get::<i32, _>(0))
        .collect::<Result<_, _>>()?;
    pids.sort();
    pids.dedup();
    assert!(
        pids.len() >= 2,
        "expected multiple backends, saw pids {pids:?}"
    );
    assert!(
        elapsed.as_millis() < 1200,
        "4 x pg_sleep(0.4) took {elapsed:?}; backends did not run in parallel"
    );
    println!(
        "4 concurrent pg_sleep(0.4) queries across {} backends finished in {elapsed:?}",
        pids.len()
    );

    let rows = db
        .query("SELECT sum(balance)::INT FROM accounts", &[])
        .await?;
    println!(
        "native PGlite API sees the SQLx data: total balance {}",
        rows[0].get::<i32>(0)?
    );

    pool.close().await;
    db.close().await?;
    println!("multi_process: all checks passed via SQLx");
    Ok(())
}
