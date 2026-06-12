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

    sqlx::query("CREATE TABLE users (id serial PRIMARY KEY, name text NOT NULL)")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO users (name) VALUES ('alice'), ('bob')")
        .execute(&pool)
        .await?;

    let rows = sqlx::query("SELECT id, name FROM users WHERE id > $1 ORDER BY id")
        .bind(0i32)
        .fetch_all(&pool)
        .await?;
    for row in &rows {
        let id: i32 = row.try_get(0)?;
        let name: &str = row.try_get(1)?;
        println!("{id}: {name}");
    }

    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO users (name) VALUES ('carol')")
        .execute(&mut *tx)
        .await?;
    tx.rollback().await?;

    let row = sqlx::query("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await?;
    let count: i64 = row.try_get(0)?;
    println!("count after rollback: {count}");
    assert_eq!(count, 2);

    pool.close().await;
    db.close().await?;
    println!("basic: all checks passed via SQLx");
    Ok(())
}
