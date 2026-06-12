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

    sqlx::raw_sql(
        "CREATE TABLE measurements (
           city text, at date, temp int,
           PRIMARY KEY (city, at)
         ) PARTITION BY RANGE (at);
         CREATE TABLE m_2025 PARTITION OF measurements FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
         CREATE TABLE m_2026 PARTITION OF measurements FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
         INSERT INTO measurements VALUES ('oslo', '2025-06-01', 18), ('oslo', '2026-06-01', 21)",
    )
    .execute(&pool)
    .await?;
    let row = sqlx::query("SELECT count(*) FROM m_2026")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.try_get::<i64, _>(0)?, 1);
    println!("PASS declarative range partitioning routes rows");

    sqlx::raw_sql(
        "CREATE TABLE inventory (
           sku text PRIMARY KEY,
           qty int NOT NULL DEFAULT 0,
           price numeric NOT NULL,
           value numeric GENERATED ALWAYS AS (qty * price) STORED
         );
         INSERT INTO inventory (sku, qty, price) VALUES ('apple', 10, 2.5)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO inventory (sku, qty, price) VALUES ('apple', 5, 2.5)
         ON CONFLICT (sku) DO UPDATE SET qty = inventory.qty + EXCLUDED.qty",
    )
    .execute(&pool)
    .await?;
    let row = sqlx::query("SELECT qty, value::float8 FROM inventory")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.try_get::<i32, _>(0)?, 15);
    assert_eq!(row.try_get::<f64, _>(1)?, 37.5);
    println!("PASS upsert (ON CONFLICT) + generated column");

    sqlx::raw_sql(
        "CREATE MATERIALIZED VIEW expensive AS
           SELECT sku FROM inventory WHERE price > 1;
         REFRESH MATERIALIZED VIEW expensive",
    )
    .execute(&pool)
    .await?;
    let row = sqlx::query("SELECT count(*) FROM expensive")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.try_get::<i64, _>(0)?, 1);
    println!("PASS materialized view + refresh");

    let rows = sqlx::query("SELECT relname::text FROM pg_class WHERE relname = 'inventory_pkey'")
        .fetch_all(&pool)
        .await?;
    assert_eq!(rows.len(), 1);
    println!("PASS system catalogs queryable");

    pool.close().await;
    db.close().await?;
    println!("ddl_power: all checks passed via SQLx");
    Ok(())
}
